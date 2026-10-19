use serde_json::{json, Value};
use stateset_warehouse::{
    domain,
    icommerce::{self, Mappings, Preview},
};

fn move_input(quantity: &str) -> Value {
    json!({"kind":"move_inventory","input":{"from_location_id":7,"to_location_id":9,"sku":"SKU-1","lot_id":null,"quantity":quantity,"reason":null,"performed_by":null},
        "mappings":{"locations":{"7":"Receiving-Original","9":"BIN-Exact"}}})
}
fn preview(value: Value) -> Preview {
    serde_json::from_value(value).expect("upstream model")
}

#[test]
fn upstream_move_converts_exact_whole_units_and_preserves_ids() {
    let command = preview(move_input("12.000"))
        .command()
        .expect("whole units");
    assert_eq!(
        serde_json::to_value(command).expect("command"),
        json!({"type":"move_stock","sku":"SKU-1","lot":"UNTRACKED","from":"Receiving-Original","to":"BIN-Exact","quantity":12})
    );
}
#[test]
fn quantities_are_never_rounded_or_clamped() {
    for quantity in ["1.5", "-1", "0", "1000001", "79228162514264337593543950335"] {
        assert!(
            preview(move_input(quantity)).command().is_err(),
            "{quantity}"
        );
    }
}

#[test]
fn overprecision_and_numeric_json_cannot_round_fractions_into_whole_units() {
    for quantity in [
        "1.00000000000000000000000000001",
        "0.00000000000000000000000000001",
        "-0.00000000000000000000000000001",
    ] {
        assert!(
            serde_json::from_value::<Preview>(move_input(quantity)).is_err(),
            "{quantity}"
        );
    }
    for quantity in [json!(1), json!(1.0), json!(0.00001)] {
        let mut value = move_input("1");
        value["input"]["quantity"] = quantity;
        assert!(serde_json::from_value::<Preview>(value).is_err());
    }
    for field in ["initial_quantity", "reorder_point", "safety_stock"] {
        let mut value = json!({"kind":"create_inventory_item","category":"Parts","input":{"sku":"SKU-1","name":"Original"}});
        value["input"][field] = json!("0.00000000000000000000000000001");
        assert!(serde_json::from_value::<Preview>(value).is_err(), "{field}");
    }
    let original = preview(move_input("1.0000000000000000000000000000"));
    let wire = serde_json::to_string(&original).expect("actual upstream serializer");
    let decoded: Preview = serde_json::from_str(&wire).expect("strict upstream roundtrip");
    assert_eq!(
        serde_json::to_value(original.command().expect("exact whole")).expect("JSON"),
        serde_json::to_value(decoded.command().expect("same whole")).expect("JSON")
    );
}
#[test]
fn normalization_unmapped_ids_aliases_and_actor_overrides_are_rejected() {
    let original = move_input("1");
    for (pointer, value) in [
        ("/input/sku", json!(" SKU-1 ")),
        ("/input/from_location_id", json!(44)),
        (
            "/input/lot_id",
            json!("89b70a01-a367-4dde-9aac-2a85b53047bd"),
        ),
        ("/input/performed_by", json!("another:actor")),
        ("/input/reason", json!("Unmapped upstream evidence")),
        ("/mappings/locations/7", json!("BIN-Exact")),
        ("/mappings/locations/9", json!(" BIN-Exact ")),
    ] {
        let mut changed = original.clone();
        *changed.pointer_mut(pointer).expect("field") = value;
        assert!(preview(changed).command().is_err(), "{pointer}");
    }
}
#[test]
fn lot_mapping_retains_the_original_wms_literal() {
    let mut value = move_input("1");
    value["input"]["lot_id"] = json!("89b70a01-a367-4dde-9aac-2a85b53047bd");
    value["mappings"]["lots"] = json!({"89b70a01-a367-4dde-9aac-2a85b53047bd":"Lot-Original-É"});
    let command = serde_json::to_value(preview(value).command().expect("mapped")).expect("json");
    assert_eq!(command["lot"], "Lot-Original-É");
}
#[test]
fn upstream_product_validation_cannot_introduce_invisible_stock() {
    let input = json!({"kind":"create_inventory_item","category":"Parts","input":{"sku":"SKU-1","name":"Original product","reorder_point":"2","initial_quantity":"0","unit_of_measure":"EA"}});
    let command =
        serde_json::to_value(preview(input.clone()).command().expect("product")).expect("json");
    assert_eq!(
        command,
        json!({"type":"create_product","sku":"SKU-1","name":"Original product","category":"Parts","reorder_point":2})
    );
    for (field, value) in [
        ("initial_quantity", json!("1")),
        ("reorder_point", json!("1.5")),
        ("unit_of_measure", json!("kg")),
        ("description", json!("Must not disappear")),
        ("location_id", json!(7)),
        ("safety_stock", json!("3")),
    ] {
        let mut changed = input.clone();
        changed["input"][field] = value;
        assert!(preview(changed).command().is_err(), "{field}");
    }
}
#[test]
fn inventory_uses_upstream_types_without_mutating_original_lot_positions() {
    let state = domain::demo_state();
    let before = serde_json::to_value(&state).expect("state");
    let mappings = Mappings {
        locations: state
            .locations
            .iter()
            .enumerate()
            .map(|(index, location)| ((index + 1) as i32, location.code.clone()))
            .collect(),
        lots: Default::default(),
    };
    let rows = icommerce::inventory(&state, &mappings).expect("typed inventory");
    assert_eq!(rows.len(), state.products.len());
    for row in rows {
        let stocks: Vec<_> = state
            .stocks
            .iter()
            .filter(|stock| stock.sku == row.sku)
            .collect();
        assert_eq!(
            row.total_on_hand.to_string(),
            stocks
                .iter()
                .map(|stock| stock.on_hand)
                .sum::<i64>()
                .to_string()
        );
        assert_eq!(
            row.total_allocated.to_string(),
            stocks
                .iter()
                .map(|stock| stock.reserved)
                .sum::<i64>()
                .to_string()
        );
        assert_eq!(row.total_available, row.total_on_hand - row.total_allocated);
    }
    assert_eq!(serde_json::to_value(&state).expect("state"), before);
    assert!(icommerce::inventory(&state, &Mappings::default()).is_err());
}

