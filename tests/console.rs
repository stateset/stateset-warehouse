use axum::{
    body::Body,
    http::{Request, StatusCode},
    Router,
};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use stateset_warehouse::{
    api::{self, App},
    console::{self, Assets, Page},
    domain::{self, Location, Stock},
    integrations::Connections,
    store::{Identity, Store},
};
use std::sync::Arc;
use tower::ServiceExt;

fn assets(tenant: &str) -> Assets {
    Assets::parse(&serde_json::to_vec(&json!({"schema_version":1,"facilities":[{"tenant":tenant,"warehouse_id":"wh-pdx","layout":{"name":"Synthetic plan","unit":"grid","width":100,"height":50,"bins":[{"code":"RECV-01","x":0,"y":0,"width":40,"height":40}]},"product_images":{}}]})).expect("valid isolated console fixture"), std::env::temp_dir()).expect("valid isolated console fixture")
}
#[test]
fn console_quantities_are_exact_strings_and_pages_are_bound_to_the_review() {
    let mut state = domain::demo_state();
    state.stocks = vec![
        Stock {
            sku: "A".into(),
            lot: "L".into(),
            location: "RECV-01".into(),
            on_hand: 9_007_199_254_740_991,
            reserved: 3,
        },
        Stock {
            sku: "B".into(),
            lot: "L".into(),
            location: "RECV-01".into(),
            on_hand: 9,
            reserved: 0,
        },
    ];
    let page = Page {
        limit: Some(1),
        ..Page::default()
    };
    let first = console::project(&state, true, "tenant-a", &Assets::empty(), &page)
        .expect("valid isolated console fixture");
    assert_eq!(first["stock"]["on_hand_each"], "9007199254741000");
    assert_eq!(first["tenant"], "tenant-a");
    assert_eq!(first["unit"], "each");
    assert_eq!(
        first["bins"]["items"]
            .as_array()
            .expect("valid isolated console fixture")
            .len(),
        1
    );
    let next = Page {
        limit: Some(1),
        after: Some(
            first["bins"]["next_after"]
                .as_str()
                .expect("valid isolated console fixture")
                .into(),
        ),
        review: Some(state.version.to_string()),
    };
    let second = console::project(&state, true, "tenant-a", &Assets::empty(), &next)
        .expect("valid isolated console fixture");
    assert!(
        second["bins"]["items"][0]["code"].as_str() > first["bins"]["items"][0]["code"].as_str()
    );
    state.version += 1;
    assert_eq!(
        console::project(&state, true, "tenant-a", &Assets::empty(), &next)
            .expect_err("changed or held console evidence must reject")
            .status,
        409
    );
    assert_eq!(
        console::project(
            &state,
            false,
            "tenant-a",
            &Assets::empty(),
            &Page::default()
        )
        .expect_err("changed or held console evidence must reject")
        .status,
        409
    );
}
#[test]
fn floor_plans_never_substitute_tenants_or_silently_keep_removed_bins() {
    let mut state = domain::demo_state();
    let config = assets("tenant-a");
    let own = console::project(&state, true, "tenant-a", &config, &Page::default())
        .expect("valid isolated console fixture");
    assert_eq!(own["floor_plan"]["available"], true);
    let other = console::project(&state, true, "tenant-b", &config, &Page::default())
        .expect("valid isolated console fixture");
    assert_eq!(other["floor_plan"]["reason"], "not_configured");
    state.locations.retain(|row| row.code != "RECV-01");
    state.stocks.retain(|row| row.location != "RECV-01");
    let changed = console::project(&state, true, "tenant-a", &config, &Page::default())
        .expect("valid isolated console fixture");
    assert_eq!(changed["floor_plan"]["available"], false);
    assert_eq!(changed["floor_plan"]["reason"], "bin_sources_changed");
}
#[test]
fn configured_geometry_and_asset_paths_are_bounded_before_reading_files() {
    let valid = json!({"schema_version":1,"facilities":[{"tenant":"t","warehouse_id":"w","layout":{"name":"plan","unit":"grid","width":100,"height":100,"bins":[{"code":"A","x":0,"y":0,"width":30,"height":30}]},"product_images":{"SKU":"product.png"}}]});
    for fault in [
        "parent",
        "absolute",
        "overflow",
        "overlap",
        "duplicate",
        "unknown",
    ] {
        let mut value = valid.clone();
        match fault {
            "parent" => value["facilities"][0]["product_images"]["SKU"] = json!("../outside.png"),
            "absolute" => value["facilities"][0]["product_images"]["SKU"] = json!("/outside.png"),
            "overflow" => value["facilities"][0]["layout"]["bins"][0]["width"] = json!(u32::MAX),
            "overlap" => {
                let mut row = value["facilities"][0]["layout"]["bins"][0].clone();
                row["code"] = json!("B");
                value["facilities"][0]["layout"]["bins"]
                    .as_array_mut()
                    .expect("valid isolated console fixture")
                    .push(row);
            }
            "duplicate" => {
                let row = value["facilities"][0].clone();
                value["facilities"]
                    .as_array_mut()
                    .expect("valid isolated console fixture")
                    .push(row);
            }
            _ => value["unknown"] = json!(true),
        }
        assert!(
            Assets::parse(
                &serde_json::to_vec(&value).expect("valid isolated console fixture"),
                std::env::temp_dir()
            )
            .is_err(),
            "{fault}"
        );
    }
    assert!(Assets::parse(
        &serde_json::to_vec(&valid).expect("valid isolated console fixture"),
        std::env::temp_dir()
    )
    .is_ok());
}
fn app(store: Store, tenant: &str, role: &str) -> Router {
    api::router(App {
        store,
        credentials: Arc::new(vec![(
            "not-a-secret".into(),
            Identity {
                tenant: tenant.into(),
                actor: "operator".into(),
                role: role.into(),
            },
        )]),
        demo: false,
        connections: Connections::default(),
    })
}
async fn call(
    router: Router,
    path: &str,
    authorized: bool,
) -> (StatusCode, axum::http::HeaderMap, Vec<u8>) {
    let mut request = Request::builder().uri(path);
    if authorized {
        request = request.header("authorization", "Bearer not-a-secret");
    }
    let response = router
        .oneshot(
            request
                .body(Body::empty())
                .expect("valid isolated console fixture"),
        )
        .await
        .expect("valid isolated console fixture");
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("valid isolated console fixture")
        .to_bytes()
        .to_vec();
    (status, headers, bytes)
}
fn lazy() -> Store {
    Store {
        pool: sqlx::postgres::PgPoolOptions::new()
            .connect_lazy("postgres://unused@127.0.0.1:1/unused")
            .expect("valid isolated console fixture"),
    }
}
#[tokio::test]
async fn all_visual_routes_require_authentication_and_invalid_queries_avoid_database_reads() {
    for path in [
        "/v1/warehouses/wh-main/console",
        "/v1/warehouses/wh-main/console/products/SKU",
        "/v1/warehouses/wh-main/console/products/SKU/image",
    ] {
        assert_eq!(
            call(app(lazy(), "t", "viewer"), path, false).await.0,
            StatusCode::UNAUTHORIZED
        );
    }
    for suffix in [
        "/console?limit=0",
        "/console?limit=65",
        "/console?after=A",
        "/console?unknown=true",
        "/console?review=-1",
        "/console/products/SKU?unknown=true",
        "/console/products/SKU/image",
    ] {
        let (status, _, _) = call(
            app(lazy(), "t", "viewer"),
            &format!("/v1/warehouses/wh-main{suffix}"),
            true,
        )
        .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{suffix}");
    }
}
#[test]
fn visual_bin_pages_include_empty_bins_without_claiming_capacity() {
    let mut state = domain::demo_state();
    state.locations.push(Location {
        code: "ZZ-EMPTY".into(),
        zone: "Z".into(),
        kind: "storage".into(),
    });
    let page = console::project(&state, true, "t", &Assets::empty(), &Page::default())
        .expect("valid isolated console fixture");
    let empty = page["bins"]["items"]
        .as_array()
        .expect("valid isolated console fixture")
        .iter()
        .find(|bin| bin["code"] == "ZZ-EMPTY")
        .expect("valid isolated console fixture");
    assert_eq!(empty["on_hand_each"], "0");
    assert_eq!(empty["reserved_each"], "0");
    assert!(empty.get("capacity").is_none());
    assert_eq!(page["floor_plan"]["available"], false);
}

