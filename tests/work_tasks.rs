//! Owned putaway work through real HTTP and PostgreSQL; all data synthetic.
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
use std::sync::Arc;
use tower::ServiceExt;

const W: &str = "wh-main";

struct Fixture {
    app: Router,
    store: Store,
    tenant: String,
}

impl Fixture {
    async fn new() -> Self {
        let url = std::env::var("WMS_TEST_DATABASE_URL").expect("isolated PostgreSQL test URL");
        assert!(url.contains("test"));
        let store = Store::connect(&url).await.expect("current migrations");
        let tenant = format!("planning-{}", domain::id());
        store.provision(&tenant, false).await.expect("tenant");
        let credentials = ["admin", "operator", "other", "viewer", "foreign"]
            .map(|role| {
                (
                    format!("synthetic-{role}-key-0000000000000000"),
                    Identity {
                        tenant: if role == "foreign" {
                            format!("foreign-{tenant}")
                        } else {
                            tenant.clone()
                        },
                        actor: format!("synthetic:{role}"),
                        role: if role == "other" || role == "foreign" {
                            "operator".into()
                        } else {
                            role.into()
                        },
                    },
                )
            })
            .to_vec();
        let app = api::router(App {
            store: store.clone(),
            credentials: Arc::new(credentials),
            demo: false,
            connections: Connections::default(),
        });
        Self { app, store, tenant }
    }

    async fn call(
        &self,
        role: &str,
        method: &str,
        uri: &str,
        body: Option<Value>,
    ) -> (StatusCode, Value) {
        let request = Request::builder()
            .method(method)
            .uri(uri)
            .header(
                "authorization",
                format!("Bearer synthetic-{role}-key-0000000000000000"),
            )
            .header("content-type", "application/json")
            .body(body.map_or_else(Body::empty, |b| Body::from(b.to_string())))
            .expect("request");
        let response = self.app.clone().oneshot(request).await.expect("response");
        let status = response.status();
        let bytes = response
            .into_body()
            .collect()
            .await
            .expect("body")
            .to_bytes();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        )
    }

    async fn version(&self) -> i64 {
        self.store
            .state(&self.tenant, W)
            .await
            .expect("state")
            .version
    }

    /// Execute a canonical command, with the plan's reviewed resource versions
    /// when given, otherwise the current warehouse version.
    async fn command(&self, command: Value, resource_versions: Option<Value>) -> Value {
        let mut request = json!({
            "request_id": domain::id(),
            "expected_version": self.version().await,
            "command": command,
        });
        if let Some(versions) = resource_versions.filter(|v| !v.is_null()) {
            request["resource_versions"] = versions;
        }
        let (status, body) = self
            .call(
                "admin",
                "POST",
                &format!("/v1/warehouses/{W}/commands"),
                Some(request),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        body
    }

    async fn setup(&self) {
        for sku in ["SKU-A", "SKU-B"] {
            self.command(json!({"type":"create_product","sku":sku,"name":format!("Synthetic {sku}"),"category":"Synthetic","reorder_point":0}), None).await;
        }
        for (code, zone) in [("S-1", "Z1"), ("S-2", "Z1"), ("S-10", "Z1"), ("S-20", "Z2")] {
            self.command(
                json!({"type":"create_location","code":code,"zone":zone,"kind":"storage"}),
                None,
            )
            .await;
        }
        self.command(json!({"type":"count_stock","sku":"SKU-A","lot":"L1","location":"S-2","counted":4,"reason":"Synthetic opening"}), None).await;
        self.command(json!({"type":"count_stock","sku":"SKU-A","lot":"L1","location":"RECV-01","counted":5,"reason":"Synthetic arrival"}), None).await;
        self.command(json!({"type":"count_stock","sku":"SKU-B","lot":"L9","location":"RECV-01","counted":3,"reason":"Synthetic arrival"}), None).await;
    }

    async fn proof(&self) {
        let (status, report) = self
            .call(
                "admin",
                "GET",
                &format!("/v1/warehouses/{W}/reconciliation"),
                None,
            )
            .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(report["ok"], true, "{report}");
    }
}

