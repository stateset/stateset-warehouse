//! Mandatory in CI. Explicitly ignored locally unless invoked with --ignored;
//! missing Postgres fails rather than silently reporting a passing DB test.
use serde_json::{json, Value};
use sqlx::Row;
use stateset_warehouse::{
    api::{router, App},
    domain::*,
    integrations::Connections,
    store::{Identity, Request, Store},
};
use std::{
    collections::BTreeMap,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
};
use tower::ServiceExt;

async fn setup() -> (Store, Identity, State) {
    let url = std::env::var("WMS_TEST_DATABASE_URL")
        .expect("WMS_TEST_DATABASE_URL must point at an isolated Postgres test database");
    assert!(
        reqwest::Url::parse(&url)
            .expect("test database URL")
            .path()
            .contains("test"),
        "Test database name must contain 'test'; never run against production"
    );
    let store = Store::connect(&url).await.expect("real Postgres available");
    let identity = Identity {
        tenant: format!("test-{}", id()),
        actor: "test:operator".into(),
        role: "admin".into(),
    };
    let mut s = State::empty(Warehouse {
        id: "wh-test".into(),
        code: "TEST".into(),
        name: "Test Warehouse".into(),
        city: "Test".into(),
        timezone: "UTC".into(),
    });
    for c in [
        Command::CreateProduct {
            sku: "SKU".into(),
            name: "Product".into(),
            category: "General".into(),
            reorder_point: 1,
        },
        Command::CreateLocation {
            code: "A".into(),
            zone: "Storage".into(),
            kind: "storage".into(),
        },
        Command::CreateLocation {
            code: "B".into(),
            zone: "Storage".into(),
            kind: "storage".into(),
        },
        Command::CountStock {
            sku: "SKU".into(),
            lot: "LOT".into(),
            location: "A".into(),
            counted: 10,
            reason: "Test opening physical count".into(),
        },
    ] {
        s = s.execute(&c, "fixture").expect("fixture").0;
    }
    sqlx::query(
        "INSERT INTO wms_workspaces (tenant,warehouse_id,version,state) VALUES ($1,$2,$3,$4)",
    )
    .bind(&identity.tenant)
    .bind(&s.warehouse.id)
    .bind(s.version)
    .bind(json!(s))
    .execute(&store.pool)
    .await
    .expect("insert isolated fixture");
    (store, identity, s)
}
fn req(version: i64, command: Command) -> Request {
    Request {
        request_id: id(),
        expected_version: version,
        command,
    }
}
fn create_order(reference: &str, q: i64) -> Command {
    Command::CreateOrder {
        reference: reference.into(),
        source_order_id: Some(format!("oms-{reference}")),
        customer: "Customer".into(),
        destination: "City".into(),
        priority: "normal".into(),
        lines: vec![LineInput {
            id: "1".into(),
            sku: "SKU".into(),
            quantity: q,
        }],
    }
}
async fn execute(store: &Store, i: &Identity, c: Command) -> Value {
    let s = store.state(&i.tenant, "wh-test").await.expect("state");
    store
        .execute(i, "wh-test", &req(s.version, c))
        .await
        .expect("command")["result"]
        .clone()
}
async fn counts(store: &Store, tenant: &str) -> (i64, i64, i64) {
    let row=sqlx::query("SELECT (SELECT count(*) FROM wms_commands WHERE tenant=$1) AS commands,(SELECT count(*) FROM wms_events WHERE tenant=$1) AS events,(SELECT count(*) FROM wms_outbox WHERE tenant=$1) AS deliveries").bind(tenant).fetch_one(&store.pool).await.expect("counts");
    (
        row.get("commands"),
        row.get("events"),
        row.get("deliveries"),
    )
}

