//! Direct SQL serial-source checks in isolated private schemas. Most roll back;
//! the concurrency case commits only its owned synthetic schema for two readers.
use sqlx::{PgConnection, Postgres, Transaction};
use stateset_warehouse::{domain, store::Store};

async fn schema() -> Transaction<'static, Postgres> {
    let url = std::env::var("WMS_TEST_DATABASE_URL").expect("isolated PostgreSQL test URL");
    assert!(reqwest::Url::parse(&url)
        .expect("test URL")
        .path()
        .contains("test"));
    let store = Store::connect(&url).await.expect("current migrated server");
    let mut tx = store.pool.begin().await.expect("test transaction");
    let schema = format!("serial_registry_{}", domain::id().replace('-', ""));
    sqlx::raw_sql(&format!(
        "CREATE SCHEMA {schema}; SET LOCAL search_path TO {schema},public"
    ))
    .execute(&mut *tx)
    .await
    .expect("private schema");
    let baseline: &[&str] = &[
        include_str!("../migrations/001_initial.sql"),
        include_str!("../migrations/002_relational_execution.sql"),
        include_str!("../migrations/003_native_operations.sql"),
        include_str!("../migrations/004_native_source_ownership.sql"),
        include_str!("../migrations/005_conditional_oms_fulfillment.sql"),
        include_str!("../migrations/006_oms_rejections.sql"),
        include_str!("../migrations/007_operations_indexes.sql"),
        include_str!("../migrations/008_delivery_recipient.sql"),
        include_str!("../migrations/009_resource_versions.sql"),
        include_str!("../migrations/010_customer_returns.sql"),
        include_str!("../migrations/011_immutable_command_receipts.sql"),
        include_str!("../migrations/012_receipt_cancellation.sql"),
        include_str!("../migrations/013_shipment_evidence.sql"),
        include_str!("../migrations/014_facility_transfers.sql"),
        include_str!("../migrations/015_split_fulfillment.sql"),
        include_str!("../migrations/016_order_remainder_cancellation.sql"),
        include_str!("../migrations/017_lot_controls.sql"),
        include_str!("../migrations/018_lot_trace.sql"),
        include_str!("../migrations/019_transfer_loss.sql"),
        include_str!("../migrations/020_pack_units.sql"),
    ];
    for sql in baseline {
        sqlx::raw_sql(sql)
            .execute(&mut *tx)
            .await
            .expect("pristine legacy migration");
    }
    sqlx::raw_sql(r#"INSERT INTO wms_warehouses VALUES('serial-test','main',0,'{"id":"main","code":"MAIN","name":"Synthetic","city":"Synthetic","timezone":"UTC"}');
      INSERT INTO wms_products VALUES('serial-test','main','SKU','{"sku":"SKU","name":"Legacy","category":"Synthetic","reorder_point":0}');
      INSERT INTO wms_warehouses VALUES('serial-test','other',0,'{"id":"other","code":"OTHER","name":"Synthetic","city":"Synthetic","timezone":"UTC"}');"#)
        .execute(&mut *tx).await.expect("actual untracked legacy catalog");
    // Represent a committed legacy catalog before testing ALTER TABLE, rather
    // than carrying earlier fixture trigger events into the upgrade DDL.
    sqlx::raw_sql("SET CONSTRAINTS ALL IMMEDIATE; SET CONSTRAINTS ALL DEFERRED")
        .execute(&mut *tx)
        .await
        .expect("settled legacy source facts");
    sqlx::raw_sql(include_str!("../migrations/021_serial_tracking.sql"))
        .execute(&mut *tx)
        .await
        .expect("isolated serial schema");
    sqlx::raw_sql(include_str!(
        "../migrations/023_serial_target_resolution.sql"
    ))
    .execute(&mut *tx)
    .await
    .expect("current serial target validation");
    sqlx::raw_sql(include_str!(
        "../migrations/024_serial_event_payload_reuse.sql"
    ))
    .execute(&mut *tx)
    .await
    .expect("current original payload source predicates");
    sqlx::raw_sql(include_str!(
        "../migrations/025_serial_resolved_read_sources.sql"
    ))
    .execute(&mut *tx)
    .await
    .expect("resolved read source component");
    sqlx::raw_sql(include_str!(
        "../migrations/026_serial_event_batch_index.sql"
    ))
    .execute(&mut *tx)
    .await
    .expect("bounded original event batch lookup");
    sqlx::raw_sql(include_str!(
        "../migrations/027_serial_reference_classes.sql"
    ))
    .execute(&mut *tx)
    .await
    .expect("statement-local exact serial reference classes");
    sqlx::raw_sql(include_str!("../migrations/028_serial_read_source.sql"))
        .execute(&mut *tx)
        .await
        .expect("read-only historical sources");
    sqlx::raw_sql(include_str!(
        "../migrations/029_serial_scope_source_sets.sql"
    ))
    .execute(&mut *tx)
    .await
    .expect("statement-local serial source sets");
    tx
}

async fn rejected(c: &mut PgConnection, sql: &str, expected: &str) {
    sqlx::query("SAVEPOINT serial_rejection")
        .execute(&mut *c)
        .await
        .expect("savepoint");
    let result = sqlx::raw_sql(sql).execute(&mut *c).await;
    let error = match result {
        Err(error) => error,
        Ok(_) => sqlx::query("SET CONSTRAINTS ALL IMMEDIATE")
            .execute(&mut *c)
            .await
            .expect_err("invalid original source/identity must fail at commit"),
    };
    assert_eq!(
        error.as_database_error().and_then(|e| e.code()).as_deref(),
        Some(expected),
        "unexpected rejection class: {error}"
    );
    sqlx::query("ROLLBACK TO SAVEPOINT serial_rejection")
        .execute(&mut *c)
        .await
        .expect("rollback expected rejection");
    sqlx::query("RELEASE SAVEPOINT serial_rejection")
        .execute(&mut *c)
        .await
        .expect("release savepoint");
}

