//! Mandatory in CI. Explicitly ignored locally unless invoked with --ignored;
//! missing Postgres fails rather than silently reporting a passing DB test.
use serde_json::Value;
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
    store
        .provision(&identity.tenant, false)
        .await
        .expect("provision normalized warehouse");
    for c in [
        Command::CreateProduct {
            serial_controlled: false,
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
        let state = store
            .state(&identity.tenant, "wh-main")
            .await
            .expect("state");
        store
            .execute(&identity, "wh-main", &req(state.version, c))
            .await
            .expect("fixture command");
    }
    // Seed commands are real committed work, excluded from per-scenario counts.
    sqlx::query("DELETE FROM wms_outbox WHERE tenant=$1")
        .bind(&identity.tenant)
        .execute(&store.pool)
        .await
        .expect("clear fixture queue");
    let s = store
        .state(&identity.tenant, "wh-main")
        .await
        .expect("seeded state");
    (store, identity, s)
}
fn req(version: i64, command: Command) -> Request {
    Request {
        serial_inputs: Vec::new(),
        unit_inputs: Vec::new(),
        request_id: id(),
        expected_version: version,
        resource_versions: None,
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
    let s = store.state(&i.tenant, "wh-main").await.expect("state");
    store
        .execute(i, "wh-main", &req(s.version, c))
        .await
        .expect("command")["result"]
        .clone()
}
async fn counts(store: &Store, tenant: &str) -> (i64, i64, i64) {
    let row=sqlx::query("SELECT (SELECT count(*)-4 FROM wms_commands WHERE tenant=$1) AS commands,(SELECT count(*)-4 FROM wms_events WHERE tenant=$1) AS events,(SELECT count(*) FROM wms_outbox WHERE tenant=$1) AS deliveries").bind(tenant).fetch_one(&store.pool).await.expect("counts");
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
        .state(&i.tenant, "wh-main")
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
                    "wh-main",
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
    let s = store.state(&i.tenant, "wh-main").await.expect("state");
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
            "wh-main",
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
            store.execute(&i, "wh-main", &r).await
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
        .execute(&i, "wh-main", &request)
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
        .execute(&i, "wh-main", &request)
        .await
        .expect("original response recovered");
    assert_eq!(replay["result"], original["result"]);
    assert_eq!(replay["replayed"], true);
    assert_eq!(counts(&restored, &i.tenant).await, (2, 2, 2));
    assert_eq!(
        restored
            .receipt(&i, "wh-main", &request.request_id)
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
    store.execute(&i, "wh-main", &r).await.expect("first");
    r.command = create_order("SO-DIFFERENT", 2);
    assert_eq!(
        store
            .execute(&i, "wh-main", &r)
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
            .execute(&another, "wh-main", &r)
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
                    "wh-main",
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
            "wh-main",
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
    let state = store.state(&i.tenant, "wh-main").await.expect("state");
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
    let s = store.state(&i.tenant, "wh-main").await.expect("state");
    let request = req(
        s.version,
        Command::Receive {
            receipt_id: receipt["entity_id"].as_str().expect("ID").into(),
            line_id: "1".into(),
            quantity: 5,
            location: "RECV-01".into(),
        },
    );
    store.execute(&i, "wh-main", &request).await.expect("first");
    store.execute(&i, "wh-main", &request).await.expect("retry");
    let s = store.state(&i.tenant, "wh-main").await.expect("state");
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
    assert!(store.execute(&i, "wh-main", &request).await.is_err());
    assert_eq!(store.state(&i.tenant, "wh-main").await.expect("state"), s);
    assert_eq!(counts(&store, &i.tenant).await, (0, 0, 0));
    sqlx::raw_sql(&format!(
        "DROP TRIGGER {trigger} ON wms_outbox; DROP FUNCTION {function}();"
    ))
    .execute(&store.pool)
    .await
    .expect("remove fault");
    store
        .execute(&i, "wh-main", &request)
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
        .execute(&i, "wh-main", &request)
        .await
        .expect("private order");
    let outsider = Identity {
        tenant: "other-org".into(),
        actor: "attacker".into(),
        role: "admin".into(),
    };
    assert_eq!(
        store
            .state(&outsider.tenant, "wh-main")
            .await
            .expect_err("hidden state")
            .status,
        404
    );
    assert_eq!(
        store
            .execute(&outsider, "wh-main", &request)
            .await
            .expect_err("hidden warehouse")
            .status,
        404
    );
    assert_eq!(
        store
            .receipt(&outsider, "wh-main", &request.request_id)
            .await
            .expect("receipt lookup")["found"],
        false
    );
    let event: String = sqlx::query_scalar(
        "SELECT id FROM wms_events WHERE tenant=$1 ORDER BY version DESC LIMIT 1",
    )
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
    let o = store.outbox(&i.tenant, "wh-main").await.expect("queue");
    assert_eq!(o[0]["status"], "pending");
    assert_eq!(o[0]["attempts"], 1);
    sqlx::query("UPDATE wms_outbox SET status='delivering',lease_token='crashed-worker',lease_until=now()-interval '1 second' WHERE tenant=$1").bind(&i.tenant).execute(&store.pool).await.expect("simulate worker crash");
    assert!(c.dispatch_one(&store).await.expect("lease recovery"));
    let o = store.outbox(&i.tenant, "wh-main").await.expect("queue");
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
    let o = store.outbox(&i.tenant, "wh-main").await.expect("queue");
    assert_eq!(o[0]["status"], "dead_letter");
    assert_eq!(o[0]["attempts"], 8);
    assert_eq!(
        store
            .state(&i.tenant, "wh-main")
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
        store.outbox(&i.tenant, "wh-main").await.expect("queue")[0]["status"],
        "pending"
    );
    server.abort();
}

#[tokio::test]
#[ignore = "requires isolated Postgres and local HTTP; scripts/check.sh --postgres"]
async fn exhausted_delivery_lease_preserves_uncertainty_fences_stale_sender_and_allows_progress() {
    use axum::{
        http::{HeaderMap, StatusCode},
        routing::post,
        Router,
    };
    use std::time::Duration;
    use tokio::sync::Notify;

    let (store, i, _) = setup().await;
    execute(&store, &i, create_order("SO-EXHAUSTED", 1)).await;
    let first = store.outbox(&i.tenant, "wh-main").await.expect("queue")[0].clone();
    let first_id = first["id"].as_str().expect("outbox ID");
    let first_event = first["event_id"].as_str().expect("event ID");
    let calls = Arc::new(AtomicUsize::new(0));
    let seen = Arc::new(tokio::sync::Mutex::new(Vec::new()));
    let eighth_started = Arc::new(Notify::new());
    let release_eighth = Arc::new(Notify::new());
    let receiver = Router::new().route(
        "/events",
        post({
            let calls = calls.clone();
            let seen = seen.clone();
            let eighth_started = eighth_started.clone();
            let release_eighth = release_eighth.clone();
            move |headers: HeaderMap| {
                let calls = calls.clone();
                let seen = seen.clone();
                let eighth_started = eighth_started.clone();
                let release_eighth = release_eighth.clone();
                async move {
                    let ordinal = calls.fetch_add(1, Ordering::SeqCst);
                    seen.lock().await.push(
                        headers["Idempotency-Key"]
                            .to_str()
                            .expect("idempotency header")
                            .to_string(),
                    );
                    if ordinal < 7 {
                        return StatusCode::SERVICE_UNAVAILABLE;
                    }
                    if ordinal == 7 {
                        eighth_started.notify_one();
                        release_eighth.notified().await;
                    }
                    StatusCode::ACCEPTED
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
    let connections = Connections(Arc::new(BTreeMap::from([(
        (i.tenant.clone(), "oms".into()),
        (format!("http://{address}/events"), "test-token".into()),
    )])));

    for _ in 0..7 {
        sqlx::query("UPDATE wms_outbox SET due_at=now()-interval '1 minute' WHERE id=$1")
            .bind(first_id)
            .execute(&store.pool)
            .await
            .expect("advance retry clock");
        assert!(connections
            .dispatch_one(&store)
            .await
            .expect("failed attempt"));
    }
    sqlx::query("UPDATE wms_outbox SET due_at=now()-interval '1 minute' WHERE id=$1")
        .bind(first_id)
        .execute(&store.pool)
        .await
        .expect("advance final attempt clock");
    let stale_sender = tokio::spawn({
        let connections = connections.clone();
        let store = store.clone();
        async move { connections.dispatch_one(&store).await }
    });
    tokio::time::timeout(Duration::from_secs(3), eighth_started.notified())
        .await
        .expect("eighth request reached actual receiver");

    // Another event can progress after exhaustion; retiring a lease is itself
    // work, so the dispatch loop must not stop before inspecting the next row.
    execute(&store, &i, create_order("SO-AFTER-EXHAUSTED", 1)).await;
    let second = store
        .outbox(&i.tenant, "wh-main")
        .await
        .expect("next event")
        .into_iter()
        .find(|item| item["id"] != first["id"])
        .expect("second outbox row");
    sqlx::query("UPDATE wms_outbox SET lease_until=now()-interval '1 second' WHERE id=$1 AND status='delivering' AND attempts=8")
        .bind(first_id).execute(&store.pool).await.expect("expire eighth delivery lease");
    assert!(connections
        .dispatch_one(&store)
        .await
        .expect("exhausted lease is retired as progress"));
    assert_eq!(
        calls.load(Ordering::SeqCst),
        8,
        "no ninth automatic HTTP request"
    );
    let retired: Value = sqlx::query_scalar("SELECT to_jsonb(o) FROM wms_outbox o WHERE id=$1")
        .bind(first_id)
        .fetch_one(&store.pool)
        .await
        .expect("retired queue row");
    assert_eq!(retired["status"], "dead_letter");
    assert_eq!(retired["attempts"], 8);
    assert!(retired["lease_token"].is_null() && retired["lease_until"].is_null());
    assert!(retired["last_error"]
        .as_str()
        .expect("retained uncertainty")
        .contains("uncertain"));
    let history: Value = sqlx::query_scalar("SELECT jsonb_agg(to_jsonb(a) ORDER BY created_at,id) FROM wms_delivery_attempts a WHERE outbox_id=$1")
        .bind(first_id).fetch_one(&store.pool).await.expect("retained delivery history");
    let attempts = history.as_array().expect("attempt history");
    assert_eq!(attempts.len(), 8);
    assert_eq!(
        attempts.iter().filter(|a| a["outcome"] == "failed").count(),
        7
    );
    assert_eq!(
        attempts
            .iter()
            .filter(|a| a["outcome"] == "uncertain")
            .count(),
        1
    );
    assert!(attempts.iter().all(|a| !a["finished_at"].is_null()));

    // The old HTTP request now succeeds, but its retired lease has no authority
    // to replace either the queue decision or the finished uncertain evidence.
    release_eighth.notify_one();
    assert!(tokio::time::timeout(Duration::from_secs(3), stale_sender)
        .await
        .expect("old sender finished")
        .expect("sender task")
        .expect("stale completion"));
    let after: Value = sqlx::query_scalar("SELECT to_jsonb(o) FROM wms_outbox o WHERE id=$1")
        .bind(first_id)
        .fetch_one(&store.pool)
        .await
        .expect("fenced queue row");
    let after_history: Value = sqlx::query_scalar("SELECT jsonb_agg(to_jsonb(a) ORDER BY created_at,id) FROM wms_delivery_attempts a WHERE outbox_id=$1")
        .bind(first_id).fetch_one(&store.pool).await.expect("fenced history");
    assert_eq!(after, retired);
    assert_eq!(after_history, history);

    assert!(connections
        .dispatch_one(&store)
        .await
        .expect("next event progresses"));
    assert_eq!(calls.load(Ordering::SeqCst), 9);
    assert!(!connections
        .dispatch_one(&store)
        .await
        .expect("queue is exhausted"));
    assert_eq!(
        store
            .outbox(&i.tenant, "wh-main")
            .await
            .expect("queue")
            .into_iter()
            .find(|item| item["id"] == second["id"])
            .expect("second event")["status"],
        "delivered"
    );

    store
        .retry(&i, first_id)
        .await
        .expect("explicit audited requeue");
    let audit: i64 = sqlx::query_scalar("SELECT count(*) FROM wms_admin_audit WHERE tenant=$1 AND actor=$2 AND action='outbox.retry' AND reference=$3")
        .bind(&i.tenant).bind(&i.actor).bind(first_id).fetch_one(&store.pool).await.expect("retry audit");
    assert_eq!(audit, 1);
    assert!(connections
        .dispatch_one(&store)
        .await
        .expect("authorized retry"));
    assert_eq!(calls.load(Ordering::SeqCst), 10);
    let queue = store
        .outbox(&i.tenant, "wh-main")
        .await
        .expect("final queue");
    let retried = queue
        .iter()
        .find(|item| item["id"] == first["id"])
        .expect("retried row");
    assert_eq!(retried["status"], "delivered");
    assert_eq!(retried["attempts"], 1);
    let total: i64 =
        sqlx::query_scalar("SELECT count(*) FROM wms_delivery_attempts WHERE outbox_id=$1")
            .bind(first_id)
            .fetch_one(&store.pool)
            .await
            .expect("all retained attempts");
    assert_eq!(total, 9);
    let ids = seen.lock().await;
    assert!(ids[..8].iter().all(|event| event == first_event));
    assert_eq!(
        ids[8],
        second["event_id"].as_str().expect("second event ID")
    );
    assert_eq!(ids[9], first_event);
    server.abort();
}

#[tokio::test]
#[ignore = "requires isolated Postgres; scripts/check.sh --postgres"]
async fn database_constraint_rejects_raw_balance_corruption() {
    let (store, i, mut s) = setup().await;
    s.stocks[0].reserved = s.stocks[0].on_hand + 1;
    let error = sqlx::query(
        "UPDATE wms_stock SET reserved=on_hand+1 WHERE tenant=$1 AND warehouse_id='wh-main'",
    )
    .bind(&i.tenant)
    .execute(&store.pool)
    .await
    .expect_err("database must reject direct SQL corruption");
    assert_eq!(
        error.as_database_error().and_then(|e| e.code()).as_deref(),
        Some("23514")
    );
    store
        .state(&i.tenant, "wh-main")
        .await
        .expect("state")
        .validate()
        .expect("original still valid");
}

#[tokio::test]
#[ignore = "requires isolated Postgres and local HTTP; scripts/check.sh --postgres"]
async fn delivery_recipient_change_cannot_redirect_original_event_even_after_admin_retry() {
    use axum::{
        http::{HeaderMap, StatusCode},
        routing::post,
        Json, Router,
    };
    let (store, i, _) = setup().await;
    execute(&store, &i, create_order("SO-PINNED-RECEIVER", 1)).await;
    let item = store
        .outbox(&i.tenant, "wh-main")
        .await
        .expect("delivery recipient fixture operation")[0]
        .clone();
    let oid = item["id"].as_str().expect("string evidence");
    let seen = Arc::new(tokio::sync::Mutex::new(Vec::new()));
    let wrong_calls = Arc::new(AtomicUsize::new(0));
    let receiver = Router::new()
        .route(
            "/original",
            post({
                let seen = seen.clone();
                move |headers: HeaderMap, Json(body): Json<Value>| {
                    let seen = seen.clone();
                    async move {
                        let mut calls = seen.lock().await;
                        calls.push((
                            headers["authorization"]
                                .to_str()
                                .expect("receiver request header")
                                .to_string(),
                            headers["idempotency-key"]
                                .to_str()
                                .expect("receiver request header")
                                .to_string(),
                            body,
                        ));
                        if calls.len() == 1 {
                            StatusCode::SERVICE_UNAVAILABLE
                        } else {
                            StatusCode::ACCEPTED
                        }
                    }
                }
            }),
        )
        .route(
            "/replacement",
            post({
                let calls = wrong_calls.clone();
                move || {
                    let calls = calls.clone();
                    async move {
                        calls.fetch_add(1, Ordering::SeqCst);
                        StatusCode::ACCEPTED
                    }
                }
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("delivery recipient fixture operation");
    let address = listener.local_addr().expect("local receiver address");
    let server = tokio::spawn(async move {
        axum::serve(listener, receiver)
            .await
            .expect("delivery recipient fixture operation")
    });
    let connections = |path: &str, token: &str| {
        Connections(Arc::new(BTreeMap::from([(
            (i.tenant.clone(), "oms".into()),
            (format!("http://{address}/{path}"), token.into()),
        )])))
    };
    assert!(connections("original", "old-token")
        .dispatch_one(&store)
        .await
        .expect("delivery recipient fixture operation"));
    sqlx::query("UPDATE wms_outbox SET due_at=now() WHERE id=$1")
        .bind(oid)
        .execute(&store.pool)
        .await
        .expect("delivery recipient fixture operation");
    assert!(connections("replacement", "different-token")
        .dispatch_one(&store)
        .await
        .expect("delivery recipient fixture operation"));
    assert_eq!(
        wrong_calls.load(Ordering::SeqCst),
        0,
        "a retained event must never reach a replacement receiver"
    );
    let held = store
        .outbox(&i.tenant, "wh-main")
        .await
        .expect("delivery recipient fixture operation")[0]
        .clone();
    assert_eq!(held["status"], "dead_letter");
    assert_eq!(
        held["attempts"], 1,
        "configuration holds are not HTTP attempts"
    );
    assert!(held["last_error"]
        .as_str()
        .expect("string evidence")
        .contains("receiver"));
    store
        .retry(&i, oid)
        .await
        .expect("delivery recipient fixture operation");
    assert!(connections("replacement", "different-token")
        .dispatch_one(&store)
        .await
        .expect("delivery recipient fixture operation"));
    assert_eq!(
        wrong_calls.load(Ordering::SeqCst),
        0,
        "audited requeue cannot repoint the receiver"
    );
    store
        .retry(&i, oid)
        .await
        .expect("delivery recipient fixture operation");
    assert!(connections("original", "rotated-token")
        .dispatch_one(&store)
        .await
        .expect("delivery recipient fixture operation"));
    let done = store
        .outbox(&i.tenant, "wh-main")
        .await
        .expect("delivery recipient fixture operation")[0]
        .clone();
    assert_eq!(done["status"], "delivered");
    let calls = seen.lock().await;
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0].0, "Bearer old-token");
    assert_eq!(calls[1].0, "Bearer rotated-token");
    assert_eq!(calls[0].1, calls[1].1);
    assert_eq!(calls[0].2, calls[1].2);
    assert_eq!(
        calls[0].1,
        item["event_id"].as_str().expect("string evidence")
    );
    let attempts: i64 =
        sqlx::query_scalar("SELECT count(*) FROM wms_delivery_attempts WHERE outbox_id=$1")
            .bind(oid)
            .fetch_one(&store.pool)
            .await
            .expect("delivery recipient fixture operation");
    assert_eq!(attempts, 2);
    assert!(store
        .reconciliation(&i.tenant, "wh-main")
        .await
        .expect("delivery recipient fixture operation")["ok"]
        .as_bool()
        .expect("delivery recipient fixture operation"));
    server.abort();
}

#[tokio::test]
#[ignore = "requires isolated Postgres and local HTTP; scripts/check.sh --postgres"]
async fn historical_delivery_without_receiver_proof_stays_held_across_requeue() {
    use axum::{http::StatusCode, routing::post, Router};
    let (store, i, _) = setup().await;
    execute(&store, &i, create_order("SO-LEGACY-RECEIVER", 1)).await;
    let item = store
        .outbox(&i.tenant, "wh-main")
        .await
        .expect("delivery recipient fixture operation")[0]
        .clone();
    let oid = item["id"].as_str().expect("string evidence");
    // This is the durable marker assigned by the upgrade to historical sends.
    sqlx::query("UPDATE wms_outbox SET recipient_fingerprint='unverified' WHERE id=$1")
        .bind(oid)
        .execute(&store.pool)
        .await
        .expect("delivery recipient fixture operation");
    let calls = Arc::new(AtomicUsize::new(0));
    let receiver = Router::new().route(
        "/events",
        post({
            let calls = calls.clone();
            move || {
                let calls = calls.clone();
                async move {
                    calls.fetch_add(1, Ordering::SeqCst);
                    StatusCode::ACCEPTED
                }
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("delivery recipient fixture operation");
    let address = listener.local_addr().expect("local receiver address");
    let server = tokio::spawn(async move {
        axum::serve(listener, receiver)
            .await
            .expect("delivery recipient fixture operation")
    });
    let connections = Connections(Arc::new(BTreeMap::from([(
        (i.tenant.clone(), "oms".into()),
        (format!("http://{address}/events"), "token".into()),
    )])));
    assert!(connections
        .dispatch_one(&store)
        .await
        .expect("delivery recipient fixture operation"));
    store
        .retry(&i, oid)
        .await
        .expect("delivery recipient fixture operation");
    assert!(connections
        .dispatch_one(&store)
        .await
        .expect("delivery recipient fixture operation"));
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    let item = store
        .outbox(&i.tenant, "wh-main")
        .await
        .expect("delivery recipient fixture operation")[0]
        .clone();
    assert_eq!(item["status"], "dead_letter");
    assert!(item["last_error"]
        .as_str()
        .expect("string evidence")
        .contains("unknown"));
    let attempts: i64 =
        sqlx::query_scalar("SELECT count(*) FROM wms_delivery_attempts WHERE outbox_id=$1")
            .bind(oid)
            .fetch_one(&store.pool)
            .await
            .expect("delivery recipient fixture operation");
    assert_eq!(
        attempts, 0,
        "a configuration hold creates no invented delivery attempt"
    );
    server.abort();
}

#[tokio::test]
#[ignore = "requires isolated Postgres; scripts/check.sh --postgres"]
async fn delivery_recipient_migration_preserves_history_and_fences_identity_changes() {
    let (store, _, _) = setup().await;
    let mut tx = store
        .pool
        .begin()
        .await
        .expect("delivery recipient fixture operation");
    let schema = format!("recipient_upgrade_{}", id().replace('-', ""));
    sqlx::raw_sql(&format!(
        "CREATE SCHEMA {schema}; SET LOCAL search_path TO {schema}"
    ))
    .execute(&mut *tx)
    .await
    .expect("delivery recipient fixture operation");
    // Upgrade the actual preceding seven-migration schema, including an event
    // with a reset retry counter whose retained attempt still proves a send.
    for migration in [
        include_str!("../migrations/001_initial.sql"),
        include_str!("../migrations/002_relational_execution.sql"),
        include_str!("../migrations/003_native_operations.sql"),
        include_str!("../migrations/004_native_source_ownership.sql"),
        include_str!("../migrations/005_conditional_oms_fulfillment.sql"),
        include_str!("../migrations/006_oms_rejections.sql"),
        include_str!("../migrations/007_operations_indexes.sql"),
    ] {
        sqlx::raw_sql(migration)
            .execute(&mut *tx)
            .await
            .expect("delivery recipient fixture operation");
    }
    sqlx::raw_sql("INSERT INTO wms_events(id,tenant,warehouse_id,version,payload) VALUES('event','tenant','warehouse',1,'{}');
        INSERT INTO wms_outbox(id,event_id,tenant,destination,attempts,status) VALUES
        ('never','event','tenant','oms',0,'pending'),
        ('counter','event','tenant','oms',2,'delivering'),
        ('history','event','tenant','oms',0,'pending'),
        ('done','event','tenant','oms',1,'delivered');
        INSERT INTO wms_delivery_attempts(id,outbox_id,outcome,detail) VALUES('old','history','uncertain','original detail')")
        .execute(&mut *tx).await.expect("delivery recipient fixture operation");
    sqlx::raw_sql(include_str!("../migrations/008_delivery_recipient.sql"))
        .execute(&mut *tx)
        .await
        .expect("delivery recipient fixture operation");
    let rows: Vec<(String, Option<String>)> =
        sqlx::query_as("SELECT id,recipient_fingerprint FROM wms_outbox ORDER BY id")
            .fetch_all(&mut *tx)
            .await
            .expect("delivery recipient fixture operation");
    assert_eq!(
        rows,
        vec![
            ("counter".into(), Some("unverified".into())),
            ("done".into(), Some("unverified".into())),
            ("history".into(), Some("unverified".into())),
            ("never".into(), None)
        ]
    );
    let history: (String, String) =
        sqlx::query_as("SELECT outcome,detail FROM wms_delivery_attempts WHERE id='old'")
            .fetch_one(&mut *tx)
            .await
            .expect("delivery recipient fixture operation");
    assert_eq!(history, ("uncertain".into(), "original detail".into()));
    let status: String = sqlx::query_scalar("SELECT status FROM wms_outbox WHERE id='done'")
        .fetch_one(&mut *tx)
        .await
        .expect("delivery recipient fixture operation");
    assert_eq!(status, "delivered");
    sqlx::query("UPDATE wms_outbox SET recipient_fingerprint=$1 WHERE id='never'")
        .bind("a".repeat(64))
        .execute(&mut *tx)
        .await
        .expect("delivery recipient fixture operation");
    for change in [
        "recipient_fingerprint=NULL",
        "recipient_fingerprint=repeat('b',64)",
        "tenant='other'",
        "destination='tms'",
        "event_id='different'",
        "status='delivering',attempts=attempts+1,lease_token='legacy-worker',lease_until=now()+interval '30 seconds'",
    ] {
        sqlx::query("SAVEPOINT guard_probe")
            .execute(&mut *tx)
            .await
            .expect("delivery recipient fixture operation");
        let error = sqlx::query(&format!("UPDATE wms_outbox SET {change} WHERE id='never'"))
            .execute(&mut *tx)
            .await
            .expect_err("direct SQL must not change original delivery identity");
        assert_eq!(
            error.as_database_error().and_then(|e| e.code()).as_deref(),
            Some("23514")
        );
        sqlx::query("ROLLBACK TO SAVEPOINT guard_probe")
            .execute(&mut *tx)
            .await
            .expect("delivery recipient fixture operation");
    }
    for (assertion, allowed) in [("b".repeat(64), false), ("a".repeat(64), true)] {
        sqlx::query("SAVEPOINT receiver_assertion")
            .execute(&mut *tx)
            .await
            .expect("receiver assertion savepoint");
        sqlx::query("SELECT set_config('wms.delivery_recipient',$1,true)")
            .bind(assertion)
            .execute(&mut *tx)
            .await
            .expect("transaction-local receiver assertion");
        let claim = sqlx::query("UPDATE wms_outbox SET status='delivering',attempts=attempts+1,lease_token='current-worker',lease_until=now()+interval '30 seconds' WHERE id='never'")
            .execute(&mut *tx).await;
        if allowed {
            assert_eq!(
                claim.expect("matching receiver may claim").rows_affected(),
                1
            );
        } else {
            let error = claim.expect_err("different receiver cannot claim");
            assert_eq!(
                error.as_database_error().and_then(|e| e.code()).as_deref(),
                Some("23514")
            );
        }
        sqlx::query("ROLLBACK TO SAVEPOINT receiver_assertion")
            .execute(&mut *tx)
            .await
            .expect("discard assertion and claim probe");
    }
    sqlx::query("UPDATE wms_outbox SET status='pending',attempts=0 WHERE id='history'")
        .execute(&mut *tx)
        .await
        .expect("delivery recipient fixture operation");
    let pin: String =
        sqlx::query_scalar("SELECT recipient_fingerprint FROM wms_outbox WHERE id='history'")
            .fetch_one(&mut *tx)
            .await
            .expect("delivery recipient fixture operation");
    assert_eq!(pin, "unverified");
    tx.rollback()
        .await
        .expect("delivery recipient fixture operation");
}

#[tokio::test]
#[ignore = "requires isolated Postgres; scripts/check.sh --postgres"]
async fn lot_controls_persist_hold_independence_and_fence_scoped_forward_work_without_mutating_custody(
) {
    let (store, i, _) = setup().await;
    let original = req(
        store
            .state(&i.tenant, "wh-main")
            .await
            .expect("lot control fixture and original evidence")
            .version,
        Command::RecordLotExpiry {
            sku: "SKU".into(),
            lot: "LOT".into(),
            expires_at: "2099-10-21T05:00:00-07:00".into(),
            reason: "Read original manufacturer label".into(),
        },
    );
    let recorded = store
        .execute(&i, "wh-main", &original)
        .await
        .expect("lot control fixture and original evidence");
    let order = execute(&store, &i, create_order("LOT-CONTROL", 4)).await["entity_id"]
        .as_str()
        .expect("lot control fixture and original evidence")
        .to_string();
    execute(
        &store,
        &i,
        Command::Allocate {
            order_id: order.clone(),
        },
    )
    .await;
    let before = store
        .state(&i.tenant, "wh-main")
        .await
        .expect("lot control fixture and original evidence");
    assert_eq!(
        before.lot_controls[0].expires_at.as_deref(),
        Some("2099-10-21T12:00:00+00:00")
    );
    let pick = Command::Pick {
        order_id: order.clone(),
        line_id: "1".into(),
        location: "A".into(),
        lot: "LOT".into(),
        quantity: 1,
    };
    let mut old_pick = req(before.version, pick.clone());
    old_pick.resource_versions = stateset_warehouse::concurrency::reviewed_versions(&before, &pick)
        .expect("lot control fixture and original evidence");
    let hold1 = execute(
        &store,
        &i,
        Command::QuarantineLot {
            sku: "SKU".into(),
            lot: "LOT".into(),
            reason: "Damaged label under investigation".into(),
        },
    )
    .await["entity_id"]
        .as_str()
        .expect("lot control fixture and original evidence")
        .to_string();
    let hold2 = execute(
        &store,
        &i,
        Command::QuarantineLot {
            sku: "SKU".into(),
            lot: "LOT".into(),
            reason: "Separate temperature excursion".into(),
        },
    )
    .await["entity_id"]
        .as_str()
        .expect("lot control fixture and original evidence")
        .to_string();
    execute(
        &store,
        &i,
        Command::ReleaseLotHold {
            hold_id: hold1.clone(),
            reason: "Label independently verified".into(),
        },
    )
    .await;
    let held = store
        .state(&i.tenant, "wh-main")
        .await
        .expect("lot control fixture and original evidence");
    assert_eq!(held.stocks, before.stocks);
    assert_eq!(held.lot_controls[0].holds.len(), 1);
    assert_eq!(held.lot_controls[0].holds[0].id, hold2);
    let denied = store
        .execute(&i, "wh-main", &req(held.version, pick.clone()))
        .await
        .expect_err("lot control must reject this action");
    assert_eq!(denied.status, 422);
    assert!(denied.message.contains("Quarantined"));
    let operator = Identity {
        role: "operator".into(),
        ..i.clone()
    };
    assert_eq!(
        store
            .execute(
                &operator,
                "wh-main",
                &req(
                    held.version,
                    Command::ReleaseLotHold {
                        hold_id: hold2.clone(),
                        reason: "Unauthorized release".into()
                    }
                )
            )
            .await
            .expect_err("lot control must reject this action")
            .status,
        403
    );
    execute(
        &store,
        &i,
        Command::ReleaseLotHold {
            hold_id: hold2,
            reason: "Temperature review complete".into(),
        },
    )
    .await;
    assert_eq!(
        store
            .execute(&i, "wh-main", &old_pick)
            .await
            .expect_err("lot control must reject this action")
            .status,
        409,
        "a hold/release ABA invalidates the old scan"
    );
    execute(&store, &i, pick).await;
    execute(
        &store,
        &i,
        Command::CorrectLotExpiry {
            sku: "SKU".into(),
            lot: "LOT".into(),
            expires_at: Some("2000-01-01T00:00:00Z".into()),
            reason: "Correct transcription against original label".into(),
        },
    )
    .await;
    assert_eq!(
        store
            .execute(&i, "wh-main", &original)
            .await
            .expect("lot control fixture and original evidence")["result"],
        recorded["result"],
        "replay retains original time and result after correction"
    );
    let now = store
        .state(&i.tenant, "wh-main")
        .await
        .expect("lot control fixture and original evidence");
    assert_eq!(
        now.orders
            .iter()
            .find(|o| o.id == order)
            .expect("lot control fixture and original evidence")
            .allocations[0]
            .picked,
        1
    );
    assert_eq!(
        stateset_warehouse::lot_control::availability_at(&now, "SKU", "LOT", chrono::Utc::now()),
        stateset_warehouse::lot_control::Availability::Expired
    );
    execute(
        &store,
        &i,
        Command::Unpick {
            order_id: order.clone(),
            line_id: "1".into(),
            location: "A".into(),
            lot: "LOT".into(),
            quantity: 1,
            reason: "Actually returned expired unit".into(),
        },
    )
    .await;
    execute(
        &store,
        &i,
        Command::ResetAllocation {
            order_id: order,
            reason: "Release remaining unpicked expired stock".into(),
        },
    )
    .await;
    let proof = store
        .reconciliation(&i.tenant, "wh-main")
        .await
        .expect("lot control fixture and original evidence");
    assert_eq!(
        proof["ok"], true,
        "original eligibility must be proved at its original version/time: {proof}"
    );
    assert_eq!(proof["lot_controls"]["policy_changes"], 6);
    assert_eq!(proof["lot_controls"]["eligibility_decisions"], 2);
}

#[tokio::test]
#[ignore = "requires isolated Postgres; scripts/check.sh --postgres"]
async fn lot_required_missing_labels_and_fefo_work_in_full_and_selective_database_snapshots() {
    let (store, i, _) = setup().await;
    execute(
        &store,
        &i,
        Command::SetProductExpiryPolicy {
            sku: "SKU".into(),
            required: true,
            reason: "Manufacturer requires exact expiry label".into(),
        },
    )
    .await;
    let order = execute(&store, &i, create_order("LOT-FEFO", 4)).await["entity_id"]
        .as_str()
        .expect("lot control fixture and original evidence")
        .to_string();
    let state = store
        .state(&i.tenant, "wh-main")
        .await
        .expect("lot control fixture and original evidence");
    let allocate = Command::Allocate {
        order_id: order.clone(),
    };
    assert_eq!(
        store
            .execute(&i, "wh-main", &req(state.version, allocate.clone()))
            .await
            .expect_err("lot control must reject this action")
            .status,
        422
    );
    for command in [
        Command::CountStock {
            sku: "SKU".into(),
            lot: "EARLY".into(),
            location: "B".into(),
            counted: 3,
            reason: "Observed another labelled lot".into(),
        },
        Command::RecordLotExpiry {
            sku: "SKU".into(),
            lot: "LOT".into(),
            expires_at: "2099-10-22T00:00:00Z".into(),
            reason: "Read original late label".into(),
        },
        Command::RecordLotExpiry {
            sku: "SKU".into(),
            lot: "EARLY".into(),
            expires_at: "2099-10-21T00:00:00Z".into(),
            reason: "Read original early label".into(),
        },
    ] {
        execute(&store, &i, command).await;
    }
    let state = store
        .state(&i.tenant, "wh-main")
        .await
        .expect("lot control fixture and original evidence");
    let mut scoped = req(state.version, allocate.clone());
    scoped.resource_versions =
        stateset_warehouse::concurrency::reviewed_versions(&state, &allocate)
            .expect("lot control fixture and original evidence");
    store
        .execute(&i, "wh-main", &scoped)
        .await
        .expect("lot control fixture and original evidence");
    let state = store
        .state(&i.tenant, "wh-main")
        .await
        .expect("lot control fixture and original evidence");
    let allocations = &state
        .orders
        .iter()
        .find(|o| o.id == order)
        .expect("lot control fixture and original evidence")
        .allocations;
    assert_eq!(
        allocations
            .iter()
            .find(|a| a.lot == "EARLY")
            .expect("lot control fixture and original evidence")
            .quantity,
        3
    );
    assert_eq!(
        allocations
            .iter()
            .find(|a| a.lot == "LOT")
            .expect("lot control fixture and original evidence")
            .quantity,
        1
    );
    assert_eq!(
        store
            .reconciliation(&i.tenant, "wh-main")
            .await
            .expect("lot control fixture and original evidence")["ok"],
        true
    );
}

#[tokio::test]
#[ignore = "requires isolated Postgres; scripts/check.sh --postgres"]
async fn lot_database_guards_reject_unattributed_policy_and_direct_held_pick() {
    let (store, i, _) = setup().await;
    let forged=sqlx::query("UPDATE wms_products SET data=data||'{\"expiry_required\":true}'::jsonb WHERE tenant=$1 AND warehouse_id='wh-main' AND sku='SKU'").bind(&i.tenant).execute(&store.pool).await;
    assert!(
        forged.is_err(),
        "product JSON cannot grant its own expiry policy without a source"
    );
    let order = execute(&store, &i, create_order("LOT-SQL", 2)).await["entity_id"]
        .as_str()
        .expect("lot control fixture and original evidence")
        .to_string();
    execute(
        &store,
        &i,
        Command::Allocate {
            order_id: order.clone(),
        },
    )
    .await;
    execute(
        &store,
        &i,
        Command::QuarantineLot {
            sku: "SKU".into(),
            lot: "LOT".into(),
            reason: "Actual quality concern".into(),
        },
    )
    .await;
    let before = store
        .state(&i.tenant, "wh-main")
        .await
        .expect("lot control fixture and original evidence");
    let direct=sqlx::query("UPDATE wms_allocations SET picked=1 WHERE tenant=$1 AND warehouse_id='wh-main' AND order_id=$2").bind(&i.tenant).bind(&order).execute(&store.pool).await;
    assert!(
        direct.is_err(),
        "a direct SQL pick must pass the same current quality check"
    );
    let immutable=sqlx::query("UPDATE wms_lot_changes SET document=document||'{\"reason\":\"rewritten\"}'::jsonb WHERE tenant=$1").bind(&i.tenant).execute(&store.pool).await;
    assert!(
        immutable.is_err(),
        "original attributed policy evidence is immutable"
    );
    let after = store
        .state(&i.tenant, "wh-main")
        .await
        .expect("lot control fixture and original evidence");
    assert_eq!(before.stocks, after.stocks);
    assert_eq!(before.orders, after.orders);
    assert_eq!(
        store
            .reconciliation(&i.tenant, "wh-main")
            .await
            .expect("lot control fixture and original evidence")["ok"],
        true
    );
}

#[tokio::test]
#[ignore = "requires isolated Postgres; scripts/check.sh --postgres"]
async fn lot_metadata_and_revisions_roll_back_with_the_final_delivery_and_exact_retry_records_one_hold(
) {
    let (store, i, before) = setup().await;
    let suffix = id().replace('-', "");
    let function = format!("wms_lot_fault_{suffix}");
    let trigger = format!("wms_lot_fault_trigger_{suffix}");
    sqlx::raw_sql(&format!("CREATE FUNCTION {function}() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.tenant='{}' THEN RAISE EXCEPTION 'injected final lot outbox failure'; END IF; RETURN NEW; END $$; CREATE TRIGGER {trigger} BEFORE INSERT ON wms_outbox FOR EACH ROW EXECUTE FUNCTION {function}();",i.tenant))
        .execute(&store.pool).await.expect("install isolated tenant fault");
    let request = req(
        before.version,
        Command::QuarantineLot {
            sku: "SKU".into(),
            lot: "LOT".into(),
            reason: "Observed quality issue".into(),
        },
    );
    let failed = store.execute(&i, "wh-main", &request).await;
    sqlx::raw_sql(&format!(
        "DROP TRIGGER {trigger} ON wms_outbox; DROP FUNCTION {function}();"
    ))
    .execute(&store.pool)
    .await
    .expect("remove isolated fault");
    assert!(failed.is_err());
    assert_eq!(
        store
            .state(&i.tenant, "wh-main")
            .await
            .expect("rolled back state"),
        before
    );
    assert_eq!(counts(&store, &i.tenant).await, (0, 0, 0));
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM wms_lot_changes WHERE tenant=$1")
        .bind(&i.tenant)
        .fetch_one(&store.pool)
        .await
        .expect("original policy count");
    assert_eq!(n, 0);
    let accepted = store
        .execute(&i, "wh-main", &request)
        .await
        .expect("original retry after rollback");
    let replay = store
        .execute(&i, "wh-main", &request)
        .await
        .expect("exact committed replay");
    assert_eq!(accepted["result"], replay["result"]);
    let after = store
        .state(&i.tenant, "wh-main")
        .await
        .expect("one committed hold");
    assert_eq!(after.lot_controls[0].holds.len(), 1);
    assert_eq!(counts(&store, &i.tenant).await, (1, 1, 1));
}

#[tokio::test]
#[ignore = "requires isolated Postgres; scripts/check.sh --postgres"]
async fn lot_forensic_proofs_detect_missing_decisions_and_rewritten_policy_even_when_custody_balances(
) {
    let (store, i, _) = setup().await;
    execute(
        &store,
        &i,
        Command::RecordLotExpiry {
            sku: "SKU".into(),
            lot: "LOT".into(),
            expires_at: "2099-01-01T00:00:00Z".into(),
            reason: "Original exact label".into(),
        },
    )
    .await;
    let order = execute(&store, &i, create_order("LOT-FORENSIC", 2)).await["entity_id"]
        .as_str()
        .expect("order")
        .to_string();
    execute(
        &store,
        &i,
        Command::Allocate {
            order_id: order.clone(),
        },
    )
    .await;
    let mut tx = store
        .pool
        .begin()
        .await
        .expect("isolated forensic transaction");
    sqlx::query("SET LOCAL TIME ZONE 'Pacific/Auckland'")
        .execute(&mut *tx)
        .await
        .expect("different database session timezone");
    let timezone_valid: bool = sqlx::query_scalar("SELECT bool_and(wms_lot_event_valid(tenant,warehouse_id,version)) FROM wms_events WHERE tenant=$1")
        .bind(&i.tenant).fetch_one(&mut *tx).await.expect("original proofs in another timezone");
    assert!(
        timezone_valid,
        "eligibility evidence must retain canonical UTC across sessions and restores"
    );
    // Privileged corruption is deliberately injected only inside a rolled-back
    // test transaction. The independent verifier must not trust trigger success.
    sqlx::query("ALTER TABLE wms_lot_decisions DISABLE TRIGGER USER")
        .execute(&mut *tx)
        .await
        .expect("inject privileged missing evidence");
    let version: i64 =
        sqlx::query_scalar("DELETE FROM wms_lot_decisions WHERE tenant=$1 RETURNING version")
            .bind(&i.tenant)
            .fetch_one(&mut *tx)
            .await
            .expect("remove original decision");
    let valid: bool = sqlx::query_scalar("SELECT wms_lot_event_valid($1,'wh-main',$2)")
        .bind(&i.tenant)
        .bind(version)
        .fetch_one(&mut *tx)
        .await
        .expect("independent missing-row proof");
    assert!(
        !valid,
        "balanced custody cannot substitute for the missing eligibility decision"
    );
    let order_valid: bool = sqlx::query_scalar("SELECT wms_lot_order_valid($1,'wh-main',$2)")
        .bind(&i.tenant)
        .bind(&order)
        .fetch_one(&mut *tx)
        .await
        .expect("later forward admission check");
    assert!(
        !order_valid,
        "new forward work must retain its original quality evidence"
    );
    tx.rollback()
        .await
        .expect("restore immutable original decisions");
    let mut tx = store
        .pool
        .begin()
        .await
        .expect("isolated forensic transaction");
    sqlx::query("ALTER TABLE wms_lot_changes DISABLE TRIGGER USER")
        .execute(&mut *tx)
        .await
        .expect("inject privileged rewrite");
    let version: i64=sqlx::query_scalar("UPDATE wms_lot_changes SET document=document||'{\"expires_at\":\"2100-01-01T00:00:00+00:00\"}'::jsonb WHERE tenant=$1 RETURNING version").bind(&i.tenant).fetch_one(&mut *tx).await.expect("rewrite without touching stock");
    let valid: bool = sqlx::query_scalar("SELECT wms_lot_change_valid($1,'wh-main',$2,0)")
        .bind(&i.tenant)
        .bind(version)
        .fetch_one(&mut *tx)
        .await
        .expect("independent original source proof");
    assert!(
        !valid,
        "a cached label cannot replace its immutable original command/event"
    );
    tx.rollback()
        .await
        .expect("restore immutable original label");
    let mut tx = store
        .pool
        .begin()
        .await
        .expect("isolated original-coordinate corruption");
    sqlx::query("ALTER TABLE wms_events DISABLE TRIGGER USER")
        .execute(&mut *tx)
        .await
        .expect("inject privileged source rewrite");
    let version: i64=sqlx::query_scalar("UPDATE wms_events SET payload=payload||'{\"id\":\"forged-other-original\"}'::jsonb WHERE tenant=$1 AND payload#>>'{command,type}'='allocate' RETURNING version").bind(&i.tenant).fetch_one(&mut *tx).await.expect("rewrite source id without moving stock");
    let valid: bool = sqlx::query_scalar(
        "SELECT wms_lot_decision_valid($1,'wh-main',$2,'allocation','SKU','LOT')",
    )
    .bind(&i.tenant)
    .bind(version)
    .fetch_one(&mut *tx)
    .await
    .expect("independent original-coordinate proof");
    assert!(
        !valid,
        "matching quantities cannot replace the original source event identity"
    );
    tx.rollback().await.expect("restore original source event");
    assert_eq!(
        store
            .reconciliation(&i.tenant, "wh-main")
            .await
            .expect("original reconciliation")["ok"],
        true
    );
}

#[tokio::test]
#[ignore = "requires isolated Postgres; scripts/check.sh --postgres"]
async fn lot_original_eligibility_rejects_a_forged_event_type_with_unchanged_command_and_custody() {
    let (store, i, _) = setup().await;
    let order = execute(&store, &i, create_order("LOT-EVENT-KIND", 2)).await["entity_id"]
        .as_str()
        .expect("original order")
        .to_string();
    execute(&store, &i, Command::Allocate { order_id: order }).await;
    let mut tx = store
        .pool
        .begin()
        .await
        .expect("owned forensic transaction");
    sqlx::query("ALTER TABLE wms_events DISABLE TRIGGER USER")
        .execute(&mut *tx)
        .await
        .expect("explicit privileged corruption inside rollback");
    let version: i64=sqlx::query_scalar("UPDATE wms_events SET payload=payload||'{\"type\":\"fulfillment.shipped\"}'::jsonb WHERE tenant=$1 AND payload#>>'{command,type}'='allocate' RETURNING version").bind(&i.tenant).fetch_one(&mut *tx).await.expect("rewrite event kind without changing quantities or original command");
    let valid: bool = sqlx::query_scalar(
        "SELECT wms_lot_decision_valid($1,'wh-main',$2,'allocation','SKU','LOT')",
    )
    .bind(&i.tenant)
    .bind(version)
    .fetch_one(&mut *tx)
    .await
    .expect("independent original semantic event proof");
    tx.rollback()
        .await
        .expect("restore original event and all trigger guards before assertion");
    assert!(!valid,"eligibility evidence must bind its original semantic event type, not just its command and unchanged balances");
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn startup_refuses_a_database_migrated_by_a_newer_release() {
    use sqlx::{postgres::PgConnectOptions, Connection};
    let url = std::env::var("WMS_TEST_DATABASE_URL").expect("explicit isolated test server");
    let mut target = reqwest::Url::parse(&url).expect("test database URI");
    assert!(target.path().contains("test"));
    let name = format!(
        "wms_newer_schema_test_{}",
        stateset_warehouse::domain::id().replace('-', "")
    );
    assert!(name.len() <= 63);
    let options: PgConnectOptions = url.parse().expect("test server options");
    let mut admin = sqlx::PgConnection::connect_with(&options.clone().database("postgres"))
        .await
        .expect("owned test database administrator");
    sqlx::query(&format!(
        "CREATE DATABASE {name} TEMPLATE template0 ENCODING 'UTF8'"
    ))
    .execute(&mut admin)
    .await
    .expect("fresh owned database");
    target.set_path(&format!("/{name}"));
    let store = stateset_warehouse::store::Store::connect(target.as_str())
        .await
        .expect("current release migrates an empty database");
    sqlx::query("INSERT INTO wms_schema_migrations(name,digest) VALUES('999_from_a_newer_release','synthetic')")
        .execute(&store.pool)
        .await
        .expect("synthetic newer ledger row");
    let before: i64 = sqlx::query_scalar("SELECT count(*) FROM wms_schema_migrations")
        .fetch_one(&store.pool)
        .await
        .expect("ledger count");
    store.pool.close().await;
    let refused = stateset_warehouse::store::Store::connect(target.as_str()).await;
    let error = refused
        .err()
        .expect("an older binary must refuse a newer schema");
    assert_eq!(error.status, 503);
    assert!(
        error.message.contains("999_from_a_newer_release"),
        "{}",
        error.message
    );
    // Nothing was applied or recorded by the refused start.
    let mut check = sqlx::PgConnection::connect(target.as_str())
        .await
        .expect("inspect");
    let after: i64 = sqlx::query_scalar("SELECT count(*) FROM wms_schema_migrations")
        .fetch_one(&mut check)
        .await
        .expect("ledger count");
    assert_eq!(after, before);
    check.close().await.expect("closed");
    sqlx::query(
        "SELECT pg_terminate_backend(pid) FROM pg_stat_activity WHERE datname=$1 AND pid<>pg_backend_pid()",
    )
    .bind(&name)
    .execute(&mut admin)
    .await
    .expect("end leftover sessions on the owned database");
    sqlx::query(&format!("DROP DATABASE {name}"))
        .execute(&mut admin)
        .await
        .expect("remove owned database");
}
