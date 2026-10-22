//! PostgreSQL archive COPY runs with an empty session search path. Custody
//! CHECK predicates must preserve their original validation in that context.
use super::*;
#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn restore_place_checks_resolve_helpers_with_an_empty_session_search_path() {
    let f = Fixture::new().await;
    let mut tx = f
        .store
        .pool
        .begin()
        .await
        .expect("owned restore predicate transaction");
    sqlx::query("SELECT pg_catalog.set_config('search_path','',true)")
        .execute(&mut *tx)
        .await
        .expect("same empty path used by pg_restore");
    for place in [
        json!({"status":"on_hand","warehouse_id":"main","location":"A"}),
        json!({"status":"missing","warehouse_id":"main","location":"A"}),
        json!({"status":"staged","warehouse_id":"main","order_id":"order","line_id":"line","allocation_location":"A","package_id":null}),
        json!({"status":"shipped","warehouse_id":"main","order_id":"order","line_id":"line","package_id":null,"handoff_event_id":"event","return_receipt_id":null}),
        json!({"status":"in_transit","source_warehouse_id":"main","destination_warehouse_id":"other","transfer_id":"transfer","line_id":"line"}),
        json!({"status":"lost","source_warehouse_id":"main","destination_warehouse_id":"other","transfer_id":"transfer","line_id":"line","loss_id":"loss"}),
    ] {
        let valid: bool = sqlx::query_scalar("SELECT public.wms_serial_place_valid($1)")
            .bind(place.clone())
            .fetch_one(&mut *tx)
            .await
            .expect("restored custody CHECK must resolve its scalar helper without a session path");
        assert!(valid, "{place}");
    }
    for place in [
        json!({"status":"on_hand","warehouse_id":"main","location":"STAGE-01"}),
        json!({"status":"on_hand","warehouse_id":" main","location":"A"}),
        json!({"status":"in_transit","source_warehouse_id":"main","destination_warehouse_id":"main","transfer_id":"transfer","line_id":"line"}),
        json!({"status":"missing","warehouse_id":"main","location":"A","extra":"forged"}),
    ] {
        let valid: bool = sqlx::query_scalar("SELECT public.wms_serial_place_valid($1)")
            .bind(place.clone())
            .fetch_one(&mut *tx)
            .await
            .expect("invalid restored predicate remains evaluable");
        assert!(!valid, "{place}");
    }
}

