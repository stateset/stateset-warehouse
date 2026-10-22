//! Migration 035 checks serial custody per (warehouse, SKU) at commit. These
//! tests prove the SKU-scoped predicates agree exactly with the complete
//! warehouse predicate on deliberately damaged rows, and that only damage to
//! the SKU a commit touches blocks it.
use super::*;

const CASES: &[(&str, &str, &[&str])] = &[
    ("clean", "", &[]),
    (
        "stock above serial custody",
        "UPDATE wms_stock SET on_hand=on_hand+1 WHERE tenant=$1 AND warehouse_id='wh-main' AND sku='SERIAL' AND location='A'",
        &["SERIAL"],
    ),
    (
        "unit moved without stock",
        "UPDATE wms_serial_units SET place=jsonb_set(place,'{location}','\"B\"') WHERE tenant=$1 AND sku='SECOND' AND serial='S-1'",
        &["SECOND"],
    ),
    (
        "unit in an unknown bin",
        "UPDATE wms_serial_units SET place=jsonb_set(place,'{location}','\"Z\"') WHERE tenant=$1 AND sku='SERIAL' AND serial='U-2'",
        &["SERIAL"],
    ),
    (
        "unit staged without allocation",
        "UPDATE wms_serial_units SET place=jsonb_build_object('status','staged','warehouse_id','wh-main','order_id','missing','line_id','missing','allocation_location','A','package_id',null) WHERE tenant=$1 AND sku='SECOND' AND serial='S-2'",
        &["SECOND"],
    ),
    (
        "storage bin no longer storage",
        "UPDATE wms_locations SET kind='staging',data=jsonb_set(data,'{kind}','\"staging\"') WHERE tenant=$1 AND warehouse_id='wh-main' AND code='A'",
        &["SERIAL", "SECOND"],
    ),
];

