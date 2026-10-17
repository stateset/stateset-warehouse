//! Conditional OMS API guards and real PostgreSQL journal recovery.
use axum::{
    body::Body,
    extract::State as HttpState,
    http::{HeaderMap, Request as HttpRequest, StatusCode},
    response::IntoResponse,
    routing::{get, post},
    Extension, Json, Router,
};
use chrono::{DateTime, Duration};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use stateset_warehouse::{
    api::App,
    domain::{self, Command, LineInput},
    integrations::Connections,
    native_store::{self, Claim, Freeze},
    oms_client::{payload_digest, FulfillmentLine, FulfillmentPayload, PreparedFulfillment},
    oms_fulfillment_api::{self, claim_fulfillment, finish_fulfillment},
    store::{Identity, Request, Store},
};
use std::sync::Arc;
use tower::ServiceExt;

fn api(role: &str, demo: bool) -> Router {
    let pool = sqlx::postgres::PgPoolOptions::new()
        .connect_lazy("postgres://unused@127.0.0.1:1/unused")
        .expect("lazy pool");
    Router::new()
        .route(
            "/v1/warehouses/{warehouse}/orders/{order}/oms-fulfillment",
            post(oms_fulfillment_api::fulfill),
        )
        .layer(Extension(Identity {
            tenant: "guard-fixture".into(),
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
async fn call(app: Router, body: Value) -> StatusCode {
    app.oneshot(
        HttpRequest::builder()
            .method("POST")
            .uri("/v1/warehouses/wh-main/orders/order/oms-fulfillment")
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .expect("request"),
    )
    .await
    .expect("response")
    .status()
}
#[tokio::test]
async fn conditional_fulfillment_denies_viewer_and_oms_roles_before_any_database_or_http() {
    for role in ["viewer", "oms"] {
        assert_eq!(
            call(
                api(role, false),
                json!({"request_id":"valid-request-id","expected_version":0})
            )
            .await,
            StatusCode::FORBIDDEN
        );
    }
}
#[tokio::test]
async fn conditional_fulfillment_is_disabled_in_demo_even_for_an_administrator() {
    assert_eq!(
        call(
            api("admin", true),
            json!({"request_id":"valid-request-id","expected_version":0})
        )
        .await,
        StatusCode::CONFLICT
    );
}
#[tokio::test]
async fn conditional_fulfillment_rejects_spoofed_input_and_invalid_identity_before_connecting() {
    for body in [
        json!({"request_id":"valid-request-id","expected_version":0,"actor":"spoofed"}),
        json!({"request_id":"valid-request-id","expected_version":0,"event_id":"spoofed"}),
        json!({"request_id":"bad/id","expected_version":0}),
        json!({"request_id":"short","expected_version":0}),
        json!({"request_id":"valid-request-id","expected_version":-1}),
    ] {
        assert_eq!(
            call(api("operator", false), body).await,
            StatusCode::UNPROCESSABLE_ENTITY
        );
    }
}

async fn postgres() -> (Store, Identity) {
    let url = std::env::var("WMS_TEST_DATABASE_URL").expect("isolated WMS_TEST_DATABASE_URL");
    assert!(
        reqwest::Url::parse(&url)
            .expect("database URI")
            .path()
            .contains("test"),
        "only isolated test databases"
    );
    let store = Store::connect(&url).await.expect("test store");
    let identity = Identity {
        tenant: format!("conditional-{}", domain::id()),
        actor: "fixture:conditional-operator".into(),
        role: "admin".into(),
    };
    store
        .provision(&identity.tenant, false)
        .await
        .expect("warehouse");
    (store, identity)
}
async fn execute(store: &Store, i: &Identity, command: Command) -> Value {
    let expected_version = store
        .state(&i.tenant, "wh-main")
        .await
        .expect("current state")
        .version;
    store
        .execute(
            i,
            "wh-main",
            &Request {
                request_id: domain::id(),
                expected_version,
                command,
            },
        )
        .await
        .expect("fixture command")["result"]
        .clone()
}
async fn shipped_fixture(store: &Store, i: &Identity) -> PreparedFulfillment {
    for command in [
        Command::CreateProduct {
            sku: "SKU".into(),
            name: "Synthetic item".into(),
            category: "Test".into(),
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
    let source = domain::id();
    let result = execute(
        store,
        i,
        Command::CreateOrder {
            reference: "SO-CONDITIONAL".into(),
            source_order_id: Some(format!("oms:oms-fixture:{source}")),
            customer: "Synthetic customer".into(),
            destination: "Synthetic destination".into(),
            priority: "normal".into(),
            lines: vec![
                LineInput {
                    id: format!("oms-line-{:x}", Sha256::digest(b"source:source-line-a")),
                    sku: "SKU".into(),
                    quantity: 2,
                },
                LineInput {
                    id: format!("oms-line-{:x}", Sha256::digest(b"source:source-line-b")),
                    sku: "SKU".into(),
                    quantity: 3,
                },
            ],
        },
    )
    .await;
    let order_id = result["entity_id"].as_str().expect("order").to_string();
    execute(
        store,
        i,
        Command::Allocate {
            order_id: order_id.clone(),
        },
    )
    .await;
    let state = store
        .state(&i.tenant, "wh-main")
        .await
        .expect("allocated state");
    for allocation in &state
        .orders
        .iter()
        .find(|o| o.id == order_id)
        .expect("allocated order")
        .allocations
    {
        execute(
            store,
            i,
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
        store,
        i,
        Command::Pack {
            order_id: order_id.clone(),
        },
    )
    .await;
    execute(
        store,
        i,
        Command::Ship {
            order_id: order_id.clone(),
            carrier: "Synthetic carrier".into(),
            handoff_reference: "Synthetic physical handoff".into(),
        },
    )
    .await;
    let order: domain::Order = serde_json::from_value(
        store
            .detail(&i.tenant, "wh-main", "orders", &order_id)
            .await
            .expect("shipped order"),
    )
    .expect("canonical order");
    let event_id:String=sqlx::query_scalar("SELECT id FROM wms_events WHERE tenant=$1 AND payload->>'type'='fulfillment.shipped' AND payload->'order'->>'id'=$2")
        .bind(&i.tenant).bind(&order_id).fetch_one(&store.pool).await.expect("shipment event");
    let payload = FulfillmentPayload {
        request_id: domain::id(),
        order_id: source,
        event_id: event_id.clone(),
        expected_updated_at: "2026-10-17T00:00:00Z".into(),
        warehouse_id: "wh-main".into(),
        warehouse_order_id: order_id.clone(),
        carrier: order.carrier,
        handoff_reference: order.handoff_reference,
        handed_off_at: order.shipped_at.expect("handoff time"),
        lines: order
            .lines
            .iter()
            .map(|l| FulfillmentLine {
                line_id: l.id.clone(),
                sku: l.sku.clone(),
                quantity_each: l.quantity,
            })
            .collect(),
    };
    PreparedFulfillment {
        tenant: i.tenant.clone(),
        warehouse_id: "wh-main".into(),
        order_id,
        event_id: event_id.clone(),
        recipient_base_url: "http://127.0.0.1:1".into(),
        recipient_tenant_id: "oms-fixture".into(),
        idempotency_key: payload.request_id.clone(),
        digest: payload_digest(&payload).expect("payload digest"),
        payload,
    }
}
async fn freeze(
    store: &Store,
    i: &Identity,
    key: &str,
    prepared: &PreparedFulfillment,
) -> Result<String, stateset_warehouse::store::Error> {
    let version = store.state(&i.tenant, "wh-main").await?.version;
    let input = json!({"request_id":key,"expected_version":version});
    let digest = native_store::digest(
        "oms_fulfillment",
        "wh-main",
        Some(&prepared.order_id),
        &input,
    )?;
    native_store::freeze(
        store,
        i,
        "wh-main",
        Freeze {
            kind: "oms_fulfillment",
            request_id: key,
            digest: &digest,
            input: &input,
            expected_version: version,
            order_id: Some(&prepared.order_id),
            source_snapshot: Some(&json!({"data":{"revision":"raw-source-revision"}})),
            frozen: &json!(prepared),
        },
    )
    .await?;
    Ok(digest)
}

fn acknowledgement(prepared: &PreparedFulfillment, rejected: bool) -> Value {
    let timestamp = (DateTime::parse_from_rfc3339(&prepared.payload.expected_updated_at)
        .expect("retained source revision")
        + Duration::microseconds(1))
    .to_rfc3339();
    let mut response = json!({"data":{
        "contract":stateset_warehouse::oms_client::CONTRACT,
        "tenant_id":prepared.recipient_tenant_id,"order_id":prepared.payload.order_id,
        "event_id":prepared.event_id,"request_id":prepared.payload.request_id,
        "request_digest":prepared.digest,"replayed":false,
        "status":if rejected {"rejected"}else{"fulfilled"}
    }});
    if rejected {
        response["data"]["reason"] = json!("source_revision_changed");
        response["data"]["observed_updated_at"] = json!(timestamp);
    } else {
        response["data"]["applied_updated_at"] = json!(timestamp);
    }
    response
}

#[tokio::test]
#[ignore = "requires isolated Postgres; WMS_TEST_DATABASE_URL"]
async fn conditional_journal_recovers_expired_lease_without_replacing_frozen_handoff_or_accepting_old_worker(
) {
    let (store, i) = postgres().await;
    let prepared = shipped_fixture(&store, &i).await;
    let key = "conditional-lost-response";
    let digest = freeze(&store, &i, key, &prepared)
        .await
        .expect("frozen handoff");
    let first = match claim_fulfillment(&store, &i, "wh-main", key, &digest)
        .await
        .expect("first claim")
    {
        Claim::Ready { record, lease } => {
            assert_eq!(record.frozen, json!(prepared));
            lease
        }
        _ => panic!("fresh claim required"),
    };
    assert!(matches!(
        claim_fulfillment(&store, &i, "wh-main", key, &digest)
            .await
            .expect("active lease"),
        Claim::Existing(_)
    ));
    sqlx::query("UPDATE wms_native_requests SET lease_until=now()-interval '1 second' WHERE tenant=$1 AND request_id=$2")
        .bind(&i.tenant).bind(key).execute(&store.pool).await.expect("simulate worker crash and elapsed lease");
    let second = match claim_fulfillment(&store, &i, "wh-main", key, &digest)
        .await
        .expect("reclaimed exact intent")
    {
        Claim::Ready { record, lease } => {
            assert_eq!(record.attempts, 2);
            assert_eq!(record.frozen, json!(prepared));
            lease
        }
        _ => panic!("expired lease must be reclaimed"),
    };
    let late = finish_fulfillment(
        &store,
        &i,
        "wh-main",
        key,
        &first,
        Ok(json!({"fixture":"late worker"})),
    )
    .await
    .expect("late worker CAS");
    assert_eq!(late.status, "sending");
    let response = acknowledgement(&prepared, false);
    let completed = finish_fulfillment(&store, &i, "wh-main", key, &second, Ok(response.clone()))
        .await
        .expect("accepted journal receipt");
    assert_eq!(completed.response, Some(response));
    assert!(matches!(
        claim_fulfillment(&store, &i, "wh-main", key, &digest)
            .await
            .expect("completed replay"),
        Claim::Existing(_)
    ));
    let attempts = native_store::attempts(&store, &i, "wh-main", key)
        .await
        .expect("attempt evidence");
    assert_eq!(attempts.len(), 2);
    assert_eq!(attempts[0]["outcome"], "uncertain");
    assert_eq!(attempts[1]["outcome"], "accepted");
    assert!(sqlx::query(
        "UPDATE wms_native_requests SET response='{}'::jsonb WHERE tenant=$1 AND request_id=$2"
    )
    .bind(&i.tenant)
    .bind(key)
    .execute(&store.pool)
    .await
    .is_err());
    assert!(
        sqlx::query("DELETE FROM wms_native_attempts WHERE tenant=$1 AND request_id=$2")
            .bind(&i.tenant)
            .bind(key)
            .execute(&store.pool)
            .await
            .is_err()
    );
    assert_eq!(
        freeze(&store, &i, "conditional-second-key", &prepared)
            .await
            .expect_err("one event/order intent")
            .status,
        409
    );
    let mut other = i.clone();
    other.actor = "fixture:another-operator".into();
    assert_eq!(
        claim_fulfillment(&store, &other, "wh-main", key, &digest)
            .await
            .err()
            .expect("actor binding")
            .status,
        409
    );
    assert_eq!(
        claim_fulfillment(&store, &i, "wh-main", key, &"f".repeat(64))
            .await
            .err()
            .expect("payload binding")
            .status,
        409
    );
}

#[tokio::test]
#[ignore = "requires isolated Postgres; WMS_TEST_DATABASE_URL"]
async fn conditional_journal_requires_actual_committed_event_and_rechecks_normalized_shipment_before_first_send(
) {
    let (store, i) = postgres().await;
    let mut prepared = shipped_fixture(&store, &i).await;
    let real_event = prepared.event_id.clone();
    prepared.event_id = domain::id();
    prepared.payload.event_id = prepared.event_id.clone();

    prepared.digest = payload_digest(&prepared.payload).expect("digest");
    assert!(
        freeze(&store, &i, "conditional-missing-event", &prepared)
            .await
            .is_err(),
        "invented event cannot enter the durable journal"
    );
    prepared.event_id = real_event.clone();
    prepared.payload.event_id = real_event.clone();

    prepared.digest = payload_digest(&prepared.payload).expect("digest");
    let key = "conditional-first-send-guard";
    let digest = freeze(&store, &i, key, &prepared)
        .await
        .expect("real committed event");
    // This synthetic administrative corruption changes normalized evidence
    // while leaving the order JSON intact. The first-send fence must catch it.
    sqlx::query("DELETE FROM wms_allocations WHERE tenant=$1 AND order_id=$2 AND line_id=$3")
        .bind(&i.tenant)
        .bind(&prepared.order_id)
        .bind(format!(
            "oms-line-{:x}",
            Sha256::digest(b"source:source-line-a")
        ))
        .execute(&store.pool)
        .await
        .expect("injected normalized shipment corruption");
    assert_eq!(
        claim_fulfillment(&store, &i, "wh-main", key, &digest)
            .await
            .err()
            .expect("canonical full shipment required")
            .status,
        409
    );
    let record = native_store::lookup(&store, &i, "wh-main", key)
        .await
        .expect("journal")
        .expect("retained intent");
    assert_eq!(record.status, "frozen");
    assert_eq!(record.attempts, 0);
}

#[tokio::test]
#[ignore = "requires isolated Postgres; WMS_TEST_DATABASE_URL"]
async fn only_durable_oms_rejection_releases_intent_for_a_separately_reviewed_request() {
    let (store, i) = postgres().await;
    let prepared = shipped_fixture(&store, &i).await;
    let key = "conditional-rejected-original";
    let digest = freeze(&store, &i, key, &prepared)
        .await
        .expect("original intent");
    let first = match claim_fulfillment(&store, &i, "wh-main", key, &digest)
        .await
        .expect("first send")
    {
        Claim::Ready { lease, .. } => lease,
        _ => panic!("fresh claim"),
    };
    let uncertain = finish_fulfillment(
        &store,
        &i,
        "wh-main",
        key,
        &first,
        Ok(json!({"data":{"status":"unrecognized"}})),
    )
    .await
    .expect("unrecognized acknowledgement remains uncertain");
    assert_eq!(uncertain.status, "uncertain");
    assert!(uncertain.response.is_none());
    assert_eq!(
        freeze(&store, &i, "conditional-unsafe-new-key", &prepared)
            .await
            .expect_err("uncertainty retains unique shipment ownership")
            .status,
        409
    );
    let second = match claim_fulfillment(&store, &i, "wh-main", key, &digest)
        .await
        .expect("exact uncertainty retry")
    {
        Claim::Ready { lease, .. } => lease,
        _ => panic!("reclaimed claim"),
    };
    let response = acknowledgement(&prepared, true);
    let rejected = finish_fulfillment(&store, &i, "wh-main", key, &second, Ok(response.clone()))
        .await
        .expect("exact durable rejection");
    assert_eq!(rejected.status, "rejected");
    assert_eq!(rejected.response, Some(response.clone()));
    match claim_fulfillment(&store, &i, "wh-main", key, &digest)
        .await
        .expect("rejection replay")
    {
        Claim::Existing(record) => {
            assert_eq!(record.status, "rejected");
            assert_eq!(record.attempts, 2);
        }
        _ => panic!("known rejection must never resend"),
    };
    let attempts = native_store::attempts(&store, &i, "wh-main", key)
        .await
        .expect("attempts");
    assert_eq!(attempts[0]["outcome"], "uncertain");
    assert_eq!(attempts[1]["outcome"], "rejected");
    assert!(sqlx::query(
        "UPDATE wms_native_requests SET status='frozen' WHERE tenant=$1 AND request_id=$2"
    )
    .bind(&i.tenant)
    .bind(key)
    .execute(&store.pool)
    .await
    .is_err());
    assert!(sqlx::query(
        "UPDATE wms_native_requests SET response='{}'::jsonb WHERE tenant=$1 AND request_id=$2"
    )
    .bind(&i.tenant)
    .bind(key)
    .execute(&store.pool)
    .await
    .is_err());
    assert!(sqlx::query("UPDATE wms_native_attempts SET outcome='accepted' WHERE tenant=$1 AND request_id=$2 AND outcome='rejected'").bind(&i.tenant).bind(key).execute(&store.pool).await.is_err());
    let mut reviewed = prepared.clone();
    reviewed.payload.request_id = domain::id();
    reviewed.idempotency_key = reviewed.payload.request_id.clone();
    reviewed.payload.expected_updated_at = "2026-10-18T00:00:00Z".into();
    reviewed.digest = payload_digest(&reviewed.payload).expect("new explicitly reviewed predicate");
    let reviewed_digest = freeze(&store, &i, "conditional-reviewed-new-key", &reviewed)
        .await
        .expect("known rejection releases order/event ownership for explicit review");
    assert_ne!(digest, reviewed_digest);
    let app = stateset_warehouse::api::router(App {
        store: store.clone(),
        credentials: Arc::new(vec![("fixture-conditional-api-key".into(), i.clone())]),
        demo: false,
        connections: Connections::default(),
    });
    let original_input = json!({"request_id":key,"expected_version":rejected.expected_version});
    let (status, replay) = authenticated(
        app,
        "POST",
        &format!(
            "/v1/warehouses/wh-main/orders/{}/oms-fulfillment",
            prepared.order_id
        ),
        original_input,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(replay["status"], "rejected");
    assert_eq!(replay["response"], response);
}

#[tokio::test]
#[ignore = "requires isolated Postgres; WMS_TEST_DATABASE_URL"]
async fn journal_completion_revalidates_exact_acknowledgements_and_retains_uncertain_shipment_ownership(
) {
    let (store, i) = postgres().await;
    let prepared = shipped_fixture(&store, &i).await;
    let key = "conditional-completion-boundary";
    let digest = freeze(&store, &i, key, &prepared)
        .await
        .expect("immutable original intent");
    let rejected = acknowledgement(&prepared, true);
    let accepted = acknowledgement(&prepared, false);
    let mut cases = Vec::new();
    let mut missing_time = rejected.clone();
    missing_time["data"]
        .as_object_mut()
        .expect("acknowledgement object")
        .remove("observed_updated_at");
    cases.push(("missing durable rejection timestamp", missing_time));
    for (name, field, value) in [
        (
            "foreign recipient",
            "tenant_id",
            json!("another-oms-tenant"),
        ),
        ("foreign source order", "order_id", json!(domain::id())),
        ("foreign physical event", "event_id", json!(domain::id())),
        ("foreign remote request", "request_id", json!(domain::id())),
        (
            "wrong payload fingerprint",
            "request_digest",
            json!("f".repeat(64)),
        ),
        ("unknown rejection reason", "reason", json!("unrecognized")),
        (
            "contradictory applied rejection",
            "applied_updated_at",
            accepted["data"]["applied_updated_at"].clone(),
        ),
    ] {
        let mut malformed = rejected.clone();
        malformed["data"][field] = value;
        cases.push((name, malformed));
    }
    let mut stale_success = accepted.clone();
    stale_success["data"]["applied_updated_at"] = json!(prepared.payload.expected_updated_at);
    cases.push(("nonadvancing fulfillment revision", stale_success));
    let mut missing_success_time = accepted.clone();
    missing_success_time["data"]
        .as_object_mut()
        .expect("acknowledgement object")
        .remove("applied_updated_at");
    cases.push(("missing fulfillment timestamp", missing_success_time));
    for (name, malformed) in cases {
        let lease = match claim_fulfillment(&store, &i, "wh-main", key, &digest)
            .await
            .expect("exact original claim")
        {
            Claim::Ready { lease, .. } => lease,
            _ => panic!("uncertain request must remain recoverable"),
        };
        // The transport-specific completion helper must not bypass OMS checks.
        let bypass = native_store::finish_transport(
            &store,
            &i,
            "wh-main",
            key,
            &lease,
            Ok(accepted.clone()),
        )
        .await
        .expect("wrong-kind helper is unable to change the OMS intent");
        assert_eq!(bypass.status, "sending");
        assert!(bypass.response.is_none());
        let mut other = i.clone();
        other.actor = "fixture:unrelated-completer".into();
        assert_eq!(
            finish_fulfillment(&store, &other, "wh-main", key, &lease, Ok(accepted.clone()))
                .await
                .expect_err("lease completion remains bound to the original operator")
                .status,
            409
        );
        let result = finish_fulfillment(&store, &i, "wh-main", key, &lease, Ok(malformed))
            .await
            .expect(name);
        assert_eq!(result.status, "uncertain", "{name}");
        assert!(result.response.is_none(), "{name}");
        assert!(!result.last_error.is_empty(), "{name}");
        assert_eq!(
            result.frozen,
            json!(prepared),
            "The exact recoverable request must remain unchanged: {name}"
        );
        assert_eq!(
            freeze(&store, &i, "conditional-forbidden-replacement", &prepared)
                .await
                .expect_err("malformed response must not release physical shipment ownership")
                .status,
            409,
            "{name}"
        );
    }
    let lease = match claim_fulfillment(&store, &i, "wh-main", key, &digest)
        .await
        .expect("recover original once receiver supplies exact evidence")
    {
        Claim::Ready { lease, .. } => lease,
        _ => panic!("recoverable original claim"),
    };
    let completed = finish_fulfillment(&store, &i, "wh-main", key, &lease, Ok(accepted.clone()))
        .await
        .expect("exact accepted acknowledgment");
    assert_eq!(completed.status, "completed");
    assert_eq!(completed.response, Some(accepted));
    let attempts = native_store::attempts(&store, &i, "wh-main", key)
        .await
        .expect("durable attempt evidence");
    assert_eq!(
        attempts
            .iter()
            .filter(|attempt| attempt["outcome"] == "uncertain")
            .count(),
        10
    );
    assert_eq!(
        attempts
            .iter()
            .filter(|attempt| attempt["outcome"] == "accepted")
            .count(),
        1
    );
}

#[derive(Clone)]
struct Wire(Arc<tokio::sync::Mutex<WireState>>);
struct WireState {
    source: Value,
    source_reads: usize,
    capability_reads: usize,
    posts: Vec<(String, Value)>,
    accepted: Option<Value>,
    mutations: usize,
    capability_ok: bool,
}
async fn wire_source(HttpState(wire): HttpState<Wire>, headers: HeaderMap) -> Json<Value> {
    assert_eq!(
        headers["x-stateset-api-key"],
        "fixture-conditional-oms-secret"
    );
    let mut state = wire.0.lock().await;
    state.source_reads += 1;
    Json(json!({"meta":{"raw":"original-source-envelope"},"data":state.source}))
}
async fn wire_capability(HttpState(wire): HttpState<Wire>, headers: HeaderMap) -> Json<Value> {
    assert_eq!(
        headers["x-stateset-api-key"],
        "fixture-conditional-oms-secret"
    );
    let mut state = wire.0.lock().await;
    state.capability_reads += 1;
    Json(
        json!({"data":{"contract":stateset_warehouse::oms_client::CONTRACT,"tenant_id":"oms-fixture","conditional_revision":true,"terminal_state_fence":true,"durable_idempotency":true,"durable_rejections":state.capability_ok}}),
    )
}
async fn wire_fulfill(
    HttpState(wire): HttpState<Wire>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> axum::response::Response {
    assert_eq!(
        headers["x-stateset-api-key"],
        "fixture-conditional-oms-secret"
    );
    let key = headers["idempotency-key"]
        .to_str()
        .expect("idempotency key")
        .to_string();
    let mut state = wire.0.lock().await;
    state.posts.push((key, body.clone()));
    if let Some(accepted) = &state.accepted {
        assert_eq!(
            state.posts[0], state.posts[1],
            "retry must reuse exactly the frozen key and full body"
        );
        let mut response = accepted.clone();
        response["data"]["replayed"] = json!(true);
        return Json(response).into_response();
    }
    state.mutations += 1;
    let payload: FulfillmentPayload =
        serde_json::from_value(body.clone()).expect("contract payload");
    state.accepted = Some(
        json!({"data":{"contract":stateset_warehouse::oms_client::CONTRACT,"tenant_id":"oms-fixture","order_id":body["order_id"],"event_id":body["event_id"],"request_id":body["request_id"],"request_digest":payload_digest(&payload).expect("payload digest"),"status":"fulfilled","applied_updated_at":"2026-10-17T00:00:01Z","replayed":false}}),
    );
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(json!({"error":"Synthetic response loss after atomic OMS acceptance"})),
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
async fn authenticated(app: Router, method: &str, path: &str, body: Value) -> (StatusCode, Value) {
    let response = app
        .oneshot(
            HttpRequest::builder()
                .method(method)
                .uri(path)
                .header("authorization", "Bearer fixture-conditional-api-key")
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
        serde_json::from_slice(&bytes).expect("JSON response"),
    )
}
#[tokio::test]
#[ignore = "requires isolated Postgres and loopback HTTP; WMS_TEST_DATABASE_URL"]
async fn authenticated_conditional_oms_handoff_recovers_remote_commit_after_response_loss_without_second_fulfillment(
) {
    let (store, i) = postgres().await;
    let prepared = shipped_fixture(&store, &i).await;
    let source = json!({"id":prepared.payload.order_id,"orderNumber":"SO-CONDITIONAL","shopifyOrderId":"shopify-fixture","status":"processing","lineItems":[
        {"title":"Synthetic item A","quantity":2,"price":10,"sku":"SKU","variantId":"variant-a","shopifyLineItemId":"source-line-a"},
        {"title":"Synthetic item B","quantity":3,"price":10,"sku":"SKU","variantId":"variant-b","shopifyLineItemId":"source-line-b"}],
        "createdAt":"2026-10-16T00:00:00Z","updatedAt":"2026-10-17T00:00:00Z","unknown":{"originalRawEvidence":[1,null,"retain"]}});
    let wire = Wire(Arc::new(tokio::sync::Mutex::new(WireState {
        source,
        source_reads: 0,
        capability_reads: 0,
        posts: Vec::new(),
        accepted: None,
        mutations: 0,
        capability_ok: false,
    })));
    let remote = Router::new()
        .route("/v1/tenants/oms-fixture/orders/{id}", get(wire_source))
        .route(
            "/v1/tenants/oms-fixture/warehouse-fulfillments",
            get(wire_capability).post(wire_fulfill),
        )
        .with_state(wire.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("conditional OMS fixture listener");
    let base = format!(
        "http://{}",
        listener.local_addr().expect("listener address")
    );
    let server = tokio::spawn(async move {
        axum::serve(listener, remote)
            .await
            .expect("fixture OMS server");
    });
    let _environment = NativeEnvironment(std::env::var_os("WMS_NATIVE_INTEGRATIONS_JSON"));
    std::env::set_var("WMS_NATIVE_INTEGRATIONS_JSON",json!({(i.tenant.clone()):{"oms":{"base_url":base,"tenant_id":"oms-fixture","api_key":"fixture-conditional-oms-secret","shipment_writeback":"conditional_v1"}}}).to_string());
    let app = stateset_warehouse::api::router(App {
        store: store.clone(),
        credentials: Arc::new(vec![("fixture-conditional-api-key".into(), i.clone())]),
        demo: false,
        connections: Connections::default(),
    });
    let path = format!(
        "/v1/warehouses/wh-main/orders/{}/oms-fulfillment",
        prepared.order_id
    );
    let input = json!({"request_id":"conditional-http-original","expected_version":store.state(&i.tenant,"wh-main").await.expect("version").version});
    let mut preflight = input.clone();
    preflight["request_id"] = json!("conditional-preparation-only");
    let (status, error) = authenticated(app.clone(), "POST", &path, preflight).await;
    assert_eq!(status, StatusCode::FAILED_DEPENDENCY, "{error}");
    assert!(
        native_store::lookup(&store, &i, "wh-main", "conditional-preparation-only")
            .await
            .expect("journal lookup")
            .is_none(),
        "GET-only failure must not retain a remote intent"
    );
    {
        let mut remote = wire.0.lock().await;
        assert!(remote.posts.is_empty());
        remote.capability_ok = true;
    }
    let (status, uncertain) = authenticated(app.clone(), "POST", &path, input.clone()).await;
    assert_eq!(status, StatusCode::OK, "{uncertain}");
    assert_eq!(uncertain["status"], "uncertain");
    assert_eq!(
        uncertain["source_snapshot"]["data"]["unknown"]["originalRawEvidence"],
        json!([1, null, "retain"])
    );
    {
        let mut remote = wire.0.lock().await;
        assert_eq!(remote.mutations, 1);
        remote.source["status"] = json!("cancelled");
        remote.source["updatedAt"] = json!("2026-10-18T00:00:00Z");
    }
    let (status, completed) = authenticated(app.clone(), "POST", &path, input.clone()).await;
    assert_eq!(status, StatusCode::OK, "{completed}");
    assert_eq!(completed["status"], "completed");
    assert_eq!(completed["response"]["data"]["replayed"], true);
    let (status, replayed) = authenticated(app.clone(), "POST", &path, input.clone()).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(replayed["response"], completed["response"]);
    let mut different = input;
    different["request_id"] = json!("conditional-http-another");
    assert_eq!(
        authenticated(app.clone(), "POST", &path, different).await.0,
        StatusCode::CONFLICT
    );
    let (status, receipt) = authenticated(
        app,
        "GET",
        "/v1/warehouses/wh-main/native-requests/conditional-http-original",
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(receipt["attempt_history"][0]["outcome"], "uncertain");
    assert_eq!(receipt["attempt_history"][1]["outcome"], "accepted");
    let remote = wire.0.lock().await;
    assert_eq!(
        remote.source_reads, 2,
        "uncertain retry must not refetch changed source"
    );
    assert_eq!(
        remote.capability_reads, 4,
        "receiver tenant/capabilities checked before each POST"
    );
    assert_eq!(remote.posts.len(), 2);
    assert_eq!(remote.mutations, 1);
    assert_eq!(remote.posts[0], remote.posts[1]);
    assert!(
        store
            .reconciliation(&i.tenant, "wh-main")
            .await
            .expect("physical reconciliation")["ok"]
            == true
    );
    server.abort();
}
