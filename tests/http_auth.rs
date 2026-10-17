use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use stateset_warehouse::{
    api::{router, App},
    integrations::Connections,
    store::{Identity, Store},
};
use std::sync::Arc;
use tower::ServiceExt;
fn app() -> axum::Router {
    let pool = sqlx::postgres::PgPoolOptions::new()
        .connect_lazy("postgres://unused:unused@127.0.0.1:1/unused")
        .expect("lazy config");
    router(App {
        store: Store { pool },
        credentials: Arc::new(vec![(
            "viewer-test-key".into(),
            Identity {
                tenant: "tenant-a".into(),
                actor: "reader".into(),
                role: "viewer".into(),
            },
        )]),
        demo: false,
        connections: Connections::default(),
    })
}
#[tokio::test]
async fn health_is_public_but_warehouse_data_is_not() {
    let r = app()
        .oneshot(
            Request::builder()
                .uri("/health")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(r.status(), StatusCode::OK);
    let r = app()
        .oneshot(
            Request::builder()
                .uri("/v1/warehouses")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(r.status(), StatusCode::UNAUTHORIZED);
}
#[tokio::test]
async fn fake_tenant_headers_never_replace_identity() {
    let r = app()
        .oneshot(
            Request::builder()
                .uri("/v1/session")
                .header("Authorization", "Bearer viewer-test-key")
                .header("X-Tenant-ID", "victim")
                .header("X-Role", "admin")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(r.status(), StatusCode::OK);
    assert_eq!(r.headers()["cache-control"], "no-store");
    let bytes = http_body_util::BodyExt::collect(r.into_body())
        .await
        .expect("body")
        .to_bytes();
    let json: serde_json::Value = serde_json::from_slice(&bytes).expect("JSON");
    assert_eq!(json["identity"]["tenant"], "tenant-a");
    assert_eq!(json["identity"]["role"], "viewer");
}
#[tokio::test]
async fn viewer_command_is_rejected_before_database_access() {
    let r=app().oneshot(Request::builder().method("POST").uri("/v1/warehouses/wh-main/commands").header("Authorization","Bearer viewer-test-key").header("Content-Type","application/json").body(Body::from(r#"{"request_id":"request-1234","expected_version":0,"command":{"type":"allocate","order_id":"1"}}"#)).expect("request")).await.expect("response");
    assert_eq!(r.status(), StatusCode::FORBIDDEN);
}
