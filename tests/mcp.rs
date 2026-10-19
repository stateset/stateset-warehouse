//! Exercise the actual HTTP transport, canonical authorization and journal.
use axum::{
    body::{to_bytes, Body},
    http::{Request, StatusCode},
    Router,
};
use rmcp::{
    model::{CallToolRequestParams, ClientConfig, ProtocolVersion},
    transport::{
        streamable_http_client::StreamableHttpClientTransportConfig, StreamableHttpClientTransport,
    },
    ServiceExt as McpServiceExt,
};
use serde_json::{json, Value};
use stateset_warehouse::{
    api::{router, App},
    integrations::Connections,
    mcp::Config,
    store::{Identity, Store},
};
use std::{sync::Arc, time::Duration};
use tower::ServiceExt;

fn application(store: Store, tenant: &str) -> Router {
    router(App {
        store,
        demo: false,
        connections: Connections::default(),
        credentials: Arc::new(
            [
                ("admin", tenant, "alice", "admin"),
                ("worker", tenant, "worker", "operator"),
                ("viewer", tenant, "reader", "viewer"),
                ("oms", tenant, "oms", "oms"),
                ("bob", tenant, "bob", "admin"),
                ("outsider", "mcp-other-tenant", "alice", "admin"),
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
fn lazy() -> Router {
    application(
        Store {
            pool: sqlx::postgres::PgPoolOptions::new()
                .connect_lazy("postgres://unused@127.0.0.1:1/unused")
                .expect("lazy pool"),
        },
        "mcp-tenant",
    )
}
fn call(name: &str, args: Value) -> Value {
    json!({"jsonrpc":"2.0","id":7,"method":"tools/call","params":{"name":name,"arguments":args}})
}
async fn rpc(
    app: &Router,
    token: &str,
    body: Value,
    headers: &[(&str, &str)],
) -> (StatusCode, Value) {
    let mut request = Request::builder()
        .method("POST")
        .uri("/mcp")
        .header("host", "localhost:8088")
        .header("content-type", "application/json")
        .header("accept", "application/json, text/event-stream")
        .header("mcp-protocol-version", "2025-11-25")
        .header("x-request-id", "forged")
        .header("x-tenant-id", "victim")
        .header("x-role", "admin");
    if !token.is_empty() {
        request = request.header("authorization", format!("Bearer {token}"));
    }
    let mut request = request.body(Body::from(body.to_string())).expect("request");
    for (name, value) in headers {
        request.headers_mut().insert(
            axum::http::HeaderName::from_bytes(name.as_bytes()).expect("name"),
            value.parse().expect("value"),
        );
    }
    let response = tokio::time::timeout(Duration::from_secs(8), app.clone().oneshot(request))
        .await
        .expect("bounded response")
        .expect("HTTP response");
    let status = response.status();
    assert_eq!(response.headers()["cache-control"], "no-store");
    assert_ne!(response.headers()["x-request-id"], "forged");
    let bytes = to_bytes(response.into_body(), 16 * 1024 * 1024)
        .await
        .expect("response body");
    let body = serde_json::from_slice(&bytes)
        .unwrap_or_else(|_| json!({"text":String::from_utf8_lossy(&bytes)}));
    (status, body)
}
async fn tool(app: &Router, token: &str, name: &str, args: Value) -> Value {
    let (status, body) = rpc(app, token, call(name, args), &[]).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.get("error").is_none(), "{body}");
    body["result"].clone()
}
fn payload(result: &Value, status: u16) -> Value {
    assert_eq!(
        result["structuredContent"]["http_status"], status,
        "{result}"
    );
    assert_eq!(
        result["isError"].as_bool().unwrap_or(false),
        status >= 400,
        "{result}"
    );
    result["structuredContent"]["body"].clone()
}

#[test]
fn configuration_rejects_wildcards_ambiguous_targets_and_insecure_browser_origins() {
    Config::parse(r#"{"allowed_hosts":["warehouse.example:443","127.0.0.1:8088","[::1]"],"allowed_origins":["https://agents.example","http://localhost:8080"]}"#).expect("explicit trusted authorities");
    for raw in [
        r#"{"allowed_hosts":[]}"#,
        r#"{"allowed_hosts":["*"]}"#,
        r#"{"allowed_hosts":["user@host"]}"#,
        r#"{"allowed_hosts":["https://host"]}"#,
        r#"{"allowed_hosts":["host/path"]}"#,
        r#"{"allowed_hosts":["host"],"allowed_origins":["null"]}"#,
        r#"{"allowed_hosts":["host"],"allowed_origins":["http://agents.example"]}"#,
        r#"{"allowed_hosts":["host"],"allowed_origins":["https://agents.example/path"]}"#,
        r#"{"allowed_hosts":["host"],"allowed_origins":["https://user@agents.example"]}"#,
        r#"{"allowed_hosts":["host"],"disable_auth":true}"#,
    ] {
        assert!(Config::parse(raw).is_err(), "accepted {raw}");
    }
}

#[tokio::test]
async fn discovery_and_every_call_use_current_http_identity() {
    let app = lazy();
    let list = json!({"jsonrpc":"2.0","id":1,"method":"tools/list"});
    assert_eq!(
        rpc(&app, "", list.clone(), &[]).await.0,
        StatusCode::UNAUTHORIZED
    );
    let (_, admin) = rpc(&app, "admin", list.clone(), &[]).await;
    let tools = admin["result"]["tools"].as_array().expect("tool catalog");
    let catalog_bytes = serde_json::to_vec(tools).expect("catalog JSON").len();
    eprintln!(
        "MCP administrator catalog: {} tools, {catalog_bytes} bytes",
        tools.len()
    );
    assert!(
        catalog_bytes < 256 * 1024,
        "Discovery must not repeat every unrelated command schema: {catalog_bytes} bytes"
    );
    assert!(
        tools.len() > 45,
        "all WMS commands plus read and integration tools"
    );
    for name in [
        "wms_serials",
        "wms_serial_history",
        "wms_serial_observations",
        "wms_unit_packs",
        "wms_unit_observations",
        "wms_set_unit_pack",
        "wms_lot_trace",
        "wms_lots",
        "wms_record_lot_expiry",
        "wms_quarantine_lot",
        "wms_set_product_expiry_policy",
        "wms_correct_lot_expiry",
        "wms_release_lot_hold",
        "wms_record_transfer_loss",
        "wms_reverse_transfer_loss",
    ] {
        assert!(
            tools.iter().any(|t| t["name"] == name),
            "Missing typed quality tool {name}"
        );
    }
    let (_, worker_catalog) = rpc(&app, "worker", list.clone(), &[]).await;
    let worker_tools = worker_catalog["result"]["tools"]
        .as_array()
        .expect("worker tools");
    for name in ["wms_lots", "wms_record_lot_expiry", "wms_quarantine_lot"] {
        assert!(
            worker_tools.iter().any(|t| t["name"] == name),
            "Worker missing urgent safety tool {name}"
        );
    }
    for name in [
        "wms_set_unit_pack",
        "wms_set_product_expiry_policy",
        "wms_correct_lot_expiry",
        "wms_release_lot_hold",
    ] {
        assert!(
            !worker_tools.iter().any(|t| t["name"] == name),
            "Supervisor tool exposed to worker {name}"
        );
        payload(&tool(&app, "worker", name, json!({})).await, 403);
    }
    let (_, viewer) = rpc(&app, "viewer", list.clone(), &[]).await;
    assert!(viewer["result"]["tools"]
        .as_array()
        .expect("viewer tools")
        .iter()
        .all(|t| t["annotations"]["readOnlyHint"] == true));
    let (_, oms) = rpc(&app, "oms", list.clone(), &[]).await;
    assert!(!oms["result"]["tools"]
        .as_array()
        .expect("OMS tools")
        .iter()
        .any(|t| t["name"] == "wms_move_stock"));
    let identity = payload(&tool(&app, "viewer", "wms_session", json!({})).await, 200);
    assert_eq!(
        identity["identity"],
        json!({"tenant":"mcp-tenant","actor":"reader","role":"viewer"})
    );
    let other = payload(&tool(&app, "outsider", "wms_session", json!({})).await, 200);
    assert_eq!(other["identity"]["tenant"], "mcp-other-tenant");
    // No initialization/session identifier or earlier privileged request grants access.
    assert_eq!(
        rpc(
            &app,
            "",
            call("wms_session", json!({})),
            &[("mcp-session-id", "invented")]
        )
        .await
        .0,
        StatusCode::UNAUTHORIZED
    );
    for name in [
        "wms_move_stock",
        "wms_transport",
        "wms_requeue_delivery",
        "wms_metrics",
        "wms_import_oms",
    ] {
        payload(&tool(&app, "viewer", name, json!({})).await, 403);
    }
    // Validate every local $ref from the full input root, including nested line types.
    fn references(value: &Value, root: &Value) {
        if let Some(reference) = value.get("$ref").and_then(Value::as_str) {
            let target = reference.strip_prefix('#').expect("no remote schemas");
            assert!(
                root.pointer(target).is_some(),
                "unresolved schema reference {reference}"
            );
        }
        match value {
            Value::Object(m) => {
                for v in m.values() {
                    references(v, root)
                }
            }
            Value::Array(a) => {
                for v in a {
                    references(v, root)
                }
            }
            _ => (),
        }
    }
    for definition in tools {
        references(&definition["inputSchema"], &definition["inputSchema"]);
        assert_eq!(definition["inputSchema"]["additionalProperties"], false);
    }
    let pack = tools
        .iter()
        .find(|t| t["name"] == "wms_pack_shipment")
        .expect("package tool");
    assert_eq!(
        pack["inputSchema"]["properties"]["request"]["properties"]["command"]["properties"]["type"]
            ["const"],
        "pack_shipment"
    );
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL; scripts/local-postgres.sh"]
async fn real_mcp_lot_trace_preserves_long_original_cursors_scope_and_read_only_source_reviews() {
    let (store, identity, app) = database().await;
    let first_bin = format!("TRACE-{}-A", "B".repeat(190));
    let second_bin = format!("TRACE-{}-B", "B".repeat(190));
    command(&app,json!({"type":"create_product","sku":"TRACE-SKU","name":"Synthetic trace product","category":"Test","reorder_point":0})).await;
    for bin in [&first_bin, &second_bin] {
        command(
            &app,
            json!({"type":"create_location","code":bin,"zone":"Trace","kind":"storage"}),
        )
        .await;
        command(&app,json!({"type":"count_stock","sku":"TRACE-SKU","lot":"TRACE-LOT","location":bin,"counted":2,"reason":"Observed synthetic trace custody"})).await;
    }
    let before:(i64,i64)=sqlx::query_as("SELECT (SELECT count(*) FROM wms_commands WHERE tenant=$1),(SELECT count(*) FROM wms_events WHERE tenant=$1)").bind(&identity.tenant).fetch_one(&store.pool).await.expect("isolated MCP trace fixture and original evidence");
    let first = payload(
        &tool(
            &app,
            "viewer",
            "wms_lot_trace",
            json!({"sku":"TRACE-SKU","lot":"TRACE-LOT","section":"positions","limit":1}),
        )
        .await,
        200,
    );
    assert_eq!(first["summary"]["physical"], 4);
    let cursor = first["next_cursor"].as_str().expect("second position");
    assert!(
        cursor.len() > 240,
        "exercise the actual canonical cursor beyond a generic resource name's length"
    );
    let second=payload(&tool(&app,"viewer","wms_lot_trace",json!({"sku":"TRACE-SKU","lot":"TRACE-LOT","section":"positions","limit":1,"review":first["review"],"after":cursor})).await,200);
    assert_eq!(
        second["items"]
            .as_array()
            .expect("isolated MCP trace fixture and original evidence")
            .len(),
        1
    );
    assert_ne!(first["items"][0]["key"], second["items"][0]["key"]);
    assert!(second["next_cursor"].is_null());
    let foreign = payload(
        &tool(
            &app,
            "outsider",
            "wms_lot_trace",
            json!({"sku":"TRACE-SKU","lot":"TRACE-LOT"}),
        )
        .await,
        200,
    );
    assert_eq!(foreign["items"], json!([]));
    assert_eq!(foreign["summary"]["physical"], 0);
    for args in [
        json!({"sku":"TRACE-SKU"}),
        json!({"sku":"TRACE-SKU","lot":"TRACE-LOT","tenant":identity.tenant}),
        json!({"sku":"TRACE-SKU","lot":"TRACE-LOT","section":"unknown"}),
        json!({"sku":"TRACE-SKU","lot":"TRACE-LOT","after":"x".repeat(4097)}),
    ] {
        payload(&tool(&app, "viewer", "wms_lot_trace", args).await, 422);
    }
    let after:(i64,i64)=sqlx::query_as("SELECT (SELECT count(*) FROM wms_commands WHERE tenant=$1),(SELECT count(*) FROM wms_events WHERE tenant=$1)").bind(&identity.tenant).fetch_one(&store.pool).await.expect("isolated MCP trace fixture and original evidence");
    assert_eq!(
        after, before,
        "trace reads cannot create a warehouse command or event"
    );
    command(&app,json!({"type":"quarantine_lot","sku":"TRACE-SKU","lot":"TRACE-LOT","reason":"New restriction invalidates the old source review"})).await;
    payload(&tool(&app,"viewer","wms_lot_trace",json!({"sku":"TRACE-SKU","lot":"TRACE-LOT","section":"positions","review":first["review"],"after":cursor})).await,409);
}

#[tokio::test]
async fn tool_input_never_selects_a_principal_remote_url_or_different_command() {
    let app = lazy();
    for args in [
        json!({"tenant":"victim"}),
        json!({"actor":"admin"}),
        json!({"url":"https://evil.invalid"}),
        json!({"_meta":{"role":"admin"}}),
    ] {
        payload(&tool(&app, "admin", "wms_session", args).await, 422);
    }
    for args in [
        json!({"warehouse_id":"wh-main","limit":101}),
        json!({"warehouse_id":"wh-main","limit":1.5}),
        json!({"warehouse_id":"wh-main","open_only":"true"}),
    ] {
        payload(&tool(&app, "admin", "wms_transfers", args).await, 422);
    }
    payload(
        &tool(
            &app,
            "admin",
            "wms_history",
            json!({"warehouse_id":"wh-main","kind":"../../operations/metrics"}),
        )
        .await,
        422,
    );
    payload(&tool(&app,"admin","wms_allocate",json!({"warehouse_id":"wh-main","request":{"request_id":"original-request","expected_version":0,"command":{"type":"cancel_order","order_id":"123"}}})).await,422);
    payload(&tool(&app,"oms","wms_create_order",json!({"warehouse_id":"wh-main","request":{"request_id":"original-request","expected_version":0,"command":{"type":"create_order","reference":"test","customer":"test","destination":"test","priority":"normal","lines":[]}}})).await,403);
    payload(&tool(&app,"admin","wms_allocate",json!({"warehouse_id":"wh-main","request":{"request_id":"original-request","command":{"type":"allocate","order_id":"123"}}})).await,422);
    let (status, error) = rpc(&app, "admin", call("wms_arbitrary_http", json!({})), &[]).await;
    assert_eq!(status, StatusCode::OK);
    assert!(error.get("error").is_some(), "{error}");
}

#[tokio::test]
async fn transport_rejects_dns_rebinding_untrusted_origins_and_unbounded_bodies() {
    let app = lazy();
    let request = call("wms_session", json!({}));
    for headers in [
        &[("host", "evil.invalid")][..],
        &[("host", "localhost.evil.invalid")],
        &[("origin", "https://evil.invalid")],
        &[("origin", "null")],
    ] {
        assert_eq!(
            rpc(&app, "viewer", request.clone(), headers).await.0,
            StatusCode::FORBIDDEN
        );
    }
    payload(&tool(&app, "viewer", "wms_session", json!({})).await, 200);
    assert_eq!(
        rpc(&app, "viewer", request.clone(), &[("host", "[::1]:8088")])
            .await
            .0,
        StatusCode::OK
    );
    let body = call("wms_session", json!({"padding":"x".repeat(513*1024)}));
    assert_eq!(
        rpc(&app, "admin", body, &[]).await.0,
        StatusCode::PAYLOAD_TOO_LARGE
    );
}

#[tokio::test]
async fn modern_stateless_requests_check_protocol_metadata_and_routing_headers() {
    let app = lazy();
    let mut body = call("wms_session", json!({}));
    body["params"]["_meta"] = json!({"io.modelcontextprotocol/protocolVersion":"2026-07-28",
        "io.modelcontextprotocol/clientInfo":{"name":"wms-contract","version":"1"},
        "io.modelcontextprotocol/clientCapabilities":{}});
    let headers = [
        ("mcp-protocol-version", "2026-07-28"),
        ("mcp-method", "tools/call"),
        ("mcp-name", "wms_session"),
    ];
    let (status, result) = rpc(&app, "viewer", body.clone(), &headers).await;
    assert_eq!(status, StatusCode::OK, "{result}");
    payload(&result["result"], 200);
    let mut bad = headers;
    bad[2] = ("mcp-name", "wms_move_stock");
    assert_eq!(
        rpc(&app, "viewer", body.clone(), &bad).await.0,
        StatusCode::BAD_REQUEST
    );
    body["params"]["_meta"]["io.modelcontextprotocol/protocolVersion"] = json!("2025-11-25");
    assert_eq!(
        rpc(&app, "viewer", body, &headers).await.0,
        StatusCode::BAD_REQUEST
    );
}

struct Listener {
    url: String,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for Listener {
    fn drop(&mut self) {
        self.task.abort();
    }
}
impl Listener {
    async fn start(app: Router) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("isolated loopback listener");
        let url = format!(
            "http://{}/mcp",
            listener.local_addr().expect("bound address")
        );
        let task = tokio::spawn(async move {
            axum::serve(listener, app).await.expect("owned server");
        });
        Self { url, task }
    }
}

#[tokio::test]
async fn official_sdk_negotiates_legacy_http_and_modern_discovery() {
    let server = Listener::start(lazy()).await;
    for version in [ProtocolVersion::V_2025_11_25, ProtocolVersion::V_2026_07_28] {
        let transport = StreamableHttpClientTransport::from_config(
            StreamableHttpClientTransportConfig::with_uri(server.url.clone()).auth_header("viewer"),
        );
        let mut info = ClientConfig::default();
        info.protocol_version = version;
        let client = tokio::time::timeout(Duration::from_secs(8), info.serve(transport))
            .await
            .expect("bounded SDK discovery")
            .expect("SDK handshake/discovery");
        let tools = client.list_all_tools().await.expect("SDK tool discovery");
        assert!(tools.iter().any(|t| t.name == "wms_session"));
        let result = client
            .call_tool(CallToolRequestParams::new("wms_session"))
            .await
            .expect("SDK call");
        assert_eq!(
            result.structured_content.expect("structured response")["body"]["identity"]["actor"],
            "reader"
        );
        client.cancel().await.expect("client shutdown");
    }
}

async fn database() -> (Store, Identity, Router) {
    let url = std::env::var("WMS_TEST_DATABASE_URL").expect("isolated database URL");
    assert!(reqwest::Url::parse(&url)
        .expect("DB URL")
        .path()
        .contains("test"));
    let store = Store::connect(&url).await.expect("isolated database");
    let identity = Identity {
        tenant: format!("mcp-{}", uuid::Uuid::new_v4()),
        actor: "alice".into(),
        role: "admin".into(),
    };
    store
        .provision(&identity.tenant, false)
        .await
        .expect("synthetic facility");
    let app = application(store.clone(), &identity.tenant);
    (store, identity, app)
}
async fn command(app: &Router, body: Value) -> Value {
    let name = format!("wms_{}", body["type"].as_str().expect("command type"));
    let state = payload(
        &tool(
            app,
            "admin",
            "wms_snapshot",
            json!({"warehouse_id":"wh-main"}),
        )
        .await,
        200,
    );
    let request = json!({"request_id":uuid::Uuid::new_v4().to_string(),"expected_version":state["version"],"command":body});
    payload(
        &tool(
            app,
            "admin",
            &name,
            json!({"warehouse_id":"wh-main","request":request}),
        )
        .await,
        200,
    )
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL; scripts/local-postgres.sh"]
async fn real_http_mcp_partial_fulfillment_and_exact_retry_share_the_canonical_journal() {
    let (store, identity, app) = database().await;
    command(&app,json!({"type":"create_product","sku":"MCP-SKU","name":"Synthetic","category":"Test","reorder_point":1})).await;
    command(
        &app,
        json!({"type":"create_location","code":"A-01","zone":"Storage","kind":"storage"}),
    )
    .await;
    command(&app,json!({"type":"count_stock","sku":"MCP-SKU","lot":"LOT","location":"A-01","counted":3,"reason":"Synthetic observed count"})).await;
    let state = payload(
        &tool(
            &app,
            "admin",
            "wms_snapshot",
            json!({"warehouse_id":"wh-main"}),
        )
        .await,
        200,
    );
    let original = json!({"request_id":uuid::Uuid::new_v4().to_string(),"expected_version":state["version"],"command":{
        "type":"create_order","reference":"MCP-ORDER","customer":"Synthetic","destination":"Test dock","priority":"normal",
        "lines":[{"id":"line","sku":"MCP-SKU","quantity":5}]}});
    let args = json!({"warehouse_id":"wh-main","request":original});
    // Use real TCP for the save and discard its complete response, simulating
    // a caller that lost the result after the warehouse committed.
    let server = Listener::start(app.clone()).await;
    let wire = reqwest::Client::new()
        .post(&server.url)
        .bearer_auth("admin")
        .header("accept", "application/json, text/event-stream")
        .header("mcp-protocol-version", "2025-11-25")
        .json(&call("wms_create_order", args.clone()))
        .send()
        .await
        .expect("wire save");
    assert_eq!(wire.status(), StatusCode::OK);
    drop(wire);
    let receipt = payload(
        &tool(
            &app,
            "admin",
            "wms_command_receipt",
            json!({"warehouse_id":"wh-main","request_id":original["request_id"]}),
        )
        .await,
        200,
    );
    let oid = receipt["result"]["entity_id"]
        .as_str()
        .expect("original order")
        .to_string();
    command(&app, json!({"type":"allocate_available","order_id":oid})).await;
    let replay = payload(
        &tool(&app, "admin", "wms_create_order", args.clone()).await,
        200,
    );
    assert_eq!(
        replay["result"], receipt["result"],
        "exact replay preserves the original version and event"
    );
    payload(
        &tool(&app, "bob", "wms_create_order", args.clone()).await,
        409,
    );
    let hidden = payload(
        &tool(
            &app,
            "outsider",
            "wms_command_receipt",
            json!({"warehouse_id":"wh-main","request_id":original["request_id"]}),
        )
        .await,
        200,
    );
    assert_eq!(
        hidden["found"], false,
        "another tenant cannot recover this request"
    );
    let mut stale = original.clone();
    stale["request_id"] = json!(uuid::Uuid::new_v4().to_string());
    let conflict = payload(
        &tool(
            &app,
            "admin",
            "wms_create_order",
            json!({"warehouse_id":"wh-main","request":stale}),
        )
        .await,
        409,
    );
    assert!(
        conflict.get("command_outcome").is_some(),
        "original structured conflict retained: {conflict}"
    );
    command(&app,json!({"type":"pick","order_id":oid,"line_id":"line","location":"A-01","lot":"LOT","quantity":2})).await;
    let packed = command(
        &app,
        json!({"type":"pack_shipment","order_id":oid,"reference":"MCP-PACKAGE",
        "lines":[{"line_id":"line","location":"A-01","lot":"LOT","quantity":2}]}),
    )
    .await;
    let package = packed["result"]["entity_id"].as_str().expect("package");
    command(&app,json!({"type":"handoff_shipment","shipment_id":package,"carrier":"Observed synthetic carrier","handoff_reference":"ACTUAL-TEST-HANDOFF"})).await;
    let progress = payload(
        &tool(
            &app,
            "worker",
            "wms_shipments",
            json!({"warehouse_id":"wh-main","order_id":oid}),
        )
        .await,
        200,
    );
    assert_eq!(progress["progress"][0]["shipped"], 2, "{progress}");
    let eligible = payload(
        &tool(
            &app,
            "worker",
            "wms_return_eligibility",
            json!({"warehouse_id":"wh-main","order_id":oid}),
        )
        .await,
        200,
    );
    assert_eq!(eligible["lines"][0]["shipped"], 2);
    command(&app,json!({"type":"create_return","reference":"MCP-RMA","order_id":oid,"reason":"Observed return",
        "lines":[{"id":"return-line","order_line_id":"line","lot":"LOT","quantity":1}]})).await;
    let proof = payload(
        &tool(
            &app,
            "admin",
            "wms_reconciliation",
            json!({"warehouse_id":"wh-main"}),
        )
        .await,
        200,
    );
    assert_eq!(proof["ok"], true, "{proof}");
    let state = store
        .state(&identity.tenant, "wh-main")
        .await
        .expect("independent SQL state");
    assert_eq!(state.orders.len(), 1);
    assert_eq!(state.orders[0].status, "partially_shipped");
    assert_eq!(state.stocks[0].on_hand, 1);
    let count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM wms_commands WHERE tenant=$1 AND request_id=$2")
            .bind(&identity.tenant)
            .bind(original["request_id"].as_str().expect("request ID"))
            .fetch_one(&store.pool)
            .await
            .expect("journal proof");
    assert_eq!(count, 1);
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL; scripts/local-postgres.sh"]
async fn mcp_preserves_exact_resource_fences_and_rest_body_limits() {
    use stateset_warehouse::{concurrency, domain::Command};
    let (store, identity, app) = database().await;
    command(&app,json!({"type":"create_product","sku":"FENCE","name":"Synthetic","category":"Test","reorder_point":1})).await;
    command(
        &app,
        json!({"type":"create_location","code":"A-01","zone":"Storage","kind":"storage"}),
    )
    .await;
    command(&app,json!({"type":"count_stock","sku":"FENCE","lot":"LOT","location":"A-01","counted":3,"reason":"Observed count"})).await;
    let review = store
        .state(&identity.tenant, "wh-main")
        .await
        .expect("review state");
    let count = Command::CountStock {
        sku: "FENCE".into(),
        lot: "LOT".into(),
        location: "A-01".into(),
        counted: 4,
        reason: "Original observed count".into(),
    };
    let scoped = json!({"request_id":uuid::Uuid::new_v4().to_string(),"expected_version":review.version,
        "resource_versions":concurrency::reviewed_versions(&review,&count).expect("complete reviewed scope"),"command":count});
    // An unrelated catalog change leaves the exact reviewed stock scope valid.
    command(&app,json!({"type":"create_product","sku":"OTHER","name":"Other","category":"Test","reorder_point":1})).await;
    payload(
        &tool(
            &app,
            "admin",
            "wms_count_stock",
            json!({"warehouse_id":"wh-main","request":scoped}),
        )
        .await,
        200,
    );
    let mut stale = scoped.clone();
    stale["request_id"] = json!(uuid::Uuid::new_v4().to_string());
    let conflict = payload(
        &tool(
            &app,
            "admin",
            "wms_count_stock",
            json!({"warehouse_id":"wh-main","request":stale}),
        )
        .await,
        409,
    );
    assert!(
        conflict.to_string().contains("changed_resources"),
        "{conflict}"
    );
    let before = store
        .state(&identity.tenant, "wh-main")
        .await
        .expect("before oversized save");
    let large = json!({"request_id":uuid::Uuid::new_v4().to_string(),"expected_version":before.version,
        "command":{"type":"count_stock","sku":"FENCE","lot":"LOT","location":"A-01","counted":99,"reason":"x".repeat(260*1024)}});
    // Below the MCP wire limit, above the canonical REST command-body limit.
    payload(
        &tool(
            &app,
            "admin",
            "wms_count_stock",
            json!({"warehouse_id":"wh-main","request":large}),
        )
        .await,
        413,
    );
    assert_eq!(
        store
            .state(&identity.tenant, "wh-main")
            .await
            .expect("after rejected save"),
        before
    );
    payload(
        &tool(
            &app,
            "admin",
            "wms_snapshot",
            json!({"warehouse_id":"wh-main/../../operations/metrics"}),
        )
        .await,
        404,
    );
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL; scripts/local-postgres.sh"]
async fn mcp_oms_closes_only_source_linked_partial_remainders_through_the_original_journal() {
    let (store, identity, app) = database().await;
    command(&app,json!({"type":"create_product","sku":"MCP-CLOSE","name":"Synthetic","category":"Test","reorder_point":1})).await;
    command(
        &app,
        json!({"type":"create_location","code":"A-01","zone":"Storage","kind":"storage"}),
    )
    .await;
    command(&app,json!({"type":"count_stock","sku":"MCP-CLOSE","lot":"LOT","location":"A-01","counted":3,"reason":"Observed synthetic count"})).await;
    let created=command(&app,json!({"type":"create_order","reference":"MCP-CLOSED-ORDER","source_order_id":"oms:synthetic-source:original-order",
        "customer":"Synthetic","destination":"Verified dock","priority":"normal","lines":[{"id":"line","sku":"MCP-CLOSE","quantity":5}]})).await;
    let oid = created["result"]["entity_id"].as_str().expect("order ID");
    command(&app, json!({"type":"allocate_available","order_id":oid})).await;
    command(&app,json!({"type":"pick","order_id":oid,"line_id":"line","location":"A-01","lot":"LOT","quantity":2})).await;
    let packed=command(&app,json!({"type":"pack_shipment","order_id":oid,"reference":"MCP-CLOSED-PACKAGE","lines":[{"line_id":"line","location":"A-01","lot":"LOT","quantity":2}]})).await;
    command(&app,json!({"type":"handoff_shipment","shipment_id":packed["result"]["entity_id"],"carrier":"Observed carrier","handoff_reference":"ACTUAL-SYNTHETIC-PICKUP"})).await;
    let review = payload(
        &tool(
            &app,
            "worker",
            "wms_shipments",
            json!({"warehouse_id":"wh-main","order_id":oid}),
        )
        .await,
        200,
    );
    let request = json!({"request_id":uuid::Uuid::new_v4().to_string(),"expected_version":review["version"],"command":{"type":"cancel_remainder","order_id":oid,"reason":"Customer cancelled all remaining demand"}});
    let args = json!({"warehouse_id":"wh-main","request":request});
    payload(
        &tool(&app, "viewer", "wms_cancel_remainder", args.clone()).await,
        403,
    );
    let accepted = payload(
        &tool(&app, "oms", "wms_cancel_remainder", args.clone()).await,
        200,
    );
    let replay = payload(&tool(&app, "oms", "wms_cancel_remainder", args).await, 200);
    assert_eq!(replay["result"], accepted["result"]);
    assert_eq!(replay["replayed"], true);
    let state = store
        .state(&identity.tenant, "wh-main")
        .await
        .expect("independent state");
    assert_eq!(state.orders[0].status, "closed_partial");
    assert_eq!(
        state.orders[0]
            .remainder_cancellation
            .as_ref()
            .expect("closure")
            .actor,
        "oms"
    );
    assert_eq!(state.stocks.iter().map(|s| s.reserved).sum::<i64>(), 0);
    let proof = payload(
        &tool(
            &app,
            "viewer",
            "wms_reconciliation",
            json!({"warehouse_id":"wh-main"}),
        )
        .await,
        200,
    );
    assert_eq!(proof["ok"], true);
    payload(
        &tool(
            &app,
            "worker",
            "wms_oms_proposal",
            json!({"warehouse_id":"wh-main","order_id":oid}),
        )
        .await,
        409,
    );
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL; scripts/local-postgres.sh"]
async fn real_mcp_lot_controls_share_actor_bound_recovery_roles_and_current_custody_view() {
    let (store, identity, app) = database().await;
    command(&app,json!({"type":"create_product","sku":"QUALITY-SKU","name":"Synthetic quality product","category":"Test","reorder_point":1})).await;
    command(&app,json!({"type":"record_lot_expiry","sku":"QUALITY-SKU","lot":"LOT","expires_at":"2000-01-01T00:00:00Z","reason":"Read actual original label"})).await;
    let version = store
        .state(&identity.tenant, "wh-main")
        .await
        .expect("coherent warehouse version")
        .version;
    let original = json!({"warehouse_id":"wh-main","request":{"request_id":uuid::Uuid::new_v4().to_string(),"expected_version":version,"command":{"type":"quarantine_lot","sku":"QUALITY-SKU","lot":"LOT","reason":"Separate actual quality concern"}}});
    let accepted = payload(
        &tool(&app, "worker", "wms_quarantine_lot", original.clone()).await,
        200,
    );
    let receipt = payload(
        &tool(
            &app,
            "worker",
            "wms_command_receipt",
            json!({"warehouse_id":"wh-main","request_id":original["request"]["request_id"]}),
        )
        .await,
        200,
    );
    assert_eq!(receipt["found"], true);
    assert_eq!(receipt["result"], accepted["result"]);
    payload(
        &tool(&app, "admin", "wms_quarantine_lot", original.clone()).await,
        409,
    );
    payload(
        &tool(
            &app,
            "outsider",
            "wms_lots",
            json!({"warehouse_id":"wh-main","sku":"QUALITY-SKU"}),
        )
        .await,
        404,
    );
    let lots = payload(
        &tool(
            &app,
            "viewer",
            "wms_lots",
            json!({"warehouse_id":"wh-main","sku":"QUALITY-SKU","limit":1}),
        )
        .await,
        200,
    );
    assert_eq!(lots["items"][0]["control"]["holds"][0]["actor"], "worker");
    assert_eq!(lots["items"][0]["availability"], "quarantined");
    assert_eq!(lots["items"][0]["physical"], 0);
    assert_eq!(lots["items"][0]["usable"], 0);
    let release = json!({"warehouse_id":"wh-main","request":{"request_id":uuid::Uuid::new_v4().to_string(),"expected_version":lots["version"],"command":{"type":"release_lot_hold","hold_id":accepted["result"]["entity_id"],"reason":"Supervisor closed exact investigation"}}});
    payload(
        &tool(&app, "worker", "wms_release_lot_hold", release.clone()).await,
        403,
    );
    payload(
        &tool(&app, "admin", "wms_release_lot_hold", release).await,
        200,
    );
    let replay = payload(
        &tool(&app, "worker", "wms_quarantine_lot", original).await,
        200,
    );
    assert_eq!(replay["replayed"], true);
    assert_eq!(replay["result"], accepted["result"]);
    let lots = payload(
        &tool(
            &app,
            "viewer",
            "wms_lots",
            json!({"warehouse_id":"wh-main","sku":"QUALITY-SKU"}),
        )
        .await,
        200,
    );
    assert_eq!(
        lots["items"][0]["availability"], "expired",
        "release cannot extend expiry"
    );
    assert_eq!(
        store
            .reconciliation(&identity.tenant, "wh-main")
            .await
            .expect("independent original proof")["ok"],
        true
    );
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL; scripts/local-postgres.sh"]
async fn real_mcp_transfer_loss_journal_reversal_and_replay_share_destination_authority_without_stock_credit(
) {
    let (store, identity, app) = database().await;
    command(&app,json!({"type":"create_product","sku":"LOSS-SKU","name":"Synthetic loss product","category":"Test","reorder_point":0})).await;
    command(
        &app,
        json!({"type":"create_location","code":"LOSS-A","zone":"Test","kind":"storage"}),
    )
    .await;
    command(&app,json!({"type":"count_stock","sku":"LOSS-SKU","lot":"LOT","location":"LOSS-A","counted":5,"reason":"Synthetic opening count"})).await;
    payload(&tool(&app,"admin","wms_create_warehouse",json!({"warehouse_id":"wh-east","request":{"request_id":uuid::Uuid::new_v4().to_string(),"expected_version":0,"command":{"type":"create_warehouse","code":"EAST","name":"Synthetic destination","city":"Vancouver","timezone":"America/Vancouver"}}})).await,200);
    payload(&tool(&app,"admin","wms_create_product",json!({"warehouse_id":"wh-east","request":{"request_id":uuid::Uuid::new_v4().to_string(),"expected_version":1,"command":{"type":"create_product","sku":"LOSS-SKU","name":"Synthetic loss product","category":"Test","reorder_point":0}}})).await,200);
    let dispatched=command(&app,json!({"type":"dispatch_transfer","reference":"LOSS-XFER","destination_warehouse_id":"wh-east","carrier":"Synthetic carrier","handoff_reference":"ORIGINAL-HANDOFF","lines":[{"id":"a","sku":"LOSS-SKU","lot":"LOT","location":"LOSS-A","quantity":3}]})).await;
    let tid = dispatched["result"]["entity_id"]
        .as_str()
        .expect("original transfer");
    let before = store
        .state(&identity.tenant, "wh-east")
        .await
        .expect("destination before loss");
    let original = json!({"warehouse_id":"wh-east","request":{"request_id":uuid::Uuid::new_v4().to_string(),"expected_version":before.version,"command":{"type":"record_transfer_loss","transfer_id":tid,"line_id":"a","quantity":2,"incident_reference":"CARRIER-CASE","reason":"Controller and carrier confirmed loss"}}});
    payload(
        &tool(&app, "worker", "wms_record_transfer_loss", original.clone()).await,
        403,
    );
    let accepted = payload(
        &tool(&app, "admin", "wms_record_transfer_loss", original.clone()).await,
        200,
    );
    let lid = accepted["result"]["entity_id"]
        .as_str()
        .expect("loss identity");
    let journal = payload(
        &tool(
            &app,
            "viewer",
            "wms_transfer_losses",
            json!({"warehouse_id":"wh-main","transfer_id":tid,"limit":1}),
        )
        .await,
        200,
    );
    assert_eq!(journal["page_source_ok"], true);
    assert_eq!(journal["items"][0]["id"], lid);
    assert_eq!(journal["items"][0]["quantity"], 2);
    payload(
        &tool(
            &app,
            "outsider",
            "wms_transfer_losses",
            json!({"warehouse_id":"wh-east","transfer_id":tid}),
        )
        .await,
        404,
    );
    let after = store
        .state(&identity.tenant, "wh-east")
        .await
        .expect("destination after loss");
    assert_eq!(before.stocks, after.stocks);
    assert_eq!(before.movements, after.movements);
    let reverse = json!({"warehouse_id":"wh-east","request":{"request_id":uuid::Uuid::new_v4().to_string(),"expected_version":after.version,"command":{"type":"reverse_transfer_loss","loss_id":lid,"quantity":1,"reason":"One unit located; actual receiving remains separate"}}});
    payload(
        &tool(&app, "worker", "wms_reverse_transfer_loss", reverse.clone()).await,
        403,
    );
    payload(
        &tool(&app, "admin", "wms_reverse_transfer_loss", reverse.clone()).await,
        200,
    );
    let current = store
        .state(&identity.tenant, "wh-east")
        .await
        .expect("destination after reversal");
    assert_eq!(before.stocks, current.stocks);
    assert_eq!(before.movements, current.movements);
    let replay = payload(
        &tool(&app, "admin", "wms_record_transfer_loss", original).await,
        200,
    );
    assert_eq!(replay["result"], accepted["result"]);
    assert_eq!(replay["replayed"], true);
    let page = payload(
        &tool(
            &app,
            "viewer",
            "wms_transfer_losses",
            json!({"warehouse_id":"wh-east","transfer_id":tid,"limit":1}),
        )
        .await,
        200,
    );
    assert_eq!(page["items"][0]["reversed"], 1);
    let next=payload(&tool(&app,"viewer","wms_transfer_losses",json!({"warehouse_id":"wh-east","transfer_id":tid,"limit":1,"after":page["next_cursor"],"review":page["review"]})).await,200);
    assert_eq!(next["items"][0]["kind"], "reversal");
    assert_eq!(
        store
            .reconciliation(&identity.tenant, "wh-east")
            .await
            .expect("independent physical and source proof")["ok"],
        true
    );
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn real_mcp_pack_observations_keep_original_definition_replay_and_authority() {
    let (store, identity, app) = database().await;
    command(&app,json!({"type":"create_product","sku":"PACK-SKU","name":"Synthetic pack product","category":"Test","reorder_point":0})).await;
    let before = store
        .state(&identity.tenant, "wh-main")
        .await
        .expect("isolated pack-unit fixture and original source evidence");
    let definition = json!({"warehouse_id":"wh-main","request":{"request_id":uuid::Uuid::new_v4().to_string(),"expected_version":before.version,"command":{"type":"set_unit_pack","sku":"PACK-SKU","code":"CASE","label":"Reviewed case","each_per_pack":12,"enabled":true,"reason":"Read original case label"}}});
    payload(
        &tool(&app, "worker", "wms_set_unit_pack", definition.clone()).await,
        403,
    );
    let defined = payload(
        &tool(&app, "admin", "wms_set_unit_pack", definition).await,
        200,
    );
    let did = defined["result"]["entity_id"]
        .as_str()
        .expect("isolated pack-unit fixture and original source evidence");
    let state = store
        .state(&identity.tenant, "wh-main")
        .await
        .expect("isolated pack-unit fixture and original source evidence");
    let original = json!({"warehouse_id":"wh-main","request":{"request_id":uuid::Uuid::new_v4().to_string(),"expected_version":state.version,"unit_inputs":[{"target":["a"],"sku":"PACK-SKU","pack_code":"CASE","definition_id":did,"pack_quantity":2,"each_per_pack":12}],"command":{"type":"create_receipt","reference":"PACK-MCP","supplier":"Synthetic supplier","lines":[{"id":"a","sku":"PACK-SKU","lot":"LOT","quantity":24}]}}});
    payload(
        &tool(&app, "viewer", "wms_create_receipt", original.clone()).await,
        403,
    );
    let saved = payload(
        &tool(&app, "worker", "wms_create_receipt", original.clone()).await,
        200,
    );
    command(&app,json!({"type":"set_unit_pack","sku":"PACK-SKU","code":"CASE","label":"Reviewed new case","each_per_pack":6,"enabled":true,"reason":"New case label"})).await;
    let replay = payload(
        &tool(&app, "worker", "wms_create_receipt", original.clone()).await,
        200,
    );
    assert_eq!(replay["result"], saved["result"]);
    assert_eq!(replay["replayed"], true);
    let current = payload(
        &tool(
            &app,
            "viewer",
            "wms_unit_packs",
            json!({"warehouse_id":"wh-main","sku":"PACK-SKU","code":"CASE"}),
        )
        .await,
        200,
    );
    assert_eq!(current["total_records"], 1);
    assert_eq!(current["items"][0]["each_per_pack"], 6);
    let observations = payload(
        &tool(
            &app,
            "viewer",
            "wms_unit_observations",
            json!({"warehouse_id":"wh-main","sku":"PACK-SKU"}),
        )
        .await,
        200,
    );
    assert_eq!(observations["total_records"], 1);
    assert_eq!(observations["items"][0]["definition_id"], did);
    assert_eq!(observations["items"][0]["each_quantity"], 24);
    assert_eq!(observations["page_source_ok"], true);
    let mut stale = original;
    stale["request"]["request_id"] = json!(uuid::Uuid::new_v4().to_string());
    stale["request"]["expected_version"] = json!(
        store
            .state(&identity.tenant, "wh-main")
            .await
            .expect("isolated pack-unit fixture and original source evidence")
            .version
    );
    payload(
        &tool(&app, "worker", "wms_create_receipt", stale).await,
        409,
    );
    payload(
        &tool(
            &app,
            "outsider",
            "wms_unit_observations",
            json!({"warehouse_id":"wh-main","sku":"PACK-SKU"}),
        )
        .await,
        404,
    );
}