#[tokio::test]
#[ignore = "requires isolated Postgres; scripts/check.sh --postgres"]
async fn concurrent_last_unit_allocation_never_oversells() {
    let (store, i, _) = setup().await;
    let mut orders = vec![];
    for n in 0..20 {
        orders.push(
            execute(&store, &i, create_order(&format!("SO-{n}"), 10)).await["entity_id"]
                .as_str()
                .expect("order ID")
                .to_string(),
        );
    }
    let version = store
        .state(&i.tenant, "wh-test")
        .await
        .expect("state")
        .version;
    let mut tasks = vec![];
    for oid in orders {
        let store = store.clone();
        let i = i.clone();
        tasks.push(tokio::spawn(async move {
            store
                .execute(
                    &i,
                    "wh-test",
                    &req(version, Command::Allocate { order_id: oid }),
                )
                .await
        }));
    }
    let mut successes = 0;
    for task in tasks {
        if task.await.expect("join").is_ok() {
            successes += 1;
        }
    }
    assert_eq!(successes, 1);
    let s = store.state(&i.tenant, "wh-test").await.expect("state");
    assert_eq!(s.stocks.iter().map(|b| b.reserved).sum::<i64>(), 10);
    s.validate().expect("reconciled");
    let waiting = s
        .orders
        .iter()
        .find(|o| o.status == "pending")
        .expect("pending");
    let e = store
        .execute(
            &i,
            "wh-test",
            &req(
                s.version,
                Command::Allocate {
                    order_id: waiting.id.clone(),
                },
            ),
        )
        .await
        .expect_err("short after fresh read");
    assert_eq!(e.status, 422);
}
#[tokio::test]
#[ignore = "requires isolated Postgres; scripts/check.sh --postgres"]
async fn concurrent_identical_requests_create_one_receipt_and_event() {
    let (store, i, s) = setup().await;
    let request = req(s.version, create_order("SO-DUP", 1));
    let mut tasks = vec![];
    for _ in 0..16 {
        let store = store.clone();
        let i = i.clone();
        let r = request.clone();
        tasks.push(tokio::spawn(async move {
            store.execute(&i, "wh-test", &r).await
        }));
    }
    let mut original = 0;
    let mut entities = std::collections::BTreeSet::new();
    for t in tasks {
        let r = t.await.expect("join").expect("replay");
        if r["replayed"] == false {
            original += 1;
        }
        entities.insert(r["result"]["entity_id"].as_str().expect("id").to_string());
    }
    assert_eq!(original, 1);
    assert_eq!(entities.len(), 1);
    assert_eq!(counts(&store, &i.tenant).await, (1, 1, 1));
}
#[tokio::test]
#[ignore = "requires isolated Postgres; scripts/check.sh --postgres"]
async fn restart_and_lost_response_return_original_save() {
    let (store, i, s) = setup().await;
    let request = req(s.version, create_order("SO-RESTART", 2));
    let original = store
        .execute(&i, "wh-test", &request)
        .await
        .expect("first save");
    execute(
        &store,
        &i,
        Command::CreateLocation {
            code: "C".into(),
            zone: "Storage".into(),
            kind: "storage".into(),
        },
    )
    .await;
    store.pool.close().await;
    let restored = Store::connect(&std::env::var("WMS_TEST_DATABASE_URL").expect("URL"))
        .await
        .expect("reconnect");
    let replay = restored
        .execute(&i, "wh-test", &request)
        .await
        .expect("original response recovered");
    assert_eq!(replay["result"], original["result"]);
    assert_eq!(replay["replayed"], true);
    assert_eq!(counts(&restored, &i.tenant).await, (2, 2, 2));
    assert_eq!(
        restored
            .receipt(&i, "wh-test", &request.request_id)
            .await
            .expect("receipt")["found"],
        true
    );
}
#[tokio::test]
#[ignore = "requires isolated Postgres; scripts/check.sh --postgres"]
async fn request_key_payload_or_actor_conflicts_are_rejected() {
    let (store, i, s) = setup().await;
    let mut r = req(s.version, create_order("SO-KEY", 2));
    store.execute(&i, "wh-test", &r).await.expect("first");
    r.command = create_order("SO-DIFFERENT", 2);
    assert_eq!(
        store
            .execute(&i, "wh-test", &r)
            .await
            .expect_err("payload conflict")
            .status,
        409
    );
    r.command = create_order("SO-KEY", 2);
    let mut another = i.clone();
    another.actor = "other:operator".into();
    assert_eq!(
        store
            .execute(&another, "wh-test", &r)
            .await
            .expect_err("actor conflict")
            .status,
        409
    );
}
#[tokio::test]
#[ignore = "requires isolated Postgres; scripts/check.sh --postgres"]
async fn stale_counts_and_competing_moves_cannot_overwrite_new_stock() {
    let (store, i, s) = setup().await;
    let mut tasks = vec![];
    for _ in 0..8 {
        let store = store.clone();
        let i = i.clone();
        let version = s.version;
        tasks.push(tokio::spawn(async move {
            store
                .execute(
                    &i,
                    "wh-test",
                    &req(
                        version,
                        Command::MoveStock {
                            sku: "SKU".into(),
                            lot: "LOT".into(),
                            from: "A".into(),
                            to: "B".into(),
                            quantity: 8,
                        },
                    ),
                )
                .await
        }));
    }
    let mut successes = 0;
    for t in tasks {
        if t.await.expect("join").is_ok() {
            successes += 1;
        }
    }
    assert_eq!(successes, 1);
    let e = store
        .execute(
            &i,
            "wh-test",
            &req(
                s.version,
                Command::CountStock {
                    sku: "SKU".into(),
                    lot: "LOT".into(),
                    location: "A".into(),
                    counted: 10,
                    reason: "Stale physical count".into(),
                },
            ),
        )
        .await
        .expect_err("stale count");
    assert_eq!(e.status, 409);
    let state = store.state(&i.tenant, "wh-test").await.expect("state");
    assert_eq!(
        state
            .stocks
            .iter()
            .find(|b| b.location == "A")
            .expect("A")
            .on_hand,
        2
    );
    state.validate().expect("valid");
}
#[tokio::test]
#[ignore = "requires isolated Postgres; scripts/check.sh --postgres"]
async fn duplicate_receipt_submission_does_not_receive_twice() {
    let (store, i, _) = setup().await;
    let receipt = execute(
        &store,
        &i,
        Command::CreateReceipt {
            reference: "RCV-DUP".into(),
            supplier: "Supplier".into(),
            lines: vec![ReceiptInput {
                id: "1".into(),
                sku: "SKU".into(),
                lot: "LOT-2".into(),
                quantity: 5,
            }],
        },
    )
    .await;
    let s = store.state(&i.tenant, "wh-test").await.expect("state");
    let request = req(
        s.version,
        Command::Receive {
            receipt_id: receipt["entity_id"].as_str().expect("ID").into(),
            line_id: "1".into(),
            quantity: 5,
            location: "RECV-01".into(),
        },
    );
    store.execute(&i, "wh-test", &request).await.expect("first");
    store.execute(&i, "wh-test", &request).await.expect("retry");
    let s = store.state(&i.tenant, "wh-test").await.expect("state");
    assert_eq!(
        s.stocks
            .iter()
            .find(|b| b.lot == "LOT-2")
            .expect("received")
            .on_hand,
        5
    );
    assert_eq!(
        s.movements.iter().filter(|m| m.kind == "receipt").count(),
        1
    );
}
#[tokio::test]
#[ignore = "requires isolated Postgres; scripts/check.sh --postgres"]
async fn failure_after_state_event_and_receipt_writes_rolls_everything_back() {
    let (store, i, s) = setup().await;
    let suffix = id().replace('-', "");
    let function = format!("wms_fault_{suffix}");
    let trigger = format!("wms_fault_trigger_{suffix}");
    let sql=format!("CREATE FUNCTION {function}() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.tenant='{}' THEN RAISE EXCEPTION 'injected final outbox failure'; END IF; RETURN NEW; END $$; CREATE TRIGGER {trigger} BEFORE INSERT ON wms_outbox FOR EACH ROW EXECUTE FUNCTION {function}();",i.tenant);
    sqlx::raw_sql(&sql)
        .execute(&store.pool)
        .await
        .expect("install isolated fault");
    let request = req(
        s.version,
        Command::CountStock {
            sku: "SKU".into(),
            lot: "LOT".into(),
            location: "A".into(),
            counted: 7,
            reason: "Test count".into(),
        },
    );
    assert!(store.execute(&i, "wh-test", &request).await.is_err());
    assert_eq!(store.state(&i.tenant, "wh-test").await.expect("state"), s);
    assert_eq!(counts(&store, &i.tenant).await, (0, 0, 0));
    sqlx::raw_sql(&format!(
        "DROP TRIGGER {trigger} ON wms_outbox; DROP FUNCTION {function}();"
    ))
    .execute(&store.pool)
    .await
    .expect("remove fault");
    store
        .execute(&i, "wh-test", &request)
        .await
        .expect("same request safely applies after rollback");
    assert_eq!(counts(&store, &i.tenant).await, (1, 1, 1));
}
#[tokio::test]
#[ignore = "requires isolated Postgres; scripts/check.sh --postgres"]
async fn tenant_boundaries_apply_to_state_commands_receipts_and_events() {
    use axum::{
        body::Body,
        http::{Request as HttpRequest, StatusCode},
    };
    let (store, i, s) = setup().await;
    let request = req(s.version, create_order("SO-PRIVATE", 1));
    store
        .execute(&i, "wh-test", &request)
        .await
        .expect("private order");
    let outsider = Identity {
        tenant: "other-org".into(),
        actor: "attacker".into(),
        role: "admin".into(),
    };
    assert_eq!(
        store
            .state(&outsider.tenant, "wh-test")
            .await
            .expect_err("hidden state")
            .status,
        404
    );
    assert_eq!(
        store
            .execute(&outsider, "wh-test", &request)
            .await
            .expect_err("hidden warehouse")
            .status,
        404
    );
    assert_eq!(
        store
            .receipt(&outsider, "wh-test", &request.request_id)
            .await
            .expect("receipt lookup")["found"],
        false
    );
    let event: String = sqlx::query_scalar("SELECT id FROM wms_events WHERE tenant=$1")
        .bind(&i.tenant)
        .fetch_one(&store.pool)
        .await
        .expect("event ID");
    let app = router(App {
        store,
        credentials: Arc::new(vec![("outsider-key".into(), outsider)]),
        demo: false,
        connections: Connections::default(),
    });
    let response = app
        .oneshot(
            HttpRequest::builder()
                .uri(format!("/v1/events/{event}"))
                .header("Authorization", "Bearer outsider-key")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}
#[tokio::test]
#[ignore = "requires isolated Postgres and local HTTP; scripts/check.sh --postgres"]
async fn failed_delivery_retries_same_event_id_and_recovers_expired_lease() {
    use axum::{
        http::{HeaderMap, StatusCode},
        routing::post,
        Router,
    };
    let (store, i, _) = setup().await;
    execute(&store, &i, create_order("SO-DELIVER", 1)).await;
    let attempts = Arc::new(AtomicUsize::new(0));
    let seen = Arc::new(tokio::sync::Mutex::new(Vec::new()));
    let receiver = Router::new().route(
        "/events",
        post({
            let attempts = attempts.clone();
            let seen = seen.clone();
            move |headers: HeaderMap| {
                let attempts = attempts.clone();
                let seen = seen.clone();
                async move {
                    seen.lock().await.push(
                        headers
                            .get("Idempotency-Key")
                            .expect("idempotency header")
                            .to_str()
                            .expect("header")
                            .to_string(),
                    );
                    if attempts.fetch_add(1, Ordering::SeqCst) == 0 {
                        StatusCode::SERVICE_UNAVAILABLE
                    } else {
                        StatusCode::ACCEPTED
                    }
                }
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("local HTTP");
    let address = listener.local_addr().expect("address");
    let server = tokio::spawn(async move {
        axum::serve(listener, receiver).await.expect("receiver");
    });
    let c = Connections(Arc::new(BTreeMap::from([(
        (i.tenant.clone(), "oms".into()),
        (format!("http://{address}/events"), "test-token".into()),
    )])));
    assert!(c.dispatch_one(&store).await.expect("failed turn"));
    let o = store.outbox(&i.tenant, "wh-test").await.expect("queue");
    assert_eq!(o[0]["status"], "pending");
    assert_eq!(o[0]["attempts"], 1);
    sqlx::query("UPDATE wms_outbox SET status='delivering',lease_token='crashed-worker',lease_until=now()-interval '1 second' WHERE tenant=$1").bind(&i.tenant).execute(&store.pool).await.expect("simulate worker crash");
    assert!(c.dispatch_one(&store).await.expect("lease recovery"));
    let o = store.outbox(&i.tenant, "wh-test").await.expect("queue");
    assert_eq!(o[0]["status"], "delivered");
    assert_eq!(o[0]["attempts"], 2);
    let ids = seen.lock().await;
    assert_eq!(ids.len(), 2);
    assert_eq!(ids[0], ids[1]);
    assert!(!c
        .dispatch_one(&store)
        .await
        .expect("no duplicate after acceptance"));
    server.abort();
}
#[tokio::test]
#[ignore = "requires isolated Postgres and local HTTP; scripts/check.sh --postgres"]
async fn rejected_receiver_dead_letters_without_rolling_back_physical_work() {
    use axum::{http::StatusCode, routing::post, Router};
    let (store, i, _) = setup().await;
    execute(&store, &i, create_order("SO-DEAD", 1)).await;
    let receiver = Router::new().route("/events", post(|| async { StatusCode::BAD_REQUEST }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("HTTP");
    let address = listener.local_addr().expect("address");
    let server = tokio::spawn(async move {
        axum::serve(listener, receiver).await.expect("receiver");
    });
    let c = Connections(Arc::new(BTreeMap::from([(
        (i.tenant.clone(), "oms".into()),
        (format!("http://{address}/events"), "token".into()),
    )])));
    for _ in 0..8 {
        sqlx::query("UPDATE wms_outbox SET due_at=now() WHERE tenant=$1")
            .bind(&i.tenant)
            .execute(&store.pool)
            .await
            .expect("test clock");
        assert!(c.dispatch_one(&store).await.expect("delivery"));
    }
    let o = store.outbox(&i.tenant, "wh-test").await.expect("queue");
    assert_eq!(o[0]["status"], "dead_letter");
    assert_eq!(o[0]["attempts"], 8);
    assert_eq!(
        store
            .state(&i.tenant, "wh-test")
            .await
            .expect("state")
            .orders
            .len(),
        1
    );
    store
        .retry(&i, o[0]["id"].as_str().expect("outbox ID"))
        .await
        .expect("explicit retry");
    assert_eq!(
        store.outbox(&i.tenant, "wh-test").await.expect("queue")[0]["status"],
        "pending"
    );
    server.abort();
}

#[tokio::test]
#[ignore = "requires isolated Postgres; scripts/check.sh --postgres"]
async fn database_constraint_rejects_raw_balance_corruption() {
    let (store, i, mut s) = setup().await;
    s.stocks[0].reserved = s.stocks[0].on_hand + 1;
    let error = sqlx::query(
        "UPDATE wms_workspaces SET state=$1 WHERE tenant=$2 AND warehouse_id='wh-test'",
    )
    .bind(json!(s))
    .bind(&i.tenant)
    .execute(&store.pool)
    .await
    .expect_err("database must reject direct SQL corruption");
    assert_eq!(
        error.as_database_error().and_then(|e| e.code()).as_deref(),
        Some("23514")
    );
    store
        .state(&i.tenant, "wh-test")
        .await
        .expect("state")
        .validate()
        .expect("original still valid");
}