impl Fixture {
    async fn work(&self, actor: &str, suffix: &str, body: Option<Value>) -> (StatusCode, Value) {
        self.call(
            actor,
            if body.is_some() { "POST" } else { "GET" },
            &format!("/v1/warehouses/{W}/work{suffix}"),
            body,
        )
        .await
    }
    async fn tasks(&self) -> Vec<Value> {
        let (s, q) = self.work("admin", "?view=all", None).await;
        assert_eq!(s, StatusCode::OK, "{q}");
        q["tasks"].as_array().expect("tasks").clone()
    }
    async fn change(&self, actor: &str, task: &Value, action: &str) -> Value {
        let (s,t) = self.work(actor,&format!("/{}/action",task["id"].as_str().expect("id")),Some(json!({"revision":task["revision"],"action":action,"reason":"Synthetic reason"}))).await;
        assert_eq!(s, StatusCode::OK, "{t}");
        t
    }
    async fn review(&self, task: &Value) -> Value {
        let (s, r) = self
            .work(
                "operator",
                &format!("/{}/review", task["id"].as_str().expect("id")),
                Some(json!({"revision":task["revision"]})),
            )
            .await;
        assert_eq!(s, StatusCode::OK, "{r}");
        r
    }
    async fn execute_work(&self, request: Value) -> (StatusCode, Value) {
        self.call(
            "operator",
            "POST",
            &format!("/v1/warehouses/{W}/commands"),
            Some(request),
        )
        .await
    }
}
#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn discovery_claim_race_and_metadata_never_move_inventory() {
    let f = Fixture::new().await;
    f.setup().await;
    let version = f.version().await;
    let (a, b) = tokio::join!(
        f.work("operator", "/discover", Some(json!({}))),
        f.work("other", "/discover", Some(json!({})))
    );
    assert_eq!(a.0, StatusCode::OK);
    assert_eq!(b.0, StatusCode::OK);
    assert_eq!(
        a.1["created"].as_i64().expect("count") + b.1["created"].as_i64().expect("count"),
        2
    );
    let tasks = f.tasks().await;
    let task = &tasks[0];
    let path = format!("/{}/action", task["id"].as_str().expect("id"));
    let body = json!({"revision":task["revision"],"action":"claim"});
    let (a, b) = tokio::join!(
        f.work("operator", &path, Some(body.clone())),
        f.work("other", &path, Some(body))
    );
    assert!(
        (a.0 == StatusCode::OK && b.0 == StatusCode::CONFLICT)
            || (b.0 == StatusCode::OK && a.0 == StatusCode::CONFLICT)
    );
    let (actor, claimed) = if a.0 == StatusCode::OK {
        ("operator", a.1)
    } else {
        ("other", b.1)
    };
    let paused = f.change(actor, &claimed, "pause").await;
    let blocked = f.change(actor, &paused, "block").await;
    assert_eq!(blocked["status"], "blocked");
    let (s, q) = f.work("operator", "/discover", Some(json!({}))).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(q["created"], 0);
    let resumed = f.change(actor, &blocked, "resume").await;
    let released = f.change("admin", &resumed, "release").await;
    assert!(released["owner"].is_null());
    assert_eq!(
        f.version().await,
        version,
        "metadata cannot advance physical state"
    );
    assert_eq!(
        f.work("viewer", "/discover", Some(json!({}))).await.0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        f.work("foreign", "?view=all", None).await.0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        f.work(
            "viewer",
            &path,
            Some(json!({"revision":released["revision"],"action":"claim"}))
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    f.proof().await;
}
#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn physical_receipt_creates_work_once_and_move_retries_complete_once() {
    let f = Fixture::new().await;
    f.setup().await;
    let receipt=f.command(json!({"type":"create_receipt","reference":"WORK-RCV","supplier":"Synthetic","lines":[{"id":"LINE-1","sku":"SKU-A","lot":"NEW-LOT","quantity":3}]}),None).await;
    let id = receipt["result"]["entity_id"].as_str().expect("receipt id");
    let state = f.store.state(&f.tenant, W).await.expect("state");
    let line = &state
        .receipts
        .iter()
        .find(|r| r.id == id)
        .expect("receipt")
        .lines[0];
    let request = json!({"request_id":domain::id(),"expected_version":state.version,"command":{"type":"receive","receipt_id":id,"line_id":line.id,"quantity":3,"location":"RECV-01"}});
    for _ in 0..2 {
        let (s, b) = f
            .call(
                "operator",
                "POST",
                &format!("/v1/warehouses/{W}/commands"),
                Some(request.clone()),
            )
            .await;
        assert_eq!(s, StatusCode::OK, "{b}");
    }
    let tasks = f.tasks().await;
    assert_eq!(tasks.len(), 1);
    assert_eq!(tasks[0]["quantity"], 3);
    let claimed = f.change("operator", &tasks[0], "claim").await;
    let review = f.review(&claimed).await;
    let mut forged = review["request"].clone();
    forged["command"]["quantity"] = json!(1);
    assert_eq!(
        f.execute_work(forged).await.0,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    assert_eq!(
        f.call(
            "other",
            "POST",
            &format!("/v1/warehouses/{W}/commands"),
            Some(review["request"].clone())
        )
        .await
        .0,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    let (a, b) = tokio::join!(
        f.execute_work(review["request"].clone()),
        f.execute_work(review["request"].clone())
    );
    assert_eq!(a.0, StatusCode::OK, "{}", a.1);
    assert_eq!(b.0, StatusCode::OK, "{}", b.1);
    assert_ne!(a.1["replayed"], b.1["replayed"]);
    assert!(f.tasks().await.is_empty());
    let (_, closed) = f.work("viewer", "?view=completed", None).await;
    assert_eq!(closed["tasks"][0]["completed_quantity"], 3);
    assert_eq!(closed["tasks"][0]["status"], "completed");
    let event_count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM wms_work_task_events WHERE tenant=$1 AND action='putaway_confirmed'",
    )
    .bind(&f.tenant)
    .fetch_one(&f.store.pool)
    .await
    .expect("events");
    assert_eq!(event_count, 1);
    f.proof().await;
}
#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn capacity_and_ownership_changes_fence_review_with_definitive_retry_proof() {
    let f = Fixture::new().await;
    f.setup().await;
    f.work("operator", "/discover", Some(json!({}))).await;
    let tasks = f.tasks().await;
    let task = tasks
        .iter()
        .find(|t| t["sku"] == "SKU-A")
        .expect("SKU-A task");
    let claimed = f.change("operator", task, "claim").await;
    let (s, b) = f
        .call(
            "admin",
            "PUT",
            &format!("/v1/warehouses/{W}/locations/S-2/capacity"),
            Some(json!({"max_units":6})),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{b}");
    let review = f.review(&claimed).await;
    assert_eq!(
        review["request"]["command"]["quantity"], 2,
        "partial move respects destination capacity"
    );
    // A different SKU filling the destination is absent from a normal MoveStock resource map.
    f.command(json!({"type":"count_stock","sku":"SKU-B","lot":"OTHER","location":"S-2","counted":2,"reason":"Synthetic competing arrival"}),None).await;
    let (s, b) = f.execute_work(review["request"].clone()).await;
    assert_eq!(s, StatusCode::CONFLICT, "{b}");
    assert_eq!(b["command_outcome"]["reason"], "changed_resources");
    let current = f
        .tasks()
        .await
        .into_iter()
        .find(|t| t["id"] == task["id"])
        .expect("task");
    let fresh = f.review(&current).await;
    let current = f
        .tasks()
        .await
        .into_iter()
        .find(|t| t["id"] == task["id"])
        .expect("task");
    let released = f.change("admin", &current, "release").await;
    f.change("other", &released, "claim").await;
    let (s, b) = f.execute_work(fresh["request"].clone()).await;
    assert_eq!(s, StatusCode::CONFLICT, "{b}");
    assert_eq!(b["command_outcome"]["reason"], "changed_resources");
    let forged =
        sqlx::query("UPDATE wms_work_tasks SET completed_quantity=1 WHERE tenant=$1 AND id=$2")
            .bind(&f.tenant)
            .bind(task["id"].as_str())
            .execute(&f.store.pool)
            .await;
    assert!(forged.is_err(), "SQL cannot forge task progress");
    let erased = sqlx::query("DELETE FROM wms_work_executions WHERE tenant=$1")
        .bind(&f.tenant)
        .execute(&f.store.pool)
        .await;
    assert!(erased.is_err(), "review evidence is immutable");
    f.proof().await;
}
#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn partial_putaway_preserves_remaining_work_and_capacity_edits_invalidate_reviews() {
    let f = Fixture::new().await;
    f.setup().await;
    f.work("operator", "/discover", Some(json!({}))).await;
    let tasks = f.tasks().await;
    let task = tasks.iter().find(|t| t["sku"] == "SKU-A").expect("task");
    let claimed = f.change("operator", task, "claim").await;
    f.call(
        "admin",
        "PUT",
        &format!("/v1/warehouses/{W}/locations/S-2/capacity"),
        Some(json!({"max_units":6})),
    )
    .await;
    let path = format!("/{}/review", claimed["id"].as_str().expect("task id"));
    for quantity in [0, 10_001] {
        let (status, _) = f
            .work(
                "operator",
                &path,
                Some(json!({"revision":claimed["revision"],"quantity":quantity})),
            )
            .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    }
    let (status, review) = f
        .work(
            "operator",
            &path,
            Some(json!({"revision":claimed["revision"],"quantity":2})),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{review}");
    let (s, b) = f.execute_work(review["request"].clone()).await;
    assert_eq!(s, StatusCode::OK, "{b}");
    let current = f
        .tasks()
        .await
        .into_iter()
        .find(|t| t["id"] == task["id"])
        .expect("task");
    assert_eq!(current["completed_quantity"], 2);
    assert_eq!(current["status"], "claimed");
    let review = f.review(&current).await;
    assert_eq!(review["request"]["command"]["quantity"], 3);
    let to = review["request"]["command"]["to"]
        .as_str()
        .expect("destination");
    f.call(
        "admin",
        "PUT",
        &format!("/v1/warehouses/{W}/locations/{to}/capacity"),
        Some(json!({"max_units":1})),
    )
    .await;
    let (s, b) = f.execute_work(review["request"].clone()).await;
    assert_eq!(s, StatusCode::CONFLICT, "{b}");
    assert_eq!(b["command_outcome"]["reason"], "changed_resources");
    let current = f
        .tasks()
        .await
        .into_iter()
        .find(|t| t["id"] == task["id"])
        .expect("task");
    let before = f.version().await;
    let cancelled = f.change("admin", &current, "cancel").await;
    assert_eq!(cancelled["completed_quantity"], 2);
    assert_eq!(f.version().await, before);
    let (_, discovered) = f.work("operator", "/discover", Some(json!({}))).await;
    assert_eq!(
        discovered["created"], 1,
        "only cancelled remainder becomes available again"
    );
    f.proof().await;
}
