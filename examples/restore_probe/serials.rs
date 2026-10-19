//! Nonempty serial recovery, exact ownership and post-restore continuation.
//! The parent connects only to uniquely named synthetic rehearsal databases.
use super::*;
use axum::{
    extract::{Query, State},
    Extension,
};
use stateset_warehouse::{api::App, integrations::Connections, serial_reads, serials::Input};
use std::{collections::BTreeSet, sync::Arc};
const SRC: &str = "wh-restore-serial-source";
const DST: &str = "wh-restore-serial-destination";
const EXTRA: &str = "wh-restore-serial-extra";
const SKU: &str = "RESTORE-SERIAL";
const EMPTY: &str = "RESTORE-SERIAL-EMPTY";
const LOT: &str = "SERIAL-ORIGINAL-LOT";
const LABELS: [&str; 13] = [
    "Unit-A",
    "unit-a",
    "STAGED",
    "PACKED",
    "SHIPPED",
    "RMA-RETURNED",
    "RMA-PENDING",
    "IN-TRANSIT",
    "LOST",
    "ARRIVED",
    "MISSING",
    "FOUND",
    "LOCAL",
];
fn observed(target: &[&str], labels: &[&str]) -> Vec<Input> {
    vec![Input {
        target: target.iter().map(|s| (*s).into()).collect(),
        sku: SKU.into(),
        serials: labels.iter().map(|s| (*s).into()).collect(),
    }]
}
async fn request(
    store: &Store,
    w: &str,
    key: &str,
    c: Value,
    serials: Vec<Input>,
    packs: Vec<stateset_warehouse::units::Input>,
) -> Result<Request> {
    let command: Command = serde_json::from_value(c)?;
    let version = if matches!(&command, Command::CreateWarehouse { .. }) {
        0
    } else {
        application(store.state(TENANT, w).await)?.version
    };
    Ok(Request {
        request_id: key.into(),
        expected_version: version,
        resource_versions: None,
        unit_inputs: packs,
        serial_inputs: serials,
        command,
    })
}
async fn execute(
    store: &Store,
    originals: &mut Vec<Value>,
    w: &str,
    key: &str,
    c: Value,
    serials: Vec<Input>,
) -> Result<Value> {
    let r = request(store, w, key, c, serials, vec![]).await?;
    let response = application(store.execute(&identity(), w, &r).await)?;
    originals.push(json!({"warehouse_id":w,"request":r,"response":response}));
    Ok(response)
}
async fn custody(store: &Store) -> Result<Value> {
    let rows: Value = sqlx::query_scalar(
        "SELECT jsonb_object_agg(serial,place) FROM wms_serial_units WHERE tenant=$1 AND sku=$2",
    )
    .bind(TENANT)
    .bind(SKU)
    .fetch_one(&store.pool)
    .await?;
    Ok(rows)
}
async fn facts(store: &Store) -> Result<Value> {
    let mut result = serde_json::Map::new();
    for table in [
        "wms_serial_modes",
        "wms_serial_heads",
        "wms_serial_batches",
        "wms_serial_units",
        "wms_serial_steps",
        "wms_serial_observations",
    ] {
        let sql=format!("SELECT COALESCE(jsonb_agg(to_jsonb(r)-'tenant' ORDER BY to_jsonb(r)::text),'[]'::jsonb) FROM {table} r WHERE tenant=$1 AND sku=ANY($2)");
        let rows: Value = sqlx::query_scalar(&sql)
            .bind(TENANT)
            .bind(vec![SKU, EMPTY])
            .fetch_one(&store.pool)
            .await?;
        require(
            rows.as_array().is_some_and(|a| !a.is_empty()),
            "Serial recovery point must populate all six identity tables",
        )?;
        result.insert(table.into(), rows);
    }
    Ok(Value::Object(result))
}
async fn page(store: &Store, view: &str, query: Value) -> Result<Value> {
    let app = App {
        store: store.clone(),
        demo: false,
        credentials: Arc::new(vec![]),
        connections: Connections::default(),
    };
    let q = Ok(Query(serde_json::from_value::<serial_reads::PageQuery>(
        query,
    )?));
    let result = match view {
        "current" => serial_reads::current(State(app), Extension(identity()), q).await,
        "history" => serial_reads::history(State(app), Extension(identity()), q).await,
        "observations" => serial_reads::observations(State(app), Extension(identity()), q).await,
        _ => return Err("Unsupported serial evidence view".into()),
    };
    Ok(application(result)?.0)
}
async fn pages(store: &Store, view: &str, serial: Option<&str>) -> Result<Value> {
    let mut query = json!({"sku":SKU,"limit":2});
    if let Some(s) = serial {
        query["serial"] = json!(s);
    }
    let mut rows = Vec::new();
    let mut cursors = BTreeSet::new();
    let mut review = None;
    loop {
        let p = page(store, view, query.clone()).await?;
        require(
            p["page_source_ok"] == true,
            "Restored selected serial source checks disagree",
        )?;
        if let Some(v) = &review {
            require(
                v == &p["review"],
                "Global serial review changed during retained page traversal",
            )?;
        } else {
            review = Some(p["review"].clone());
        }
        rows.extend(
            p["items"]
                .as_array()
                .ok_or("Missing serial page records")?
                .clone(),
        );
        if let Some(c) = p["next_cursor"].as_str() {
            require(
                cursors.insert(c.to_string()),
                "Serial evidence cursor repeated",
            )?;
            query["after"] = json!(c);
            query["review"] = p["review"].clone();
        } else {
            require(
                p["total_records"].as_u64() == Some(rows.len() as u64),
                "Serial evidence page traversal lost or duplicated records",
            )?;
            break;
        }
    }
    Ok(json!({"view":view,"serial":serial,"review":review,"records":rows}))
}
async fn retained(store: &Store) -> Result<Value> {
    let mut lineage = Vec::new();
    for uid in LABELS {
        lineage.push(pages(store, "history", Some(uid)).await?);
    }
    Ok(
        json!({"facts":facts(store).await?,"current":pages(store,"current",None).await?,"observations":pages(store,"observations",None).await?,"lineage":lineage}),
    )
}
async fn wh_version(store: &Store, w: &str) -> Result<i64> {
    Ok(
        sqlx::query_scalar(
            "SELECT version FROM wms_warehouses WHERE tenant=$1 AND warehouse_id=$2",
        )
        .bind(TENANT)
        .bind(w)
        .fetch_one(&store.pool)
        .await?,
    )
}
async fn physical(store: &Store) -> Result<Value> {
    let mut tx = store.pool.begin().await?;
    sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY")
        .execute(&mut *tx)
        .await?;
    let stock:Value=sqlx::query_scalar("SELECT jsonb_agg(to_jsonb(s) ORDER BY warehouse_id,sku,lot,location) FROM wms_stock s WHERE tenant=$1 AND sku=$2").bind(TENANT).bind(SKU).fetch_one(&mut *tx).await?;
    let movements: Value = sqlx::query_scalar(
        "SELECT jsonb_agg(to_jsonb(m) ORDER BY id) FROM wms_movements m WHERE tenant=$1 AND sku=$2",
    )
    .bind(TENANT)
    .bind(SKU)
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(json!({"stock":stock,"movements":movements}))
}
async fn reject_unchanged(
    store: &Store,
    w: &str,
    r: &Request,
    status: u16,
    who: &Identity,
) -> Result<()> {
    let before = table_evidence(store).await?;
    let error = store
        .execute(who, w, r)
        .await
        .err()
        .ok_or("Invalid restored serial operation unexpectedly committed")?;
    require(
        error.status == status,
        "Restored serial operation returned the wrong rejection",
    )?;
    require(
        table_evidence(store).await? == before,
        "Rejected restored serial work changed a WMS table",
    )
}