#[test]
fn unsupported_nested_fields_and_noncanonical_mapping_keys_fail_at_decode() {
    for field in ["actor", "serial_controlled", "unit_inputs", "quantity_each"] {
        let mut value = move_input("1");
        value["input"][field] = json!("must not disappear");
        assert!(serde_json::from_value::<Preview>(value).is_err(), "{field}");
    }
    for key in ["07", "+7", "0x7", "2147483648"] {
        let mut value = move_input("1");
        value["mappings"]["locations"][key] = json!("OTHER");
        assert!(serde_json::from_value::<Preview>(value).is_err(), "{key}");
    }
    let mut value = move_input("1");
    value["mappings"]["lots"] = json!({"89B70A01-A367-4DDE-9AAC-2A85B53047BD":"LOT"});
    assert!(serde_json::from_value::<Preview>(value).is_err());
}

#[test]
fn duplicate_wire_fields_never_replace_the_original_mapping_or_quantity() {
    let wire = serde_json::to_string(&move_input("1")).expect("wire");
    for changed in [
        wire.replace(
            "\"7\":\"Receiving-Original\"",
            "\"7\":\"Receiving-Original\",\"7\":\"OTHER\"",
        ),
        wire.replace(
            "\"quantity\":\"1\"",
            "\"quantity\":\"1\",\"quantity\":\"2\"",
        ),
    ] {
        assert_ne!(wire, changed);
        assert!(serde_json::from_str::<Preview>(&changed).is_err());
    }
}

