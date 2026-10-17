use axum::{
    body::Body,
    http::{Request, StatusCode},
    Router,
};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use stateset_warehouse::{
    api::{self, App},
    domain,
    integrations::Connections,
    store::{Identity, Store},
};
use std::{collections::BTreeMap, sync::Arc};
use tower::ServiceExt;

const TOKEN: &str = "synthetic-operations-key";
const SECRET: &str = "SENSITIVE-FIXTURE-DO-NOT-EXPOSE";

fn app(store: Store, tenant: &str, role: &str) -> Router {
    app_connections(store, tenant, role, Connections::default())
}
fn app_connections(store: Store, tenant: &str, role: &str, connections: Connections) -> Router {
    api::router(App {
        store,
        credentials: Arc::new(vec![(
            TOKEN.into(),
            Identity {
                tenant: tenant.into(),
                actor: SECRET.into(),
                role: role.into(),
            },
        )]),
        demo: false,
        connections,
    })
}
fn lazy_store() -> Store {
    Store {
        pool: sqlx::postgres::PgPoolOptions::new()
            .connect_lazy("postgres://unused@127.0.0.1:1/unused")
            .expect("lazy database URI"),
    }
}
async fn call(app: Router, uri: &str, token: Option<&str>) -> (StatusCode, String, String) {
    let mut builder = Request::builder().uri(uri);
    if let Some(token) = token {
        builder = builder.header("authorization", format!("Bearer {token}"));
    }
    let response = app
        .oneshot(builder.body(Body::empty()).expect("request"))
        .await
        .expect("response");
    let status = response.status();
    let content_type = response
        .headers()
        .get("content-type")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_string();
    if status.is_success() {
        assert_eq!(response.headers()["cache-control"], "no-store");
    }
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("body")
        .to_bytes();
    (
        status,
        String::from_utf8(bytes.to_vec()).expect("UTF8 body"),
        content_type,
    )
}

#[tokio::test]
async fn operational_routes_require_authentication_before_database_access() {
    for path in ["/v1/operations/health", "/v1/operations/metrics"] {
        for token in [None, Some("wrong-synthetic-key")] {
            assert_eq!(
                call(app(lazy_store(), "guard-tenant", "admin"), path, token)
                    .await
                    .0,
                StatusCode::UNAUTHORIZED,
                "{path}"
            );
        }
    }
}