pub(super) async fn seed(store: &Store) -> Result<Value> {
    let mut originals = Vec::new();
    for (w, code) in [(SRC, "SER-SRC"), (DST, "SER-DST")] {
        execute(store,&mut originals,w,&format!("restore-serial-create-{code}"),json!({"type":"create_warehouse","code":code,"name":"Synthetic serial recovery facility","city":"Vancouver","timezone":"America/Vancouver"}),vec![]).await?;
        execute(store,&mut originals,w,&format!("restore-serial-product-{code}"),json!({"type":"create_product","sku":SKU,"name":"Synthetic literal serial recovery","category":"Test","reorder_point":0,"serial_controlled":true}),vec![]).await?;
    }
    execute(store,&mut originals,SRC,"restore-serial-empty-mode",json!({"type":"create_product","sku":EMPTY,"name":"Tracked SKU without identities before recovery","category":"Test","reorder_point":0,"serial_controlled":true}),vec![]).await?;
    for bin in ["SER-01", "SER-02", "EMPTY-01"] {
        execute(
            store,
            &mut originals,
            SRC,
            &format!("restore-serial-bin-{bin}"),
            json!({"type":"create_location","code":bin,"zone":"Synthetic","kind":"storage"}),
            vec![],
        )
        .await?;
    }
    let receipt=entity(&execute(store,&mut originals,SRC,"restore-serial-inbound",json!({"type":"create_receipt","reference":"RESTORE-SERIAL-INBOUND","supplier":"Synthetic supplier","lines":[{"id":"IN","sku":SKU,"lot":LOT,"quantity":13}]}),vec![]).await?)?;
    execute(store,&mut originals,SRC,"restore-serial-receive",json!({"type":"receive","receipt_id":receipt,"line_id":"IN","location":"RECV-01","quantity":13}),observed(&[],&LABELS)).await?;
    execute(store,&mut originals,SRC,"restore-serial-putaway",json!({"type":"move_stock","sku":SKU,"lot":LOT,"from":"RECV-01","to":"SER-01","quantity":13}),observed(&[],&LABELS)).await?;
    let definition=entity(&execute(store,&mut originals,SRC,"restore-serial-pair-definition",json!({"type":"set_unit_pack","sku":SKU,"code":"PAIR","label":"Original pair","each_per_pack":2,"enabled":true,"reason":"Synthetic original package label"}),vec![]).await?)?;
    let pair = request(
        store,
        SRC,
        "restore-serial-pair-move",
        json!({"type":"move_stock","sku":SKU,"lot":LOT,"from":"SER-01","to":"SER-02","quantity":2}),
        observed(&[], &["Unit-A", "unit-a"]),
        vec![stateset_warehouse::units::Input {
            target: vec![],
            sku: SKU.into(),
            pack_code: "PAIR".into(),
            definition_id: definition.clone(),
            pack_quantity: 1,
            each_per_pack: 2,
        }],
    )
    .await?;
    let pair_response = application(store.execute(&identity(), SRC, &pair).await)?;
    originals.push(json!({"warehouse_id":SRC,"request":pair,"response":pair_response}));
    let reduced: Vec<_> = LABELS
        .into_iter()
        .filter(|u| !["Unit-A", "unit-a", "MISSING", "FOUND"].contains(u))
        .collect();
    execute(store,&mut originals,SRC,"restore-serial-missing-count",json!({"type":"count_stock","sku":SKU,"lot":LOT,"location":"SER-01","counted":9,"reason":"Two original units absent in a complete count"}),observed(&[],&reduced)).await?;
    let found: Vec<_> = LABELS
        .into_iter()
        .filter(|u| !["Unit-A", "unit-a", "MISSING"].contains(u))
        .collect();
    for (key, reason) in [
        (
            "restore-serial-found-count",
            "Original found unit observed in its original bin",
        ),
        (
            "restore-serial-noop-count",
            "Unchanged complete observation is still original evidence",
        ),
    ] {
        execute(store,&mut originals,SRC,key,json!({"type":"count_stock","sku":SKU,"lot":LOT,"location":"SER-01","counted":10,"reason":reason}),observed(&[],&found)).await?;
    }
    execute(store,&mut originals,SRC,"restore-serial-zero-count",json!({"type":"count_stock","sku":SKU,"lot":LOT,"location":"EMPTY-01","counted":0,"reason":"Witnessed complete empty location"}),observed(&[],&[])).await?;
    let order=entity(&execute(store,&mut originals,SRC,"restore-serial-order",json!({"type":"create_order","reference":"RESTORE-SERIAL-ORDER","source_order_id":null,"customer":"Synthetic customer","destination":"Synthetic destination","priority":"normal","lines":[{"id":"STAGED-LINE","sku":SKU,"quantity":2},{"id":"SHIP-LINE","sku":SKU,"quantity":1},{"id":"RETURN-LINE","sku":SKU,"quantity":2}]}),vec![]).await?)?;
    execute(
        store,
        &mut originals,
        SRC,
        "restore-serial-allocation",
        json!({"type":"allocate_available","order_id":order}),
        vec![],
    )
    .await?;
    for (line, labels) in [
        ("STAGED-LINE", vec!["STAGED", "PACKED"]),
        ("SHIP-LINE", vec!["SHIPPED"]),
        ("RETURN-LINE", vec!["RMA-RETURNED", "RMA-PENDING"]),
    ] {
        execute(store,&mut originals,SRC,&format!("restore-serial-pick-{line}"),json!({"type":"pick","order_id":order,"line_id":line,"location":"SER-01","lot":LOT,"quantity":labels.len()}),observed(&[],&labels)).await?;
    }
    let open_package=entity(&execute(store,&mut originals,SRC,"restore-serial-open-package",json!({"type":"pack_shipment","order_id":order,"reference":"RESTORE-OPEN-SERIAL-PACKAGE","lines":[{"line_id":"STAGED-LINE","location":"SER-01","lot":LOT,"quantity":1}]}),observed(&["STAGED-LINE","SER-01",LOT],&["PACKED"])).await?)?;
    let mut shipped = observed(&["SHIP-LINE", "SER-01", LOT], &["SHIPPED"]);
    shipped.extend(observed(
        &["RETURN-LINE", "SER-01", LOT],
        &["RMA-RETURNED", "RMA-PENDING"],
    ));
    let package=entity(&execute(store,&mut originals,SRC,"restore-serial-shipped-package",json!({"type":"pack_shipment","order_id":order,"reference":"RESTORE-SHIPPED-SERIAL-PACKAGE","lines":[{"line_id":"SHIP-LINE","location":"SER-01","lot":LOT,"quantity":1},{"line_id":"RETURN-LINE","location":"SER-01","lot":LOT,"quantity":2}]}),shipped).await?)?;
    execute(store,&mut originals,SRC,"restore-serial-handoff",json!({"type":"handoff_shipment","shipment_id":package,"carrier":"Synthetic carrier","handoff_reference":"RESTORE-SERIAL-HANDOFF"}),vec![]).await?;
    let rma=entity(&execute(store,&mut originals,SRC,"restore-serial-rma",json!({"type":"create_return","reference":"RESTORE-SERIAL-RMA","order_id":order,"reason":"Exact shipped original identities","lines":[{"id":"RMA-LINE","order_line_id":"RETURN-LINE","lot":LOT,"quantity":2}]}),observed(&["RMA-LINE"],&["RMA-RETURNED","RMA-PENDING"])).await?)?;
    execute(store,&mut originals,SRC,"restore-serial-partial-return",json!({"type":"receive","receipt_id":rma,"line_id":"RMA-LINE","location":"RECV-01","quantity":1}),observed(&[],&["RMA-RETURNED"])).await?;
    let transfer=entity(&execute(store,&mut originals,SRC,"restore-serial-dispatch",json!({"type":"dispatch_transfer","reference":"RESTORE-SERIAL-TRANSFER","destination_warehouse_id":DST,"carrier":"Synthetic carrier","handoff_reference":"RESTORE-SERIAL-DISPATCH","lines":[{"id":"TRANSFER-LINE","sku":SKU,"lot":LOT,"location":"SER-01","quantity":3}]}),observed(&["TRANSFER-LINE"],&["IN-TRANSIT","LOST","ARRIVED"])).await?)?;
    execute(store,&mut originals,DST,"restore-serial-arrival",json!({"type":"receive_transfer","transfer_id":transfer,"line_id":"TRANSFER-LINE","location":"RECV-01","quantity":1,"arrival_reference":"RESTORE-SERIAL-ARRIVED"}),observed(&[],&["ARRIVED"])).await?;
    let loss=entity(&execute(store,&mut originals,DST,"restore-serial-loss",json!({"type":"record_transfer_loss","transfer_id":transfer,"line_id":"TRANSFER-LINE","quantity":1,"incident_reference":"RESTORE-SERIAL-LOST","reason":"Exact original dispatched unit confirmed missing"}),observed(&[],&["LOST"])).await?)?;
    execute(store,&mut originals,SRC,"restore-serial-changed-pair",json!({"type":"set_unit_pack","sku":SKU,"code":"PAIR","label":"Later three-each package","each_per_pack":3,"enabled":true,"reason":"Later label cannot reinterpret original pair and serial observations"}),vec![]).await?;
    let status:Value=sqlx::query_scalar("SELECT jsonb_object_agg(status,quantity) FROM(SELECT place->>'status' AS status,count(*)::bigint AS quantity FROM wms_serial_units WHERE tenant=$1 AND sku=$2 GROUP BY place->>'status') q").bind(TENANT).bind(SKU).fetch_one(&store.pool).await?;
    require(
        status == json!({"on_hand":6,"staged":2,"shipped":2,"in_transit":1,"lost":1,"missing":1}),
        "Serial recovery point must retain all six custody states",
    )?;
    Ok(
        json!({"sku":SKU,"source":SRC,"destination":DST,"order_id":order,"open_package_id":open_package,"rma_id":rma,"transfer_id":transfer,"loss_id":loss,"pair_request":pair,"originals":originals,"status_counts":status,"evidence":retained(store).await?}),
    )
}

