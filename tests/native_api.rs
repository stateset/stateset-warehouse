use axum::{
    body::Body,
    extract::State as HttpState,
    http::{HeaderMap, Request as HttpRequest, StatusCode},
    response::IntoResponse,
    routing::{get, post},
    Extension, Json, Router,
};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use stateset_warehouse::{
    api::{self, App},
    domain::{self, Command, LineInput},
    integrations::Connections,
    native::{NativeIntegrations, PreparedTransport, TransportPlan},
    native_api,
    native_store::{self, Claim, Freeze},
    store::{Identity, Request, Store},
};
use std::sync::Arc;
use tower::ServiceExt;

fn app(role: &str, demo: bool) -> axum::Router {
    let pool = sqlx::postgres::PgPoolOptions::new()
        .connect_lazy("postgres://unused:unused@127.0.0.1:1/unused")
        .expect("lazy configuration");
    native_api::router()
        .layer(Extension(Identity {
            tenant: "native-test".into(),
            actor: "fixture:actor".into(),
            role: role.into(),
        }))
        .with_state(App {
            store: Store { pool },
            credentials: Arc::new(Vec::new()),
            demo,
            connections: Connections::default(),
        })
}
fn import_body() -> Value {
    json!({"request_id":"native-request-1","expected_version":0,"oms_order_id":"00000000-0000-4000-8000-000000000999","source_updated_at":"2026-10-11T12:00:00Z","details":{"customer":"Synthetic customer","destination":"Verified destination","priority":"normal"}})
}
async fn call(app: axum::Router, method: &str, path: &str, body: Value) -> (StatusCode, Value) {
    let response = app
        .oneshot(
            HttpRequest::builder()
                .method(method)
                .uri(path)
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .expect("HTTP request"),
        )
        .await
        .expect("HTTP response");
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
#[tokio::test]
async fn viewers_cannot_trigger_native_http_or_read_sensitive_native_receipts() {
    for (method, path, body) in [
        ("GET", "/v1/integrations/oms/orders", Value::Null),
        ("POST", "/v1/warehouses/wh-main/imports/oms", import_body()),
        (
            "GET",
            "/v1/warehouses/wh-main/orders/order-1/readiness",
            Value::Null,
        ),
        (
            "GET",
            "/v1/warehouses/wh-main/native-requests/request-1234",
            Value::Null,
        ),
        (
            "GET",
            "/v1/warehouses/wh-main/orders/order-1/oms-fulfillment",
            Value::Null,
        ),
    ] {
        assert_eq!(
            call(app("viewer", false), method, path, body).await.0,
            StatusCode::FORBIDDEN,
            "{path}"
        );
    }
}
#[tokio::test]
async fn native_http_is_disabled_for_synthetic_demo_even_with_admin_role() {
    assert_eq!(
        call(
            app("admin", true),
            "GET",
            "/v1/integrations/oms/orders",
            Value::Null
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    assert_eq!(
        call(
            app("admin", true),
            "POST",
            "/v1/warehouses/wh-main/imports/oms",
            import_body()
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    // Source-bound OMS credentials can use import, but cannot bypass demo rules.
    assert_eq!(
        call(
            app("oms", true),
            "POST",
            "/v1/warehouses/wh-main/imports/oms",
            import_body()
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
}
#[tokio::test]
async fn malformed_native_input_is_rejected_before_any_database_or_upstream_connection() {
    let mut cases = Vec::new();
    let mut body = import_body();
    body["actor"] = json!("spoofed");
    cases.push(body);
    let mut body = import_body();
    body["details"]["source_order_id"] = json!("spoofed");
    cases.push(body);
    let mut body = import_body();
    body["source_updated_at"] = json!("not-a-timestamp");
    cases.push(body);
    let mut body = import_body();
    body["request_id"] = json!("bad/id");
    cases.push(body);
    let mut body = import_body();
    body["oms_order_id"] = json!("../another-tenant");
    cases.push(body);
    let mut body = import_body();
    body["expected_version"] = json!(-1);
    cases.push(body);
    for body in cases {
        assert_eq!(
            call(
                app("admin", false),
                "POST",
                "/v1/warehouses/wh-main/imports/oms",
                body
            )
            .await
            .0,
            StatusCode::UNPROCESSABLE_ENTITY
        );
    }
    assert_eq!(
        call(
            app("admin", false),
            "GET",
            "/v1/integrations/oms/orders?page=0",
            Value::Null
        )
        .await
        .0,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    assert_eq!(
        call(
            app("admin", false),
            "GET",
            "/v1/integrations/oms/orders?tenant=victim",
            Value::Null
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
}
#[test]
fn native_request_hash_binds_operation_order_path_and_complete_input() {
    let input = import_body();
    let a = native_store::digest("oms_import", "wh-a", None, &input).expect("digest");
    assert_ne!(
        a,
        native_store::digest("oms_import", "wh-b", None, &input).expect("other warehouse")
    );
    assert_ne!(
        a,
        native_store::digest("tms_transport", "wh-a", Some("order-a"), &input)
            .expect("other operation")
    );
    let mut changed = input.clone();
    changed["expected_version"] = json!(1);
    assert_ne!(
        a,
        native_store::digest("oms_import", "wh-a", None, &changed).expect("changed input")
    );
    assert_ne!(
        native_store::import_command_id("tenant-a", "wh-a", "request-1"),
        native_store::import_command_id("tenant-b", "wh-a", "request-1")
    );
}

async fn postgres() -> (Store, Identity) {
    let url = std::env::var("WMS_TEST_DATABASE_URL").expect("isolated WMS_TEST_DATABASE_URL");
    assert!(
        url.to_ascii_lowercase().contains("test"),
        "database name must contain test"
    );
    let store = Store::connect(&url).await.expect("normalized test store");
    sqlx::raw_sql(include_str!("../migrations/003_native_operations.sql"))
        .execute(&store.pool)
        .await
        .expect("native schema");
    sqlx::raw_sql(include_str!(
        "../migrations/004_native_source_ownership.sql"
    ))
    .execute(&store.pool)
    .await
    .expect("native source ownership schema");
    let i = Identity {
        tenant: format!("native-{}", domain::id()),
        actor: "fixture:native-operator".into(),
        role: "admin".into(),
    };
    store
        .provision(&i.tenant, false)
        .await
        .expect("test warehouse");
    (store, i)
}
async fn execute(store: &Store, i: &Identity, command: Command) -> Value {
    let state = store
        .state(&i.tenant, "wh-main")
        .await
        .expect("current state");
    store
        .execute(
            i,
            "wh-main",
            &Request {
                request_id: domain::id(),
                expected_version: state.version,
                command,
            },
        )
        .await
        .expect("fixture command")["result"]
        .clone()
}
fn plan() -> TransportPlan {
    serde_json::from_value(json!({"reference":"FR-NATIVE-TEST","customer_id":"c1","mode":"FTL","equipment":"Dry van","commodity":"Synthetic goods","weight_lbs":1000,"miles":180,"origin":{"facility":"Origin","city":"Portland","state":"OR","date":"2026-10-20","address_lines":["15 Dock Street"],"postal_code":"97201","country":"US","contact":"Dock","phone":"555-0100","instructions":""},"destination":{"facility":"Destination","city":"Seattle","state":"WA","date":"2026-10-21","address_lines":["25 Receiving Ave"],"postal_code":"98101","country":"US","contact":"Receiving","phone":"555-0101","instructions":""},"buy_cents":100000,"sell_cents":120000})).expect("source-derived transport plan")
}
async fn packed(store: &Store, i: &Identity) -> (domain::State, String, PreparedTransport) {
    for command in [
        Command::CreateProduct {
            sku: "SKU".into(),
            name: "Synthetic SKU".into(),
            category: "General".into(),
            reorder_point: 0,
        },
        Command::CreateLocation {
            code: "A".into(),
            zone: "Storage".into(),
            kind: "storage".into(),
        },
        Command::CountStock {
            sku: "SKU".into(),
            lot: "LOT".into(),
            location: "A".into(),
            counted: 10,
            reason: "Synthetic opening count".into(),
        },
    ] {
        execute(store, i, command).await;
    }
    let result = execute(
        store,
        i,
        Command::CreateOrder {
            reference: "SO-NATIVE".into(),
            source_order_id: Some("oms:oms-acme:00000000-0000-4000-8000-000000000999".into()),
            customer: "Synthetic customer".into(),
            destination: "Verified destination".into(),
            priority: "normal".into(),
            lines: vec![LineInput {
                id: "line-1".into(),
                sku: "SKU".into(),
                quantity: 2,
            }],
        },
    )
    .await;
    let id = result["entity_id"]
        .as_str()
        .expect("order identity")
        .to_string();
    execute(
        store,
        i,
        Command::Allocate {
            order_id: id.clone(),
        },
    )
    .await;
    execute(
        store,
        i,
        Command::Pick {
            order_id: id.clone(),
            line_id: "line-1".into(),
            location: "A".into(),
            lot: "LOT".into(),
            quantity: 2,
        },
    )
    .await;
    execute(
        store,
        i,
        Command::Pack {
            order_id: id.clone(),
        },
    )
    .await;
    let state = store
        .state(&i.tenant, "wh-main")
        .await
        .expect("packed state");
    let c=NativeIntegrations::from_json(&json!({(i.tenant.clone()):{"oms":{"base_url":"http://localhost:1","tenant_id":"oms-acme","api_key":"fixture-oms-key"},"tms":{"base_url":"http://localhost:1","tenant_id":"freight-acme","bearer_token":"fixture-tms-key"}}}).to_string()).expect("fixture mapping");
    let prepared = c
        .prepare_transport(&i.tenant, &state, &id, &plan())
        .expect("prepared transport");
    (state, id, prepared)
}
#[tokio::test]
#[ignore = "requires isolated Postgres; WMS_TEST_DATABASE_URL"]
async fn durable_transport_intents_enforce_one_order_and_one_sender_then_recover_uncertainty() {
    let (store, i) = postgres().await;
    let (state, id, prepared) = packed(&store, &i).await;
    let input =
        json!({"request_id":"native-journal-1","expected_version":state.version,"plan":plan()});
    let digest =
        native_store::digest("tms_transport", "wh-main", Some(&id), &input).expect("digest");
    let frozen = json!(prepared);
    let (record, replayed) = native_store::freeze(
        &store,
        &i,
        "wh-main",
        Freeze {
            kind: "tms_transport",
            request_id: "native-journal-1",
            digest: &digest,
            input: &input,
            expected_version: state.version,
            order_id: Some(&id),
            source_snapshot: None,
            frozen: &frozen,
        },
    )
    .await
    .expect("durable frozen plan");
    assert!(!replayed);
    assert_eq!(record.status, "frozen");
    let conflict = native_store::freeze(
        &store,
        &i,
        "wh-main",
        Freeze {
            kind: "tms_transport",
            request_id: "native-journal-2",
            digest: &digest,
            input: &input,
            expected_version: state.version,
            order_id: Some(&id),
            source_snapshot: None,
            frozen: &frozen,
        },
    )
    .await
    .expect_err("one intent per order");
    assert_eq!(conflict.status, 409);
    let mut foreign = i.clone();
    foreign.actor = "another-actor".into();
    assert!(native_store::claim_transport(
        &store,
        &foreign,
        "wh-main",
        "native-journal-1",
        &digest
    )
    .await
    .is_err());
    let first = native_store::claim_transport(&store, &i, "wh-main", "native-journal-1", &digest)
        .await
        .expect("first sender");
    let lease = match first {
        Claim::Ready { lease, .. } => lease,
        Claim::Existing(_) => panic!("expected new lease"),
    };
    assert!(matches!(
        native_store::claim_transport(&store, &i, "wh-main", "native-journal-1", &digest)
            .await
            .expect("concurrent claim"),
        Claim::Existing(_)
    ));
    let uncertain = native_store::finish_transport(
        &store,
        &i,
        "wh-main",
        "native-journal-1",
        &lease,
        Err("Response lost after remote acceptance".into()),
    )
    .await
    .expect("retain uncertainty");
    assert_eq!(uncertain.status, "uncertain");
    // Local picking may change after an uncertain attempt. The original remote
    // key must still be replayable; the retry does not invent a second intent.
    execute(
        &store,
        &i,
        Command::Unpick {
            order_id: id.clone(),
            line_id: "line-1".into(),
            location: "A".into(),
            lot: "LOT".into(),
            quantity: 1,
            reason: "Synthetic recovery".into(),
        },
    )
    .await;
    let retry = native_store::claim_transport(&store, &i, "wh-main", "native-journal-1", &digest)
        .await
        .expect("retry original intent");
    let (lease, retry) = match retry {
        Claim::Ready { record, lease } => (lease, record),
        Claim::Existing(_) => panic!("expected retry lease"),
    };
    assert_eq!(
        native_store::prepared(&retry)
            .expect("same exact plan")
            .idempotency_key,
        prepared.idempotency_key
    );
    let completed = native_store::finish_transport(
        &store,
        &i,
        "wh-main",
        "native-journal-1",
        &lease,
        Ok(json!({"data":{"id":"load-original"},"replayed":true})),
    )
    .await
    .expect("complete original");
    assert_eq!(completed.status, "completed");
    assert_eq!(completed.attempts, 2);
    assert!(matches!(
        native_store::claim_transport(&store, &i, "wh-main", "native-journal-1", &digest)
            .await
            .expect("completed receipt"),
        Claim::Existing(_)
    ));
}
#[tokio::test]
#[ignore = "requires isolated Postgres; WMS_TEST_DATABASE_URL"]
async fn first_transport_send_rechecks_packed_state_and_exception_then_allows_unrelated_inventory()
{
    let (store, i) = postgres().await;
    let (state, id, prepared) = packed(&store, &i).await;
    let input = json!({"plan":plan()});
    let digest =
        native_store::digest("tms_transport", "wh-main", Some(&id), &input).expect("digest");
    let frozen = json!(prepared);
    native_store::freeze(
        &store,
        &i,
        "wh-main",
        Freeze {
            kind: "tms_transport",
            request_id: "native-revalidate-1",
            digest: &digest,
            input: &input,
            expected_version: state.version,
            order_id: Some(&id),
            source_snapshot: None,
            frozen: &frozen,
        },
    )
    .await
    .expect("intent");
    let exception = execute(
        &store,
        &i,
        Command::ReportException {
            order_id: Some(id),
            category: "dock_hold".into(),
            detail: "Synthetic stop before network".into(),
        },
    )
    .await;
    assert_eq!(
        native_store::claim_transport(&store, &i, "wh-main", "native-revalidate-1", &digest)
            .await
            .err()
            .expect("exception holds first send")
            .status,
        409
    );
    execute(
        &store,
        &i,
        Command::ResolveException {
            exception_id: exception["entity_id"].as_str().expect("exception").into(),
            resolution: "Synthetic hold released".into(),
        },
    )
    .await;
    execute(
        &store,
        &i,
        Command::CountStock {
            sku: "SKU".into(),
            lot: "LOT".into(),
            location: "A".into(),
            counted: 8,
            reason: "Unrelated unreserved bin verified".into(),
        },
    )
    .await;
    assert!(matches!(
        native_store::claim_transport(&store, &i, "wh-main", "native-revalidate-1", &digest)
            .await
            .expect("revalidated current order facts"),
        Claim::Ready { .. }
    ));
}
#[tokio::test]
#[ignore = "requires isolated Postgres; WMS_TEST_DATABASE_URL"]
async fn expired_native_delivery_lease_retains_uncertain_attempt_before_reclaim() {
    let (store, i) = postgres().await;
    let (state, id, prepared) = packed(&store, &i).await;
    let input = json!({"plan":plan()});
    let digest =
        native_store::digest("tms_transport", "wh-main", Some(&id), &input).expect("digest");
    native_store::freeze(
        &store,
        &i,
        "wh-main",
        Freeze {
            kind: "tms_transport",
            request_id: "native-expired-1",
            digest: &digest,
            input: &input,
            expected_version: state.version,
            order_id: Some(&id),
            source_snapshot: None,
            frozen: &json!(prepared),
        },
    )
    .await
    .expect("intent");
    let first = native_store::claim_transport(&store, &i, "wh-main", "native-expired-1", &digest)
        .await
        .expect("claim");
    let old = match first {
        Claim::Ready { lease, .. } => lease,
        _ => panic!("new lease"),
    };
    sqlx::query("UPDATE wms_native_requests SET lease_until=now()-interval '1 second' WHERE tenant=$1 AND request_id='native-expired-1'").bind(&i.tenant).execute(&store.pool).await.expect("simulate process crash");
    let retry = native_store::claim_transport(&store, &i, "wh-main", "native-expired-1", &digest)
        .await
        .expect("reclaim original");
    assert!(matches!(retry, Claim::Ready { .. }));
    let outcome: String = sqlx::query_scalar("SELECT outcome FROM wms_native_attempts WHERE id=$1")
        .bind(old)
        .fetch_one(&store.pool)
        .await
        .expect("original attempt audit");
    assert_eq!(outcome, "uncertain");
}

#[tokio::test]
#[ignore = "requires isolated Postgres; WMS_TEST_DATABASE_URL"]
async fn import_api_recovers_after_command_commit_using_frozen_source_and_actor_bound_receipt() {
    let (store, i) = postgres().await;
    execute(
        &store,
        &i,
        Command::CreateProduct {
            sku: "SKU".into(),
            name: "Synthetic product".into(),
            category: "General".into(),
            reorder_point: 0,
        },
    )
    .await;
    let state = store
        .state(&i.tenant, "wh-main")
        .await
        .expect("seeded state");
    let command = Command::CreateOrder {
        reference: "SO-RECOVERED-NATIVE".into(),
        source_order_id: Some("oms:oms-acme:00000000-0000-4000-8000-000000000999".into()),
        customer: "Synthetic customer".into(),
        destination: "Verified destination".into(),
        priority: "normal".into(),
        lines: vec![LineInput {
            id: "line-original".into(),
            sku: "SKU".into(),
            quantity: 2,
        }],
    };
    let mut input = import_body();
    input["expected_version"] = json!(state.version);
    let request = input["request_id"].as_str().expect("request ID");
    let digest = native_store::digest("oms_import", "wh-main", None, &input)
        .expect("exact input fingerprint");
    let command_request = Request {
        request_id: native_store::import_command_id(&i.tenant, "wh-main", request),
        expected_version: state.version,
        command,
    };
    let frozen = json!({"command_request":command_request,"oms_order":{"id":input["oms_order_id"],"status":"processing"}});
    let original = json!({"meta":{"requestId":"original-upstream-request"},"data":{"id":input["oms_order_id"],"futureField":{"original":[1,"retain exactly",null]},"status":"processing"}});
    native_store::freeze(
        &store,
        &i,
        "wh-main",
        Freeze {
            kind: "oms_import",
            request_id: request,
            digest: &digest,
            input: &input,
            expected_version: state.version,
            order_id: None,
            source_snapshot: Some(&original),
            frozen: &frozen,
        },
    )
    .await
    .expect("snapshot journal before command");
    let committed = store
        .execute(&i, "wh-main", &command_request)
        .await
        .expect("command commits before simulated process crash");
    let make_router = |identity: Identity| {
        native_api::router()
            .layer(Extension(identity))
            .with_state(App {
                store: store.clone(),
                credentials: Arc::new(Vec::new()),
                demo: false,
                connections: Connections::default(),
            })
    };
    // There is no configured OMS in this test: a recovery must not fetch or
    // translate another source snapshot after the command already committed.
    let (status, response) = call(
        make_router(i.clone()),
        "POST",
        "/v1/warehouses/wh-main/imports/oms",
        input.clone(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(response["status"], "completed");
    assert_eq!(response["response"]["result"], committed["result"]);
    assert_eq!(response["source_snapshot"], original);
    let (status, replay) = call(
        make_router(i.clone()),
        "POST",
        "/v1/warehouses/wh-main/imports/oms",
        input.clone(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(replay["response"], response["response"]);
    let mut other = i.clone();
    other.actor = "fixture:another-actor".into();
    assert_eq!(
        call(
            make_router(other),
            "POST",
            "/v1/warehouses/wh-main/imports/oms",
            input.clone()
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    let mut changed = input.clone();
    changed["details"]["customer"] = json!("Changed customer");
    assert_eq!(
        call(
            make_router(i.clone()),
            "POST",
            "/v1/warehouses/wh-main/imports/oms",
            changed
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    let mut foreign = i.clone();
    foreign.tenant = "another-tenant-without-this-warehouse".into();
    assert_eq!(
        call(
            make_router(foreign),
            "POST",
            "/v1/warehouses/wh-main/imports/oms",
            input.clone()
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM wms_orders WHERE tenant=$1")
        .bind(&i.tenant)
        .fetch_one(&store.pool)
        .await
        .expect("orders");
    assert_eq!(count, 1);
    assert!(sqlx::query(
        "UPDATE wms_native_requests SET frozen='{}'::jsonb WHERE tenant=$1 AND request_id=$2"
    )
    .bind(&i.tenant)
    .bind(request)
    .execute(&store.pool)
    .await
    .is_err());
    assert!(
        sqlx::query("DELETE FROM wms_native_requests WHERE tenant=$1 AND request_id=$2")
            .bind(&i.tenant)
            .bind(request)
            .execute(&store.pool)
            .await
            .is_err()
    );
}

#[tokio::test]
#[ignore = "requires isolated Postgres; WMS_TEST_DATABASE_URL"]
async fn native_intents_and_finished_attempts_reject_database_evidence_tampering() {
    let (store, i) = postgres().await;
    let (state, id, prepared) = packed(&store, &i).await;
    let input = json!({"plan":plan()});
    let digest =
        native_store::digest("tms_transport", "wh-main", Some(&id), &input).expect("digest");
    native_store::freeze(
        &store,
        &i,
        "wh-main",
        Freeze {
            kind: "tms_transport",
            request_id: "native-immutable-1",
            digest: &digest,
            input: &input,
            expected_version: state.version,
            order_id: Some(&id),
            source_snapshot: Some(&json!({"original":true})),
            frozen: &json!(prepared),
        },
    )
    .await
    .expect("intent");
    for mutation in [
        "actor='tampered'",
        "digest=repeat('x',64)",
        "input='{}'::jsonb",
        "source_snapshot='{}'::jsonb",
        "frozen='{}'::jsonb",
        "expected_version=expected_version+1",
        "order_id='different-order'",
    ] {
        let sql=format!("UPDATE wms_native_requests SET {mutation} WHERE tenant=$1 AND request_id='native-immutable-1'");
        assert!(
            sqlx::query(&sql)
                .bind(&i.tenant)
                .execute(&store.pool)
                .await
                .is_err(),
            "{mutation}"
        );
    }
    let claim = native_store::claim_transport(&store, &i, "wh-main", "native-immutable-1", &digest)
        .await
        .expect("claim");
    let lease = match claim {
        Claim::Ready { lease, .. } => lease,
        _ => panic!("new lease"),
    };
    native_store::finish_transport(
        &store,
        &i,
        "wh-main",
        "native-immutable-1",
        &lease,
        Err("Synthetic uncertain remote response".into()),
    )
    .await
    .expect("retained uncertain attempt");
    assert!(
        sqlx::query("UPDATE wms_native_attempts SET detail='tampered' WHERE id=$1")
            .bind(&lease)
            .execute(&store.pool)
            .await
            .is_err()
    );
    assert!(sqlx::query("DELETE FROM wms_native_attempts WHERE id=$1")
        .bind(&lease)
        .execute(&store.pool)
        .await
        .is_err());
}

#[tokio::test]
#[ignore = "requires isolated Postgres; WMS_TEST_DATABASE_URL"]
async fn competing_warehouses_cannot_import_the_same_complete_native_oms_order() {
    let (store, i) = postgres().await;
    execute(
        &store,
        &i,
        Command::CreateProduct {
            sku: "SKU".into(),
            name: "Synthetic product".into(),
            category: "General".into(),
            reorder_point: 0,
        },
    )
    .await;
    sqlx::query("INSERT INTO wms_warehouses(tenant,warehouse_id,version,metadata) VALUES($1,'wh-secondary',0,$2)").bind(&i.tenant).bind(json!({"id":"wh-secondary","code":"SECOND","name":"Synthetic second warehouse","city":"Test city","timezone":"UTC"})).execute(&store.pool).await.expect("second warehouse");
    sqlx::query("INSERT INTO wms_products SELECT tenant,'wh-secondary',sku,data FROM wms_products WHERE tenant=$1 AND warehouse_id='wh-main'").bind(&i.tenant).execute(&store.pool).await.expect("second warehouse SKU");
    sqlx::query("INSERT INTO wms_locations SELECT tenant,'wh-secondary',code,kind,data FROM wms_locations WHERE tenant=$1 AND warehouse_id='wh-main'").bind(&i.tenant).execute(&store.pool).await.expect("second warehouse bins");
    let source = "oms:oms-acme:00000000-0000-4000-8000-000000000999";
    let command = Command::CreateOrder {
        reference: "SO-SOURCE-OWNER".into(),
        source_order_id: Some(source.into()),
        customer: "Synthetic customer".into(),
        destination: "Verified destination".into(),
        priority: "normal".into(),
        lines: vec![LineInput {
            id: "source-line".into(),
            sku: "SKU".into(),
            quantity: 2,
        }],
    };
    let a_input = json!({"request_id":"source-owner-primary"});
    let b_input = json!({"request_id":"source-owner-secondary"});
    let a_digest =
        native_store::digest("oms_import", "wh-main", None, &a_input).expect("primary digest");
    let b_digest = native_store::digest("oms_import", "wh-secondary", None, &b_input)
        .expect("secondary digest");
    let version = store
        .state(&i.tenant, "wh-main")
        .await
        .expect("primary state")
        .version;
    let a_request = Request {
        request_id: native_store::import_command_id(&i.tenant, "wh-main", "source-owner-primary"),
        expected_version: version,
        command: command.clone(),
    };
    let b_request = Request {
        request_id: native_store::import_command_id(
            &i.tenant,
            "wh-secondary",
            "source-owner-secondary",
        ),
        expected_version: 0,
        command: command.clone(),
    };
    let a_frozen = json!({"command_request":a_request});
    let b_frozen = json!({"command_request":b_request});
    let a = native_store::freeze(
        &store,
        &i,
        "wh-main",
        Freeze {
            kind: "oms_import",
            request_id: "source-owner-primary",
            digest: &a_digest,
            input: &a_input,
            expected_version: version,
            order_id: None,
            source_snapshot: None,
            frozen: &a_frozen,
        },
    );
    let b = native_store::freeze(
        &store,
        &i,
        "wh-secondary",
        Freeze {
            kind: "oms_import",
            request_id: "source-owner-secondary",
            digest: &b_digest,
            input: &b_input,
            expected_version: 0,
            order_id: None,
            source_snapshot: None,
            frozen: &b_frozen,
        },
    );
    let (a, b) = tokio::join!(a, b);
    assert_ne!(
        a.is_ok(),
        b.is_ok(),
        "exactly one full-source intent must win across warehouses"
    );
    let (winner, loser, request, key) = match (a, b) {
        (Ok(_), Err(error)) => {
            assert_eq!(error.status, 409);
            ("wh-main", "wh-secondary", a_request, "source-owner-primary")
        }
        (Err(error), Ok(_)) => {
            assert_eq!(error.status, 409);
            (
                "wh-secondary",
                "wh-main",
                b_request,
                "source-owner-secondary",
            )
        }
        _ => panic!("one source owner required"),
    };
    let response = store
        .execute(&i, winner, &request)
        .await
        .expect("single source owner command");
    native_store::complete_import(&store, &i, winner, key, &response)
        .await
        .expect("owner receipt");
    assert_eq!(
        native_store::check_import_source(
            &store,
            &i,
            loser,
            "another-native-source-request",
            source
        )
        .await
        .expect_err("friendly preflight rejects another warehouse")
        .status,
        409
    );
    // A manual command bypassing the native route still cannot defeat the
    // database's tenant-wide ownership constraint for the native source prefix.
    let version = store
        .state(&i.tenant, loser)
        .await
        .expect("losing warehouse state")
        .version;
    assert!(store
        .execute(
            &i,
            loser,
            &Request {
                request_id: domain::id(),
                expected_version: version,
                command
            }
        )
        .await
        .is_err());
    let count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM wms_orders WHERE tenant=$1 AND source_order_id=$2",
    )
    .bind(&i.tenant)
    .bind(source)
    .fetch_one(&store.pool)
    .await
    .expect("source owners");
    assert_eq!(count, 1);
}

#[derive(Clone)]
struct NativeWire(Arc<tokio::sync::Mutex<NativeWireState>>);
struct NativeWireState {
    source: Value,
    oms_reads: usize,
    oms_writes: usize,
    tms_requests: Vec<(String, Value)>,
    accepted: Option<Value>,
    created_loads: usize,
}
async fn wire_source(HttpState(wire): HttpState<NativeWire>, headers: HeaderMap) -> Json<Value> {
    assert_eq!(headers["x-stateset-api-key"], "fixture-native-oms-secret");
    let mut wire = wire.0.lock().await;
    wire.oms_reads += 1;
    Json(json!({"meta":{"requestId":"original-raw-response"},"data":wire.source}))
}
async fn wire_orders(HttpState(wire): HttpState<NativeWire>, headers: HeaderMap) -> Json<Value> {
    assert_eq!(headers["x-stateset-api-key"], "fixture-native-oms-secret");
    let mut wire = wire.0.lock().await;
    wire.oms_reads += 1;
    Json(
        json!({"meta":{"requestId":"list-response"},"data":{"items":[wire.source],"pagination":{"page":1,"pageSize":100,"totalItems":1,"totalPages":1,"hasNext":false,"hasPrevious":false}}}),
    )
}
async fn forbidden_oms_write(HttpState(wire): HttpState<NativeWire>) -> StatusCode {
    wire.0.lock().await.oms_writes += 1;
    StatusCode::INTERNAL_SERVER_ERROR
}
async fn wire_access(headers: HeaderMap) -> Json<Value> {
    assert_eq!(headers["authorization"], "Bearer fixture-native-tms-secret");
    Json(
        json!({"tenant_id":"freight-acme","access":{"subject":"fixture:operations","role":"operations","capabilities":["workspace.read","operations.write"]}}),
    )
}
async fn wire_tms(
    HttpState(wire): HttpState<NativeWire>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> axum::response::Response {
    assert_eq!(headers["authorization"], "Bearer fixture-native-tms-secret");
    let key = headers["idempotency-key"]
        .to_str()
        .expect("stable key")
        .to_string();
    let mut wire = wire.0.lock().await;
    wire.tms_requests.push((key, body.clone()));
    if let Some(accepted) = &wire.accepted {
        assert_eq!(wire.tms_requests[0], wire.tms_requests[1]);
        return Json(json!({"data":accepted,"replayed":true})).into_response();
    }
    wire.created_loads += 1;
    wire.accepted = Some(
        json!({"id":"load-accepted-before-response-failed","reference":body["load"]["reference"],"sync_order_id":body["order_id"],"status":"planned","source_order":wire.source}),
    );
    // The remote load is committed, then the response fails. A client must not
    // infer rejection or issue a new transport identity from this HTTP status.
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(json!({"error":"Simulated response failure after native TMS commit"})),
    )
        .into_response()
}
struct NativeEnvironment(Option<std::ffi::OsString>);
impl Drop for NativeEnvironment {
    fn drop(&mut self) {
        match self.0.take() {
            Some(value) => std::env::set_var("WMS_NATIVE_INTEGRATIONS_JSON", value),
            None => std::env::remove_var("WMS_NATIVE_INTEGRATIONS_JSON"),
        }
    }
}
static NATIVE_ENVIRONMENT: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
#[tokio::test]
async fn blank_native_environment_supports_unconfigured_compose_defaults() {
    let _serial = NATIVE_ENVIRONMENT.lock().await;
    let _environment = NativeEnvironment(std::env::var_os("WMS_NATIVE_INTEGRATIONS_JSON"));
    std::env::set_var("WMS_NATIVE_INTEGRATIONS_JSON", " \n\t ");
    assert!(NativeIntegrations::from_env()
        .expect("blank native configuration is disabled")
        .tenants
        .is_empty());
    assert!(NativeIntegrations::from_json("").is_err());
}
async fn authenticated_call(
    app: Router,
    method: &str,
    path: &str,
    body: Value,
) -> (StatusCode, Value) {
    let response = app
        .oneshot(
            HttpRequest::builder()
                .method(method)
                .uri(path)
                .header("authorization", "Bearer native-api-fixture-key")
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .expect("authenticated request"),
        )
        .await
        .expect("authenticated response");
    let status = response.status();
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("response body")
        .to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).expect("JSON API response"),
    )
}
#[tokio::test]
#[ignore = "requires isolated Postgres; WMS_TEST_DATABASE_URL"]
async fn authenticated_native_http_and_postgres_workflow_recovers_remote_acceptance_without_second_load(
) {
    let _serial = NATIVE_ENVIRONMENT.lock().await;
    let (store, i) = postgres().await;
    for command in [
        Command::CreateProduct {
            sku: "SKU".into(),
            name: "Synthetic product".into(),
            category: "General".into(),
            reorder_point: 0,
        },
        Command::CreateLocation {
            code: "A".into(),
            zone: "Storage".into(),
            kind: "storage".into(),
        },
        Command::CountStock {
            sku: "SKU".into(),
            lot: "LOT".into(),
            location: "A".into(),
            counted: 10,
            reason: "Synthetic opening physical count".into(),
        },
    ] {
        execute(&store, &i, command).await;
    }
    let source = json!({"id":"00000000-0000-4000-8000-000000000999","shopifyOrderId":"shopify:41001","orderNumber":"SO-NATIVE-HTTP","status":"processing","lineItems":[{"title":"Synthetic item","quantity":2.0,"price":19.99,"sku":"SKU","variantId":"v-1","shopifyLineItemId":"source-line-a"},{"title":"Synthetic item","quantity":3.0,"price":19.99,"sku":"SKU","variantId":"v-1","shopifyLineItemId":"source-line-b"}],"createdAt":"2026-10-10T12:00:00Z","updatedAt":"2026-10-11T12:00:00Z","futureField":{"undigestedEvidence":["retain",1,null]}});
    let wire = NativeWire(Arc::new(tokio::sync::Mutex::new(NativeWireState {
        source,
        oms_reads: 0,
        oms_writes: 0,
        tms_requests: Vec::new(),
        accepted: None,
        created_loads: 0,
    })));
    let remote = Router::new()
        .route("/v1/tenants/oms-acme/orders", get(wire_orders))
        .route("/v1/tenants/oms-acme/orders/{id}", get(wire_source))
        .route(
            "/v1/tenants/oms-acme/orders/{id}/fulfillment",
            post(forbidden_oms_write),
        )
        .route(
            "/v1/tenants/oms-acme/orders/{id}/tracking",
            post(forbidden_oms_write),
        )
        .route("/v1/sync/import", post(wire_tms))
        .route("/v1/access", get(wire_access))
        .with_state(wire.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("native contract server");
    let base = format!("http://{}", listener.local_addr().expect("native address"));
    let server = tokio::spawn(async move {
        axum::serve(listener, remote).await.expect("native server");
    });
    let _environment = NativeEnvironment(std::env::var_os("WMS_NATIVE_INTEGRATIONS_JSON"));
    std::env::set_var("WMS_NATIVE_INTEGRATIONS_JSON",json!({(i.tenant.clone()):{"oms":{"base_url":base,"tenant_id":"oms-acme","api_key":"fixture-native-oms-secret"},"tms":{"base_url":base,"tenant_id":"freight-acme","bearer_token":"fixture-native-tms-secret"}}}).to_string());
    let app = api::router(App {
        store: store.clone(),
        credentials: Arc::new(vec![("native-api-fixture-key".into(), i.clone())]),
        demo: false,
        connections: Connections::default(),
    });
    let (status, page) = authenticated_call(
        app.clone(),
        "GET",
        "/v1/integrations/oms/orders?page=1",
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(page["items"].as_array().expect("OMS list").len(), 1);
    let mut input = import_body();
    input["request_id"] = json!("native-http-import");
    input["expected_version"] = json!(
        store
            .state(&i.tenant, "wh-main")
            .await
            .expect("warehouse")
            .version
    );
    let mut stale = input.clone();
    stale["request_id"] = json!("native-http-stale");
    stale["source_updated_at"] = json!("2026-10-09T12:00:00Z");
    assert_eq!(
        authenticated_call(
            app.clone(),
            "POST",
            "/v1/warehouses/wh-main/imports/oms",
            stale
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    let (status, imported) = authenticated_call(
        app.clone(),
        "POST",
        "/v1/warehouses/wh-main/imports/oms",
        input.clone(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{imported}");
    assert_eq!(imported["status"], "completed");
    assert_eq!(
        imported["source_snapshot"]["data"]["futureField"]["undigestedEvidence"],
        json!(["retain", 1, null])
    );
    let reads = wire.0.lock().await.oms_reads;
    let (status, replayed) = authenticated_call(
        app.clone(),
        "POST",
        "/v1/warehouses/wh-main/imports/oms",
        input,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(replayed["response"], imported["response"]);
    assert_eq!(wire.0.lock().await.oms_reads, reads);
    let order_id = imported["response"]["result"]["entity_id"]
        .as_str()
        .expect("native order")
        .to_string();
    execute(
        &store,
        &i,
        Command::Allocate {
            order_id: order_id.clone(),
        },
    )
    .await;
    let allocated = store
        .state(&i.tenant, "wh-main")
        .await
        .expect("allocations");
    let order = allocated
        .orders
        .iter()
        .find(|o| o.id == order_id)
        .expect("allocated order");
    assert_eq!(order.lines.len(), 2);
    assert_ne!(order.lines[0].id, order.lines[1].id);
    for allocation in &order.allocations {
        execute(
            &store,
            &i,
            Command::Pick {
                order_id: order_id.clone(),
                line_id: allocation.line_id.clone(),
                location: allocation.location.clone(),
                lot: allocation.lot.clone(),
                quantity: allocation.quantity,
            },
        )
        .await;
    }
    execute(
        &store,
        &i,
        Command::Pack {
            order_id: order_id.clone(),
        },
    )
    .await;
    let packed = store
        .state(&i.tenant, "wh-main")
        .await
        .expect("packed state");
    let transport_body = json!({"request_id":"native-http-transport","expected_version":packed.version,"plan":plan()});
    let path = format!("/v1/warehouses/wh-main/orders/{order_id}/transport");
    let (status, uncertain) =
        authenticated_call(app.clone(), "POST", &path, transport_body.clone()).await;
    assert_eq!(status, StatusCode::OK, "{uncertain}");
    assert_eq!(uncertain["status"], "uncertain");
    assert!(uncertain["last_error"]
        .as_str()
        .expect("uncertain detail")
        .contains("HTTP 500"));
    let oms_before_retry = wire.0.lock().await.oms_reads;
    {
        let mut remote = wire.0.lock().await;
        remote.source["status"] = json!("cancelled");
        remote.source["updatedAt"] = json!("2026-10-12T12:00:00Z");
    }
    let first = order.allocations.first().expect("first allocation");
    execute(
        &store,
        &i,
        Command::Unpick {
            order_id: order_id.clone(),
            line_id: first.line_id.clone(),
            location: first.location.clone(),
            lot: first.lot.clone(),
            quantity: 1,
            reason: "Local review after uncertain remote acceptance".into(),
        },
    )
    .await;
    let (status, accepted) =
        authenticated_call(app.clone(), "POST", &path, transport_body.clone()).await;
    assert_eq!(status, StatusCode::OK, "{accepted}");
    assert_eq!(accepted["status"], "completed");
    assert_eq!(accepted["response"]["replayed"], true);
    assert_eq!(wire.0.lock().await.oms_reads, oms_before_retry);
    let (_, again) = authenticated_call(app.clone(), "POST", &path, transport_body).await;
    assert_eq!(again["response"], accepted["response"]);
    let (status, receipt) = authenticated_call(
        app,
        "GET",
        "/v1/warehouses/wh-main/native-requests/native-http-transport",
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(receipt["attempt_history"][0]["outcome"], "uncertain");
    assert_eq!(receipt["attempt_history"][1]["outcome"], "accepted");
    let remote = wire.0.lock().await;
    assert_eq!(remote.created_loads, 1);
    assert_eq!(remote.tms_requests.len(), 2);
    assert_eq!(remote.tms_requests[0], remote.tms_requests[1]);
    assert_eq!(remote.oms_writes, 0);
    let intents: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM wms_native_requests WHERE tenant=$1 AND kind='tms_transport'",
    )
    .bind(&i.tenant)
    .fetch_one(&store.pool)
    .await
    .expect("one retained transport intent");
    assert_eq!(intents, 1);
    server.abort();
}
