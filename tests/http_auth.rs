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

#[tokio::test]
async fn api_errors_and_successes_have_private_caching_and_unforgeable_correlation() {
    let forged = "00000000-0000-4000-8000-000000000001";
    let mut ids = std::collections::HashSet::new();
    for (path, credential, expected) in [
        ("/v1/session", false, StatusCode::UNAUTHORIZED),
        ("/v1/session", true, StatusCode::OK),
        ("/v1/missing-api", false, StatusCode::UNAUTHORIZED),
        ("/v1/missing-api", true, StatusCode::NOT_FOUND),
        ("/v1/", false, StatusCode::UNAUTHORIZED),
        ("/v1/", true, StatusCode::NOT_FOUND),
    ] {
        let mut request = Request::builder().uri(path).header("X-Request-ID", forged);
        if credential {
            request = request.header("Authorization", "Bearer viewer-test-key");
        }
        let response = app()
            .oneshot(request.body(Body::empty()).expect("request"))
            .await
            .expect("response");
        assert_eq!(response.status(), expected);
        assert_eq!(response.headers()["cache-control"], "no-store");
        assert_eq!(response.headers()["x-content-type-options"], "nosniff");
        assert_eq!(response.headers()["x-frame-options"], "DENY");
        assert_eq!(response.headers()["referrer-policy"], "no-referrer");
        let policy = response.headers()["content-security-policy"]
            .to_str()
            .expect("CSP");
        assert!(policy.contains("script-src 'self'"));
        assert!(policy.contains("frame-ancestors 'none'"));
        let id = response.headers()["x-request-id"]
            .to_str()
            .expect("request ID")
            .to_string();
        uuid::Uuid::parse_str(&id).expect("server-generated UUID");
        assert_ne!(id, forged);
        assert!(ids.insert(id));
        let bytes = http_body_util::BodyExt::collect(response.into_body())
            .await
            .expect("body")
            .to_bytes();
        serde_json::from_slice::<serde_json::Value>(&bytes)
            .expect("API response is JSON, never SPA HTML");
    }
}

#[tokio::test]
async fn database_unavailability_fails_readiness_within_a_bounded_deadline() {
    let response = tokio::time::timeout(
        std::time::Duration::from_secs(4),
        app().oneshot(
            Request::builder()
                .uri("/ready")
                .body(Body::empty())
                .expect("request"),
        ),
    )
    .await
    .expect("readiness must not wait for the pool's full connection timeout")
    .expect("response");
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(response.headers()["cache-control"], "no-store");
    let response = app()
        .oneshot(
            Request::builder()
                .uri("/health")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("liveness response");
    assert_eq!(response.status(), StatusCode::OK);
}
