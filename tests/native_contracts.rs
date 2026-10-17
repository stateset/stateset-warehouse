//! Fixtures mirror OMS domain/mod.rs::Order and Freight tests/workflows.rs::plan.
//! These test adapter contracts and wire requests, not a running external service.
use axum::{
    extract::State as HttpState,
    http::{HeaderMap, StatusCode},
    routing::{get, post},
    Json, Router,
};
use serde_json::{json, Value};
use stateset_warehouse::{
    domain::{self, Command},
    native::{self, ImportDetails, NativeIntegrations, OmsOrder, TransportPlan},
};
use std::{collections::BTreeSet, sync::Arc};
use tokio::sync::Mutex;

const SOURCE: &str = "00000000-0000-4000-8000-000000000999";
fn oms_fixture() -> Value {
    json!({"id":SOURCE,"shopifyOrderId":"shopify:41001","orderNumber":"SO-NATIVE-1","status":"processing","lineItems":[{"title":"Everyday Tee","quantity":2.0,"price":19.99,"sku":"SKU-1","variantId":"v-1","shopifyLineItemId":"line-a"},{"title":"Everyday Tee","quantity":3.0,"price":19.99,"sku":"SKU-1","variantId":"v-1","shopifyLineItemId":"line-b"}],"createdAt":"2026-10-10T12:00:00Z","updatedAt":"2026-10-11T12:00:00Z","tags":[],"locationId":"warehouse-pdx","fulfillmentStatus":"UNFULFILLED"})
}
fn configured(base: &str) -> NativeIntegrations {
    NativeIntegrations::from_json(&json!({"acme":{"oms":{"base_url":base,"tenant_id":"oms-acme","api_key":"fixture-oms-key"},"tms":{"base_url":base,"tenant_id":"freight-acme","bearer_token":"fixture-tms-operations-key"}}}).to_string()).expect("native configuration")
}