async fn two_controlled_skus(f: &Fixture) {
    f.catalog().await;
    f.plain(json!({"type":"create_product","sku":"SECOND","name":"Second synthetic serialized product","category":"Synthetic","reorder_point":0,"serial_controlled":true})).await;
    f.execute(json!({"type":"count_stock","sku":"SERIAL","lot":"LOT","location":"A","counted":2,"reason":"Synthetic first SKU"}),observed(&[],&["U-1","U-2"])).await;
    let mut second = observed(&[], &["S-1", "S-2"]);
    second[0].sku = "SECOND".into();
    f.execute(json!({"type":"count_stock","sku":"SECOND","lot":"LOT","location":"A","counted":2,"reason":"Synthetic second SKU"}),second).await;
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn sku_scoped_custody_equals_the_complete_warehouse_predicate_on_damaged_rows() {
    let f = Fixture::new().await;
    two_controlled_skus(&f).await;
    let mut tx = f.store.pool.begin().await.expect("rollback-only damage");
    sqlx::raw_sql("ALTER TABLE wms_serial_units DISABLE TRIGGER USER; ALTER TABLE wms_stock DISABLE TRIGGER USER; ALTER TABLE wms_locations DISABLE TRIGGER USER")
        .execute(&mut *tx).await.expect("bypass guards only inside this transaction");
    for (name, damage, broken) in CASES {
        sqlx::query("SAVEPOINT damage")
            .execute(&mut *tx)
            .await
            .expect("savepoint");
        if !damage.is_empty() {
            sqlx::query(damage)
                .bind(&f.who.tenant)
                .execute(&mut *tx)
                .await
                .expect(name);
        }
        let (whole, scoped, first, second): (bool, bool, bool, bool) = sqlx::query_as(
            r#"
            WITH skus AS (
              SELECT sku FROM wms_serial_units WHERE tenant=$1
              UNION SELECT sku FROM wms_stock WHERE tenant=$1 AND warehouse_id='wh-main'
              UNION SELECT sku FROM wms_allocations WHERE tenant=$1 AND warehouse_id='wh-main'
            ), verdicts AS (
              SELECT sku,wms_serial_scope_current_valid_sku($1,'wh-main',sku)
                AND wms_serial_scope_transit_valid_sku($1,'wh-main',sku) AS ok FROM skus
            )
            SELECT wms_serial_scope_current_valid($1,'wh-main') AND wms_serial_scope_transit_valid($1,'wh-main'),
              (SELECT bool_and(ok) FROM verdicts),
              (SELECT ok FROM verdicts WHERE sku='SERIAL'),
              (SELECT ok FROM verdicts WHERE sku='SECOND')
        "#,
        )
        .bind(&f.who.tenant)
        .fetch_one(&mut *tx)
        .await
        .expect("complete and SKU-scoped verdicts");
        assert_eq!(
            whole, scoped,
            "{name}: warehouse and SKU conjunction differ"
        );
        assert_eq!(whole, broken.is_empty(), "{name}: damage detected");
        assert_eq!(first, !broken.contains(&"SERIAL"), "{name}: SERIAL verdict");
        assert_eq!(
            second,
            !broken.contains(&"SECOND"),
            "{name}: SECOND verdict"
        );
        sqlx::query("ROLLBACK TO SAVEPOINT damage")
            .execute(&mut *tx)
            .await
            .expect("undo damage");
    }
    tx.rollback().await.expect("no damage survives");
    f.proof().await;
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn custody_damage_blocks_only_commits_that_touch_the_damaged_sku() {
    let f = Fixture::new().await;
    two_controlled_skus(&f).await;
    // Deliberate bypass in the owned synthetic DB: one SECOND unit appears in
    // bin B while stock still says A. Guards are restored before committing.
    let mut tx = f.store.pool.begin().await.expect("forensic damage");
    sqlx::raw_sql("ALTER TABLE wms_serial_units DISABLE TRIGGER wms_serial_unit_identity; ALTER TABLE wms_serial_units DISABLE TRIGGER wms_serial_unit_bound")
        .execute(&mut *tx).await.expect("bypass only the named guards");
    sqlx::query("UPDATE wms_serial_units SET place=jsonb_set(place,'{location}','\"B\"') WHERE tenant=$1 AND sku='SECOND' AND serial='S-1'")
        .bind(&f.who.tenant).execute(&mut *tx).await.expect("custody disagrees with stock");
    sqlx::raw_sql("SET CONSTRAINTS ALL IMMEDIATE; ALTER TABLE wms_serial_units ENABLE TRIGGER wms_serial_unit_identity; ALTER TABLE wms_serial_units ENABLE TRIGGER wms_serial_unit_bound")
        .execute(&mut *tx).await.expect("restore guards");
    tx.commit().await.expect("retain synthetic damage");

    let report = f
        .store
        .reconciliation(&f.who.tenant, "wh-main")
        .await
        .expect("full reconciliation");
    assert_eq!(report["serials"]["physical_ok"], false);
    assert_eq!(report["ok"], false);

    // A real command on the undamaged SKU commits.
    let (_, moved) = f
        .execute(
            json!({"type":"move_stock","sku":"SERIAL","lot":"LOT","from":"A","to":"B","quantity":1}),
            observed(&[], &["U-1"]),
        )
        .await;
    assert_eq!(moved["result"]["type"], "inventory.moved", "{moved}");

    // Any commit that touches the damaged SKU's custody is still rejected,
    // here an otherwise inert stock row update checked at commit.
    for (sku, rejected) in [("SECOND", true), ("SERIAL", false)] {
        let mut tx = f.store.pool.begin().await.expect("custody touch");
        sqlx::query("UPDATE wms_stock SET on_hand=on_hand WHERE tenant=$1 AND warehouse_id='wh-main' AND sku=$2 AND location='A'")
            .bind(&f.who.tenant).bind(sku).execute(&mut *tx).await.expect("touch stock row");
        let outcome = tx.commit().await;
        assert_eq!(outcome.is_err(), rejected, "{sku}: {outcome:?}");
        if let Err(error) = outcome {
            assert_eq!(
                error.as_database_error().and_then(|e| e.code()).as_deref(),
                Some("23514")
            );
        }
    }
    let pending: i64 =
        sqlx::query_scalar("SELECT count(*) FROM wms_serial_pending_checks WHERE tenant=$1")
            .bind(&f.who.tenant)
            .fetch_one(&f.store.pool)
            .await
            .expect("pending keys");
    assert_eq!(pending, 0);
}