pub(super) async fn verify_and_replay(store: &Store, q: &Value) -> Result<()> {
    require(
        q["sku"] == SKU && q["source"] == SRC && q["destination"] == DST,
        "Serial restore fixture identity changed",
    )?;
    require(retained(store).await?==q["evidence"],"Restored serial identity, lot, custody, original history, observations, global review or pages changed")?;
    eprintln!("Restored serial rows, bounded pages and all original unit lineages match; replaying retained requests");
    let before = table_evidence(store).await?;
    for original in q["originals"]
        .as_array()
        .ok_or("Missing original serial requests")?
    {
        let w = original["warehouse_id"]
            .as_str()
            .ok_or("Missing original serial warehouse")?;
        let r: Request = serde_json::from_value(original["request"].clone())?;
        let replay = application(store.execute(&identity(), w, &r).await)?;
        require(
            replay["replayed"] == true && replay["result"] == original["response"]["result"],
            "Original serial/pack request did not replay its exact actor-bound result",
        )?;
    }
    require(
        table_evidence(store).await? == before,
        "Original serial replays changed a WMS table",
    )?;
    require(
        retained(store).await? == q["evidence"],
        "Original replay changed retained serial read evidence",
    )
}

pub(super) async fn recover(store: &Store, q: &Value) -> Result<Value> {
    eprintln!("Serial continuation: checking actor, exact packet, duplicate identity and obsolete pack rejection");
    let mut originals = Vec::new();
    let rma = q["rma_id"].as_str().ok_or("Missing restored RMA")?;
    let transfer = q["transfer_id"]
        .as_str()
        .ok_or("Missing restored transfer")?;
    let original: Request = serde_json::from_value(q["pair_request"].clone())?;
    let mut wrong_actor = identity();
    wrong_actor.actor = "rehearsal:other-actor".into();
    reject_unchanged(store, SRC, &original, 409, &wrong_actor).await?;
    let mut altered = original.clone();
    altered.serial_inputs[0].serials.reverse();
    reject_unchanged(store, SRC, &altered, 409, &identity()).await?;
    let duplicate=request(store,DST,"restore-serial-reject-duplicate",json!({"type":"count_stock","sku":SKU,"lot":LOT,"location":"RECV-01","counted":1,"reason":"Cannot create second custody of a restored original serial"}),observed(&[],&["Unit-A"]),vec![]).await?;
    reject_unchanged(store, DST, &duplicate, 422, &identity()).await?;
    let stale_pack = request(
        store,
        SRC,
        "restore-serial-reject-stale-pair",
        json!({"type":"move_stock","sku":SKU,"lot":LOT,"from":"SER-02","to":"SER-01","quantity":2}),
        observed(&[], &["Unit-A", "unit-a"]),
        original.unit_inputs.clone(),
    )
    .await?;
    reject_unchanged(store, SRC, &stale_pack, 409, &identity()).await?;
    // Destination-only recovery advances the tenant/SKU identity review, while
    // source warehouse/resource versions remain untouched. Reversal creates no stock.
    let cursor = page(
        store,
        "current",
        json!({"sku":SKU,"warehouse_id":SRC,"limit":1}),
    )
    .await?;
    require(
        cursor["next_cursor"].is_string(),
        "Source serial review needs a retained page position",
    )?;
    let source_version = wh_version(store, SRC).await?;
    let before_physical = physical(store).await?;
    execute(store,&mut originals,DST,"restore-serial-new-loss-reversal",json!({"type":"reverse_transfer_loss","loss_id":q["loss_id"],"quantity":1,"reason":"Exact restored missing unit recovered; not yet received"}),observed(&[],&["LOST"])).await?;
    require(
        physical(store).await? == before_physical,
        "Restored serial loss reversal moved physical stock or a movement",
    )?;
    let app = App {
        store: store.clone(),
        demo: false,
        credentials: Arc::new(vec![]),
        connections: Connections::default(),
    };
    let query = serde_json::from_value(
        json!({"sku":SKU,"warehouse_id":SRC,"limit":1,"after":cursor["next_cursor"],"review":cursor["review"]}),
    )?;
    let error = serial_reads::current(State(app), Extension(identity()), Ok(Query(query)))
        .await
        .err()
        .ok_or("Destination recovery failed to expire source serial pages")?;
    require(
        error.status == 409,
        "Restored global serial review returned the wrong stale status",
    )?;
    for label in ["LOST", "IN-TRANSIT"] {
        execute(store,&mut originals,DST,&format!("restore-serial-new-arrival-{label}"),json!({"type":"receive_transfer","transfer_id":transfer,"line_id":"TRANSFER-LINE","location":"RECV-01","quantity":1,"arrival_reference":format!("RESTORE-RECOVERED-{label}")}),observed(&[],&[label])).await?;
    }
    require(
        wh_version(store, SRC).await? == source_version,
        "Restored destination work changed the source warehouse version",
    )?;
    let now = custody(store).await?;
    require(
        now["LOST"]["status"] == "on_hand"
            && now["LOST"]["warehouse_id"] == DST
            && now["IN-TRANSIT"]["warehouse_id"] == DST,
        "Recovered transfer units lost original destination ownership",
    )?;
    eprintln!("Destination recovery preserved source warehouse version and expired its global serial pages");
    // Cancel only the unreceived lease, then authorize/receive that exact unit.
    execute(store,&mut originals,SRC,"restore-serial-new-rma-cancel",json!({"type":"cancel_receipt","receipt_id":rma,"reason":"Release only the original unreceived serial allowance"}),vec![]).await?;
    let now = custody(store).await?;
    require(
        now["RMA-RETURNED"]["status"] == "on_hand"
            && now["RMA-PENDING"]["status"] == "shipped"
            && now["RMA-PENDING"]["return_receipt_id"].is_null(),
        "Partial restored RMA cancellation changed received stock or failed exact lease release",
    )?;
    let new_rma=entity(&execute(store,&mut originals,SRC,"restore-serial-new-rma",json!({"type":"create_return","reference":"RESTORE-SECOND-SERIAL-RMA","order_id":q["order_id"],"reason":"Only released original identity remains eligible","lines":[{"id":"NEW-RMA-LINE","order_line_id":"RETURN-LINE","lot":LOT,"quantity":1}]}),observed(&["NEW-RMA-LINE"],&["RMA-PENDING"])).await?)?;
    execute(store,&mut originals,SRC,"restore-serial-new-return-receive",json!({"type":"receive","receipt_id":new_rma,"line_id":"NEW-RMA-LINE","location":"RECV-01","quantity":1}),observed(&[],&["RMA-PENDING"])).await?;
    execute(store,&mut originals,SRC,"restore-serial-new-unpack",json!({"type":"unpack_shipment","shipment_id":q["open_package_id"],"reason":"Original restored package reviewed before repacking"}),vec![]).await?;
    let package=entity(&execute(store,&mut originals,SRC,"restore-serial-new-repack",json!({"type":"pack_shipment","order_id":q["order_id"],"reference":"RESTORE-REPACKED-SERIALS","lines":[{"line_id":"STAGED-LINE","location":"SER-01","lot":LOT,"quantity":2}]}),observed(&["STAGED-LINE","SER-01",LOT],&["STAGED","PACKED"])).await?)?;
    execute(store,&mut originals,SRC,"restore-serial-new-package-handoff",json!({"type":"handoff_shipment","shipment_id":package,"carrier":"Synthetic recovery carrier","handoff_reference":"RESTORE-SECOND-SERIAL-HANDOFF"}),vec![]).await?;
    execute(
        store,
        &mut originals,
        SRC,
        "restore-serial-new-each-move",
        json!({"type":"move_stock","sku":SKU,"lot":LOT,"from":"SER-02","to":"SER-01","quantity":2}),
        observed(&[], &["Unit-A", "unit-a"]),
    )
    .await?;
    execute(store,&mut originals,SRC,"restore-serial-new-found-count",json!({"type":"count_stock","sku":SKU,"lot":LOT,"location":"SER-01","counted":5,"reason":"Complete observed set finds the same original missing identity"}),observed(&[],&["Unit-A","unit-a","FOUND","LOCAL","MISSING"])).await?;
    execute(store,&mut originals,SRC,"restore-serial-first-empty-sku-observation",json!({"type":"count_stock","sku":EMPTY,"lot":"EMPTY-SKU-LOT","location":"EMPTY-01","counted":1,"reason":"Restored zero head accepts its first real identity without inferring old stock"}),vec![Input{target:vec![],sku:EMPTY.into(),serials:vec!["Unit-A".into()]}]).await?;
    execute(store,&mut originals,EXTRA,"restore-serial-new-empty-facility",json!({"type":"create_warehouse","code":"SER-EXTRA","name":"Synthetic recovered extra facility","city":"Vancouver","timezone":"UTC"}),vec![]).await?;
    let changed_mode=request(store,EXTRA,"restore-serial-reject-mode-change",json!({"type":"create_product","sku":SKU,"name":"Invalid changed mode","category":"Test","reorder_point":0,"serial_controlled":false}),vec![],vec![]).await?;
    reject_unchanged(store, EXTRA, &changed_mode, 422, &identity()).await?;
    eprintln!("Serial continuation: RMA cancel/replacement, unpack/repack/handoff, found count and empty-head birth passed; replaying all requests");
    let before = table_evidence(store).await?;
    for original in originals.iter().chain(
        q["originals"]
            .as_array()
            .ok_or("Missing original serial requests")?,
    ) {
        let r: Request = serde_json::from_value(original["request"].clone())?;
        let replay = application(
            store
                .execute(
                    &identity(),
                    original["warehouse_id"]
                        .as_str()
                        .ok_or("Missing serial replay facility")?,
                    &r,
                )
                .await,
        )?;
        require(
            replay["replayed"] == true && replay["result"] == original["response"]["result"],
            "Serial history continuation changed an original retry",
        )?;
    }
    require(
        table_evidence(store).await? == before,
        "Old or new serial retries changed a WMS table after ownership continuation",
    )?;
    let final_custody = custody(store).await?;
    require(
        final_custody["STAGED"]["status"] == "shipped"
            && final_custody["PACKED"]["status"] == "shipped"
            && final_custody["RMA-PENDING"]["status"] == "on_hand"
            && final_custody["MISSING"]["status"] == "on_hand",
        "Serial ownership continuation did not retain exact original units",
    )?;
    let final_snapshot = snapshot(store).await?;
    Ok(
        json!({"ok":true,"rejected_actor_payload_duplicate_mode_and_obsolete_pack_changed_no_tables":true,"loss_reversal_moved_no_stock_or_movements":true,"destination_changes_expired_global_source_pages_without_changing_source_warehouse":true,"exact_lost_and_in_transit_units_arrived":true,"partial_rma_cancel_reauthorize_receive_preserved_original_unit":true,"unpack_repack_handoff_preserved_exact_allocation_owners":true,"missing_found_count_preserved_original_lot":true,"restored_zero_head_accepted_first_identity":true,"all_old_and_new_original_replays_changed_no_tables":true,"custody":final_custody,"global_serial_facts":facts(store).await?,"all_warehouse_proofs":final_snapshot["warehouses"]}),
    )
}
