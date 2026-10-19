//! The same canonical plan is checked before each positive explicit-observation
//! transaction in this integration suite, including original return/transfer lines.
use super::extended::http;
use super::*;
use axum::Router;
use std::sync::Arc;
fn app(f: &Fixture) -> Router {
    stateset_warehouse::api::router(stateset_warehouse::api::App {
        store: f.store.clone(),
        demo: false,
        connections: stateset_warehouse::integrations::Connections::default(),
        credentials: Arc::new(vec![
            ("plan".into(), f.who.clone()),
            (
                "viewer".into(),
                Identity {
                    role: "viewer".into(),
                    ..f.who.clone()
                },
            ),
            (
                "outsider".into(),
                Identity {
                    tenant: format!("outsider-{}", domain::id()),
                    ..f.who.clone()
                },
            ),
        ]),
    })
}
pub(super) async fn verify(f: &Fixture, warehouse: &str, r: &Request) {
    let command = json!(&r.command);
    if ![
        "count_stock",
        "move_stock",
        "receive",
        "pick",
        "unpick",
        "create_return",
        "pack_shipment",
        "dispatch_transfer",
        "receive_transfer",
        "record_transfer_loss",
        "reverse_transfer_loss",
    ]
    .contains(&command["type"].as_str().expect("canonical command type"))
    {
        return;
    }
    let before = f.facts().await;
    let (status, page) = http(
        &app(f),
        "POST",
        &format!("/v1/warehouses/{warehouse}/serial-observation-plan"),
        "plan",
        Some(json!({"plan_id":r.request_id,"command":command})),
    )
    .await;
    assert_eq!(status, 200, "{page}");
    assert_eq!(page["plan_id"], r.request_id);
    assert_eq!(page["warehouse_id"], warehouse);
    assert_eq!(page["command_type"], command["type"]);
    let mut expected: Vec<_> = r
        .serial_inputs
        .iter()
        .map(|i| json!({"target":i.target,"sku":i.sku,"each_quantity":i.serials.len()}))
        .collect();
    let mut actual = page["targets"]
        .as_array()
        .expect("original observation groups")
        .clone();
    expected.sort_by_key(Value::to_string);
    actual.sort_by_key(Value::to_string);
    assert_eq!(actual, expected);
    assert_eq!(
        f.facts().await,
        before,
        "preparing labels cannot create execution evidence"
    );
}
#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn observation_plans_resolve_original_targets_without_saving_and_enforce_role_scope_and_bounds(
) {
    let f = Fixture::new().await;
    f.catalog().await;
    let app = app(&f);
    let before = f.facts().await;
    let path = "/v1/warehouses/wh-main/serial-observation-plan";
    let command = json!({"type":"count_stock","sku":"SERIAL","lot":"LOT","location":"A","counted":0,"reason":"A witnessed empty count still requires an explicit group"});
    let body = json!({"plan_id":domain::id(),"command":command});
    let (status, page) = http(&app, "POST", path, "plan", Some(body.clone())).await;
    assert_eq!(status, 200, "{page}");
    assert_eq!(
        page["targets"],
        json!([{"target":[],"sku":"SERIAL","each_quantity":0}])
    );
    assert_eq!(page["required_labels"], 0);
    for (token, expected) in [("viewer", 403), ("outsider", 404)] {
        assert_eq!(
            http(&app, "POST", path, token, Some(body.clone())).await.0,
            expected
        );
    }
    let mut invalid = body.clone();
    invalid["plan_id"] = json!("invalid");
    assert_eq!(http(&app, "POST", path, "plan", Some(invalid)).await.0, 422);
    let mut large = body.clone();
    large["command"]["counted"] = json!(10001);
    assert_eq!(http(&app, "POST", path, "plan", Some(large)).await.0, 422);
    for c in [
        json!({"type":"receive","receipt_id":"missing","line_id":"missing","location":"RECV-01","quantity":1}),
        json!({"type":"move_stock","sku":"MISSING","lot":"LOT","from":"A","to":"B","quantity":1}),
    ] {
        assert_eq!(
            http(
                &app,
                "POST",
                path,
                "plan",
                Some(json!({"plan_id":domain::id(),"command":c}))
            )
            .await
            .0,
            404
        );
    }
    let duplicate = json!({"type":"dispatch_transfer","reference":"Invalid duplicate target","destination_warehouse_id":"wh-other","carrier":"Synthetic","handoff_reference":"SYNTHETIC","lines":[{"id":"SAME","sku":"SERIAL","lot":"LOT","location":"A","quantity":1},{"id":"SAME","sku":"SERIAL","lot":"LOT","location":"A","quantity":1}]});
    assert_eq!(
        http(
            &app,
            "POST",
            path,
            "plan",
            Some(json!({"plan_id":domain::id(),"command":duplicate}))
        )
        .await
        .0,
        422
    );
    assert_eq!(f.facts().await, before);
}