#[test]
fn full_order_import_rejects_partial_or_unknown_fulfillment_in_canonical_and_source_snapshots() {
    for status in [
        "PARTIALLY_FULFILLED",
        "FULFILLED",
        "SHIPPED",
        "DELIVERED",
        "UNKNOWN",
    ] {
        for field in ["canonical", "source", "source-camel"] {
            let mut source = oms_fixture();
            if field == "canonical" {
                source["fulfillmentStatus"] = json!(status);
            } else if field == "source" {
                source["sourceOrderPayload"] = json!({"fulfillment_status":status});
            } else {
                source["sourceOrderPayload"] = json!({"fulfillmentStatus":status});
            }
            let order = serde_json::from_value(source).expect("canonical source fixture");
            assert!(
                configured("http://localhost:1")
                    .import_command("acme", &order, &details())
                    .is_err(),
                "unsafe {field} fulfillment {status}"
            );
        }
    }
    let safe = serde_json::from_value(oms_fixture()).expect("unfulfilled source");
    assert!(configured("http://localhost:1")
        .import_command("acme", &safe, &details())
        .is_ok());
}
#[test]
fn raw_source_cancellation_cannot_create_new_full_order_work() {
    for field in ["cancelled_at", "cancelledAt"] {
        for value in [json!("2026-10-17T01:00:00Z"), json!(true), json!(" ")] {
            let mut source = oms_fixture();
            source["sourceOrderPayload"] = json!({});
            source["sourceOrderPayload"][field] = value;
            let order = serde_json::from_value(source).expect("active canonical source");
            assert!(configured("http://localhost:1")
                .import_command("acme", &order, &details())
                .is_err());
        }
        for value in [json!(null), json!("")] {
            let mut source = oms_fixture();
            source["sourceOrderPayload"] = json!({});
            source["sourceOrderPayload"][field] = value;
            let order = serde_json::from_value(source).expect("uncancelled source");
            assert!(configured("http://localhost:1")
                .import_command("acme", &order, &details())
                .is_ok());
        }
    }
}
fn details() -> ImportDetails {
    ImportDetails {
        customer: "Synthetic customer".into(),
        destination: "Warehouse operator verified destination".into(),
        priority: "normal".into(),
    }
}
fn transport_plan() -> TransportPlan {
    // Field names and supported limits are taken from Freight NewLoad and its
    // source-derived plan fixture. Actual values are synthetic.
    serde_json::from_value(json!({"reference":"FR-WMS-NATIVE-1","customer_id":"c1","mode":"FTL","equipment":"Dry van","commodity":"Consumer goods","weight_lbs":24000,"miles":180,"origin":{"facility":"Portland DC","city":"Portland","state":"OR","date":"2026-10-20","address_lines":["15 Dock Street"],"postal_code":"97201","country":"US","contact":"Dock one","phone":"555-0100","instructions":"Scheduled readiness only"},"destination":{"facility":"Seattle DC","city":"Seattle","state":"WA","date":"2026-10-21","address_lines":["25 Receiving Ave"],"postal_code":"98101","country":"US","contact":"Receiving","phone":"555-0101","instructions":"Appointment to be confirmed by TMS"},"buy_cents":123456,"sell_cents":150000})).expect("native NewLoad fixture")
}
fn packed() -> (domain::State, String) {
    let mut state = domain::demo_state();
    let order = state
        .orders
        .iter_mut()
        .find(|o| o.status == "packed")
        .expect("packed synthetic fixture");
    order.source_order_id = Some(format!("oms:oms-acme:{SOURCE}"));
    let mut fixture = oms_fixture();
    fixture["lineItems"] = json!([{"title":"Bottle","quantity":order.lines[0].quantity,"price":25.0,"sku":order.lines[0].sku,"variantId":null,"shopifyLineItemId":"line-a"}]);
    let import = configured("http://localhost:1")
        .import_command(
            "acme",
            &serde_json::from_value(fixture).expect("packed source"),
            &details(),
        )
        .expect("packed source identity");
    let line_id = serde_json::to_value(import).expect("import")["lines"][0]["id"]
        .as_str()
        .expect("line identity")
        .to_string();
    order.lines[0].id = line_id.clone();
    for allocation in &mut order.allocations {
        allocation.line_id = line_id.clone();
    }
    let id = order.id.clone();
    state.validate().expect("valid synthetic packed state");
    (state, id)
}
fn packed_oms_fixture() -> Value {
    let mut fixture = oms_fixture();
    fixture["lineItems"] = json!([{"title":"Bottle","quantity":8.0,"price":25.0,"sku":"SS-BTL-005","variantId":null,"shopifyLineItemId":"line-a"}]);
    fixture
}

