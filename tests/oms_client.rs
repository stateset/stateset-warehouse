//! Wire-level conditional OMS handoff contracts. The companion receiver has
//! separate real PostgreSQL proofs; this fixture checks WMS dispatch behavior.
use axum::{extract::State, http::HeaderMap, routing::get, Json, Router};
use chrono::{DateTime, Duration};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use stateset_warehouse::{
    domain::{Allocation, Order, OrderLine},
    native::{self, NativeIntegrations},
    oms_client::{FulfillmentLine, FulfillmentPayload, PreparedFulfillment, CONTRACT},
};
use std::{collections::BTreeMap, sync::Arc};
use tokio::sync::Mutex;

const SOURCE: &str = "10000000-0000-4000-8000-000000000001";
const ORDER: &str = "20000000-0000-4000-8000-000000000002";
const EVENT: &str = "30000000-0000-4000-8000-000000000003";
const REQUEST: &str = "40000000-0000-4000-8000-000000000004";
const SOURCE_REVISION: &str = "2026-10-10T12:00:00+00:00";

fn line_id(id: &str) -> String {
    format!(
        "oms-line-{:x}",
        Sha256::digest(format!("source:{id}").as_bytes())
    )
}
fn source_order() -> Value {
    json!({"id":SOURCE,"tenantId":"oms-acme","shopifyOrderId":"shopify:accepted-order",
      "orderNumber":"SO-CONDITIONAL-1","status":"processing",
      "lineItems":[
        {"title":"Cap","quantity":2.0,"price":19.95,"sku":"SKU-A","variantId":null,"shopifyLineItemId":"a"},
        {"title":"Cap","quantity":3.0,"price":19.95,"sku":"SKU-A","variantId":null,"shopifyLineItemId":"b"}],
      "createdAt":"2026-10-01T12:00:00Z","updatedAt":SOURCE_REVISION})
}
fn configured(base: &str, enabled: bool) -> NativeIntegrations {
    let mut connection =
        json!({"base_url":base,"tenant_id":"oms-acme","api_key":"fixture-oms-key"});
    if enabled {
        connection["shipment_writeback"] = json!("conditional_v1");
    }
    NativeIntegrations::from_json(&json!({"acme":{"oms":connection}}).to_string())
        .expect("configuration")
}
fn shipped() -> Order {
    let lines = vec![
        OrderLine {
            id: line_id("a"),
            sku: "SKU-A".into(),
            quantity: 2,
        },
        OrderLine {
            id: line_id("b"),
            sku: "SKU-A".into(),
            quantity: 3,
        },
    ];
    let allocations = lines
        .iter()
        .map(|line| Allocation {
            line_id: line.id.clone(),
            sku: line.sku.clone(),
            lot: "LOT-1".into(),
            location: "A-01".into(),
            quantity: line.quantity,
            picked: line.quantity,
            shipped: line.quantity,
            released: 0,
        })
        .collect();
    Order {
        id: ORDER.into(),
        reference: "SO-CONDITIONAL-1".into(),
        source_order_id: Some(format!("oms:oms-acme:{SOURCE}")),
        customer: "Verified customer".into(),
        destination: "Verified destination".into(),
        priority: "normal".into(),
        status: "shipped".into(),
        lines,
        allocations,
        carrier: "Physical carrier".into(),
        handoff_reference: "Signed handoff 42".into(),
        created_at: "2026-10-02T12:00:00Z".into(),
        shipped_at: Some("2026-10-12T12:00:00Z".into()),
    }
}