fn application(store: stateset_warehouse::store::Store, tenant: &str) -> axum::Router {
    use stateset_warehouse::{
        api::{self, App},
        integrations::Connections,
        store::Identity,
    };
    api::router(App {
        store,
        demo: false,
        connections: Connections::default(),
        credentials: std::sync::Arc::new(
            [
                ("admin", tenant, "operator:original", "admin"),
                ("viewer", tenant, "reader", "viewer"),
                ("other-actor", tenant, "operator:other", "admin"),
                (
                    "other-tenant",
                    "icommerce-isolation-other",
                    "operator:original",
                    "admin",
                ),
            ]
            .into_iter()
            .map(|(key, tenant, actor, role)| {
                (
                    key.into(),
                    Identity {
                        tenant: tenant.into(),
                        actor: actor.into(),
                        role: role.into(),
                    },
                )
            })
            .collect(),
        ),
    })
}
async fn http(app: &axum::Router, token: &str, path: &str, body: Value) -> (u16, Value) {
    use http_body_util::BodyExt;
    use tower::ServiceExt;
    let mut request = axum::http::Request::builder()
        .method("POST")
        .uri(path)
        .header("content-type", "application/json");
    if !token.is_empty() {
        request = request.header("authorization", format!("Bearer {token}"));
    }
    let response = app
        .clone()
        .oneshot(
            request
                .body(axum::body::Body::from(body.to_string()))
                .expect("request"),
        )
        .await
        .expect("response");
    let status = response.status().as_u16();
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
async fn preview_authorization_and_strict_inputs_are_enforced_before_database_access() {
    let app = application(
        stateset_warehouse::store::Store {
            pool: sqlx::postgres::PgPoolOptions::new()
                .connect_lazy("postgres://unused@127.0.0.1:1/unused")
                .expect("lazy pool"),
        },
        "icommerce-lazy",
    );
    let path = "/v1/warehouses/wh-main/icommerce/preview";
    assert_eq!(http(&app, "", path, move_input("1")).await.0, 401);
    assert_eq!(http(&app, "viewer", path, move_input("1")).await.0, 403);
    let mut changed = move_input("1");
    changed["input"]["actor"] = json!("forged");
    assert_eq!(http(&app, "admin", path, changed).await.0, 422);
    assert_eq!(http(&app, "admin", path, move_input("1.5")).await.0, 422);
    let (status, error) = http(
        &app,
        "admin",
        path,
        move_input("1.00000000000000000000000000001"),
    )
    .await;
    assert_eq!(status, 422);
    assert!(error["error"]
        .as_str()
        .expect("canonical JSON error")
        .contains("exact decimal precision"));
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn typed_previews_are_read_only_tenant_scoped_and_commit_only_through_original_wms_requests()
{
    use stateset_warehouse::store::{Identity, Request, Store};
    let url = std::env::var("WMS_TEST_DATABASE_URL").expect("isolated PostgreSQL URL");
    assert!(reqwest::Url::parse(&url)
        .expect("URL")
        .path()
        .contains("test"));
    let store = Store::connect(&url).await.expect("current schema");
    let who = Identity {
        tenant: format!("icommerce-{}", domain::id()),
        actor: "operator:original".into(),
        role: "admin".into(),
    };
    store
        .provision(&who.tenant, false)
        .await
        .expect("isolated tenant");
    store
        .provision("icommerce-isolation-other", false)
        .await
        .expect("separate tenant");
    let app = application(store.clone(), &who.tenant);
    let path = "/v1/warehouses/wh-main/icommerce/preview";
    let input = json!({"kind":"create_inventory_item","category":"Parts","input":{"sku":"SKU-1","name":"Original product","reorder_point":"2","unit_of_measure":"EA"}});
    let before = serde_json::to_value(store.state(&who.tenant, "wh-main").await.expect("state"))
        .expect("json");
    let (status, preview) = http(&app, "admin", path, input.clone()).await;
    assert_eq!(status, 200, "{preview}");
    assert_eq!(preview["committed"], false);
    assert_eq!(preview["tenant"], who.tenant);
    assert_eq!(preview["actor"], who.actor);
    assert_eq!(preview["upstream_revision"], icommerce::UPSTREAM_REVISION);
    assert_eq!(
        serde_json::to_value(store.state(&who.tenant, "wh-main").await.expect("state"))
            .expect("json"),
        before
    );
    assert_eq!(
        http(&app, "admin", path, input).await.1["reviewed_version"],
        0
    );
    let request: Request = serde_json::from_value(json!({"request_id":domain::id(),"expected_version":preview["reviewed_version"],"command":preview["command"]})).expect("canonical request");
    let committed = store
        .execute(&who, "wh-main", &request)
        .await
        .expect("canonical product execution");
    assert_eq!(committed["replayed"], false);
    for command in [
        json!({"type":"create_location","code":"BIN-Exact","zone":"Storage","kind":"storage"}),
        json!({"type":"count_stock","sku":"SKU-1","lot":"UNTRACKED","location":"RECV-01","counted":2,"reason":"Observed opening whole units"}),
    ] {
        let request: Request = serde_json::from_value(json!({"request_id":domain::id(),"expected_version":store.state(&who.tenant,"wh-main").await.expect("state").version,"command":command})).expect("request");
        store
            .execute(&who, "wh-main", &request)
            .await
            .expect("original WMS inventory authority");
    }
    let mut movement = move_input("1.000");
    movement["mappings"]["locations"]["7"] = json!("RECV-01");
    let before = serde_json::to_value(store.state(&who.tenant, "wh-main").await.expect("state"))
        .expect("json");
    let (status, reviewed) = http(&app, "admin", path, movement.clone()).await;
    assert_eq!(status, 200, "{reviewed}");
    assert_eq!(reviewed["serial_observation_required"], false);
    assert_eq!(
        serde_json::to_value(store.state(&who.tenant, "wh-main").await.expect("state"))
            .expect("json"),
        before
    );
    let (status, _) = http(&app, "other-tenant", path, movement).await;
    assert_eq!(status, 422, "another tenant has no original SKU/location");
    let move_request = json!({"request_id":domain::id(),"expected_version":reviewed["reviewed_version"],"command":reviewed["command"]});
    let route = "/v1/warehouses/wh-main/commands";
    let (status, moved) = http(&app, "admin", route, move_request.clone()).await;
    assert_eq!(status, 200, "{moved}");
    let after = serde_json::to_value(store.state(&who.tenant, "wh-main").await.expect("state"))
        .expect("json");
    let (status, replay) = http(&app, "admin", route, move_request.clone()).await;
    assert_eq!(status, 200, "{replay}");
    assert_eq!(replay["replayed"], true);
    assert_eq!(replay["result"], moved["result"]);
    assert_eq!(http(&app, "other-actor", route, move_request).await.0, 409);
    assert_eq!(
        serde_json::to_value(store.state(&who.tenant, "wh-main").await.expect("state"))
            .expect("json"),
        after
    );
    let (status, export) = http(
        &app,
        "viewer",
        "/v1/warehouses/wh-main/icommerce/inventory",
        json!({"locations":{"7":"RECV-01","9":"BIN-Exact","10":"STAGE-01"}}),
    )
    .await;
    assert_eq!(status, 200, "{export}");
    assert_eq!(export["read_only"], true);
    assert_eq!(export["source_positions"], after["stocks"]);
    assert_eq!(
        serde_json::to_value(store.state(&who.tenant, "wh-main").await.expect("state"))
            .expect("json"),
        after
    );
    assert_eq!(
        store
            .reconciliation(&who.tenant, "wh-main")
            .await
            .expect("physical proof")["ok"],
        true
    );
}
