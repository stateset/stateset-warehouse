//! Original relational workflow fixtures for the unregistered serial schema.
//! These deliberately bypass the Rust planner. Placeholder command digests do
//! not establish authenticated HTTP admission or canonical request-byte proof.
use super::{domain, raw_staged_order, raw_staged_order_for, PgConnection};
use serde_json::{json, Value};

struct Event {
    id: String,
    at: String,
    warehouse: &'static str,
    lot_changes: Value,
    loss: Value,
    previous_order: Value,
    command: Value,
    kind: &'static str,
    entity: String,
    inputs: Value,
    // (literal UID, original observation target, resulting custody)
    effects: Vec<(String, Value, Value)>,
    shipment: Value,
}

async fn begin(c: &mut PgConnection) -> Event {
    begin_in(c, "main").await
}

async fn begin_in(c: &mut PgConnection, warehouse: &'static str) -> Event {
    let at = domain::now();
    let previous_order = sqlx::query_scalar(
        "SELECT COALESCE(wms_current_order('serial-test',$1,'ORDER'),'null'::jsonb)",
    )
    .bind(warehouse)
    .fetch_one(&mut *c)
    .await
    .expect("actual preceding order ownership, if present");
    sqlx::query("UPDATE wms_warehouses SET version=version+1 WHERE tenant='serial-test' AND warehouse_id=$1")
        .bind(warehouse).execute(&mut *c).await.expect("independent original warehouse command");
    Event {
        id: domain::id(),
        at,
        warehouse,
        previous_order,
        command: Value::Null,
        kind: "",
        entity: String::new(),
        inputs: json!([]),
        effects: vec![],
        shipment: Value::Null,
        lot_changes: json!([]),
        loss: Value::Null,
    }
}