#[tokio::test]
async fn operations_roles_reject_viewers_integrations_and_operator_metrics_without_database() {
    for role in ["viewer", "oms", "tms", "invented"] {
        for path in ["/v1/operations/health", "/v1/operations/metrics"] {
            assert_eq!(
                call(app(lazy_store(), "guard-tenant", role), path, Some(TOKEN))
                    .await
                    .0,
                StatusCode::FORBIDDEN,
                "{role}: {path}"
            );
        }
    }
    assert_eq!(
        call(
            app(lazy_store(), "guard-tenant", "operator"),
            "/v1/operations/metrics",
            Some(TOKEN)
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
}

#[tokio::test]
async fn operations_reject_invalid_or_scope_override_queries_without_database() {
    for (path, expected) in [
        (
            "/v1/operations/health?tenant=other",
            StatusCode::BAD_REQUEST,
        ),
        (
            "/v1/operations/health?warehouse_id=other",
            StatusCode::BAD_REQUEST,
        ),
        ("/v1/operations/health?limit=wat", StatusCode::BAD_REQUEST),
        (
            "/v1/operations/health?limit=1&limit=2",
            StatusCode::BAD_REQUEST,
        ),
        (
            "/v1/operations/health?limit=0",
            StatusCode::UNPROCESSABLE_ENTITY,
        ),
        (
            "/v1/operations/health?limit=101",
            StatusCode::UNPROCESSABLE_ENTITY,
        ),
        (
            "/v1/operations/health?limit=-1",
            StatusCode::UNPROCESSABLE_ENTITY,
        ),
        (
            "/v1/operations/health?after=",
            StatusCode::UNPROCESSABLE_ENTITY,
        ),
        (
            "/v1/operations/health?after=..%2Fother",
            StatusCode::UNPROCESSABLE_ENTITY,
        ),
        (
            "/v1/operations/metrics?tenant=other",
            StatusCode::BAD_REQUEST,
        ),
        ("/v1/operations/metrics?limit=1", StatusCode::BAD_REQUEST),
    ] {
        assert_eq!(
            call(
                app(lazy_store(), "guard-tenant", "admin"),
                path,
                Some(TOKEN)
            )
            .await
            .0,
            expected,
            "{path}"
        );
    }
}

async fn postgres() -> (Store, String) {
    let url = std::env::var("WMS_TEST_DATABASE_URL").expect("isolated WMS_TEST_DATABASE_URL");
    assert!(
        reqwest::Url::parse(&url)
            .expect("database URL")
            .path()
            .contains("test"),
        "only an isolated test database is allowed"
    );
    let store = Store::connect(&url).await.expect("migrated database");
    let tenant = format!("operations-{}", domain::id());
    store
        .provision(&tenant, false)
        .await
        .expect("tenant warehouse");
    (store, tenant)
}
async fn read(store: &Store, tenant: &str, role: &str, suffix: &str) -> Value {
    let (status, body, _) = call(
        app(store.clone(), tenant, role),
        &format!("/v1/operations/health{suffix}"),
        Some(TOKEN),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    serde_json::from_str(&body).expect("snapshot JSON")
}
fn metric_samples(body: &str) -> BTreeMap<String, i64> {
    body.lines()
        .filter(|line| !line.starts_with('#') && !line.is_empty())
        .map(|line| {
            let (key, value) = line.rsplit_once(' ').expect("Prometheus sample");
            (key.to_string(), value.parse().expect("integer gauge"))
        })
        .collect()
}
async fn event(store: &Store, tenant: &str, version: i64, age: i64) -> String {
    let id = domain::id();
    sqlx::query("INSERT INTO wms_events(id,tenant,warehouse_id,version,payload,created_at) VALUES($1,$2,'wh-main',$3,$4,now()-($5::bigint*interval '1 second'))")
        .bind(&id).bind(tenant).bind(version).bind(json!({"type":"inventory.counted","private":SECRET})).bind(age)
        .execute(&store.pool).await.expect("synthetic event");
    id
}
async fn seed(store: &Store, tenant: &str) {
    for (version, destination, status, age, due, lease) in [
        (1, "oms", "pending", 3600, -100_i64, None),
        (2, "oms", "pending", 2000, 600, None),
        (3, "oms", "delivering", 3000, -200, Some(600_i64)),
        (4, "tms", "delivering", 4000, -300, Some(-150)),
        (5, "tms", "dead_letter", 5000, -500, None),
        (6, "oms", "delivered", 6000, -1000, None),
    ] {
        let event_id = event(store, tenant, version, age).await;
        sqlx::query("INSERT INTO wms_outbox(id,event_id,tenant,destination,status,due_at,lease_token,lease_until,last_error) VALUES($1,$2,$3,$4,$5,now()+($6::bigint*interval '1 second'),$7,CASE WHEN $8::bigint IS NULL THEN NULL ELSE now()+($8*interval '1 second') END,$9)")
            .bind(domain::id()).bind(event_id).bind(tenant).bind(destination).bind(status).bind(due)
            .bind(lease.map(|_| SECRET)).bind(lease).bind(SECRET).execute(&store.pool).await.expect("synthetic outbox");
    }
    for (offset, kind, status, lease) in [
        (0, "oms_import", "frozen", None),
        (1, "tms_transport", "sending", Some(600_i64)),
        (2, "tms_transport", "sending", Some(-150)),
        (3, "tms_transport", "uncertain", None),
        (4, "oms_import", "completed", None),
        (5, "oms_import", "local_conflict", None),
        // Rejection is a database-wide terminal status. Aggregation must retain
        // it even if a later adapter uses it for another native operation.
        (6, "tms_transport", "rejected", None),
    ] {
        let created_age = 7200 + offset * 10;
        sqlx::query("INSERT INTO wms_native_requests(tenant,warehouse_id,request_id,kind,actor,digest,input,expected_version,order_id,source_snapshot,frozen,status,lease_token,lease_until,response,last_error,created_at,updated_at) VALUES($1,'wh-main',$2,$3,$4,$5,$6,0,$7,$6,$6,$8,$9,CASE WHEN $10::bigint IS NULL THEN NULL ELSE now()+($10*interval '1 second') END,CASE WHEN $8 IN('completed','rejected') THEN $6 ELSE NULL END,$4,now()-($11::bigint*interval '1 second'),now()-interval '120 seconds')")
            .bind(tenant).bind(format!("request-{}",domain::id())).bind(kind).bind(SECRET).bind("0".repeat(64))
            .bind(json!({"private":SECRET})).bind(if kind=="tms_transport" {Some(domain::id())} else {None})
            .bind(status).bind(lease.map(|_| SECRET)).bind(lease).bind(created_age)
            .execute(&store.pool).await.expect("synthetic native receipt");
    }
    for (warehouse, version) in [("wh-a", 11_i64), ("wh-z", 22)] {
        sqlx::query(
            "INSERT INTO wms_warehouses(tenant,warehouse_id,version,metadata) VALUES($1,$2,$3,$4)",
        )
        .bind(tenant)
        .bind(warehouse)
        .bind(version)
        .bind(json!({"id":warehouse,"private":SECRET}))
        .execute(&store.pool)
        .await
        .expect("warehouse metadata");
    }
    for (warehouse, status) in [
        ("wh-a", "open"),
        ("wh-a", "open"),
        ("wh-a", "resolved"),
        ("wh-main", "open"),
    ] {
        sqlx::query(
            "INSERT INTO wms_exceptions(tenant,warehouse_id,id,status,data) VALUES($1,$2,$3,$4,$5)",
        )
        .bind(tenant)
        .bind(warehouse)
        .bind(domain::id())
        .bind(status)
        .bind(json!({"private":SECRET}))
        .execute(&store.pool)
        .await
        .expect("synthetic exception");
    }
}

#[tokio::test]
#[ignore = "requires isolated WMS_TEST_DATABASE_URL"]
async fn empty_operational_snapshot_is_zero_complete_and_prometheus_has_fixed_cardinality() {
    let (store, tenant) = postgres().await;
    let snapshot = read(&store, &tenant, "operator", "").await;
    assert_eq!(snapshot["schema_version"], 1);
    chrono::DateTime::parse_from_rfc3339(snapshot["snapshot_at"].as_str().expect("time"))
        .expect("RFC3339");
    assert_eq!(snapshot["outbox"]["total"], 0);
    assert_eq!(
        snapshot["outbox"]["receivers"],
        json!([
            {"destination":"oms","configured":false},
            {"destination":"tms","configured":false}
        ])
    );
    assert_eq!(
        snapshot["outbox"]["groups"]
            .as_array()
            .expect("groups")
            .len(),
        8
    );
    assert_eq!(snapshot["native"]["total"], 0);
    assert_eq!(
        snapshot["native"]["groups"]
            .as_array()
            .expect("groups")
            .len(),
        18
    );
    assert_eq!(snapshot["warehouses"]["total"], 1);
    assert_eq!(snapshot["warehouses"]["open_exceptions"], 0);
    assert_eq!(
        snapshot["warehouses"]["items"],
        json!([{"warehouse_id":"wh-main","version":0,"open_exceptions":0}])
    );
    assert_eq!(snapshot["warehouses"]["next_after"], Value::Null);
    let (status, body, content_type) = call(
        app(store, &tenant, "admin"),
        "/v1/operations/metrics",
        Some(TOKEN),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(content_type, "text/plain; version=0.0.4; charset=utf-8");
    let samples = metric_samples(&body);
    assert_eq!(samples.len(), 112);
    assert_eq!(samples["wms_warehouse_count"], 1);
    for (key, value) in &samples {
        assert!(
            *value == 0 || key == "wms_warehouse_count",
            "{key}: {value}"
        );
    }
    assert!(!body.contains(&tenant));
    assert!(!body.contains(SECRET));
    assert!(!body.contains("wh-main"));
}

#[tokio::test]
#[ignore = "requires isolated WMS_TEST_DATABASE_URL"]
async fn operational_counts_ages_pagination_and_metrics_are_exact_and_tenant_scoped() {
    let (store, tenant) = postgres().await;
    seed(&store, &tenant).await;
    let other_tenant = format!("operations-other-{}", domain::id());
    store
        .provision(&other_tenant, false)
        .await
        .expect("other warehouse");
    seed(&store, &other_tenant).await;
    let before: Value = sqlx::query_scalar("SELECT jsonb_build_object('outbox',(SELECT jsonb_agg(to_jsonb(o) ORDER BY id) FROM wms_outbox o WHERE tenant=$1),'native',(SELECT jsonb_agg(to_jsonb(n) ORDER BY request_id) FROM wms_native_requests n WHERE tenant=$1),'warehouses',(SELECT jsonb_agg(to_jsonb(w) ORDER BY warehouse_id) FROM wms_warehouses w WHERE tenant=$1))")
        .bind(&tenant).fetch_one(&store.pool).await.expect("before evidence");
    let first = read(&store, &tenant, "operator", "?limit=1").await;
    let outbox = &first["outbox"];
    for (key, count) in [
        ("total", 6),
        ("pending", 2),
        ("delivering", 2),
        ("delivered", 1),
        ("dead_letter", 1),
        ("due", 2),
        ("active_leases", 1),
        ("expired_leases", 1),
    ] {
        assert_eq!(outbox[key], count, "{key}");
    }
    assert!((4000..=4010).contains(&outbox["oldest_pending_age_seconds"].as_i64().expect("age")));
    let native = &first["native"];
    for (key, count) in [
        ("total", 7),
        ("requires_review", 3),
        ("uncertain", 1),
        ("rejected", 1),
        ("local_conflict", 1),
    ] {
        assert_eq!(native[key], count, "{key}");
    }
    assert_eq!(first["warehouses"]["total"], 3);
    assert_eq!(first["warehouses"]["open_exceptions"], 3);
    assert_eq!(
        first["warehouses"]["items"],
        json!([{"warehouse_id":"wh-a","version":11,"open_exceptions":2}])
    );
    assert_eq!(first["warehouses"]["next_after"], "wh-a");
    let second = read(&store, &tenant, "admin", "?after=wh-a&limit=1").await;
    assert_eq!(
        second["warehouses"]["items"],
        json!([{"warehouse_id":"wh-main","version":0,"open_exceptions":1}])
    );
    assert_eq!(second["warehouses"]["next_after"], "wh-main");
    let third = read(&store, &tenant, "admin", "?after=wh-main&limit=1").await;
    assert_eq!(
        third["warehouses"]["items"],
        json!([{"warehouse_id":"wh-z","version":22,"open_exceptions":0}])
    );
    assert!(third["warehouses"]["next_after"].is_null());
    let last = read(&store, &tenant, "admin", "?after=wh-z").await;
    assert_eq!(last["warehouses"]["items"], json!([]));
    for page in [&second, &third, &last] {
        assert_eq!(
            page["outbox"]["total"], 6,
            "warehouse pagination must not filter tenant aggregates"
        );
        assert_eq!(page["native"]["total"], 7);
        assert_eq!(page["warehouses"]["open_exceptions"], 3);
    }
    assert!(!first.to_string().contains(SECRET));
    assert!(!first.to_string().contains(&other_tenant));
    let (_, body, _) = call(
        app(store.clone(), &tenant, "admin"),
        "/v1/operations/metrics",
        Some(TOKEN),
    )
    .await;
    let samples = metric_samples(&body);
    assert_eq!(samples.len(), 112);
    assert_eq!(
        samples["wms_outbox_receiver_configured{destination=\"oms\"}"],
        0
    );
    assert_eq!(
        samples["wms_outbox_receiver_configured{destination=\"tms\"}"],
        0
    );
    assert_eq!(
        samples["wms_outbox_items{destination=\"oms\",status=\"pending\"}"],
        2
    );
    assert_eq!(samples["wms_outbox_due_items{destination=\"oms\"}"], 1);
    assert_eq!(samples["wms_outbox_due_items{destination=\"tms\"}"], 1);
    assert_eq!(samples["wms_outbox_active_leases{destination=\"oms\"}"], 1);
    assert_eq!(samples["wms_outbox_expired_leases{destination=\"tms\"}"], 1);
    assert_eq!(
        samples["wms_native_requests{kind=\"tms_transport\",status=\"sending\"}"],
        2
    );
    assert_eq!(
        samples["wms_native_active_leases{kind=\"tms_transport\",status=\"sending\"}"],
        1
    );
    assert_eq!(
        samples["wms_native_expired_leases{kind=\"tms_transport\",status=\"sending\"}"],
        1
    );
    assert_eq!(samples["wms_open_exceptions"], 3);
    assert_eq!(samples["wms_warehouse_count"], 3);
    for (key, range) in [
        (
            "wms_outbox_oldest_due_age_seconds{destination=\"oms\"}",
            100..=110,
        ),
        (
            "wms_outbox_oldest_due_age_seconds{destination=\"tms\"}",
            150..=160,
        ),
        (
            "wms_native_oldest_updated_age_seconds{kind=\"tms_transport\",status=\"uncertain\"}",
            120..=130,
        ),
    ] {
        assert!(range.contains(&samples[key]), "{key}: {}", samples[key]);
    }
    for secret in [
        SECRET,
        tenant.as_str(),
        other_tenant.as_str(),
        "wh-main",
        "request-",
    ] {
        assert!(!body.contains(secret), "redacted {secret}");
    }
    let connections = Connections(Arc::new(BTreeMap::from([
        (
            (tenant.clone(), "oms".into()),
            (format!("https://{SECRET}.example/events"), SECRET.into()),
        ),
        (
            (other_tenant.clone(), "tms".into()),
            (format!("https://{SECRET}.example/events"), SECRET.into()),
        ),
    ])));
    let configured_app = app_connections(store.clone(), &tenant, "admin", connections);
    let (status, configured_body, _) =
        call(configured_app.clone(), "/v1/operations/health", Some(TOKEN)).await;
    assert_eq!(status, StatusCode::OK);
    let configured: Value = serde_json::from_str(&configured_body).expect("configured snapshot");
    assert_eq!(
        configured["outbox"]["receivers"],
        json!([
            {"destination":"oms","configured":true},
            {"destination":"tms","configured":false}
        ])
    );
    assert!(!configured_body.contains(SECRET));
    let (status, configured_body, _) =
        call(configured_app, "/v1/operations/metrics", Some(TOKEN)).await;
    assert_eq!(status, StatusCode::OK);
    let configured = metric_samples(&configured_body);
    assert_eq!(
        configured["wms_outbox_receiver_configured{destination=\"oms\"}"],
        1
    );
    assert_eq!(
        configured["wms_outbox_receiver_configured{destination=\"tms\"}"],
        0
    );
    assert!(!configured_body.contains(SECRET));
    let after: Value = sqlx::query_scalar("SELECT jsonb_build_object('outbox',(SELECT jsonb_agg(to_jsonb(o) ORDER BY id) FROM wms_outbox o WHERE tenant=$1),'native',(SELECT jsonb_agg(to_jsonb(n) ORDER BY request_id) FROM wms_native_requests n WHERE tenant=$1),'warehouses',(SELECT jsonb_agg(to_jsonb(w) ORDER BY warehouse_id) FROM wms_warehouses w WHERE tenant=$1))")
        .bind(&tenant).fetch_one(&store.pool).await.expect("after evidence");
    assert_eq!(
        before, after,
        "operational reads do not mutate business or delivery evidence"
    );
}

#[tokio::test]
#[ignore = "requires isolated WMS_TEST_DATABASE_URL"]
async fn operational_snapshot_keeps_one_database_view_across_concurrent_commits() {
    let (store, tenant) = postgres().await;
    let first_event = event(&store, &tenant, 1, 60).await;
    sqlx::query("INSERT INTO wms_outbox(id,event_id,tenant,destination) VALUES($1,$2,$3,'oms')")
        .bind(domain::id())
        .bind(first_event)
        .bind(&tenant)
        .execute(&store.pool)
        .await
        .expect("initial event delivery");

    // Block the second aggregate after the outbox aggregate has acquired its
    // MVCC snapshot. Observe actual PostgreSQL lock waiters, rather than relying
    // on a sleep or a product-only test hook.
    let mut blocker = store.pool.begin().await.expect("table lock transaction");
    sqlx::query("LOCK TABLE wms_native_requests IN ACCESS EXCLUSIVE MODE")
        .execute(&mut *blocker)
        .await
        .expect("temporary native aggregate barrier");
    let blocker_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *blocker)
        .await
        .expect("barrier PID");
    let request_app = app(store.clone(), &tenant, "admin");
    let request =
        tokio::spawn(async move { call(request_app, "/v1/operations/health", Some(TOKEN)).await });
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            let waiting: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE datname=current_database() AND query LIKE 'SELECT kind,status,count(*) AS count%' AND $1=ANY(pg_blocking_pids(pid)))")
                .bind(blocker_pid).fetch_one(&store.pool).await.expect("observe native aggregate barrier");
            if waiting {
                break;
            }
            tokio::task::yield_now().await;
        }
    }).await.expect("snapshot reached deterministic query barrier");
    let mut update = store
        .pool
        .begin()
        .await
        .expect("concurrent committed update");
    let second_event = domain::id();
    sqlx::query("INSERT INTO wms_events(id,tenant,warehouse_id,version,payload) VALUES($1,$2,'wh-main',2,'{}')")
        .bind(&second_event).bind(&tenant).execute(&mut *update).await.expect("new event");
    sqlx::query("INSERT INTO wms_outbox(id,event_id,tenant,destination) VALUES($1,$2,$3,'oms')")
        .bind(domain::id())
        .bind(&second_event)
        .bind(&tenant)
        .execute(&mut *update)
        .await
        .expect("new event delivery");
    sqlx::query("UPDATE wms_warehouses SET version=1 WHERE tenant=$1 AND warehouse_id='wh-main'")
        .bind(&tenant)
        .execute(&mut *update)
        .await
        .expect("new warehouse version");
    sqlx::query("INSERT INTO wms_exceptions(tenant,warehouse_id,id,status,data) VALUES($1,'wh-main',$2,'open','{}')")
        .bind(&tenant).bind(domain::id()).execute(&mut *update).await.expect("new exception");
    update.commit().await.expect("commit concurrent changes");
    blocker.commit().await.expect("release query barrier");
    let (status, body, _) = request.await.expect("snapshot request task");
    assert_eq!(status, StatusCode::OK, "{body}");
    let original: Value = serde_json::from_str(&body).expect("original snapshot");
    assert_eq!(original["outbox"]["total"], 1);
    assert_eq!(original["warehouses"]["open_exceptions"], 0);
    assert_eq!(original["warehouses"]["items"][0]["version"], 0);
    let current = read(&store, &tenant, "admin", "").await;
    assert_eq!(current["outbox"]["total"], 2);
    assert_eq!(current["warehouses"]["open_exceptions"], 1);
    assert_eq!(current["warehouses"]["items"][0]["version"], 1);
}