// Independent receiver-style canonicalization rather than calling the client
// fingerprint implementation under test. Array and timestamp spelling remain.
fn canonical(value: &Value) -> String {
    match value {
        Value::Object(object) => {
            let mut keys: Vec<_> = object.keys().collect();
            keys.sort();
            format!(
                "{{{}}}",
                keys.into_iter()
                    .map(|key| format!(
                        "{}:{}",
                        serde_json::to_string(key).expect("key"),
                        canonical(&object[key])
                    ))
                    .collect::<Vec<_>>()
                    .join(",")
            )
        }
        Value::Array(array) => format!(
            "[{}]",
            array.iter().map(canonical).collect::<Vec<_>>().join(",")
        ),
        _ => serde_json::to_string(value).expect("JSON scalar"),
    }
}
fn digest(payload: &FulfillmentPayload) -> String {
    format!(
        "{:x}",
        Sha256::digest(canonical(&serde_json::to_value(payload).expect("payload")).as_bytes())
    )
}
fn prepared(base: &str) -> PreparedFulfillment {
    let order = shipped();
    let mut lines: Vec<_> = order
        .lines
        .into_iter()
        .map(|line| FulfillmentLine {
            line_id: line.id,
            sku: line.sku,
            quantity_each: line.quantity,
        })
        .collect();
    lines.sort_by(|a, b| a.line_id.cmp(&b.line_id));
    let payload = FulfillmentPayload {
        request_id: REQUEST.into(),
        order_id: SOURCE.into(),
        event_id: EVENT.into(),
        expected_updated_at: SOURCE_REVISION.into(),
        warehouse_id: "wh-main".into(),
        warehouse_order_id: ORDER.into(),
        carrier: order.carrier,
        handoff_reference: order.handoff_reference,
        handed_off_at: order.shipped_at.expect("shipped timestamp"),
        lines,
    };
    PreparedFulfillment {
        tenant: "acme".into(),
        warehouse_id: "wh-main".into(),
        order_id: ORDER.into(),
        event_id: EVENT.into(),
        recipient_base_url: base.into(),
        recipient_tenant_id: "oms-acme".into(),
        idempotency_key: REQUEST.into(),
        digest: digest(&payload),
        payload,
    }
}
#[derive(Debug, Clone)]
struct Seen {
    method: &'static str,
    headers: HeaderMap,
    body: Value,
}
#[derive(Clone)]
struct Capture {
    requests: Arc<Mutex<Vec<Seen>>>,
    source: Arc<Mutex<Value>>,
    capabilities: Arc<Mutex<Value>>,
    acknowledgement_mutation: Arc<Mutex<Option<(String, Value)>>>,
    rejection_reason: Arc<Mutex<Option<String>>>,
    receipts: Arc<Mutex<BTreeMap<String, Value>>>,
}
impl Capture {
    fn new() -> Self {
        Self {
            requests: Arc::new(Mutex::new(Vec::new())),
            source: Arc::new(Mutex::new(source_order())),
            capabilities: Arc::new(Mutex::new(
                json!({"contract":CONTRACT,"tenant_id":"oms-acme",
                "conditional_revision":true,"terminal_state_fence":true,"durable_idempotency":true,"durable_rejections":true}),
            )),
            acknowledgement_mutation: Arc::new(Mutex::new(None)),
            rejection_reason: Arc::new(Mutex::new(None)),
            receipts: Arc::new(Mutex::new(BTreeMap::new())),
        }
    }
    async fn posts(&self) -> Vec<Seen> {
        self.requests
            .lock()
            .await
            .iter()
            .filter(|request| request.method == "POST handoff")
            .cloned()
            .collect()
    }
}
async fn get_source(State(capture): State<Capture>, headers: HeaderMap) -> Json<Value> {
    capture.requests.lock().await.push(Seen {
        method: "GET order",
        headers,
        body: Value::Null,
    });
    Json(json!({"data":capture.source.lock().await.clone()}))
}
async fn capabilities(State(capture): State<Capture>, headers: HeaderMap) -> Json<Value> {
    capture.requests.lock().await.push(Seen {
        method: "GET capabilities",
        headers,
        body: Value::Null,
    });
    Json(json!({"data":capture.capabilities.lock().await.clone()}))
}
async fn handoff(
    State(capture): State<Capture>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Json<Value> {
    capture.requests.lock().await.push(Seen {
        method: "POST handoff",
        headers,
        body: body.clone(),
    });
    let mut receipts = capture.receipts.lock().await;
    let key = body["request_id"].as_str().expect("request");
    let mut response = if let Some(prior) = receipts.get(key) {
        let mut prior = prior.clone();
        prior["replayed"] = json!(true);
        prior
    } else {
        let time =
            DateTime::parse_from_rfc3339(body["expected_updated_at"].as_str().expect("revision"))
                .expect("revision timestamp")
                + Duration::microseconds(1);
        let mut response = json!({"contract":CONTRACT,"tenant_id":"oms-acme","order_id":body["order_id"],
            "event_id":body["event_id"],"request_id":body["request_id"],"request_digest":format!("{:x}",Sha256::digest(canonical(&body).as_bytes())),
            "status":"fulfilled","replayed":false,"applied_updated_at":time.to_rfc3339()});
        if let Some(reason) = capture.rejection_reason.lock().await.clone() {
            response
                .as_object_mut()
                .expect("acknowledgement")
                .remove("applied_updated_at");
            response["status"] = json!("rejected");
            response["reason"] = json!(reason);
            response["observed_updated_at"] = json!(SOURCE_REVISION);
        }
        receipts.insert(key.into(), response.clone());
        response
    };
    if let Some((field, replacement)) = capture.acknowledgement_mutation.lock().await.clone() {
        response[&field] = replacement;
    }
    Json(json!({"data":response}))
}
async fn serve(capture: Capture) -> (String, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("local protocol HTTP listener");
    let address = listener.local_addr().expect("listener address");
    let router = Router::new()
        .route("/v1/tenants/oms-acme/orders/{id}", get(get_source))
        .route(
            "/v1/tenants/oms-acme/warehouse-fulfillments",
            get(capabilities).post(handoff),
        )
        .with_state(capture);
    let task = tokio::spawn(async move {
        axum::serve(listener, router)
            .await
            .expect("protocol fixture");
    });
    (format!("http://{address}"), task)
}

#[tokio::test]
async fn conditional_fulfillment_is_explicit_opt_in_and_legacy_writeback_stays_held() {
    let capture = Capture::new();
    let (base, task) = serve(capture.clone()).await;
    let integration = configured(&base, false);
    let error = integration
        .prepare_conditional_fulfillment("acme", "wh-main", EVENT, &shipped())
        .await
        .expect_err("held by default");
    assert_eq!(error, native::OMS_WRITEBACK_HOLD);
    assert!(integration
        .execute_conditional_fulfillment("acme", &prepared(&base))
        .await
        .is_err());
    assert!(
        capture.requests.lock().await.is_empty(),
        "No network request is necessary without explicit opt in"
    );
    task.abort();
}

#[tokio::test]
async fn missing_contract_guarantees_or_foreign_tenant_never_receive_a_post() {
    for (field, replacement) in [
        ("contract", json!("unconditional.legacy")),
        ("conditional_revision", json!(false)),
        ("terminal_state_fence", json!(false)),
        ("durable_idempotency", json!(false)),
        ("durable_rejections", json!(false)),
        ("durable_rejections", Value::Null),
        ("tenant_id", json!("other-company")),
    ] {
        let capture = Capture::new();
        capture.capabilities.lock().await[field] = replacement;
        let (base, task) = serve(capture.clone()).await;
        assert!(
            configured(&base, true)
                .execute_conditional_fulfillment("acme", &prepared(&base))
                .await
                .is_err(),
            "{field}"
        );
        assert!(
            capture.posts().await.is_empty(),
            "{field} must fail before POST"
        );
        task.abort();
    }
}

#[tokio::test]
async fn retained_recipient_cannot_be_rebound_and_same_tenant_secret_rotation_is_allowed() {
    let capture = Capture::new();
    let (base, task) = serve(capture.clone()).await;
    let original = prepared(&base);
    for (changed_base, changed_tenant) in [
        (format!("{base}/different-recipient"), "oms-acme"),
        (base.clone(), "other-company"),
    ] {
        let mut integration = configured(&base, true);
        let cfg = integration
            .tenants
            .get_mut("acme")
            .expect("tenant")
            .oms
            .as_mut()
            .expect("OMS");
        cfg.base_url = changed_base;
        cfg.tenant_id = changed_tenant.into();
        assert!(integration
            .execute_conditional_fulfillment("acme", &original)
            .await
            .expect_err("recipient pin")
            .contains("authority changed"));
    }
    assert!(configured(&base, true)
        .execute_conditional_fulfillment("foreign-company", &original)
        .await
        .is_err());
    assert!(capture.requests.lock().await.is_empty());
    let mut rotated = configured(&format!("{base}/"), true);
    rotated
        .tenants
        .get_mut("acme")
        .expect("tenant")
        .oms
        .as_mut()
        .expect("OMS")
        .api_key = "fixture-rotated-key".into();
    rotated
        .execute_conditional_fulfillment("acme", &original)
        .await
        .expect("same recipient with rotated key");
    let requests = capture.requests.lock().await;
    assert_eq!(requests.len(), 2);
    for request in requests.iter() {
        assert_eq!(request.headers["x-stateset-api-key"], "fixture-rotated-key");
    }
    task.abort();
}

#[tokio::test]
async fn prepare_preserves_same_sku_line_owners_and_exact_physical_handoff() {
    let capture = Capture::new();
    let (base, task) = serve(capture.clone()).await;
    let integration = configured(&base, true);
    let (raw, frozen) = integration
        .prepare_conditional_fulfillment("acme", "wh-main", EVENT, &shipped())
        .await
        .expect("prepare");
    assert_eq!(raw["data"], source_order());
    assert_eq!(frozen.payload.order_id, SOURCE);
    assert_eq!(frozen.payload.warehouse_order_id, ORDER);
    assert_eq!(frozen.payload.handoff_reference, "Signed handoff 42");
    assert_eq!(frozen.payload.lines.len(), 2);
    assert_ne!(
        frozen.payload.lines[0].line_id,
        frozen.payload.lines[1].line_id
    );
    assert_eq!(frozen.digest, digest(&frozen.payload));
    assert!(
        capture.posts().await.is_empty(),
        "Preparation has no remote mutation"
    );
    integration
        .execute_conditional_fulfillment("acme", &frozen)
        .await
        .expect("dispatch");
    let posts = capture.posts().await;
    assert_eq!(posts.len(), 1);
    assert_eq!(posts[0].headers["idempotency-key"], frozen.idempotency_key);
    assert_eq!(frozen.payload.request_id, frozen.idempotency_key);
    assert_ne!(frozen.payload.request_id, frozen.payload.event_id);
    assert_eq!(
        posts[0].body,
        serde_json::to_value(&frozen.payload).expect("payload")
    );
    task.abort();
}

#[tokio::test]
async fn exact_frozen_retry_does_not_refresh_source_revision_after_later_source_changes() {
    let capture = Capture::new();
    let (base, task) = serve(capture.clone()).await;
    let integration = configured(&base, true);
    let (_, frozen) = integration
        .prepare_conditional_fulfillment("acme", "wh-main", EVENT, &shipped())
        .await
        .expect("prepare");
    let first = integration
        .execute_conditional_fulfillment("acme", &frozen)
        .await
        .expect("first delivery");
    {
        let mut source = capture.source.lock().await;
        source["status"] = json!("cancelled");
        source["updatedAt"] = json!("2026-10-13T12:00:00Z");
        source["lineItems"][0]["quantity"] = json!(999);
    }
    let restored: PreparedFulfillment =
        serde_json::from_value(serde_json::to_value(&frozen).expect("journal"))
            .expect("restore journal");
    let replay = integration
        .execute_conditional_fulfillment("acme", &restored)
        .await
        .expect("recover original acknowledgement");
    assert_eq!(first["data"]["replayed"], false);
    assert_eq!(replay["data"]["replayed"], true);
    let posts = capture.posts().await;
    assert_eq!(posts.len(), 2);
    assert_eq!(posts[0].body, posts[1].body);
    assert_eq!(
        posts[0].headers["idempotency-key"],
        posts[1].headers["idempotency-key"]
    );
    assert_eq!(posts[1].body["expected_updated_at"], SOURCE_REVISION);
    assert_eq!(
        capture
            .requests
            .lock()
            .await
            .iter()
            .filter(|r| r.method == "GET order")
            .count(),
        1
    );
    task.abort();
}

#[tokio::test]
async fn wrong_acknowledgement_scope_fingerprint_status_or_revision_is_uncertain() {
    let capture = Capture::new();
    let (base, task) = serve(capture.clone()).await;
    let integration = configured(&base, true);
    let frozen = prepared(&base);
    for (field, replacement) in [
        ("contract", json!("legacy")),
        ("tenant_id", json!("foreign")),
        ("order_id", json!(ORDER)),
        ("event_id", json!(ORDER)),
        ("request_id", json!(ORDER)),
        ("request_digest", json!("0".repeat(64))),
        ("status", json!("processing")),
        ("replayed", json!("true")),
        ("applied_updated_at", json!(SOURCE_REVISION)),
        ("applied_updated_at", json!("invalid")),
    ] {
        *capture.acknowledgement_mutation.lock().await = Some((field.into(), replacement));
        let error = integration
            .execute_conditional_fulfillment("acme", &frozen)
            .await
            .expect_err("bad acknowledgement");
        assert!(
            error.contains("uncertain") || error.contains("retain the original"),
            "{field}: {error}"
        );
    }
    assert_eq!(capture.posts().await.len(), 10);
    task.abort();
}

#[tokio::test]
async fn durable_rejection_replays_unchanged_and_fresh_review_gets_a_separate_request_identity() {
    let capture = Capture::new();
    *capture.rejection_reason.lock().await = Some("source_revision_changed".into());
    let (base, task) = serve(capture.clone()).await;
    let integration = configured(&base, true);
    let original = prepared(&base);
    let first = integration
        .execute_conditional_fulfillment("acme", &original)
        .await
        .expect("durable negative acknowledgement");
    assert_eq!(first["data"]["status"], "rejected");
    assert_eq!(first["data"]["reason"], "source_revision_changed");
    *capture.rejection_reason.lock().await = None;
    capture.source.lock().await["updatedAt"] = json!("2026-10-14T12:00:00Z");
    let recovered = integration
        .execute_conditional_fulfillment("acme", &original)
        .await
        .expect("original negative receipt replay");
    assert_eq!(recovered["data"]["status"], "rejected");
    assert_eq!(recovered["data"]["replayed"], true);
    let (_, reviewed) = integration
        .prepare_conditional_fulfillment("acme", "wh-main", EVENT, &shipped())
        .await
        .expect("new explicit canonical review");
    assert_eq!(reviewed.event_id, original.event_id);
    assert_ne!(reviewed.payload.request_id, original.payload.request_id);
    assert_eq!(reviewed.idempotency_key, reviewed.payload.request_id);
    let accepted = integration
        .execute_conditional_fulfillment("acme", &reviewed)
        .await
        .expect("separately reviewed request");
    assert_eq!(accepted["data"]["status"], "fulfilled");
    let posts = capture.posts().await;
    assert_eq!(posts.len(), 3);
    assert_eq!(posts[0].body, posts[1].body);
    assert_eq!(
        posts[0].headers["idempotency-key"],
        posts[1].headers["idempotency-key"]
    );
    assert_ne!(
        posts[1].headers["idempotency-key"],
        posts[2].headers["idempotency-key"]
    );
    task.abort();
}

#[tokio::test]
async fn unscoped_or_malformed_rejection_never_authorizes_a_replacement() {
    let capture = Capture::new();
    *capture.rejection_reason.lock().await = Some("source_revision_changed".into());
    let (base, task) = serve(capture.clone()).await;
    let integration = configured(&base, true);
    let frozen = prepared(&base);
    for (field, replacement) in [
        ("tenant_id", json!("foreign")),
        ("order_id", json!(ORDER)),
        ("event_id", json!(ORDER)),
        ("request_id", json!(ORDER)),
        ("request_digest", json!("0".repeat(64))),
        ("reason", json!("unrecognized-rejection")),
        ("reason", Value::Null),
        ("observed_updated_at", json!("invalid")),
        ("observed_updated_at", Value::Null),
        ("replayed", Value::Null),
    ] {
        *capture.acknowledgement_mutation.lock().await = Some((field.into(), replacement));
        let error = integration
            .execute_conditional_fulfillment("acme", &frozen)
            .await
            .expect_err("rejection must be exact durable evidence");
        assert!(
            error.contains("uncertain") || error.contains("retain the original"),
            "{field}: {error}"
        );
    }
    assert_eq!(capture.posts().await.len(), 10);
    task.abort();
}

#[tokio::test]
async fn fractional_or_changed_canonical_source_and_terminal_source_cannot_start_an_intent() {
    for mutation in [
        "fraction",
        "quantity",
        "line_identity",
        "tenant",
        "terminal",
        "partial",
        "unknown_fulfillment",
    ] {
        let capture = Capture::new();
        {
            let mut source = capture.source.lock().await;
            match mutation {
                "fraction" => {
                    source["lineItems"][0]["quantity"] =
                        serde_json::from_str("2.00000000000000000001").expect("exact JSON fraction")
                }
                "quantity" => source["lineItems"][0]["quantity"] = json!(3),
                "line_identity" => {
                    source["lineItems"][0]["shopifyLineItemId"] = json!("another-line")
                }
                "tenant" => source["tenantId"] = json!("foreign"),
                "terminal" => source["status"] = json!("cancelled"),
                "partial" => source["fulfillmentStatus"] = json!("PARTIALLY_FULFILLED"),
                "unknown_fulfillment" => {
                    source["fulfillmentStatus"] = json!("an-unrecognized-forward-state")
                }
                _ => unreachable!(),
            }
        }
        let (base, task) = serve(capture.clone()).await;
        assert!(
            configured(&base, true)
                .prepare_conditional_fulfillment("acme", "wh-main", EVENT, &shipped())
                .await
                .is_err(),
            "{mutation}"
        );
        assert!(capture.posts().await.is_empty());
        task.abort();
    }
}

#[test]
fn retained_handoff_rejects_invalid_provider_fields_even_with_a_recomputed_fingerprint() {
    let valid = prepared("http://127.0.0.1:1");
    valid
        .validate("acme")
        .expect("valid retained protocol input");
    for case in [
        "warehouse",
        "carrier",
        "handoff",
        "sku",
        "line_identity",
        "uuid",
    ] {
        let mut invalid = valid.clone();
        match case {
            "warehouse" => {
                invalid.warehouse_id = "x".repeat(101);
                invalid.payload.warehouse_id = invalid.warehouse_id.clone();
            }
            "carrier" => invalid.payload.carrier = "Carrier\nInjected value".into(),
            "handoff" => invalid.payload.handoff_reference = "x".repeat(241),
            "sku" => invalid.payload.lines[0].sku = "x".repeat(101),
            "line_identity" => invalid.payload.lines[0].line_id = "oms-line-not-a-sha256".into(),
            "uuid" => {
                invalid.event_id = "3ABCDEF0-0000-4000-8000-000000000003".into();
                invalid.payload.event_id = invalid.event_id.clone();
            }
            _ => unreachable!(),
        }
        invalid.digest = digest(&invalid.payload);
        assert!(
            invalid.validate("acme").is_err(),
            "{case} must fail before a durable unsendable intent is created"
        );
    }
}
