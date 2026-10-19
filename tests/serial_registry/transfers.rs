//! Original cross-facility custody, with no application transition planner.
use super::super::{controlled_catalog, raw_unit_event, schema};
use super::{begin_in, domain, record, settle, Event, PgConnection};
use serde_json::{json, Value};

fn transit() -> Value {
    json!({"status":"in_transit","source_warehouse_id":"main","destination_warehouse_id":"other","transfer_id":"TRANSFER","line_id":"TRANSFER-LINE"})
}

async fn dispatch(c: &mut PgConnection) {
    let mut event = begin_in(c, "main").await;
    let version: i64 = sqlx::query_scalar(
        "SELECT version FROM wms_warehouses WHERE tenant='serial-test' AND warehouse_id='main'",
    )
    .fetch_one(&mut *c)
    .await
    .expect("actual dispatch head");
    let movement = domain::id();
    sqlx::query("INSERT INTO wms_transfers(tenant,id,source_warehouse_id,destination_warehouse_id,reference,carrier,handoff_reference,actor,dispatched_at,source_version,source_event_id) VALUES('serial-test','TRANSFER','main','other','Synthetic transfer','Synthetic carrier','SYNTHETIC-DISPATCH','synthetic:admin',$1,$2,$3)")
        .bind(&event.at).bind(version).bind(&event.id).execute(&mut *c).await.expect("original immutable physical source handoff");
    sqlx::query("INSERT INTO wms_transfer_lines VALUES('serial-test','TRANSFER','TRANSFER-LINE','main','other','CONTROLLED','LOT','A',1,$1)")
        .bind(&movement).execute(&mut *c).await.expect("exact original dispatch line/bin/lot");
    sqlx::query("UPDATE wms_stock SET on_hand=0 WHERE tenant='serial-test' AND warehouse_id='main' AND sku='CONTROLLED' AND location='A'")
        .execute(&mut *c).await.expect("source physically relinquishes original unit");
    let data = json!({"id":movement,"kind":"transfer_dispatch","sku":"CONTROLLED","lot":"LOT","from":"A","to":null,"quantity":1,"reference":"TRANSFER","receipt_id":null,"receipt_line_id":null,"reason":"SYNTHETIC-DISPATCH","actor":"synthetic:admin","at":event.at,"transfer_id":"TRANSFER","transfer_line_id":"TRANSFER-LINE","transfer_event_id":event.id});
    sqlx::query("INSERT INTO wms_movements(id,tenant,warehouse_id,sku,lot,from_location,to_location,quantity,kind,reference,transfer_id,transfer_line_id,transfer_event_id,data) VALUES($1,'serial-test','main','CONTROLLED','LOT','A',NULL,1,'transfer_dispatch','TRANSFER','TRANSFER','TRANSFER-LINE',$2,$3)")
        .bind(movement).bind(&event.id).bind(data).execute(&mut *c).await.expect("independent append-only source dispatch movement");
    event.command = json!({"type":"dispatch_transfer","reference":"Synthetic transfer","destination_warehouse_id":"other","carrier":"Synthetic carrier","handoff_reference":"SYNTHETIC-DISPATCH","lines":[{"id":"TRANSFER-LINE","sku":"CONTROLLED","lot":"LOT","location":"A","quantity":1}]});
    event.kind = "transfer.dispatched";
    event.entity = "TRANSFER".into();
    event.inputs = json!([{"target":["TRANSFER-LINE"],"sku":"CONTROLLED","serials":["ONE"]}]);
    event.effects = vec![("ONE".into(), json!(["TRANSFER-LINE"]), transit())];
    record(c, event).await;
}