async fn record(c: &mut PgConnection, event: Event) -> String {
    let warehouse_version: i64 = sqlx::query_scalar(
        "SELECT version FROM wms_warehouses WHERE tenant='serial-test' AND warehouse_id=$1",
    )
    .bind(event.warehouse)
    .fetch_one(&mut *c)
    .await
    .expect("actual warehouse head");
    let version: i64 = sqlx::query_scalar(
        "SELECT version+1 FROM wms_serial_heads WHERE tenant='serial-test' AND sku='CONTROLLED'",
    )
    .fetch_one(&mut *c)
    .await
    .expect("independent tenant/SKU review");
    let request_id = format!("raw-serial-workflow-{}", event.id);
    sqlx::query("UPDATE wms_serial_heads SET version=$1,last_event_id=$2 WHERE tenant='serial-test' AND sku='CONTROLLED'")
        .bind(version).bind(&event.id).execute(&mut *c).await.expect("advance original serial review");
    sqlx::query("INSERT INTO wms_serial_batches VALUES('serial-test','CONTROLLED',$1,$2,$3,$4)")
        .bind(version)
        .bind(event.warehouse)
        .bind(warehouse_version)
        .bind(&event.id)
        .execute(&mut *c)
        .await
        .expect("original serial batch");
    for (index, (uid, target, after)) in event.effects.iter().enumerate() {
        let (revision, before): (i64, Value) = sqlx::query_as("SELECT revision,place FROM wms_serial_units WHERE tenant='serial-test' AND sku='CONTROLLED' AND serial=$1")
            .bind(uid).fetch_one(&mut *c).await.expect("exact previous UID and original owner");
        sqlx::query("UPDATE wms_serial_units SET revision=$1,place=$2,last_event_id=$3 WHERE tenant='serial-test' AND sku='CONTROLLED' AND serial=$4")
            .bind(revision+1).bind(after).bind(&event.id).bind(uid).execute(&mut *c).await.expect("actual current identity");
        sqlx::query("INSERT INTO wms_serial_steps VALUES('serial-test','CONTROLLED',$1,'LOT',$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,'synthetic:admin',$12)")
            .bind(uid).bind(revision+1).bind(revision).bind(version).bind(event.warehouse).bind(warehouse_version).bind(&event.id).bind((index+1) as i32).bind(target).bind(before).bind(after).bind(&event.at)
            .execute(&mut *c).await.expect("immutable original per-unit effect");
    }
    for (index, input) in event
        .inputs
        .as_array()
        .expect("original observation array")
        .iter()
        .enumerate()
    {
        sqlx::query(
            "INSERT INTO wms_serial_observations VALUES('serial-test',$1,$2,$3,$4,$5,$6,$7)",
        )
        .bind(event.warehouse)
        .bind(&event.id)
        .bind((index + 1) as i32)
        .bind(input["sku"].as_str().expect("observed SKU"))
        .bind(version)
        .bind(&input["target"])
        .bind(&input["serials"])
        .execute(&mut *c)
        .await
        .expect("original observed UID/target group");
    }
    let steps: Value=sqlx::query_scalar("SELECT COALESCE(jsonb_agg(to_jsonb(s)-'tenant' ORDER BY ordinal),'[]'::jsonb) FROM wms_serial_steps s WHERE tenant='serial-test' AND event_id=$1")
        .bind(&event.id).fetch_one(&mut *c).await.expect("original exact normalized effect bytes");
    let inventory: Value=sqlx::query_scalar("SELECT COALESCE(jsonb_agg(jsonb_build_object('stock',jsonb_build_object('sku',sku,'lot',lot,'location',location,'on_hand',on_hand,'reserved',reserved),'version',$1) ORDER BY location),'[]'::jsonb) FROM wms_stock WHERE tenant='serial-test' AND warehouse_id=$2 AND sku='CONTROLLED'")
        .bind(warehouse_version).bind(event.warehouse).fetch_one(&mut *c).await.expect("independent current physical balances");
    let order: Value = sqlx::query_scalar(
        "SELECT COALESCE(wms_current_order('serial-test',$1,'ORDER'),'null'::jsonb)",
    )
    .bind(event.warehouse)
    .fetch_one(&mut *c)
    .await
    .expect("independent current order ownership, if present");
    let decisions: Value=sqlx::query_scalar("SELECT COALESCE(jsonb_agg(wms_lot_decision_document(kind,sku,lot,checked_at,required,document) ORDER BY kind,sku,lot),'[]'::jsonb) FROM wms_lot_decisions WHERE tenant='serial-test' AND warehouse_id=$2 AND version=$1")
        .bind(warehouse_version).bind(event.warehouse).fetch_one(&mut *c).await.expect("original lot eligibility observations");
    let result = json!({"entity_id":event.entity,"version":warehouse_version,"type":event.kind});
    let receipt: Value=sqlx::query_scalar("SELECT COALESCE((SELECT data FROM wms_receipts WHERE tenant='serial-test' AND warehouse_id=$2 AND id=$1),'null'::jsonb)")
        .bind(&event.entity).bind(event.warehouse).fetch_one(&mut *c).await.expect("original complete receipt document when applicable");
    let payload = json!({"schema_version":2,"id":event.id,"tenant":"serial-test","warehouse_id":event.warehouse,"version":warehouse_version,"actor":"synthetic:admin","occurred_at":event.at,"request_id":request_id,"type":event.kind,"result":result,
        "command":event.command,"inventory_changes":inventory,"order":order,"previous_order":event.previous_order,"shipment":event.shipment,"receipt":receipt,"transfer_loss_record":event.loss,"lot_changes":event.lot_changes,"lot_decisions":decisions,"serial_heads":{"CONTROLLED":version},"serial_steps":steps,"serial_inputs":event.inputs});
    sqlx::query("INSERT INTO wms_events(id,tenant,warehouse_id,version,payload) VALUES($1,'serial-test',$2,$3,$4)")
        .bind(&event.id).bind(event.warehouse).bind(warehouse_version).bind(payload).execute(&mut *c).await.expect("complete immutable original command event");
    sqlx::query("INSERT INTO wms_commands(tenant,warehouse_id,request_id,actor,digest,result) VALUES('serial-test',$1,$2,'synthetic:admin',$3,$4)")
        .bind(event.warehouse).bind(request_id).bind("0".repeat(64)).bind(result).execute(&mut *c).await.expect("raw attributed receipt, not HTTP byte proof");
    // Compare every facility after each raw workflow mutation, including
    // deliberately invalid custody, before deferred guards accept or reject it.
    // This exercises on-hand, staged/package, shipped, missing and transit/loss
    // predicates against the immutable predecessor with the same original rows.
    let outcomes: Vec<(String, Option<bool>, Option<bool>)> = sqlx::query_as(
        "SELECT warehouse_id,wms_serial_scope_v29_oracle(tenant,warehouse_id),wms_serial_scope_current_valid(tenant,warehouse_id) FROM wms_warehouses WHERE tenant='serial-test'",
    )
    .fetch_all(&mut *c)
    .await
    .expect("complete custody predicate differential");
    for (warehouse, original, current) in outcomes {
        assert_eq!(current, original, "custody verdict changed at {warehouse}");
    }
    event.id
}

