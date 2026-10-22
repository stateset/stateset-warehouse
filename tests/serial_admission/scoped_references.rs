//! Metadata grouping preserves complete proofs, literal populations and fresh snapshots.
use super::*;
use sqlx::PgConnection;

const CURRENT: &str = include_str!("../../src/serial_reconciliation.sql");

fn reference_map(migration: &str, sources: bool) -> String {
    let start = migration
        .find(" WITH reference_classes AS MATERIALIZED (")
        .expect("reference classes");
    let end = start
        + migration[start..]
            .find(" ), expected_stock AS (")
            .expect("unchanged physical equations");
    let (relation, verdict) = if sources {
        ("reference_sources", "u.ok")
    } else {
        (
            "reference_classes",
            "wms_serial_place_references_valid(u.tenant,u.sku,u.lot,u.place)",
        )
    };
    // Parse each actual migration's CTEs; compare every argument, verdict and
    // argument collation against the immutable predecessor's scalar validator.
    format!("{} ) SELECT COALESCE(jsonb_object_agg(jsonb_build_array(u.tenant,u.sku,u.lot,u.place::text)::text,jsonb_build_object('tenant_collation',pg_collation_for(u.tenant),'sku_collation',pg_collation_for(u.sku),'lot_collation',pg_collation_for(u.lot),'ok',{verdict})),'{{}}') FROM {relation} u", migration[start..end].replace("tenant=t", "tenant=$1").replace("=w", "=$2"))
}