#[test]
fn quantity_decimal_validation_rejects_fractional_each_without_float_rounding() {
    for (raw, expected) in [
        ("1", 1),
        ("2.0", 2),
        ("1e6", 1_000_000),
        ("1000e-3", 1),
        ("0.001e3", 1),
        ("123.0000", 123),
    ] {
        let number = serde_json::from_str(raw).expect("JSON decimal");
        assert_eq!(
            native::whole_each(&number).expect("whole each"),
            expected,
            "{raw}"
        );
    }
    for raw in [
        "0",
        "-1",
        "1.5",
        "0.0001",
        "1000001",
        "1.00000000000000000000001",
        "999999.99999999999999999",
        "1e-1000",
        "1e1001",
    ] {
        let number = serde_json::from_str(raw).expect("JSON decimal");
        assert!(
            native::whole_each(&number).is_err(),
            "{raw} must not become a whole each"
        );
    }
}
#[test]
fn import_preserves_two_line_owners_for_the_same_sku_and_scopes_source_identity() {
    let integration = configured("http://localhost:1");
    let order: OmsOrder = serde_json::from_value(oms_fixture()).expect("actual OMS order schema");
    let command = integration
        .import_command("acme", &order, &details())
        .expect("typed native import");
    let value = serde_json::to_value(&command).expect("command");
    assert_eq!(value["source_order_id"], format!("oms:oms-acme:{SOURCE}"));
    let lines = value["lines"].as_array().expect("lines");
    assert_eq!(lines.len(), 2);
    assert_ne!(lines[0]["id"], lines[1]["id"]);
    assert_eq!(lines[0]["sku"], lines[1]["sku"]);
    assert_eq!(lines[0]["quantity"], 2);
    assert_eq!(lines[1]["quantity"], 3);
}
#[test]
fn native_import_rejects_missing_sku_duplicate_line_ids_fractions_and_terminal_orders() {
    let integration = configured("http://localhost:1");
    let mut cases = Vec::new();
    for status in [
        "cancelled",
        "returned",
        "shipped",
        "fulfilled",
        "delivered",
        "unknown",
    ] {
        let mut value = oms_fixture();
        value["status"] = json!(status);
        cases.push(value);
    }
    let mut missing = oms_fixture();
    missing["lineItems"][0]["sku"] = Value::Null;
    cases.push(missing);
    let mut duplicate = oms_fixture();
    duplicate["lineItems"][1]["shopifyLineItemId"] = json!("line-a");
    cases.push(duplicate);
    let mut fraction = oms_fixture();
    fraction["lineItems"][0]["quantity"] =
        serde_json::from_str("1.00000000000000001").expect("precise fraction");
    cases.push(fraction);
    let mut tenant = oms_fixture();
    tenant["tenantId"] = json!("another-company");
    cases.push(tenant);
    for case in cases {
        let order = serde_json::from_value(case).expect("source schema");
        assert!(integration
            .import_command("acme", &order, &details())
            .is_err());
    }
}
#[test]
fn source_line_ids_remain_stable_when_upstream_with_line_ids_changes_ordering() {
    let integration = configured("http://localhost:1");
    let mut first = oms_fixture();
    let a = integration
        .import_command(
            "acme",
            &serde_json::from_value(first.clone()).expect("order"),
            &details(),
        )
        .expect("import");
    first["lineItems"].as_array_mut().expect("lines").reverse();
    let b = integration
        .import_command(
            "acme",
            &serde_json::from_value(first).expect("order"),
            &details(),
        )
        .expect("import");
    let a = serde_json::to_value(a).expect("command");
    let b = serde_json::to_value(b).expect("command");
    assert_eq!(a["lines"][0], b["lines"][1]);
}
#[test]
fn integration_configuration_binds_each_tenant_without_secrets_in_summary_or_errors() {
    let integration = configured("http://localhost:1");
    let summary = integration.summary("acme").to_string();
    assert!(!summary.contains("fixture-oms-key"));
    assert!(!summary.contains("fixture-tms-operations-key"));
    assert_eq!(integration.summary("other")["oms"]["configured"], false);
    assert!(NativeIntegrations::from_json(r#"{"acme":{"oms":{"base_url":"http://remote.example","tenant_id":"acme","api_key":"secret"}}}"#).is_err());
    assert!(NativeIntegrations::from_json(r#"{"acme":{"oms":{"base_url":"https://example.test","tenant_id":"../another-tenant","api_key":"secret"}}}"#).is_err());
    assert!(NativeIntegrations::from_json(
        r#"{"acme":{"tms":{"base_url":"https://freight.example.test","bearer_token":"secret"}}}"#
    )
    .is_err());
}
#[test]
fn preparation_freezes_exact_native_transport_body_and_scoped_idempotency_key() {
    let integration = configured("http://localhost:1");
    let (state, id) = packed();
    let plan = transport_plan();
    let first = integration
        .prepare_transport("acme", &state, &id, &plan)
        .expect("ready transport");
    let second = integration
        .prepare_transport("acme", &state, &id, &plan)
        .expect("same transport");
    assert_eq!(first.idempotency_key, second.idempotency_key);
    assert_eq!(first.payload, json!({"order_id":SOURCE,"load":plan}));
    assert_eq!(first.readiness.kind, "warehouse.order_ready");
    assert_eq!(first.readiness.warehouse_status, "packed");
    assert!(first.payload.get("pickup").is_none());
    assert!(first.payload.get("delivered_at").is_none());
    let restored: native::PreparedTransport =
        serde_json::from_value(serde_json::to_value(&first).expect("journal value"))
            .expect("recover frozen plan");
    assert_eq!(restored.payload, first.payload);
    assert!(integration
        .prepare_transport("other", &state, &id, &plan)
        .is_err());
}
#[test]
fn readiness_rejects_unpacked_and_foreign_oms_source_orders() {
    let integration = configured("http://localhost:1");
    let (mut state, id) = packed();
    state
        .orders
        .iter_mut()
        .find(|o| o.id == id)
        .expect("order")
        .source_order_id = Some(format!("oms:foreign:{SOURCE}"));
    assert!(integration
        .prepare_transport("acme", &state, &id, &transport_plan())
        .is_err());
    let pending = state
        .orders
        .iter()
        .find(|o| o.status == "pending")
        .expect("pending order");
    assert!(native::ready_export("acme", &state, &pending.id).is_err());
}
#[test]
fn freight_plans_require_explicit_native_fields_and_reject_invented_status_commands() {
    let plan = transport_plan();
    plan.validate().expect("supported Freight NewLoad");
    let mut value = serde_json::to_value(plan).expect("plan");
    value["status"] = json!("in_transit");
    assert!(serde_json::from_value::<TransportPlan>(value).is_err());
    for field in [
        "customer_id",
        "buy_cents",
        "sell_cents",
        "weight_lbs",
        "miles",
    ] {
        let mut value = serde_json::to_value(transport_plan()).expect("plan");
        value.as_object_mut().expect("object").remove(field);
        assert!(
            serde_json::from_value::<TransportPlan>(value).is_err(),
            "operator must supply {field}"
        );
    }
    let mut invalid = transport_plan();
    invalid.destination.date = invalid.origin.date.pred_opt().expect("previous date");
    assert!(invalid.validate().is_err());
    let mut invalid = transport_plan();
    invalid.weight_lbs = 0;
    assert!(invalid.validate().is_err());
    let mut invalid = transport_plan();
    invalid.origin.facility = "bad~facility".into();
    assert!(invalid.validate().is_err());
}

#[derive(Clone)]
struct Capture {
    requests: Arc<Mutex<Vec<(String, HeaderMap, Value)>>>,
    order: Value,
}
async fn serve(router: Router) -> (String, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("local native-contract HTTP listener");
    let address = listener.local_addr().expect("listener address");
    let task = tokio::spawn(async move {
        axum::serve(listener, router)
            .await
            .expect("contract mock server");
    });
    (format!("http://{address}"), task)
}
async fn get_order(HttpState(c): HttpState<Capture>, headers: HeaderMap) -> Json<Value> {
    c.requests
        .lock()
        .await
        .push(("GET order".into(), headers, Value::Null));
    Json(json!({"meta":{"requestId":"fixture-request"},"data":c.order}))
}
async fn list_orders(HttpState(c): HttpState<Capture>, headers: HeaderMap) -> Json<Value> {
    c.requests
        .lock()
        .await
        .push(("GET page".into(), headers, Value::Null));
    Json(
        json!({"meta":{"requestId":"fixture-request"},"data":{"items":[c.order],"pagination":{"page":1,"pageSize":100,"totalItems":1,"totalPages":1,"hasNext":false,"hasPrevious":false}}}),
    )
}
async fn import_transport(
    HttpState(c): HttpState<Capture>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Json<Value> {
    let mut requests = c.requests.lock().await;
    let replayed = !requests.is_empty();
    requests.push(("POST /v1/sync/import".into(), headers, body.clone()));
    Json(
        json!({"replayed":replayed,"data":{"id":"load_fixture_1","reference":body["load"]["reference"],"sync_order_id":body["order_id"],"status":"planned","source_order":c.order}}),
    )
}
async fn freight_access(headers: HeaderMap) -> Json<Value> {
    assert_eq!(
        headers["authorization"],
        "Bearer fixture-tms-operations-key"
    );
    Json(
        json!({"tenant_id":"freight-acme","access":{"subject":"fixture:operations","role":"operations","capabilities":["workspace.read","operations.write"]}}),
    )
}
#[tokio::test]
async fn wire_oms_requests_use_tenant_paths_api_key_and_actual_response_envelopes() {
    let capture = Capture {
        requests: Arc::new(Mutex::new(Vec::new())),
        order: oms_fixture(),
    };
    let router = Router::new()
        .route("/v1/tenants/oms-acme/orders", get(list_orders))
        .route("/v1/tenants/oms-acme/orders/{id}", get(get_order))
        .with_state(capture.clone());
    let (base, task) = serve(router).await;
    let integration = configured(&base);
    let page = integration
        .list_orders("acme", 1)
        .await
        .expect("native OMS list");
    assert_eq!(page.items.len(), 1);
    assert!(!page.pagination.has_next);
    let order = integration
        .fetch_order("acme", SOURCE)
        .await
        .expect("native OMS get");
    assert_eq!(order.id, SOURCE);
    assert!(integration
        .fetch_order("another-company", SOURCE)
        .await
        .is_err());
    assert!(integration
        .fetch_order("acme", "../another-company")
        .await
        .is_err());
    let requests = capture.requests.lock().await;
    assert_eq!(requests.len(), 2);
    for (_, headers, _) in requests.iter() {
        assert_eq!(headers["x-stateset-api-key"], "fixture-oms-key");
        assert!(!headers.contains_key("authorization"));
    }
    task.abort();
}
#[tokio::test]
async fn wire_tms_uses_source_verified_import_and_reuses_frozen_key_on_retry() {
    let capture = Capture {
        requests: Arc::new(Mutex::new(Vec::new())),
        order: packed_oms_fixture(),
    };
    let (base, task) = serve(
        Router::new()
            .route("/v1/sync/import", post(import_transport))
            .route("/v1/access", get(freight_access))
            .with_state(capture.clone()),
    )
    .await;
    let integration = configured(&base);
    let (state, id) = packed();
    let prepared = integration
        .prepare_transport("acme", &state, &id, &transport_plan())
        .expect("prepared");
    let first = integration
        .execute_transport("acme", &prepared)
        .await
        .expect("first native import");
    let second = integration
        .execute_transport("acme", &prepared)
        .await
        .expect("recovered native import");
    assert_eq!(first["data"], second["data"]);
    assert_eq!(second["replayed"], true);
    let requests = capture.requests.lock().await;
    assert_eq!(requests.len(), 2);
    for (route, headers, body) in requests.iter() {
        assert_eq!(route, "POST /v1/sync/import");
        assert_eq!(
            headers["authorization"],
            "Bearer fixture-tms-operations-key"
        );
        assert_eq!(headers["idempotency-key"], prepared.idempotency_key);
        assert_eq!(body, &prepared.payload);
    }
    drop(requests);
    let mut tampered = prepared.clone();
    tampered.tenant = "another-company".into();
    assert!(integration
        .execute_transport("acme", &tampered)
        .await
        .is_err());
    let mut tampered = prepared.clone();
    tampered.payload["order_id"] = json!("00000000-0000-4000-8000-000000000001");
    assert!(integration
        .execute_transport("acme", &tampered)
        .await
        .is_err());
    assert_eq!(capture.requests.lock().await.len(), 2);
    task.abort();
}
async fn rotated_freight_access(headers: HeaderMap) -> Json<Value> {
    let token = headers["authorization"].to_str().expect("bearer token");
    Json(
        json!({"tenant_id":if token=="Bearer fixture-wrong-tenant"{"other-freight-tenant"}else{"freight-acme"},"access":{"capabilities":if token=="Bearer fixture-readonly"{vec!["workspace.read"]}else{vec!["workspace.read","operations.write"]}}}),
    )
}
#[tokio::test]
async fn retained_transport_pins_recipient_authority_and_allows_only_same_tenant_secret_rotation() {
    let capture = Capture {
        requests: Arc::new(Mutex::new(Vec::new())),
        order: packed_oms_fixture(),
    };
    let (base, task) = serve(
        Router::new()
            .route("/v1/sync/import", post(import_transport))
            .route("/v1/access", get(rotated_freight_access))
            .with_state(capture.clone()),
    )
    .await;
    let integration = configured(&base);
    let (state, id) = packed();
    let prepared = integration
        .prepare_transport("acme", &state, &id, &transport_plan())
        .expect("frozen authority");
    assert_eq!(prepared.recipient_base_url, base);
    assert_eq!(prepared.recipient_tenant_id, "freight-acme");
    integration
        .execute_transport("acme", &prepared)
        .await
        .expect("first authority acceptance");
    let mut changed = integration.clone();
    changed
        .tenants
        .get_mut("acme")
        .expect("tenant")
        .tms
        .as_mut()
        .expect("TMS")
        .base_url = "http://127.0.0.1:1/another-authority".into();
    assert!(changed
        .execute_transport("acme", &prepared)
        .await
        .expect_err("new endpoint must not receive original intent")
        .contains("authority changed"));
    let mut changed = integration.clone();
    changed
        .tenants
        .get_mut("acme")
        .expect("tenant")
        .tms
        .as_mut()
        .expect("TMS")
        .tenant_id = "other-tenant".into();
    assert!(changed
        .execute_transport("acme", &prepared)
        .await
        .expect_err("new tenant must not receive original intent")
        .contains("authority changed"));
    assert_eq!(capture.requests.lock().await.len(), 1);
    let mut rotated = integration.clone();
    let config = rotated
        .tenants
        .get_mut("acme")
        .expect("tenant")
        .tms
        .as_mut()
        .expect("TMS");
    config.bearer_token = "fixture-rotated-secret".into();
    config.base_url = format!("{base}/");
    let replay = rotated
        .execute_transport("acme", &prepared)
        .await
        .expect("same authority with rotated credential");
    assert_eq!(replay["replayed"], true);
    for token in ["fixture-wrong-tenant", "fixture-readonly"] {
        let mut changed = integration.clone();
        changed
            .tenants
            .get_mut("acme")
            .expect("tenant")
            .tms
            .as_mut()
            .expect("TMS")
            .bearer_token = token.into();
        assert!(changed.execute_transport("acme", &prepared).await.is_err());
    }
    let requests = capture.requests.lock().await;
    assert_eq!(requests.len(), 2);
    assert_eq!(
        requests[0].1["idempotency-key"],
        requests[1].1["idempotency-key"]
    );
    assert_eq!(requests[0].2, requests[1].2);
    task.abort();
}
#[tokio::test]
async fn new_transport_preflight_checks_canonical_oms_lines_and_rejects_cancelled_source() {
    for (mutation, should_pass) in [("none", true), ("quantity", false), ("cancelled", false)] {
        let mut order = packed_oms_fixture();
        if mutation == "quantity" {
            order["lineItems"][0]["quantity"] = json!(9);
        }
        if mutation == "cancelled" {
            order["status"] = json!("cancelled");
        }
        let capture = Capture {
            requests: Arc::new(Mutex::new(Vec::new())),
            order,
        };
        let (base, task) = serve(
            Router::new()
                .route("/v1/tenants/oms-acme/orders/{id}", get(get_order))
                .route("/v1/access", get(freight_access))
                .with_state(capture.clone()),
        )
        .await;
        let integration = configured(&base);
        let (state, id) = packed();
        let prepared = integration
            .prepare_transport("acme", &state, &id, &transport_plan())
            .expect("frozen plan");
        assert_eq!(
            integration
                .verify_transport_source("acme", &prepared)
                .await
                .is_ok(),
            should_pass,
            "{mutation}"
        );
        let requests = capture.requests.lock().await;
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].0, "GET order");
        task.abort();
    }
}
#[tokio::test]
async fn tms_retained_source_mismatch_is_reported_as_uncertain_instead_of_accepted() {
    let capture = Capture {
        requests: Arc::new(Mutex::new(Vec::new())),
        order: oms_fixture(),
    };
    let (base, task) = serve(
        Router::new()
            .route("/v1/sync/import", post(import_transport))
            .route("/v1/access", get(freight_access))
            .with_state(capture.clone()),
    )
    .await;
    let integration = configured(&base);
    let (state, id) = packed();
    let prepared = integration
        .prepare_transport("acme", &state, &id, &transport_plan())
        .expect("frozen plan");
    let error = integration
        .execute_transport("acme", &prepared)
        .await
        .expect_err("different source lines cannot confirm handoff");
    assert!(error.contains("acceptance is uncertain"));
    assert_eq!(capture.requests.lock().await.len(), 1);
    task.abort();
}
#[tokio::test]
async fn native_http_rejects_redirects_bad_envelopes_and_oversized_responses() {
    for (status, body) in [
        (StatusCode::FOUND, "redirect".to_owned()),
        (StatusCode::OK, "{\"data\":{\"ok\":true}}".to_owned()),
        (
            StatusCode::OK,
            "x".repeat(native::MAX_NATIVE_RESPONSE_BYTES + 1),
        ),
    ] {
        let router = Router::new().route(
            "/v1/tenants/oms-acme/orders/{id}",
            get(move || {
                let body = body.clone();
                async move {
                    (
                        status,
                        [(
                            "location",
                            "https://credentials-must-not-follow.example.test",
                        )],
                        body,
                    )
                }
            }),
        );
        let (base, task) = serve(router).await;
        assert!(configured(&base).fetch_order("acme", SOURCE).await.is_err());
        task.abort();
    }
}
#[tokio::test]
async fn oms_handoff_remains_held_because_native_mutation_has_no_atomic_source_guard() {
    let integration = configured("http://localhost:1");
    let (mut state, id) = packed();
    let mut fixture = oms_fixture();
    fixture["lineItems"] = json!([{"title":"Bottle","quantity":8.0,"price":25.0,"sku":"SS-BTL-005","variantId":null,"shopifyLineItemId":"line-a"}]);
    let command = integration
        .import_command(
            "acme",
            &serde_json::from_value(fixture.clone()).expect("order"),
            &details(),
        )
        .expect("native command");
    let line_id = serde_json::to_value(command).expect("command")["lines"][0]["id"]
        .as_str()
        .expect("line ID")
        .to_string();
    let order = state
        .orders
        .iter_mut()
        .find(|o| o.id == id)
        .expect("packed order");
    order.lines[0].id = line_id.clone();
    for allocation in &mut order.allocations {
        allocation.line_id = line_id.clone();
    }
    state = state
        .execute(
            &Command::Ship {
                order_id: id.clone(),
                carrier: "Physical carrier".into(),
                handoff_reference: "Signed dock release 42".into(),
            },
            "fixture:operator",
        )
        .expect("physical handoff")
        .0;
    let capture = Capture {
        requests: Arc::new(Mutex::new(Vec::new())),
        order: fixture,
    };
    let (base, task) = serve(
        Router::new()
            .route("/v1/tenants/oms-acme/orders/{id}", get(get_order))
            .with_state(capture.clone()),
    )
    .await;
    let integration = configured(&base);
    let order = state
        .orders
        .iter()
        .find(|o| o.id == id)
        .expect("shipped order");
    let proposal = integration
        .prepare_oms_fulfillment("acme", &state.warehouse.id, "event-1", order)
        .await
        .expect("source-checked held proposal");
    assert_eq!(proposal["state"], "held");
    assert_eq!(proposal["payload"]["status"], "FULFILLED");
    assert_eq!(
        proposal["payload"]["fulfillmentData"]["handoff_reference"],
        "Signed dock release 42"
    );
    assert_eq!(
        integration
            .dispatch_oms_shipment("acme", &state.warehouse.id, "event-1", &json!(order))
            .await
            .expect_err("unsafe upstream mutation must stay held"),
        native::OMS_WRITEBACK_HOLD
    );
    let methods: BTreeSet<_> = capture
        .requests
        .lock()
        .await
        .iter()
        .map(|r| r.0.clone())
        .collect();
    assert_eq!(methods, BTreeSet::from(["GET order".to_owned()]));
    task.abort();
}
