//! Independent immutable source23 predicates retain every original dependency.
//! Local payload decoding must preserve both healthy and damaged-source results.
use super::*;
use sqlx::PgConnection;
fn definition<'a>(s: &'a str, start: &str, end: &str) -> &'a str {
    let a = s.rfind(start).expect("original definition");
    let b = s[a..].find(end).expect("original definition boundary") + a;
    &s[a..b]
}
async fn oracle(c: &mut PgConnection) {
    sqlx::query("CREATE TEMP TABLE serial_payload_oracle_marker(n INTEGER) ON COMMIT DROP")
        .execute(&mut *c)
        .await
        .expect("owned temporary oracle namespace");
    let schema21 = include_str!("../../migrations/021_serial_tracking.sql");
    let schema23 = include_str!("../../migrations/023_serial_target_resolution.sql");
    let definitions = [
        definition(
            schema21,
            "CREATE FUNCTION wms_serial_batch_original(",
            "\nCREATE FUNCTION wms_serial_step_bound",
        ),
        definition(
            schema23,
            "CREATE OR REPLACE FUNCTION wms_serial_event_inputs_bound(",
            "\n-- Internal read-only",
        ),
        definition(
            schema21,
            "CREATE OR REPLACE FUNCTION wms_serial_observation_bound(",
            "\nCREATE FUNCTION wms_serial_unit_bound",
        ),
        definition(
            schema21,
            "CREATE FUNCTION wms_serial_step_semantic_valid(",
            "\nCREATE OR REPLACE FUNCTION wms_serial_step_bound",
        ),
        definition(
            schema21,
            "CREATE OR REPLACE FUNCTION wms_serial_step_bound(",
            "\nCREATE INDEX wms_serial_history_stock",
        ),
        definition(
            schema21,
            "CREATE FUNCTION wms_serial_unit_bound(",
            "\n-- Validate the latest projection",
        ),
    ];
    for source in definitions {
        let mut sql = source.to_owned();
        for name in [
            "wms_serial_batch_original",
            "wms_serial_event_inputs_bound",
            "wms_serial_observation_bound",
            "wms_serial_step_semantic_valid",
            "wms_serial_step_bound",
            "wms_serial_unit_bound",
        ] {
            sql = sql.replace(&format!("{name}("), &format!("pg_temp.{name}_v23("));
        }
        sqlx::raw_sql(&sql)
            .execute(&mut *c)
            .await
            .expect("unmodified source23 predicates and dependency graph");
    }
}
async fn compare(c: &mut PgConnection, tenant: &str) {
    for (table, order, current, previous) in [
        (
            "wms_serial_batches",
            "sku,version",
            "wms_serial_batch_original",
            "pg_temp.wms_serial_batch_original_v23",
        ),
        (
            "wms_serial_units",
            "sku,serial",
            "wms_serial_unit_bound",
            "pg_temp.wms_serial_unit_bound_v23",
        ),
        (
            "wms_serial_steps",
            "sku,serial,revision",
            "wms_serial_step_bound",
            "pg_temp.wms_serial_step_bound_v23",
        ),
        (
            "wms_serial_steps",
            "sku,serial,revision",
            "wms_serial_step_semantic_valid",
            "pg_temp.wms_serial_step_semantic_valid_v23",
        ),
        (
            "wms_serial_observations",
            "warehouse_id,event_id,ordinal",
            "wms_serial_observation_bound",
            "pg_temp.wms_serial_observation_bound_v23",
        ),
    ] {
        // Both complete dependency graphs are deliberately expensive. Compare
        // every row in primary-key order, under the same transaction snapshot,
        // without making the oracle itself exceed the product statement limit.
        let count: i64 =
            sqlx::query_scalar(&format!("SELECT count(*) FROM {table} WHERE tenant=$1"))
                .bind(tenant)
                .fetch_one(&mut *c)
                .await
                .expect("complete oracle row count");
        let mut checked = 0;
        while checked < count {
            let (rows, same): (i64, bool) = sqlx::query_as(&format!(
                "SELECT count(*),COALESCE(bool_and({current}(r::{table}) IS NOT DISTINCT FROM {previous}(r::{table})),true) FROM (SELECT * FROM {table} WHERE tenant=$1 ORDER BY {order} LIMIT 32 OFFSET $2) r"
            ))
            .bind(tenant)
            .bind(checked)
            .fetch_one(&mut *c)
            .await
            .expect("exact old/new row-source verdicts");
            assert!(same, "{current} changed an original verdict");
            assert!(rows > 0, "oracle omitted source rows");
            checked += rows;
        }
        assert_eq!(checked, count, "oracle checked every original source row");
    }
    let same:bool=sqlx::query_scalar("SELECT COALESCE(bool_and(wms_serial_event_inputs_bound(e.tenant,e.warehouse_id,e.id) IS NOT DISTINCT FROM pg_temp.wms_serial_event_inputs_bound_v23(e.tenant,e.warehouse_id,e.id)),true) FROM wms_events e WHERE tenant=$1").bind(tenant).fetch_one(c).await.expect("exact original event input verdict");
    assert!(same);
}
#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn decoded_payloads_preserve_every_original_source_predicate_with_malformed_and_changed_facts(
) {
    let f = Fixture::new().await;
    f.catalog().await;
    f.execute(json!({"type":"count_stock","sku":"SERIAL","lot":"LOT","location":"A","counted":2,"reason":"Original exact labels"}),observed(&[],&["Literal","literal"])).await;
    f.execute(
        json!({"type":"move_stock","sku":"SERIAL","lot":"LOT","from":"A","to":"B","quantity":1}),
        observed(&[], &["Literal"]),
    )
    .await;
    let mut tx = f.store.pool.begin().await.expect("owned source oracle");
    oracle(&mut tx).await;
    compare(&mut tx, &f.who.tenant).await;
    sqlx::raw_sql("ALTER TABLE wms_events DISABLE TRIGGER wms_events_immutable; ALTER TABLE wms_commands DISABLE TRIGGER wms_commands_immutable; ALTER TABLE wms_serial_steps DISABLE TRIGGER wms_serial_step_immutable; ALTER TABLE wms_serial_batches DISABLE TRIGGER wms_serial_batch_immutable; ALTER TABLE wms_serial_observations DISABLE TRIGGER wms_serial_observation_immutable").execute(&mut *tx).await.expect("named rollback-only source damage");
    for sql in [
        "UPDATE wms_events SET payload='null' WHERE tenant=$1 AND payload#>>'{command,type}'='count_stock'",
        "UPDATE wms_events SET payload='[]' WHERE tenant=$1 AND payload#>>'{command,type}'='count_stock'",
        "UPDATE wms_events SET payload='123' WHERE tenant=$1 AND payload#>>'{command,type}'='count_stock'",
        "UPDATE wms_events SET payload=jsonb_set(payload,'{id}','[1,2]') WHERE tenant=$1 AND payload#>>'{command,type}'='count_stock'",
        "UPDATE wms_events SET payload=jsonb_set(payload,'{result}','null') WHERE tenant=$1 AND payload#>>'{command,type}'='count_stock'",
        "UPDATE wms_events SET payload=jsonb_set(payload,'{serial_inputs}','{}') WHERE tenant=$1 AND payload#>>'{command,type}'='count_stock'",
        "UPDATE wms_events SET payload=jsonb_set(payload,'{serial_steps}','{}') WHERE tenant=$1 AND payload#>>'{command,type}'='count_stock'",
        "UPDATE wms_events SET payload=jsonb_set(payload,'{command,type}','\"move_stock\"') WHERE tenant=$1 AND payload#>>'{command,type}'='count_stock'",
        "UPDATE wms_events SET payload=payload||jsonb_build_object('unrelated_retained_blob',repeat('Z',200000)) WHERE tenant=$1 AND payload#>>'{command,type}'='count_stock'",
        "UPDATE wms_events SET warehouse_id='foreign' WHERE tenant=$1 AND payload#>>'{command,type}'='count_stock'",
        "UPDATE wms_commands SET result=jsonb_set(result,'{type}','\"forged\"') WHERE tenant=$1 AND result->>'type'='inventory.counted'",
        "UPDATE wms_serial_batches SET warehouse_version=warehouse_version+1 WHERE tenant=$1 AND version=1",
        "UPDATE wms_serial_steps SET recorded_at='forged' WHERE tenant=$1 AND revision=1",
        "UPDATE wms_serial_steps SET actor='forged' WHERE tenant=$1 AND revision=1",
        "UPDATE wms_serial_observations SET ordinal=99 WHERE tenant=$1 AND sku_version=1",
    ] {
        sqlx::query("SAVEPOINT original_payload_damage").execute(&mut *tx).await.expect("damage boundary");sqlx::query(sql).bind(&f.who.tenant).execute(&mut *tx).await.expect("owned original source damage");compare(&mut tx,&f.who.tenant).await;sqlx::query("ROLLBACK TO SAVEPOINT original_payload_damage").execute(&mut *tx).await.expect("restore original source bytes");
    }
    tx.rollback()
        .await
        .expect("remove forensic oracle and bypasses");
    f.proof().await;
}
#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn full_512unit_birth_move_and_unchanged_count_keep_standalone_guards_and_original_replays() {
    let f = Fixture::new().await;
    f.catalog().await;
    let labels: Vec<String> = (0..512).map(|n| format!("Burst-{n:04}")).collect();
    let refs: Vec<&str> = labels.iter().map(String::as_str).collect();
    let (birth,saved)=f.execute(json!({"type":"count_stock","sku":"SERIAL","lot":"LOT","location":"A","counted":512,"reason":"Complete synthetic512unit burst"}),observed(&[],&refs)).await;
    super::reads::verify_large_original_pages(&f, refs[0], 512).await;
    f.execute(
        json!({"type":"move_stock","sku":"SERIAL","lot":"LOT","from":"A","to":"B","quantity":1}),
        observed(&[], &[refs[0]]),
    )
    .await;
    f.execute(json!({"type":"count_stock","sku":"SERIAL","lot":"LOT","location":"A","counted":511,"reason":"Complete unchanged original remaining set"}),observed(&[],&refs[1..])).await;
    f.proof().await;
    let before = f.facts().await;
    let replay = f
        .store
        .execute(&f.who, "wh-main", &birth)
        .await
        .expect("exact original full burst replay");
    assert_eq!(replay["replayed"], true);
    assert_eq!(replay["result"], saved["result"]);
    assert_eq!(f.facts().await, before);
    let mut tx = f.store.pool.begin().await.expect("burst source23 oracle");
    oracle(&mut tx).await;
    compare(&mut tx, &f.who.tenant).await;
    tx.rollback()
        .await
        .expect("remove temporary predicate copies");
}