async fn compare(
    c: &mut PgConnection,
    tenant: &str,
    warehouse: &str,
    healthy: bool,
) -> Option<bool> {
    let old_refs: Value = sqlx::query_scalar(&reference_map(
        include_str!("../../migrations/027_serial_reference_classes.sql"),
        false,
    ))
    .bind(tenant)
    .bind(warehouse)
    .fetch_one(&mut *c)
    .await
    .expect("original reference flags and collations");
    let new_refs: Value = sqlx::query_scalar(&reference_map(
        include_str!("../../migrations/030_serial_scope_custody_counts.sql"),
        true,
    ))
    .bind(tenant)
    .bind(warehouse)
    .fetch_one(&mut *c)
    .await
    .expect("joined reference flags and collations");
    assert_eq!(
        new_refs, old_refs,
        "reference arguments, verdicts or collations changed"
    );
    let original: Option<bool> =
        sqlx::query_scalar("SELECT wms_serial_scope_current_valid_legacy_oracle($1,$2)")
            .bind(tenant)
            .bind(warehouse)
            .fetch_one(&mut *c)
            .await
            .expect("immutable original scope predicate");
    let grouped: Option<bool> = sqlx::query_scalar("SELECT wms_serial_scope_current_valid($1,$2)")
        .bind(tenant)
        .bind(warehouse)
        .fetch_one(&mut *c)
        .await
        .expect("current exact reference classes");
    assert_eq!(grouped, original, "scope verdict changed at {warehouse}");
    let legacy_query = CURRENT.replace(
        "wms_serial_scope_current_valid(",
        "wms_serial_scope_current_valid_legacy_oracle(",
    );
    let original: Value = sqlx::query_scalar(&legacy_query)
        .bind(tenant)
        .bind(warehouse)
        .fetch_one(&mut *c)
        .await
        .expect("complete proof with immutable scope oracle");
    let current: Value = sqlx::query_scalar(CURRENT)
        .bind(tenant)
        .bind(warehouse)
        .fetch_one(&mut *c)
        .await
        .expect("complete proof with grouped references");
    assert_eq!(current, original, "complete proof changed at {warehouse}");
    if healthy {
        assert_eq!(current["ok"], true, "{current}");
    }
    grouped
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn exact_reference_classes_keep_original_empty_duplicate_and_damaged_source_outcomes() {
    let f = Fixture::new().await;
    let mut tx = f.store.pool.begin().await.expect("original scope oracle");
    // Derive the oracle from the immutable predecessor, not the new function.
    let migration = include_str!("../../migrations/021_serial_tracking.sql");
    let start = migration
        .find("CREATE FUNCTION wms_serial_scope_current_valid(")
        .expect("original scope declaration");
    let end = start + migration[start..].find("$$;").expect("original scope body") + 3;
    let legacy = migration[start..end].replacen(
        "wms_serial_scope_current_valid(",
        "wms_serial_scope_current_valid_legacy_oracle(",
        1,
    );
    sqlx::raw_sql(&legacy)
        .execute(&mut *tx)
        .await
        .expect("rollback-only original oracle definition");
    compare(&mut tx, &f.who.tenant, "wh-main", true).await;
    f.plain(json!({"type":"create_product","sku":"UNTRACKED","name":"Synthetic untracked","category":"Synthetic","reorder_point":0})).await;
    f.plain(json!({"type":"count_stock","sku":"UNTRACKED","lot":"PLAIN","location":"RECV-01","counted":3,"reason":"Original untracked count"})).await;
    compare(&mut tx, &f.who.tenant, "wh-main", true).await;
    f.catalog().await;
    f.execute(json!({"type":"count_stock","sku":"SERIAL","lot":"LOT","location":"A","counted":0,"reason":"Original empty identity set"}),observed(&[],&[])).await;
    compare(&mut tx, &f.who.tenant, "wh-main", true).await;

    let mut labels: Vec<String> = (0..59).map(|n| format!("Unit-{n:03}")).collect();
    labels.extend(["Unit-A", "unit-a", "é", "é", "Scope-missing"].map(str::to_owned));
    let all: Vec<_> = labels.iter().map(String::as_str).collect();
    f.execute(json!({"type":"count_stock","sku":"SERIAL","lot":"LOT","location":"A","counted":64,"reason":"Shared metadata references preserve every original literal"}),observed(&[],&all)).await;
    let present: Vec<_> = all.into_iter().filter(|s| *s != "Scope-missing").collect();
    f.execute(json!({"type":"count_stock","sku":"SERIAL","lot":"LOT","location":"A","counted":63,"reason":"One original unit is missing"}),observed(&[],&present)).await;
    extended::other_warehouse(&f).await;
    extended::at(
        &f,
        "wh-other",
        json!({"type":"create_location","code":"A","zone":"Synthetic","kind":"storage"}),
        vec![],
    )
    .await;
    extended::at(&f,"wh-other",json!({"type":"count_stock","sku":"SERIAL","lot":"LOT","location":"A","counted":1,"reason":"Independent facility identity"}),observed(&[],&["Remote"])).await;
    for warehouse in ["wh-main", "wh-other", "wh-scope-missing"] {
        compare(&mut tx, &f.who.tenant, warehouse, true).await;
    }
    let literals: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM wms_serial_units WHERE tenant=$1 AND serial=ANY($2)",
    )
    .bind(&f.who.tenant)
    .bind(vec!["Unit-A", "unit-a", "é", "é"])
    .fetch_one(&mut *tx)
    .await
    .expect("distinct original case and Unicode literals");
    assert_eq!(literals, 4);
    sqlx::raw_sql("ALTER TABLE wms_products DISABLE TRIGGER wms_serial_product_mode; ALTER TABLE wms_serial_units DISABLE TRIGGER wms_serial_unit_identity; ALTER TABLE wms_serial_units ALTER COLUMN place DROP NOT NULL; ALTER TABLE wms_serial_units DROP CONSTRAINT wms_serial_units_place_check")
        .execute(&mut *tx).await.expect("owned rollback-only malformed-source fixture");
    for (damage, expected) in [
        ("UPDATE wms_products SET data=jsonb_set(data,'{serial_controlled}','false') WHERE tenant=$1 AND warehouse_id='wh-main' AND sku='SERIAL'",false),
        ("UPDATE wms_serial_units SET place=jsonb_set(place,'{location}','null') WHERE tenant=$1 AND serial='Scope-missing'",false),
        ("UPDATE wms_serial_units SET place=jsonb_set(place,'{location}','1') WHERE tenant=$1 AND serial='Scope-missing'",false),
        ("UPDATE wms_serial_units SET place=jsonb_set(place,'{location}','1.0') WHERE tenant=$1 AND serial='Scope-missing'",false),
        ("UPDATE wms_serial_units SET place=place-'location' WHERE tenant=$1 AND serial='Scope-missing'",false),
        ("UPDATE wms_serial_units SET place='null' WHERE tenant=$1 AND serial='Scope-missing'",true),
        ("UPDATE wms_serial_units SET place=NULL WHERE tenant=$1 AND serial='Scope-missing'",true),
        ("UPDATE wms_serial_units SET place='[]' WHERE tenant=$1 AND serial='Scope-missing'",true),
        ("UPDATE wms_serial_units SET lot='OTHER' WHERE tenant=$1 AND serial='Scope-missing'",true),
        ("UPDATE wms_serial_units SET place=jsonb_set(place,'{warehouse_id}','\"wh-scope-missing\"') WHERE tenant=$1 AND serial='Scope-missing'",true),
    ] {
        sqlx::query("SAVEPOINT reference_damage").execute(&mut *tx).await.expect("original fact boundary");
        let changed=sqlx::query(damage).bind(&f.who.tenant).execute(&mut *tx).await.expect("owned metadata damage");
        assert!(changed.rows_affected()>0);
        assert_eq!(compare(&mut tx,&f.who.tenant,"wh-main",false).await,Some(expected));
        for warehouse in ["wh-other","wh-scope-missing"] {compare(&mut tx,&f.who.tenant,warehouse,false).await;}
        sqlx::query("ROLLBACK TO SAVEPOINT reference_damage").execute(&mut *tx).await.expect("restore exact original facts");
        for warehouse in ["wh-main","wh-other","wh-scope-missing"] {compare(&mut tx,&f.who.tenant,warehouse,true).await;}
    }
    tx.rollback()
        .await
        .expect("restore all guard definitions and constraints");
    f.proof().await;
}

