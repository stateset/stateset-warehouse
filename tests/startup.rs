//! Exercise the shipped entry point with an isolated environment and owned
//! processes. A router fixture cannot prove startup ordering or provisioning.
use serde_json::{json, Value};
use stateset_warehouse::store::Store;
use std::{
    collections::BTreeMap,
    fs,
    path::PathBuf,
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

struct Process {
    child: Child,
    root: PathBuf,
}
impl Drop for Process {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = fs::remove_dir_all(&self.root);
    }
}
impl Process {
    fn start(settings: &BTreeMap<String, String>) -> Self {
        let root = std::env::temp_dir().join(format!("wms-startup-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&root).expect("owned runtime directory");
        // dotenv walks ancestors; a clean child environment alone is not enough.
        assert!(root.ancestors().all(|p| !p.join(".env").exists()));
        let log = fs::File::create(root.join("server.log")).expect("owned log");
        let child = Command::new(env!("CARGO_BIN_EXE_stateset-warehouse"))
            .env_clear()
            .env("NO_COLOR", "1")
            .env("RUST_LOG", "stateset_warehouse=info")
            .env("WMS_BIND", "127.0.0.1:0")
            .envs(settings)
            .current_dir(&root)
            .stdin(Stdio::null())
            .stdout(log.try_clone().expect("log handle"))
            .stderr(log)
            .spawn()
            .expect("start actual WMS binary");
        Self { child, root }
    }
    fn log(&self) -> String {
        fs::read_to_string(self.root.join("server.log")).expect("startup log")
    }
    async fn rejects(&mut self, diagnostic: &str) {
        let start = Instant::now();
        loop {
            if let Some(status) = self.child.try_wait().expect("owned child status") {
                assert!(!status.success(), "invalid deployment started successfully");
                assert!(
                    self.log().contains(diagnostic),
                    "expected startup diagnostic: {diagnostic}"
                );
                return;
            }
            assert!(
                start.elapsed() < Duration::from_secs(10),
                "invalid deployment did not exit"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }
    async fn ready(&mut self, client: &reqwest::Client) -> String {
        let start = Instant::now();
        loop {
            assert!(
                self.child.try_wait().expect("owned child status").is_none(),
                "server exited before readiness"
            );
            for line in self
                .log()
                .lines()
                .filter(|line| line.contains("StateSet WMS listening"))
            {
                let port = line
                    .split("127.0.0.1:")
                    .nth(1)
                    .expect("bound loopback address")
                    .chars()
                    .take_while(char::is_ascii_digit)
                    .collect::<String>();
                assert!(port.parse::<u16>().expect("bound port") > 0);
                let base = format!("http://127.0.0.1:{port}");
                if client
                    .get(format!("{base}/ready"))
                    .send()
                    .await
                    .is_ok_and(|r| r.status().is_success())
                {
                    return base;
                }
            }
            assert!(
                start.elapsed() < Duration::from_secs(15),
                "server readiness deadline"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }
}
fn workos(tenant: &str, other: &str) -> String {
    json!({"client_id":"client_startup", "api_origin":"https://auth.example.com", "issuer":"https://auth.example.com/",
        "organizations":{"org_one":{"tenant":tenant,"roles":{"worker":"operator"}},"org_two":{"tenant":other,"roles":{"reader":"viewer"}}}}).to_string()
}
fn settings() -> BTreeMap<String, String> {
    BTreeMap::from([(
        "WMS_WORKOS_JSON".into(),
        workos("startup-one", "startup-two"),
    )])
}

#[tokio::test]
async fn malformed_receivers_are_rejected_before_database_access() {
    for destination in ["OMS", "TMS"] {
        for (url, token, tenant, diagnostic) in [
            (
                Some("https://receiver.example/events"),
                None,
                None,
                "Receiver URL and token must be configured together",
            ),
            (
                None,
                Some("test-token-at-least-sixteen"),
                None,
                "Receiver URL and token must be configured together",
            ),
            (
                Some("http://receiver.example/events"),
                Some("test-token-at-least-sixteen"),
                Some("startup-one"),
                "Receivers require HTTPS",
            ),
            (
                Some("https://receiver.example/events"),
                Some("short"),
                Some("startup-one"),
                "Receivers require HTTPS",
            ),
            (
                Some("https://receiver.example/events"),
                Some("test-token-at-least-sixteen"),
                None,
                "WMS_WEBHOOK_TENANT is required",
            ),
            (
                Some("https://receiver.example/events"),
                Some("test-token-at-least-sixteen"),
                Some("unmapped"),
                "Webhook tenant must have a configured identity",
            ),
        ] {
            let mut env = settings();
            if let Some(url) = url {
                env.insert(format!("WMS_{destination}_WEBHOOK_URL"), url.into());
            }
            if let Some(token) = token {
                env.insert(format!("WMS_{destination}_WEBHOOK_TOKEN"), token.into());
            }
            if let Some(tenant) = tenant {
                env.insert("WMS_WEBHOOK_TENANT".into(), tenant.into());
            }
            // No DATABASE_URL: these errors must be detected before DB access.
            Process::start(&env).rejects(diagnostic).await;
        }
    }
}

#[tokio::test]
async fn startup_identity_guards_cannot_fall_back_to_another_provider() {
    for (overrides, diagnostic) in [
        (
            vec![("WMS_WORKOS_JSON", "{}")],
            "Invalid WMS_WORKOS_JSON schema",
        ),
        (vec![("WMS_DEMO", "true")], "Demo mode cannot enable WorkOS"),
        (
            vec![(
                "WMS_API_KEYS_JSON",
                r#"{"startup-key-at-least-thirty-two-characters":{"tenant":"startup-one","actor":"workos:user_fake","role":"admin"}}"#,
            )],
            "workos: actors are reserved",
        ),
        (
            vec![("WMS_NATIVE_INTEGRATIONS_JSON", r#"{"unmapped":{}}"#)],
            "Every native integration tenant must have a configured identity",
        ),
    ] {
        let mut env = settings();
        env.extend(
            overrides
                .into_iter()
                .map(|(k, v)| (k.to_string(), v.to_string())),
        );
        Process::start(&env).rejects(diagnostic).await;
    }
    Process::start(&BTreeMap::new())
        .rejects("Configure WorkOS or API keys")
        .await;
}

#[tokio::test]
async fn occupied_listener_is_rejected_before_database_access() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("owned port");
    let mut env = settings();
    env.insert(
        "WMS_BIND".into(),
        listener.local_addr().expect("address").to_string(),
    );
    env.insert(
        "DATABASE_URL".into(),
        "postgres://unused:unused@127.0.0.1:1/unused_test".into(),
    );
    Process::start(&env).rejects("AddrInUse").await;
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL; scripts/local-postgres.sh"]
async fn invalid_receiver_configuration_cannot_provision_a_tenant() {
    let url = std::env::var("WMS_TEST_DATABASE_URL").expect("isolated test database");
    assert!(reqwest::Url::parse(&url)
        .expect("DB URL")
        .path()
        .contains("test"));
    let store = Store::connect(&url).await.expect("connect");
    let one = format!("startup-{}", uuid::Uuid::new_v4());
    let two = format!("startup-{}", uuid::Uuid::new_v4());
    let mut env = BTreeMap::from([
        ("DATABASE_URL".into(), url),
        ("WMS_WORKOS_JSON".into(), workos(&one, &two)),
        (
            "WMS_OMS_WEBHOOK_URL".into(),
            "https://receiver.example/events".into(),
        ),
    ]);
    Process::start(&env)
        .rejects("Receiver URL and token must be configured together")
        .await;
    for tenant in [&one, &two] {
        assert!(
            store
                .list(tenant)
                .await
                .expect("tenant warehouses")
                .is_empty(),
            "invalid deployment provisioned tenant {tenant}"
        );
    }
    env.remove("WMS_OMS_WEBHOOK_URL");
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("owned port");
    env.insert(
        "WMS_BIND".into(),
        listener.local_addr().expect("address").to_string(),
    );
    Process::start(&env).rejects("AddrInUse").await;
    for tenant in [&one, &two] {
        assert!(
            store
                .list(tenant)
                .await
                .expect("tenant warehouses")
                .is_empty(),
            "invalid deployment provisioned tenant {tenant}"
        );
    }
    store.pool.close().await;
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL; scripts/local-postgres.sh"]
async fn workos_only_and_mixed_startup_provision_exact_tenants_and_preserve_work_on_restart() {
    let url = std::env::var("WMS_TEST_DATABASE_URL").expect("isolated test database");
    assert!(reqwest::Url::parse(&url)
        .expect("DB URL")
        .path()
        .contains("test"));
    let store = Store::connect(&url).await.expect("connect");
    let tenants: Vec<_> = (0..3)
        .map(|_| format!("startup-{}", uuid::Uuid::new_v4()))
        .collect();
    let mut env = BTreeMap::from([
        ("DATABASE_URL".into(), url),
        ("WMS_WORKOS_JSON".into(), workos(&tenants[0], &tenants[1])),
    ]);
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(2))
        .build()
        .expect("HTTP client");
    let mut server = Process::start(&env);
    let base = server.ready(&client).await;
    let config = client
        .get(format!("{base}/auth/config"))
        .send()
        .await
        .expect("public config");
    assert_eq!(config.status(), 200);
    assert_eq!(config.headers()["cache-control"], "no-store");
    assert_eq!(
        config.json::<Value>().await.expect("config JSON"),
        json!({"provider":"workos","clientId":"client_startup","apiHostname":"auth.example.com"})
    );
    for tenant in &tenants[..2] {
        let state = store
            .state(tenant, "wh-main")
            .await
            .expect("mapped tenant provisioned by binary");
        assert_eq!(state.version, 0);
        assert!(state.products.is_empty());
        assert!(state.orders.is_empty());
        assert!(state.stocks.is_empty());
    }
    assert!(store
        .list(&tenants[2])
        .await
        .expect("unmapped tenant")
        .is_empty());
    for token in ["", "demo-warehouse", "unconfigured-service-key"] {
        assert_eq!(
            client
                .get(format!("{base}/v1/warehouses"))
                .bearer_auth(token)
                .send()
                .await
                .expect("auth guard")
                .status(),
            401
        );
    }
    drop(server);

    let key = format!("startup-{}", uuid::Uuid::new_v4());
    let other_key = format!("startup-{}", uuid::Uuid::new_v4());
    env.insert(
        "WMS_API_KEYS_JSON".into(),
        json!({
            &key:{"tenant":tenants[0],"actor":"startup:operator","role":"admin"},
            &other_key:{"tenant":tenants[2],"actor":"startup:service","role":"viewer"}
        })
        .to_string(),
    );
    let mut server = Process::start(&env);
    let base = server.ready(&client).await;
    for tenant in &tenants {
        assert_eq!(store.list(tenant).await.expect("tenant union").len(), 1);
    }
    let session = client
        .get(format!("{base}/v1/session"))
        .bearer_auth(&key)
        .header("X-Tenant-ID", &tenants[1])
        .send()
        .await
        .expect("session")
        .json::<Value>()
        .await
        .expect("JSON");
    assert_eq!(session["identity"]["tenant"], tenants[0]);
    assert_eq!(session["identity"]["actor"], "startup:operator");
    let request = json!({"request_id":"startup-save-0001","expected_version":0,"command":{"type":"create_product","sku":"STARTUP","name":"Preserve on restart","category":"General","reorder_point":1}});
    let response = client
        .post(format!("{base}/v1/warehouses/wh-main/commands"))
        .bearer_auth(&key)
        .json(&request)
        .send()
        .await
        .expect("command");
    assert_eq!(response.status(), 200);
    let saved = response.json::<Value>().await.expect("command result");
    assert_eq!(saved["replayed"], false);
    let other = client
        .get(format!("{base}/v1/warehouses/wh-main"))
        .bearer_auth(&other_key)
        .send()
        .await
        .expect("other tenant");
    assert_eq!(other.status(), 200);
    assert!(other.json::<Value>().await.expect("state")["products"]
        .as_array()
        .expect("products")
        .is_empty());
    drop(server);

    // A real process restart provisions idempotently and keeps command receipts.
    let mut server = Process::start(&env);
    let base = server.ready(&client).await;
    let response = client
        .post(format!("{base}/v1/warehouses/wh-main/commands"))
        .bearer_auth(&key)
        .json(&request)
        .send()
        .await
        .expect("replay");
    assert_eq!(response.status(), 200);
    let replay = response.json::<Value>().await.expect("replay result");
    assert_eq!(replay["replayed"], true);
    assert_eq!(replay["result"], saved["result"]);
    drop(server);

    env.remove("WMS_API_KEYS_JSON");
    let mut server = Process::start(&env);
    let base = server.ready(&client).await;
    assert_eq!(
        client
            .get(format!("{base}/v1/session"))
            .bearer_auth(&key)
            .send()
            .await
            .expect("retired key")
            .status(),
        401
    );
    let state = store
        .state(&tenants[0], "wh-main")
        .await
        .expect("retained work");
    assert_eq!(state.version, 1);
    assert_eq!(state.products.len(), 1);
    for tenant in &tenants[1..] {
        assert_eq!(
            store
                .state(tenant, "wh-main")
                .await
                .expect("isolated tenant")
                .version,
            0
        );
    }
    drop(server);
    store.pool.close().await;
}