async fn settle(c: &mut PgConnection) {
    sqlx::raw_sql("SET CONSTRAINTS ALL IMMEDIATE; SET CONSTRAINTS ALL DEFERRED")
        .execute(&mut *c)
        .await
        .expect("complete original custody, stock, package and lot proofs");
}

fn staged(package: Option<&str>, line: &str) -> Value {
    json!({"status":"staged","warehouse_id":"main","order_id":"ORDER","line_id":line,"allocation_location":"A","package_id":package})
}

async fn pack(c: &mut PgConnection, package: &str, line: &str) -> String {
    pack_observed(c, package, line, line).await
}

async fn pack_observed(
    c: &mut PgConnection,
    package: &str,
    line: &str,
    observed_line: &str,
) -> String {
    let mut event = begin(c).await;
    let version: i64 = sqlx::query_scalar(
        "SELECT version FROM wms_warehouses WHERE tenant='serial-test' AND warehouse_id='main'",
    )
    .fetch_one(&mut *c)
    .await
    .expect("packing version");
    sqlx::query("UPDATE wms_orders SET data=data||'{\"shipment_mode\":\"packages\"}' WHERE tenant='serial-test' AND warehouse_id='main' AND id='ORDER'")
        .execute(&mut *c).await.expect("explicit package fulfillment mode");
    sqlx::query("INSERT INTO wms_split_orders SELECT 'serial-test','main','ORDER',$1,$2,wms_split_contract(wms_current_order('serial-test','main','ORDER')) WHERE NOT EXISTS(SELECT 1 FROM wms_split_orders WHERE tenant='serial-test' AND warehouse_id='main' AND order_id='ORDER')")
        .bind(&event.id).bind(version).execute(&mut *c).await.expect("immutable original split order contract");
    sqlx::query("INSERT INTO wms_packages(tenant,warehouse_id,id,order_id,reference,actor,packed_at,source_event_id,source_version) VALUES('serial-test','main',$1,'ORDER',$1,'synthetic:admin',$2,$3,$4)")
        .bind(package).bind(&event.at).bind(&event.id).bind(version).execute(&mut *c).await.expect("original immutable package");
    sqlx::query("INSERT INTO wms_package_lines VALUES('serial-test','main',$1,'ORDER',$2,'CONTROLLED','A','LOT',1)")
        .bind(package).bind(line).execute(&mut *c).await.expect("exact original package line/bin/lot");
    event.command = json!({"type":"pack_shipment","order_id":"ORDER","reference":package,"lines":[{"line_id":line,"location":"A","lot":"LOT","quantity":1}]});
    event.kind = "shipment.packed";
    event.entity = package.into();
    event.inputs =
        json!([{"target":[observed_line,"A","LOT"],"sku":"CONTROLLED","serials":["ONE"]}]);
    event.effects = vec![(
        "ONE".into(),
        json!([line, "A", "LOT"]),
        staged(Some(package), line),
    )];
    event.shipment = sqlx::query_scalar("SELECT wms_package_document('serial-test','main',$1)")
        .bind(package)
        .fetch_one(&mut *c)
        .await
        .expect("original package source document");
    record(c, event).await
}