async fn mode_verdict(c: &mut PgConnection, tenant: &str, sku: Option<&str>) -> bool {
    let (old, new): (bool, bool) = sqlx::query_as("SELECT NOT EXISTS(SELECT 1 FROM wms_serial_modes m WHERE m.tenant=$1 AND m.controlled AND($2::text IS NULL OR m.sku=$2)AND NOT wms_serial_mode_original(m)),wms_serial_modes_original_for_scope($1,$2)")
        .bind(tenant).bind(sku).fetch_one(&mut *c).await.expect("independent original mode selection");
    assert_eq!(new, old, "mode source changed for {tenant}/{sku:?}");
    old
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn scoped_mode_sources_preserve_original_selection_and_actual_commit_rejections() {
    let f = Fixture::new().await;
    let independent = Fixture::new().await;
    let mut tx = f
        .store
        .pool
        .begin()
        .await
        .expect("rollback-only source comparisons");
    assert!(mode_verdict(&mut tx, &f.who.tenant, None).await);
    f.catalog().await;
    independent.catalog().await;
    f.plain(json!({"type":"create_product","sku":"OTHER","name":"Independent controlled SKU","category":"Synthetic","reorder_point":0,"serial_controlled":true})).await;
    f.plain(json!({"type":"create_product","sku":"UNTRACKED","name":"Untracked SKU","category":"Synthetic","reorder_point":0})).await;
    extended::other_warehouse(&f).await;
    for sku in ["SERIAL", "OTHER"] {
        let input = Input {
            target: vec![],
            sku: sku.into(),
            serials: vec!["One".into()],
        };
        f.execute(json!({"type":"count_stock","sku":sku,"lot":"LOT","location":"A","counted":1,"reason":"Original controlled source"}),vec![input]).await;
    }
    let event = "(SELECT source_event_id FROM wms_serial_modes WHERE tenant=$1 AND sku='SERIAL')";
    let receipt =
        "tenant=$1 AND request_id=(SELECT payload->>'request_id' FROM wms_events WHERE id=<EVENT>)";
    for damage in [
        "UPDATE wms_events SET payload=jsonb_set(payload,'{actor}','\"forged\"') WHERE id=<EVENT>",
        "UPDATE wms_events SET payload=jsonb_set(payload,'{command,sku}','\"OTHER\"') WHERE id=<EVENT>",
        "UPDATE wms_events SET payload='null' WHERE id=<EVENT>",
        "UPDATE wms_events SET payload='[]' WHERE id=<EVENT>",
        "UPDATE wms_commands SET actor='forged' WHERE <RECEIPT>",
        "UPDATE wms_commands SET result=jsonb_set(result,'{type}','\"forged\"') WHERE <RECEIPT>",
        "DELETE FROM wms_commands WHERE <RECEIPT>",
        "UPDATE wms_serial_modes SET source_event_id=(SELECT source_event_id FROM wms_serial_modes WHERE tenant=$1 AND sku='OTHER') WHERE tenant=$1 AND sku='SERIAL'",
        "UPDATE wms_serial_modes SET source_warehouse_id='wh-other' WHERE tenant=$1 AND sku='SERIAL'",
        "UPDATE wms_serial_modes SET controlled=false,source_warehouse_id=NULL,source_event_id=NULL WHERE tenant=$1 AND sku='SERIAL'",
        "UPDATE wms_products SET data=jsonb_set(data,'{serial_controlled}','false') WHERE tenant=$1 AND sku='SERIAL' AND warehouse_id='wh-main'",
        "UPDATE wms_products SET data=jsonb_set(data,'{serial_controlled}','null') WHERE tenant=$1 AND sku='SERIAL' AND warehouse_id='wh-main'",
        "UPDATE wms_products SET data=jsonb_set(data,'{serial_controlled}','\"true\"') WHERE tenant=$1 AND sku='SERIAL' AND warehouse_id='wh-main'",
        "UPDATE wms_products SET data=data-'serial_controlled' WHERE tenant=$1 AND sku='SERIAL' AND warehouse_id='wh-main'",
        "UPDATE wms_products SET data=jsonb_set(data,'{serial_controlled}','true') WHERE tenant=$1 AND sku='UNTRACKED'",
    ] {
        sqlx::query("SAVEPOINT mode_damage").execute(&mut *tx).await.expect("original mode boundary");
        // Only this owned transaction bypasses triggers while constructing a
        // malformed source. Restore origin BEFORE testing the real commit guard.
        sqlx::query("SET LOCAL session_replication_role=replica").execute(&mut *tx).await.expect("forensic mutation only");
        let mutation = damage.replace("<RECEIPT>", receipt).replace("<EVENT>", event);
        assert!(sqlx::query(&mutation).bind(&f.who.tenant).execute(&mut *tx).await.expect("owned malformed source").rows_affected() > 0);
        sqlx::query("SET LOCAL session_replication_role=origin").execute(&mut *tx).await.expect("actual guards restored");
        for tenant in [&f.who.tenant, &independent.who.tenant] {
            for sku in [None, Some("SERIAL"), Some("OTHER"), Some("UNTRACKED"), Some("ABSENT")] {
                mode_verdict(&mut tx, tenant, sku).await;
            }
        }
        // Migration 035 checks custody for the SKU a commit touches; damage
        // confined to another SKU no longer rejects it.
        for sku in ["SERIAL", "OTHER"] {
            let expected: bool = sqlx::query_scalar("SELECT NOT EXISTS(SELECT 1 FROM wms_serial_modes m WHERE tenant=$1 AND sku=$2 AND controlled) OR (NOT EXISTS(SELECT 1 FROM wms_serial_modes m WHERE tenant=$1 AND sku=$2 AND controlled AND NOT wms_serial_mode_original(m)) AND wms_serial_scope_current_valid_sku($1,'wh-main',$2) AND wms_serial_scope_transit_valid_sku($1,'wh-main',$2))")
                .bind(&f.who.tenant).bind(sku).fetch_one(&mut *tx).await.expect("original commit expression");
            sqlx::query("SAVEPOINT actual_guard").execute(&mut *tx).await.expect("real write boundary");
            let changed=sqlx::query("UPDATE wms_stock SET on_hand=on_hand WHERE tenant=$1 AND warehouse_id='wh-main' AND sku=$2")
                .bind(&f.who.tenant).bind(sku).execute(&mut *tx).await.expect("real deferred stock guard");
            assert!(changed.rows_affected()>0);
            let result=sqlx::query("SET CONSTRAINTS ALL IMMEDIATE").execute(&mut *tx).await;
            if expected { result.expect("original caller permits selected SKU"); }
            else { assert_eq!(result.expect_err("original source rejection must remain").as_database_error().and_then(|e|e.code()).as_deref(),Some("23514")); }
            sqlx::query("ROLLBACK TO SAVEPOINT actual_guard").execute(&mut *tx).await.expect("discard actual guard write");
        }
        sqlx::query("ROLLBACK TO SAVEPOINT mode_damage").execute(&mut *tx).await.expect("restore every original source");
        for sku in [None, Some("SERIAL"), Some("OTHER"), Some("UNTRACKED"), Some("ABSENT")] {
            assert!(mode_verdict(&mut tx, &f.who.tenant, sku).await);
        }
    }
    tx.rollback().await.expect("remove all forensic changes");
    f.proof().await;
    independent.proof().await;
}
