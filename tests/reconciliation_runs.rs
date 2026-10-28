//! Background reconciliation runs through the HTTP service against PostgreSQL:
//! the same proof as the synchronous endpoint, its own statement deadline,
//! abandoned and limited runs, retention, roles and tenant isolation.
use axum::{
    body::Body,
    http::{Request as HttpRequest, StatusCode},
    Router,
};
use http_body_util::BodyExt;
use serde_json::Value;
use stateset_warehouse::{
    api::{router, App},
    domain::id,
    integrations::Connections,
    store::{Identity, Store},
};
use std::{sync::Arc, time::Duration};
use tower::ServiceExt;

struct Fixture {
    store: Store,
    tenant: String,
    app: Router,
}

impl Fixture {
    async fn new() -> Self {
        let url = std::env::var("WMS_TEST_DATABASE_URL").expect("isolated PostgreSQL test URL");
        assert!(reqwest::Url::parse(&url)
            .expect("test URL")
            .path()
            .contains("test"));
        let store = Store::connect(&url).await.expect("current migrations");
        let tenant = format!("runs-{}", id());
        store
            .provision(&tenant, true)
            .await
            .expect("demo warehouse with history");
        let who = |token: &str, tenant: &str, role: &str| {
            (
                token.to_string(),
                Identity {
                    tenant: tenant.into(),
                    actor: format!("runs:{token}"),
                    role: role.into(),
                },
            )
        };
        let app = router(App {
            store: store.clone(),
            credentials: Arc::new(vec![
                who("viewer-runs", &tenant, "viewer"),
                who("oms-runs", &tenant, "oms"),
                who("outsider-runs", &format!("outsider-{}", id()), "admin"),
            ]),
            demo: false,
            connections: Connections::default(),
        });
        Self { store, tenant, app }
    }

    async fn http(&self, method: &str, path: &str, token: &str) -> (StatusCode, Value) {
        let response = self
            .app
            .clone()
            .oneshot(
                HttpRequest::builder()
                    .method(method)
                    .uri(path)
                    .header("Authorization", format!("Bearer {token}"))
                    .body(Body::empty())
                    .expect("HTTP request"),
            )
            .await
            .expect("HTTP response");
        let status = response.status();
        let bytes = response
            .into_body()
            .collect()
            .await
            .expect("HTTP body")
            .to_bytes();
        (status, serde_json::from_slice(&bytes).expect("JSON body"))
    }

    async fn start(&self, token: &str) -> (StatusCode, Value) {
        self.http("POST", "/v1/warehouses/wh-pdx/reconciliation/runs", token)
            .await
    }

