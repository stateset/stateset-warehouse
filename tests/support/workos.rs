use super::*;
use axum::{
    body::Body,
    http::{Request, StatusCode},
    routing::get,
    Router,
};
use http_body_util::BodyExt;
use jsonwebtoken::{encode, EncodingKey, Header};
use std::sync::atomic::{AtomicUsize, Ordering};
use tower::ServiceExt;

fn config() -> WorkOsConfig {
    WorkOsConfig::parse(r#"{"client_id":"client_warehouse","api_origin":"https://auth.example.com","issuer":"https://auth.example.com/","organizations":{"org_acme":{"tenant":"acme","roles":{"warehouse_reader":"viewer","warehouse_worker":"operator","warehouse_manager":"admin"}},"org_second":{"tenant":"second","roles":{"warehouse_reader":"viewer"}}}}"#).expect("config")
}
fn claims() -> Value {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_secs();
    json!({"iss":"https://auth.example.com/","sub":"user_alice","sid":"session_alice","org_id":"org_acme","role":"warehouse_reader","iat":now-1,"exp":now+299})
}
fn signed(value: &Value, second: bool) -> String {
    let mut header = Header::new(Algorithm::RS256);
    header.kid = Some(if second { "test-key-2" } else { "test-key-1" }.into());
    let der: &[u8] = if second {
        include_bytes!("../fixtures/workos-test-key2.der")
    } else {
        include_bytes!("../fixtures/workos-test-key.der")
    };
    encode(&header, value, &EncodingKey::from_rsa_der(der)).expect("sign test JWT")
}
fn jwk(second: bool) -> Value {
    serde_json::from_str(if second {
        include_str!("../fixtures/workos-test-jwk2.json")
    } else {
        include_str!("../fixtures/workos-test-jwk.json")
    })
    .expect("JWK")
}
struct Fixture {
    auth: WorkOs,
    response: Arc<std::sync::Mutex<(StatusCode, String)>>,
    calls: Arc<AtomicUsize>,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.task.abort();
    }
}
fn app(auth: WorkOs, store: crate::store::Store) -> Router {
    crate::api::router_with_auth(
        crate::api::App {
            store,
            credentials: Arc::new(vec![(
                "service-key".into(),
                Identity {
                    tenant: "service".into(),
                    actor: "oms:bridge".into(),
                    role: "oms".into(),
                },
            )]),
            demo: false,
            connections: crate::integrations::Connections::default(),
        },
        Some(auth),
    )
}
fn lazy_store() -> crate::store::Store {
    crate::store::Store {
        pool: sqlx::postgres::PgPoolOptions::new()
            .connect_lazy("postgres://unused:unused@127.0.0.1:1/unused")
            .expect("lazy pool"),
    }
}
impl Fixture {
    async fn new() -> Self {
        let response = Arc::new(std::sync::Mutex::new((
            StatusCode::OK,
            json!({"keys":[jwk(false)]}).to_string(),
        )));
        let calls = Arc::new(AtomicUsize::new(0));
        let (reply, count) = (response.clone(), calls.clone());
        let router = Router::new().route(
            "/keys",
            get(move || {
                count.fetch_add(1, Ordering::SeqCst);
                let response = reply.lock().expect("fixture lock").clone();
                async move { response }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("listener");
        let url = format!("http://{}/keys", listener.local_addr().expect("address"));
        let task = tokio::spawn(async move {
            axum::serve(listener, router).await.expect("fixture server");
        });
        let mut auth = WorkOs::new(config()).expect("client");
        // A local HTTP override exists only in this cfg(test) module.
        auth.jwks_url = url;
        Self {
            auth,
            response,
            calls,
            task,
        }
    }
    fn respond(&self, status: StatusCode, value: Value) {
        *self.response.lock().expect("fixture lock") = (status, value.to_string());
    }
    fn router(&self) -> Router {
        app(self.auth.clone(), lazy_store())
    }
}
#[test]
fn configuration_rejects_ambiguous_or_unsafe_authority() {
    let valid = json!({"client_id":"client_warehouse","api_origin":"https://auth.example.com","issuer":"https://auth.example.com/","organizations":{"org_acme":{"tenant":"acme","roles":{"member":"viewer"}}}});
    assert!(WorkOsConfig::parse(&valid.to_string()).is_ok());
    for (field, value) in [
        ("api_origin", json!("http://auth.example.com")),
        ("api_origin", json!("https://auth.example.com/")),
        ("api_origin", json!("https://user:pass@auth.example.com")),
        ("api_origin", json!("https://auth.example.com/keys")),
        ("issuer", json!("https://other.example.com/")),
        ("client_id", json!("../../other-client")),
        ("organizations", json!({})),
        ("audience", json!("")),
        ("unknown", json!(true)),
        (
            "organizations",
            json!({"org_acme":{"tenant":"demo","roles":{"member":"viewer"}}}),
        ),
        (
            "organizations",
            json!({"org_acme":{"tenant":"acme","roles":{"member":"oms"}}}),
        ),
        (
            "organizations",
            json!({"org_a":{"tenant":"acme","roles":{"member":"viewer"}},"org_b":{"tenant":"acme","roles":{"member":"admin"}}}),
        ),
    ] {
        let mut candidate = valid.clone();
        candidate[field] = value;
        assert!(
            WorkOsConfig::parse(&candidate.to_string()).is_err(),
            "accepted {candidate}"
        );
    }
}
#[tokio::test]
async fn signed_membership_maps_only_server_approved_identity() {
    let fixture = Fixture::new().await;
    let mut value = claims();
    value["tenant"] = json!("victim");
    value["permissions"] = json!(["admin"]);
    let identity = fixture
        .auth
        .authenticate(&signed(&value, false))
        .await
        .expect("identity");
    assert_eq!(
        (
            identity.tenant.as_str(),
            identity.actor.as_str(),
            identity.role.as_str()
        ),
        ("acme", "workos:user_alice", "viewer")
    );
    value["org_id"] = json!("org_second");
    let other = fixture
        .auth
        .authenticate(&signed(&value, false))
        .await
        .expect("other org");
    assert_eq!(other.tenant, "second");
    assert_eq!(other.actor, identity.actor);
    assert_eq!(fixture.calls.load(Ordering::SeqCst), 1);
}
#[tokio::test]
async fn invalid_claims_and_signatures_fail_closed() {
    let fixture = Fixture::new().await;
    let now = claims()["iat"].as_u64().expect("iat") + 1;
    for (field, replacement, status) in [
        ("iss", json!("https://attacker.example/"), 401),
        ("exp", json!(now - 1), 401),
        ("iat", json!(now + 90), 401),
        ("exp", json!(now + 3600), 401),
        ("iat", Value::Null, 401),
        ("nbf", json!(now + 60), 401),
        ("sub", json!(""), 401),
        ("sid", Value::Null, 401),
        ("org_id", Value::Null, 401),
        ("role", Value::Null, 401),
        ("impersonator", json!({"email":"admin@example.com"}), 401),
        ("aud", json!("other-app"), 401),
        ("org_id", json!("org_unmapped"), 403),
        ("role", json!("admin"), 403),
    ] {
        let mut value = claims();
        value[field] = replacement;
        assert_eq!(
            fixture
                .auth
                .authenticate(&signed(&value, false))
                .await
                .expect_err("invalid claims")
                .status,
            status,
            "case {field}"
        );
    }
    let good = signed(&claims(), false);
    let mut forged: Vec<String> = good.split('.').map(str::to_owned).collect();
    forged[2] = signed(&claims(), true)
        .split('.')
        .nth(2)
        .expect("signature")
        .to_owned();
    assert_eq!(
        fixture
            .auth
            .authenticate(&forged.join("."))
            .await
            .expect_err("forged signature")
            .status,
        401
    );
    let mut hs = Header::new(Algorithm::HS256);
    hs.kid = Some("test-key-1".into());
    let token = encode(&hs, &claims(), &EncodingKey::from_secret(b"attacker")).expect("HMAC");
    assert_eq!(
        fixture
            .auth
            .authenticate(&token)
            .await
            .expect_err("algorithm confusion")
            .status,
        401
    );
    assert_eq!(fixture.calls.load(Ordering::SeqCst), 1);
}
#[tokio::test]
async fn configured_audience_requires_presence_and_exact_match() {
    let mut fixture = Fixture::new().await;
    fixture.auth.config.audience = Some("warehouse".into());
    for audience in [None, Some("other"), Some("warehouse")] {
        let mut value = claims();
        if let Some(audience) = audience {
            value["aud"] = json!(audience);
        }
        assert_eq!(
            fixture
                .auth
                .authenticate(&signed(&value, false))
                .await
                .is_ok(),
            audience == Some("warehouse")
        );
    }
}
#[tokio::test]
async fn concurrent_jwks_requests_are_coalesced_and_unknown_kids_are_rate_limited() {
    let fixture = Fixture::new().await;
    let mut tasks = tokio::task::JoinSet::new();
    for _ in 0..20 {
        let auth = fixture.auth.clone();
        let token = signed(&claims(), false);
        tasks.spawn(async move { auth.authenticate(&token).await.is_ok() });
    }
    while let Some(result) = tasks.join_next().await {
        assert!(result.expect("task"));
    }
    for _ in 0..20 {
        assert_eq!(
            fixture
                .auth
                .authenticate(&signed(&claims(), true))
                .await
                .expect_err("unknown kid")
                .status,
            401
        );
    }
    assert_eq!(fixture.calls.load(Ordering::SeqCst), 1);
}
#[tokio::test]
async fn rotated_keys_replace_old_keys_and_expired_cache_never_bypasses_outage() {
    let fixture = Fixture::new().await;
    assert!(fixture
        .auth
        .authenticate(&signed(&claims(), false))
        .await
        .is_ok());
    fixture.respond(StatusCode::OK, json!({"keys":[jwk(true)]}));
    fixture.auth.cache.lock().await.attempted = Some(Instant::now() - REFRESH_COOLDOWN);
    assert!(fixture
        .auth
        .authenticate(&signed(&claims(), true))
        .await
        .is_ok());
    assert_eq!(
        fixture
            .auth
            .authenticate(&signed(&claims(), false))
            .await
            .expect_err("retired key")
            .status,
        401
    );
    fixture.respond(StatusCode::SERVICE_UNAVAILABLE, json!({}));
    {
        let mut cache = fixture.auth.cache.lock().await;
        cache.fetched = Some(Instant::now() - KEY_TTL);
        cache.attempted = None;
    }
    for _ in 0..2 {
        assert_eq!(
            fixture
                .auth
                .authenticate(&signed(&claims(), true))
                .await
                .expect_err("outage")
                .status,
            503
        );
    }
    assert_eq!(fixture.calls.load(Ordering::SeqCst), 3);
}
#[tokio::test]
async fn malformed_oversized_redirected_and_ambiguous_keysets_are_rejected() {
    for (status, body) in [
        (StatusCode::FOUND, json!({"keys":[jwk(false)]})),
        (StatusCode::OK, json!({"keys":[jwk(false),jwk(false)]})),
        (StatusCode::OK, json!({"keys":[]})),
        (
            StatusCode::OK,
            json!({"padding":"x".repeat(MAX_JWKS_BYTES+1)}),
        ),
        (
            StatusCode::OK,
            json!({"keys":[{"kid":"test-key-1","kty":"oct","k":"secret"}]}),
        ),
    ] {
        let fixture = Fixture::new().await;
        fixture.respond(status, body);
        assert_eq!(
            fixture
                .auth
                .authenticate(&signed(&claims(), false))
                .await
                .expect_err("invalid keys")
                .status,
            503
        );
    }
}
#[tokio::test]
async fn http_auth_enforces_roles_hides_mappings_and_preserves_service_keys() {
    let fixture = Fixture::new().await;
    let token = signed(&claims(), false);
    for (path, method, bearer, status) in [
        ("/auth/config", "GET", "", 200),
        ("/v1/session", "GET", token.as_str(), 200),
        ("/v1/session", "GET", "service-key", 200),
        ("/v1/session", "GET", "invalid", 401),
        ("/v1/warehouses/main/commands", "POST", token.as_str(), 403),
    ] {
        let response=fixture.router().oneshot(Request::builder().uri(path).method(method)
            .header("authorization",format!("Bearer {bearer}")).header("X-Tenant-ID","victim").header("X-Role","admin")
            .header("content-type","application/json").body(Body::from(r#"{"request_id":"request-1234","expected_version":0,"command":{"type":"allocate","order_id":"1"}}"#)).expect("request")).await.expect("response");
        assert_eq!(response.status().as_u16(), status, "{path}");
        assert_eq!(response.headers()["cache-control"], "no-store");
        assert!(response.headers()["content-security-policy"]
            .to_str()
            .expect("CSP")
            .contains("connect-src 'self' https://auth.example.com;"));
        let bytes = response
            .into_body()
            .collect()
            .await
            .expect("body")
            .to_bytes();
        let json: Value = serde_json::from_slice(&bytes).expect("JSON");
        if path == "/auth/config" {
            assert_eq!(json, config().public());
            assert!(!String::from_utf8_lossy(&bytes).contains("acme"));
        }
        if bearer == token && path == "/v1/session" {
            assert_eq!(json["identity"]["tenant"], "acme");
            assert_eq!(json["identity"]["role"], "viewer");
        }
    }
}
#[tokio::test]
async fn unresponsive_signing_provider_has_a_bounded_http_failure() {
    let fixture = Fixture::new().await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("listener");
    let address = listener.local_addr().expect("address");
    let slow = Router::new().route(
        "/keys",
        get(|| async {
            tokio::time::sleep(Duration::from_secs(30)).await;
            "{}"
        }),
    );
    let task = tokio::spawn(async move {
        axum::serve(listener, slow).await.expect("slow fixture");
    });
    let mut auth = fixture.auth.clone();
    auth.jwks_url = format!("http://{address}/keys");
    let start = Instant::now();
    let response = app(auth, lazy_store())
        .oneshot(
            Request::builder()
                .uri("/v1/session")
                .header(
                    "authorization",
                    format!("Bearer {}", signed(&claims(), false)),
                )
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");
    task.abort();
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(response.headers()["cache-control"], "no-store");
    assert!(start.elapsed() < Duration::from_secs(5));
}
#[tokio::test]
#[ignore = "requires isolated PostgreSQL; scripts/local-postgres.sh"]
async fn signed_identity_owns_persisted_commands_across_refresh_and_tenant_changes() {
    let url = std::env::var("WMS_TEST_DATABASE_URL").expect("isolated test database");
    assert!(reqwest::Url::parse(&url)
        .expect("DB URL")
        .path()
        .contains("test"));
    let store = crate::store::Store::connect(&url).await.expect("connect");
    let mut fixture = Fixture::new().await;
    let tenant = format!("auth-{}", uuid::Uuid::new_v4());
    let other = format!("auth-{}", uuid::Uuid::new_v4());
    fixture
        .auth
        .config
        .organizations
        .get_mut("org_acme")
        .expect("org")
        .tenant = tenant.clone();
    fixture
        .auth
        .config
        .organizations
        .get_mut("org_second")
        .expect("org")
        .tenant = other.clone();
    store.provision(&tenant, false).await.expect("provision");
    store.provision(&other, false).await.expect("provision");
    let app = app(fixture.auth.clone(), store.clone());
    async fn http(app: &Router, value: &Value, path: &str, body: Option<Value>) -> (u16, Value) {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(path)
                    .method(if body.is_some() { "POST" } else { "GET" })
                    .header("authorization", format!("Bearer {}", signed(value, false)))
                    .header("content-type", "application/json")
                    .body(Body::from(body.map(|b| b.to_string()).unwrap_or_default()))
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
        (status, serde_json::from_slice(&bytes).expect("JSON"))
    }
    let mut alice = claims();
    alice["role"] = json!("warehouse_manager");
    let body = json!({"request_id":"workos-save-0001","expected_version":0,"command":{"type":"create_product","sku":"AUTH-SKU","name":"Auth test","category":"General","reorder_point":1}});
    let path = "/v1/warehouses/wh-main/commands";
    let receipt_path = format!("{path}/workos-save-0001");
    let (status, result) = http(&app, &alice, path, Some(body.clone())).await;
    assert_eq!(status, 200, "{result}");
    assert_eq!(result["replayed"], false);
    alice["sid"] = json!("session_renewed");
    let (status, replay) = http(&app, &alice, path, Some(body.clone())).await;
    assert_eq!(status, 200);
    assert_eq!(replay["replayed"], true);
    assert_eq!(replay["result"], result["result"]);
    let mut bob = alice.clone();
    bob["sub"] = json!("user_bob");
    assert_eq!(http(&app, &bob, path, Some(body.clone())).await.0, 409);
    assert_eq!(
        http(&app, &bob, &receipt_path, None).await.1["found"],
        false
    );
    let mut switched = alice.clone();
    switched["org_id"] = json!("org_second");
    switched["role"] = json!("warehouse_reader");
    assert_eq!(
        http(&app, &switched, &receipt_path, None).await.1["found"],
        false
    );
    assert_eq!(http(&app, &switched, path, Some(body.clone())).await.0, 403);
    alice["role"] = json!("warehouse_reader");
    assert_eq!(http(&app, &alice, path, Some(body)).await.0, 403);
    let original = store.state(&tenant, "wh-main").await.expect("state");
    assert_eq!(original.version, 1);
    assert_eq!(original.products.len(), 1);
    assert!(store
        .state(&other, "wh-main")
        .await
        .expect("other state")
        .products
        .is_empty());
    let owner: String = sqlx::query_scalar(
        "SELECT actor FROM wms_commands WHERE tenant=$1 AND request_id='workos-save-0001'",
    )
    .bind(&tenant)
    .fetch_one(&store.pool)
    .await
    .expect("persisted actor");
    assert_eq!(owner, "workos:user_alice");
}