#[tokio::test]
#[ignore = "requires isolated WMS_TEST_DATABASE_URL"]
async fn blocked_operational_read_fails_with_bounded_unavailability_instead_of_zero_counts() {
    let (store, tenant) = postgres().await;
    let mut blocker = store.pool.begin().await.expect("blocking transaction");
    sqlx::query("LOCK TABLE wms_native_requests IN ACCESS EXCLUSIVE MODE")
        .execute(&mut *blocker)
        .await
        .expect("held native table");
    let started = std::time::Instant::now();
    let (status, body, _) = call(
        app(store.clone(), &tenant, "admin"),
        "/v1/operations/health",
        Some(TOKEN),
    )
    .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
    assert!(
        started.elapsed() < std::time::Duration::from_secs(7),
        "aggregate read has a five-second deadline"
    );
    let error: Value = serde_json::from_str(&body).expect("JSON unavailability");
    assert!(error["error"].is_string());
    assert!(
        error.get("outbox").is_none(),
        "failure is never a false empty queue"
    );
    assert!(
        !body.contains("wms_native_requests"),
        "SQL details are private"
    );
    assert!(!body.contains(SECRET));
    blocker.commit().await.expect("release native table");
    let recovered = read(&store, &tenant, "admin", "").await;
    assert_eq!(recovered["native"]["total"], 0);
}