async fn close(c: &mut PgConnection, package: &str, handoff: bool, omit_effect: bool) -> String {
    let mut event = begin(c).await;
    let version: i64 = sqlx::query_scalar(
        "SELECT version FROM wms_warehouses WHERE tenant='serial-test' AND warehouse_id='main'",
    )
    .fetch_one(&mut *c)
    .await
    .expect("closure version");
    let (kind, reason, carrier, reference) = if handoff {
        ("handed_off", "", "Synthetic carrier", "SYNTHETIC-HANDOFF")
    } else {
        ("unpacked", "Actual package contents reopened", "", "")
    };
    sqlx::query("INSERT INTO wms_package_closures VALUES('serial-test','main',$1,'ORDER',$2,$3,$4,$5,'synthetic:admin',$6,$7,$8)")
        .bind(package).bind(kind).bind(reason).bind(carrier).bind(reference).bind(&event.at).bind(&event.id).bind(version).execute(&mut *c).await.expect("original exact package closure");
    if handoff {
        sqlx::raw_sql("UPDATE wms_allocations SET shipped=1 WHERE tenant='serial-test' AND warehouse_id='main' AND order_id='ORDER' AND line_id='LINE-1'; UPDATE wms_stock SET on_hand=0,reserved=0 WHERE tenant='serial-test' AND warehouse_id='main' AND sku='CONTROLLED' AND location='STAGE-01'")
            .execute(&mut *c).await.expect("independent physical handoff and allocation execution");
        sqlx::query("UPDATE wms_orders SET status='partially_shipped',data=data||jsonb_build_object('status','partially_shipped','carrier',$1::text,'handoff_reference',$2::text,'shipped_at',$3::text) WHERE tenant='serial-test' AND warehouse_id='main' AND id='ORDER'")
            .bind(carrier).bind(reference).bind(&event.at).execute(&mut *c).await.expect("actual partial order handoff header");
        let id = domain::id();
        let data = json!({"id":id,"kind":"shipment","sku":"CONTROLLED","lot":"LOT","from":"STAGE-01","to":null,"quantity":1,"reference":"Synthetic order","receipt_id":null,"receipt_line_id":null,"reason":reference,"actor":"synthetic:admin","at":event.at,"package_id":package,"order_line_id":"LINE-1","allocation_location":"A","shipment_event_id":event.id});
        sqlx::query("INSERT INTO wms_movements(id,tenant,warehouse_id,sku,lot,from_location,to_location,quantity,kind,reference,package_id,order_line_id,allocation_location,shipment_event_id,data) VALUES($1,'serial-test','main','CONTROLLED','LOT','STAGE-01',NULL,1,'shipment','Synthetic order',$2,'LINE-1','A',$3,$4)")
            .bind(id).bind(package).bind(&event.id).bind(data).execute(&mut *c).await.expect("exact original package handoff movement");
        event.command = json!({"type":"handoff_shipment","shipment_id":package,"carrier":carrier,"handoff_reference":reference});
        event.kind = "shipment.handed_off";
        if !omit_effect {
            event.effects = vec![(
                "ONE".into(),
                json!([]),
                json!({"status":"shipped","warehouse_id":"main","order_id":"ORDER","line_id":"LINE-1","package_id":package,"handoff_event_id":event.id,"return_receipt_id":null}),
            )];
        }
    } else {
        event.command = json!({"type":"unpack_shipment","shipment_id":package,"reason":reason});
        event.kind = "shipment.unpacked";
        if !omit_effect {
            event.effects = vec![("ONE".into(), json!([]), staged(None, "LINE-1"))];
        }
    }
    event.entity = package.into();
    event.shipment = sqlx::query_scalar("SELECT wms_package_document('serial-test','main',$1)")
        .bind(package)
        .fetch_one(&mut *c)
        .await
        .expect("original closed package document");
    record(c, event).await
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL; development schema is not registered"]
async fn package_unpack_repack_and_handoff_retain_original_serial_ownership() {
    let mut tx = raw_staged_order().await;
    pack(&mut tx, "PACKAGE-1", "LINE-1").await;
    settle(&mut tx).await;
    close(&mut tx, "PACKAGE-1", false, false).await;
    settle(&mut tx).await;
    pack(&mut tx, "PACKAGE-2", "LINE-1").await;
    settle(&mut tx).await;
    let handoff = close(&mut tx, "PACKAGE-2", true, false).await;
    settle(&mut tx).await;
    let actual: Value=sqlx::query_scalar("SELECT place FROM wms_serial_units WHERE tenant='serial-test' AND sku='CONTROLLED' AND serial='ONE'")
        .fetch_one(&mut *tx).await.expect("actual final original customer custody");
    assert_eq!(
        actual,
        json!({"status":"shipped","warehouse_id":"main","order_id":"ORDER","line_id":"LINE-1","package_id":"PACKAGE-2","handoff_event_id":handoff,"return_receipt_id":null})
    );
    let proof: bool=sqlx::query_scalar("SELECT wms_split_consistent('serial-test','main','ORDER') AND wms_serial_scope_current_valid('serial-test','main') AND (SELECT bool_and(wms_serial_event_valid(tenant,warehouse_id,id)) FROM wms_events)")
        .fetch_one(&mut *tx).await.expect("all original package and serial history proofs");
    assert!(proof);
    tx.rollback().await.expect("discard private schema");
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL; development schema is not registered"]
async fn package_observation_cannot_substitute_a_different_same_sku_line() {
    let mut tx = raw_staged_order().await;
    sqlx::query("SAVEPOINT wrong_package_target")
        .execute(&mut *tx)
        .await
        .expect("independent forged original observation");
    let id = pack_observed(&mut tx, "WRONG-TARGET", "LINE-1", "LINE-2").await;
    let actual: bool=sqlx::query_scalar("SELECT wms_split_consistent('serial-test','main','ORDER') AND wms_serial_scope_current_valid('serial-test','main')")
        .fetch_one(&mut *tx).await.expect("all actual original package and physical quantities remain correct");
    assert!(actual);
    let claimed: bool =
        sqlx::query_scalar("SELECT wms_serial_event_valid('serial-test','main',$1)")
            .bind(id)
            .fetch_one(&mut *tx)
            .await
            .expect("check claimed same-SKU observation owner independently");
    assert!(!claimed);
    let error = sqlx::query("SET CONSTRAINTS ALL IMMEDIATE")
        .execute(&mut *tx)
        .await
        .expect_err("wrong original package target must fail admission");
    assert_eq!(
        error.as_database_error().and_then(|e| e.code()).as_deref(),
        Some("23514")
    );
    sqlx::raw_sql(
        "ROLLBACK TO SAVEPOINT wrong_package_target; RELEASE SAVEPOINT wrong_package_target",
    )
    .execute(&mut *tx)
    .await
    .expect("restore independently correct staging");
    pack(&mut tx, "CORRECT-TARGET", "LINE-1").await;
    settle(&mut tx).await;
    tx.rollback().await.expect("discard private schema");
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL; development schema is not registered"]
async fn unpack_and_handoff_require_every_original_package_identity_effect() {
    let mut tx = raw_staged_order().await;
    pack(&mut tx, "ORIGINAL-PACKAGE", "LINE-1").await;
    settle(&mut tx).await;
    for handoff in [false, true] {
        sqlx::query("SAVEPOINT omitted_package_identity")
            .execute(&mut *tx)
            .await
            .expect("independent complete closure with omitted UID effect");
        let id = close(&mut tx, "ORIGINAL-PACKAGE", handoff, true).await;
        let package: bool = sqlx::query_scalar(
            "SELECT wms_split_consistent('serial-test','main','ORDER')",
        )
        .fetch_one(&mut *tx)
        .await
        .expect("normal stock, allocation, original package/closure and handoff movement proofs");
        assert!(package);
        let serial: bool =
            sqlx::query_scalar("SELECT wms_serial_event_valid('serial-test','main',$1)")
                .bind(id)
                .fetch_one(&mut *tx)
                .await
                .expect("independent complete original UID closure check");
        assert!(
            !serial,
            "an empty observation array cannot establish an empty original package"
        );
        let error=sqlx::query("SET CONSTRAINTS ALL IMMEDIATE").execute(&mut *tx).await.expect_err("complete physical/package evidence still requires the unit's derived custody effect");
        assert_eq!(
            error.as_database_error().and_then(|e| e.code()).as_deref(),
            Some("23514")
        );
        sqlx::raw_sql("ROLLBACK TO SAVEPOINT omitted_package_identity; RELEASE SAVEPOINT omitted_package_identity").execute(&mut *tx).await.expect("restore active original package");
    }
    close(&mut tx, "ORIGINAL-PACKAGE", true, false).await;
    settle(&mut tx).await;
    tx.rollback().await.expect("discard private schema");
}

async fn authorize_return(
    c: &mut PgConnection,
    receipt: &str,
    claimed_line: &str,
    claimed_lot: &str,
) -> String {
    authorize_return_lines(c, receipt, claimed_line, claimed_lot, false).await
}

async fn authorize_return_lines(
    c: &mut PgConnection,
    receipt: &str,
    claimed_line: &str,
    claimed_lot: &str,
    duplicate: bool,
) -> String {
    let mut event = begin(c).await;
    let reason = "Original handed-off package contents";
    let document = json!({"id":receipt,"reference":receipt,"supplier":"Customer return","status":"expected","created_at":event.at,
        "lines":[{"id":"RMA-LINE","sku":"CONTROLLED","lot":"LOT","quantity":1,"received":0}],
        "return_source":{"order_id":"ORDER","reason":reason,"line_ids":{"RMA-LINE":"LINE-1"}}});
    sqlx::query("INSERT INTO wms_receipts(tenant,warehouse_id,id,reference,status,data) VALUES('serial-test','main',$1,$1,'expected',$2)")
        .bind(receipt).bind(document).execute(&mut *c).await.expect("original expected return receipt");
    sqlx::query("INSERT INTO wms_receipt_lines VALUES('serial-test','main',$1,'RMA-LINE','CONTROLLED','LOT',1,0)")
        .bind(receipt).execute(&mut *c).await.expect("exact original return expectation");
    sqlx::query("INSERT INTO wms_return_lines VALUES('serial-test','main',$1,'RMA-LINE','ORDER','LINE-1','CONTROLLED','LOT',1,$2)")
        .bind(receipt).bind(reason).execute(&mut *c).await.expect("independent original shipped line/lot authorization");
    let mut place: Value=sqlx::query_scalar("SELECT place FROM wms_serial_units WHERE tenant='serial-test' AND sku='CONTROLLED' AND serial='ONE'")
        .fetch_one(&mut *c).await.expect("actual original customer custody");
    place["return_receipt_id"] = json!(receipt);
    let line = json!({"id":"RMA-LINE","order_line_id":claimed_line,"lot":claimed_lot,"quantity":1});
    let lines = if duplicate {
        json!([line.clone(), line])
    } else {
        json!([line])
    };
    event.command = json!({"type":"create_return","reference":receipt,"order_id":"ORDER","reason":reason,"lines":lines});
    event.kind = "return.authorized";
    event.entity = receipt.into();
    event.inputs = json!([{"target":["RMA-LINE"],"sku":"CONTROLLED","serials":["ONE"]}]);
    event.effects = vec![("ONE".into(), json!(["RMA-LINE"]), place)];
    record(c, event).await
}

async fn cancel_return(c: &mut PgConnection, receipt: &str) -> String {
    let mut event = begin(c).await;
    let reason = "Customer will not return this authorized unit";
    let cancellation = json!({"reason":reason,"actor":"synthetic:admin","cancelled_at":event.at,
        "lines":{"RMA-LINE":{"sku":"CONTROLLED","lot":"LOT","quantity":1,"received":0,"cancelled":1}}});
    sqlx::query("INSERT INTO wms_receipt_cancellations VALUES('serial-test','main',$1,$2,'synthetic:admin',$3)")
        .bind(receipt).bind(reason).bind(&event.at).execute(&mut *c).await.expect("immutable actual cancelled remainder source");
    sqlx::query("INSERT INTO wms_receipt_cancelled_lines VALUES('serial-test','main',$1,'RMA-LINE','CONTROLLED','LOT',1,0,1)")
        .bind(receipt).execute(&mut *c).await.expect("exact unreceived authorization released");
    sqlx::query("UPDATE wms_receipts SET status='cancelled',data=data||jsonb_build_object('status','cancelled','cancellation',$1::jsonb) WHERE tenant='serial-test' AND warehouse_id='main' AND id=$2")
        .bind(cancellation).bind(receipt).execute(&mut *c).await.expect("original receipt closure document");
    let mut place: Value=sqlx::query_scalar("SELECT place FROM wms_serial_units WHERE tenant='serial-test' AND sku='CONTROLLED' AND serial='ONE'")
        .fetch_one(&mut *c).await.expect("unreceived customer unit still belongs to original handoff");
    place["return_receipt_id"] = Value::Null;
    event.command = json!({"type":"cancel_receipt","receipt_id":receipt,"reason":reason});
    event.kind = "receipt.cancelled";
    event.entity = receipt.into();
    event.effects = vec![("ONE".into(), json!([]), place)];
    record(c, event).await
}

async fn receive_return(c: &mut PgConnection, receipt: &str) -> String {
    let mut event = begin(c).await;
    sqlx::query("UPDATE wms_receipt_lines SET received=1 WHERE tenant='serial-test' AND warehouse_id='main' AND receipt_id=$1 AND id='RMA-LINE'")
        .bind(receipt).execute(&mut *c).await.expect("actual original returned-line execution");
    sqlx::query("UPDATE wms_receipts SET status='received',data=jsonb_set(data||'{\"status\":\"received\"}','{lines,0,received}','1') WHERE tenant='serial-test' AND warehouse_id='main' AND id=$1")
        .bind(receipt).execute(&mut *c).await.expect("fully received return document");
    sqlx::query("UPDATE wms_stock SET on_hand=on_hand+1 WHERE tenant='serial-test' AND warehouse_id='main' AND sku='CONTROLLED' AND lot='LOT' AND location='RECEIVING'")
        .execute(&mut *c).await.expect("actual returned physical custody");
    let id = domain::id();
    let data = json!({"id":id,"kind":"receipt","sku":"CONTROLLED","lot":"LOT","from":null,"to":"RECEIVING","quantity":1,"reference":receipt,"receipt_id":receipt,"receipt_line_id":"RMA-LINE","reason":"Observed original returned unit","actor":"synthetic:admin","at":event.at});
    sqlx::query("INSERT INTO wms_movements(id,tenant,warehouse_id,sku,lot,from_location,to_location,quantity,kind,reference,receipt_id,receipt_line_id,data) VALUES($1,'serial-test','main','CONTROLLED','LOT',NULL,'RECEIVING',1,'receipt',$2,$2,'RMA-LINE',$3)")
        .bind(id).bind(receipt).bind(data).execute(&mut *c).await.expect("independent actual returned-unit receipt evidence");
    event.command = json!({"type":"receive","receipt_id":receipt,"line_id":"RMA-LINE","location":"RECEIVING","quantity":1});
    event.kind = "receipt.received";
    event.entity = receipt.into();
    event.inputs = json!([{"target":[],"sku":"CONTROLLED","serials":["ONE"]}]);
    event.effects = vec![(
        "ONE".into(),
        json!([]),
        json!({"status":"on_hand","warehouse_id":"main","location":"RECEIVING"}),
    )];
    record(c, event).await
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL; development schema is not registered"]
async fn return_authorization_cannot_relabel_the_original_command_line_or_lot() {
    let mut tx = raw_staged_order().await;
    pack(&mut tx, "ORIGINAL-SHIPPED-PACKAGE", "LINE-1").await;
    settle(&mut tx).await;
    close(&mut tx, "ORIGINAL-SHIPPED-PACKAGE", true, false).await;
    settle(&mut tx).await;
    for (claimed_line, claimed_lot, duplicate) in [
        ("LINE-2", "LOT", false),
        ("LINE-1", "OTHER-LOT", false),
        ("LINE-1", "LOT", true),
    ] {
        sqlx::query("SAVEPOINT relabelled_return_command")
            .execute(&mut *tx)
            .await
            .expect("forensic original command/source mismatch");
        authorize_return_lines(
            &mut tx,
            "WRONG-SOURCE",
            claimed_line,
            claimed_lot,
            duplicate,
        )
        .await;
        let actual: bool=sqlx::query_scalar("SELECT wms_shipment_consistent('serial-test','main','ORDER') AND wms_serial_scope_current_valid('serial-test','main')")
            .fetch_one(&mut *tx).await.expect("original actual package, return allowance, UID and physical custody remain balanced");
        assert!(actual);
        let error = sqlx::query("SET CONSTRAINTS ALL IMMEDIATE")
            .execute(&mut *tx)
            .await
            .expect_err("original command cannot relabel or repeat its exact same-SKU shipped line/lot target");
        assert_eq!(
            error.as_database_error().and_then(|e| e.code()).as_deref(),
            Some("23514")
        );
        sqlx::raw_sql("ROLLBACK TO SAVEPOINT relabelled_return_command; RELEASE SAVEPOINT relabelled_return_command").execute(&mut *tx).await.expect("restore original unreserved customer custody");
    }
    authorize_return(&mut tx, "CANCELLED-RMA", "LINE-1", "LOT").await;
    settle(&mut tx).await;
    cancel_return(&mut tx, "CANCELLED-RMA").await;
    settle(&mut tx).await;
    authorize_return(&mut tx, "RECEIVED-RMA", "LINE-1", "LOT").await;
    settle(&mut tx).await;
    receive_return(&mut tx, "RECEIVED-RMA").await;
    settle(&mut tx).await;
    let all: bool = sqlx::query_scalar(
        "SELECT bool_and(wms_serial_event_valid(tenant,warehouse_id,id)) FROM wms_events",
    )
    .fetch_one(&mut *tx)
    .await
    .expect("all original serial and RMA sources survive later actual receipt");
    assert!(all);
    let place: Value=sqlx::query_scalar("SELECT place FROM wms_serial_units WHERE tenant='serial-test' AND sku='CONTROLLED' AND serial='ONE'")
        .fetch_one(&mut *tx).await.expect("actual final returned physical custody");
    assert_eq!(
        place,
        json!({"status":"on_hand","warehouse_id":"main","location":"RECEIVING"})
    );
    tx.rollback().await.expect("discard private schema");
}

async fn classic_ship(c: &mut PgConnection, omit_effect: bool) -> String {
    let mut event = begin(c).await;
    let carrier = "Synthetic full-order carrier";
    let reference = "SYNTHETIC-FULL-HANDOFF";
    sqlx::raw_sql("UPDATE wms_allocations SET shipped=1 WHERE tenant='serial-test' AND warehouse_id='main' AND order_id='ORDER' AND line_id='LINE-1'; UPDATE wms_stock SET on_hand=0,reserved=0 WHERE tenant='serial-test' AND warehouse_id='main' AND sku='CONTROLLED' AND location='STAGE-01'")
        .execute(&mut *c).await.expect("actual whole-order staging debit and allocation handoff");
    sqlx::query("UPDATE wms_orders SET status='shipped',data=data||jsonb_build_object('status','shipped','carrier',$1::text,'handoff_reference',$2::text,'shipped_at',$3::text) WHERE tenant='serial-test' AND warehouse_id='main' AND id='ORDER'")
        .bind(carrier).bind(reference).bind(&event.at).execute(&mut *c).await.expect("actual complete original shipment header");
    let id = domain::id();
    let data = json!({"id":id,"kind":"shipment","sku":"CONTROLLED","lot":"LOT","from":"STAGE-01","to":null,"quantity":1,"reference":"Synthetic order","receipt_id":null,"receipt_line_id":null,"reason":reference,"actor":"synthetic:admin","at":event.at});
    sqlx::query("INSERT INTO wms_movements(id,tenant,warehouse_id,sku,lot,from_location,to_location,quantity,kind,reference,data) VALUES($1,'serial-test','main','CONTROLLED','LOT','STAGE-01',NULL,1,'shipment','Synthetic order',$2)")
        .bind(id).bind(data).execute(&mut *c).await.expect("independent full-order physical handoff movement");
    event.command =
        json!({"type":"ship","order_id":"ORDER","carrier":carrier,"handoff_reference":reference});
    event.kind = "fulfillment.shipped";
    event.entity = "ORDER".into();
    if !omit_effect {
        event.effects = vec![(
            "ONE".into(),
            json!([]),
            json!({"status":"shipped","warehouse_id":"main","order_id":"ORDER","line_id":"LINE-1","package_id":null,"handoff_event_id":event.id,"return_receipt_id":null}),
        )];
    }
    record(c, event).await
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL; development schema is not registered"]
async fn classic_whole_order_handoff_requires_the_complete_original_serial_set() {
    let mut tx = raw_staged_order_for(false).await;
    sqlx::query("SAVEPOINT omitted_classic_identity")
        .execute(&mut *tx)
        .await
        .expect("complete physical shipment with missing UID effect");
    let id = classic_ship(&mut tx, true).await;
    let shipment: bool =
        sqlx::query_scalar("SELECT wms_shipment_consistent('serial-test','main','ORDER')")
            .fetch_one(&mut *tx)
            .await
            .expect("original full-order source and movement are independently consistent");
    assert!(shipment);
    let serial: bool = sqlx::query_scalar("SELECT wms_serial_event_valid('serial-test','main',$1)")
        .bind(id)
        .fetch_one(&mut *tx)
        .await
        .expect("independent exact original handoff UID set");
    assert!(!serial);
    let error = sqlx::query("SET CONSTRAINTS ALL IMMEDIATE")
        .execute(&mut *tx)
        .await
        .expect_err("whole-order shipment cannot omit original unit custody");
    assert_eq!(
        error.as_database_error().and_then(|e| e.code()).as_deref(),
        Some("23514")
    );
    sqlx::raw_sql("ROLLBACK TO SAVEPOINT omitted_classic_identity; RELEASE SAVEPOINT omitted_classic_identity").execute(&mut *tx).await.expect("restore independently complete staged source");
    let id = classic_ship(&mut tx, false).await;
    settle(&mut tx).await;
    let place: Value=sqlx::query_scalar("SELECT place FROM wms_serial_units WHERE tenant='serial-test' AND sku='CONTROLLED' AND serial='ONE'")
        .fetch_one(&mut *tx).await.expect("actual original full-order handoff custody");
    assert_eq!(
        place,
        json!({"status":"shipped","warehouse_id":"main","order_id":"ORDER","line_id":"LINE-1","package_id":null,"handoff_event_id":id,"return_receipt_id":null})
    );
    tx.rollback().await.expect("discard private schema");
}

#[path = "transfers.rs"]
mod transfers;