async fn controlled_catalog(c: &mut PgConnection) {
    // Complete positive catalog source binding for the future schema. This is
    // an explicitly raw SQL fixture; it is not an accepted HTTP command.
    let event_id = domain::id();
    let result = serde_json::json!({"entity_id":"CONTROLLED","version":1,"type":"product.created"});
    let payload = serde_json::json!({"schema_version":2,"id":event_id,"tenant":"serial-test","warehouse_id":"main","version":1,
        "actor":"synthetic:admin","occurred_at":domain::now(),"request_id":"serial-raw-catalog-source","type":"product.created","result":result,
        "command":{"type":"create_product","sku":"CONTROLLED","name":"Controlled","category":"Synthetic","reorder_point":0,"serial_controlled":true},
        "inventory_changes":[],"order":null,"previous_order":null,"lot_changes":[],"lot_decisions":[]});
    sqlx::query(
        "UPDATE wms_warehouses SET version=1 WHERE tenant='serial-test' AND warehouse_id='main'",
    )
    .execute(&mut *c)
    .await
    .expect("source warehouse version");
    sqlx::query("INSERT INTO wms_serial_modes VALUES('serial-test','CONTROLLED',true,'main',$1)")
        .bind(&event_id)
        .execute(&mut *c)
        .await
        .expect("new mode before product admission");
    sqlx::query("INSERT INTO wms_serial_heads VALUES('serial-test','CONTROLLED',0,NULL)")
        .execute(&mut *c)
        .await
        .expect("initial serial review");
    sqlx::raw_sql(r#"INSERT INTO wms_products VALUES('serial-test','main','CONTROLLED','{"sku":"CONTROLLED","name":"Controlled","category":"Synthetic","reorder_point":0,"serial_controlled":true}');
      INSERT INTO wms_locations VALUES('serial-test','main','A','storage','{"code":"A","zone":"Synthetic","kind":"storage"}');"#).execute(&mut *c).await.expect("tracked source catalog and bin");
    sqlx::query("INSERT INTO wms_events(id,tenant,warehouse_id,version,payload) VALUES($1,'serial-test','main',1,$2)").bind(&event_id).bind(payload).execute(&mut *c).await.expect("original catalog event");
    sqlx::query("INSERT INTO wms_commands(tenant,warehouse_id,request_id,actor,digest,result) VALUES('serial-test','main','serial-raw-catalog-source','synthetic:admin',$1,$2)")
        .bind("0".repeat(64)).bind(result).execute(&mut *c).await.expect("raw source actor/result receipt, not a wire-digest assertion");
    sqlx::query("SET CONSTRAINTS ALL IMMEDIATE")
        .execute(&mut *c)
        .await
        .expect("complete positive catalog source binding");
    sqlx::query("SET CONSTRAINTS ALL DEFERRED")
        .execute(&mut *c)
        .await
        .expect("deferred physical fixtures");
}

async fn raw_unit_event(
    c: &mut PgConnection,
    revision: i64,
    from: Option<&str>,
    to: &str,
    before_override: Option<&str>,
) {
    let event_id = domain::id();
    let warehouse_version = revision + 1;
    let previous_unit: Option<i64> = sqlx::query_scalar("SELECT revision FROM wms_serial_units WHERE tenant='serial-test' AND sku='CONTROLLED' AND serial='ONE'")
        .fetch_optional(&mut *c).await.expect("original unit revision");
    let unit_revision = previous_unit.map_or(1, |value| value + 1);
    let request_id = format!("serial-raw-unit-{revision}");
    let at = domain::now();
    let place = serde_json::json!({"status":"on_hand","warehouse_id":"main","location":to});
    let before = from.map(|location| serde_json::json!({"status":"on_hand","warehouse_id":"main","location":before_override.unwrap_or(location)}));
    let kind = if from.is_some() {
        "inventory.moved"
    } else {
        "inventory.counted"
    };
    let command = if let Some(location) = from {
        serde_json::json!({"type":"move_stock","sku":"CONTROLLED","lot":"LOT","from":location,"to":to,"quantity":1})
    } else {
        serde_json::json!({"type":"count_stock","sku":"CONTROLLED","lot":"LOT","location":to,"counted":1,"reason":"Observed initial serial"})
    };
    let result = serde_json::json!({"entity_id":format!("CONTROLLED / {to}"),"version":warehouse_version,"type":kind});
    sqlx::query(
        "UPDATE wms_warehouses SET version=$1 WHERE tenant='serial-test' AND warehouse_id='main'",
    )
    .bind(warehouse_version)
    .execute(&mut *c)
    .await
    .expect("canonical warehouse event version");
    sqlx::query("INSERT INTO wms_locations VALUES('serial-test','main',$1,'storage',jsonb_build_object('code',$1,'zone','Synthetic','kind','storage')) ON CONFLICT DO NOTHING").bind(to).execute(&mut *c).await.expect("destination bin");
    if let Some(location) = from {
        sqlx::query("UPDATE wms_stock SET on_hand=on_hand-1 WHERE tenant='serial-test' AND warehouse_id='main' AND sku='CONTROLLED' AND lot='LOT' AND location=$1").bind(location).execute(&mut *c).await.expect("source physical debit");
    }
    sqlx::query("INSERT INTO wms_stock VALUES('serial-test','main','CONTROLLED','LOT',$1,1,0) ON CONFLICT(tenant,warehouse_id,sku,lot,location) DO UPDATE SET on_hand=wms_stock.on_hand+1").bind(to).execute(&mut *c).await.expect("destination physical credit");
    let movement_id = domain::id();
    let movement = serde_json::json!({"id":movement_id,"kind":if from.is_some(){"putaway"}else{"count"},"sku":"CONTROLLED","lot":"LOT","from":from,"to":to,"quantity":1,"reference":"Synthetic serial schema fixture","receipt_id":null,"receipt_line_id":null,"reason":"Raw attributed test source","actor":"synthetic:admin","at":at});
    sqlx::query("INSERT INTO wms_movements(id,tenant,warehouse_id,sku,lot,from_location,to_location,quantity,kind,reference,data) VALUES($1,'serial-test','main','CONTROLLED','LOT',$2,$3,1,$4,'Synthetic serial schema fixture',$5)")
        .bind(&movement_id).bind(from).bind(to).bind(if from.is_some(){"putaway"}else{"count"}).bind(movement).execute(&mut *c).await.expect("independent append-only stock ledger source");
    sqlx::query("UPDATE wms_serial_heads SET version=$1,last_event_id=$2 WHERE tenant='serial-test' AND sku='CONTROLLED'").bind(revision).bind(&event_id).execute(&mut *c).await.expect("serial SKU review advance");
    sqlx::query(
        "INSERT INTO wms_serial_batches VALUES('serial-test','CONTROLLED',$1,'main',$2,$3)",
    )
    .bind(revision)
    .bind(warehouse_version)
    .bind(&event_id)
    .execute(&mut *c)
    .await
    .expect("original SKU event batch");
    if previous_unit.is_none() {
        sqlx::query(
            "INSERT INTO wms_serial_units VALUES('serial-test','CONTROLLED','ONE','LOT',1,$1,$2)",
        )
        .bind(&place)
        .bind(&event_id)
        .execute(&mut *c)
        .await
        .expect("first registry identity");
    } else {
        sqlx::query("UPDATE wms_serial_units SET revision=$1,place=$2,last_event_id=$3 WHERE tenant='serial-test' AND sku='CONTROLLED' AND serial='ONE'").bind(unit_revision).bind(&place).bind(&event_id).execute(&mut *c).await.expect("current registry projection");
    }
    sqlx::query("INSERT INTO wms_serial_steps VALUES('serial-test','CONTROLLED','ONE','LOT',$1,$2,$3,'main',$4,$5,1,'[]',$6,$7,'synthetic:admin',$8)")
        .bind(unit_revision).bind(previous_unit).bind(revision).bind(warehouse_version).bind(&event_id).bind(before).bind(place).bind(&at).execute(&mut *c).await.expect("immutable original unit transition");
    sqlx::query("INSERT INTO wms_serial_observations VALUES('serial-test','main',$1,1,'CONTROLLED',$2,'[]','[\"ONE\"]')").bind(&event_id).bind(revision).execute(&mut *c).await.expect("original serial observation");
    let step: serde_json::Value = sqlx::query_scalar("SELECT to_jsonb(s)-'tenant' FROM wms_serial_steps s WHERE tenant='serial-test' AND event_id=$1").bind(&event_id).fetch_one(&mut *c).await.expect("bound normalized source bytes");
    let changes: serde_json::Value = sqlx::query_scalar("SELECT jsonb_agg(jsonb_build_object('stock',jsonb_build_object('sku',sku,'lot',lot,'location',location,'on_hand',on_hand,'reserved',reserved),'version',$1)) FROM wms_stock WHERE tenant='serial-test' AND warehouse_id='main' AND sku='CONTROLLED'").bind(warehouse_version).fetch_one(&mut *c).await.expect("canonical physical snapshot");
    let payload = serde_json::json!({"schema_version":2,"id":event_id,"tenant":"serial-test","warehouse_id":"main","version":warehouse_version,
        "actor":"synthetic:admin","occurred_at":at,"request_id":request_id,"type":kind,"result":result,"command":command,"inventory_changes":changes,
        "order":null,"previous_order":null,"lot_changes":[],"lot_decisions":[],"serial_heads":{"CONTROLLED":revision},"serial_steps":[step],"serial_inputs":[{"target":[],"sku":"CONTROLLED","serials":["ONE"]}]});
    sqlx::query("INSERT INTO wms_events(id,tenant,warehouse_id,version,payload) VALUES($1,'serial-test','main',$2,$3)").bind(&event_id).bind(warehouse_version).bind(payload).execute(&mut *c).await.expect("self-contained raw transition event");
    sqlx::query("INSERT INTO wms_commands(tenant,warehouse_id,request_id,actor,digest,result) VALUES('serial-test','main',$1,'synthetic:admin',$2,$3)").bind(&request_id).bind("0".repeat(64)).bind(result).execute(&mut *c).await.expect("raw actor/result attribution, not HTTP byte-proof");
}

async fn raw_count_observation(c: &mut PgConnection, version: i64, serials: &[&str]) {
    raw_count_fixture(c, version, serials, &[], true, true).await;
}

// Explicit raw sources let forensic cases omit a projection/head write while
// retaining complete, immutable event/observation/effect bytes. No HTTP claim.
async fn raw_count_fixture(
    c: &mut PgConnection,
    version: i64,
    serials: &[&str],
    effects: &[(&str, serde_json::Value)],
    project: bool,
    advance_head: bool,
) {
    let event_id = domain::id();
    let at = domain::now();
    let warehouse_version = version + 1;
    let request_id = format!("serial-raw-observation-{version}");
    let result = serde_json::json!({"entity_id":"CONTROLLED / A","version":warehouse_version,"type":"inventory.counted"});
    sqlx::query(
        "UPDATE wms_warehouses SET version=$1 WHERE tenant='serial-test' AND warehouse_id='main'",
    )
    .bind(warehouse_version)
    .execute(&mut *c)
    .await
    .expect("observed warehouse version");
    if advance_head {
        sqlx::query("UPDATE wms_serial_heads SET version=$1,last_event_id=$2 WHERE tenant='serial-test' AND sku='CONTROLLED'").bind(version).bind(&event_id).execute(&mut *c).await.expect("observation review advance");
    }
    sqlx::query(
        "INSERT INTO wms_serial_batches VALUES('serial-test','CONTROLLED',$1,'main',$2,$3)",
    )
    .bind(version)
    .bind(warehouse_version)
    .bind(&event_id)
    .execute(&mut *c)
    .await
    .expect("original observation batch");
    sqlx::query("INSERT INTO wms_serial_observations VALUES('serial-test','main',$1,1,'CONTROLLED',$2,'[]',$3)").bind(&event_id).bind(version).bind(serde_json::json!(serials)).execute(&mut *c).await.expect("exact original observed set");
    for (index, (serial, after)) in effects.iter().enumerate() {
        let previous: Option<(i64, serde_json::Value)> = sqlx::query_as("SELECT revision,place FROM wms_serial_units WHERE tenant='serial-test' AND sku='CONTROLLED' AND serial=$1").bind(serial).fetch_optional(&mut *c).await.expect("original current identity");
        let previous_revision = previous.as_ref().map(|p| p.0);
        let before = previous.as_ref().map(|p| &p.1);
        let revision = previous_revision.map_or(1, |v| v + 1);
        if project {
            sqlx::query("INSERT INTO wms_serial_units VALUES('serial-test','CONTROLLED',$1,'LOT',$2,$3,$4) ON CONFLICT(tenant,sku,serial) DO UPDATE SET revision=EXCLUDED.revision,place=EXCLUDED.place,last_event_id=EXCLUDED.last_event_id").bind(serial).bind(revision).bind(after).bind(&event_id).execute(&mut *c).await.expect("actual identity projection");
        }
        sqlx::query("INSERT INTO wms_serial_steps VALUES('serial-test','CONTROLLED',$1,'LOT',$2,$3,$4,'main',$5,$6,$7,'[]',$8,$9,'synthetic:admin',$10)").bind(serial).bind(revision).bind(previous_revision).bind(version).bind(warehouse_version).bind(&event_id).bind((index+1) as i32).bind(before).bind(after).bind(&at).execute(&mut *c).await.expect("original immutable identity effect");
    }
    let steps: serde_json::Value=sqlx::query_scalar("SELECT COALESCE(jsonb_agg(to_jsonb(s)-'tenant' ORDER BY ordinal),'[]'::jsonb) FROM wms_serial_steps s WHERE tenant='serial-test' AND event_id=$1").bind(&event_id).fetch_one(&mut *c).await.expect("exact original effect bytes");
    let payload = serde_json::json!({"schema_version":2,"id":event_id,"tenant":"serial-test","warehouse_id":"main","version":warehouse_version,"actor":"synthetic:admin","occurred_at":at,"request_id":request_id,"type":"inventory.counted","result":result,
      "command":{"type":"count_stock","sku":"CONTROLLED","lot":"LOT","location":"A","counted":serials.len(),"reason":"Exact observed set"},
      "inventory_changes":[],"order":null,"previous_order":null,"lot_changes":[],"lot_decisions":[],"serial_heads":{"CONTROLLED":version},"serial_steps":steps,"serial_inputs":[{"target":[],"sku":"CONTROLLED","serials":serials}]});
    sqlx::query("INSERT INTO wms_events(id,tenant,warehouse_id,version,payload) VALUES($1,'serial-test','main',$2,$3)").bind(&event_id).bind(warehouse_version).bind(payload).execute(&mut *c).await.expect("complete original observation event");
    sqlx::query("INSERT INTO wms_commands(tenant,warehouse_id,request_id,actor,digest,result) VALUES('serial-test','main',$1,'synthetic:admin',$2,$3)").bind(request_id).bind("0".repeat(64)).bind(result).execute(&mut *c).await.expect("attributed raw source, not an HTTP digest assertion");
}

struct RawTransition {
    command: serde_json::Value,
    kind: &'static str,
    entity: &'static str,
    recorded_at: String,
    after: serde_json::Value,
}

// A raw SQL ownership fixture, independent of the Rust transition planner.
// Physical stock/allocation/movement writes are intentionally separate below.
async fn raw_selected_transition(c: &mut PgConnection, version: i64, transition: RawTransition) {
    let event_id = domain::id();
    let at = transition.recorded_at;
    let warehouse_version: i64 = sqlx::query_scalar(
        "SELECT version FROM wms_warehouses WHERE tenant='serial-test' AND warehouse_id='main'",
    )
    .fetch_one(&mut *c)
    .await
    .expect("independent warehouse command version");
    let request_id = format!("serial-raw-owned-{version}");
    let result = serde_json::json!({"entity_id":transition.entity,"version":warehouse_version,"type":transition.kind});
    let previous: Option<(i64,serde_json::Value)>=sqlx::query_as("SELECT revision,place FROM wms_serial_units WHERE tenant='serial-test' AND sku='CONTROLLED' AND serial='ONE'").fetch_optional(&mut *c).await.expect("actual prior unit owner");
    let previous_revision = previous.as_ref().map(|p| p.0);
    let revision = previous_revision.map_or(1, |v| v + 1);
    let before = previous.map(|p| p.1);
    sqlx::query(
        "UPDATE wms_warehouses SET version=$1 WHERE tenant='serial-test' AND warehouse_id='main'",
    )
    .bind(warehouse_version)
    .execute(&mut *c)
    .await
    .expect("original warehouse version");
    sqlx::query("UPDATE wms_serial_heads SET version=$1,last_event_id=$2 WHERE tenant='serial-test' AND sku='CONTROLLED'").bind(version).bind(&event_id).execute(&mut *c).await.expect("original SKU review");
    sqlx::query(
        "INSERT INTO wms_serial_batches VALUES('serial-test','CONTROLLED',$1,'main',$2,$3)",
    )
    .bind(version)
    .bind(warehouse_version)
    .bind(&event_id)
    .execute(&mut *c)
    .await
    .expect("original SKU batch");
    sqlx::query("INSERT INTO wms_serial_units VALUES('serial-test','CONTROLLED','ONE','LOT',$1,$2,$3) ON CONFLICT(tenant,sku,serial) DO UPDATE SET revision=EXCLUDED.revision,place=EXCLUDED.place,last_event_id=EXCLUDED.last_event_id").bind(revision).bind(&transition.after).bind(&event_id).execute(&mut *c).await.expect("current actual unit");
    sqlx::query("INSERT INTO wms_serial_steps VALUES('serial-test','CONTROLLED','ONE','LOT',$1,$2,$3,'main',$4,$5,1,'[]',$6,$7,'synthetic:admin',$8)").bind(revision).bind(previous_revision).bind(version).bind(warehouse_version).bind(&event_id).bind(before).bind(&transition.after).bind(&at).execute(&mut *c).await.expect("immutable owner transition");
    sqlx::query("INSERT INTO wms_serial_observations VALUES('serial-test','main',$1,1,'CONTROLLED',$2,'[]','[\"ONE\"]')").bind(&event_id).bind(version).execute(&mut *c).await.expect("original exact selected identity");
    let step: serde_json::Value=sqlx::query_scalar("SELECT to_jsonb(s)-'tenant' FROM wms_serial_steps s WHERE tenant='serial-test' AND event_id=$1").bind(&event_id).fetch_one(&mut *c).await.expect("original normalized transition bytes");
    let inventory: serde_json::Value=sqlx::query_scalar("SELECT jsonb_agg(jsonb_build_object('stock',jsonb_build_object('sku',sku,'lot',lot,'location',location,'on_hand',on_hand,'reserved',reserved),'version',$1) ORDER BY location) FROM wms_stock WHERE tenant='serial-test' AND warehouse_id='main' AND sku='CONTROLLED'").bind(warehouse_version).fetch_one(&mut *c).await.expect("actual canonical quantities");
    let order: serde_json::Value = sqlx::query_scalar(
        "SELECT COALESCE(wms_current_order('serial-test','main','ORDER'),'null'::jsonb)",
    )
    .fetch_one(&mut *c)
    .await
    .expect("current normalized allocation source");
    let mut previous = order.clone();
    if matches!(transition.command["type"].as_str(), Some("pick" | "unpick")) {
        previous["allocations"][0]["picked"] =
            serde_json::json!(if transition.command["type"] == "pick" {
                0
            } else {
                1
            });
    }
    let decisions: serde_json::Value=sqlx::query_scalar("SELECT COALESCE(jsonb_agg(wms_lot_decision_document(kind,sku,lot,checked_at,required,document) ORDER BY kind,sku,lot),'[]'::jsonb) FROM wms_lot_decisions WHERE tenant='serial-test' AND warehouse_id='main' AND version=$1").bind(warehouse_version).fetch_one(&mut *c).await.expect("original independent lot decisions");
    let payload = serde_json::json!({"schema_version":2,"id":event_id,"tenant":"serial-test","warehouse_id":"main","version":warehouse_version,"actor":"synthetic:admin","occurred_at":at,"request_id":request_id,"type":transition.kind,"result":result,"command":transition.command,"inventory_changes":inventory,"order":order,"previous_order":previous,"lot_changes":[],"lot_decisions":decisions,"serial_heads":{"CONTROLLED":version},"serial_steps":[step],"serial_inputs":[{"target":[],"sku":"CONTROLLED","serials":["ONE"]}]});
    sqlx::query("INSERT INTO wms_events(id,tenant,warehouse_id,version,payload) VALUES($1,'serial-test','main',$2,$3)").bind(&event_id).bind(warehouse_version).bind(payload).execute(&mut *c).await.expect("complete original owner event");
    sqlx::query("INSERT INTO wms_commands(tenant,warehouse_id,request_id,actor,digest,result) VALUES('serial-test','main',$1,'synthetic:admin',$2,$3)").bind(request_id).bind("0".repeat(64)).bind(result).execute(&mut *c).await.expect("raw owner attribution, not HTTP wire proof");
}

async fn raw_supplier_receive(c: &mut PgConnection) {
    let at = domain::now();
    sqlx::raw_sql(r#"UPDATE wms_warehouses SET version=2 WHERE tenant='serial-test' AND warehouse_id='main';
      INSERT INTO wms_locations VALUES('serial-test','main','RECEIVING','receiving','{"code":"RECEIVING","kind":"receiving","zone":"Synthetic"}');
      INSERT INTO wms_receipts(tenant,warehouse_id,id,reference,status,data) VALUES('serial-test','main','INBOUND','Synthetic supplier receipt','partial','{"id":"INBOUND","reference":"Synthetic supplier receipt","status":"partial"}');
      INSERT INTO wms_receipt_lines VALUES('serial-test','main','INBOUND','SUPPLIER-LINE','CONTROLLED','LOT',2,1);
      INSERT INTO wms_stock VALUES('serial-test','main','CONTROLLED','LOT','RECEIVING',1,0);"#).execute(&mut *c).await.expect("actual inbound supplier execution rows");
    let id = domain::id();
    let data = serde_json::json!({"id":id,"kind":"receipt","sku":"CONTROLLED","lot":"LOT","from":null,"to":"RECEIVING","quantity":1,"reference":"Synthetic supplier receipt","receipt_id":"INBOUND","receipt_line_id":"SUPPLIER-LINE","reason":"Raw actual supplier receipt","actor":"synthetic:admin","at":at});
    sqlx::query("INSERT INTO wms_movements(id,tenant,warehouse_id,sku,lot,from_location,to_location,quantity,kind,reference,receipt_id,receipt_line_id,data) VALUES($1,'serial-test','main','CONTROLLED','LOT',NULL,'RECEIVING',1,'receipt','Synthetic supplier receipt','INBOUND','SUPPLIER-LINE',$2)").bind(id).bind(data).execute(&mut *c).await.expect("independent physical ledger and original receipt evidence");
    raw_selected_transition(c,1,RawTransition { command:serde_json::json!({"type":"receive","receipt_id":"INBOUND","line_id":"SUPPLIER-LINE","location":"RECEIVING","quantity":1}),kind:"receipt.received",entity:"INBOUND",recorded_at:at,after:serde_json::json!({"status":"on_hand","warehouse_id":"main","location":"RECEIVING"}) }).await;
}

async fn raw_pick_stock(c: &mut PgConnection, picking: bool) -> String {
    let at = domain::now();
    sqlx::query("UPDATE wms_warehouses SET version=version+1 WHERE tenant='serial-test' AND warehouse_id='main'").execute(&mut *c).await.expect("begin independent physical command version");
    let picked = if picking { 1i64 } else { 0 };
    sqlx::query("UPDATE wms_allocations SET picked=$1 WHERE tenant='serial-test' AND warehouse_id='main' AND order_id='ORDER' AND line_id='LINE-1'").bind(picked).execute(&mut *c).await.expect("independent exact allocation owner");
    sqlx::query("UPDATE wms_stock SET on_hand=1-$1,reserved=1-$1 WHERE tenant='serial-test' AND warehouse_id='main' AND sku='CONTROLLED' AND location='A'").bind(picked).execute(&mut *c).await.expect("original bin balance/reservation");
    sqlx::query("UPDATE wms_stock SET on_hand=$1,reserved=$1 WHERE tenant='serial-test' AND warehouse_id='main' AND sku='CONTROLLED' AND location='STAGE-01'").bind(picked).execute(&mut *c).await.expect("staging balance/reservation");
    let (from, to, kind) = if picking {
        ("A", "STAGE-01", "pick")
    } else {
        ("STAGE-01", "A", "unpick")
    };
    let id = domain::id();
    let data = serde_json::json!({"id":id,"kind":kind,"sku":"CONTROLLED","lot":"LOT","from":from,"to":to,"quantity":1,"reference":"Synthetic ownership fixture","receipt_id":null,"receipt_line_id":null,"reason":"Raw actual owner proof","actor":"synthetic:admin","at":domain::now()});
    sqlx::query("INSERT INTO wms_movements(id,tenant,warehouse_id,sku,lot,from_location,to_location,quantity,kind,reference,data) VALUES($1,'serial-test','main','CONTROLLED','LOT',$2,$3,1,$4,'Synthetic ownership fixture',$5)").bind(id).bind(from).bind(to).bind(kind).bind(data).execute(&mut *c).await.expect("independent append-only physical movement");
    at
}

async fn raw_staged_order() -> Transaction<'static, Postgres> {
    raw_staged_order_for(true).await
}

async fn raw_staged_order_for(two_lines: bool) -> Transaction<'static, Postgres> {
    let mut tx = schema().await;
    controlled_catalog(&mut tx).await;
    raw_supplier_receive(&mut tx).await;
    sqlx::raw_sql("SET CONSTRAINTS ALL IMMEDIATE; SET CONSTRAINTS ALL DEFERRED")
        .execute(&mut *tx)
        .await
        .expect("complete actual receiving-location custody and original receipt evidence");
    raw_unit_event(&mut tx, 2, Some("RECEIVING"), "A", None).await;
    sqlx::raw_sql("SET CONSTRAINTS ALL IMMEDIATE; SET CONSTRAINTS ALL DEFERRED")
        .execute(&mut *tx)
        .await
        .expect("settle actual putaway before a separate allocation event");
    let allocation_at = domain::now();
    sqlx::raw_sql(r#"UPDATE wms_warehouses SET version=version+1 WHERE tenant='serial-test' AND warehouse_id='main';
      INSERT INTO wms_locations VALUES('serial-test','main','STAGE-01','staging','{"code":"STAGE-01","kind":"staging","zone":"Synthetic"}');
      INSERT INTO wms_orders(tenant,warehouse_id,id,reference,status,data) VALUES('serial-test','main','ORDER','Synthetic order','picking','{"id":"ORDER","reference":"Synthetic order","source_order_id":null,"customer":"Synthetic customer","destination":"Synthetic destination","priority":"normal","status":"picking","carrier":"","handoff_reference":"","created_at":"2026-10-18T00:00:00Z","shipped_at":null}');
      INSERT INTO wms_order_lines VALUES('serial-test','main','ORDER','LINE-1','CONTROLLED',1);
      INSERT INTO wms_allocations VALUES('serial-test','main','ORDER','LINE-1','CONTROLLED','LOT','A',1,0,0,0);
      UPDATE wms_stock SET reserved=1 WHERE tenant='serial-test' AND sku='CONTROLLED' AND location='A';
      INSERT INTO wms_stock VALUES('serial-test','main','CONTROLLED','LOT','STAGE-01',0,0);
      "#).execute(&mut *tx).await.expect("independent initial allocation and physical ledger");
    if two_lines {
        sqlx::query("INSERT INTO wms_order_lines VALUES('serial-test','main','ORDER','LINE-2','CONTROLLED',1)").execute(&mut *tx).await.expect("independent second same-SKU line");
    }
    let allocation_id = domain::id();
    let allocation_result = serde_json::json!({"entity_id":"ORDER","version":4,"type":"fulfillment.available_allocated"});
    let order: serde_json::Value =
        sqlx::query_scalar("SELECT wms_current_order('serial-test','main','ORDER')")
            .fetch_one(&mut *tx)
            .await
            .expect("original allocation quantities");
    let mut previous = order.clone();
    previous["allocations"] = serde_json::json!([]);
    let decisions: serde_json::Value=sqlx::query_scalar("SELECT jsonb_agg(wms_lot_decision_document(kind,sku,lot,checked_at,required,document) ORDER BY kind,sku,lot) FROM wms_lot_decisions WHERE tenant='serial-test' AND warehouse_id='main' AND version=4").fetch_one(&mut *tx).await.expect("original allocation lot check");
    let payload = serde_json::json!({"schema_version":2,"id":allocation_id,"tenant":"serial-test","warehouse_id":"main","version":4,"actor":"synthetic:admin","occurred_at":allocation_at,"request_id":"serial-raw-allocation","type":"fulfillment.available_allocated","result":allocation_result,"command":{"type":"allocate_available","order_id":"ORDER"},"inventory_changes":[],"order":order,"previous_order":previous,"lot_changes":[],"lot_decisions":decisions});
    sqlx::query("INSERT INTO wms_events(id,tenant,warehouse_id,version,payload) VALUES($1,'serial-test','main',4,$2)").bind(allocation_id).bind(payload).execute(&mut *tx).await.expect("separate original allocation event");
    sqlx::query("INSERT INTO wms_commands(tenant,warehouse_id,request_id,actor,digest,result) VALUES('serial-test','main','serial-raw-allocation','synthetic:admin',$1,$2)").bind("0".repeat(64)).bind(allocation_result).execute(&mut *tx).await.expect("raw allocation receipt attribution");
    sqlx::raw_sql("SET CONSTRAINTS ALL IMMEDIATE; SET CONSTRAINTS ALL DEFERRED")
        .execute(&mut *tx)
        .await
        .expect("complete original allocation source and stock equations");
    let at = raw_pick_stock(&mut tx, true).await;
    sqlx::query("UPDATE wms_orders SET status=wms_split_status('serial-test','main','ORDER'),data=jsonb_set(data,'{status}',to_jsonb(wms_split_status('serial-test','main','ORDER'))) WHERE tenant='serial-test' AND warehouse_id='main' AND id='ORDER'").execute(&mut *tx).await.expect("actual full or partial pick status");
    raw_selected_transition(&mut tx,3,RawTransition {command:serde_json::json!({"type":"pick","order_id":"ORDER","line_id":"LINE-1","location":"A","lot":"LOT","quantity":1}),kind:"fulfillment.picked",entity:"ORDER",recorded_at:at,after:serde_json::json!({"status":"staged","warehouse_id":"main","order_id":"ORDER","line_id":"LINE-1","allocation_location":"A","package_id":null})}).await;
    sqlx::raw_sql("SET CONSTRAINTS ALL IMMEDIATE; SET CONSTRAINTS ALL DEFERRED")
        .execute(&mut *tx)
        .await
        .expect("actual staged UID agrees with exact picked allocation");
    tx
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL; exercises a private serial schema"]
async fn serial_supplier_receive_pick_unpick_preserves_exact_same_sku_order_line_owner() {
    let mut tx = raw_staged_order().await;
    sqlx::query("SAVEPOINT wrong_same_sku_owner")
        .execute(&mut *tx)
        .await
        .expect("forensic wrong line");
    let at = raw_pick_stock(&mut tx, false).await;
    raw_selected_transition(&mut tx,4,RawTransition {command:serde_json::json!({"type":"unpick","order_id":"ORDER","line_id":"LINE-2","location":"A","lot":"LOT","quantity":1}),kind:"fulfillment.unpicked",entity:"ORDER",recorded_at:at,after:serde_json::json!({"status":"on_hand","warehouse_id":"main","location":"A"})}).await;
    let balanced: bool =
        sqlx::query_scalar("SELECT wms_serial_scope_current_valid('serial-test','main')")
            .fetch_one(&mut *tx)
            .await
            .expect("forged command leaves actual stock and line quantities balanced");
    assert!(balanced);
    let error = sqlx::query("SET CONSTRAINTS ALL IMMEDIATE")
        .execute(&mut *tx)
        .await
        .expect_err("another same-SKU line cannot claim this unit's unpick");
    assert_eq!(
        error.as_database_error().and_then(|e| e.code()).as_deref(),
        Some("23514")
    );
    sqlx::raw_sql(
        "ROLLBACK TO SAVEPOINT wrong_same_sku_owner; RELEASE SAVEPOINT wrong_same_sku_owner",
    )
    .execute(&mut *tx)
    .await
    .expect("restore actual staged ownership");
    let at = raw_pick_stock(&mut tx, false).await;
    raw_selected_transition(&mut tx,4,RawTransition {command:serde_json::json!({"type":"unpick","order_id":"ORDER","line_id":"LINE-1","location":"A","lot":"LOT","quantity":1}),kind:"fulfillment.unpicked",entity:"ORDER",recorded_at:at,after:serde_json::json!({"status":"on_hand","warehouse_id":"main","location":"A"})}).await;
    sqlx::query("SET CONSTRAINTS ALL IMMEDIATE")
        .execute(&mut *tx)
        .await
        .expect("exact original line's unpick succeeds");
    let all: bool = sqlx::query_scalar(
        "SELECT bool_and(wms_serial_event_valid(tenant,warehouse_id,id)) FROM wms_events",
    )
    .fetch_one(&mut *tx)
    .await
    .expect("original supplier receipt, pick and unpick retain independently valid sources");
    assert!(all);
    tx.rollback().await.expect("discard private schema");
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL; exercises a private serial schema"]
async fn serial_history_cannot_leave_current_unit_or_sku_review_behind() {
    let mut tx = schema().await;
    controlled_catalog(&mut tx).await;
    raw_unit_event(&mut tx, 1, None, "A", None).await;
    sqlx::raw_sql("SET CONSTRAINTS ALL IMMEDIATE; SET CONSTRAINTS ALL DEFERRED")
        .execute(&mut *tx)
        .await
        .expect("valid original custody");
    let missing = serde_json::json!({"status":"missing","warehouse_id":"main","location":"A"});
    for (serials, effects, project, head) in [
        (vec!["ONE"], vec![], true, false),
        (vec![], vec![("ONE", missing.clone())], false, true),
    ] {
        sqlx::query("SAVEPOINT omitted_projection")
            .execute(&mut *tx)
            .await
            .expect("forensic fixture");
        raw_count_fixture(&mut tx, 2, &serials, &effects, project, head).await;
        let event_valid: bool = sqlx::query_scalar(
            "SELECT bool_and(wms_serial_event_valid(tenant,warehouse_id,id)) FROM wms_events",
        )
        .fetch_one(&mut *tx)
        .await
        .expect("complete original effects at their historical point");
        assert!(event_valid);
        let balanced: bool =
            sqlx::query_scalar("SELECT wms_serial_scope_current_valid('serial-test','main')")
                .fetch_one(&mut *tx)
                .await
                .expect("current quantities alone remain balanced");
        assert!(balanced);
        let error = sqlx::query("SET CONSTRAINTS ALL IMMEDIATE")
            .execute(&mut *tx)
            .await
            .expect_err("new immutable history must advance its current projection and review");
        assert_eq!(
            error.as_database_error().and_then(|e| e.code()).as_deref(),
            Some("23514")
        );
        sqlx::raw_sql(
            "ROLLBACK TO SAVEPOINT omitted_projection; RELEASE SAVEPOINT omitted_projection",
        )
        .execute(&mut *tx)
        .await
        .expect("restore valid original custody");
    }
    tx.rollback().await.expect("discard private schema");
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL; exercises a private serial schema"]
async fn serial_equal_quantity_replacement_and_found_counts_fence_original_stock_reviews() {
    let mut tx = schema().await;
    controlled_catalog(&mut tx).await;
    raw_unit_event(&mut tx, 1, None, "A", None).await;
    sqlx::raw_sql("SET CONSTRAINTS ALL IMMEDIATE; SET CONSTRAINTS ALL DEFERRED")
        .execute(&mut *tx)
        .await
        .expect("valid original custody");
    let revisions="SELECT resource,revision FROM wms_resource_versions WHERE tenant='serial-test' AND warehouse_id='main' AND resource IN ('[\"stock\",\"CONTROLLED\",\"LOT\",\"A\"]','[\"inventory\",\"CONTROLLED\"]') ORDER BY resource";
    let mut previous: Vec<(String, i64)> = sqlx::query_as(revisions)
        .fetch_all(&mut *tx)
        .await
        .expect("original reviewed counters");
    assert_eq!(previous.len(), 2);
    let on_hand = serde_json::json!({"status":"on_hand","warehouse_id":"main","location":"A"});
    let missing = serde_json::json!({"status":"missing","warehouse_id":"main","location":"A"});
    for (version, observed, effects) in [
        (
            2,
            vec!["TWO"],
            vec![("ONE", missing.clone()), ("TWO", on_hand.clone())],
        ),
        (3, vec!["ONE"], vec![("TWO", missing), ("ONE", on_hand)]),
        (4, vec!["ONE"], vec![]),
    ] {
        raw_count_fixture(&mut tx, version, &observed, &effects, true, true).await;
        sqlx::query("SET CONSTRAINTS ALL IMMEDIATE")
            .execute(&mut *tx)
            .await
            .expect("complete replacement, found or unchanged count");
        let current: Vec<(String, i64)> = sqlx::query_as(revisions)
            .fetch_all(&mut *tx)
            .await
            .expect("database-owned stock and SKU reviews");
        for ((resource, before), (same, after)) in previous.iter().zip(&current) {
            assert_eq!(resource, same);
            assert_eq!(
                *after,
                *before + 1,
                "unchanged quantity count invalidates reviewed work"
            );
        }
        previous = current;
        let stock: i64=sqlx::query_scalar("SELECT on_hand FROM wms_stock WHERE tenant='serial-test' AND sku='CONTROLLED' AND location='A'").fetch_one(&mut *tx).await.expect("canonical each balance");
        assert_eq!(stock, 1);
        let movements: i64 =
            sqlx::query_scalar("SELECT count(*) FROM wms_movements WHERE tenant='serial-test'")
                .fetch_one(&mut *tx)
                .await
                .expect("no invented quantity movements");
        assert_eq!(movements, 1);
        let identity: String=sqlx::query_scalar("SELECT serial FROM wms_serial_units WHERE tenant='serial-test' AND place->>'status'='on_hand'").fetch_one(&mut *tx).await.expect("exact observed current identity");
        assert_eq!(identity, observed[0]);
        sqlx::query("SET CONSTRAINTS ALL DEFERRED")
            .execute(&mut *tx)
            .await
            .expect("next physical observation");
    }
    let all: bool = sqlx::query_scalar(
        "SELECT bool_and(wms_serial_event_valid(tenant,warehouse_id,id)) FROM wms_events",
    )
    .fetch_one(&mut *tx)
    .await
    .expect("all historical exact observed sets remain valid");
    assert!(all);
    tx.rollback().await.expect("discard private schema");
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL; exercises a private serial schema"]
async fn serial_event_cannot_omit_missing_units_or_relabel_an_unchanged_count_with_balanced_stock()
{
    let mut tx = schema().await;
    controlled_catalog(&mut tx).await;
    raw_unit_event(&mut tx, 1, None, "A", None).await;
    sqlx::query("SET CONSTRAINTS ALL IMMEDIATE")
        .execute(&mut *tx)
        .await
        .expect("positive original counted identity");
    sqlx::query("SET CONSTRAINTS ALL DEFERRED")
        .execute(&mut *tx)
        .await
        .expect("forensic observation cases");
    for serials in [vec![], vec!["UNREGISTERED"]] {
        sqlx::query("SAVEPOINT omitted_serial_effect")
            .execute(&mut *tx)
            .await
            .expect("forensic observation savepoint");
        raw_count_observation(&mut tx, 2, &serials).await;
        let balanced: bool =
            sqlx::query_scalar("SELECT wms_serial_scope_current_valid('serial-test','main')")
                .fetch_one(&mut *tx)
                .await
                .expect("all actual stock/identity quantities remain balanced");
        assert!(balanced);
        let error=sqlx::query("SET CONSTRAINTS ALL IMMEDIATE").execute(&mut *tx).await.expect_err("a matching original event and observation cannot omit the required identity effect");
        assert_eq!(
            error.as_database_error().and_then(|e| e.code()).as_deref(),
            Some("23514")
        );
        assert!(error.to_string().contains("Original serial event"));
        sqlx::query("ROLLBACK TO SAVEPOINT omitted_serial_effect")
            .execute(&mut *tx)
            .await
            .expect("restore original custody");
        sqlx::query("RELEASE SAVEPOINT omitted_serial_effect")
            .execute(&mut *tx)
            .await
            .expect("release forensic savepoint");
    }
    raw_count_observation(&mut tx, 2, &["ONE"]).await;
    sqlx::query("SET CONSTRAINTS ALL IMMEDIATE")
        .execute(&mut *tx)
        .await
        .expect("a genuine unchanged observed set needs no invented movement");
    sqlx::query("SET CONSTRAINTS ALL DEFERRED")
        .execute(&mut *tx)
        .await
        .expect("next true move");
    raw_unit_event(&mut tx, 3, Some("A"), "B", None).await;
    sqlx::query("SET CONSTRAINTS ALL IMMEDIATE")
        .execute(&mut *tx)
        .await
        .expect("a later move preserves the old count point");
    let all: bool = sqlx::query_scalar(
        "SELECT bool_and(wms_serial_event_valid(tenant,warehouse_id,id)) FROM wms_events",
    )
    .fetch_one(&mut *tx)
    .await
    .expect("historical count proofs survive later movement");
    assert!(all);
    let unit: i64=sqlx::query_scalar("SELECT revision FROM wms_serial_units WHERE tenant='serial-test' AND sku='CONTROLLED' AND serial='ONE'").fetch_one(&mut *tx).await.expect("actual unit revision");
    assert_eq!(unit, 2);
    let head: i64 = sqlx::query_scalar(
        "SELECT version FROM wms_serial_heads WHERE tenant='serial-test' AND sku='CONTROLLED'",
    )
    .fetch_one(&mut *tx)
    .await
    .expect("SKU observations advance independently");
    assert_eq!(head, 3);
    tx.rollback().await.expect("discard private schema");
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL; exercises a private serial schema"]
async fn serial_schema_history_chain_rejects_stock_balanced_forgery_with_complete_original_fact_sources(
) {
    let mut tx = schema().await;
    controlled_catalog(&mut tx).await;
    raw_unit_event(&mut tx, 1, None, "A", None).await;
    sqlx::query("SET CONSTRAINTS ALL IMMEDIATE")
        .execute(&mut *tx)
        .await
        .expect("complete positive raw registry birth");
    sqlx::query("SET CONSTRAINTS ALL DEFERRED")
        .execute(&mut *tx)
        .await
        .expect("next original event");
    raw_unit_event(&mut tx, 2, Some("A"), "B", None).await;
    sqlx::query("SET CONSTRAINTS ALL IMMEDIATE")
        .execute(&mut *tx)
        .await
        .expect("complete positive raw registry move");
    let valid: bool = sqlx::query_scalar("SELECT wms_serial_scope_current_valid('serial-test','main') AND bool_and(wms_serial_unit_bound(u)) FROM wms_serial_units u").fetch_one(&mut *tx).await.expect("current custody and original-chain proof");
    assert!(valid);
    sqlx::query("SET CONSTRAINTS ALL DEFERRED")
        .execute(&mut *tx)
        .await
        .expect("forensic negative case");
    sqlx::query("SAVEPOINT original_chain_forgery")
        .execute(&mut *tx)
        .await
        .expect("forensic fixture");
    raw_unit_event(&mut tx, 3, Some("B"), "A", Some("C")).await;
    let balanced: bool =
        sqlx::query_scalar("SELECT wms_serial_scope_current_valid('serial-test','main')")
            .fetch_one(&mut *tx)
            .await
            .expect("physical/registry totals remain balanced");
    assert!(balanced);
    let error = sqlx::query("SET CONSTRAINTS ALL IMMEDIATE")
        .execute(&mut *tx)
        .await
        .expect_err("complete new original event cannot rewrite previous actual custody");
    assert_eq!(
        error.as_database_error().and_then(|e| e.code()).as_deref(),
        Some("23514")
    );
    assert!(error.to_string().contains("Original serial source"));
    sqlx::query("ROLLBACK TO SAVEPOINT original_chain_forgery")
        .execute(&mut *tx)
        .await
        .expect("restore original positive history");
    sqlx::query("SET CONSTRAINTS ALL IMMEDIATE")
        .execute(&mut *tx)
        .await
        .expect("original positive history remains valid");
    tx.rollback().await.expect("discard private schema");
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL; exercises a private serial schema"]
async fn serial_schema_inputs_require_each_original_target_and_literal_uid_exactly_once() {
    let mut tx = schema().await;
    controlled_catalog(&mut tx).await;
    let query = "SELECT wms_serial_inputs_match_command('serial-test','main',$1,$2)";
    let count = serde_json::json!({"type":"count_stock","sku":"CONTROLLED","lot":"LOT","location":"A","counted":2,"reason":"Observed exact labels"});
    let exact = serde_json::json!([{"target":[],"sku":"CONTROLLED","serials":["Exact","exact"]}]);
    let valid: bool = sqlx::query_scalar(query)
        .bind(&count)
        .bind(&exact)
        .fetch_one(&mut *tx)
        .await
        .expect("valid case-sensitive target");
    assert!(valid);
    for bad in [
        serde_json::json!([]),
        serde_json::json!([{"target":[],"sku":"CONTROLLED","serials":["Exact","Exact"]}]),
        serde_json::json!([{"target":["wrong"],"sku":"CONTROLLED","serials":["Exact","exact"]}]),
        serde_json::json!([{"target":[],"sku":"CONTROLLED","serials":["Exact"]}]),
        serde_json::json!([{"target":[],"sku":"CONTROLLED","serials":["Exact",true]}]),
        serde_json::json!([{"target":[],"sku":"CONTROLLED","serials":["Exact"," exact"]}]),
        serde_json::json!([{"target":[],"sku":"CONTROLLED","serials":["Exact","exact"],"invented":true}]),
        serde_json::json!([{"target":[],"sku":"CONTROLLED","serials":["Exact","exact"]},{"target":[],"sku":"CONTROLLED","serials":[]}]),
    ] {
        let valid: bool = sqlx::query_scalar(query)
            .bind(&count)
            .bind(bad)
            .fetch_one(&mut *tx)
            .await
            .expect("malformed/missing serial observations");
        assert!(!valid);
    }
    let zero = serde_json::json!({"type":"count_stock","sku":"CONTROLLED","lot":"LOT","location":"A","counted":0,"reason":"Actually counted empty"});
    let valid: bool = sqlx::query_scalar(query)
        .bind(zero)
        .bind(serde_json::json!([{"target":[],"sku":"CONTROLLED","serials":[]}]))
        .fetch_one(&mut *tx)
        .await
        .expect("explicit observed zero");
    assert!(valid);
    let untracked = serde_json::json!({"type":"count_stock","sku":"SKU","lot":"LOT","location":"A","counted":2,"reason":"Original whole-each operation"});
    let valid: bool = sqlx::query_scalar(query)
        .bind(untracked)
        .bind(serde_json::json!([]))
        .fetch_one(&mut *tx)
        .await
        .expect("legacy untracked operation");
    assert!(valid);
    let foreign: bool =
        sqlx::query_scalar("SELECT wms_serial_inputs_match_command('serial-test','other',$1,$2)")
            .bind(count)
            .bind(exact)
            .fetch_one(&mut *tx)
            .await
            .expect("unconfigured destination");
    assert!(!foreign);
    tx.rollback().await.expect("discard private schema");
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL; exercises a private serial schema"]
async fn serial_schema_zero_count_preserves_original_observation_and_serial_review_without_unit_steps(
) {
    let mut tx = schema().await;
    controlled_catalog(&mut tx).await;
    let event_id = domain::id();
    let result =
        serde_json::json!({"entity_id":"CONTROLLED / A","version":2,"type":"inventory.counted"});
    let payload = serde_json::json!({"schema_version":2,"id":event_id,"tenant":"serial-test","warehouse_id":"main","version":2,
        "actor":"synthetic:admin","occurred_at":domain::now(),"request_id":"serial-raw-zero-count","type":"inventory.counted","result":result,
        "command":{"type":"count_stock","sku":"CONTROLLED","lot":"LOT","location":"A","counted":0,"reason":"Explicitly observed zero identities"},
        "inventory_changes":[{"stock":{"sku":"CONTROLLED","lot":"LOT","location":"A","on_hand":0,"reserved":0},"version":2}],
        "order":null,"previous_order":null,"lot_changes":[],"lot_decisions":[],
        "serial_heads":{"CONTROLLED":1},"serial_steps":[],"serial_inputs":[{"target":[],"sku":"CONTROLLED","serials":[]}]});
    sqlx::query(
        "UPDATE wms_warehouses SET version=2 WHERE tenant='serial-test' AND warehouse_id='main'",
    )
    .execute(&mut *tx)
    .await
    .expect("original count version");
    sqlx::query("INSERT INTO wms_stock VALUES('serial-test','main','CONTROLLED','LOT','A',0,0)")
        .execute(&mut *tx)
        .await
        .expect("zero stock bucket");
    sqlx::query("UPDATE wms_serial_heads SET version=1,last_event_id=$1 WHERE tenant='serial-test' AND sku='CONTROLLED'").bind(&event_id).execute(&mut *tx).await.expect("one original serial review advance");
    sqlx::query("INSERT INTO wms_serial_batches VALUES('serial-test','CONTROLLED',1,'main',2,$1)")
        .bind(&event_id)
        .execute(&mut *tx)
        .await
        .expect("zero-count source batch");
    sqlx::query("INSERT INTO wms_events(id,tenant,warehouse_id,version,payload) VALUES($1,'serial-test','main',2,$2)").bind(&event_id).bind(payload).execute(&mut *tx).await.expect("original zero-count event");
    sqlx::query("INSERT INTO wms_commands(tenant,warehouse_id,request_id,actor,digest,result) VALUES('serial-test','main','serial-raw-zero-count','synthetic:admin',$1,$2)").bind("0".repeat(64)).bind(result).execute(&mut *tx).await.expect("raw attributed result, not wire-digest proof");
    let missing: bool =
        sqlx::query_scalar("SELECT wms_serial_event_inputs_bound('serial-test','main',$1)")
            .bind(&event_id)
            .fetch_one(&mut *tx)
            .await
            .expect("missing zero observation");
    assert!(!missing);
    sqlx::query("INSERT INTO wms_serial_observations VALUES('serial-test','main',$1,1,'CONTROLLED',1,'[]','[]')").bind(&event_id).execute(&mut *tx).await.expect("literal zero observation");
    let bound: bool =
        sqlx::query_scalar("SELECT wms_serial_event_inputs_bound('serial-test','main',$1)")
            .bind(&event_id)
            .fetch_one(&mut *tx)
            .await
            .expect("complete zero observation");
    assert!(bound);
    sqlx::query("SET CONSTRAINTS ALL IMMEDIATE")
        .execute(&mut *tx)
        .await
        .expect("complete positive zero-count fact bindings");
    let units: i64 = sqlx::query_scalar("SELECT count(*) FROM wms_serial_units")
        .fetch_one(&mut *tx)
        .await
        .expect("no invented unit");
    assert_eq!(units, 0);
    let steps: i64 = sqlx::query_scalar("SELECT count(*) FROM wms_serial_steps")
        .fetch_one(&mut *tx)
        .await
        .expect("no invented unit transition");
    assert_eq!(steps, 0);
    let version: i64 = sqlx::query_scalar(
        "SELECT version FROM wms_serial_heads WHERE tenant='serial-test' AND sku='CONTROLLED'",
    )
    .fetch_one(&mut *tx)
    .await
    .expect("observations still fence lineage pages");
    assert_eq!(version, 1);
    tx.rollback().await.expect("discard private schema");
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL; exercises a private serial schema"]
async fn serial_schema_current_scope_counts_are_independent_of_current_registry_claims() {
    let mut tx = schema().await;
    controlled_catalog(&mut tx).await;
    let valid_on_hand: bool = sqlx::query_scalar(
        "SELECT wms_serial_place_references_valid('serial-test','CONTROLLED','LOT',$1)",
    )
    .bind(serde_json::json!({"status":"on_hand","warehouse_id":"main","location":"A"}))
    .fetch_one(&mut *tx)
    .await
    .expect("configured physical custody references");
    assert!(valid_on_hand);
    for place in [
        serde_json::json!({"status":"staged","warehouse_id":"main","order_id":"unknown","line_id":"line","allocation_location":"A","package_id":null}),
        serde_json::json!({"status":"shipped","warehouse_id":"main","order_id":"unknown","line_id":"line","package_id":null,"handoff_event_id":"unknown","return_receipt_id":null}),
        serde_json::json!({"status":"shipped","warehouse_id":"main","order_id":"unknown","line_id":"line","package_id":"unknown","handoff_event_id":"unknown","return_receipt_id":"unknown"}),
        serde_json::json!({"status":"in_transit","source_warehouse_id":"main","destination_warehouse_id":"other","transfer_id":"unknown","line_id":"line"}),
        serde_json::json!({"status":"lost","source_warehouse_id":"main","destination_warehouse_id":"other","transfer_id":"unknown","line_id":"line","loss_id":"unknown"}),
    ] {
        let valid: bool = sqlx::query_scalar(
            "SELECT wms_serial_place_references_valid('serial-test','CONTROLLED','LOT',$1)",
        )
        .bind(place)
        .fetch_one(&mut *tx)
        .await
        .expect("every position reference branch must execute against the real schema");
        assert!(!valid);
    }
    let query = "SELECT wms_serial_scope_current_valid('serial-test','main')";
    let empty: bool = sqlx::query_scalar(query)
        .fetch_one(&mut *tx)
        .await
        .expect("empty controlled scope");
    assert!(empty);
    sqlx::query("SAVEPOINT incomplete_custody")
        .execute(&mut *tx)
        .await
        .expect("isolated deliberately incomplete custody fixture");
    sqlx::raw_sql(r#"INSERT INTO wms_serial_units VALUES('serial-test','CONTROLLED','ONE','LOT',1,'{"status":"on_hand","warehouse_id":"main","location":"A"}','deliberately-deferred-source')"#)
        .execute(&mut *tx).await.expect("deferred identity fixture");
    let extra: bool = sqlx::query_scalar(query)
        .fetch_one(&mut *tx)
        .await
        .expect("extra registry custody check");
    assert!(!extra);
    sqlx::query("INSERT INTO wms_stock VALUES('serial-test','main','CONTROLLED','LOT','A',1,0)")
        .execute(&mut *tx)
        .await
        .expect("matching physical stock, separately uncommitted ledger source");
    let matched: bool = sqlx::query_scalar(query)
        .fetch_one(&mut *tx)
        .await
        .expect("matched serial/physical equation");
    assert!(matched);
    sqlx::query("UPDATE wms_stock SET on_hand=2 WHERE tenant='serial-test' AND sku='CONTROLLED'")
        .execute(&mut *tx)
        .await
        .expect("missing second identity fixture");
    let missing: bool = sqlx::query_scalar(query)
        .fetch_one(&mut *tx)
        .await
        .expect("missing identity check");
    assert!(!missing);
    sqlx::query("ROLLBACK TO SAVEPOINT incomplete_custody")
        .execute(&mut *tx)
        .await
        .expect("discard physical fixture that deliberately lacks original unit/ledger sources");
    sqlx::query("SET CONSTRAINTS ALL IMMEDIATE")
        .execute(&mut *tx)
        .await
        .expect("only valid catalog remains");
    tx.rollback().await.expect("discard private schema");
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL; exercises a private serial schema"]
async fn serial_schema_preserves_legacy_catalog_and_checks_utf8_literal_places() {
    let mut tx = schema().await;
    let data: serde_json::Value = sqlx::query_scalar("SELECT data FROM wms_products WHERE tenant='serial-test' AND warehouse_id='main' AND sku='SKU'").fetch_one(&mut *tx).await.expect("legacy product");
    assert_eq!(
        data,
        serde_json::json!({"sku":"SKU","name":"Legacy","category":"Synthetic","reorder_point":0})
    );
    let mode: bool = sqlx::query_scalar(
        "SELECT controlled FROM wms_serial_modes WHERE tenant='serial-test' AND sku='SKU'",
    )
    .fetch_one(&mut *tx)
    .await
    .expect("explicit legacy untracked mode");
    assert!(!mode);
    for text in [
        "é".repeat(121),
        "invalid\u{85}control".into(),
        " wrapped ".into(),
        "\u{a0}wrapped".into(),
        "".into(),
    ] {
        let valid: bool = sqlx::query_scalar("SELECT wms_serial_text($1)")
            .bind(text)
            .fetch_one(&mut *tx)
            .await
            .expect("text guard");
        assert!(!valid);
    }
    let valid: bool = sqlx::query_scalar("SELECT wms_serial_text($1)")
        .bind("é".repeat(120))
        .fetch_one(&mut *tx)
        .await
        .expect("UTF8 boundary");
    assert!(valid);
    for place in [
        serde_json::json!({"status":"on_hand","warehouse_id":"main","location":"STAGE-01"}),
        serde_json::json!({"status":"missing","warehouse_id":"main","location":"STAGE-01"}),
        serde_json::json!({"status":"on_hand","warehouse_id":"main","location":"A","invented":true}),
        serde_json::json!({"status":"staged","warehouse_id":"main","order_id":"order","line_id":"line","allocation_location":"A"}),
    ] {
        let valid: bool = sqlx::query_scalar("SELECT wms_serial_place_valid($1)")
            .bind(place)
            .fetch_one(&mut *tx)
            .await
            .expect("place guard");
        assert!(!valid);
    }
    let valid: bool = sqlx::query_scalar("SELECT wms_serial_place_valid($1)").bind(serde_json::json!({"status":"staged","warehouse_id":"main","order_id":"order","line_id":"line","allocation_location":"A","package_id":null})).fetch_one(&mut *tx).await.expect("exact staged identity");
    assert!(valid);
    tx.rollback().await.expect("discard private test schema");
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL; exercises a private serial schema"]
async fn serial_schema_rejects_mode_changes_foreign_facility_drift_and_missing_original_facts() {
    let mut tx = schema().await;
    rejected(&mut tx, "UPDATE wms_products SET data=data || '{\"serial_controlled\":true}' WHERE tenant='serial-test' AND sku='SKU'", "23514").await;
    rejected(&mut tx, "UPDATE wms_serial_modes SET controlled=true,source_warehouse_id='main',source_event_id='invented' WHERE tenant='serial-test' AND sku='SKU'", "23514").await;
    rejected(&mut tx, "INSERT INTO wms_products VALUES('serial-test','other','SKU','{\"sku\":\"SKU\",\"name\":\"Synthetic\",\"category\":\"Test\",\"reorder_point\":0,\"serial_controlled\":true}')", "23514").await;
    rejected(
        &mut tx,
        "INSERT INTO wms_serial_heads VALUES('serial-test','SKU',0,NULL)",
        "23514",
    )
    .await;
    rejected(
        &mut tx,
        "DELETE FROM wms_serial_modes WHERE tenant='serial-test' AND sku='SKU'",
        "23514",
    )
    .await;
    sqlx::query("SET CONSTRAINTS ALL IMMEDIATE")
        .execute(&mut *tx)
        .await
        .expect("settled source facts before truncate admission");
    rejected(&mut tx, "TRUNCATE wms_serial_modes CASCADE", "23514").await;
    sqlx::query("SET CONSTRAINTS ALL IMMEDIATE")
        .execute(&mut *tx)
        .await
        .expect("unchanged legacy facts remain valid");
    tx.rollback().await.expect("discard private test schema");
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL; exercises a private serial schema"]
async fn serial_schema_uid_key_is_global_per_sku_and_case_sensitive_before_deferred_source_checks()
{
    let mut tx = schema().await;
    // Source facts intentionally remain deferred/uncommitted in this key test.
    // It proves the unique-key rejection, not complete successful admission.
    sqlx::query("SAVEPOINT registry_key")
        .execute(&mut *tx)
        .await
        .expect("key test");
    sqlx::raw_sql(r#"INSERT INTO wms_serial_units VALUES('serial-test','SKU','Exact','LOT',1,'{"status":"on_hand","warehouse_id":"main","location":"A"}','deferred-source');
      INSERT INTO wms_serial_units VALUES('serial-test','SKU','exact','LOT',1,'{"status":"on_hand","warehouse_id":"other","location":"A"}','deferred-source');"#).execute(&mut *tx).await.expect("literal case is distinct");
    sqlx::query("SAVEPOINT duplicate_key")
        .execute(&mut *tx)
        .await
        .expect("duplicate test");
    let error = sqlx::raw_sql(r#"INSERT INTO wms_serial_units VALUES('serial-test','SKU','Exact','ANOTHER-LOT',1,'{"status":"on_hand","warehouse_id":"other","location":"B"}','another-source')"#).execute(&mut *tx).await.expect_err("same identity cannot be registered in a second facility or lot");
    assert_eq!(
        error.as_database_error().and_then(|e| e.code()).as_deref(),
        Some("23505")
    );
    assert_eq!(
        error.as_database_error().and_then(|e| e.constraint()),
        Some("wms_serial_units_pkey")
    );
    sqlx::query("ROLLBACK TO SAVEPOINT registry_key")
        .execute(&mut *tx)
        .await
        .expect("discard incomplete key fixture");
    tx.rollback().await.expect("discard private test schema");
}

#[path = "serial_registry/workflows.rs"]
mod workflows;