#[tokio::test]
#[ignore = "requires isolated WMS_TEST_DATABASE_URL"]
async fn actual_console_reads_keep_tenants_versions_and_journal_unchanged() {
    let url = std::env::var("WMS_TEST_DATABASE_URL").expect("isolated test database");
    let name = url
        .split('?')
        .next()
        .expect("valid isolated console fixture")
        .rsplit('/')
        .next()
        .expect("valid isolated console fixture");
    assert!(name.contains("test"));
    let store = Store::connect(&url)
        .await
        .expect("valid isolated console fixture");
    let tenant = format!("console-{}", domain::id());
    store
        .provision(&tenant, false)
        .await
        .expect("valid isolated console fixture");
    let before = store
        .snapshot(&tenant, "wh-main")
        .await
        .expect("valid isolated console fixture");
    let before = serde_json::to_value(before).expect("valid isolated console fixture");
    let (status, headers, body) = call(
        app(store.clone(), &tenant, "viewer"),
        "/v1/warehouses/wh-main/console?limit=1",
        true,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
    assert_eq!(headers["cache-control"], "no-store");
    let view: Value = serde_json::from_slice(&body).expect("valid isolated console fixture");
    assert_eq!(view["tenant"], tenant);
    assert_eq!(view["source_ok"], true);
    assert_eq!(view["stock"]["on_hand_each"], "0");
    let next = format!(
        "/v1/warehouses/wh-main/console?limit=1&after={}&review={}",
        view["bins"]["next_after"]
            .as_str()
            .expect("valid isolated console fixture"),
        view["version"]
            .as_str()
            .expect("valid isolated console fixture")
    );
    assert_eq!(
        call(app(store.clone(), &tenant, "operator"), &next, true)
            .await
            .0,
        StatusCode::OK
    );
    let after = serde_json::to_value(
        store
            .snapshot(&tenant, "wh-main")
            .await
            .expect("valid isolated console fixture"),
    )
    .expect("valid isolated console fixture");
    for field in [
        "version",
        "stocks",
        "products",
        "orders",
        "receipts",
        "movements",
        "activity",
    ] {
        assert_eq!(before[field], after[field], "{field}");
    }
    let other = format!("console-other-{}", domain::id());
    store
        .provision(&other, false)
        .await
        .expect("valid isolated console fixture");
    let (_, _, body) = call(
        app(store.clone(), &other, "viewer"),
        "/v1/warehouses/wh-main/console",
        true,
    )
    .await;
    assert_eq!(
        serde_json::from_slice::<Value>(&body).expect("valid isolated console fixture")["tenant"],
        other
    );
    assert_eq!(
        call(
            app(store, &tenant, "viewer"),
            "/v1/warehouses/wh-main/console/products/not-a-product",
            true
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
#[ignore = "requires isolated WMS_TEST_DATABASE_URL"]
async fn actual_product_media_is_authenticated_scoped_versioned_and_read_only() {
    let url = std::env::var("WMS_TEST_DATABASE_URL").expect("isolated test database");
    assert!(reqwest::Url::parse(&url)
        .expect("valid isolated console fixture")
        .path()
        .contains("test"));
    let store = Store::connect(&url)
        .await
        .expect("valid isolated console fixture");
    let who = Identity {
        tenant: format!("console-media-{}", domain::id()),
        actor: "operator".into(),
        role: "admin".into(),
    };
    store
        .provision(&who.tenant, false)
        .await
        .expect("valid isolated console fixture");
    let request: stateset_warehouse::store::Request=serde_json::from_value(json!({"request_id":domain::id(),"expected_version":0,"command":{"type":"create_product","sku":"Sku-Ab12","name":"Synthetic exact-case product","category":"Synthetic","reorder_point":3}})).expect("valid isolated console fixture");
    store
        .execute(&who, "wh-main", &request)
        .await
        .expect("valid isolated console fixture");
    let root = std::env::temp_dir().join(format!("wms-console-media-{}", domain::id()));
    std::fs::create_dir(&root).expect("valid isolated console fixture");
    let png: Vec<u8> = vec![
        137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82, 0, 0, 0, 2, 0, 0, 0, 2, 8, 2,
        0, 0, 0, 253, 212, 154, 115, 0, 0, 0, 22, 73, 68, 65, 84, 120, 156, 99, 148, 219, 114, 130,
        129, 129, 129, 137, 129, 129, 129, 129, 129, 1, 0, 18, 184, 1, 158, 136, 46, 25, 8, 0, 0,
        0, 0, 73, 69, 78, 68, 174, 66, 96, 130,
    ];
    std::fs::write(root.join("product.png"), &png).expect("valid isolated console fixture");
    let config = json!({"schema_version":1,"facilities":[{"tenant":who.tenant,"warehouse_id":"wh-main","layout":null,"product_images":{"Sku-Ab12":"product.png"}}]});
    let assets = Assets::parse(
        &serde_json::to_vec(&config).expect("valid isolated console fixture"),
        root.clone(),
    )
    .expect("valid isolated console fixture");
    let make = |tenant: &str, role: &str| {
        api::router_with_console_assets(
            App {
                store: store.clone(),
                credentials: Arc::new(vec![(
                    "not-a-secret".into(),
                    Identity {
                        tenant: tenant.into(),
                        actor: "operator".into(),
                        role: role.into(),
                    },
                )]),
                demo: false,
                connections: Connections::default(),
            },
            assets.clone(),
        )
    };
    let facts = || async {
        sqlx::query_scalar::<_,Value>("SELECT jsonb_build_object('commands',(SELECT count(*) FROM wms_commands WHERE tenant=$1),'events',(SELECT count(*) FROM wms_events WHERE tenant=$1),'movements',(SELECT count(*) FROM wms_movements WHERE tenant=$1),'outbox',(SELECT count(*) FROM wms_outbox WHERE tenant=$1))").bind(&who.tenant).fetch_one(&store.pool).await.expect("valid isolated console fixture")
    };
    let before = facts().await;
    let base = "/v1/warehouses/wh-main/console/products/Sku-Ab12";
    let (status, _, body) = call(make(&who.tenant, "viewer"), base, true).await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
    let product: Value = serde_json::from_slice(&body).expect("valid isolated console fixture");
    assert_eq!(product["sku"], "Sku-Ab12");
    assert_eq!(product["label"]["value"], "Sku-Ab12");
    assert_eq!(product["image_status"], "ready");
    assert_eq!(product["image"]["width"], 2);
    assert_eq!(product["demo"], false);
    let image_url = format!(
        "{base}/image?review={}&sha256={}&scope={}",
        product["version"]
            .as_str()
            .expect("valid isolated console fixture"),
        product["image"]["sha256"]
            .as_str()
            .expect("valid isolated console fixture"),
        product["image_scope"]
            .as_str()
            .expect("valid isolated console fixture")
    );
    let (status, headers, body) = call(make(&who.tenant, "viewer"), &image_url, true).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers["content-type"], "image/png");
    assert_eq!(headers["cache-control"], "no-store");
    assert_eq!(body, png);
    assert_eq!(
        call(make(&who.tenant, "viewer"), &image_url, false).await.0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        call(make(&who.tenant, "operator"), &image_url, true)
            .await
            .0,
        StatusCode::CONFLICT,
        "role change invalidates image scope"
    );
    for changed in [
        image_url.replace("review=1", "review=0"),
        image_url.replace(
            product["image"]["sha256"]
                .as_str()
                .expect("valid isolated console fixture"),
            &"0".repeat(64),
        ),
        image_url.replace(
            product["image_scope"]
                .as_str()
                .expect("valid isolated console fixture"),
            &"0".repeat(64),
        ),
    ] {
        assert_eq!(
            call(make(&who.tenant, "viewer"), &changed, true).await.0,
            StatusCode::CONFLICT
        );
    }
    assert_eq!(
        call(
            make(&who.tenant, "viewer"),
            "/v1/warehouses/wh-main/console/products/SKU-AB12",
            true
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    let other = format!("console-media-other-{}", domain::id());
    store
        .provision(&other, false)
        .await
        .expect("valid isolated console fixture");
    assert_eq!(
        call(make(&other, "viewer"), base, true).await.0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        call(make(&other, "viewer"), &image_url, true).await.0,
        StatusCode::CONFLICT
    );
    std::fs::write(root.join("product.png"), b"not a PNG").expect("valid isolated console fixture");
    let (status, _, body) = call(make(&who.tenant, "viewer"), base, true).await;
    assert_eq!(status, StatusCode::OK);
    let degraded: Value = serde_json::from_slice(&body).expect("valid isolated console fixture");
    assert_eq!(degraded["image_status"], "unavailable");
    assert!(degraded["image"].is_null());
    assert_eq!(degraded["label"]["value"], "Sku-Ab12");
    #[cfg(unix)]
    {
        let outside = root.with_extension("png");
        std::fs::write(&outside, &png).expect("valid isolated console fixture");
        std::fs::remove_file(root.join("product.png")).expect("valid isolated console fixture");
        std::os::unix::fs::symlink(&outside, root.join("product.png"))
            .expect("valid isolated console fixture");
        let (_, _, body) = call(make(&who.tenant, "viewer"), base, true).await;
        assert_eq!(
            serde_json::from_slice::<Value>(&body).expect("valid isolated console fixture")
                ["image_status"],
            "unavailable"
        );
        assert_eq!(
            call(make(&who.tenant, "viewer"), &image_url, true).await.0,
            StatusCode::NOT_FOUND
        );
    }
    assert_eq!(
        before,
        facts().await,
        "visual reads cannot append commands, events, movements or outbox"
    );
    assert_eq!(
        store
            .state(&who.tenant, "wh-main")
            .await
            .expect("valid isolated console fixture")
            .version,
        1
    );
    eprintln!("Retained synthetic media fixture: {}", root.display());
}