async fn loss(c: &mut PgConnection, reversal: bool, omit_effect: bool) {
    let mut event = begin_in(c, "other").await;
    let version: i64 = sqlx::query_scalar(
        "SELECT version FROM wms_warehouses WHERE tenant='serial-test' AND warehouse_id='other'",
    )
    .fetch_one(&mut *c)
    .await
    .expect("actual destination settlement head");
    let (id, kind, original, incident) = if reversal {
        ("REVERSAL", "reversal", Some("LOSS"), None)
    } else {
        ("LOSS", "loss", None, Some("SYNTHETIC-INCIDENT"))
    };
    let reason = if reversal {
        "Original lost unit recovered; arrival remains required"
    } else {
        "Original dispatched unit confirmed missing"
    };
    sqlx::query("INSERT INTO wms_transfer_loss_records VALUES('serial-test',$1,'TRANSFER','TRANSFER-LINE','other',$2,1,$3,$4,$5,'synthetic:admin',$6,$7,$8)")
        .bind(id).bind(kind).bind(original).bind(incident).bind(reason).bind(&event.at).bind(version).bind(&event.id).execute(&mut *c).await.expect("immutable original incident or exact reversal");
    event.loss=sqlx::query_scalar("SELECT to_jsonb(r)-'tenant' FROM wms_transfer_loss_records r WHERE tenant='serial-test' AND id=$1")
        .bind(id).fetch_one(&mut *c).await.expect("exact normalized loss source bytes");
    event.command = if reversal {
        json!({"type":"reverse_transfer_loss","loss_id":"LOSS","quantity":1,"reason":reason})
    } else {
        json!({"type":"record_transfer_loss","transfer_id":"TRANSFER","line_id":"TRANSFER-LINE","quantity":1,"incident_reference":incident,"reason":reason})
    };
    event.kind = if reversal {
        "transfer.loss_reversed"
    } else {
        "transfer.loss_recorded"
    };
    event.entity = id.into();
    event.inputs = json!([{"target":[],"sku":"CONTROLLED","serials":["ONE"]}]);
    if !omit_effect {
        let place = if reversal {
            transit()
        } else {
            json!({"status":"lost","source_warehouse_id":"main","destination_warehouse_id":"other","transfer_id":"TRANSFER","line_id":"TRANSFER-LINE","loss_id":"LOSS"})
        };
        event.effects = vec![("ONE".into(), json!([]), place)];
    }
    record(c, event).await;
}

async fn import_arrival_lot(c: &mut PgConnection, event: &mut Event, version: i64) {
    let source_version: i64 = sqlx::query_scalar(
        "SELECT version FROM wms_warehouses WHERE tenant='serial-test' AND warehouse_id='main'",
    )
    .fetch_one(&mut *c)
    .await
    .expect("actual source lot review at arrival");
    let source: Value =
        sqlx::query_scalar("SELECT wms_lot_document('serial-test','main','CONTROLLED','LOT',$1)")
            .bind(source_version)
            .fetch_one(&mut *c)
            .await
            .expect("actual current source restrictions");
    let frozen: Value=sqlx::query_scalar("SELECT document FROM wms_lot_decisions WHERE tenant='serial-test' AND warehouse_id='main' AND version=3 AND kind='transfer_dispatch' AND sku='CONTROLLED' AND lot='LOT'")
        .fetch_one(&mut *c).await.expect("original retained dispatch eligibility");
    let document = json!({"kind":"lot.arrival_restricted","sku":"CONTROLLED","lot":"LOT","required":null,"expires_at":null,"hold_id":null,"reason":"Transfer arrival retains original and current lot restrictions","actor":"synthetic:admin","recorded_at":event.at,
        "arrival":{"transfer_id":"TRANSFER","line_id":"TRANSFER-LINE","source_warehouse_id":"main","source_version":source_version,"source":source,"source_required":false,"dispatched":frozen,"expiry_recorded":false}});
    sqlx::query("INSERT INTO wms_lot_changes VALUES('serial-test','other',$1,0,'CONTROLLED','LOT','lot.arrival_restricted',$2,$3)")
        .bind(version).bind(&event.id).bind(&document).execute(&mut *c).await.expect("original source-bound arrival policy import");
    event.lot_changes = json!([document]);
}

