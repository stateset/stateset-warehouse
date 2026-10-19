//! Differential source-bound validation against the immutable schema21 predicate.
use super::*;
use sqlx::{PgConnection, Row};
async fn original(c: &mut PgConnection) {
    sqlx::query("CREATE TEMP TABLE serial_target_oracle_marker(n INTEGER) ON COMMIT DROP")
        .execute(&mut *c)
        .await
        .expect("owned temporary namespace");
    let source = include_str!("../../migrations/021_serial_tracking.sql");
    let start = source
        .find("CREATE FUNCTION wms_serial_inputs_match_command(")
        .expect("original predicate start");
    let tail = &source[start..];
    let end = tail
        .find("\nCREATE FUNCTION wms_serial_event_inputs_bound")
        .expect("original predicate end");
    let sql = tail[..end].replacen(
        "CREATE FUNCTION wms_serial_inputs_match_command(",
        "CREATE FUNCTION pg_temp.wms_serial_inputs_original(",
        1,
    );
    sqlx::raw_sql(&sql)
        .execute(c)
        .await
        .expect("independent unmodified original predicate");
}
async fn compare(
    c: &mut PgConnection,
    tenant: &str,
    command: &Value,
    inputs: &Value,
    expected: bool,
) {
    let (current,old):(bool,bool)=sqlx::query_as("SELECT wms_serial_inputs_match_command($1,'wh-main',$2,$3),pg_temp.wms_serial_inputs_original($1,'wh-main',$2,$3)").bind(tenant).bind(command).bind(inputs).fetch_one(c).await.expect("compare complete original target semantics");
    assert_eq!(
        current, old,
        "original predicate disagreement command{command}, inputs{inputs}"
    );
    assert_eq!(current, expected, "expected target validation outcome");
}
#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn resolved_target_set_matches_original_root_group_shape_and_exact_count_rules() {
    let f = Fixture::new().await;
    f.catalog().await;
    let mut tx = f
        .store
        .pool
        .begin()
        .await
        .expect("owned differential transaction");
    original(&mut tx).await;
    let command = json!({"type":"count_stock","sku":"SERIAL","lot":"LOT","location":"A","counted":2,"reason":"Synthetic complete original set"});
    let good = json!([{"target":[],"sku":"SERIAL","serials":["Case","case"]}]);
    compare(&mut tx, &f.who.tenant, &command, &good, true).await;
    for bad in [
        json!(null),
        json!({}),
        json!([]),
        json!([{"target":[],"sku":"SERIAL","serials":["Case"]}]),
        json!([{"target":[],"sku":"OTHER","serials":["Case","case"]}]),
        json!([{"target":["wrong"],"sku":"SERIAL","serials":["Case","case"]}]),
        json!([{"target":[],"sku":"SERIAL","serials":["Case","Case"]}]),
        json!([{"target":[],"sku":"SERIAL","serials":[" leading","case"]}]),
        json!([{"target":[],"sku":"SERIAL","serials":["Case","case"],"extra":true}]),
        json!([{"target":[],"sku":"SERIAL","serials":null}]),
        json!([{"target":[],"sku":"SERIAL","serials":["Case",1]}]),
    ] {
        compare(&mut tx, &f.who.tenant, &command, &bad, false).await;
    }
    let mut zero = command.clone();
    zero["counted"] = json!(0);
    compare(
        &mut tx,
        &f.who.tenant,
        &zero,
        &json!([{"target":[],"sku":"SERIAL","serials":[]}]),
        true,
    )
    .await;
    let move_zero =
        json!({"type":"move_stock","sku":"SERIAL","lot":"LOT","from":"A","to":"B","quantity":0});
    compare(
        &mut tx,
        &f.who.tenant,
        &move_zero,
        &json!([{"target":[],"sku":"SERIAL","serials":[]}]),
        false,
    )
    .await;
    let unrelated =
        json!({"type":"create_location","code":"NEW","zone":"Synthetic","kind":"storage"});
    compare(&mut tx, &f.who.tenant, &unrelated, &json!([]), true).await;
    let stored =
        sqlx::query("SELECT digest FROM wms_schema_migrations WHERE name='021_serial_tracking'")
            .fetch_one(&mut *tx)
            .await
            .expect("unchanged original migration digest");
    assert!(!stored.get::<String, _>("digest").is_empty());
    tx.rollback().await.expect("remove temporary oracle");
}
#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn hundred_original_line_targets_match_original_predicate_and_actual_dispatch() {
    let f = Fixture::new().await;
    f.catalog().await;
    extended::other_warehouse(&f).await;
    let labels: Vec<String> = (0..100).map(|n| format!("Original-{n:03}")).collect();
    f.execute(json!({"type":"count_stock","sku":"SERIAL","lot":"LOT","location":"A","counted":100,"reason":"Synthetic100original units"}),vec![Input{target:vec![],sku:"SERIAL".into(),serials:labels.clone()}]).await;
    let lines:Vec<Value>=(0..100).map(|n|json!({"id":format!("line-{n:03}"),"sku":"SERIAL","lot":"LOT","location":"A","quantity":1})).collect();
    let command = json!({"type":"dispatch_transfer","reference":"HUNDRED-ORIGINAL-LINES","destination_warehouse_id":"wh-other","carrier":"Synthetic","handoff_reference":"ORIGINAL-HANDOFF","lines":lines});
    let inputs: Vec<Input> = (0..100)
        .map(|n| Input {
            target: vec![format!("line-{n:03}")],
            sku: "SERIAL".into(),
            serials: vec![labels[n].clone()],
        })
        .collect();
    let good = json!(&inputs);
    let mut tx = f
        .store
        .pool
        .begin()
        .await
        .expect("owned100target comparison");
    original(&mut tx).await;
    compare(&mut tx, &f.who.tenant, &command, &good, true).await;
    let mut missing = good.clone();
    missing.as_array_mut().expect("groups").pop();
    compare(&mut tx, &f.who.tenant, &command, &missing, false).await;
    let mut duplicate = good.clone();
    duplicate[99] = duplicate[0].clone();
    compare(&mut tx, &f.who.tenant, &command, &duplicate, false).await;
    let mut reused = good.clone();
    reused[99]["serials"] = reused[0]["serials"].clone();
    compare(&mut tx, &f.who.tenant, &command, &reused, false).await;
    let mut wrong = good.clone();
    wrong[99]["target"] = json!(["absent-line"]);
    compare(&mut tx, &f.who.tenant, &command, &wrong, false).await;
    let mut duplicated_command = command.clone();
    duplicated_command["lines"][99] = duplicated_command["lines"][0].clone();
    compare(&mut tx, &f.who.tenant, &duplicated_command, &good, false).await;
    tx.rollback().await.expect("remove comparison oracle");
    f.execute(command, inputs).await;
    f.proof().await;
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn event_observation_set_matches_original_on_zero_missing_changed_and_extra_rows() {
    let f = Fixture::new().await;
    f.catalog().await;
    let (count,_)=f.execute(json!({"type":"count_stock","sku":"SERIAL","lot":"LOT","location":"A","counted":1,"reason":"Original nonempty observation"}),observed(&[],&["Original"])).await;
    let (zero,_)=f.execute(json!({"type":"count_stock","sku":"SERIAL","lot":"LOT","location":"B","counted":0,"reason":"Original explicit empty group"}),observed(&[],&[])).await;
    let mut tx = f
        .store
        .pool
        .begin()
        .await
        .expect("owned original observation oracle");
    original(&mut tx).await;
    let source = include_str!("../../migrations/021_serial_tracking.sql");
    let start = source
        .find("CREATE FUNCTION wms_serial_event_inputs_bound(")
        .expect("original event inputs");
    let tail = &source[start..];
    let end = tail
        .find("\nCREATE OR REPLACE FUNCTION wms_serial_observation_bound")
        .expect("original metadata function end");
    let sql = tail[..end].replacen(
        "CREATE FUNCTION wms_serial_event_inputs_bound(",
        "CREATE FUNCTION pg_temp.wms_serial_event_inputs_original(",
        1,
    );
    sqlx::raw_sql(&sql)
        .execute(&mut *tx)
        .await
        .expect("original complete group/ordinal/head comparison");
    let events: Vec<String> = sqlx::query_scalar(
        "SELECT id FROM wms_events WHERE tenant=$1 AND payload->>'request_id'=ANY($2)",
    )
    .bind(&f.who.tenant)
    .bind(vec![count.request_id, zero.request_id])
    .fetch_all(&mut *tx)
    .await
    .expect("two original observation events");
    assert_eq!(events.len(), 2);
    for event in &events {
        let pair:(bool,bool)=sqlx::query_as("SELECT wms_serial_event_inputs_bound($1,'wh-main',$2),pg_temp.wms_serial_event_inputs_original($1,'wh-main',$2)").bind(&f.who.tenant).bind(event).fetch_one(&mut *tx).await.expect("original nonempty/zero source");
        assert_eq!(pair, (true, true));
    }
    sqlx::raw_sql("ALTER TABLE wms_serial_observations DISABLE TRIGGER wms_serial_observation_immutable; ALTER TABLE wms_events DISABLE TRIGGER wms_events_immutable").execute(&mut *tx).await.expect("named corruption guards in rollback-only fixture");
    for sql in [
        "DELETE FROM wms_serial_observations WHERE tenant=$1 AND event_id=$2",
        "UPDATE wms_serial_observations SET ordinal=99 WHERE tenant=$1 AND event_id=$2",
        "UPDATE wms_serial_observations SET target='[\"foreign\"]' WHERE tenant=$1 AND event_id=$2",
        "UPDATE wms_serial_observations SET serials='[\"forged\"]' WHERE tenant=$1 AND event_id=$2",
        "INSERT INTO wms_serial_observations(tenant,warehouse_id,event_id,ordinal,sku,sku_version,target,serials) SELECT tenant,warehouse_id,event_id,99,sku,sku_version,jsonb_build_array('extra'),serials FROM wms_serial_observations WHERE tenant=$1 AND event_id=$2",
        "UPDATE wms_events SET payload=jsonb_set(payload,'{serial_heads,SERIAL}','999') WHERE tenant=$1 AND id=$2",
    ] {
        for event in &events {sqlx::query("SAVEPOINT original_observation_damage").execute(&mut *tx).await.expect("damage boundary");sqlx::query(sql).bind(&f.who.tenant).bind(event).execute(&mut *tx).await.expect("retained source damage");let pair:(bool,bool)=sqlx::query_as("SELECT wms_serial_event_inputs_bound($1,'wh-main',$2),pg_temp.wms_serial_event_inputs_original($1,'wh-main',$2)").bind(&f.who.tenant).bind(event).fetch_one(&mut *tx).await.expect("differential damaged source result");assert_eq!(pair,(false,false),"{sql}");sqlx::query("ROLLBACK TO SAVEPOINT original_observation_damage").execute(&mut *tx).await.expect("restore exact original source");}
    }
    tx.rollback()
        .await
        .expect("remove oracle and named corruption fixtures");
    f.proof().await;
}