    /// Poll a run until it finishes, as a client would.
    async fn finished(&self, run: &str) -> Value {
        for _ in 0..300 {
            let (status, body) = self
                .http(
                    "GET",
                    &format!("/v1/warehouses/wh-pdx/reconciliation/runs/{run}"),
                    "viewer-runs",
                )
                .await;
            assert_eq!(status, StatusCode::OK, "{body}");
            if ["succeeded", "failed"].contains(&body["status"].as_str().unwrap_or_default()) {
                return body;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        panic!("reconciliation run {run} did not finish within 30s");
    }

    async fn insert(&self, status: &str, requested_ago: &str, deadline_in: &str) {
        let (started, finished, error) = match status {
            "queued" => (None, None, None),
            "running" => (Some(requested_ago), None, None),
            _ => (
                Some(requested_ago),
                Some(requested_ago),
                Some("Synthetic failed run"),
            ),
        };
        sqlx::query(
            "INSERT INTO wms_reconciliation_runs(tenant,warehouse_id,id,requested_by,status,requested_at,started_at,finished_at,deadline_at,error)
             VALUES($1,'wh-pdx',$2,'runs:fixture',$3,now()-$4::interval,now()-$5::interval,now()-$6::interval,now()+$7::interval,$8)",
        )
        .bind(&self.tenant)
        .bind(id())
        .bind(status)
        .bind(requested_ago)
        .bind(started)
        .bind(finished)
        .bind(deadline_in)
        .bind(error)
        .execute(&self.store.pool)
        .await
        .expect("synthetic run row");
    }
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn a_run_returns_the_synchronous_proof_for_its_version() {
    let f = Fixture::new().await;
    let (status, run) = f.start("viewer-runs").await;
    assert_eq!(status, StatusCode::ACCEPTED, "{run}");
    assert!(["queued", "running"].contains(&run["status"].as_str().unwrap_or_default()));
    assert_eq!(run["requested_by"], "runs:viewer-runs");
    let done = f.finished(run["id"].as_str().expect("run id")).await;
    assert_eq!(done["status"], "succeeded", "{done}");
    assert_eq!(done["error"], Value::Null);
    let (status, direct) = f
        .http("GET", "/v1/warehouses/wh-pdx/reconciliation", "viewer-runs")
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(direct["ok"], true, "{direct}");
    assert_eq!(done["result"], direct, "a run is the same complete proof");
    assert_eq!(done["version"], direct["version"]);
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn the_proof_honors_the_deadline_it_is_given() {
    let f = Fixture::new().await;
    let error = f
        .store
        .reconciliation_within(&f.tenant, "wh-pdx", Some(Duration::from_millis(1)))
        .await
        .expect_err("a 1ms statement deadline cancels the proof");
    assert_eq!(error.status, 503);
    let report = f
        .store
        .reconciliation_within(&f.tenant, "wh-pdx", Some(Duration::from_secs(120)))
        .await
        .expect("a longer deadline completes the same proof");
    assert_eq!(report["ok"], true);
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn abandoned_runs_read_as_failed_and_active_runs_are_limited() {
    let f = Fixture::new().await;
    // A replica that stopped mid-run leaves it running past its deadline.
    f.insert("running", "20 minutes", "-10 minutes").await;
    let abandoned: String = sqlx::query_scalar(
        "SELECT id FROM wms_reconciliation_runs WHERE tenant=$1 AND status='running'",
    )
    .bind(&f.tenant)
    .fetch_one(&f.store.pool)
    .await
    .expect("abandoned run");
    let (status, body) = f
        .http(
            "GET",
            &format!("/v1/warehouses/wh-pdx/reconciliation/runs/{abandoned}"),
            "viewer-runs",
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["status"], "failed");
    assert_eq!(
        body["error"],
        "Abandoned: the run did not finish before its deadline"
    );
    // Expired runs do not count against the limit; two unexpired ones do.
    f.insert("queued", "1 second", "10 minutes").await;
    f.insert("queued", "1 second", "10 minutes").await;
    let (status, body) = f.start("viewer-runs").await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS, "{body}");
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn starting_a_run_keeps_the_newest_finished_runs() {
    let f = Fixture::new().await;
    for minutes in 1..=25 {
        f.insert("failed", &format!("{minutes} minutes"), "1 hour")
            .await;
    }
    let (status, run) = f.start("viewer-runs").await;
    assert_eq!(status, StatusCode::ACCEPTED, "{run}");
    f.finished(run["id"].as_str().expect("run id")).await;
    let kept: Vec<String> = sqlx::query_scalar(
        "SELECT error FROM wms_reconciliation_runs WHERE tenant=$1 AND error IS NOT NULL ORDER BY requested_at DESC",
    )
    .bind(&f.tenant)
    .fetch_all(&f.store.pool)
    .await
    .expect("retained runs");
    assert_eq!(kept.len(), 20, "the 20 newest finished runs remain");
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn runs_need_a_warehouse_role_and_stay_inside_their_tenant() {
    let f = Fixture::new().await;
    let (status, _) = f.start("oms-runs").await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, run) = f.start("viewer-runs").await;
    assert_eq!(status, StatusCode::ACCEPTED);
    let path = format!(
        "/v1/warehouses/wh-pdx/reconciliation/runs/{}",
        run["id"].as_str().expect("run id")
    );
    let (status, _) = f.http("GET", &path, "outsider-runs").await;
    assert_eq!(status, StatusCode::NOT_FOUND, "another tenant sees nothing");
    let (status, _) = f
        .http(
            "POST",
            "/v1/warehouses/wh-other/reconciliation/runs",
            "viewer-runs",
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let done = f.finished(run["id"].as_str().expect("run id")).await;
    assert_eq!(done["status"], "succeeded", "{done}");
}