async fn arrival(c: &mut PgConnection) {
    let mut event = begin_in(c, "other").await;
    let version: i64 = sqlx::query_scalar(
        "SELECT version FROM wms_warehouses WHERE tenant='serial-test' AND warehouse_id='other'",
    )
    .fetch_one(&mut *c)
    .await
    .expect("actual arrival head");
    import_arrival_lot(c, &mut event, version).await;
    sqlx::query(
        "INSERT INTO wms_stock VALUES('serial-test','other','CONTROLLED','LOT','RECEIVING',1,0)",
    )
    .execute(&mut *c)
    .await
    .expect("actual destination receiving custody");
    let movement = domain::id();
    let data = json!({"id":movement,"kind":"transfer_receive","sku":"CONTROLLED","lot":"LOT","from":null,"to":"RECEIVING","quantity":1,"reference":"TRANSFER","receipt_id":null,"receipt_line_id":null,"reason":"SYNTHETIC-ARRIVAL","actor":"synthetic:admin","at":event.at,"transfer_id":"TRANSFER","transfer_line_id":"TRANSFER-LINE","transfer_event_id":event.id});
    sqlx::query("INSERT INTO wms_movements(id,tenant,warehouse_id,sku,lot,from_location,to_location,quantity,kind,reference,transfer_id,transfer_line_id,transfer_event_id,data) VALUES($1,'serial-test','other','CONTROLLED','LOT',NULL,'RECEIVING',1,'transfer_receive','TRANSFER','TRANSFER','TRANSFER-LINE',$2,$3)")
        .bind(movement).bind(&event.id).bind(data).execute(&mut *c).await.expect("independent physical destination arrival");
    event.command = json!({"type":"receive_transfer","transfer_id":"TRANSFER","line_id":"TRANSFER-LINE","location":"RECEIVING","quantity":1,"arrival_reference":"SYNTHETIC-ARRIVAL"});
    event.kind = "transfer.received";
    event.entity = "TRANSFER".into();
    event.inputs = json!([{"target":[],"sku":"CONTROLLED","serials":["ONE"]}]);
    event.effects = vec![(
        "ONE".into(),
        json!([]),
        json!({"status":"on_hand","warehouse_id":"other","location":"RECEIVING"}),
    )];
    record(c, event).await;
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL; development schema is not registered"]
async fn destination_loss_reversal_and_actual_arrival_retain_original_global_serial_custody() {
    let mut tx = schema().await;
    controlled_catalog(&mut tx).await;
    raw_unit_event(&mut tx, 1, None, "A", None).await;
    sqlx::raw_sql(r#"INSERT INTO wms_products VALUES('serial-test','other','CONTROLLED','{"sku":"CONTROLLED","name":"Controlled","category":"Synthetic","reorder_point":0,"serial_controlled":true}');
      INSERT INTO wms_locations VALUES('serial-test','other','RECEIVING','receiving','{"code":"RECEIVING","kind":"receiving","zone":"Synthetic"}');"#)
        .execute(&mut *tx).await.expect("matching destination tracking mode and receiving bin");
    settle(&mut tx).await;
    dispatch(&mut tx).await;
    settle(&mut tx).await;
    for reversal in [false, true] {
        sqlx::query("SAVEPOINT omitted_transfer_identity")
            .execute(&mut *tx)
            .await
            .expect("forensic complete incident with omitted identity");
        loss(&mut tx, reversal, true).await;
        let original: bool =
            sqlx::query_scalar("SELECT wms_transfer_consistent('serial-test','TRANSFER')")
                .fetch_one(&mut *tx)
                .await
                .expect("all original transfer and incident sources are independently consistent");
        assert!(original);
        let identity: bool =
            sqlx::query_scalar("SELECT wms_serial_scope_transit_valid('serial-test','other')")
                .fetch_one(&mut *tx)
                .await
                .expect("independent exact original per-loss UID equation");
        assert!(!identity);
        let error = sqlx::query("SET CONSTRAINTS ALL IMMEDIATE")
            .execute(&mut *tx)
            .await
            .expect_err("loss/reversal must settle the exact original unit");
        assert_eq!(
            error.as_database_error().and_then(|e| e.code()).as_deref(),
            Some("23514")
        );
        sqlx::raw_sql("ROLLBACK TO SAVEPOINT omitted_transfer_identity; RELEASE SAVEPOINT omitted_transfer_identity").execute(&mut *tx).await.expect("restore prior exact in-transit/lost unit ownership");
        loss(&mut tx, reversal, false).await;
        settle(&mut tx).await;
        let physical: i64=sqlx::query_scalar("SELECT COALESCE(sum(on_hand),0)::bigint FROM wms_stock WHERE tenant='serial-test' AND sku='CONTROLLED'")
            .fetch_one(&mut *tx).await.expect("incident settlement does not invent arrival stock");
        assert_eq!(physical, 0);
    }
    arrival(&mut tx).await;
    settle(&mut tx).await;
    let proof: bool=sqlx::query_scalar("SELECT wms_transfer_consistent('serial-test','TRANSFER') AND wms_serial_scope_current_valid('serial-test','main') AND wms_serial_scope_current_valid('serial-test','other') AND wms_serial_scope_transit_valid('serial-test','main') AND (SELECT bool_and(wms_serial_event_valid(tenant,warehouse_id,id)) FROM wms_events)")
        .fetch_one(&mut *tx).await.expect("all original source, loss, reversal, lot import and actual arrival proofs");
    assert!(proof);
    let place: Value=sqlx::query_scalar("SELECT place FROM wms_serial_units WHERE tenant='serial-test' AND sku='CONTROLLED' AND serial='ONE'")
        .fetch_one(&mut *tx).await.expect("same literal unit reaches destination physical custody");
    assert_eq!(
        place,
        json!({"status":"on_hand","warehouse_id":"other","location":"RECEIVING"})
    );
    let reviews: (i64,i64)=sqlx::query_as("SELECT (SELECT version FROM wms_warehouses WHERE tenant='serial-test' AND warehouse_id='main'),version FROM wms_serial_heads WHERE tenant='serial-test' AND sku='CONTROLLED'")
        .fetch_one(&mut *tx).await.expect("destination work fences global serial pages independently of the unchanged source warehouse head");
    assert_eq!(reviews, (3, 5));
    tx.rollback().await.expect("discard private schema");
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL; development schema is not registered"]
async fn concurrent_facility_reviews_restart_at_the_same_global_serial_head() {
    let mut setup = schema().await;
    controlled_catalog(&mut setup).await;
    raw_unit_event(&mut setup, 1, None, "A", None).await;
    sqlx::raw_sql(r#"INSERT INTO wms_products VALUES('serial-test','other','CONTROLLED','{"sku":"CONTROLLED","name":"Controlled","category":"Synthetic","reorder_point":0,"serial_controlled":true}');
      INSERT INTO wms_locations VALUES('serial-test','other','RECEIVING','receiving','{"code":"RECEIVING","kind":"receiving","zone":"Synthetic"}');"#)
        .execute(&mut *setup).await.expect("matching independent destination catalog");
    settle(&mut setup).await;
    let namespace: String = sqlx::query_scalar("SELECT current_schema()")
        .fetch_one(&mut *setup)
        .await
        .expect("owned synthetic private schema");
    assert!(
        namespace.starts_with("serial_registry_")
            && namespace
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_')
    );
    setup
        .commit()
        .await
        .expect("retain owned isolated schema so independent connections can observe it");
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(3)
        .after_connect(move |c, _| {
            let search_path = format!("SET search_path TO {namespace},public");
            Box::pin(async move {
                sqlx::query(&search_path).execute(c).await?;
                Ok(())
            })
        })
        .connect(&std::env::var("WMS_TEST_DATABASE_URL").expect("isolated test URL"))
        .await
        .expect("independent connections to only the owned private schema");
    let mut source = pool.begin().await.expect("source count transaction");
    let mut destination = pool.begin().await.expect("destination count transaction");
    for c in [&mut *source, &mut *destination] {
        sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ")
            .execute(&mut *c)
            .await
            .expect("actual admission isolation");
        let reviewed: i64 = sqlx::query_scalar(
            "SELECT version FROM wms_serial_heads WHERE tenant='serial-test' AND sku='CONTROLLED'",
        )
        .fetch_one(c)
        .await
        .expect("independent same original global review");
        assert_eq!(reviewed, 1);
    }
    let mut source_event = begin_in(&mut source, "main").await;
    // Distinct warehouse rows are already writable. Only the tenant/SKU head
    // links these reviews; a warehouse-wide lock cannot establish this fence.
    let destination_event = begin_in(&mut destination, "other").await;
    source_event.command = json!({"type":"count_stock","sku":"CONTROLLED","lot":"LOT","location":"A","counted":1,"reason":"Observed unchanged original unit"});
    source_event.kind = "inventory.counted";
    source_event.entity = "CONTROLLED / A".into();
    source_event.inputs = json!([{"target":[],"sku":"CONTROLLED","serials":["ONE"]}]);
    record(&mut source, source_event).await;
    settle(&mut source).await;
    source
        .commit()
        .await
        .expect("complete independently valid original source count commits");
    let error=sqlx::query("UPDATE wms_serial_heads SET version=version+1,last_event_id=$1 WHERE tenant='serial-test' AND sku='CONTROLLED'")
        .bind(destination_event.id).execute(&mut *destination).await.expect_err("stale independent destination admission must restart before serial history writes");
    assert_eq!(
        error.as_database_error().and_then(|e| e.code()).as_deref(),
        Some("40001")
    );
    destination
        .rollback()
        .await
        .expect("rollback every destination change after the serialization failure");
    let actual: (i64,i64,i64,i64)=sqlx::query_as("SELECT (SELECT version FROM wms_warehouses WHERE tenant='serial-test' AND warehouse_id='other'),version,(SELECT revision FROM wms_serial_units WHERE tenant='serial-test' AND sku='CONTROLLED' AND serial='ONE'),(SELECT count(*) FROM wms_serial_batches WHERE tenant='serial-test' AND warehouse_id='other') FROM wms_serial_heads WHERE tenant='serial-test' AND sku='CONTROLLED'")
        .fetch_one(&pool).await.expect("only the valid original count survives");
    assert_eq!(actual, (0, 2, 1, 0));
    let original: bool = sqlx::query_scalar(
        "SELECT bool_and(wms_serial_event_valid(tenant,warehouse_id,id)) FROM wms_events",
    )
    .fetch_one(&pool)
    .await
    .expect("all committed source facts remain complete after conflict rollback");
    assert!(original);
    pool.close().await;
    // The local runner stops and retains its owned DB, including this exact
    // schema. No real workspace or unrelated schema is changed or dropped.
}