async fn table_contents(pool: &sqlx::PgPool) -> Value {
    use sqlx::Row;
    let mut tx = pool
        .begin()
        .await
        .expect("owned table evidence transaction");
    sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY")
        .execute(&mut *tx)
        .await
        .expect("consistent table contents");
    sqlx::query("SET LOCAL TIME ZONE 'UTC'")
        .execute(&mut *tx)
        .await
        .expect("normalized timestamps");
    let rows=sqlx::query("SELECT tablename FROM pg_tables WHERE schemaname='public' AND tablename LIKE 'wms_%' AND tablename<>'wms_schema_migrations' ORDER BY tablename").fetch_all(&mut *tx).await.expect("all application tables except the advancing migration ledger");
    let mut result = serde_json::Map::new();
    for row in rows {
        let name: String = row.get("tablename");
        assert!(name.bytes().all(|b| b.is_ascii_lowercase() || b == b'_'));
        let sql=format!("SELECT COALESCE(jsonb_agg(to_jsonb(r) ORDER BY to_jsonb(r)::text),'[]'::jsonb) FROM public.{name} r");
        let data: Value = sqlx::query_scalar(&sql)
            .fetch_one(&mut *tx)
            .await
            .expect("original table rows");
        if name == "wms_serial_pending_checks" {
            assert_eq!(data, json!([]), "transient validation keys survived commit");
        } else {
            result.insert(name, data);
        }
    }
    tx.commit().await.expect("read-only evidence complete");
    Value::Object(result)
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL role permitted to create owned test databases"]
async fn populated_21_upgrade_to_current_preserves_every_data_table_and_original_serial_replay() {
    use sha2::{Digest, Sha256};
    use sqlx::{postgres::PgConnectOptions, Connection};
    use std::path::Path;
    let url = std::env::var("WMS_TEST_DATABASE_URL").expect("explicit isolated test server");
    let mut target = reqwest::Url::parse(&url).expect("test database URI");
    assert!(target.path().contains("test"));
    let name = format!("wms_serial_upgrade_test_{}", domain::id().replace('-', ""));
    assert!(
        name.len() <= 63
            && name
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
    );
    let options: PgConnectOptions = url.parse().expect("test server options");
    let mut admin = sqlx::PgConnection::connect_with(&options.database("postgres"))
        .await
        .expect("owned test database administrator");
    sqlx::query(&format!(
        "CREATE DATABASE {name} TEMPLATE template0 ENCODING 'UTF8'"
    ))
    .execute(&mut admin)
    .await
    .expect("fresh owned upgrade database");
    admin.close().await.expect("administrator closed");
    target.set_path(&format!("/{name}"));
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(6)
        .connect(target.as_str())
        .await
        .expect("owned schema21 database");
    sqlx::query("CREATE TABLE wms_schema_migrations(name TEXT PRIMARY KEY,digest TEXT NOT NULL,applied_at TIMESTAMPTZ NOT NULL DEFAULT now())").execute(&pool).await.expect("original migration ledger");
    let mut migrations: Vec<_> =
        std::fs::read_dir(Path::new(env!("CARGO_MANIFEST_DIR")).join("migrations"))
            .expect("authoritative migration sources")
            .map(|e| e.expect("migration entry").path())
            .filter(|p| {
                p.extension().is_some_and(|e| e == "sql")
                    && p.file_stem()
                        .and_then(|s| s.to_str())
                        .and_then(|s| s.get(..3))
                        .and_then(|s| s.parse::<u32>().ok())
                        .is_some_and(|n| n <= 21)
            })
            .collect();
    migrations.sort();
    assert_eq!(migrations.len(), 21);
    for path in migrations {
        let sql = std::fs::read_to_string(&path).expect("unaltered original migration bytes");
        sqlx::raw_sql(&sql)
            .execute(&pool)
            .await
            .expect("actual schema21 predecessor migration");
        sqlx::query("INSERT INTO wms_schema_migrations(name,digest) VALUES($1,$2)")
            .bind(
                path.file_stem()
                    .and_then(|s| s.to_str())
                    .expect("migration name"),
            )
            .bind(format!("{:x}", Sha256::digest(sql.as_bytes())))
            .execute(&pool)
            .await
            .expect("predecessor original migration digest");
    }
    let mut f = Fixture {
        store: Store { pool },
        who: Identity {
            tenant: format!("serial-upgrade-{}", domain::id()),
            actor: "synthetic:schema-upgrade-operator".into(),
            role: "admin".into(),
        },
    };
    // Current admission shares selected event proofs. Seed the actual v21
    // schema using its unchanged original validator through a temporary naming
    // adapter; remove the adapter before the upgrade boundary. No newer
    // predicate, table, index or migration is installed in the predecessor.
    sqlx::raw_sql("CREATE FUNCTION wms_serial_event_valid_read_source(t TEXT,w TEXT,eid TEXT) RETURNS BOOLEAN LANGUAGE SQL STABLE AS $$ SELECT wms_serial_event_valid(t,w,eid) $$")
        .execute(&f.store.pool).await.expect("predecessor-only source adapter");
    f.store
        .provision(&f.who.tenant, false)
        .await
        .expect("owned original workspace");
    f.catalog().await;
    let (original,saved)=f.execute(json!({"type":"count_stock","sku":"SERIAL","lot":"LOT","location":"A","counted":2,"reason":"Original case-sensitive observations on schema21"}),observed(&[],&["Original","original"])).await;
    sqlx::query("DROP FUNCTION wms_serial_event_valid_read_source(TEXT,TEXT,TEXT)")
        .execute(&f.store.pool)
        .await
        .expect("restore pristine v21 schema before upgrade");
    // The predecessor has no migration25 read component. Prove its real
    // schema21 facts with the retained predecessor-compatible complete query;
    // the current Store/API query is exercised after the actual upgrade.
    let original_proof: Value =
        sqlx::query_scalar(include_str!("../fixtures/serial_reconciliation_v24.sql"))
            .bind(&f.who.tenant)
            .bind("wh-main")
            .fetch_one(&f.store.pool)
            .await
            .expect("complete predecessor serial source and custody proof");
    assert_eq!(original_proof["ok"], true);
    assert_eq!(original_proof["source_ok"], true);
    assert_eq!(original_proof["physical_ok"], true);
    let old_config:Option<Vec<String>>=sqlx::query_scalar("SELECT proconfig FROM pg_proc WHERE oid='public.wms_serial_place_valid(jsonb)'::regprocedure").fetch_one(&f.store.pool).await.expect("original unpinned predicate");
    assert!(old_config.is_none());
    let before = table_contents(&f.store.pool).await;
    for table in [
        "wms_serial_modes",
        "wms_serial_heads",
        "wms_serial_batches",
        "wms_serial_units",
        "wms_serial_steps",
        "wms_serial_observations",
    ] {
        assert!(!before[table]
            .as_array()
            .expect("original nonempty serial evidence")
            .is_empty());
    }
    let ledger: Value = sqlx::query_scalar(
        "SELECT jsonb_agg(to_jsonb(m) ORDER BY name) FROM wms_schema_migrations m",
    )
    .fetch_one(&f.store.pool)
    .await
    .expect("original21 ledger rows");
    f.store.pool.close().await;
    f.store = Store::connect(target.as_str())
        .await
        .expect("actual application upgrade21→33");
    assert_eq!(
        table_contents(&f.store.pool).await,
        before,
        "migrations22–34 changed existing application data"
    );
    let old_rows:Value=sqlx::query_scalar("SELECT jsonb_agg(to_jsonb(m) ORDER BY name) FROM wms_schema_migrations m WHERE name<'022_serial_restore_search_path'").fetch_one(&f.store.pool).await.expect("original21 ledger remains intact");
    assert_eq!(old_rows, ledger);
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM wms_schema_migrations")
        .fetch_one(&f.store.pool)
        .await
        .expect("upgraded ledger count");
    assert_eq!(count, 34);
    f.proof().await;
    f.execute(
        json!({"type":"move_stock","sku":"SERIAL","lot":"LOT","from":"A","to":"B","quantity":1}),
        observed(&[], &["Original"]),
    )
    .await;
    let later = table_contents(&f.store.pool).await;
    let replay = f
        .store
        .execute(&f.who, "wh-main", &original)
        .await
        .expect("original schema21 request after new30 custody counts");
    assert_eq!(replay["replayed"], true);
    assert_eq!(replay["result"], saved["result"]);
    assert_eq!(table_contents(&f.store.pool).await, later);
    let mut tx = f
        .store
        .pool
        .begin()
        .await
        .expect("upgraded restore predicate");
    sqlx::query("SELECT pg_catalog.set_config('search_path','',true)")
        .execute(&mut *tx)
        .await
        .expect("empty restore path");
    let valid: bool = sqlx::query_scalar("SELECT public.wms_serial_place_valid($1)")
        .bind(json!({"status":"on_hand","warehouse_id":"wh-main","location":"B"}))
        .fetch_one(&mut *tx)
        .await
        .expect("upgraded original predicate resolves its helper");
    assert!(valid);
    tx.commit().await.expect("predicate check complete");
    f.proof().await;
    f.store.pool.close().await;
    // Keep the uniquely named source/upgrade database with the owning isolated
    // server's evidence; no pre-existing database or inventory is altered.
    eprintln!("Retained owned schema21→33 upgrade database {name}; all{}data tables and{}original ledger rows unchanged, exact replay preserved",before.as_object().expect("original table evidence").len(),ledger.as_array().expect("original migration evidence").len());
}
