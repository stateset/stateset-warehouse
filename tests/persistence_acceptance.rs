//! Real PostgreSQL acceptance through the HTTP service, without a TCP listener.
//! Run with scripts/local-postgres.sh, or explicitly supply an isolated test DB.
use axum::{
    body::Body,
    http::{Request as HttpRequest, StatusCode},
    Router,
};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use stateset_warehouse::{
    api::{router, App},
    domain::{id, Command, LineInput, ReceiptInput, State},
    integrations::Connections,
    store::{Identity, Request, Store},
};
use std::sync::Arc;
use tower::ServiceExt;

struct Fixture {
    store: Store,
    identity: Identity,
    app: Router,
}

impl Fixture {
    async fn new() -> Self {
        let url = std::env::var("WMS_TEST_DATABASE_URL")
            .expect("set WMS_TEST_DATABASE_URL to an isolated PostgreSQL database");
        assert!(
            reqwest::Url::parse(&url)
                .expect("database URL")
                .path()
                .contains("test"),
            "acceptance database name must contain test"
        );
        let store = Store::connect(&url).await.expect("PostgreSQL connection");
        let identity = Identity {
            tenant: format!("acceptance-{}", id()),
            actor: "acceptance:admin".into(),
            role: "admin".into(),
        };
        store
            .provision(&identity.tenant, false)
            .await
            .expect("isolated warehouse");
        let app = router(App {
            store: store.clone(),
            credentials: Arc::new(vec![
                ("admin-acceptance".into(), identity.clone()),
                (
                    "operator-acceptance".into(),
                    Identity {
                        role: "operator".into(),
                        ..identity.clone()
                    },
                ),
                (
                    "viewer-acceptance".into(),
                    Identity {
                        role: "viewer".into(),
                        ..identity.clone()
                    },
                ),
                (
                    "other-admin-acceptance".into(),
                    Identity {
                        tenant: identity.tenant.clone(),
                        actor: "acceptance:other-admin".into(),
                        role: "admin".into(),
                    },
                ),
                (
                    "oms-acceptance".into(),
                    Identity {
                        tenant: identity.tenant.clone(),
                        actor: "acceptance:oms".into(),
                        role: "oms".into(),
                    },
                ),
                (
                    "outsider-acceptance".into(),
                    Identity {
                        tenant: format!("outsider-{}", id()),
                        actor: "acceptance:outsider".into(),
                        role: "admin".into(),
                    },
                ),
            ]),
            demo: false,
            connections: Connections::default(),
        });
        Self {
            store,
            identity,
            app,
        }
    }

    async fn state(&self) -> State {
        self.store
            .state(&self.identity.tenant, "wh-main")
            .await
            .expect("warehouse state")
    }

    async fn http(
        &self,
        method: &str,
        path: &str,
        token: &str,
        body: Option<Value>,
    ) -> (StatusCode, Value) {
        let response = self
            .app
            .clone()
            .oneshot(
                HttpRequest::builder()
                    .method(method)
                    .uri(path)
                    .header("Authorization", format!("Bearer {token}"))
                    .header("Content-Type", "application/json")
                    .body(body.map_or_else(Body::empty, |v| Body::from(v.to_string())))
                    .expect("HTTP request"),
            )
            .await
            .expect("HTTP response");
        let status = response.status();
        let bytes = response
            .into_body()
            .collect()
            .await
            .expect("HTTP body")
            .to_bytes();
        (status, serde_json::from_slice(&bytes).expect("JSON body"))
    }

    async fn submit(&self, token: &str, request: &Request) -> (StatusCode, Value) {
        self.http(
            "POST",
            "/v1/warehouses/wh-main/commands",
            token,
            Some(json!(request)),
        )
        .await
    }

    async fn command(&self, command: Command) -> Value {
        self.command_as("admin-acceptance", command).await
    }

    async fn command_as(&self, token: &str, command: Command) -> Value {
        let request = Request {
            serial_inputs: Vec::new(),
            unit_inputs: Vec::new(),
            request_id: id(),
            expected_version: self.state().await.version,
            resource_versions: None,
            command,
        };
        let (status, response) = self.submit(token, &request).await;
        assert_eq!(status, StatusCode::OK, "{response}");
        response["result"].clone()
    }

    async fn reject(&self, command: Command, expected: StatusCode) {
        let before = self.state().await;
        let request = Request {
            serial_inputs: Vec::new(),
            unit_inputs: Vec::new(),
            request_id: id(),
            expected_version: before.version,
            resource_versions: None,
            command,
        };
        let (status, response) = self.submit("admin-acceptance", &request).await;
        assert_eq!(status, expected, "{response}");
        assert_eq!(self.state().await, before, "rejected work mutated state");
    }

    async fn seed_catalog(&self) {
        self.command(Command::CreateProduct {
            serial_controlled: false,
            sku: "SKU-A".into(),
            name: "Warehouse acceptance product".into(),
            category: "General".into(),
            reorder_point: 1,
        })
        .await;
        self.command(Command::CreateLocation {
            code: "A-01".into(),
            zone: "Storage".into(),
            kind: "storage".into(),
        })
        .await;
    }
}

fn entity(result: &Value) -> String {
    result["entity_id"].as_str().expect("entity ID").into()
}

async fn command_evidence_counts(f: &Fixture) -> (i64, i64, i64) {
    sqlx::query_as(
        "SELECT (SELECT count(*) FROM wms_commands WHERE tenant=$1), (SELECT count(*) FROM wms_events WHERE tenant=$1), (SELECT count(*) FROM wms_outbox WHERE tenant=$1)",
    )
    .bind(&f.identity.tenant)
    .fetch_one(&f.store.pool)
    .await
    .expect("command, event and delivery counts")
}

fn opening_count(counted: i64) -> Command {
    Command::CountStock {
        sku: "SKU-A".into(),
        lot: "LOT-A".into(),
        location: "A-01".into(),
        counted,
        reason: "Verified opening count".into(),
    }
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL; scripts/local-postgres.sh"]
async fn stale_http_command_proves_noncommit_and_fences_a_delayed_original() {
    let f = Fixture::new().await;
    f.seed_catalog().await;
    let original = Request {
        serial_inputs: Vec::new(),
        unit_inputs: Vec::new(),
        request_id: id(),
        expected_version: f.state().await.version,
        resource_versions: None,
        command: opening_count(9),
    };
    // Another operator's committed work advances the shared version before
    // either copy of this request reaches the warehouse lock.
    f.command_as("other-admin-acceptance", opening_count(5))
        .await;
    let before = f.state().await;
    let before_counts = command_evidence_counts(&f).await;
    let (status, response) = f.submit("admin-acceptance", &original).await;
    assert_eq!(status, StatusCode::CONFLICT, "{response}");
    assert_eq!(
        response["command_outcome"],
        json!({
            "status":"not_committed",
            "reason":"stale_version",
            "tenant":f.identity.tenant,
            "actor":f.identity.actor,
            "warehouse_id":"wh-main",
            "request_id":original.request_id,
            "expected_version":original.expected_version,
            "current_version":before.version
        })
    );
    assert_eq!(command_evidence_counts(&f).await, before_counts);
    assert_eq!(
        f.state().await,
        before,
        "stale rejection changed stock or history"
    );
    let (status, receipt) = f
        .http(
            "GET",
            &format!("/v1/warehouses/wh-main/commands/{}", original.request_id),
            "admin-acceptance",
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(receipt, json!({"found":false}));

    // A delayed original arrives after the definitive rejection and another
    // commit. It remains stale rather than executing or becoming a receipt.
    f.command(opening_count(6)).await;
    let advanced = f.state().await;
    let advanced_counts = command_evidence_counts(&f).await;
    let (status, delayed) = f.submit("admin-acceptance", &original).await;
    assert_eq!(status, StatusCode::CONFLICT, "{delayed}");
    assert_eq!(
        delayed["command_outcome"]["current_version"],
        advanced.version
    );
    assert_eq!(
        delayed["command_outcome"]["request_id"],
        original.request_id
    );
    assert_eq!(f.state().await, advanced);
    assert_eq!(command_evidence_counts(&f).await, advanced_counts);
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL; scripts/local-postgres.sh"]
async fn command_noncommit_proof_never_overrides_receipts_conflicts_or_future_versions() {
    let f = Fixture::new().await;
    f.seed_catalog().await;
    let original = Request {
        serial_inputs: Vec::new(),
        unit_inputs: Vec::new(),
        request_id: id(),
        expected_version: f.state().await.version,
        resource_versions: None,
        command: opening_count(7),
    };
    let (status, saved) = f.submit("admin-acceptance", &original).await;
    assert_eq!(status, StatusCode::OK, "{saved}");
    f.command(opening_count(8)).await;
    let before = f.state().await;
    let before_counts = command_evidence_counts(&f).await;
    let (status, replayed) = f.submit("admin-acceptance", &original).await;
    assert_eq!(status, StatusCode::OK, "{replayed}");
    assert_eq!(replayed, json!({"replayed":true,"result":saved["result"]}));
    assert!(replayed.get("command_outcome").is_none());

    let mut changed = original.clone();
    changed.command = opening_count(11);
    let mut future = original.clone();
    future.request_id = id();
    future.expected_version = before.version + 1;
    let mut invalid_id = original.clone();
    invalid_id.request_id = "invalid".into();
    for (token, request, expected_status) in [
        ("admin-acceptance", &changed, StatusCode::CONFLICT),
        ("other-admin-acceptance", &original, StatusCode::CONFLICT),
        ("admin-acceptance", &future, StatusCode::CONFLICT),
        (
            "admin-acceptance",
            &invalid_id,
            StatusCode::UNPROCESSABLE_ENTITY,
        ),
        ("oms-acceptance", &original, StatusCode::FORBIDDEN),
        ("unknown-acceptance", &original, StatusCode::UNAUTHORIZED),
        ("outsider-acceptance", &original, StatusCode::NOT_FOUND),
    ] {
        let (status, response) = f.submit(token, request).await;
        assert_eq!(status, expected_status, "{response}");
        assert!(response["error"].is_string(), "{response}");
        assert!(response.get("command_outcome").is_none(), "{response}");
        assert_eq!(response.as_object().expect("error object").len(), 1);
    }
    let (status, response) = f
        .http(
            "POST",
            "/v1/warehouses/absent-warehouse/commands",
            "admin-acceptance",
            Some(json!(original)),
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(response.get("command_outcome").is_none(), "{response}");
    assert_eq!(f.state().await, before);
    assert_eq!(command_evidence_counts(&f).await, before_counts);
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL; scripts/local-postgres.sh"]
async fn http_warehouse_lifecycle_recovers_short_pick_and_reconciles() {
    let f = Fixture::new().await;
    f.seed_catalog().await;
    let receipt = entity(
        &f.command(Command::CreateReceipt {
            reference: "ASN-REPEATED-SKU".into(),
            supplier: "Supplier".into(),
            lines: vec![
                ReceiptInput {
                    id: "receipt-1".into(),
                    sku: "SKU-A".into(),
                    lot: "LOT-A".into(),
                    quantity: 5,
                },
                ReceiptInput {
                    id: "receipt-2".into(),
                    sku: "SKU-A".into(),
                    lot: "LOT-A".into(),
                    quantity: 7,
                },
            ],
        })
        .await,
    );
    let receive = Request {
        serial_inputs: Vec::new(),
        unit_inputs: Vec::new(),
        request_id: id(),
        expected_version: f.state().await.version,
        resource_versions: None,
        command: Command::Receive {
            receipt_id: receipt.clone(),
            line_id: "receipt-1".into(),
            quantity: 5,
            location: "RECV-01".into(),
        },
    };
    let (status, first) = f.submit("admin-acceptance", &receive).await;
    assert_eq!(status, StatusCode::OK, "{first}");
    let (status, replay) = f.submit("admin-acceptance", &receive).await;
    assert_eq!(status, StatusCode::OK, "{replay}");
    assert_eq!(replay["replayed"], true);
    assert_eq!(replay["result"], first["result"]);
    let received = f.state().await;
    assert_eq!(received.receipts[0].lines[0].received, 5);
    assert_eq!(received.receipts[0].lines[1].received, 0);
    f.command(Command::Receive {
        receipt_id: receipt,
        line_id: "receipt-2".into(),
        quantity: 7,
        location: "RECV-01".into(),
    })
    .await;
    f.command(Command::MoveStock {
        sku: "SKU-A".into(),
        lot: "LOT-A".into(),
        from: "RECV-01".into(),
        to: "A-01".into(),
        quantity: 12,
    })
    .await;
    let order = entity(
        &f.command_as(
            "oms-acceptance",
            Command::CreateOrder {
                reference: "SO-REPEATED-SKU".into(),
                source_order_id: Some("oms-order-acceptance".into()),
                customer: "Warehouse customer".into(),
                destination: "Vancouver".into(),
                priority: "normal".into(),
                lines: vec![
                    LineInput {
                        id: "order-1".into(),
                        sku: "SKU-A".into(),
                        quantity: 3,
                    },
                    LineInput {
                        id: "order-2".into(),
                        sku: "SKU-A".into(),
                        quantity: 5,
                    },
                ],
            },
        )
        .await,
    );
    f.command(Command::Allocate {
        order_id: order.clone(),
    })
    .await;
    for (line, quantity) in [("order-1", 3), ("order-2", 2)] {
        f.command(Command::Pick {
            order_id: order.clone(),
            line_id: line.into(),
            location: "A-01".into(),
            lot: "LOT-A".into(),
            quantity,
        })
        .await;
    }
    let exception = entity(
        &f.command(Command::ReportException {
            order_id: Some(order.clone()),
            category: "Short pick".into(),
            detail: "Physical source bin differs from the recorded quantity".into(),
        })
        .await,
    );
    f.reject(
        Command::Pack {
            order_id: order.clone(),
        },
        StatusCode::UNPROCESSABLE_ENTITY,
    )
    .await;
    f.command(Command::Unpick {
        order_id: order.clone(),
        line_id: "order-2".into(),
        location: "A-01".into(),
        lot: "LOT-A".into(),
        quantity: 2,
        reason: "Return second line for physical recount".into(),
    })
    .await;
    let s = f.state().await;
    assert_eq!(s.orders[0].allocations[0].picked, 3);
    assert_eq!(s.orders[0].allocations[1].picked, 0);
    assert_eq!(
        s.stocks
            .iter()
            .find(|b| b.location == "STAGE-01")
            .expect("staging")
            .on_hand,
        3
    );
    f.reject(
        Command::ResetAllocation {
            order_id: order.clone(),
            reason: "Cannot release the other line's staged units".into(),
        },
        StatusCode::UNPROCESSABLE_ENTITY,
    )
    .await;
    f.command(Command::Unpick {
        order_id: order.clone(),
        line_id: "order-1".into(),
        location: "A-01".into(),
        lot: "LOT-A".into(),
        quantity: 3,
        reason: "Return remaining staged units for recount".into(),
    })
    .await;
    f.command(Command::ResetAllocation {
        order_id: order.clone(),
        reason: "Release every reservation before recount".into(),
    })
    .await;
    f.command(Command::CountStock {
        sku: "SKU-A".into(),
        lot: "LOT-A".into(),
        location: "A-01".into(),
        counted: 8,
        reason: "Physical recount finds eight, four fewer than recorded".into(),
    })
    .await;
    f.command(Command::ResolveException {
        exception_id: exception,
        resolution: "All staged units returned and source inventory recounted".into(),
    })
    .await;
    f.command(Command::Allocate {
        order_id: order.clone(),
    })
    .await;
    for (line, quantity) in [("order-1", 3), ("order-2", 5)] {
        f.command(Command::Pick {
            order_id: order.clone(),
            line_id: line.into(),
            location: "A-01".into(),
            lot: "LOT-A".into(),
            quantity,
        })
        .await;
    }
    f.command(Command::Pack {
        order_id: order.clone(),
    })
    .await;
    f.command(Command::Ship {
        order_id: order.clone(),
        carrier: "Carrier".into(),
        handoff_reference: "TMS-HANDOFF-1".into(),
    })
    .await;
    let s = f.state().await;
    let (status, shipped) = f
        .http(
            "GET",
            &format!("/v1/warehouses/wh-main/orders/{order}"),
            "admin-acceptance",
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{shipped}");
    assert_eq!(shipped["status"], "shipped");
    assert_eq!(shipped["handoff_reference"], "TMS-HANDOFF-1");
    assert!(s.stocks.iter().all(|b| b.on_hand == 0 && b.reserved == 0));
    s.validate().expect("inventory and ownership reconcile");
    let (status, reconciliation) = f
        .http(
            "GET",
            "/v1/warehouses/wh-main/reconciliation",
            "admin-acceptance",
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(reconciliation["ok"], true);
    let (status, _) = f
        .http("GET", "/v1/warehouses/wh-main", "outsider-acceptance", None)
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, receipt) = f
        .http(
            "GET",
            &format!("/v1/warehouses/wh-main/commands/{}", receive.request_id),
            "admin-acceptance",
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(receipt["found"], true);
    let deliveries = f
        .store
        .outbox(&f.identity.tenant, "wh-main")
        .await
        .expect("outbox");
    assert_eq!(
        deliveries
            .iter()
            .filter(|d| d["destination"] == "tms")
            .count(),
        2
    );
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL; scripts/local-postgres.sh"]
async fn database_reconnect_preserves_receipts_and_api_authority() {
    let f = Fixture::new().await;
    f.seed_catalog().await;
    let request = Request {
        serial_inputs: Vec::new(),
        unit_inputs: Vec::new(),
        request_id: id(),
        expected_version: f.state().await.version,
        resource_versions: None,
        command: Command::CountStock {
            sku: "SKU-A".into(),
            lot: "LOT-A".into(),
            location: "A-01".into(),
            counted: 9,
            reason: "Physical opening count".into(),
        },
    };
    let (status, original) = f.submit("admin-acceptance", &request).await;
    assert_eq!(status, StatusCode::OK, "{original}");
    let before = f.state().await;
    f.store.pool.close().await;
    let restored = Store::connect(&std::env::var("WMS_TEST_DATABASE_URL").expect("URL"))
        .await
        .expect("reconnect");
    let after = restored
        .state(&f.identity.tenant, "wh-main")
        .await
        .expect("restored state");
    assert_eq!(before, after);
    let replay = restored
        .execute(&f.identity, "wh-main", &request)
        .await
        .expect("recover save");
    assert_eq!(replay["replayed"], true);
    assert_eq!(replay["result"], original["result"]);
    assert_eq!(
        restored
            .state(&f.identity.tenant, "wh-main")
            .await
            .expect("state"),
        after
    );
    let mut oms = f.identity.clone();
    oms.role = "oms".into();
    assert_eq!(
        restored
            .execute(&oms, "wh-main", &request)
            .await
            .expect_err("OMS cannot adjust stock")
            .status,
        403
    );
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL; scripts/local-postgres.sh"]
async fn history_above_previous_aggregate_limit_keeps_commands_operable() {
    let f = Fixture::new().await;
    f.seed_catalog().await;
    // Historical audit entries do not change inventory. Insert a realistic
    // volume directly to isolate history growth from command execution cost.
    sqlx::query(
        "INSERT INTO wms_activity (id,tenant,warehouse_id,kind,data) \
         SELECT 'history-' || $1 || '-' || n::text,$1,'wh-main','inventory.reviewed', \
         jsonb_build_object('id','history-' || $1 || '-' || n::text, \
             'kind','inventory.reviewed','reference','SCALING-ACCEPTANCE', \
             'detail',repeat('historical evidence ',20),'actor','acceptance:reviewer', \
             'at','2026-10-17T00:00:00Z') \
         FROM generate_series(1,30000) AS n",
    )
    .bind(&f.identity.tenant)
    .execute(&f.store.pool)
    .await
    .expect("append history independently of the operational projection");
    let bytes: i64 = sqlx::query_scalar(
        "SELECT sum(octet_length(data::text))::bigint FROM wms_activity WHERE tenant=$1",
    )
    .bind(&f.identity.tenant)
    .fetch_one(&f.store.pool)
    .await
    .expect("measure retained history");
    assert!(
        bytes > 8 * 1024 * 1024,
        "fixture must exceed the former ceiling"
    );
    let before = f.state().await;
    assert!(
        before.activity.len() <= 100,
        "operational view must bound history"
    );
    f.command(Command::CountStock {
        sku: "SKU-A".into(),
        lot: "LOT-A".into(),
        location: "A-01".into(),
        counted: 11,
        reason: "Physical count after extensive audit history".into(),
    })
    .await;
    let after = f.state().await;
    assert_eq!(after.version, before.version + 1);
    assert_eq!(after.stocks.iter().map(|b| b.on_hand).sum::<i64>(), 11);
    assert!(after.activity.len() <= 100);
    after
        .validate()
        .expect("bounded operational view reconciles");
    let retained: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM wms_activity WHERE tenant=$1 AND kind='inventory.reviewed'",
    )
    .bind(&f.identity.tenant)
    .fetch_one(&f.store.pool)
    .await
    .expect("retained historical evidence");
    assert_eq!(retained, 30000, "a new command must never truncate history");
    let (status, first_page) = f
        .http(
            "GET",
            "/v1/warehouses/wh-main/history/activity?limit=50",
            "admin-acceptance",
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{first_page}");
    let first_items = first_page["items"].as_array().expect("first history page");
    assert_eq!(first_items.len(), 50);
    let cursor = first_page["next_cursor"]
        .as_i64()
        .expect("older history cursor");
    let (status, second_page) = f
        .http(
            "GET",
            &format!("/v1/warehouses/wh-main/history/activity?limit=50&before={cursor}"),
            "admin-acceptance",
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{second_page}");
    let second_items = second_page["items"]
        .as_array()
        .expect("second history page");
    assert_eq!(second_items.len(), 50);
    assert!(second_items
        .iter()
        .all(|item| item["cursor"].as_i64().expect("history sequence") < cursor));
    assert!(second_items.iter().all(|older| !first_items
        .iter()
        .any(|newer| newer["cursor"] == older["cursor"])));
    assert!(second_items
        .iter()
        .all(|item| item["data"]["kind"] == "inventory.reviewed"));
    let (status, reconciliation) = f
        .http(
            "GET",
            "/v1/warehouses/wh-main/reconciliation",
            "admin-acceptance",
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(reconciliation["ok"], true);
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL; scripts/local-postgres.sh"]
async fn completed_receipts_outside_dashboard_window_keep_exact_evidence() {
    let f = Fixture::new().await;
    f.seed_catalog().await;
    let mut first_receipt = None;
    for n in 0..25 {
        let receipt = entity(
            &f.command(Command::CreateReceipt {
                reference: format!("ASN-HISTORY-{n}"),
                supplier: "History supplier".into(),
                lines: vec![ReceiptInput {
                    id: "line-1".into(),
                    sku: "SKU-A".into(),
                    lot: "LOT-A".into(),
                    quantity: 1,
                }],
            })
            .await,
        );
        first_receipt.get_or_insert_with(|| receipt.clone());
        f.command(Command::Receive {
            receipt_id: receipt,
            line_id: "line-1".into(),
            quantity: 1,
            location: "RECV-01".into(),
        })
        .await;
    }
    let s = f.state().await;
    assert_eq!(s.stocks.iter().map(|stock| stock.on_hand).sum::<i64>(), 25);
    s.validate()
        .expect("bounded receipt projection retains exact movement evidence");
    let (status, receipt) = f
        .http(
            "GET",
            &format!(
                "/v1/warehouses/wh-main/receipts/{}",
                first_receipt.expect("oldest receipt")
            ),
            "admin-acceptance",
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{receipt}");
    assert_eq!(receipt["status"], "received");
    assert_eq!(receipt["lines"][0]["received"], 1);
    // A new physical command must remain executable after the old receipt has
    // moved beyond the completed-work dashboard window.
    f.command(Command::MoveStock {
        sku: "SKU-A".into(),
        lot: "LOT-A".into(),
        from: "RECV-01".into(),
        to: "A-01".into(),
        quantity: 25,
    })
    .await;
    let (status, reconciliation) = f
        .http(
            "GET",
            "/v1/warehouses/wh-main/reconciliation",
            "admin-acceptance",
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(reconciliation["ok"], true);
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL; scripts/local-postgres.sh"]
async fn scoped_reviews_allow_disjoint_operators_and_preserve_exact_replays() {
    let f = Fixture::new().await;
    f.seed_catalog().await;
    let reviewed = f.state().await;
    let requests: Vec<_> = (0..4)
        .map(|n| {
            let command = Command::CountStock {
                sku: "SKU-A".into(),
                lot: format!("LOT-{n}"),
                location: "A-01".into(),
                counted: 10,
                reason: "Independent physical count".into(),
            };
            Request {
                serial_inputs: Vec::new(),
                unit_inputs: Vec::new(),
                request_id: id(),
                expected_version: reviewed.version,
                resource_versions: stateset_warehouse::concurrency::reviewed_versions(
                    &reviewed, &command,
                )
                .expect("review"),
                command,
            }
        })
        .collect();
    let results = tokio::join!(
        f.submit("admin-acceptance", &requests[0]),
        f.submit("other-admin-acceptance", &requests[1]),
        f.submit("admin-acceptance", &requests[2]),
        f.submit("other-admin-acceptance", &requests[3])
    );
    for result in [results.0, results.1, results.2, results.3] {
        assert_eq!(result.0, StatusCode::OK, "{}", result.1);
        assert_eq!(result.1["replayed"], false);
    }
    let before = f.state().await;
    let (status, replay) = f.submit("admin-acceptance", &requests[0]).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(replay["replayed"], true);
    assert_eq!(f.state().await, before);
    let (status, _) = f.submit("other-admin-acceptance", &requests[0]).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(before.version, reviewed.version + 4);
    assert!(f
        .store
        .reconciliation(&f.identity.tenant, "wh-main")
        .await
        .expect("proof")["ok"]
        .as_bool()
        .expect("ok"));
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL; scripts/local-postgres.sh"]
async fn scoped_aba_changes_are_permanently_fenced_and_prove_noncommit() {
    let f = Fixture::new().await;
    f.seed_catalog().await;
    f.command(opening_count(9)).await;
    let state = f.state().await;
    let command = opening_count(7);
    let request = Request {
        serial_inputs: Vec::new(),
        unit_inputs: Vec::new(),
        request_id: id(),
        expected_version: state.version,
        resource_versions: stateset_warehouse::concurrency::reviewed_versions(&state, &command)
            .expect("review"),
        command,
    };
    f.command_as("other-admin-acceptance", opening_count(8))
        .await;
    f.command_as("other-admin-acceptance", opening_count(9))
        .await;
    for _ in 0..2 {
        let (status, response) = f.submit("admin-acceptance", &request).await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(response["command_outcome"]["reason"], "changed_resources");
        assert_eq!(
            response["command_outcome"]["expected_resource_versions"],
            json!(request.resource_versions)
        );
    }
    let before = f.state().await;
    let mut omitted = request.clone();
    omitted
        .resource_versions
        .as_mut()
        .expect("versions")
        .clear();
    assert_eq!(
        f.submit("admin-acceptance", &omitted).await.0,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    let mut future = request.clone();
    for value in future
        .resource_versions
        .as_mut()
        .expect("versions")
        .values_mut()
    {
        *value += 1000;
    }
    let (status, response) = f.submit("admin-acceptance", &future).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert!(response.get("command_outcome").is_none());
    assert_eq!(f.state().await, before);
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL; scripts/local-postgres.sh"]
async fn scoped_order_reviews_fence_exceptions_and_inventory_phantoms() {
    use stateset_warehouse::{concurrency::reviewed_versions, domain::LineInput};
    let f = Fixture::new().await;
    f.seed_catalog().await;
    f.command(opening_count(9)).await;
    let order = f
        .command(Command::CreateOrder {
            reference: "SCOPED-ORDER".into(),
            source_order_id: None,
            customer: "Customer".into(),
            destination: "Destination".into(),
            priority: "normal".into(),
            lines: vec![LineInput {
                id: "line-a".into(),
                sku: "SKU-A".into(),
                quantity: 2,
            }],
        })
        .await;
    let order_id = order["entity_id"].as_str().expect("order").to_string();
    let command = Command::Allocate {
        order_id: order_id.clone(),
    };
    let state = f.state().await;
    let request = Request {
        serial_inputs: Vec::new(),
        unit_inputs: Vec::new(),
        request_id: id(),
        expected_version: state.version,
        resource_versions: reviewed_versions(&state, &command).expect("review"),
        command,
    };
    let exception = f
        .command(Command::ReportException {
            order_id: Some(order_id.clone()),
            category: "Review".into(),
            detail: "Physical review".into(),
        })
        .await;
    f.command(Command::ResolveException {
        exception_id: exception["entity_id"].as_str().expect("exception").into(),
        resolution: "Controller reviewed".into(),
    })
    .await;
    assert_eq!(
        f.submit("admin-acceptance", &request).await.0,
        StatusCode::CONFLICT
    );
    let state = f.state().await;
    let mut request = Request {
        serial_inputs: Vec::new(),
        unit_inputs: Vec::new(),
        request_id: id(),
        expected_version: state.version,
        resource_versions: reviewed_versions(&state, &request.command).expect("review"),
        command: request.command,
    };
    f.command(Command::CountStock {
        sku: "SKU-A".into(),
        lot: "NEW-LOT".into(),
        location: "A-01".into(),
        counted: 2,
        reason: "Previously absent lot counted".into(),
    })
    .await;
    assert_eq!(
        f.submit("admin-acceptance", &request).await.0,
        StatusCode::CONFLICT
    );
    request.request_id = id();
    let state = f.state().await;
    request.expected_version = state.version;
    request.resource_versions = reviewed_versions(&state, &request.command).expect("review");
    assert_eq!(
        f.submit("admin-acceptance", &request).await.0,
        StatusCode::OK
    );
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL; scripts/local-postgres.sh"]
async fn return_authorizations_are_durable_line_lot_bounded_and_receive_once() {
    use stateset_warehouse::returns::ReturnLineInput;
    let f = Fixture::new().await;
    f.seed_catalog().await;
    f.command(opening_count(10)).await;
    let result = f
        .command(Command::CreateOrder {
            reference: "RETURN-SOURCE".into(),
            source_order_id: None,
            customer: "Customer".into(),
            destination: "Destination".into(),
            priority: "normal".into(),
            lines: vec![
                LineInput {
                    id: "original-a".into(),
                    sku: "SKU-A".into(),
                    quantity: 3,
                },
                LineInput {
                    id: "original-b".into(),
                    sku: "SKU-A".into(),
                    quantity: 2,
                },
            ],
        })
        .await;
    let oid = result["entity_id"].as_str().expect("order").to_string();
    f.command(Command::Allocate {
        order_id: oid.clone(),
    })
    .await;
    for (line, q) in [("original-a", 3), ("original-b", 2)] {
        f.command(Command::Pick {
            order_id: oid.clone(),
            line_id: line.into(),
            location: "A-01".into(),
            lot: "LOT-A".into(),
            quantity: q,
        })
        .await;
    }
    f.command(Command::Pack {
        order_id: oid.clone(),
    })
    .await;
    f.command(Command::Ship {
        order_id: oid.clone(),
        carrier: "Carrier".into(),
        handoff_reference: "HANDOFF".into(),
    })
    .await;
    let command = |reference: &str, line: &str, q| Command::CreateReturn {
        reference: reference.into(),
        order_id: oid.clone(),
        reason: "Customer return; inspected before restocking".into(),
        lines: vec![ReturnLineInput {
            id: id(),
            order_line_id: line.into(),
            lot: "LOT-A".into(),
            quantity: q,
        }],
    };
    let original = f.state().await;
    let request = Request {
        serial_inputs: Vec::new(),
        unit_inputs: Vec::new(),
        request_id: id(),
        expected_version: original.version,
        resource_versions: None,
        command: command("RMA-1", "original-a", 2),
    };
    let (status, response) = f.submit("admin-acceptance", &request).await;
    assert_eq!(status, StatusCode::OK, "{response}");
    let rid = response["result"]["entity_id"]
        .as_str()
        .expect("receipt")
        .to_string();
    assert_eq!(
        f.submit("admin-acceptance", &request).await.1["replayed"],
        true
    );
    assert_eq!(
        f.state()
            .await
            .stocks
            .iter()
            .map(|s| s.on_hand)
            .sum::<i64>(),
        5
    );
    f.reject(
        command("RMA-OVER", "original-a", 2),
        StatusCode::UNPROCESSABLE_ENTITY,
    )
    .await;
    f.command(command("RMA-OTHER", "original-b", 2)).await;
    let path = format!("/v1/warehouses/wh-main/orders/{oid}/return-eligibility");
    let (status, allowance) = f.http("GET", &path, "admin-acceptance", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(allowance["lines"][0]["remaining"], 1);
    assert_eq!(allowance["lines"][1]["remaining"], 0);
    assert_eq!(
        f.http("GET", &path, "outsider-acceptance", None).await.0,
        StatusCode::NOT_FOUND
    );
    let mut foreign = request.clone();
    foreign.request_id = id();
    foreign.expected_version = f.state().await.version;
    assert_eq!(
        f.submit("oms-acceptance", &foreign).await.0,
        StatusCode::FORBIDDEN
    );
    let receipt = f
        .store
        .detail(&f.identity.tenant, "wh-main", "receipts", &rid)
        .await
        .expect("receipt");
    let line = receipt["lines"][0]["id"].as_str().expect("line");
    let receive = Request {
        serial_inputs: Vec::new(),
        unit_inputs: Vec::new(),
        request_id: id(),
        expected_version: f.state().await.version,
        resource_versions: None,
        command: Command::Receive {
            receipt_id: rid.clone(),
            line_id: line.into(),
            quantity: 2,
            location: "RECV-01".into(),
        },
    };
    assert_eq!(
        f.submit("admin-acceptance", &receive).await.0,
        StatusCode::OK
    );
    assert_eq!(
        f.submit("admin-acceptance", &receive).await.1["replayed"],
        true
    );
    let received = f.state().await;
    assert_eq!(received.stocks.iter().map(|s| s.on_hand).sum::<i64>(), 7);
    assert_eq!(
        received
            .stocks
            .iter()
            .find(|s| s.location == "RECV-01")
            .expect("receiving")
            .on_hand,
        2
    );
    assert_eq!(
        f.store
            .reconciliation(&f.identity.tenant, "wh-main")
            .await
            .expect("proof")["ok"],
        true
    );
    // Source proof survives a malformed cached JSON projection.
    sqlx::query("UPDATE wms_receipts SET data=jsonb_set(data,'{return_source}','false'::jsonb) WHERE tenant=$1 AND warehouse_id='wh-main' AND id=$2")
        .bind(&f.identity.tenant).bind(&rid).execute(&f.store.pool).await.expect("corrupt cache");
    let detail = f
        .store
        .detail(&f.identity.tenant, "wh-main", "receipts", &rid)
        .await
        .expect("canonical return");
    assert_eq!(detail["return_source"]["order_id"], oid);
    assert_eq!(detail["return_source"]["line_ids"][line], "original-a");
    assert!(f.state().await.validate().is_ok());
    let deleted = sqlx::query("DELETE FROM wms_return_lines WHERE tenant=$1 AND receipt_id=$2")
        .bind(&f.identity.tenant)
        .bind(&rid)
        .execute(&f.store.pool)
        .await;
    assert!(
        deleted.is_err(),
        "return allowances cannot be silently freed"
    );
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL; scripts/local-postgres.sh"]
async fn scoped_reviews_fence_new_metadata_and_database_revision_resets() {
    let f = Fixture::new().await;
    f.seed_catalog().await;
    let state = f.state().await;
    let command = Command::CountStock {
        sku: "SKU-A".into(),
        lot: "LOT-A".into(),
        location: "LATER-BIN".into(),
        counted: 7,
        reason: "Physical count".into(),
    };
    let request = Request {
        serial_inputs: Vec::new(),
        unit_inputs: Vec::new(),
        request_id: id(),
        expected_version: state.version,
        resource_versions: stateset_warehouse::concurrency::reviewed_versions(&state, &command)
            .expect("review"),
        command,
    };
    assert_eq!(
        f.submit("admin-acceptance", &request).await.0,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    f.command(Command::CreateLocation {
        code: "LATER-BIN".into(),
        zone: "Storage".into(),
        kind: "storage".into(),
    })
    .await;
    let (status, response) = f.submit("admin-acceptance", &request).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(response["command_outcome"]["reason"], "changed_resources");
    assert!(
        sqlx::query("DELETE FROM wms_resource_versions WHERE tenant=$1")
            .bind(&f.identity.tenant)
            .execute(&f.store.pool)
            .await
            .is_err()
    );
    assert!(
        sqlx::query("UPDATE wms_resource_versions SET revision=revision-1 WHERE tenant=$1")
            .bind(&f.identity.tenant)
            .execute(&f.store.pool)
            .await
            .is_err()
    );
    assert!(sqlx::query("TRUNCATE wms_resource_versions")
        .execute(&f.store.pool)
        .await
        .is_err());
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL; scripts/local-postgres.sh"]
async fn repeatable_read_sql_returns_cannot_oversubscribe_the_same_shipped_line() {
    let f = Fixture::new().await;
    f.seed_catalog().await;
    f.command(opening_count(1)).await;
    let order = f
        .command(Command::CreateOrder {
            reference: "ONE-SHIPPED".into(),
            source_order_id: None,
            customer: "Customer".into(),
            destination: "Destination".into(),
            priority: "normal".into(),
            lines: vec![LineInput {
                id: "one-line".into(),
                sku: "SKU-A".into(),
                quantity: 1,
            }],
        })
        .await;
    let oid = order["entity_id"].as_str().expect("order").to_string();
    f.command(Command::Allocate {
        order_id: oid.clone(),
    })
    .await;
    f.command(Command::Pick {
        order_id: oid.clone(),
        line_id: "one-line".into(),
        location: "A-01".into(),
        lot: "LOT-A".into(),
        quantity: 1,
    })
    .await;
    f.command(Command::Pack {
        order_id: oid.clone(),
    })
    .await;
    f.command(Command::Ship {
        order_id: oid.clone(),
        carrier: "Carrier".into(),
        handoff_reference: "HANDOFF".into(),
    })
    .await;
    let mut receipts = vec![];
    for n in 0..2 {
        let receipt = f
            .command(Command::CreateReceipt {
                reference: format!("CANDIDATE-{n}"),
                supplier: "Customer return".into(),
                lines: vec![ReceiptInput {
                    id: "return-line".into(),
                    sku: "SKU-A".into(),
                    lot: "LOT-A".into(),
                    quantity: 1,
                }],
            })
            .await;
        receipts.push(receipt["entity_id"].as_str().expect("receipt").to_string());
    }
    let barrier = Arc::new(tokio::sync::Barrier::new(2));
    let attempt = |receipt: String| {
        let store = f.store.clone();
        let tenant = f.identity.tenant.clone();
        let order = oid.clone();
        let barrier = barrier.clone();
        async move {
            let mut tx = store.pool.begin().await.expect("transaction");
            sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ")
                .execute(&mut *tx)
                .await
                .expect("isolation");
            let _: i64 = sqlx::query_scalar("SELECT count(*) FROM wms_return_lines")
                .fetch_one(&mut *tx)
                .await
                .expect("establish snapshot");
            sqlx::query("INSERT INTO wms_return_lines VALUES($1,'wh-main',$2,'return-line',$3,'one-line','SKU-A','LOT-A',1,'Physical return')")
                .bind(tenant).bind(receipt).bind(order).execute(&mut *tx).await.expect("deferred insert");
            barrier.wait().await;
            tx.commit().await
        }
    };
    let (a, b) = tokio::join!(attempt(receipts[0].clone()), attempt(receipts[1].clone()));
    assert_eq!(
        usize::from(a.is_ok()) + usize::from(b.is_ok()),
        1,
        "{a:?} / {b:?}"
    );
    let allowance = stateset_warehouse::returns::eligible(&f.store, &f.identity, "wh-main", &oid)
        .await
        .expect("allowance");
    assert_eq!(allowance["lines"][0]["authorized"], 1);
    assert_eq!(allowance["lines"][0]["remaining"], 0);
    assert_eq!(
        f.store
            .reconciliation(&f.identity.tenant, "wh-main")
            .await
            .expect("proof")["ok"],
        true
    );
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL; scripts/local-postgres.sh"]
async fn immutable_committed_receipts_replay_without_waiting_for_physical_writers() {
    let f = Fixture::new().await;
    f.seed_catalog().await;
    let request = Request {
        serial_inputs: Vec::new(),
        unit_inputs: Vec::new(),
        request_id: id(),
        expected_version: f.state().await.version,
        resource_versions: None,
        command: opening_count(7),
    };
    let (status, original) = f.submit("admin-acceptance", &request).await;
    assert_eq!(status, StatusCode::OK);
    let mut lock = f.store.pool.begin().await.expect("lock session");
    sqlx::query(
        "SELECT version FROM wms_warehouses WHERE tenant=$1 AND warehouse_id='wh-main' FOR UPDATE",
    )
    .bind(&f.identity.tenant)
    .fetch_one(&mut *lock)
    .await
    .expect("hold writer lock");
    let replay = tokio::time::timeout(
        std::time::Duration::from_secs(1),
        f.submit("admin-acceptance", &request),
    )
    .await
    .expect("committed recovery must not wait behind a physical writer");
    assert_eq!(replay.0, StatusCode::OK);
    assert_eq!(replay.1["replayed"], true);
    assert_eq!(replay.1["result"], original["result"]);
    assert_eq!(
        f.submit("other-admin-acceptance", &request).await.0,
        StatusCode::CONFLICT
    );
    lock.rollback().await.expect("release writer");
    for query in [
        "UPDATE wms_commands SET result='{}'::jsonb WHERE tenant=$1",
        "DELETE FROM wms_commands WHERE tenant=$1",
    ] {
        assert!(
            sqlx::query(query)
                .bind(&f.identity.tenant)
                .execute(&f.store.pool)
                .await
                .is_err(),
            "original request evidence is immutable"
        );
    }
    assert!(sqlx::query("TRUNCATE wms_commands")
        .execute(&f.store.pool)
        .await
        .is_err());
}

async fn assert_scoped_matches_full_model(f: &Fixture, command: Command) {
    use stateset_warehouse::concurrency::reviewed_versions;
    let before = f.state().await;
    let expected = before.execute(&command, &f.identity.actor);
    let request = Request {
        serial_inputs: Vec::new(),
        unit_inputs: Vec::new(),
        request_id: id(),
        expected_version: before.version,
        resource_versions: reviewed_versions(&before, &command).expect("review"),
        command,
    };
    assert!(request.resource_versions.is_some());
    let (status, response) = f.submit("admin-acceptance", &request).await;
    let after = f.state().await;
    let Ok((mut expected, result)) = expected else {
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{response}");
        assert_eq!(after, before);
        return;
    };
    assert_eq!(status, StatusCode::OK, "{response}");
    assert_eq!(response["result"], result);
    let mut actual = after.clone();
    expected
        .stocks
        .sort_by(|a, b| (&a.sku, &a.lot, &a.location).cmp(&(&b.sku, &b.lot, &b.location)));
    actual
        .stocks
        .sort_by(|a, b| (&a.sku, &a.lot, &a.location).cmp(&(&b.sku, &b.lot, &b.location)));
    for state in [&mut expected, &mut actual] {
        state.orders.sort_by(|a, b| a.id.cmp(&b.id));
        state.receipts.sort_by(|a, b| a.id.cmp(&b.id));
        for order in &mut state.orders {
            if order.shipped_at.is_some() {
                order.shipped_at = Some("Recorded handoff".into());
            }
        }
    }
    assert_eq!(
        actual.stocks, expected.stocks,
        "selective stock delta diverged from full reducer"
    );
    assert_eq!(
        actual.orders, expected.orders,
        "selective load changed another owner's work"
    );
    assert_eq!(actual.receipts, expected.receipts);
    assert_eq!(actual.products, expected.products);
    assert_eq!(actual.locations, expected.locations);
    assert_eq!(actual.version, expected.version);
    assert_eq!(
        f.submit("admin-acceptance", &request).await.1["replayed"],
        true
    );
    assert_eq!(f.state().await, after, "replay altered evidence");
    assert_eq!(
        f.store
            .reconciliation(&f.identity.tenant, "wh-main")
            .await
            .expect("proof")["ok"],
        true
    );
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL; scripts/local-postgres.sh"]
async fn selective_physical_commands_match_full_reducer_with_foreign_source_and_staging_owners() {
    let f = Fixture::new().await;
    f.seed_catalog().await;
    f.command(Command::CreateLocation {
        code: "B-01".into(),
        zone: "Storage".into(),
        kind: "storage".into(),
    })
    .await;
    f.command(Command::CreateProduct {
        serial_controlled: false,
        sku: "UNRELATED".into(),
        name: "Unrelated product".into(),
        category: "Test".into(),
        reorder_point: 0,
    })
    .await;
    f.command(Command::CountStock {
        sku: "UNRELATED".into(),
        lot: "LOT-A".into(),
        location: "A-01".into(),
        counted: 9,
        reason: "Unrelated fixture".into(),
    })
    .await;
    f.command(opening_count(20)).await;
    let create = |reference: &str, quantities: &[i64]| Command::CreateOrder {
        reference: reference.into(),
        source_order_id: None,
        customer: "Customer".into(),
        destination: "Destination".into(),
        priority: "normal".into(),
        lines: quantities
            .iter()
            .enumerate()
            .map(|(n, q)| LineInput {
                id: format!("line-{n}"),
                sku: "SKU-A".into(),
                quantity: *q,
            })
            .collect(),
    };
    let other = f.command(create("FOREIGN-OWNER", &[3])).await["entity_id"]
        .as_str()
        .expect("order")
        .to_string();
    f.command(Command::Allocate {
        order_id: other.clone(),
    })
    .await;
    f.command(Command::Pick {
        order_id: other,
        line_id: "line-0".into(),
        location: "A-01".into(),
        lot: "LOT-A".into(),
        quantity: 1,
    })
    .await;
    let receipt = f
        .command(Command::CreateReceipt {
            reference: "SCOPED-ASN".into(),
            supplier: "Supplier".into(),
            lines: vec![ReceiptInput {
                id: "receipt-line".into(),
                sku: "SKU-A".into(),
                lot: "LOT-A".into(),
                quantity: 1,
            }],
        })
        .await["entity_id"]
        .as_str()
        .expect("receipt")
        .to_string();
    assert_scoped_matches_full_model(
        &f,
        Command::Receive {
            receipt_id: receipt,
            line_id: "receipt-line".into(),
            quantity: 1,
            location: "RECV-01".into(),
        },
    )
    .await;
    assert_scoped_matches_full_model(
        &f,
        Command::MoveStock {
            sku: "SKU-A".into(),
            lot: "LOT-A".into(),
            from: "RECV-01".into(),
            to: "B-01".into(),
            quantity: 1,
        },
    )
    .await;
    assert_scoped_matches_full_model(&f, opening_count(1)).await; // Cannot consume the other order's source hold.
    assert_scoped_matches_full_model(
        &f,
        Command::CountStock {
            sku: "SKU-A".into(),
            lot: "LOT-A".into(),
            location: "B-01".into(),
            counted: 1,
            reason: "Witnessed no-op count".into(),
        },
    )
    .await;
    let first = f.command(create("SCOPED-CANCEL", &[4, 2])).await["entity_id"]
        .as_str()
        .expect("order")
        .to_string();
    assert_scoped_matches_full_model(
        &f,
        Command::Allocate {
            order_id: first.clone(),
        },
    )
    .await;
    assert_scoped_matches_full_model(
        &f,
        Command::Pick {
            order_id: first.clone(),
            line_id: "line-0".into(),
            location: "A-01".into(),
            lot: "LOT-A".into(),
            quantity: 1,
        },
    )
    .await;
    assert_scoped_matches_full_model(
        &f,
        Command::Unpick {
            order_id: first.clone(),
            line_id: "line-0".into(),
            location: "A-01".into(),
            lot: "LOT-A".into(),
            quantity: 1,
            reason: "Physical return".into(),
        },
    )
    .await;
    assert_scoped_matches_full_model(
        &f,
        Command::ResetAllocation {
            order_id: first.clone(),
            reason: "Recount reviewed".into(),
        },
    )
    .await;
    assert_scoped_matches_full_model(
        &f,
        Command::Allocate {
            order_id: first.clone(),
        },
    )
    .await;
    assert_scoped_matches_full_model(
        &f,
        Command::CancelOrder {
            order_id: first,
            reason: "Customer cancellation before picking".into(),
        },
    )
    .await;
    let second = f.command(create("SCOPED-SHIP", &[4, 2])).await["entity_id"]
        .as_str()
        .expect("order")
        .to_string();
    assert_scoped_matches_full_model(
        &f,
        Command::Allocate {
            order_id: second.clone(),
        },
    )
    .await;
    for (line, q) in [("line-0", 4), ("line-1", 2)] {
        assert_scoped_matches_full_model(
            &f,
            Command::Pick {
                order_id: second.clone(),
                line_id: line.into(),
                location: "A-01".into(),
                lot: "LOT-A".into(),
                quantity: q,
            },
        )
        .await;
    }
    assert_scoped_matches_full_model(
        &f,
        Command::Pack {
            order_id: second.clone(),
        },
    )
    .await;
    assert_scoped_matches_full_model(
        &f,
        Command::Ship {
            order_id: second,
            carrier: "Carrier".into(),
            handoff_reference: "SCOPED-HANDOFF".into(),
        },
    )
    .await;
}

fn facility_request() -> Request {
    Request {
        serial_inputs: Vec::new(),
        unit_inputs: Vec::new(),
        request_id: id(),
        expected_version: 0,
        resource_versions: None,
        command: Command::CreateWarehouse {
            code: "EAST".into(),
            name: "East fulfillment".into(),
            city: "Toronto".into(),
            timezone: "America/Toronto".into(),
        },
    }
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL; scripts/local-postgres.sh"]
async fn warehouse_creation_is_atomic_actor_bound_replayable_and_tenant_scoped() {
    let f = Fixture::new().await;
    let before = f.state().await;
    let request = facility_request();
    let path = "/v1/warehouses/wh-east/commands";
    let (a, b) = tokio::join!(
        f.http("POST", path, "admin-acceptance", Some(json!(request))),
        f.http("POST", path, "admin-acceptance", Some(json!(request)))
    );
    assert_eq!(a.0, StatusCode::OK, "{}", a.1);
    assert_eq!(b.0, StatusCode::OK, "{}", b.1);
    assert_eq!(a.1["result"], b.1["result"]);
    assert_ne!(a.1["replayed"], b.1["replayed"]);
    assert_eq!(a.1["result"]["type"], "warehouse.created");
    let created = f
        .store
        .state(&f.identity.tenant, "wh-east")
        .await
        .expect("created warehouse");
    assert_eq!(created.version, 1);
    assert_eq!(created.warehouse.code, "EAST");
    assert_eq!(created.warehouse.timezone, "America/Toronto");
    assert_eq!(created.locations.len(), 2);
    assert!(created.stocks.is_empty() && created.products.is_empty());
    assert_eq!(created.activity.len(), 1);
    assert_eq!(
        f.state().await,
        before,
        "creation cannot change another warehouse"
    );
    for table in ["wms_commands", "wms_events", "wms_outbox"] {
        let count: i64 =
            sqlx::query_scalar(&format!("SELECT count(*) FROM {table} WHERE tenant=$1"))
                .bind(&f.identity.tenant)
                .fetch_one(&f.store.pool)
                .await
                .expect("count");
        assert_eq!(count, 1, "{table}");
    }
    assert_eq!(
        f.http("POST", path, "other-admin-acceptance", Some(json!(request)))
            .await
            .0,
        StatusCode::CONFLICT
    );
    let mut changed = request.clone();
    changed.command = Command::CreateWarehouse {
        code: "CHANGED".into(),
        name: "Other".into(),
        city: "Other".into(),
        timezone: "UTC".into(),
    };
    assert_eq!(
        f.http("POST", path, "admin-acceptance", Some(json!(changed)))
            .await
            .0,
        StatusCode::CONFLICT
    );
    for token in ["operator-acceptance", "viewer-acceptance", "oms-acceptance"] {
        assert_eq!(
            f.http("POST", path, token, Some(json!(request))).await.0,
            StatusCode::FORBIDDEN
        );
    }
    let update = Request {
        serial_inputs: Vec::new(),
        unit_inputs: Vec::new(),
        request_id: id(),
        expected_version: 1,
        resource_versions: None,
        command: Command::UpdateWarehouse {
            code: "EAST-2".into(),
            name: "East renamed".into(),
            city: "Ottawa".into(),
            timezone: "America/Toronto".into(),
        },
    };
    assert_eq!(
        f.http("POST", path, "admin-acceptance", Some(json!(update)))
            .await
            .0,
        StatusCode::OK
    );
    let replay = f
        .http("POST", path, "admin-acceptance", Some(json!(request)))
        .await;
    assert_eq!(
        replay.1["result"], a.1["result"],
        "creation retry returns its original version and facts"
    );
    assert_eq!(
        f.store
            .state(&f.identity.tenant, "wh-east")
            .await
            .expect("state")
            .warehouse
            .code,
        "EAST-2"
    );
    assert_eq!(
        f.http("GET", "/v1/warehouses/wh-east", "outsider-acceptance", None)
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        f.http("POST", path, "outsider-acceptance", Some(json!(request)))
            .await
            .0,
        StatusCode::OK,
        "same ID may be owned in another tenant"
    );
    assert_eq!(
        f.store
            .reconciliation(&f.identity.tenant, "wh-east")
            .await
            .expect("proof")["ok"],
        true
    );
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL; scripts/local-postgres.sh"]
async fn competing_facility_creations_and_invalid_admission_leave_one_or_no_facility() {
    let f = Fixture::new().await;
    let path = "/v1/warehouses/wh-race/commands";
    let first = facility_request();
    let second = facility_request();
    let (a, b) = tokio::join!(
        f.http("POST", path, "admin-acceptance", Some(json!(first))),
        f.http("POST", path, "other-admin-acceptance", Some(json!(second)))
    );
    assert_eq!(
        [a.0, b.0].iter().filter(|s| **s == StatusCode::OK).count(),
        1
    );
    assert_eq!(
        [a.0, b.0]
            .iter()
            .filter(|s| **s == StatusCode::CONFLICT)
            .count(),
        1
    );
    let existing = f.state().await;
    assert_eq!(
        f.submit("admin-acceptance", &facility_request()).await.0,
        StatusCode::CONFLICT,
        "even the preprovisioned version-zero warehouse cannot be recreated"
    );
    assert_eq!(f.state().await, existing);
    for (index, mut invalid) in [
        facility_request(),
        facility_request(),
        facility_request(),
        facility_request(),
    ]
    .into_iter()
    .enumerate()
    {
        match index {
            0 => {
                invalid.command = Command::CreateWarehouse {
                    code: "BAD".into(),
                    name: "Bad timezone".into(),
                    city: "Test".into(),
                    timezone: "Mars/Invalid".into(),
                }
            }
            1 => invalid.expected_version = 1,
            2 => invalid.resource_versions = Some(Default::default()),
            _ => invalid.request_id = "bad".into(),
        }
        let path = format!("/v1/warehouses/wh-invalid-{index}/commands");
        assert_eq!(
            f.http("POST", &path, "admin-acceptance", Some(json!(invalid)))
                .await
                .0,
            StatusCode::UNPROCESSABLE_ENTITY
        );
    }
    let bad_id = facility_request();
    assert_eq!(
        f.http(
            "POST",
            "/v1/warehouses/invalid%20id/commands",
            "admin-acceptance",
            Some(json!(bad_id))
        )
        .await
        .0,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    let rows: i64 = sqlx::query_scalar("SELECT count(*) FROM wms_warehouses WHERE tenant=$1")
        .bind(&f.identity.tenant)
        .fetch_one(&f.store.pool)
        .await
        .expect("warehouses");
    assert_eq!(rows, 2, "only default and one race winner exist");
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL; scripts/local-postgres.sh"]
async fn final_delivery_failure_rolls_back_facility_header_bins_and_creation_evidence() {
    let f = Fixture::new().await;
    let trigger = format!("facility_fault_{}", id().replace('-', ""));
    let sql=format!("CREATE FUNCTION {trigger}() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.tenant='{}' THEN RAISE EXCEPTION 'injected facility delivery failure'; END IF; RETURN NEW; END $$; CREATE TRIGGER {trigger} BEFORE INSERT ON wms_outbox FOR EACH ROW EXECUTE FUNCTION {trigger}()",f.identity.tenant);
    sqlx::raw_sql(&sql)
        .execute(&f.store.pool)
        .await
        .expect("fault trigger");
    let request = facility_request();
    let path = "/v1/warehouses/wh-rollback/commands";
    assert_eq!(
        f.http("POST", path, "admin-acceptance", Some(json!(request)))
            .await
            .0,
        StatusCode::SERVICE_UNAVAILABLE
    );
    for table in [
        "wms_warehouses",
        "wms_locations",
        "wms_events",
        "wms_commands",
        "wms_activity",
    ] {
        let count: i64 = sqlx::query_scalar(&format!(
            "SELECT count(*) FROM {table} WHERE tenant=$1 AND warehouse_id='wh-rollback'"
        ))
        .bind(&f.identity.tenant)
        .fetch_one(&f.store.pool)
        .await
        .expect("rollback count");
        assert_eq!(count, 0, "{table}");
    }
    sqlx::raw_sql(&format!(
        "DROP TRIGGER {trigger} ON wms_outbox; DROP FUNCTION {trigger}()"
    ))
    .execute(&f.store.pool)
    .await
    .expect("remove owned fault");
    let retry = f
        .http("POST", path, "admin-acceptance", Some(json!(request)))
        .await;
    assert_eq!(retry.0, StatusCode::OK, "{}", retry.1);
    assert_eq!(retry.1["replayed"], false);
    assert_eq!(
        f.http("POST", path, "admin-acceptance", Some(json!(request)))
            .await
            .1["replayed"],
        true
    );
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL; scripts/local-postgres.sh"]
async fn warehouse_directory_pages_all_facilities_without_cross_tenant_or_cursor_overlap() {
    let f = Fixture::new().await;
    for index in 0..103 {
        let path = format!("/v1/warehouses/facility-{index:03}/commands");
        assert_eq!(
            f.http(
                "POST",
                &path,
                "admin-acceptance",
                Some(json!(facility_request()))
            )
            .await
            .0,
            StatusCode::OK
        );
    }
    let first = f
        .http("GET", "/v1/warehouses", "admin-acceptance", None)
        .await;
    assert_eq!(first.1["warehouses"].as_array().expect("page").len(), 100);
    assert!(first.1["next_after"].is_string());
    let mut ids = std::collections::BTreeSet::new();
    let mut after = String::new();
    loop {
        let path = format!("/v1/warehouses?limit=17&after={after}");
        let page = f.http("GET", &path, "viewer-acceptance", None).await;
        assert_eq!(page.0, StatusCode::OK);
        for row in page.1["warehouses"].as_array().expect("warehouses") {
            assert!(ids.insert(row["warehouse"]["id"].as_str().expect("id").to_owned()));
        }
        match page.1["next_after"].as_str() {
            Some(cursor) => after = cursor.into(),
            None => break,
        }
    }
    assert_eq!(ids.len(), 104);
    assert!(ids.contains("wh-main"));
    assert!(f
        .http("GET", "/v1/warehouses", "outsider-acceptance", None)
        .await
        .1["warehouses"]
        .as_array()
        .expect("foreign page")
        .is_empty());
    for query in [
        "limit=0",
        "limit=101",
        "after=%0A",
        "tenant=other",
        "limit=1&limit=2",
        "unexpected=1",
    ] {
        assert_eq!(
            f.http(
                "GET",
                &format!("/v1/warehouses?{query}"),
                "admin-acceptance",
                None
            )
            .await
            .0,
            StatusCode::UNPROCESSABLE_ENTITY,
            "{query}"
        );
    }
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM wms_commands WHERE tenant=$1")
        .bind(&f.identity.tenant)
        .fetch_one(&f.store.pool)
        .await
        .expect("creation count");
    assert_eq!(
        count, 103,
        "directory reads must not create execution evidence"
    );
}

async fn packed_two_line_source(f: &Fixture) -> String {
    f.seed_catalog().await;
    f.command(opening_count(10)).await;
    let result = f
        .command(Command::CreateOrder {
            reference: "CANCELLATION-SOURCE".into(),
            source_order_id: None,
            customer: "Customer".into(),
            destination: "Destination".into(),
            priority: "normal".into(),
            lines: vec![
                LineInput {
                    id: "a".into(),
                    sku: "SKU-A".into(),
                    quantity: 3,
                },
                LineInput {
                    id: "b".into(),
                    sku: "SKU-A".into(),
                    quantity: 2,
                },
            ],
        })
        .await;
    let oid = result["entity_id"].as_str().expect("order").to_string();
    f.command(Command::Allocate {
        order_id: oid.clone(),
    })
    .await;
    for (line, q) in [("a", 3), ("b", 2)] {
        f.command(Command::Pick {
            order_id: oid.clone(),
            line_id: line.into(),
            location: "A-01".into(),
            lot: "LOT-A".into(),
            quantity: q,
        })
        .await;
    }
    f.command(Command::Pack {
        order_id: oid.clone(),
    })
    .await;
    oid
}
async fn shipped_two_line_source(f: &Fixture) -> String {
    let oid = packed_two_line_source(f).await;
    f.command(Command::Ship {
        order_id: oid.clone(),
        carrier: "Carrier".into(),
        handoff_reference: "HANDOFF-CANCEL".into(),
    })
    .await;
    oid
}
fn cancellation_return(order: &str, reference: &str, line: &str, q: i64) -> Command {
    Command::CreateReturn {
        reference: reference.into(),
        order_id: order.into(),
        reason: "Physical customer return".into(),
        lines: vec![stateset_warehouse::returns::ReturnLineInput {
            id: "r".into(),
            order_line_id: line.into(),
            lot: "LOT-A".into(),
            quantity: q,
        }],
    }
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn cancellation_of_partial_return_preserves_physical_stock_and_releases_only_exact_unreceived_allowance(
) {
    let f = Fixture::new().await;
    let oid = shipped_two_line_source(&f).await;
    let r = f
        .command(cancellation_return(&oid, "RMA-CANCEL", "a", 3))
        .await;
    let rid = r["entity_id"].as_str().expect("receipt").to_string();
    f.command(Command::Receive {
        receipt_id: rid.clone(),
        line_id: "r".into(),
        quantity: 1,
        location: "RECV-01".into(),
    })
    .await;
    let before = f.state().await;
    let request = Request {
        serial_inputs: Vec::new(),
        unit_inputs: Vec::new(),
        request_id: id(),
        expected_version: before.version,
        resource_versions: None,
        command: Command::CancelReceipt {
            receipt_id: rid.clone(),
            reason: "Customer retained the remaining two".into(),
        },
    };
    for role in ["viewer-acceptance", "oms-acceptance"] {
        assert_eq!(f.submit(role, &request).await.0, StatusCode::FORBIDDEN);
    }
    assert_eq!(
        f.submit("outsider-acceptance", &request).await.0,
        StatusCode::NOT_FOUND
    );
    let (status, original) = f.submit("operator-acceptance", &request).await;
    assert_eq!(status, StatusCode::OK, "{original}");
    let event: Value = sqlx::query_scalar("SELECT payload FROM wms_events WHERE tenant=$1 AND warehouse_id='wh-main' AND payload->>'request_id'=$2").bind(&f.identity.tenant).bind(&request.request_id).fetch_one(&f.store.pool).await.expect("immutable event");
    assert_eq!(event["type"], "receipt.cancelled");
    assert_eq!(
        event["receipt"]["cancellation"]["lines"]["r"]["cancelled"],
        2
    );
    assert_eq!(event["receipt"]["lines"][0]["received"], 1);
    assert_eq!(event["inventory_changes"], json!([]));
    let after = f.state().await;
    assert_eq!(after.stocks, before.stocks);
    assert_eq!(after.movements, before.movements);
    assert_eq!(after.orders, before.orders);
    assert_eq!(
        after
            .receipts
            .iter()
            .find(|r| r.id == rid)
            .expect("receipt")
            .cancellation
            .as_ref()
            .expect("closure")
            .lines["r"]
            .cancelled,
        2
    );
    let (status, replay) = f.submit("operator-acceptance", &request).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(replay["replayed"], true);
    assert_eq!(replay["result"], original["result"]);
    assert_eq!(f.state().await, after);
    f.reject(
        Command::Receive {
            receipt_id: rid.clone(),
            line_id: "r".into(),
            quantity: 1,
            location: "RECV-01".into(),
        },
        StatusCode::UNPROCESSABLE_ENTITY,
    )
    .await;
    let available = stateset_warehouse::returns::eligible(&f.store, &f.identity, "wh-main", &oid)
        .await
        .expect("allowance");
    assert_eq!(
        available["lines"]
            .as_array()
            .expect("lines")
            .iter()
            .find(|l| l["order_line_id"] == "a")
            .expect("a")["remaining"],
        2
    );
    f.reject(
        cancellation_return(&oid, "OVER", "a", 3),
        StatusCode::UNPROCESSABLE_ENTITY,
    )
    .await;
    f.command(cancellation_return(&oid, "REPLACEMENT", "a", 2))
        .await;
    f.command(cancellation_return(&oid, "OTHER-LINE", "b", 2))
        .await;
    // Frozen normalized records reconstruct despite malformed cached JSON.
    sqlx::query("UPDATE wms_receipts SET data=jsonb_set(data,'{cancellation}','false'::jsonb) WHERE tenant=$1 AND warehouse_id='wh-main' AND id=$2").bind(&f.identity.tenant).bind(&rid).execute(&f.store.pool).await.expect("cache corruption");
    let path = format!("/v1/warehouses/wh-main/receipts/{rid}");
    let (status, detail) = f.http("GET", &path, "admin-acceptance", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(detail["cancellation"]["lines"]["r"]["cancelled"], 2);
    assert_eq!(detail["lines"][0]["received"], 1);
    for statement in ["UPDATE wms_receipts SET status='partial',data=jsonb_set(data,'{status}','\"partial\"'::jsonb) WHERE tenant=$1 AND id=$2", "UPDATE wms_receipt_lines SET quantity=4 WHERE tenant=$1 AND receipt_id=$2", "DELETE FROM wms_receipt_cancelled_lines WHERE tenant=$1 AND receipt_id=$2", "DELETE FROM wms_receipt_cancellations WHERE tenant=$1 AND receipt_id=$2"] {
        let result=sqlx::query(statement).bind(&f.identity.tenant).bind(&rid).execute(&f.store.pool).await;assert!(result.is_err(),"must reject {statement}");
    }
    let proof = f
        .store
        .reconciliation(&f.identity.tenant, "wh-main")
        .await
        .expect("full proof");
    assert_eq!(proof["ok"], true);
    assert_eq!(
        proof["returns"]
            .as_array()
            .expect("returns")
            .iter()
            .find(|l| l["order_line_id"] == "a")
            .expect("proof")["authorized"],
        3
    );
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn cancellation_and_concurrent_receive_require_a_new_review_without_hiding_received_units() {
    let f = Fixture::new().await;
    f.seed_catalog().await;
    let r = f
        .command(Command::CreateReceipt {
            reference: "RACE-CANCEL".into(),
            supplier: "Supplier".into(),
            lines: vec![ReceiptInput {
                id: "line".into(),
                sku: "SKU-A".into(),
                lot: "LOT-A".into(),
                quantity: 10,
            }],
        })
        .await;
    let rid = r["entity_id"].as_str().expect("receipt").to_string();
    let state = f.state().await;
    let receive = Command::Receive {
        receipt_id: rid.clone(),
        line_id: "line".into(),
        quantity: 1,
        location: "RECV-01".into(),
    };
    let receive = Request {
        serial_inputs: Vec::new(),
        unit_inputs: Vec::new(),
        request_id: id(),
        expected_version: state.version,
        resource_versions: stateset_warehouse::concurrency::reviewed_versions(&state, &receive)
            .expect("review"),
        command: receive,
    };
    let cancel = Request {
        serial_inputs: Vec::new(),
        unit_inputs: Vec::new(),
        request_id: id(),
        expected_version: state.version,
        resource_versions: None,
        command: Command::CancelReceipt {
            receipt_id: rid.clone(),
            reason: "Supplier stopped delivery".into(),
        },
    };
    let (a, b) = tokio::join!(
        f.submit("operator-acceptance", &receive),
        f.submit("operator-acceptance", &cancel)
    );
    assert_eq!(
        usize::from(a.0 == StatusCode::OK) + usize::from(b.0 == StatusCode::OK),
        1,
        "{a:?}/{b:?}"
    );
    let loser = if a.0 == StatusCode::OK { &b } else { &a };
    assert_eq!(loser.0, StatusCode::CONFLICT);
    assert_eq!(loser.1["command_outcome"]["status"], "not_committed");
    if a.0 == StatusCode::OK {
        f.command(Command::CancelReceipt {
            receipt_id: rid.clone(),
            reason: "Refreshed after the final received unit".into(),
        })
        .await;
    }
    let final_state = f.state().await;
    let receipt = final_state
        .receipts
        .iter()
        .find(|r| r.id == rid)
        .expect("receipt");
    let frozen = &receipt.cancellation.as_ref().expect("closure").lines["line"];
    assert_eq!(frozen.received, if a.0 == StatusCode::OK { 1 } else { 0 });
    assert_eq!(frozen.received + frozen.cancelled, 10);
    assert_eq!(
        f.store
            .reconciliation(&f.identity.tenant, "wh-main")
            .await
            .expect("proof")["ok"],
        true
    );
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn cancellation_final_delivery_failure_rolls_back_frozen_records_and_exact_retry_succeeds() {
    let f = Fixture::new().await;
    f.seed_catalog().await;
    let r = f
        .command(Command::CreateReceipt {
            reference: "ROLLBACK-CANCEL".into(),
            supplier: "Supplier".into(),
            lines: vec![ReceiptInput {
                id: "line".into(),
                sku: "SKU-A".into(),
                lot: "LOT-A".into(),
                quantity: 2,
            }],
        })
        .await;
    let rid = r["entity_id"].as_str().expect("receipt").to_string();
    let before = f.state().await;
    let request = Request {
        serial_inputs: Vec::new(),
        unit_inputs: Vec::new(),
        request_id: id(),
        expected_version: before.version,
        resource_versions: None,
        command: Command::CancelReceipt {
            receipt_id: rid.clone(),
            reason: "Delivery withdrawn".into(),
        },
    };
    let trigger = format!("cancel_fault_{}", id().replace('-', ""));
    let sql=format!("CREATE FUNCTION {trigger}() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.tenant='{}' THEN RAISE EXCEPTION 'injected cancellation delivery failure'; END IF; RETURN NEW; END $$; CREATE TRIGGER {trigger} BEFORE INSERT ON wms_outbox FOR EACH ROW EXECUTE FUNCTION {trigger}()",f.identity.tenant);
    sqlx::raw_sql(&sql)
        .execute(&f.store.pool)
        .await
        .expect("inject final failure");
    let failed = f.submit("admin-acceptance", &request).await;
    sqlx::raw_sql(&format!(
        "DROP TRIGGER {trigger} ON wms_outbox; DROP FUNCTION {trigger}()"
    ))
    .execute(&f.store.pool)
    .await
    .expect("remove own fault");
    assert_eq!(failed.0, StatusCode::SERVICE_UNAVAILABLE, "{failed:?}");
    assert_eq!(f.state().await, before);
    for table in ["wms_receipt_cancellations", "wms_receipt_cancelled_lines"] {
        let n: i64 = sqlx::query_scalar(&format!(
            "SELECT count(*) FROM {table} WHERE tenant=$1 AND receipt_id=$2"
        ))
        .bind(&f.identity.tenant)
        .bind(&rid)
        .fetch_one(&f.store.pool)
        .await
        .expect("count");
        assert_eq!(n, 0);
    }
    let accepted = f.submit("admin-acceptance", &request).await;
    assert_eq!(accepted.0, StatusCode::OK, "{accepted:?}");
    assert_eq!(
        f.submit("admin-acceptance", &request).await.1["replayed"],
        true
    );
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn cancellation_releases_archived_partial_rma_remainder_without_erasing_received_allowance() {
    let f = Fixture::new().await;
    let oid = shipped_two_line_source(&f).await;
    f.command(Command::CreateLocation {
        code: "B-01".into(),
        zone: "Storage".into(),
        kind: "storage".into(),
    })
    .await;
    let r = f
        .command(cancellation_return(&oid, "ARCHIVED-RMA", "a", 3))
        .await;
    let rid = r["entity_id"].as_str().expect("receipt").to_string();
    f.command(Command::Receive {
        receipt_id: rid.clone(),
        line_id: "r".into(),
        quantity: 1,
        location: "RECV-01".into(),
    })
    .await;
    f.command(Command::CancelReceipt {
        receipt_id: rid.clone(),
        reason: "Only one unit returned".into(),
    })
    .await;
    for n in 0..21 {
        let r = f
            .command(Command::CreateReceipt {
                reference: format!("LATER-CLOSED-{n}"),
                supplier: "Supplier".into(),
                lines: vec![ReceiptInput {
                    id: "line".into(),
                    sku: "SKU-A".into(),
                    lot: "LOT-A".into(),
                    quantity: 1,
                }],
            })
            .await;
        f.command(Command::CancelReceipt {
            receipt_id: r["entity_id"].as_str().expect("id").into(),
            reason: "Delivery abandoned".into(),
        })
        .await;
    }
    for n in 0..100 {
        f.command(Command::MoveStock {
            sku: "SKU-A".into(),
            lot: "LOT-A".into(),
            from: if n % 2 == 0 { "A-01" } else { "B-01" }.into(),
            to: if n % 2 == 0 { "B-01" } else { "A-01" }.into(),
            quantity: 1,
        })
        .await;
    }
    assert!(
        !f.state().await.receipts.iter().any(|r| r.id == rid),
        "closed RMA must age out of operational window"
    );
    f.reject(
        cancellation_return(&oid, "OVER-ARCHIVED", "a", 3),
        StatusCode::UNPROCESSABLE_ENTITY,
    )
    .await;
    f.command(cancellation_return(&oid, "VALID-ARCHIVED", "a", 2))
        .await;
    let proof = f
        .store
        .reconciliation(&f.identity.tenant, "wh-main")
        .await
        .expect("all-history proof");
    assert_eq!(proof["ok"], true);
    assert!(proof["receipt_cancellations"]
        .as_array()
        .expect("proofs")
        .iter()
        .any(|p| p["receipt_id"] == rid && p["cancelled"] == 2 && p["ok"] == true));
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn cancellation_released_return_allowance_cannot_be_spent_twice_by_repeatable_read_sql() {
    let f = Fixture::new().await;
    let oid = shipped_two_line_source(&f).await;
    let r = f
        .command(cancellation_return(&oid, "SQL-CANCEL-RMA", "a", 3))
        .await;
    let rid = r["entity_id"].as_str().expect("receipt").to_string();
    f.command(Command::Receive {
        receipt_id: rid.clone(),
        line_id: "r".into(),
        quantity: 1,
        location: "RECV-01".into(),
    })
    .await;
    f.command(Command::CancelReceipt {
        receipt_id: rid,
        reason: "Two units not returning".into(),
    })
    .await;
    let mut candidates = vec![];
    for n in 0..2 {
        let r = f
            .command(Command::CreateReceipt {
                reference: format!("SQL-REAUTHORIZE-{n}"),
                supplier: "Customer return".into(),
                lines: vec![ReceiptInput {
                    id: "line".into(),
                    sku: "SKU-A".into(),
                    lot: "LOT-A".into(),
                    quantity: 2,
                }],
            })
            .await;
        candidates.push(r["entity_id"].as_str().expect("receipt").to_string());
    }
    let barrier = Arc::new(tokio::sync::Barrier::new(2));
    let attempt = |receipt: String| {
        let store = f.store.clone();
        let tenant = f.identity.tenant.clone();
        let order = oid.clone();
        let barrier = barrier.clone();
        async move {
            let mut tx = store.pool.begin().await.expect("transaction");
            sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ")
                .execute(&mut *tx)
                .await
                .expect("isolation");
            let _: i64 = sqlx::query_scalar("SELECT count(*) FROM wms_receipt_cancelled_lines")
                .fetch_one(&mut *tx)
                .await
                .expect("snapshot");
            sqlx::query("INSERT INTO wms_return_lines VALUES($1,'wh-main',$2,'line',$3,'a','SKU-A','LOT-A',2,'New physical return')").bind(tenant).bind(receipt).bind(order).execute(&mut *tx).await.expect("insert");
            barrier.wait().await;
            tx.commit().await
        }
    };
    let (a, b) = tokio::join!(
        attempt(candidates[0].clone()),
        attempt(candidates[1].clone())
    );
    assert_eq!(
        usize::from(a.is_ok()) + usize::from(b.is_ok()),
        1,
        "{a:?}/{b:?}"
    );
    let available = stateset_warehouse::returns::eligible(&f.store, &f.identity, "wh-main", &oid)
        .await
        .expect("allowance");
    assert_eq!(
        available["lines"]
            .as_array()
            .expect("lines")
            .iter()
            .find(|l| l["order_line_id"] == "a")
            .expect("a")["remaining"],
        0
    );
    assert_eq!(
        f.store
            .reconciliation(&f.identity.tenant, "wh-main")
            .await
            .expect("proof")["ok"],
        true
    );
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn cancellation_sql_rejects_status_only_and_incomplete_or_wrong_frozen_remainders() {
    let f = Fixture::new().await;
    f.seed_catalog().await;
    let r = f
        .command(Command::CreateReceipt {
            reference: "FORGED-CANCEL".into(),
            supplier: "Supplier".into(),
            lines: vec![
                ReceiptInput {
                    id: "first".into(),
                    sku: "SKU-A".into(),
                    lot: "LOT-A".into(),
                    quantity: 3,
                },
                ReceiptInput {
                    id: "second".into(),
                    sku: "SKU-A".into(),
                    lot: "LOT-A".into(),
                    quantity: 2,
                },
            ],
        })
        .await;
    let rid = r["entity_id"].as_str().expect("receipt").to_string();
    f.command(Command::Receive {
        receipt_id: rid.clone(),
        line_id: "first".into(),
        quantity: 1,
        location: "RECV-01".into(),
    })
    .await;
    let before = f.state().await;
    for kind in 0..3 {
        let mut tx = f.store.pool.begin().await.expect("transaction");
        sqlx::query("UPDATE wms_receipts SET status='cancelled',data=jsonb_set(data,'{status}','\"cancelled\"'::jsonb) WHERE tenant=$1 AND warehouse_id='wh-main' AND id=$2").bind(&f.identity.tenant).bind(&rid).execute(&mut *tx).await.expect("deferred status");
        if kind > 0 {
            sqlx::query("INSERT INTO wms_receipt_cancellations VALUES($1,'wh-main',$2,'Delivery abandoned','sql:fixture','2026-10-18T00:00:00Z')").bind(&f.identity.tenant).bind(&rid).execute(&mut *tx).await.expect("header");
            sqlx::query("INSERT INTO wms_receipt_cancelled_lines VALUES($1,'wh-main',$2,'first','SKU-A','LOT-A',3,1,2)").bind(&f.identity.tenant).bind(&rid).execute(&mut *tx).await.expect("line");
            if kind == 2 {
                sqlx::query("INSERT INTO wms_receipt_cancelled_lines VALUES($1,'wh-main',$2,'second','SKU-A','LOT-A',2,1,1)").bind(&f.identity.tenant).bind(&rid).execute(&mut *tx).await.expect("wrong received snapshot");
            }
        }
        assert!(
            tx.commit().await.is_err(),
            "forged cancellation {kind} must roll back"
        );
        assert_eq!(f.state().await, before);
    }
    f.command(Command::CancelReceipt {
        receipt_id: rid,
        reason: "Actual reviewed remainder".into(),
    })
    .await;
    assert_eq!(
        f.store
            .reconciliation(&f.identity.tenant, "wh-main")
            .await
            .expect("proof")["ok"],
        true
    );
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn cancellation_cannot_release_allowance_by_changing_the_original_expected_return_quantity() {
    let f = Fixture::new().await;
    let oid = shipped_two_line_source(&f).await;
    let r = f
        .command(cancellation_return(&oid, "EXPECTED-IDENTITY", "a", 3))
        .await;
    let rid = r["entity_id"].as_str().expect("receipt").to_string();
    f.command(Command::Receive {
        receipt_id: rid.clone(),
        line_id: "r".into(),
        quantity: 1,
        location: "RECV-01".into(),
    })
    .await;
    let extra = sqlx::query(
        "INSERT INTO wms_receipt_lines VALUES($1,'wh-main',$2,'unowned','SKU-A','LOT-A',1,0)",
    )
    .bind(&f.identity.tenant)
    .bind(&rid)
    .execute(&f.store.pool)
    .await;
    assert!(
        extra.is_err(),
        "RMA receipt cannot gain an unauthorized line"
    );
    let mutation=sqlx::query("UPDATE wms_receipt_lines SET quantity=4 WHERE tenant=$1 AND warehouse_id='wh-main' AND receipt_id=$2 AND id='r'").bind(&f.identity.tenant).bind(&rid).execute(&f.store.pool).await;
    f.command(Command::CancelReceipt {
        receipt_id: rid,
        reason: "Two authorized units will not arrive".into(),
    })
    .await;
    let allowance = stateset_warehouse::returns::eligible(&f.store, &f.identity, "wh-main", &oid)
        .await
        .expect("allowance");
    assert_eq!(
        allowance["lines"]
            .as_array()
            .expect("lines")
            .iter()
            .find(|l| l["order_line_id"] == "a")
            .expect("source")["remaining"],
        2,
        "Changing expected quantities must never create more return allowance"
    );
    assert!(mutation.is_err(),"original receipt line identity/expected quantity must be immutable before cancellation too");
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn return_allowance_cannot_be_inflated_by_mutating_committed_shipped_order_quantities() {
    let f = Fixture::new().await;
    let oid = shipped_two_line_source(&f).await;
    let original:Value=sqlx::query_scalar("SELECT payload FROM wms_events WHERE tenant=$1 AND warehouse_id='wh-main' AND payload->>'type'='fulfillment.shipped' AND payload->'order'->>'id'=$2").bind(&f.identity.tenant).bind(&oid).fetch_one(&f.store.pool).await.expect("original shipment evidence");
    assert_eq!(original["order"]["lines"][0]["quantity"], 3);
    let mut tx = f.store.pool.begin().await.expect("transaction");
    let mutation: std::result::Result<(),sqlx::Error> = async {
    sqlx::query("UPDATE wms_order_lines SET quantity=4 WHERE tenant=$1 AND warehouse_id='wh-main' AND order_id=$2 AND id='a'").bind(&f.identity.tenant).bind(&oid).execute(&mut *tx).await?;
    sqlx::query("UPDATE wms_allocations SET quantity=4,picked=4,shipped=4 WHERE tenant=$1 AND warehouse_id='wh-main' AND order_id=$2 AND line_id='a'").bind(&f.identity.tenant).bind(&oid).execute(&mut *tx).await?;
    tx.commit().await
    }.await;
    let allowance = stateset_warehouse::returns::eligible(&f.store, &f.identity, "wh-main", &oid)
        .await
        .expect("original allowance");
    assert_eq!(
        allowance["lines"]
            .as_array()
            .expect("lines")
            .iter()
            .find(|l| l["order_line_id"] == "a")
            .expect("line")["shipped"],
        3,
        "mutable operational rows cannot increase the original physical shipment"
    );
    assert!(
        mutation.is_err(),
        "committed shipment facts must stay bound to their original event"
    );
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn shipment_evidence_freezes_lot_ownership_handoff_and_source_but_ignores_untrusted_child_caches(
) {
    let f = Fixture::new().await;
    let oid = shipped_two_line_source(&f).await;
    let before = f.state().await;
    let document: Value = sqlx::query_scalar(
        "SELECT document FROM wms_shipment_evidence WHERE tenant=$1 AND order_id=$2",
    )
    .bind(&f.identity.tenant)
    .bind(&oid)
    .fetch_one(&f.store.pool)
    .await
    .expect("source document");
    assert_eq!(
        document,
        json!(before
            .orders
            .iter()
            .find(|order| order.id == oid)
            .expect("canonical order")),
        "SQL evidence projection must retain the complete typed Order contract"
    );

    for statement in [
        "UPDATE wms_allocations SET lot='FORGED' WHERE tenant=$1 AND order_id=$2",
        "DELETE FROM wms_allocations WHERE tenant=$1 AND order_id=$2",
        "UPDATE wms_orders SET data=jsonb_set(data,'{handoff_reference}','\"FORGED\"'::jsonb) WHERE tenant=$1 AND id=$2",
        "UPDATE wms_orders SET status='pending',data=jsonb_set(data,'{status}','\"pending\"'::jsonb) WHERE tenant=$1 AND id=$2",
        "DELETE FROM wms_shipment_evidence WHERE tenant=$1 AND order_id=$2",
        "UPDATE wms_shipment_evidence SET source_version=source_version+1 WHERE tenant=$1 AND order_id=$2",
    ] {assert!(sqlx::query(statement).bind(&f.identity.tenant).bind(&oid).execute(&f.store.pool).await.is_err(),"must reject {statement}");}
    assert_eq!(f.state().await, before);
    sqlx::query("UPDATE wms_orders SET data=jsonb_set(jsonb_set(data,'{lines}','false'::jsonb),'{allocations}','{}'::jsonb) WHERE tenant=$1 AND id=$2").bind(&f.identity.tenant).bind(&oid).execute(&f.store.pool).await.expect("cached children are not authoritative");
    let allowance = stateset_warehouse::returns::eligible(&f.store, &f.identity, "wh-main", &oid)
        .await
        .expect("original source");
    assert_eq!(allowance["shipment_source"]["kind"], "event");
    assert_eq!(allowance["lines"][0]["shipped"], 3);
    assert!(allowance["lines"]
        .as_array()
        .expect("lines")
        .iter()
        .all(|l| l["lot"] == "LOT-A"));
    f.command(cancellation_return(&oid, "FROM-IMMUTABLE-SOURCE", "a", 3))
        .await;
    let proof = f
        .store
        .reconciliation(&f.identity.tenant, "wh-main")
        .await
        .expect("all-history proof");
    assert_eq!(proof["ok"], true);
    assert_eq!(proof["shipments"][0]["source_kind"], "event");
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn shipment_evidence_original_event_cannot_be_duplicated_or_replaced_with_a_baseline() {
    let f = Fixture::new().await;
    let oid = shipped_two_line_source(&f).await;
    let original:Value=sqlx::query_scalar("SELECT payload FROM wms_events WHERE tenant=$1 AND payload->>'type'='fulfillment.shipped' AND payload#>>'{order,id}'=$2").bind(&f.identity.tenant).bind(&oid).fetch_one(&f.store.pool).await.expect("event");
    let mut forged = original.clone();
    let event_id = id();
    let version = f.state().await.version + 1;
    forged["id"] = json!(event_id);
    forged["version"] = json!(version);
    forged["result"]["version"] = json!(version);
    forged["request_id"] = json!(id());
    let duplicate=sqlx::query("INSERT INTO wms_events(id,tenant,warehouse_id,version,payload) VALUES($1,$2,'wh-main',$3,$4)").bind(&event_id).bind(&f.identity.tenant).bind(version).bind(forged).execute(&f.store.pool).await;
    assert!(
        duplicate.is_err(),
        "one full-order handoff cannot acquire another original event"
    );
    let replacement=sqlx::query("INSERT INTO wms_shipment_evidence(tenant,warehouse_id,order_id,source_kind,source_version,document) SELECT tenant,warehouse_id,order_id,'synthetic_fixture',source_version,document FROM wms_shipment_evidence WHERE tenant=$1 AND order_id=$2").bind(&f.identity.tenant).bind(&oid).execute(&f.store.pool).await;
    assert!(
        replacement.is_err(),
        "physical handoff cannot be relabelled as a baseline"
    );
    let count:i64=sqlx::query_scalar("SELECT count(*) FROM wms_events WHERE tenant=$1 AND payload->>'type'='fulfillment.shipped'").bind(&f.identity.tenant).fetch_one(&f.store.pool).await.expect("event count");
    assert_eq!(count, 1);
    assert_eq!(
        f.store
            .reconciliation(&f.identity.tenant, "wh-main")
            .await
            .expect("proof")["ok"],
        true
    );
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn shipment_evidence_and_physical_handoff_roll_back_on_final_delivery_failure_and_retry_once()
{
    let f = Fixture::new().await;
    let oid = packed_two_line_source(&f).await;
    let before = f.state().await;
    let request = Request {
        serial_inputs: Vec::new(),
        unit_inputs: Vec::new(),
        request_id: id(),
        expected_version: before.version,
        resource_versions: None,
        command: Command::Ship {
            order_id: oid.clone(),
            carrier: "Carrier".into(),
            handoff_reference: "ATOMIC-HANDOFF".into(),
        },
    };
    let trigger = format!("shipment_fault_{}", id().replace('-', ""));
    let fault=format!("CREATE FUNCTION {trigger}() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.tenant='{}' THEN RAISE EXCEPTION 'injected shipment delivery failure'; END IF; RETURN NEW; END $$; CREATE TRIGGER {trigger} BEFORE INSERT ON wms_outbox FOR EACH ROW EXECUTE FUNCTION {trigger}()",f.identity.tenant);
    sqlx::raw_sql(&fault)
        .execute(&f.store.pool)
        .await
        .expect("own fault trigger");
    let failed = f.submit("operator-acceptance", &request).await;
    sqlx::raw_sql(&format!(
        "DROP TRIGGER {trigger} ON wms_outbox; DROP FUNCTION {trigger}()"
    ))
    .execute(&f.store.pool)
    .await
    .expect("remove own fault");
    assert_eq!(failed.0, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(f.state().await, before);
    let count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM wms_shipment_evidence WHERE tenant=$1")
            .bind(&f.identity.tenant)
            .fetch_one(&f.store.pool)
            .await
            .expect("evidence count");
    assert_eq!(count, 0);
    let accepted = f.submit("operator-acceptance", &request).await;
    assert_eq!(accepted.0, StatusCode::OK, "{accepted:?}");
    let shipped = f.state().await;
    let replay = f.submit("operator-acceptance", &request).await;
    assert_eq!(replay.1["replayed"], true);
    assert_eq!(replay.1["result"], accepted.1["result"]);
    assert_eq!(f.state().await, shipped);
    let count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM wms_shipment_evidence WHERE tenant=$1")
            .bind(&f.identity.tenant)
            .fetch_one(&f.store.pool)
            .await
            .expect("evidence count");
    assert_eq!(count, 1);
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn shipment_evidence_serializes_repeatable_read_edits_started_before_the_handoff() {
    let f = Fixture::new().await;
    let oid = packed_two_line_source(&f).await;
    let mut stale = f.store.pool.begin().await.expect("stale transaction");
    sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ")
        .execute(&mut *stale)
        .await
        .expect("isolation");
    let count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM wms_shipment_evidence WHERE tenant=$1")
            .bind(&f.identity.tenant)
            .fetch_one(&mut *stale)
            .await
            .expect("snapshot before handoff");
    assert_eq!(count, 0);
    f.command(Command::Ship {
        order_id: oid.clone(),
        carrier: "Carrier".into(),
        handoff_reference: "HANDOFF-AFTER-SNAPSHOT".into(),
    })
    .await;
    let result=sqlx::query("INSERT INTO wms_allocations VALUES($1,'wh-main',$2,'a','SKU-A','STALE-LOT','A-01',1,1,1,0)").bind(&f.identity.tenant).bind(&oid).execute(&mut *stale).await;
    assert!(
        result.is_err(),
        "an old snapshot cannot miss the newly frozen shipment"
    );
    stale.rollback().await.expect("rollback stale repair");
    let allowance = stateset_warehouse::returns::eligible(&f.store, &f.identity, "wh-main", &oid)
        .await
        .expect("original source");
    assert!(allowance["lines"]
        .as_array()
        .expect("lines")
        .iter()
        .all(|l| l["lot"] == "LOT-A"));
    assert_eq!(
        f.store
            .reconciliation(&f.identity.tenant, "wh-main")
            .await
            .expect("proof")["ok"],
        true
    );
}

async fn preceding_shipment_schema(
    f: &Fixture,
    legacy: Option<&State>,
    include_events: bool,
) -> sqlx::Transaction<'static, sqlx::Postgres> {
    preceding_shipment_schema_with_lot_compatibility(f, legacy, include_events, false).await
}
async fn preceding_shipment_schema_with_lot_compatibility(
    f: &Fixture,
    legacy: Option<&State>,
    include_events: bool,
    previous_lot_contract: bool,
) -> sqlx::Transaction<'static, sqlx::Postgres> {
    let mut tx = f.store.pool.begin().await.expect("upgrade transaction");
    let schema = format!("shipment_upgrade_{}", id().replace('-', "_"));
    sqlx::raw_sql(&format!(
        "CREATE SCHEMA {schema}; SET LOCAL search_path TO {schema},public"
    ))
    .execute(&mut *tx)
    .await
    .expect("own upgrade schema");
    sqlx::raw_sql(include_str!("../migrations/001_initial.sql"))
        .execute(&mut *tx)
        .await
        .expect("initial schema");
    if let Some(state) = legacy {
        sqlx::query(
            "INSERT INTO wms_workspaces(tenant,warehouse_id,version,state) VALUES($1,$2,$3,$4)",
        )
        .bind(&f.identity.tenant)
        .bind(&state.warehouse.id)
        .bind(state.version)
        .bind(json!(state))
        .execute(&mut *tx)
        .await
        .expect("original retained legacy snapshot");
    }
    for migration in [
        include_str!("../migrations/002_relational_execution.sql"),
        include_str!("../migrations/003_native_operations.sql"),
        include_str!("../migrations/004_native_source_ownership.sql"),
        include_str!("../migrations/005_conditional_oms_fulfillment.sql"),
        include_str!("../migrations/006_oms_rejections.sql"),
        include_str!("../migrations/007_operations_indexes.sql"),
        include_str!("../migrations/008_delivery_recipient.sql"),
        include_str!("../migrations/009_resource_versions.sql"),
        include_str!("../migrations/010_customer_returns.sql"),
        include_str!("../migrations/011_immutable_command_receipts.sql"),
        include_str!("../migrations/012_receipt_cancellation.sql"),
    ] {
        sqlx::raw_sql(migration)
            .execute(&mut *tx)
            .await
            .expect("actual preceding migration");
    }
    // Copy the real command-generated business fixture; original movements rebuild
    // the ledger/evidence projections through the preceding schema's own triggers.
    for table in [
        "wms_warehouses",
        "wms_products",
        "wms_locations",
        "wms_stock",
        "wms_orders",
        "wms_order_lines",
        "wms_allocations",
        "wms_receipts",
        "wms_receipt_lines",
        "wms_return_lines",
        "wms_movements",
        "wms_activity",
        "wms_events",
    ] {
        let filter = if table == "wms_events" && !include_events {
            " AND payload->>'type'<>'fulfillment.shipped'"
        } else {
            ""
        };
        let statement = if table == "wms_events" && previous_lot_contract {
            format!("INSERT INTO {table}(id,tenant,warehouse_id,version,payload,created_at) SELECT id,tenant,warehouse_id,version,payload-'lot_changes'-'lot_decisions'-'previous_order',created_at FROM public.{table} WHERE tenant=$1{filter}")
        } else if table == "wms_movements" {
            let columns = "seq,id,tenant,warehouse_id,sku,lot,from_location,to_location,quantity,kind,reference,receipt_id,receipt_line_id,data";
            format!("INSERT INTO {table} ({columns}) SELECT {columns} FROM public.{table} WHERE tenant=$1{filter}")
        } else {
            format!("INSERT INTO {table} SELECT * FROM public.{table} WHERE tenant=$1{filter}")
        };
        sqlx::query(&statement)
            .bind(&f.identity.tenant)
            .execute(&mut *tx)
            .await
            .expect("copy original fixture evidence");
    }
    if previous_lot_contract {
        sqlx::query("INSERT INTO wms_commands SELECT * FROM public.wms_commands WHERE tenant=$1")
            .bind(&f.identity.tenant)
            .execute(&mut *tx)
            .await
            .expect("preceding immutable original command receipts");
    }
    sqlx::query("SET CONSTRAINTS ALL IMMEDIATE")
        .execute(&mut *tx)
        .await
        .expect("preceding guards accept original fixture");
    tx
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn shipment_evidence_upgrade_preserves_original_event_and_rejects_drift_or_missing_provenance(
) {
    let f = Fixture::new().await;
    let oid = shipped_two_line_source(&f).await;
    for mode in ["valid", "drift", "missing"] {
        let mut tx = preceding_shipment_schema(&f, None, mode != "missing").await;
        if mode == "drift" {
            sqlx::query("UPDATE wms_allocations SET lot='FORGED' WHERE tenant=$1 AND order_id=$2 AND line_id='a'").bind(&f.identity.tenant).bind(&oid).execute(&mut *tx).await.expect("preceding schema allows shipped-lot drift");
        }
        sqlx::query("SAVEPOINT shipment_upgrade")
            .execute(&mut *tx)
            .await
            .expect("savepoint");
        let upgrade = sqlx::raw_sql(include_str!("../migrations/013_shipment_evidence.sql"))
            .execute(&mut *tx)
            .await;
        if mode == "valid" {
            upgrade.expect("valid original event migrates");
            let kind: String = sqlx::query_scalar(
                "SELECT source_kind FROM wms_shipment_evidence WHERE tenant=$1 AND order_id=$2",
            )
            .bind(&f.identity.tenant)
            .bind(&oid)
            .fetch_one(&mut *tx)
            .await
            .expect("provenance");
            assert_eq!(kind, "event");
            let allowance = stateset_warehouse::returns::availability(
                &mut tx,
                &f.identity.tenant,
                "wh-main",
                &oid,
            )
            .await
            .expect("source allowance");
            assert_eq!(allowance[0]["shipped"], 3);
        } else {
            assert!(
                upgrade.is_err(),
                "{mode} must not become an invented baseline"
            );
            sqlx::query("ROLLBACK TO SAVEPOINT shipment_upgrade")
                .execute(&mut *tx)
                .await
                .expect("failed upgrade preserves prior data");
        }
        tx.rollback().await.expect("remove own upgrade schema");
    }
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn shipment_evidence_upgrade_labels_legacy_and_synthetic_origins_without_inventing_handoff_events(
) {
    let f = Fixture::new().await;
    f.store
        .provision(&f.identity.tenant, true)
        .await
        .expect("disclosed synthetic demo");
    let state = f
        .store
        .state(&f.identity.tenant, "wh-pdx")
        .await
        .expect("fixture snapshot");
    let order = state
        .orders
        .iter()
        .find(|o| o.status == "shipped")
        .expect("fixture shipment");
    for legacy in [false, true] {
        let mut tx = preceding_shipment_schema(&f, legacy.then_some(&state), true).await;
        sqlx::raw_sql(include_str!("../migrations/013_shipment_evidence.sql"))
            .execute(&mut *tx)
            .await
            .expect("original baseline migrates");
        let row:(String,Option<String>,bool)=sqlx::query_as("SELECT source_kind,event_id,wms_shipment_consistent(tenant,warehouse_id,order_id) FROM wms_shipment_evidence WHERE tenant=$1 AND warehouse_id='wh-pdx' AND order_id=$2").bind(&f.identity.tenant).bind(&order.id).fetch_one(&mut *tx).await.expect("baseline source");
        assert_eq!(
            row.0,
            if legacy {
                "legacy_snapshot"
            } else {
                "synthetic_fixture"
            }
        );
        assert!(row.1.is_none());
        assert!(row.2);
        let events:i64=sqlx::query_scalar("SELECT count(*) FROM wms_events WHERE tenant=$1 AND payload->>'type'='fulfillment.shipped'").bind(&f.identity.tenant).fetch_one(&mut *tx).await.expect("event count");
        assert_eq!(
            events, 0,
            "baseline adoption must not fabricate a physical event"
        );
        tx.rollback().await.expect("remove own baseline schema");
    }
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn shipment_evidence_actual_legacy_store_import_preserves_source_without_synthetic_events() {
    let f = Fixture::new().await;
    let state = stateset_warehouse::domain::demo_state();
    let schema = format!("shipment_legacy_{}", id().replace('-', "_"));
    let mut setup = f.store.pool.begin().await.expect("legacy setup");
    sqlx::raw_sql(&format!(
        "CREATE SCHEMA {schema}; SET LOCAL search_path TO {schema},public"
    ))
    .execute(&mut *setup)
    .await
    .expect("own legacy schema");
    sqlx::raw_sql(include_str!("../migrations/001_initial.sql"))
        .execute(&mut *setup)
        .await
        .expect("original aggregate schema");
    sqlx::query(
        "INSERT INTO wms_workspaces(tenant,warehouse_id,version,state) VALUES($1,$2,$3,$4)",
    )
    .bind(&f.identity.tenant)
    .bind(&state.warehouse.id)
    .bind(state.version)
    .bind(json!(state))
    .execute(&mut *setup)
    .await
    .expect("original aggregate snapshot");
    setup
        .commit()
        .await
        .expect("publish only isolated legacy fixture");
    let mut url = reqwest::Url::parse(&std::env::var("WMS_TEST_DATABASE_URL").expect("test URL"))
        .expect("test URI");
    url.query_pairs_mut()
        .append_pair("options", &format!("-c search_path={schema},public"));
    let imported = Store::connect(url.as_str()).await;
    let observations = if let Ok(store) = &imported {
        let proof = store
            .reconciliation(&f.identity.tenant, &state.warehouse.id)
            .await;
        let scope: std::result::Result<String, _> = sqlx::query_scalar("SHOW search_path")
            .fetch_one(&store.pool)
            .await;
        let events: std::result::Result<i64, _> =
            sqlx::query_scalar("SELECT count(*) FROM wms_events WHERE tenant=$1")
                .bind(&f.identity.tenant)
                .fetch_one(&store.pool)
                .await;
        let old = store.state(&f.identity.tenant, &state.warehouse.id).await;
        let original = state
            .orders
            .iter()
            .find(|o| o.status == "shipped")
            .expect("original legacy shipment");
        let allocation = &original.allocations[0];
        let trace: std::result::Result<Vec<(Value,bool)>,_> = sqlx::query_as("SELECT document,COALESCE(source_ok,false) FROM wms_lot_trace_records($1,$2,$3,ARRAY[$4::text]) WHERE section='shipments'")
            .bind(&f.identity.tenant).bind(&allocation.sku).bind(&allocation.lot).bind(&state.warehouse.id).fetch_all(&store.pool).await;
        store.pool.close().await;
        Some((proof, scope, events, old, trace))
    } else {
        None
    };
    sqlx::raw_sql(&format!("DROP SCHEMA {schema} CASCADE"))
        .execute(&f.store.pool)
        .await
        .expect("remove own legacy fixture schema");
    imported.expect("actual Store importer accepts the original shipped snapshot");
    let (proof, scope, events, old, trace) = observations.expect("import observations");
    let trace = trace.expect("actual migrated legacy trace");
    assert!(!trace.is_empty());
    assert!(trace.iter().all(|(document,ok)|*ok && document["source_kind"]=="legacy_snapshot" && document["event_id"].is_null()),"trace must retain the actual preceding-schema baseline without inventing an observed handoff");
    assert!(scope.expect("scope").contains(&schema));
    assert_eq!(
        events.expect("event count"),
        0,
        "legacy adoption does not fabricate a physical event"
    );
    let proof = proof.expect("legacy full proof");
    assert_eq!(proof["ok"], true);
    assert!(proof["shipments"]
        .as_array()
        .expect("shipments")
        .iter()
        .all(|s| s["source_kind"] == "legacy_snapshot" && s["event_id"].is_null()));
    let mut actual = old.expect("imported state").stocks;
    let mut expected = state.stocks;
    actual.sort_by(|a, b| (&a.sku, &a.lot, &a.location).cmp(&(&b.sku, &b.lot, &b.location)));
    expected.sort_by(|a, b| (&a.sku, &a.lot, &a.location).cmp(&(&b.sku, &b.lot, &b.location)));
    assert_eq!(actual, expected);
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn shipment_evidence_aggregates_split_bin_lots_without_lending_another_lines_return_allowance(
) {
    let f = Fixture::new().await;
    f.seed_catalog().await;
    f.command(Command::CreateLocation {
        code: "B-01".into(),
        zone: "Storage".into(),
        kind: "storage".into(),
    })
    .await;
    for (location, lot, q) in [
        ("A-01", "LOT-A", 2),
        ("B-01", "LOT-A", 2),
        ("B-01", "LOT-B", 1),
    ] {
        f.command(Command::CountStock {
            sku: "SKU-A".into(),
            lot: lot.into(),
            location: location.into(),
            counted: q,
            reason: "Synthetic split-bin fixture".into(),
        })
        .await;
    }
    let r = f
        .command(Command::CreateOrder {
            reference: "SPLIT-SHIPMENT-SOURCE".into(),
            source_order_id: None,
            customer: "Customer".into(),
            destination: "Destination".into(),
            priority: "normal".into(),
            lines: vec![
                LineInput {
                    id: "a".into(),
                    sku: "SKU-A".into(),
                    quantity: 3,
                },
                LineInput {
                    id: "b".into(),
                    sku: "SKU-A".into(),
                    quantity: 2,
                },
            ],
        })
        .await;
    let oid = r["entity_id"].as_str().expect("order").to_string();
    f.command(Command::Allocate {
        order_id: oid.clone(),
    })
    .await;
    let allocations = f
        .state()
        .await
        .orders
        .into_iter()
        .find(|o| o.id == oid)
        .expect("order")
        .allocations;
    assert_eq!(allocations.len(), 4);
    for a in allocations {
        f.command(Command::Pick {
            order_id: oid.clone(),
            line_id: a.line_id,
            location: a.location,
            lot: a.lot,
            quantity: a.quantity,
        })
        .await;
    }
    f.command(Command::Pack {
        order_id: oid.clone(),
    })
    .await;
    f.command(Command::Ship {
        order_id: oid.clone(),
        carrier: "Carrier".into(),
        handoff_reference: "SPLIT-BIN-HANDOFF".into(),
    })
    .await;
    let eligible = stateset_warehouse::returns::eligible(&f.store, &f.identity, "wh-main", &oid)
        .await
        .expect("original per-line/lot quantities");
    let lines = eligible["lines"].as_array().expect("lines");
    assert_eq!(lines.len(), 3);
    assert_eq!(
        lines
            .iter()
            .find(|l| l["order_line_id"] == "a" && l["lot"] == "LOT-A")
            .expect("a")["shipped"],
        3
    );
    assert_eq!(
        lines
            .iter()
            .find(|l| l["order_line_id"] == "b" && l["lot"] == "LOT-B")
            .expect("b")["shipped"],
        1
    );
    f.reject(
        Command::CreateReturn {
            reference: "BORROWED-LOT".into(),
            order_id: oid.clone(),
            reason: "Cannot borrow another line".into(),
            lines: vec![stateset_warehouse::returns::ReturnLineInput {
                id: "r".into(),
                order_line_id: "a".into(),
                lot: "LOT-B".into(),
                quantity: 1,
            }],
        },
        StatusCode::UNPROCESSABLE_ENTITY,
    )
    .await;
    f.command(cancellation_return(&oid, "SPLIT-BIN-RMA", "a", 2))
        .await;
    let after = stateset_warehouse::returns::eligible(&f.store, &f.identity, "wh-main", &oid)
        .await
        .expect("remaining original quantity");
    assert_eq!(
        after["lines"]
            .as_array()
            .expect("lines")
            .iter()
            .find(|l| l["order_line_id"] == "a")
            .expect("a")["remaining"],
        1
    );
    assert_eq!(
        f.store
            .reconciliation(&f.identity.tenant, "wh-main")
            .await
            .expect("full proof")["ok"],
        true
    );
}

async fn transfer_at(f: &Fixture, w: &str, command: Command) -> (Request, Value) {
    let version = f
        .store
        .state(&f.identity.tenant, w)
        .await
        .expect("facility")
        .version;
    let request = Request {
        serial_inputs: Vec::new(),
        unit_inputs: Vec::new(),
        request_id: id(),
        expected_version: version,
        resource_versions: None,
        command,
    };
    let (status, response) = f
        .http(
            "POST",
            &format!("/v1/warehouses/{w}/commands"),
            "operator-acceptance",
            Some(json!(request)),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{response}");
    (request, response)
}
async fn transfer_fixture() -> Fixture {
    let f = Fixture::new().await;
    f.seed_catalog().await;
    f.command(opening_count(10)).await;
    f.command(Command::CountStock {
        sku: "SKU-A".into(),
        lot: "LOT-B".into(),
        location: "A-01".into(),
        counted: 4,
        reason: "Other original lot".into(),
    })
    .await;
    let create = Request {
        serial_inputs: Vec::new(),
        unit_inputs: Vec::new(),
        request_id: id(),
        expected_version: 0,
        resource_versions: None,
        command: Command::CreateWarehouse {
            code: "EAST".into(),
            name: "Synthetic destination".into(),
            city: "Vancouver".into(),
            timezone: "America/Vancouver".into(),
        },
    };
    f.store
        .execute(&f.identity, "wh-east", &create)
        .await
        .expect("create destination");
    let product = Request {
        serial_inputs: Vec::new(),
        unit_inputs: Vec::new(),
        request_id: id(),
        expected_version: 1,
        resource_versions: None,
        command: Command::CreateProduct {
            serial_controlled: false,
            sku: "SKU-A".into(),
            name: "Warehouse acceptance product".into(),
            category: "General".into(),
            reorder_point: 1,
        },
    };
    f.store
        .execute(&f.identity, "wh-east", &product)
        .await
        .expect("explicit destination catalog");
    f
}
fn dispatch_transfer() -> Command {
    use stateset_warehouse::transfers::LineInput;
    Command::DispatchTransfer {
        reference: "MOVE-TO-EAST".into(),
        destination_warehouse_id: "wh-east".into(),
        carrier: "Synthetic carrier".into(),
        handoff_reference: "CARRIER-ORIGINAL".into(),
        lines: vec![
            LineInput {
                id: "a".into(),
                sku: "SKU-A".into(),
                lot: "LOT-A".into(),
                location: "A-01".into(),
                quantity: 3,
            },
            LineInput {
                id: "b".into(),
                sku: "SKU-A".into(),
                lot: "LOT-A".into(),
                location: "A-01".into(),
                quantity: 2,
            },
            LineInput {
                id: "c".into(),
                sku: "SKU-A".into(),
                lot: "LOT-B".into(),
                location: "A-01".into(),
                quantity: 2,
            },
        ],
    }
}
fn transfer_arrival(id: &str, line: &str, q: i64) -> Command {
    Command::ReceiveTransfer {
        transfer_id: id.into(),
        line_id: line.into(),
        quantity: q,
        location: "RECV-01".into(),
        arrival_reference: "Witnessed original arrival".into(),
    }
}
fn transfer_loss(tid: &str, line: &str, quantity: i64) -> Command {
    Command::RecordTransferLoss {
        transfer_id: tid.into(),
        line_id: line.into(),
        quantity,
        incident_reference: format!("CARRIER-LOSS-{line}"),
        reason: "Carrier and controller confirmed missing each units".into(),
    }
}
async fn transfer_admin(f: &Fixture, command: Command) -> (Request, Value) {
    let state = f
        .store
        .state(&f.identity.tenant, "wh-east")
        .await
        .expect("destination review");
    let request = Request {
        serial_inputs: Vec::new(),
        unit_inputs: Vec::new(),
        request_id: id(),
        expected_version: state.version,
        resource_versions: None,
        command,
    };
    let (status, body) = f
        .http(
            "POST",
            "/v1/warehouses/wh-east/commands",
            "admin-acceptance",
            Some(json!(request)),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    (request, body)
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn transfer_loss_settlement_reversal_and_actual_arrival_keep_each_original_line_and_physical_custody(
) {
    let f = transfer_fixture().await;
    let (_, dispatch) = transfer_at(&f, "wh-main", dispatch_transfer()).await;
    let tid = entity(&dispatch["result"]);
    transfer_at(&f, "wh-east", transfer_arrival(&tid, "a", 2)).await;
    let source = f.state().await;
    let before = f
        .store
        .state(&f.identity.tenant, "wh-east")
        .await
        .expect("destination");
    let (original, accepted) = transfer_admin(&f, transfer_loss(&tid, "a", 1)).await;
    let lid = entity(&accepted["result"]);
    let after = f
        .store
        .state(&f.identity.tenant, "wh-east")
        .await
        .expect("destination");
    assert_eq!(before.stocks, after.stocks);
    assert_eq!(before.movements, after.movements);
    assert_eq!(source, f.state().await);
    let page = transfer_page(&f, "wh-east", 0, 50, false).await;
    let line = &page["items"][0]["data"]["lines"][0];
    assert_eq!(line["quantity"], 3);
    assert_eq!(line["received"], 2);
    assert_eq!(line["lost"], 1);
    assert_eq!(line["loss_record_count"], 1);
    let (_, journal) = f
        .http(
            "GET",
            &format!("/v1/warehouses/wh-east/transfers/{tid}/losses"),
            "viewer-acceptance",
            None,
        )
        .await;
    assert_eq!(journal["page_source_ok"], true);
    assert_eq!(journal["items"][0]["id"], lid);
    assert_eq!(journal["items"][0]["reversed"], 0);
    let bad = Request {
        serial_inputs: Vec::new(),
        unit_inputs: Vec::new(),
        request_id: id(),
        expected_version: after.version,
        resource_versions: None,
        command: transfer_arrival(&tid, "a", 1),
    };
    assert_eq!(
        f.store
            .execute(&f.identity, "wh-east", &bad)
            .await
            .expect_err("settled units cannot also arrive")
            .status,
        422
    );
    for (line, q) in [("b", 2), ("c", 2)] {
        transfer_admin(&f, transfer_loss(&tid, line, q)).await;
    }
    assert_eq!(
        transfer_page(&f, "wh-east", 0, 50, true).await["items"],
        json!([]),
        "settlement closes transit without claiming arrival"
    );
    let before = f
        .store
        .state(&f.identity.tenant, "wh-east")
        .await
        .expect("before reversal");
    let (reverse, rev) = transfer_admin(
        &f,
        Command::ReverseTransferLoss {
            loss_id: lid.clone(),
            quantity: 1,
            reason: "Original missing unit located; reopen pending actual destination receipt"
                .into(),
        },
    )
    .await;
    let after = f
        .store
        .state(&f.identity.tenant, "wh-east")
        .await
        .expect("after reversal");
    assert_eq!(before.stocks, after.stocks);
    assert_eq!(before.movements, after.movements);
    let page = transfer_page(&f, "wh-east", 0, 50, true).await;
    let l = &page["items"][0]["data"]["lines"][0];
    assert_eq!(l["lost"], 0);
    assert_eq!(l["received"], 2);
    assert_eq!(l["loss_record_count"], 2);
    let (_, journal) = f
        .http(
            "GET",
            &format!("/v1/warehouses/wh-east/transfers/{tid}/losses"),
            "viewer-acceptance",
            None,
        )
        .await;
    assert_eq!(journal["items"][0]["reversed"], 1);
    assert_eq!(journal["items"][3]["id"], rev["result"]["entity_id"]);
    transfer_at(&f, "wh-east", transfer_arrival(&tid, "a", 1)).await;
    let current = f
        .store
        .state(&f.identity.tenant, "wh-east")
        .await
        .expect("arrived");
    assert_eq!(current.stocks.iter().map(|s| s.on_hand).sum::<i64>(), 3);
    assert_eq!(source, f.state().await);
    for (request, accepted) in [(original, accepted), (reverse, rev)] {
        let replay = f
            .store
            .execute(&f.identity, "wh-east", &request)
            .await
            .expect("original replay");
        assert_eq!(replay["result"], accepted["result"]);
        assert_eq!(replay["replayed"], true);
    }
    assert_eq!(
        current,
        f.store
            .state(&f.identity.tenant, "wh-east")
            .await
            .expect("unchanged replay")
    );
    let proof = f
        .store
        .reconciliation(&f.identity.tenant, "wh-east")
        .await
        .expect("proof");
    assert_eq!(proof["ok"], true);
    assert_eq!(proof["transfers"][0]["lines"][0]["in_transit"], 0);
    assert_eq!(proof["transfers"][0]["lines"][1]["lost"], 2);
    let (_, trace) = f
        .http(
            "GET",
            "/v1/lot-trace?sku=SKU-A&lot=LOT-A&section=transfers",
            "viewer-acceptance",
            None,
        )
        .await;
    assert_eq!(trace["page_source_ok"], true);
    assert_eq!(trace["items"][1]["document"]["lost"], 2);
    assert_eq!(trace["items"][0]["document"]["loss_record_count"], 2);
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn transfer_loss_requires_destination_admin_exact_scope_caps_and_actor_bound_original_recovery(
) {
    let f = transfer_fixture().await;
    let (_, d) = transfer_at(&f, "wh-main", dispatch_transfer()).await;
    let tid = entity(&d["result"]);
    let version = f
        .store
        .state(&f.identity.tenant, "wh-east")
        .await
        .expect("review")
        .version;
    let original = Request {
        serial_inputs: Vec::new(),
        unit_inputs: Vec::new(),
        request_id: id(),
        expected_version: version,
        resource_versions: None,
        command: transfer_loss(&tid, "a", 2),
    };
    for token in ["operator-acceptance", "viewer-acceptance", "oms-acceptance"] {
        assert_eq!(
            f.http(
                "POST",
                "/v1/warehouses/wh-east/commands",
                token,
                Some(json!(original))
            )
            .await
            .0,
            StatusCode::FORBIDDEN
        );
    }
    for q in [0, -1, 4, 1_000_001] {
        let bad = Request {
            request_id: id(),
            command: transfer_loss(&tid, "a", q),
            ..original.clone()
        };
        assert_eq!(
            f.store
                .execute(&f.identity, "wh-east", &bad)
                .await
                .expect_err("line cap")
                .status,
            422
        );
    }
    let source = Request {
        request_id: id(),
        expected_version: f.state().await.version,
        ..original.clone()
    };
    assert_eq!(
        f.store
            .execute(&f.identity, "wh-main", &source)
            .await
            .expect_err("destination only")
            .status,
        422
    );
    let outsider = Identity {
        tenant: format!("foreign-{}", id()),
        ..f.identity.clone()
    };
    assert_eq!(
        f.store
            .execute(&outsider, "wh-east", &original)
            .await
            .expect_err("foreign scope")
            .status,
        404
    );
    let accepted = f
        .store
        .execute(&f.identity, "wh-east", &original)
        .await
        .expect("admin loss");
    let lid = entity(&accepted["result"]);
    let other = Identity {
        actor: "different:administrator".into(),
        ..f.identity.clone()
    };
    assert_eq!(
        f.store
            .execute(&other, "wh-east", &original)
            .await
            .expect_err("original actor")
            .status,
        409
    );
    let changed = Request {
        command: transfer_loss(&tid, "a", 1),
        ..original.clone()
    };
    assert_eq!(
        f.store
            .execute(&f.identity, "wh-east", &changed)
            .await
            .expect_err("original payload")
            .status,
        409
    );
    let (reverse, _) = transfer_admin(
        &f,
        Command::ReverseTransferLoss {
            loss_id: lid.clone(),
            quantity: 1,
            reason: "One exact unit located".into(),
        },
    )
    .await;
    let bad = Request {
        serial_inputs: Vec::new(),
        unit_inputs: Vec::new(),
        request_id: id(),
        expected_version: reverse.expected_version + 1,
        resource_versions: None,
        command: Command::ReverseTransferLoss {
            loss_id: lid,
            quantity: 2,
            reason: "Cannot borrow another loss allowance".into(),
        },
    };
    assert_eq!(
        f.store
            .execute(&f.identity, "wh-east", &bad)
            .await
            .expect_err("remaining exact loss cap")
            .status,
        422
    );
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn transfer_loss_and_reversal_roll_back_at_final_outbox_failure_then_original_retry_commits_once(
) {
    let f = transfer_fixture().await;
    let (_, d) = transfer_at(&f, "wh-main", dispatch_transfer()).await;
    let tid = entity(&d["result"]);
    let mut lid = String::new();
    for reverse in [false, true] {
        let before = f
            .store
            .state(&f.identity.tenant, "wh-east")
            .await
            .expect("before fault");
        let counts = command_evidence_counts(&f).await;
        let records: i64 =
            sqlx::query_scalar("SELECT count(*) FROM wms_transfer_loss_records WHERE tenant=$1")
                .bind(&f.identity.tenant)
                .fetch_one(&f.store.pool)
                .await
                .expect("record count");
        let original = Request {
            serial_inputs: Vec::new(),
            unit_inputs: Vec::new(),
            request_id: id(),
            expected_version: before.version,
            resource_versions: None,
            command: if reverse {
                Command::ReverseTransferLoss {
                    loss_id: lid.clone(),
                    quantity: 1,
                    reason: "Correct original loss after controller review".into(),
                }
            } else {
                transfer_loss(&tid, "a", 2)
            },
        };
        let trigger = format!("transfer_loss_fault_{}", id().replace('-', ""));
        sqlx::raw_sql(&format!("CREATE FUNCTION {trigger}() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.tenant='{}' THEN RAISE EXCEPTION 'injected loss delivery failure'; END IF; RETURN NEW; END $$; CREATE TRIGGER {trigger} BEFORE INSERT ON wms_outbox FOR EACH ROW EXECUTE FUNCTION {trigger}()",f.identity.tenant)).execute(&f.store.pool).await.expect("install own fault");
        let result = f.store.execute(&f.identity, "wh-east", &original).await;
        sqlx::raw_sql(&format!(
            "DROP TRIGGER {trigger} ON wms_outbox; DROP FUNCTION {trigger}()"
        ))
        .execute(&f.store.pool)
        .await
        .expect("remove own fault");
        assert_eq!(result.expect_err("final failure").status, 503);
        assert_eq!(command_evidence_counts(&f).await, counts);
        assert_eq!(
            before,
            f.store
                .state(&f.identity.tenant, "wh-east")
                .await
                .expect("rollback")
        );
        let after: i64 =
            sqlx::query_scalar("SELECT count(*) FROM wms_transfer_loss_records WHERE tenant=$1")
                .bind(&f.identity.tenant)
                .fetch_one(&f.store.pool)
                .await
                .expect("rolled back record count");
        assert_eq!(after, records);
        let accepted = f
            .store
            .execute(&f.identity, "wh-east", &original)
            .await
            .expect("exact retry");
        lid = entity(&accepted["result"]);
        assert_eq!(
            f.store
                .execute(&f.identity, "wh-east", &original)
                .await
                .expect("committed replay")["result"],
            accepted["result"]
        );
    }
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn transfer_loss_source_proof_rejects_stock_balanced_metadata_forgery_missing_originals_and_event_type_changes(
) {
    let f = transfer_fixture().await;
    let (_, d) = transfer_at(&f, "wh-main", dispatch_transfer()).await;
    let tid = entity(&d["result"]);
    let (_, accepted) = transfer_admin(&f, transfer_loss(&tid, "a", 2)).await;
    let lid = entity(&accepted["result"]);
    for statement in [
        "UPDATE wms_transfer_loss_records SET reason='rewritten' WHERE tenant=$1",
        "DELETE FROM wms_transfer_loss_records WHERE tenant=$1",
    ] {
        assert!(
            sqlx::query(statement)
                .bind(&f.identity.tenant)
                .execute(&f.store.pool)
                .await
                .is_err(),
            "normal history is immutable"
        );
    }
    for (table,sql) in [("wms_transfer_loss_records","UPDATE wms_transfer_loss_records SET quantity=1 WHERE tenant=$1"),("wms_transfer_loss_records","DELETE FROM wms_transfer_loss_records WHERE tenant=$1"),("wms_events","UPDATE wms_events SET payload=jsonb_set(payload,'{type}','\"transfer.received\"') WHERE tenant=$1 AND payload->>'type'='transfer.loss_recorded'")] {
        let mut tx=f.store.pool.begin().await.expect("owned forensic transaction");
        sqlx::raw_sql(&format!("ALTER TABLE {table} DISABLE TRIGGER USER")).execute(&mut *tx).await.expect("forensic bypass only in rollback transaction");
        sqlx::query(sql).bind(&f.identity.tenant).execute(&mut *tx).await.expect("balanced forensic mutation");
        let ok:bool=sqlx::query_scalar("SELECT wms_transfer_consistent($1,$2)").bind(&f.identity.tenant).bind(&tid).fetch_one(&mut *tx).await.expect("source check");assert!(!ok);
        tx.rollback().await.expect("restore original guards and evidence");
    }
    let original: Value = sqlx::query_scalar(
        "SELECT to_jsonb(r) FROM wms_transfer_loss_records r WHERE tenant=$1 AND id=$2",
    )
    .bind(&f.identity.tenant)
    .bind(&lid)
    .fetch_one(&f.store.pool)
    .await
    .expect("retained original");
    assert_eq!(original["quantity"], 2);
    assert_eq!(
        f.store
            .reconciliation(&f.identity.tenant, "wh-east")
            .await
            .expect("unchanged physical proof")["ok"],
        true
    );
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn transfer_loss_and_actual_arrival_compete_for_one_original_in_transit_allowance() {
    let f = transfer_fixture().await;
    let (_, d) = transfer_at(&f, "wh-main", dispatch_transfer()).await;
    let tid = entity(&d["result"]);
    let version = f
        .store
        .state(&f.identity.tenant, "wh-east")
        .await
        .expect("shared review")
        .version;
    let loss = Request {
        serial_inputs: Vec::new(),
        unit_inputs: Vec::new(),
        request_id: id(),
        expected_version: version,
        resource_versions: None,
        command: transfer_loss(&tid, "a", 3),
    };
    let arrival = Request {
        serial_inputs: Vec::new(),
        unit_inputs: Vec::new(),
        request_id: id(),
        expected_version: version,
        resource_versions: None,
        command: transfer_arrival(&tid, "a", 3),
    };
    let (a, b) = tokio::join!(
        f.store.execute(&f.identity, "wh-east", &loss),
        f.store.execute(&f.identity, "wh-east", &arrival)
    );
    assert_eq!(usize::from(a.is_ok()) + usize::from(b.is_ok()), 1);
    assert_eq!(a.err().or(b.err()).expect("one loser").status, 409);
    let proof = f
        .store
        .reconciliation(&f.identity.tenant, "wh-east")
        .await
        .expect("custody proof");
    let l = &proof["transfers"][0]["lines"][0];
    assert_eq!(
        l["received"].as_i64().expect("received") + l["lost"].as_i64().expect("lost"),
        3
    );
    assert_eq!(l["in_transit"], 0);
    assert_eq!(proof["ok"], true);
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn transfer_loss_sql_commit_caps_reject_attributed_oversettlement_and_over_reversal_without_physical_mutation(
) {
    use sha2::{Digest, Sha256};
    let f = transfer_fixture().await;
    let (_, d) = transfer_at(&f, "wh-main", dispatch_transfer()).await;
    let tid = entity(&d["result"]);
    let (_, loss) = transfer_admin(&f, transfer_loss(&tid, "a", 2)).await;
    let lid = entity(&loss["result"]);
    for reverse in [false, true] {
        let original_id = if reverse {
            entity(
                &transfer_admin(
                    &f,
                    Command::ReverseTransferLoss {
                        loss_id: lid.clone(),
                        quantity: 1,
                        reason: "One original missing unit located".into(),
                    },
                )
                .await
                .1["result"],
            )
        } else {
            lid.clone()
        };
        let before = f
            .store
            .state(&f.identity.tenant, "wh-east")
            .await
            .expect("before raw admission");
        let mut record: Value = sqlx::query_scalar(
            "SELECT to_jsonb(r) FROM wms_transfer_loss_records r WHERE tenant=$1 AND id=$2",
        )
        .bind(&f.identity.tenant)
        .bind(&original_id)
        .fetch_one(&f.store.pool)
        .await
        .expect("actual original row");
        let mut event: Value = sqlx::query_scalar("SELECT payload FROM wms_events WHERE id=$1")
            .bind(record["event_id"].as_str().expect("original event"))
            .fetch_one(&f.store.pool)
            .await
            .expect("actual original event");
        let rid = id();
        let eid = id();
        let request_id = id();
        let version = before.version + 1;
        record["id"] = json!(rid);
        record["event_id"] = json!(eid);
        record["version"] = json!(version);
        record["quantity"] = json!(2);
        event["id"] = json!(eid);
        event["version"] = json!(version);
        event["request_id"] = json!(request_id);
        event["command"]["quantity"] = json!(2);
        event["result"] = json!({"entity_id":rid,"version":version,"type":event["type"]});
        let mut document = record.clone();
        document
            .as_object_mut()
            .expect("original object")
            .remove("tenant");
        event["transfer_loss_record"] = document;
        let request = Request {
            serial_inputs: Vec::new(),
            unit_inputs: Vec::new(),
            request_id: request_id.clone(),
            expected_version: before.version,
            resource_versions: None,
            command: serde_json::from_value(event["command"].clone())
                .expect("complete exact command"),
        };
        let digest = format!(
            "{:x}",
            Sha256::digest(serde_json::to_vec(&request).expect("canonical request bytes"))
        );
        let mut tx = f.store.pool.begin().await.expect("raw cap admission");
        sqlx::query("UPDATE wms_warehouses SET version=$3 WHERE tenant=$1 AND warehouse_id=$2")
            .bind(&f.identity.tenant)
            .bind("wh-east")
            .bind(version)
            .execute(&mut *tx)
            .await
            .expect("serialize exact destination");
        sqlx::query("INSERT INTO wms_commands(tenant,warehouse_id,request_id,actor,digest,result) VALUES($1,'wh-east',$2,$3,$4,$5)").bind(&f.identity.tenant).bind(&request_id).bind(&f.identity.actor).bind(digest).bind(&event["result"]).execute(&mut *tx).await.expect("original actor/payload-bound receipt");
        sqlx::query("INSERT INTO wms_events(id,tenant,warehouse_id,version,payload) VALUES($1,$2,'wh-east',$3,$4)").bind(&eid).bind(&f.identity.tenant).bind(version).bind(event).execute(&mut *tx).await.expect("matching new semantic event");
        sqlx::query("INSERT INTO wms_transfer_loss_records SELECT (jsonb_populate_record(NULL::wms_transfer_loss_records,$1)).*").bind(record).execute(&mut *tx).await.expect("provisional matching loss row");
        let original:bool=sqlx::query_scalar("SELECT wms_transfer_loss_original(r) FROM wms_transfer_loss_records r WHERE tenant=$1 AND id=$2").bind(&f.identity.tenant).bind(&rid).fetch_one(&mut *tx).await.expect("separate source proof");
        assert!(
            original,
            "cap rejection must not be a vacuous missing-event failure"
        );
        let valid: bool = sqlx::query_scalar("SELECT wms_transfer_consistent($1,$2)")
            .bind(&f.identity.tenant)
            .bind(&tid)
            .fetch_one(&mut *tx)
            .await
            .expect("separate cap proof");
        assert!(!valid);
        let rejected = sqlx::raw_sql("SET CONSTRAINTS wms_transfer_loss_proof IMMEDIATE")
            .execute(&mut *tx)
            .await
            .expect_err("deferred independent cap");
        assert_eq!(
            rejected
                .as_database_error()
                .and_then(|e| e.code())
                .as_deref(),
            Some("23514")
        );
        assert!(rejected
            .as_database_error()
            .expect("database cap error")
            .message()
            .contains("Transfer lacks exact original"));
        tx.rollback().await.expect("remove rejected raw admission");
        assert_eq!(
            before,
            f.store
                .state(&f.identity.tenant, "wh-east")
                .await
                .expect("raw rollback leaves stock and history unchanged")
        );
    }
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn transfer_loss_journal_pages_bind_destination_changes_even_when_read_from_unchanged_source()
{
    let f = transfer_fixture().await;
    let (_, d) = transfer_at(&f, "wh-main", dispatch_transfer()).await;
    let tid = entity(&d["result"]);
    let (_, loss) = transfer_admin(&f, transfer_loss(&tid, "a", 2)).await;
    transfer_admin(
        &f,
        Command::ReverseTransferLoss {
            loss_id: entity(&loss["result"]),
            quantity: 1,
            reason: "One unit located".into(),
        },
    )
    .await;
    transfer_admin(&f, transfer_loss(&tid, "b", 1)).await;
    let path = format!("/v1/warehouses/wh-main/transfers/{tid}/losses");
    let before = f.state().await;
    let dest_before = f
        .store
        .state(&f.identity.tenant, "wh-east")
        .await
        .expect("destination snapshot");
    let (status, first) = f
        .http("GET", &format!("{path}?limit=1"), "viewer-acceptance", None)
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(first["items"].as_array().expect("page").len(), 1);
    assert_eq!(first["page_source_ok"], true);
    assert_eq!(first["version"], before.version);
    assert_eq!(first["review"], dest_before.version);
    let (status, second) = f
        .http(
            "GET",
            &format!(
                "{path}?limit=1&review={}&after={}",
                first["review"], first["next_cursor"]
            ),
            "viewer-acceptance",
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_ne!(first["items"][0]["id"], second["items"][0]["id"]);
    assert_eq!(second["items"][0]["kind"], "reversal");
    for query in [
        "limit=0",
        "limit=101",
        "after=-1",
        "after=1",
        "review=-1",
        "actor=forged",
        "limit=1&limit=2",
    ] {
        assert_eq!(
            f.http("GET", &format!("{path}?{query}"), "admin-acceptance", None)
                .await
                .0,
            StatusCode::UNPROCESSABLE_ENTITY,
            "{query}"
        );
    }
    assert_eq!(
        f.http("GET", &path, "outsider-acceptance", None).await.0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(before, f.state().await);
    assert_eq!(
        dest_before,
        f.store
            .state(&f.identity.tenant, "wh-east")
            .await
            .expect("reads are not writes")
    );
    transfer_admin(&f, transfer_loss(&tid, "c", 1)).await;
    assert_eq!(
        before,
        f.state().await,
        "destination loss cannot change source stock or version"
    );
    let (status, stale) = f
        .http(
            "GET",
            &format!(
                "{path}?limit=1&review={}&after={}",
                first["review"], first["next_cursor"]
            ),
            "viewer-acceptance",
            None,
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert!(
        stale.get("command_outcome").is_none(),
        "read conflicts cannot retire write journals"
    );
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn transfer_loss_upgrade_preserves_actual18_transfer_custody_and_rejects_inconsistent_originals(
) {
    let f = transfer_fixture().await;
    let (_, d) = transfer_at(&f, "wh-main", dispatch_transfer()).await;
    let tid = entity(&d["result"]);
    transfer_at(&f, "wh-east", transfer_arrival(&tid, "a", 1)).await;
    for damaged in [false, true] {
        let mut tx = f
            .store
            .pool
            .begin()
            .await
            .expect("owned upgrade transaction");
        let schema = format!("loss_upgrade_{}", id().replace('-', "_"));
        sqlx::raw_sql(&format!(
            "CREATE SCHEMA {schema}; SET LOCAL search_path TO {schema},public"
        ))
        .execute(&mut *tx)
        .await
        .expect("owned actual18 schema");
        let mut files: Vec<_> =
            std::fs::read_dir(concat!(env!("CARGO_MANIFEST_DIR"), "/migrations"))
                .expect("migration sources")
                .map(|e| e.expect("migration entry").path())
                .filter(|p| {
                    p.extension().is_some_and(|s| s == "sql")
                        && p.file_name()
                            .expect("migration filename")
                            .to_string_lossy()
                            .as_ref()
                            < "019_"
                })
                .collect();
        files.sort();
        assert_eq!(files.len(), 18);
        for p in files {
            sqlx::raw_sql(&std::fs::read_to_string(p).expect("actual previous migration"))
                .execute(&mut *tx)
                .await
                .expect("apply actual18 contract");
        }
        // Copy compatible original dispatch/arrival facts into the real previous
        // schema. Session-local trigger bypass avoids rebuilding derived rows
        // twice; every copied table is checked by the previous/current proofs.
        sqlx::raw_sql("SET LOCAL session_replication_role='replica'")
            .execute(&mut *tx)
            .await
            .expect("own snapshot copy");
        let tables:Vec<String>=sqlx::query_scalar("SELECT tablename FROM pg_tables p WHERE schemaname=$1 AND EXISTS(SELECT 1 FROM information_schema.columns c WHERE c.table_schema=p.schemaname AND c.table_name=p.tablename AND c.column_name='tenant') ORDER BY tablename").bind(&schema).fetch_all(&mut *tx).await.expect("actual previous tables");
        // The 18 migrations create 39 application tables; Store owns the
        // separate migration ledger. 38 application tables carry tenant keys;
        // delivery attempts have no tenant column and this fixture has none.
        assert_eq!(tables.len(), 38);
        let application_tables: i64 =
            sqlx::query_scalar("SELECT count(*) FROM pg_tables WHERE schemaname=$1")
                .bind(&schema)
                .fetch_one(&mut *tx)
                .await
                .expect("actual application schema");
        assert_eq!(application_tables, 39);
        for table in tables {
            assert!(table.bytes().all(|c| c.is_ascii_lowercase() || c == b'_'));
            sqlx::query(&format!(
                "INSERT INTO {table} SELECT * FROM public.{table} WHERE tenant=$1"
            ))
            .bind(&f.identity.tenant)
            .execute(&mut *tx)
            .await
            .expect("copy original facts without inventing records");
        }
        if damaged {
            sqlx::query("UPDATE wms_events SET payload=jsonb_set(payload,'{command,carrier}','\"FORGED\"') WHERE tenant=$1 AND payload->>'type'='transfer.dispatched'").bind(&f.identity.tenant).execute(&mut *tx).await.expect("preceding source corruption");
        }
        sqlx::raw_sql("SET LOCAL session_replication_role='origin'")
            .execute(&mut *tx)
            .await
            .expect("restore normal guards");
        let before:Value=sqlx::query_scalar("SELECT jsonb_build_object('events',(SELECT jsonb_agg(to_jsonb(e) ORDER BY id) FROM wms_events e),'movements',(SELECT jsonb_agg(to_jsonb(m) ORDER BY id) FROM wms_movements m),'stock',(SELECT jsonb_agg(to_jsonb(s) ORDER BY warehouse_id,sku,lot,location) FROM wms_stock s))").fetch_one(&mut *tx).await.expect("original evidence image");
        let upgraded = sqlx::raw_sql(include_str!("../migrations/019_transfer_loss.sql"))
            .execute(&mut *tx)
            .await;
        if damaged {
            assert!(
                upgraded.is_err(),
                "upgrade must not bless an inconsistent original dispatch"
            );
        } else {
            upgraded.expect("valid actual18 upgrade");
            let after:Value=sqlx::query_scalar("SELECT jsonb_build_object('events',(SELECT jsonb_agg(to_jsonb(e) ORDER BY id) FROM wms_events e),'movements',(SELECT jsonb_agg(to_jsonb(m) ORDER BY id) FROM wms_movements m),'stock',(SELECT jsonb_agg(to_jsonb(s) ORDER BY warehouse_id,sku,lot,location) FROM wms_stock s))").fetch_one(&mut *tx).await.expect("post-upgrade evidence image");
            assert_eq!(before, after);
            let count: i64 = sqlx::query_scalar("SELECT count(*) FROM wms_transfer_loss_records")
                .fetch_one(&mut *tx)
                .await
                .expect("no invented loss");
            assert_eq!(count, 0);
            let proof: bool = sqlx::query_scalar("SELECT wms_transfer_consistent($1,$2)")
                .bind(&f.identity.tenant)
                .bind(&tid)
                .fetch_one(&mut *tx)
                .await
                .expect("new proof retains original custody");
            assert!(proof);
            let document: Value = sqlx::query_scalar("SELECT wms_transfer_document($1,$2)")
                .bind(&f.identity.tenant)
                .bind(&tid)
                .fetch_one(&mut *tx)
                .await
                .expect("upgraded document");
            assert_eq!(document["lines"][0]["received"], 1);
            assert_eq!(document["lines"][0]["lost"], 0);
        }
        tx.rollback()
            .await
            .expect("remove only owned upgrade schema and copied fixture");
    }
}
async fn transfer_page(f: &Fixture, w: &str, after: i64, limit: i64, open_only: bool) -> Value {
    let (s, v) = f
        .http(
            "GET",
            &format!(
                "/v1/warehouses/{w}/transfers?after={after}&limit={limit}&open_only={open_only}"
            ),
            "operator-acceptance",
            None,
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    v
}
#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn facility_transfer_dispatch_partial_arrival_and_original_recovery_preserve_exact_custody() {
    let f = transfer_fixture().await;
    let order = f
        .command(Command::CreateOrder {
            reference: "RESERVED-LOCAL".into(),
            source_order_id: None,
            customer: "Local customer".into(),
            destination: "Local".into(),
            priority: "normal".into(),
            lines: vec![LineInput {
                id: "local".into(),
                sku: "SKU-A".into(),
                quantity: 3,
            }],
        })
        .await;
    f.command(Command::Allocate {
        order_id: entity(&order),
    })
    .await;
    let dest_before = f
        .store
        .state(&f.identity.tenant, "wh-east")
        .await
        .expect("destination");
    let (dispatch, accepted) = transfer_at(&f, "wh-main", dispatch_transfer()).await;
    let tid = entity(&accepted["result"]);
    assert_eq!(
        f.store
            .state(&f.identity.tenant, "wh-east")
            .await
            .expect("transfer qualification"),
        dest_before,
        "dispatch creates no destination stock or work"
    );
    let source = f.state().await;
    assert_eq!(source.stocks.iter().map(|s| s.on_hand).sum::<i64>(), 7);
    assert_eq!(source.stocks.iter().map(|s| s.reserved).sum::<i64>(), 3);
    let page = transfer_page(&f, "wh-east", 0, 50, true).await;
    let original = &page["items"][0]["data"];
    assert_eq!(original["ok"], true);
    assert_eq!(original["lines"][0]["received"], 0);
    let (arrival, arrived) = transfer_at(&f, "wh-east", transfer_arrival(&tid, "a", 2)).await;
    let dest = f
        .store
        .state(&f.identity.tenant, "wh-east")
        .await
        .expect("transfer qualification");
    assert_eq!(dest.stocks[0].on_hand, 2);
    assert_eq!(dest.stocks[0].lot, "LOT-A");
    let bad = Request {
        serial_inputs: Vec::new(),
        unit_inputs: Vec::new(),
        request_id: id(),
        expected_version: dest.version,
        resource_versions: None,
        command: transfer_arrival(&tid, "a", 2),
    };
    assert_eq!(
        f.store
            .execute(&f.identity, "wh-east", &bad)
            .await
            .expect_err("transfer must be rejected")
            .status,
        422,
        "one same-SKU line cannot borrow another's allowance"
    );
    transfer_at(&f, "wh-east", transfer_arrival(&tid, "b", 2)).await;
    for (w, r, original) in [
        ("wh-main", dispatch, accepted),
        ("wh-east", arrival, arrived),
    ] {
        let before = f
            .store
            .state(&f.identity.tenant, w)
            .await
            .expect("transfer qualification");
        let replay = f
            .store
            .execute(&f.identity, w, &r)
            .await
            .expect("transfer qualification");
        assert_eq!(replay["replayed"], true);
        assert_eq!(replay["result"], original["result"]);
        assert_eq!(
            f.store
                .state(&f.identity.tenant, w)
                .await
                .expect("transfer qualification"),
            before
        );
    }
    for w in ["wh-main", "wh-east"] {
        let proof = f
            .store
            .reconciliation(&f.identity.tenant, w)
            .await
            .expect("transfer qualification");
        assert_eq!(proof["ok"], true, "{proof}");
        assert_eq!(proof["transfers"][0]["lines"][0]["in_transit"], 1);
        assert_eq!(proof["transfers"][0]["lines"][1]["in_transit"], 0);
        assert_eq!(proof["transfers"][0]["lines"][2]["in_transit"], 2);
    }
    transfer_at(&f, "wh-east", transfer_arrival(&tid, "a", 1)).await;
    transfer_at(&f, "wh-east", transfer_arrival(&tid, "c", 2)).await;
    assert_eq!(
        transfer_page(&f, "wh-east", 0, 50, true).await["items"],
        json!([])
    );
    assert_eq!(
        transfer_page(&f, "wh-east", 0, 50, false).await["items"][0]["data"]["lines"][2]
            ["received"],
        2
    );
    assert_eq!(
        f.store
            .state(&f.identity.tenant, "wh-east")
            .await
            .expect("transfer qualification")
            .stocks
            .iter()
            .map(|s| s.on_hand)
            .sum::<i64>(),
        7
    );
    let events: Vec<Value> = sqlx::query_scalar(
        "SELECT payload FROM wms_events WHERE tenant=$1 AND payload->>'type' LIKE 'transfer.%'",
    )
    .bind(&f.identity.tenant)
    .fetch_all(&f.store.pool)
    .await
    .expect("transfer qualification");
    assert_eq!(events.len(), 5);
    assert!(events
        .iter()
        .all(|e| e["order"].is_null() && e["transfer"]["id"] == tid));
}
#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn facility_transfer_roles_scope_reservations_and_invalid_dispatch_have_no_side_effects() {
    let f = transfer_fixture().await;
    for token in ["viewer-acceptance", "oms-acceptance", "outsider-acceptance"] {
        let r = Request {
            serial_inputs: Vec::new(),
            unit_inputs: Vec::new(),
            request_id: id(),
            expected_version: f.state().await.version,
            resource_versions: None,
            command: dispatch_transfer(),
        };
        let (status, _) = f
            .http(
                "POST",
                "/v1/warehouses/wh-main/commands",
                token,
                Some(json!(r)),
            )
            .await;
        assert_eq!(
            status,
            if token == "outsider-acceptance" {
                StatusCode::NOT_FOUND
            } else {
                StatusCode::FORBIDDEN
            }
        );
    }
    let before = f.state().await;
    let counts = command_evidence_counts(&f).await;
    let mut invalid = dispatch_transfer();
    if let Command::DispatchTransfer { lines, .. } = &mut invalid {
        lines[0].quantity = 9;
        lines[1].quantity = 9;
    }
    f.reject(invalid, StatusCode::UNPROCESSABLE_ENTITY).await;
    let mut missing = dispatch_transfer();
    if let Command::DispatchTransfer { lines, .. } = &mut missing {
        lines[0].sku = "UNCONFIGURED".into();
    }
    f.reject(missing, StatusCode::UNPROCESSABLE_ENTITY).await;
    assert_eq!(f.state().await, before);
    assert_eq!(command_evidence_counts(&f).await, counts);
    let (_, d) = transfer_at(&f, "wh-main", dispatch_transfer()).await;
    let tid = entity(&d["result"]);
    let wrong = Request {
        serial_inputs: Vec::new(),
        unit_inputs: Vec::new(),
        request_id: id(),
        expected_version: f.state().await.version,
        resource_versions: None,
        command: transfer_arrival(&tid, "a", 1),
    };
    assert_eq!(
        f.store
            .execute(&f.identity, "wh-main", &wrong)
            .await
            .expect_err("transfer must be rejected")
            .status,
        422
    );
    let (status, _) = f
        .http(
            "GET",
            "/v1/warehouses/wh-east/transfers",
            "outsider-acceptance",
            None,
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}
#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn facility_transfer_final_outbox_failure_rolls_back_dispatch_and_arrival_then_exact_retry_once(
) {
    let f = transfer_fixture().await;
    let mut tid = String::new();
    for w in ["wh-main", "wh-east"] {
        let before = f
            .store
            .state(&f.identity.tenant, w)
            .await
            .expect("transfer qualification");
        let counts = command_evidence_counts(&f).await;
        let request = Request {
            serial_inputs: Vec::new(),
            unit_inputs: Vec::new(),
            request_id: id(),
            expected_version: before.version,
            resource_versions: None,
            command: if w == "wh-main" {
                dispatch_transfer()
            } else {
                transfer_arrival(&tid, "a", 2)
            },
        };
        let trigger = format!("transfer_fault_{}", id().replace('-', ""));
        sqlx::raw_sql(&format!("CREATE FUNCTION {trigger}() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.tenant='{}' THEN RAISE EXCEPTION 'injected transfer delivery failure'; END IF; RETURN NEW; END $$; CREATE TRIGGER {trigger} BEFORE INSERT ON wms_outbox FOR EACH ROW EXECUTE FUNCTION {trigger}()",f.identity.tenant)).execute(&f.store.pool).await.expect("transfer qualification");
        let failed = f.store.execute(&f.identity, w, &request).await;
        sqlx::raw_sql(&format!(
            "DROP TRIGGER {trigger} ON wms_outbox; DROP FUNCTION {trigger}()"
        ))
        .execute(&f.store.pool)
        .await
        .expect("transfer qualification");
        assert_eq!(failed.expect_err("transfer must be rejected").status, 503);
        assert_eq!(
            f.store
                .state(&f.identity.tenant, w)
                .await
                .expect("transfer qualification"),
            before
        );
        assert_eq!(command_evidence_counts(&f).await, counts);
        let accepted = f
            .store
            .execute(&f.identity, w, &request)
            .await
            .expect("transfer qualification");
        tid = entity(&accepted["result"]);
        let after = f
            .store
            .state(&f.identity.tenant, w)
            .await
            .expect("transfer qualification");
        let replay = f
            .store
            .execute(&f.identity, w, &request)
            .await
            .expect("transfer qualification");
        assert_eq!(replay["result"], accepted["result"]);
        assert_eq!(replay["replayed"], true);
        assert_eq!(
            f.store
                .state(&f.identity.tenant, w)
                .await
                .expect("transfer qualification"),
            after
        );
    }
    assert_eq!(
        transfer_page(&f, "wh-east", 0, 50, true).await["items"][0]["data"]["lines"][0]["received"],
        2
    );
}
#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn facility_transfer_competing_arrivals_and_stale_repeatable_read_cannot_overreceive() {
    let f = transfer_fixture().await;
    let (_, d) = transfer_at(&f, "wh-main", dispatch_transfer()).await;
    let tid = entity(&d["result"]);
    let version = f
        .store
        .state(&f.identity.tenant, "wh-east")
        .await
        .expect("transfer qualification")
        .version;
    let a = Request {
        serial_inputs: Vec::new(),
        unit_inputs: Vec::new(),
        request_id: id(),
        expected_version: version,
        resource_versions: None,
        command: transfer_arrival(&tid, "a", 2),
    };
    let b = Request {
        request_id: id(),
        ..a.clone()
    };
    let mut stale = f.store.pool.begin().await.expect("transfer qualification");
    sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ")
        .execute(&mut *stale)
        .await
        .expect("transfer qualification");
    let _: Value = sqlx::query_scalar("SELECT wms_transfer_document($1,$2)")
        .bind(&f.identity.tenant)
        .bind(&tid)
        .fetch_one(&mut *stale)
        .await
        .expect("transfer qualification");
    let (ra, rb) = tokio::join!(
        f.store.execute(&f.identity, "wh-east", &a),
        f.store.execute(&f.identity, "wh-east", &b)
    );
    assert_eq!(usize::from(ra.is_ok()) + usize::from(rb.is_ok()), 1);
    let error = sqlx::query("SELECT wms_serialize_shipment($1,'wh-east')")
        .bind(&f.identity.tenant)
        .execute(&mut *stale)
        .await
        .expect_err("transfer must be rejected");
    assert_eq!(
        error.as_database_error().and_then(|e| e.code()).as_deref(),
        Some("40001")
    );
    stale.rollback().await.expect("transfer qualification");
    let page = transfer_page(&f, "wh-east", 0, 50, true).await;
    assert_eq!(page["items"][0]["data"]["lines"][0]["received"], 2);
    let remaining = f
        .store
        .state(&f.identity.tenant, "wh-east")
        .await
        .expect("transfer qualification")
        .version;
    let over = Request {
        request_id: id(),
        expected_version: remaining,
        command: transfer_arrival(&tid, "a", 2),
        ..a
    };
    assert_eq!(
        f.store
            .execute(&f.identity, "wh-east", &over)
            .await
            .expect_err("transfer must be rejected")
            .status,
        422
    );
}
#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn facility_transfer_original_rows_are_immutable_and_closed_history_is_paged() {
    let f = transfer_fixture().await;
    let (_, d) = transfer_at(&f, "wh-main", dispatch_transfer()).await;
    let tid = entity(&d["result"]);
    for query in [
        "UPDATE wms_transfers SET carrier='replacement' WHERE tenant=$1",
        "DELETE FROM wms_transfer_lines WHERE tenant=$1",
        "UPDATE wms_transfer_lines SET quantity=quantity+1 WHERE tenant=$1",
    ] {
        assert!(sqlx::query(query)
            .bind(&f.identity.tenant)
            .execute(&f.store.pool)
            .await
            .is_err());
    }
    let mut second = dispatch_transfer();
    if let Command::DispatchTransfer {
        reference, lines, ..
    } = &mut second
    {
        *reference = "SECOND-HANDOFF".into();
        lines.truncate(1);
        lines[0].quantity = 1;
    }
    transfer_at(&f, "wh-main", second).await;
    let first = transfer_page(&f, "wh-east", 0, 1, false).await;
    assert_eq!(first["items"][0]["data"]["id"], tid);
    let cursor = first["next_cursor"]
        .as_i64()
        .expect("transfer qualification");
    let second = transfer_page(&f, "wh-east", cursor, 1, false).await;
    assert_eq!(
        second["items"]
            .as_array()
            .expect("transfer qualification")
            .len(),
        1
    );
    assert_ne!(second["items"][0]["data"]["id"], tid);
    assert!(second["next_cursor"].is_null());
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn facility_transfer_sql_rejects_stock_balanced_overarrival_with_a_forged_new_event() {
    use sqlx::Row;
    let f = transfer_fixture().await;
    let (_, d) = transfer_at(&f, "wh-main", dispatch_transfer()).await;
    let tid = entity(&d["result"]);
    transfer_at(&f, "wh-east", transfer_arrival(&tid, "a", 2)).await;
    let before = f
        .store
        .state(&f.identity.tenant, "wh-east")
        .await
        .expect("transfer qualification");
    let original=sqlx::query("SELECT m.data,e.payload FROM wms_movements m JOIN wms_events e ON e.id=m.transfer_event_id WHERE m.tenant=$1 AND m.transfer_id=$2 AND m.kind='transfer_receive'").bind(&f.identity.tenant).bind(&tid).fetch_one(&f.store.pool).await.expect("transfer qualification");
    let mut data: Value = original.get("data");
    let mut event: Value = original.get("payload");
    let eid = id();
    let mid = id();
    data["id"] = json!(mid);
    data["transfer_event_id"] = json!(eid);
    data["quantity"] = json!(2);
    event["id"] = json!(eid);
    event["version"] = json!(before.version + 1);
    event["request_id"] = json!(id());
    event["result"]["version"] = json!(before.version + 1);
    let mut tx = f.store.pool.begin().await.expect("transfer qualification");
    sqlx::query(
        "UPDATE wms_warehouses SET version=version+1 WHERE tenant=$1 AND warehouse_id='wh-east'",
    )
    .bind(&f.identity.tenant)
    .execute(&mut *tx)
    .await
    .expect("transfer qualification");
    sqlx::query("UPDATE wms_stock SET on_hand=on_hand+2 WHERE tenant=$1 AND warehouse_id='wh-east' AND sku='SKU-A' AND lot='LOT-A' AND location='RECV-01'").bind(&f.identity.tenant).execute(&mut *tx).await.expect("transfer qualification");
    sqlx::query("INSERT INTO wms_events(id,tenant,warehouse_id,version,payload) VALUES($1,$2,'wh-east',$3,$4)").bind(&eid).bind(&f.identity.tenant).bind(before.version+1).bind(&event).execute(&mut *tx).await.expect("transfer qualification");
    sqlx::query("INSERT INTO wms_movements(id,tenant,warehouse_id,sku,lot,from_location,to_location,quantity,kind,reference,data,transfer_id,transfer_line_id,transfer_event_id) VALUES($1,$2,'wh-east','SKU-A','LOT-A',NULL,'RECV-01',2,'transfer_receive',$3,$4,$3,'a',$5)").bind(&mid).bind(&f.identity.tenant).bind(&tid).bind(data).bind(&eid).execute(&mut *tx).await.expect("transfer qualification");
    let consistent: bool = sqlx::query_scalar("SELECT wms_transfer_consistent($1,$2)")
        .bind(&f.identity.tenant)
        .bind(&tid)
        .fetch_one(&mut *tx)
        .await
        .expect("transfer qualification");
    assert!(!consistent);
    let err = tx.commit().await.expect_err("transfer must be rejected");
    assert_eq!(
        err.as_database_error().and_then(|e| e.code()).as_deref(),
        Some("23514")
    );
    assert_eq!(
        f.store
            .state(&f.identity.tenant, "wh-east")
            .await
            .expect("transfer qualification"),
        before
    );
}
#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn facility_transfer_opposite_dispatches_lock_facilities_in_one_order_and_reject_reserved_stock(
) {
    let f = transfer_fixture().await;
    let version = f
        .store
        .state(&f.identity.tenant, "wh-east")
        .await
        .expect("transfer qualification")
        .version;
    let location = Request {
        serial_inputs: Vec::new(),
        unit_inputs: Vec::new(),
        request_id: id(),
        expected_version: version,
        resource_versions: None,
        command: Command::CreateLocation {
            code: "A-01".into(),
            zone: "Storage".into(),
            kind: "storage".into(),
        },
    };
    f.store
        .execute(&f.identity, "wh-east", &location)
        .await
        .expect("transfer qualification");
    let count = Request {
        serial_inputs: Vec::new(),
        unit_inputs: Vec::new(),
        request_id: id(),
        expected_version: version + 1,
        resource_versions: None,
        command: opening_count(10),
    };
    f.store
        .execute(&f.identity, "wh-east", &count)
        .await
        .expect("transfer qualification");
    let request_a = Request {
        serial_inputs: Vec::new(),
        unit_inputs: Vec::new(),
        request_id: id(),
        expected_version: f.state().await.version,
        resource_versions: None,
        command: dispatch_transfer(),
    };
    let mut reverse = dispatch_transfer();
    if let Command::DispatchTransfer {
        destination_warehouse_id,
        lines,
        ..
    } = &mut reverse
    {
        *destination_warehouse_id = "wh-main".into();
        lines.truncate(1);
    }
    let request_b = Request {
        serial_inputs: Vec::new(),
        unit_inputs: Vec::new(),
        request_id: id(),
        expected_version: version + 2,
        resource_versions: None,
        command: reverse,
    };
    let (a, b) = tokio::join!(
        f.store.execute(&f.identity, "wh-main", &request_a),
        f.store.execute(&f.identity, "wh-east", &request_b)
    );
    assert!(a.is_ok() && b.is_ok(), "{a:?} {b:?}");
    for w in ["wh-main", "wh-east"] {
        assert_eq!(
            f.store
                .reconciliation(&f.identity.tenant, w)
                .await
                .expect("transfer qualification")["ok"],
            true
        );
    }
    let order = f
        .command(Command::CreateOrder {
            reference: "RESERVE-REMAINDER".into(),
            source_order_id: None,
            customer: "Local".into(),
            destination: "Local".into(),
            priority: "normal".into(),
            lines: vec![LineInput {
                id: "reserved".into(),
                sku: "SKU-A".into(),
                quantity: 5,
            }],
        })
        .await;
    f.command(Command::Allocate {
        order_id: entity(&order),
    })
    .await;
    let mut reserved = dispatch_transfer();
    if let Command::DispatchTransfer {
        reference, lines, ..
    } = &mut reserved
    {
        *reference = "RESERVED-DISPATCH".into();
        lines.truncate(1);
        lines[0].quantity = 1;
    }
    f.reject(reserved, StatusCode::UNPROCESSABLE_ENTITY).await;
}

async fn split_source(f: &Fixture) -> String {
    f.seed_catalog().await;
    f.command(opening_count(7)).await;
    let created = f
        .command(Command::CreateOrder {
            reference: "SPLIT-ORDER".into(),
            source_order_id: None,
            customer: "Partial customer".into(),
            destination: "Verified destination".into(),
            priority: "normal".into(),
            lines: vec![
                LineInput {
                    id: "a".into(),
                    sku: "SKU-A".into(),
                    quantity: 3,
                },
                LineInput {
                    id: "b".into(),
                    sku: "SKU-A".into(),
                    quantity: 5,
                },
            ],
        })
        .await;
    let order = entity(&created);
    f.command(Command::AllocateAvailable {
        order_id: order.clone(),
    })
    .await;
    order
}
async fn split_pick(f: &Fixture, order: &str, line: &str, q: i64) {
    f.command(Command::Pick {
        order_id: order.into(),
        line_id: line.into(),
        location: "A-01".into(),
        lot: "LOT-A".into(),
        quantity: q,
    })
    .await;
}
async fn split_pack(f: &Fixture, order: &str, reference: &str, line: &str, q: i64) -> String {
    entity(
        &f.command(Command::PackShipment {
            order_id: order.into(),
            reference: reference.into(),
            lines: vec![stateset_warehouse::split_fulfillment::LineInput {
                line_id: line.into(),
                location: "A-01".into(),
                lot: "LOT-A".into(),
                quantity: q,
            }],
        })
        .await,
    )
}
async fn split_handoff(f: &Fixture, package: &str, carrier: &str) {
    f.command(Command::HandoffShipment {
        shipment_id: package.into(),
        carrier: carrier.into(),
        handoff_reference: format!("HANDOFF-{carrier}"),
    })
    .await;
}
#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn split_fulfillment_backorders_handoff_returns_and_multiple_carriers_preserve_original_quantities(
) {
    let f = Fixture::new().await;
    let oid = split_source(&f).await;
    let state = f.state().await;
    let order = state.orders.iter().find(|o| o.id == oid).expect("order");
    assert_eq!(order.status, "allocated");
    assert_eq!(order.allocations.iter().map(|a| a.quantity).sum::<i64>(), 7);
    let (status, progress) = f
        .http(
            "GET",
            &format!("/v1/warehouses/wh-main/orders/{oid}/shipments"),
            "operator-acceptance",
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{progress}");
    assert_eq!(progress["progress"][1]["unallocated"], 1);
    split_pick(&f, &oid, "a", 3).await;
    let package = split_pack(&f, &oid, "PACKAGE-A", "a", 3).await;
    split_handoff(&f, &package, "Carrier-A").await;
    let state = f.state().await;
    assert_eq!(state.orders[0].status, "partially_shipped");
    assert_eq!(state.stocks.iter().map(|b| b.on_hand).sum::<i64>(), 4);
    let (status, allowance) = f
        .http(
            "GET",
            &format!("/v1/warehouses/wh-main/orders/{oid}/return-eligibility"),
            "operator-acceptance",
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{allowance}");
    assert_eq!(allowance["lines"][0]["shipped"], 3);
    assert_eq!(allowance["lines"].as_array().expect("lines").len(), 1);
    assert_eq!(allowance["shipment_source"]["kind"], "handoff_events");
    f.command(Command::CreateReturn {
        reference: "PARTIAL-RMA".into(),
        order_id: oid.clone(),
        reason: "One original unit returned before remaining order ships".into(),
        lines: vec![stateset_warehouse::returns::ReturnLineInput {
            id: "return-a".into(),
            order_line_id: "a".into(),
            lot: "LOT-A".into(),
            quantity: 1,
        }],
    })
    .await;
    let (status, held) = f
        .http(
            "POST",
            &format!("/v1/warehouses/wh-main/orders/{oid}/oms-fulfillment"),
            "operator-acceptance",
            Some(json!({"request_id":id(),"expected_version":f.state().await.version})),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{held}");
    f.command(Command::CountStock {
        sku: "SKU-A".into(),
        lot: "LOT-A".into(),
        location: "A-01".into(),
        counted: 5,
        reason: "Actual last backordered unit arrived".into(),
    })
    .await;
    f.command(Command::AllocateAvailable {
        order_id: oid.clone(),
    })
    .await;
    split_pick(&f, &oid, "b", 5).await;
    let second = split_pack(&f, &oid, "PACKAGE-B", "b", 5).await;
    split_handoff(&f, &second, "Carrier-B").await;
    let state = f.state().await;
    let order = &state.orders[0];
    assert_eq!(order.status, "shipped");
    assert_eq!(order.carrier, "");
    assert_eq!(order.handoff_reference, "");
    assert_eq!(order.allocations.iter().map(|a| a.shipped).sum::<i64>(), 8);
    let proof = f
        .store
        .reconciliation(&f.identity.tenant, "wh-main")
        .await
        .expect("full proof");
    assert_eq!(proof["ok"], true, "{proof}");
    assert_eq!(
        proof["packages"][0]["packages"]
            .as_array()
            .expect("packages")
            .len(),
        2
    );
    let (status, held) = f
        .http(
            "POST",
            &format!("/v1/warehouses/wh-main/orders/{oid}/oms-fulfillment"),
            "operator-acceptance",
            Some(json!({"request_id":id(),"expected_version":state.version})),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{held}");
    assert!(held["error"]
        .as_str()
        .expect("error")
        .contains("multiple shipment handoffs"));
    let n: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM wms_native_requests WHERE tenant=$1 AND order_id=$2",
    )
    .bind(&f.identity.tenant)
    .bind(&oid)
    .fetch_one(&f.store.pool)
    .await
    .expect("no native intent");
    assert_eq!(n, 0);
    let (status, allowance) = f
        .http(
            "GET",
            &format!("/v1/warehouses/wh-main/orders/{oid}/return-eligibility"),
            "operator-acceptance",
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{allowance}");
    assert_eq!(allowance["lines"][0]["remaining"], 2);
    assert_eq!(allowance["lines"][1]["shipped"], 5);
    assert_eq!(
        allowance["shipment_source"]["event_ids"]
            .as_array()
            .expect("events")
            .len(),
        2
    );
}
#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn split_fulfillment_packed_claims_require_unpack_before_unpick_and_preserve_reservation_recovery(
) {
    let f = Fixture::new().await;
    let oid = split_source(&f).await;
    split_pick(&f, &oid, "a", 3).await;
    let p = split_pack(&f, &oid, "PACKED-CLAIM", "a", 2).await;
    f.reject(
        Command::Unpick {
            order_id: oid.clone(),
            line_id: "a".into(),
            location: "A-01".into(),
            lot: "LOT-A".into(),
            quantity: 2,
            reason: "Cannot steal packed claim".into(),
        },
        StatusCode::UNPROCESSABLE_ENTITY,
    )
    .await;
    f.reject(
        Command::PackShipment {
            order_id: oid.clone(),
            reference: "OVERPACK".into(),
            lines: vec![stateset_warehouse::split_fulfillment::LineInput {
                line_id: "a".into(),
                location: "A-01".into(),
                lot: "LOT-A".into(),
                quantity: 2,
            }],
        },
        StatusCode::UNPROCESSABLE_ENTITY,
    )
    .await;
    f.command(Command::UnpackShipment {
        shipment_id: p.clone(),
        reason: "Observed packing defect; goods remain staged".into(),
    })
    .await;
    f.command(Command::Unpick {
        order_id: oid.clone(),
        line_id: "a".into(),
        location: "A-01".into(),
        lot: "LOT-A".into(),
        quantity: 1,
        reason: "Physically returned one staged unit".into(),
    })
    .await;
    f.command(Command::ReleaseUnpicked {
        order_id: oid.clone(),
        reason: "Release only unpicked reservations for review".into(),
    })
    .await;
    let state = f.state().await;
    assert_eq!(state.stocks.iter().map(|b| b.reserved).sum::<i64>(), 2);
    assert_eq!(
        state.orders[0]
            .allocations
            .iter()
            .map(|a| a.shipped)
            .sum::<i64>(),
        0
    );
    f.command(Command::AllocateAvailable {
        order_id: oid.clone(),
    })
    .await;
    let q = split_pack(&f, &oid, "REPACKED-CLAIM", "a", 2).await;
    split_handoff(&f, &q, "Carrier-new").await;
    f.reject(
        Command::HandoffShipment {
            shipment_id: p,
            carrier: "Other".into(),
            handoff_reference: "Cannot hand off unpacked claim".into(),
        },
        StatusCode::UNPROCESSABLE_ENTITY,
    )
    .await;
    let proof = f
        .store
        .reconciliation(&f.identity.tenant, "wh-main")
        .await
        .expect("proof");
    assert_eq!(proof["ok"], true, "{proof}");
}
#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn split_fulfillment_single_complete_package_keeps_actual_carrier_and_one_event_provenance() {
    let f = Fixture::new().await;
    f.seed_catalog().await;
    f.command(opening_count(3)).await;
    let o = entity(
        &f.command(Command::CreateOrder {
            reference: "ONE-PACKAGE-ORDER".into(),
            source_order_id: None,
            customer: "Customer".into(),
            destination: "Destination".into(),
            priority: "normal".into(),
            lines: vec![LineInput {
                id: "a".into(),
                sku: "SKU-A".into(),
                quantity: 3,
            }],
        })
        .await,
    );
    f.command(Command::AllocateAvailable {
        order_id: o.clone(),
    })
    .await;
    split_pick(&f, &o, "a", 3).await;
    let p = split_pack(&f, &o, "ONE-PACKAGE", "a", 3).await;
    let before = f.state().await;
    let request = Request {
        serial_inputs: Vec::new(),
        unit_inputs: Vec::new(),
        request_id: id(),
        expected_version: before.version,
        resource_versions: None,
        command: Command::HandoffShipment {
            shipment_id: p,
            carrier: "Exact carrier".into(),
            handoff_reference: "Exact original handoff".into(),
        },
    };
    let accepted = f.submit("operator-acceptance", &request).await;
    assert_eq!(accepted.0, StatusCode::OK, "{accepted:?}");
    let state = f.state().await;
    assert_eq!(state.orders[0].carrier, "Exact carrier");
    assert_eq!(state.orders[0].handoff_reference, "Exact original handoff");
    let replay = f.submit("operator-acceptance", &request).await;
    assert_eq!(replay.1["replayed"], true);
    assert_eq!(replay.1["result"], accepted.1["result"]);
    assert_eq!(f.state().await, state);
    let (status, allowance) = f
        .http(
            "GET",
            &format!("/v1/warehouses/wh-main/orders/{o}/return-eligibility"),
            "operator-acceptance",
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{allowance}");
    assert_eq!(allowance["shipment_source"]["kind"], "event");
    assert_eq!(
        allowance["shipment_source"]["event_ids"]
            .as_array()
            .expect("events")
            .len(),
        1
    );
    assert_eq!(
        f.store
            .reconciliation(&f.identity.tenant, "wh-main")
            .await
            .expect("proof")["ok"],
        true
    );
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn split_fulfillment_stock_balanced_counter_drift_and_new_order_lines_cannot_replace_handoffs(
) {
    let f = Fixture::new().await;
    let oid = split_source(&f).await;
    split_pick(&f, &oid, "a", 3).await;
    split_pack(&f, &oid, "UNCHANGED-PACK", "a", 2).await;
    let before = f.state().await;
    let mut tx = f.store.pool.begin().await.expect("transaction");
    sqlx::query("UPDATE wms_allocations SET shipped=1 WHERE tenant=$1 AND warehouse_id='wh-main' AND order_id=$2 AND line_id='a'").bind(&f.identity.tenant).bind(&oid).execute(&mut *tx).await.expect("forged counter");
    sqlx::query("UPDATE wms_stock SET on_hand=on_hand-1,reserved=reserved-1 WHERE tenant=$1 AND warehouse_id='wh-main' AND sku='SKU-A' AND lot='LOT-A' AND location='STAGE-01'").bind(&f.identity.tenant).execute(&mut *tx).await.expect("balanced current stock");
    let mid = id();
    let m = json!({"id":mid,"kind":"count","sku":"SKU-A","lot":"LOT-A","from":"STAGE-01","to":null,"quantity":1,"reference":"forensic","receipt_id":null,"receipt_line_id":null,"reason":"Forged handoff through a staging count","actor":"forensic","at":"2026-10-18T00:00:00Z"});
    sqlx::query("INSERT INTO wms_movements(id,tenant,warehouse_id,sku,lot,from_location,to_location,quantity,kind,reference,data) VALUES($1,$2,'wh-main','SKU-A','LOT-A','STAGE-01',NULL,1,'count','forensic',$3)").bind(mid).bind(&f.identity.tenant).bind(m).execute(&mut *tx).await.expect("balanced physical ledger");
    let valid: bool = sqlx::query_scalar("SELECT wms_split_consistent($1,'wh-main',$2)")
        .bind(&f.identity.tenant)
        .bind(&oid)
        .fetch_one(&mut *tx)
        .await
        .expect("independent proof");
    assert!(!valid);
    let error = tx
        .commit()
        .await
        .expect_err("cannot commit counter without original package handoff");
    assert_eq!(
        error.as_database_error().and_then(|e| e.code()).as_deref(),
        Some("23514")
    );
    assert_eq!(f.state().await, before);
    let insert =
        sqlx::query("INSERT INTO wms_order_lines VALUES($1,'wh-main',$2,'invented','SKU-A',1)")
            .bind(&f.identity.tenant)
            .bind(&oid)
            .execute(&f.store.pool)
            .await;
    assert!(
        insert.is_err(),
        "new lines cannot inflate an activated original contract"
    );
    for query in [
        "UPDATE wms_packages SET reference='changed' WHERE tenant=$1",
        "UPDATE wms_package_lines SET quantity=quantity+1 WHERE tenant=$1",
        "UPDATE wms_orders SET data=data||'{\"shipment_mode\":null}'::jsonb WHERE tenant=$1",
    ] {
        assert!(sqlx::query(query)
            .bind(&f.identity.tenant)
            .execute(&f.store.pool)
            .await
            .is_err());
    }
}
#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn split_fulfillment_competing_pack_reviews_and_stale_repeatable_read_cannot_steal_claims() {
    let f = Fixture::new().await;
    let oid = split_source(&f).await;
    split_pick(&f, &oid, "a", 3).await;
    let before = f.state().await;
    let a = Request {
        serial_inputs: Vec::new(),
        unit_inputs: Vec::new(),
        request_id: id(),
        expected_version: before.version,
        resource_versions: None,
        command: Command::PackShipment {
            order_id: oid.clone(),
            reference: "ONE-RACING-PACK".into(),
            lines: vec![stateset_warehouse::split_fulfillment::LineInput {
                line_id: "a".into(),
                location: "A-01".into(),
                lot: "LOT-A".into(),
                quantity: 2,
            }],
        },
    };
    let b = Request {
        request_id: id(),
        ..a.clone()
    };
    let mut stale = f.store.pool.begin().await.expect("stale transaction");
    sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ")
        .execute(&mut *stale)
        .await
        .expect("isolation");
    let _: i64 = sqlx::query_scalar(
        "SELECT picked FROM wms_allocations WHERE tenant=$1 AND order_id=$2 AND line_id='a'",
    )
    .bind(&f.identity.tenant)
    .bind(&oid)
    .fetch_one(&mut *stale)
    .await
    .expect("establish snapshot");
    let (ra, rb) = tokio::join!(
        f.store.execute(&f.identity, "wh-main", &a),
        f.store.execute(&f.identity, "wh-main", &b)
    );
    assert_eq!(usize::from(ra.is_ok()) + usize::from(rb.is_ok()), 1);
    let error=sqlx::query("UPDATE wms_allocations SET picked=picked-2 WHERE tenant=$1 AND order_id=$2 AND line_id='a'").bind(&f.identity.tenant).bind(&oid).execute(&mut *stale).await.expect_err("stale counter edit cannot bypass later packed ownership");
    assert_eq!(
        error.as_database_error().and_then(|e| e.code()).as_deref(),
        Some("40001")
    );
    stale.rollback().await.expect("rollback");
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM wms_packages WHERE tenant=$1")
        .bind(&f.identity.tenant)
        .fetch_one(&f.store.pool)
        .await
        .expect("package count");
    assert_eq!(n, 1);
    assert_eq!(
        f.store
            .reconciliation(&f.identity.tenant, "wh-main")
            .await
            .expect("proof")["ok"],
        true
    );
}
#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn split_fulfillment_final_delivery_failure_rolls_back_mode_packing_and_physical_handoff() {
    let f = Fixture::new().await;
    f.seed_catalog().await;
    f.command(opening_count(3)).await;
    let oid = entity(
        &f.command(Command::CreateOrder {
            reference: "LEGACY-TO-PACKAGES".into(),
            source_order_id: None,
            customer: "Customer".into(),
            destination: "Destination".into(),
            priority: "normal".into(),
            lines: vec![LineInput {
                id: "a".into(),
                sku: "SKU-A".into(),
                quantity: 3,
            }],
        })
        .await,
    );
    f.command(Command::Allocate {
        order_id: oid.clone(),
    })
    .await;
    split_pick(&f, &oid, "a", 3).await;
    let mut package = String::new();
    for handoff in [false, true] {
        let before = f.state().await;
        let counts = command_evidence_counts(&f).await;
        let command = if handoff {
            Command::HandoffShipment {
                shipment_id: package.clone(),
                carrier: "Carrier".into(),
                handoff_reference: "Atomic handoff".into(),
            }
        } else {
            Command::PackShipment {
                order_id: oid.clone(),
                reference: "ATOMIC-PACKAGE".into(),
                lines: vec![stateset_warehouse::split_fulfillment::LineInput {
                    line_id: "a".into(),
                    location: "A-01".into(),
                    lot: "LOT-A".into(),
                    quantity: 3,
                }],
            }
        };
        let request = Request {
            serial_inputs: Vec::new(),
            unit_inputs: Vec::new(),
            request_id: id(),
            expected_version: before.version,
            resource_versions: None,
            command,
        };
        let trigger = format!("split_fault_{}", id().replace('-', ""));
        sqlx::raw_sql(&format!("CREATE FUNCTION {trigger}() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.tenant='{}' THEN RAISE EXCEPTION 'injected split delivery failure'; END IF; RETURN NEW; END $$; CREATE TRIGGER {trigger} BEFORE INSERT ON wms_outbox FOR EACH ROW EXECUTE FUNCTION {trigger}()",f.identity.tenant)).execute(&f.store.pool).await.expect("own trigger");
        let failed = f.store.execute(&f.identity, "wh-main", &request).await;
        sqlx::raw_sql(&format!(
            "DROP TRIGGER {trigger} ON wms_outbox; DROP FUNCTION {trigger}()"
        ))
        .execute(&f.store.pool)
        .await
        .expect("remove own trigger");
        assert_eq!(failed.expect_err("outbox failure").status, 503);
        assert_eq!(f.state().await, before);
        assert_eq!(command_evidence_counts(&f).await, counts);
        if !handoff {
            let n: i64 =
                sqlx::query_scalar("SELECT count(*) FROM wms_split_orders WHERE tenant=$1")
                    .bind(&f.identity.tenant)
                    .fetch_one(&f.store.pool)
                    .await
                    .expect("original baseline count");
            assert_eq!(n, 0);
        }
        let accepted = f
            .store
            .execute(&f.identity, "wh-main", &request)
            .await
            .expect("original retry");
        package = entity(&accepted["result"]);
        let after = f.state().await;
        let replay = f
            .store
            .execute(&f.identity, "wh-main", &request)
            .await
            .expect("replay");
        assert_eq!(replay["replayed"], true);
        assert_eq!(replay["result"], accepted["result"]);
        assert_eq!(f.state().await, after);
    }
}
#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn split_fulfillment_upgrade_preserves_prior_handoff_document_and_rejects_reinterpreting_it()
{
    let f = Fixture::new().await;
    let oid = shipped_two_line_source(&f).await;
    let mut tx = preceding_shipment_schema(&f, None, true).await;
    for sql in [
        include_str!("../migrations/013_shipment_evidence.sql"),
        include_str!("../migrations/014_facility_transfers.sql"),
    ] {
        sqlx::raw_sql(sql)
            .execute(&mut *tx)
            .await
            .expect("actual preceding schema");
    }
    // An absent optional field and a JSON null both serialize as None in Rust.
    sqlx::query("UPDATE wms_orders SET data=data||'{\"shipment_mode\":null}'::jsonb WHERE tenant=$1 AND id=$2").bind(&f.identity.tenant).bind(&oid).execute(&mut *tx).await.expect("old projection ignores unknown cached null");
    let before: Value = sqlx::query_scalar(
        "SELECT document FROM wms_shipment_evidence WHERE tenant=$1 AND order_id=$2",
    )
    .bind(&f.identity.tenant)
    .bind(&oid)
    .fetch_one(&mut *tx)
    .await
    .expect("original handoff");
    sqlx::raw_sql(include_str!("../migrations/015_split_fulfillment.sql"))
        .execute(&mut *tx)
        .await
        .expect("upgrade");
    let (after,ok):(Value,bool)=sqlx::query_as("SELECT document,wms_shipment_consistent(tenant,warehouse_id,order_id) FROM wms_shipment_evidence WHERE tenant=$1 AND order_id=$2").bind(&f.identity.tenant).bind(&oid).fetch_one(&mut *tx).await.expect("preserved source");
    assert_eq!(after, before);
    assert!(ok);
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM wms_packages")
        .fetch_one(&mut *tx)
        .await
        .expect("no fabricated packages");
    assert_eq!(n, 0);
    let altered=sqlx::query("UPDATE wms_orders SET data=data||'{\"shipment_mode\":\"packages\"}'::jsonb WHERE tenant=$1 AND id=$2").bind(&f.identity.tenant).bind(&oid).execute(&mut *tx).await;
    assert!(altered.is_err());
    tx.rollback().await.expect("remove own upgrade schema");
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn split_packages_respect_roles_tenant_scope_and_blocked_physical_recovery() {
    let f = Fixture::new().await;
    let oid = split_source(&f).await;
    split_pick(&f, &oid, "a", 3).await;
    let package = split_pack(&f, &oid, "BLOCKED-PACKAGE", "a", 3).await;
    let handoff = Command::HandoffShipment {
        shipment_id: package.clone(),
        carrier: "Synthetic carrier".into(),
        handoff_reference: "Must remain staged".into(),
    };
    for token in ["viewer-acceptance", "oms-acceptance"] {
        let request =
            json!({"request_id":id(),"expected_version":f.state().await.version,"command":handoff});
        assert_eq!(
            f.http(
                "POST",
                "/v1/warehouses/wh-main/commands",
                token,
                Some(request)
            )
            .await
            .0,
            StatusCode::FORBIDDEN
        );
    }
    assert_eq!(
        f.http(
            "GET",
            &format!("/v1/warehouses/wh-main/orders/{oid}/shipments"),
            "outsider-acceptance",
            None
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    f.reject(
        Command::PackShipment {
            order_id: oid.clone(),
            reference: "WRONG-SAME-SKU-LINE".into(),
            lines: vec![stateset_warehouse::split_fulfillment::LineInput {
                line_id: "b".into(),
                location: "A-01".into(),
                lot: "LOT-A".into(),
                quantity: 1,
            }],
        },
        StatusCode::UNPROCESSABLE_ENTITY,
    )
    .await;
    f.command(Command::ReportException {
        order_id: Some(oid.clone()),
        category: "Short pick".into(),
        detail: "Inspect staged package before handing it off".into(),
    })
    .await;
    f.reject(handoff, StatusCode::UNPROCESSABLE_ENTITY).await;
    f.command(Command::UnpackShipment {
        shipment_id: package,
        reason: "Observed packing issue; stock remains staged".into(),
    })
    .await;
    f.command(Command::Unpick {
        order_id: oid.clone(),
        line_id: "a".into(),
        location: "A-01".into(),
        lot: "LOT-A".into(),
        quantity: 3,
        reason: "Physically returned exact staged units".into(),
    })
    .await;
    f.command(Command::ReleaseUnpicked {
        order_id: oid,
        reason: "Release holds while investigating exception".into(),
    })
    .await;
    assert_eq!(
        f.state()
            .await
            .stocks
            .iter()
            .map(|s| s.reserved)
            .sum::<i64>(),
        0
    );
    let proof = f
        .store
        .reconciliation(&f.identity.tenant, "wh-main")
        .await
        .expect("blocked recovery proof");
    assert_eq!(proof["ok"], true, "{proof}");
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn split_original_packing_and_closure_documents_detect_historical_timestamp_drift() {
    let f = Fixture::new().await;
    let oid = split_source(&f).await;
    split_pick(&f, &oid, "a", 3).await;
    let package = split_pack(&f, &oid, "TIMESTAMP-ANCHOR", "a", 2).await;
    f.command(Command::UnpackShipment {
        shipment_id: package,
        reason: "Retain original unpacking facts".into(),
    })
    .await;
    for (table, column) in [
        ("wms_packages", "packed_at"),
        ("wms_package_closures", "closed_at"),
    ] {
        let mut tx = f.store.pool.begin().await.expect("forensic transaction");
        // Explicit forensic bypass: ordinary updates are already rejected.
        // The independent proof must still compare the retained original event.
        sqlx::raw_sql(&format!(
            "ALTER TABLE {table} DISABLE TRIGGER {table}_immutable"
        ))
        .execute(&mut *tx)
        .await
        .expect("forensic bypass");
        sqlx::query(&format!(
            "UPDATE {table} SET {column}='1900-01-01T00:00:00Z' WHERE tenant=$1"
        ))
        .bind(&f.identity.tenant)
        .execute(&mut *tx)
        .await
        .expect("synthetic historical drift");
        let ok: bool = sqlx::query_scalar("SELECT wms_split_consistent($1,'wh-main',$2)")
            .bind(&f.identity.tenant)
            .bind(&oid)
            .fetch_one(&mut *tx)
            .await
            .expect("independent original proof");
        assert!(
            !ok,
            "{table} timestamp drift must disagree with its original event"
        );
        tx.rollback().await.expect("restore forensic fixture");
    }
    let proof = f
        .store
        .reconciliation(&f.identity.tenant, "wh-main")
        .await
        .expect("unaltered history proof");
    assert_eq!(proof["ok"], true, "{proof}");
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn package_ownership_changes_fence_prior_scoped_unpick_reviews_without_stock_deltas() {
    use stateset_warehouse::concurrency::{key, reviewed_versions};
    let f = Fixture::new().await;
    let oid = split_source(&f).await;
    split_pick(&f, &oid, "a", 3).await;
    let command = Command::Unpick {
        order_id: oid.clone(),
        line_id: "a".into(),
        location: "A-01".into(),
        lot: "LOT-A".into(),
        quantity: 1,
        reason: "Physical review of exact unclaimed staged units".into(),
    };
    let before = f.state().await;
    let stale = Request {
        serial_inputs: Vec::new(),
        unit_inputs: Vec::new(),
        request_id: id(),
        expected_version: before.version,
        resource_versions: reviewed_versions(&before, &command).expect("review"),
        command: command.clone(),
    };
    let package = split_pack(&f, &oid, "REVISION-FENCE", "a", 2).await;
    let packed = f.state().await;
    assert_eq!(
        packed.stocks, before.stocks,
        "packing changes ownership without moving stock"
    );
    assert_eq!(
        packed.orders, before.orders,
        "existing package mode needs no cached order delta"
    );
    let order_key = key(&["order", &oid]);
    assert!(packed.resource_versions[&order_key] > before.resource_versions[&order_key]);
    let (status, conflict) = f.submit("admin-acceptance", &stale).await;
    assert_eq!(status, StatusCode::CONFLICT, "{conflict}");
    assert_eq!(conflict["command_outcome"]["reason"], "changed_resources");
    assert_eq!(
        conflict["command_outcome"]["expected_resource_versions"],
        json!(stale.resource_versions)
    );
    assert_eq!(f.state().await, packed);
    let fresh_packed = Request {
        serial_inputs: Vec::new(),
        unit_inputs: Vec::new(),
        request_id: id(),
        expected_version: packed.version,
        resource_versions: reviewed_versions(&packed, &command).expect("packed review"),
        command: command.clone(),
    };
    f.command(Command::UnpackShipment {
        shipment_id: package,
        reason: "Physically review packing change".into(),
    })
    .await;
    let unpacked = f.state().await;
    assert_eq!(unpacked.stocks, before.stocks);
    assert_eq!(unpacked.orders, before.orders);
    assert!(unpacked.resource_versions[&order_key] > packed.resource_versions[&order_key]);
    let (status, conflict) = f.submit("admin-acceptance", &fresh_packed).await;
    assert_eq!(status, StatusCode::CONFLICT, "{conflict}");
    assert_eq!(conflict["command_outcome"]["reason"], "changed_resources");
    assert_eq!(
        f.state().await,
        unpacked,
        "a claim changed and restored must still fence old scans"
    );
    let fresh = Request {
        serial_inputs: Vec::new(),
        unit_inputs: Vec::new(),
        request_id: id(),
        expected_version: unpacked.version,
        resource_versions: reviewed_versions(&unpacked, &command).expect("fresh physical review"),
        command,
    };
    let (status, response) = f.submit("admin-acceptance", &fresh).await;
    assert_eq!(status, StatusCode::OK, "{response}");
    let proof = f
        .store
        .reconciliation(&f.identity.tenant, "wh-main")
        .await
        .expect("final proof");
    assert_eq!(proof["ok"], true, "{proof}");
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn partial_remainder_cancellation_preserves_original_handoffs_returns_and_exact_recovery() {
    let f = Fixture::new().await;
    let oid = split_source(&f).await;
    split_pick(&f, &oid, "a", 3).await;
    let package = split_pack(&f, &oid, "CLOSE-ORIGINAL-HANDOFF", "a", 3).await;
    split_handoff(&f, &package, "Observed carrier").await;
    let before = f.state().await;
    let original_order = before
        .orders
        .iter()
        .find(|o| o.id == oid)
        .expect("original order");
    let request = Request {
        serial_inputs: Vec::new(),
        unit_inputs: Vec::new(),
        request_id: id(),
        expected_version: before.version,
        resource_versions: None,
        command: Command::CancelRemainder {
            order_id: oid.clone(),
            reason: "Customer withdrew all unshipped demand".into(),
        },
    };
    let (status, accepted) = f.submit("operator-acceptance", &request).await;
    assert_eq!(status, StatusCode::OK, "{accepted}");
    let after = f.state().await;
    let order = after
        .orders
        .iter()
        .find(|o| o.id == oid)
        .expect("closed partial order");
    assert_eq!(order.status, "closed_partial");
    assert_eq!(order.lines, original_order.lines);
    assert_eq!(order.shipped_at, original_order.shipped_at);
    assert_eq!(order.carrier, original_order.carrier);
    assert_eq!(after.movements, before.movements);
    assert_eq!(after.stocks.iter().map(|s| s.on_hand).sum::<i64>(), 4);
    assert_eq!(after.stocks.iter().map(|s| s.reserved).sum::<i64>(), 0);
    let closure = order.remainder_cancellation.as_ref().expect("closure");
    assert_eq!(closure.lines["a"].shipped, 3);
    assert_eq!(closure.lines["a"].cancelled, 0);
    assert_eq!(closure.lines["b"].ordered, 5);
    assert_eq!(closure.lines["b"].cancelled, 5);
    let (status, proof) = f
        .http(
            "GET",
            "/v1/warehouses/wh-main/reconciliation",
            "admin-acceptance",
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(proof["ok"], true, "{proof}");
    let norm: bool =
        sqlx::query_scalar("SELECT wms_order_cancellation_consistent($1,'wh-main',$2)")
            .bind(&f.identity.tenant)
            .bind(&oid)
            .fetch_one(&f.store.pool)
            .await
            .expect("independent closure proof");
    assert!(norm);
    f.command(Command::CreateReturn {
        reference: "CLOSED-PARTIAL-RMA".into(),
        order_id: oid.clone(),
        reason: "Actual returned original unit".into(),
        lines: vec![stateset_warehouse::returns::ReturnLineInput {
            id: "rma-line".into(),
            order_line_id: "a".into(),
            lot: "LOT-A".into(),
            quantity: 1,
        }],
    })
    .await;
    let after_return = f.state().await;
    let (status, replay) = f.submit("operator-acceptance", &request).await;
    assert_eq!(status, StatusCode::OK, "{replay}");
    assert_eq!(replay["replayed"], true);
    assert_eq!(replay["result"], accepted["result"]);
    assert_eq!(f.state().await, after_return);
    assert_eq!(
        f.submit("other-admin-acceptance", &request).await.0,
        StatusCode::CONFLICT
    );
    let (status, eligibility) = f
        .http(
            "GET",
            &format!("/v1/warehouses/wh-main/orders/{oid}/return-eligibility"),
            "operator-acceptance",
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{eligibility}");
    assert_eq!(eligibility["lines"][0]["shipped"], 3);
    assert_eq!(eligibility["lines"][0]["remaining"], 2);
    assert_eq!(eligibility["shipment_source"]["kind"], "handoff_events");
    f.reject(
        Command::AllocateAvailable {
            order_id: oid.clone(),
        },
        StatusCode::UNPROCESSABLE_ENTITY,
    )
    .await;
    f.reject(
        Command::CreateReturn {
            reference: "CANCELLED-UNSHIPPED-RMA".into(),
            order_id: oid,
            reason: "Never shipped".into(),
            lines: vec![stateset_warehouse::returns::ReturnLineInput {
                id: "rma-never-shipped".into(),
                order_line_id: "b".into(),
                lot: "LOT-A".into(),
                quantity: 1,
            }],
        },
        StatusCode::UNPROCESSABLE_ENTITY,
    )
    .await;
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn partial_remainder_cancellation_holds_staged_work_and_enforces_roles_while_allowing_blocked_recovery(
) {
    let f = Fixture::new().await;
    let oid = split_source(&f).await;
    split_pick(&f, &oid, "a", 3).await;
    let package = split_pack(&f, &oid, "CLOSE-ROLE-HANDOFF", "a", 3).await;
    split_handoff(&f, &package, "Carrier").await;
    let request = Request {
        serial_inputs: Vec::new(),
        unit_inputs: Vec::new(),
        request_id: id(),
        expected_version: f.state().await.version,
        resource_versions: None,
        command: Command::CancelRemainder {
            order_id: oid.clone(),
            reason: "Customer cancellation".into(),
        },
    };
    for token in ["viewer-acceptance", "oms-acceptance"] {
        assert_eq!(f.submit(token, &request).await.0, StatusCode::FORBIDDEN);
    }
    assert_eq!(
        f.submit("outsider-acceptance", &request).await.0,
        StatusCode::NOT_FOUND
    );
    split_pick(&f, &oid, "b", 1).await;
    let active = split_pack(&f, &oid, "UNSHIPPED-ACTIVE-PACK", "b", 1).await;
    f.command(Command::ReportException {
        order_id: Some(oid.clone()),
        category: "Customer cancellation".into(),
        detail: "Physically recover staging before closure".into(),
    })
    .await;
    f.reject(request.command.clone(), StatusCode::UNPROCESSABLE_ENTITY)
        .await;
    f.command(Command::UnpackShipment {
        shipment_id: active,
        reason: "Customer cancellation recovery".into(),
    })
    .await;
    f.reject(request.command.clone(), StatusCode::UNPROCESSABLE_ENTITY)
        .await;
    f.command(Command::Unpick {
        order_id: oid.clone(),
        line_id: "b".into(),
        location: "A-01".into(),
        lot: "LOT-A".into(),
        quantity: 1,
        reason: "Actually returned from staging".into(),
    })
    .await;
    f.command(request.command).await;
    let after = f.state().await;
    assert_eq!(
        after
            .orders
            .iter()
            .find(|o| o.id == oid)
            .expect("order")
            .status,
        "closed_partial"
    );
    assert_eq!(
        after
            .exceptions
            .iter()
            .filter(|e| e.status == "open")
            .count(),
        1,
        "closure never silently resolves the warehouse exception"
    );
    assert_eq!(after.stocks.iter().map(|b| b.on_hand).sum::<i64>(), 4);
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn partial_remainder_competing_closures_and_old_scoped_pick_cannot_reuse_released_ownership()
{
    let f = Fixture::new().await;
    let oid = split_source(&f).await;
    split_pick(&f, &oid, "a", 3).await;
    let package = split_pack(&f, &oid, "RACING-CLOSE-HANDOFF", "a", 3).await;
    split_handoff(&f, &package, "Carrier").await;
    let before = f.state().await;
    let pick = Command::Pick {
        order_id: oid.clone(),
        line_id: "b".into(),
        location: "A-01".into(),
        lot: "LOT-A".into(),
        quantity: 1,
    };
    let stale_pick = Request {
        serial_inputs: Vec::new(),
        unit_inputs: Vec::new(),
        request_id: id(),
        expected_version: before.version,
        resource_versions: stateset_warehouse::concurrency::reviewed_versions(&before, &pick)
            .expect("scope"),
        command: pick,
    };
    let first = Request {
        serial_inputs: Vec::new(),
        unit_inputs: Vec::new(),
        request_id: id(),
        expected_version: before.version,
        resource_versions: None,
        command: Command::CancelRemainder {
            order_id: oid.clone(),
            reason: "Customer cancellation".into(),
        },
    };
    let mut second = first.clone();
    second.request_id = id();
    let (a, b) = tokio::join!(
        f.submit("operator-acceptance", &first),
        f.submit("other-admin-acceptance", &second)
    );
    assert_eq!(
        [a.0, b.0].iter().filter(|s| **s == StatusCode::OK).count(),
        1,
        "{a:?} {b:?}"
    );
    assert_eq!(
        [a.0, b.0]
            .iter()
            .filter(|s| **s == StatusCode::CONFLICT)
            .count(),
        1
    );
    let after = f.state().await;
    let (status, stale) = f.submit("operator-acceptance", &stale_pick).await;
    assert_eq!(status, StatusCode::CONFLICT, "{stale}");
    assert!(stale.to_string().contains("changed_resources"), "{stale}");
    assert_eq!(f.state().await, after);
    let rows: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM wms_order_cancellations WHERE tenant=$1 AND order_id=$2",
    )
    .bind(&f.identity.tenant)
    .bind(oid)
    .fetch_one(&f.store.pool)
    .await
    .expect("one closure");
    assert_eq!(rows, 1);
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn partial_remainder_closure_rolls_back_with_a_final_delivery_failure_and_original_retry_commits_once(
) {
    let f = Fixture::new().await;
    let oid = split_source(&f).await;
    split_pick(&f, &oid, "a", 3).await;
    let package = split_pack(&f, &oid, "ROLLBACK-CLOSE-HANDOFF", "a", 3).await;
    split_handoff(&f, &package, "Carrier").await;
    let before = f.state().await;
    let before_counts = command_evidence_counts(&f).await;
    let original = Request {
        serial_inputs: Vec::new(),
        unit_inputs: Vec::new(),
        request_id: id(),
        expected_version: before.version,
        resource_versions: None,
        command: Command::CancelRemainder {
            order_id: oid.clone(),
            reason: "Customer cancellation".into(),
        },
    };
    let trigger = format!("cancel_remainder_fault_{}", id().replace('-', ""));
    sqlx::raw_sql(&format!("CREATE FUNCTION {trigger}() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.tenant='{}' THEN RAISE EXCEPTION 'injected remainder delivery failure'; END IF; RETURN NEW; END $$; CREATE TRIGGER {trigger} BEFORE INSERT ON wms_outbox FOR EACH ROW EXECUTE FUNCTION {trigger}()",f.identity.tenant)).execute(&f.store.pool).await.expect("own fault");
    let (status, rejected) = f.submit("operator-acceptance", &original).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{rejected}");
    sqlx::raw_sql(&format!(
        "DROP TRIGGER {trigger} ON wms_outbox; DROP FUNCTION {trigger}()"
    ))
    .execute(&f.store.pool)
    .await
    .expect("remove own fault");
    assert_eq!(f.state().await, before);
    assert_eq!(command_evidence_counts(&f).await, before_counts);
    let closures: i64 =
        sqlx::query_scalar("SELECT count(*) FROM wms_order_cancellations WHERE tenant=$1")
            .bind(&f.identity.tenant)
            .fetch_one(&f.store.pool)
            .await
            .expect("no partial closure");
    assert_eq!(closures, 0);
    let (status, accepted) = f.submit("operator-acceptance", &original).await;
    assert_eq!(status, StatusCode::OK, "{accepted}");
    assert_eq!(
        f.submit("operator-acceptance", &original).await.1["result"],
        accepted["result"]
    );
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn partial_remainder_source_proof_rejects_stock_balanced_ownership_drift_and_cancellation_metadata_forgery(
) {
    let f = Fixture::new().await;
    let oid = split_source(&f).await;
    split_pick(&f, &oid, "a", 3).await;
    let package = split_pack(&f, &oid, "FORGERY-CLOSE-HANDOFF", "a", 3).await;
    split_handoff(&f, &package, "Carrier").await;
    f.command(Command::CancelRemainder {
        order_id: oid.clone(),
        reason: "Original customer cancellation".into(),
    })
    .await;
    let original = f.state().await;
    let mut tx = f.store.pool.begin().await.expect("adversarial transaction");
    sqlx::query("UPDATE wms_allocations SET quantity=quantity+1,released=released+1 WHERE tenant=$1 AND warehouse_id='wh-main' AND order_id=$2 AND line_id='b'").bind(&f.identity.tenant).bind(&oid).execute(&mut *tx).await.expect("balanced prospective drift");
    let ok: bool = sqlx::query_scalar("SELECT wms_order_cancellation_consistent($1,'wh-main',$2)")
        .bind(&f.identity.tenant)
        .bind(&oid)
        .fetch_one(&mut *tx)
        .await
        .expect("independent ownership proof");
    assert!(!ok);
    assert!(
        tx.commit().await.is_err(),
        "balanced physical stock cannot hide altered original cancellation ownership"
    );
    assert_eq!(f.state().await, original);
    let mut tx = f
        .store
        .pool
        .begin()
        .await
        .expect("forensic metadata bypass");
    sqlx::raw_sql("ALTER TABLE wms_order_cancellations DISABLE TRIGGER USER")
        .execute(&mut *tx)
        .await
        .expect("explicit own forensic bypass");
    sqlx::query("UPDATE wms_order_cancellations SET cancelled_at='2020-01-01T00:00:00Z' WHERE tenant=$1 AND order_id=$2").bind(&f.identity.tenant).bind(&oid).execute(&mut *tx).await.expect("forged metadata");
    let ok: bool = sqlx::query_scalar("SELECT wms_split_consistent($1,'wh-main',$2)")
        .bind(&f.identity.tenant)
        .bind(&oid)
        .fetch_one(&mut *tx)
        .await
        .expect("full historical proof");
    assert!(!ok);
    tx.rollback()
        .await
        .expect("restore immutable source and guards");
    assert_eq!(f.state().await, original);
    let (status, held) = f
        .http(
            "GET",
            &format!("/v1/warehouses/wh-main/orders/{oid}/oms-fulfillment"),
            "operator-acceptance",
            None,
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{held}");
    let native: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM wms_native_requests WHERE tenant=$1 AND kind='oms_fulfillment'",
    )
    .bind(&f.identity.tenant)
    .fetch_one(&f.store.pool)
    .await
    .expect("native hold before intent");
    assert_eq!(native, 0);
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn partial_remainder_upgrade_preserves_preexisting_original_handoff_documents_and_creates_no_cancellation(
) {
    let f = Fixture::new().await;
    let oid = shipped_two_line_source(&f).await;
    let mut tx = preceding_shipment_schema(&f, None, true).await;
    for migration in [
        include_str!("../migrations/013_shipment_evidence.sql"),
        include_str!("../migrations/014_facility_transfers.sql"),
        include_str!("../migrations/015_split_fulfillment.sql"),
    ] {
        sqlx::raw_sql(migration)
            .execute(&mut *tx)
            .await
            .expect("actual preceding schema");
    }
    sqlx::query("UPDATE wms_orders SET data=data||'{\"remainder_cancellation\":null}'::jsonb WHERE tenant=$1 AND id=$2").bind(&f.identity.tenant).bind(&oid).execute(&mut *tx).await.expect("old nullable cache projection");
    let before: Value = sqlx::query_scalar(
        "SELECT document FROM wms_shipment_evidence WHERE tenant=$1 AND order_id=$2",
    )
    .bind(&f.identity.tenant)
    .bind(&oid)
    .fetch_one(&mut *tx)
    .await
    .expect("original immutable handoff");
    sqlx::raw_sql(include_str!(
        "../migrations/016_order_remainder_cancellation.sql"
    ))
    .execute(&mut *tx)
    .await
    .expect("actual remainder migration");
    let (after,valid):(Value,bool)=sqlx::query_as("SELECT document,wms_shipment_consistent(tenant,warehouse_id,order_id) FROM wms_shipment_evidence WHERE tenant=$1 AND order_id=$2").bind(&f.identity.tenant).bind(&oid).fetch_one(&mut *tx).await.expect("source after upgrade");
    assert_eq!(before, after);
    assert!(valid);
    let rows: i64 = sqlx::query_scalar("SELECT count(*) FROM wms_order_cancellations")
        .fetch_one(&mut *tx)
        .await
        .expect("no fabricated closure");
    assert_eq!(rows, 0);
    let changed=sqlx::query("UPDATE wms_orders SET data=data||'{\"remainder_cancellation\":{\"reason\":\"Invented\"}}'::jsonb WHERE tenant=$1 AND id=$2").bind(&f.identity.tenant).bind(oid).execute(&mut *tx).await;
    assert!(
        changed.is_err(),
        "upgraded historical handoff header remains immutable"
    );
    tx.rollback().await.expect("remove own upgrade schema");
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn lot_transfer_arrival_keeps_frozen_and_current_expiry_and_adds_independent_restriction_without_losing_custody(
) {
    let f = transfer_fixture().await;
    f.command(Command::RecordLotExpiry {
        sku: "SKU-A".into(),
        lot: "LOT-A".into(),
        expires_at: "2099-10-22T00:00:00Z".into(),
        reason: "Read source manufacturer label".into(),
    })
    .await;
    let (_, dispatch) = transfer_at(&f, "wh-main", dispatch_transfer()).await;
    let tid = entity(&dispatch["result"]);
    f.command(Command::CorrectLotExpiry {
        sku: "SKU-A".into(),
        lot: "LOT-A".into(),
        expires_at: Some("2099-10-23T00:00:00Z".into()),
        reason: "Later source transcription correction".into(),
    })
    .await;
    f.command(Command::QuarantineLot {
        sku: "SKU-A".into(),
        lot: "LOT-A".into(),
        reason: "Source investigation began in transit".into(),
    })
    .await;
    for command in [
        Command::RecordLotExpiry {
            sku: "SKU-A".into(),
            lot: "LOT-A".into(),
            expires_at: "2099-10-21T00:00:00Z".into(),
            reason: "Existing destination label".into(),
        },
        Command::QuarantineLot {
            sku: "SKU-A".into(),
            lot: "LOT-A".into(),
            reason: "Independent destination investigation".into(),
        },
    ] {
        let before = f
            .store
            .state(&f.identity.tenant, "wh-east")
            .await
            .expect("lot control fixture and original evidence");
        f.store
            .execute(
                &f.identity,
                "wh-east",
                &Request {
                    serial_inputs: Vec::new(),
                    unit_inputs: Vec::new(),
                    request_id: id(),
                    expected_version: before.version,
                    resource_versions: None,
                    command,
                },
            )
            .await
            .expect("lot control fixture and original evidence");
    }
    let before = f
        .store
        .state(&f.identity.tenant, "wh-east")
        .await
        .expect("lot control fixture and original evidence");
    let existing_hold = before.lot_controls[0].holds[0].id.clone();
    let (arrival, accepted) = transfer_at(&f, "wh-east", transfer_arrival(&tid, "a", 2)).await;
    let after = f
        .store
        .state(&f.identity.tenant, "wh-east")
        .await
        .expect("lot control fixture and original evidence");
    assert_eq!(
        after.stocks.iter().map(|s| s.on_hand).sum::<i64>(),
        2,
        "unavailable arrivals still record actual custody"
    );
    assert_eq!(
        after.lot_controls[0].expires_at.as_deref(),
        Some("2099-10-21T00:00:00+00:00")
    );
    assert_eq!(
        after.lot_controls[0].holds.len(),
        2,
        "existing hold survives and an independent propagated hold is added"
    );
    let arrival_hold = after.lot_controls[0]
        .holds
        .iter()
        .find(|h| h.id != existing_hold)
        .expect("lot control fixture and original evidence")
        .id
        .clone();
    f.store
        .execute(
            &f.identity,
            "wh-east",
            &Request {
                serial_inputs: Vec::new(),
                unit_inputs: Vec::new(),
                request_id: id(),
                expected_version: after.version,
                resource_versions: None,
                command: Command::ReleaseLotHold {
                    hold_id: existing_hold,
                    reason: "Destination investigation completed".into(),
                },
            },
        )
        .await
        .expect("lot control fixture and original evidence");
    let after = f
        .store
        .state(&f.identity.tenant, "wh-east")
        .await
        .expect("lot control fixture and original evidence");
    assert_eq!(after.lot_controls[0].holds.len(), 1);
    assert_eq!(after.lot_controls[0].holds[0].id, arrival_hold);
    assert_eq!(
        f.store
            .execute(&f.identity, "wh-east", &arrival)
            .await
            .expect("lot control fixture and original evidence")["result"],
        accepted["result"],
        "retry never regenerates a hold or changes the original arrival"
    );
    for w in ["wh-main", "wh-east"] {
        let proof = f
            .store
            .reconciliation(&f.identity.tenant, w)
            .await
            .expect("lot control fixture and original evidence");
        assert_eq!(
            proof["ok"], true,
            "independent original transfer and lot proofs: {proof}"
        );
    }
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn lot_pages_keep_custody_separate_from_usable_storage_and_enforce_tenant_scope_and_original_cursor(
) {
    let f = Fixture::new().await;
    f.seed_catalog().await;
    f.command(opening_count(10)).await;
    f.command(Command::RecordLotExpiry {
        sku: "SKU-A".into(),
        lot: "LOT-A".into(),
        expires_at: "2000-01-01T00:00:00Z".into(),
        reason: "Read expired original label".into(),
    })
    .await;
    f.command(Command::QuarantineLot {
        sku: "SKU-A".into(),
        lot: "LOT-Z".into(),
        reason: "Retain issue before actual receiving".into(),
    })
    .await;
    let (status, first) = f
        .http(
            "GET",
            "/v1/warehouses/wh-main/lots?limit=1",
            "viewer-acceptance",
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{first}");
    assert_eq!(first["items"][0]["sku"], "SKU-A");
    assert_eq!(first["items"][0]["lot"], "LOT-A");
    assert_eq!(first["items"][0]["availability"], "expired");
    assert_eq!(first["items"][0]["physical"], 10);
    assert_eq!(first["items"][0]["unreserved"], 10);
    assert_eq!(first["items"][0]["usable"], 0);
    assert_eq!(first["version"], f.state().await.version);
    assert!(chrono::DateTime::parse_from_rfc3339(
        first["checked_at"]
            .as_str()
            .expect("lot control fixture and original evidence")
    )
    .is_ok());
    let mut path = reqwest::Url::parse("http://localhost/v1/warehouses/wh-main/lots")
        .expect("lot control fixture and original evidence");
    path.query_pairs_mut()
        .append_pair("limit", "1")
        .append_pair(
            "after",
            first["next_cursor"]
                .as_str()
                .expect("lot control fixture and original evidence"),
        );
    let path = format!(
        "{}?{}",
        path.path(),
        path.query()
            .expect("lot control fixture and original evidence")
    );
    let (status, second) = f.http("GET", &path, "viewer-acceptance", None).await;
    assert_eq!(status, StatusCode::OK, "{second}");
    assert_eq!(second["items"][0]["lot"], "LOT-Z");
    assert_eq!(second["items"][0]["physical"], 0);
    assert_eq!(second["items"][0]["availability"], "quarantined");
    assert!(second["next_cursor"].is_null());
    for suffix in [
        "?limit=0",
        "?after=bad",
        "?after=%5B%22a%22%5D",
        "?sku=",
        "?sku=SKU%00",
        "?after=%5B%22SKU-A%22%2C%22%5Cu0000%22%5D",
        "?actor=forged",
    ] {
        assert_eq!(
            f.http(
                "GET",
                &format!("/v1/warehouses/wh-main/lots{suffix}"),
                "viewer-acceptance",
                None
            )
            .await
            .0,
            StatusCode::UNPROCESSABLE_ENTITY
        );
    }
    let (status, foreign) = f
        .http(
            "GET",
            "/v1/warehouses/wh-main/lots",
            "outsider-acceptance",
            None,
        )
        .await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "authenticated foreign identity cannot read tenant lots: {foreign}"
    );
}

fn lot_trace_path(section: &str, review: Option<&str>, after: Option<&str>, limit: i64) -> String {
    let mut url = reqwest::Url::parse("http://localhost/v1/lot-trace").expect("trace route");
    url.query_pairs_mut()
        .append_pair("sku", "SKU-A")
        .append_pair("lot", "LOT-A")
        .append_pair("section", section)
        .append_pair("limit", &limit.to_string());
    if let Some(value) = review {
        url.query_pairs_mut().append_pair("review", value);
    }
    if let Some(value) = after {
        url.query_pairs_mut().append_pair("after", value);
    }
    format!("{}?{}", url.path(), url.query().expect("trace query"))
}

async fn lot_trace_fixture() -> (Fixture, String) {
    let f = transfer_fixture().await;
    f.command(Command::RecordLotExpiry {
        sku: "SKU-A".into(),
        lot: "LOT-A".into(),
        expires_at: "2099-10-22T00:00:00-07:00".into(),
        reason: "Original source label".into(),
    })
    .await;
    let (_, dispatch) = transfer_at(&f, "wh-main", dispatch_transfer()).await;
    let tid = entity(&dispatch["result"]);
    let order = entity(
        &f.command(Command::CreateOrder {
            reference: "TRACE-ORDER".into(),
            source_order_id: None,
            customer: "Synthetic trace customer".into(),
            destination: "Observed destination".into(),
            priority: "normal".into(),
            lines: vec![LineInput {
                id: "trace-line".into(),
                sku: "SKU-A".into(),
                quantity: 3,
            }],
        })
        .await,
    );
    f.command(Command::AllocateAvailable {
        order_id: order.clone(),
    })
    .await;
    split_pick(&f, &order, "trace-line", 2).await;
    let package = split_pack(&f, &order, "TRACE-HANDED-OFF", "trace-line", 1).await;
    split_handoff(&f, &package, "Trace carrier").await;
    split_pack(&f, &order, "TRACE-ACTIVE-PACKAGE", "trace-line", 1).await;
    f.command(Command::CreateReturn {
        reference: "TRACE-RMA".into(),
        order_id: order.clone(),
        reason: "One earlier actual handed-off unit".into(),
        lines: vec![stateset_warehouse::returns::ReturnLineInput {
            id: "trace-return-line".into(),
            order_line_id: "trace-line".into(),
            lot: "LOT-A".into(),
            quantity: 1,
        }],
    })
    .await;
    f.command(Command::CorrectLotExpiry {
        sku: "SKU-A".into(),
        lot: "LOT-A".into(),
        expires_at: Some("2000-01-01T00:00:00Z".into()),
        reason: "Supervisor corrected original label".into(),
    })
    .await;
    f.command(Command::QuarantineLot {
        sku: "SKU-A".into(),
        lot: "LOT-A".into(),
        reason: "Trace original affected custody".into(),
    })
    .await;
    transfer_at(&f, "wh-east", transfer_arrival(&tid, "a", 1)).await;
    (f, order)
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn lot_trace_pages_retain_cross_facility_custody_active_packing_original_handoff_returns_transit_and_policy(
) {
    let (f, order) = lot_trace_fixture().await;
    let before = command_evidence_counts(&f).await;
    let (status, first) = f
        .http(
            "GET",
            &lot_trace_path("facilities", None, None, 1),
            "viewer-acceptance",
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{first}");
    assert_eq!(first["summary"]["physical"], 5);
    assert_eq!(first["summary"]["reserved"], 2);
    assert_eq!(first["summary"]["staged"], 1);
    assert_eq!(first["summary"]["receiving"], 1);
    assert_eq!(first["summary"]["usable_storage"], 0);
    assert_eq!(first["summary"]["physical_reconciliation_ok"], true);
    assert_eq!(first["summary"]["sections"]["facilities"], 2);
    let review = first["review"].as_str().expect("review");
    let mut gathered = std::collections::BTreeMap::new();
    for section in [
        "facilities",
        "positions",
        "allocations",
        "receipts",
        "shipments",
        "transfers",
        "movements",
        "policies",
        "decisions",
    ] {
        let mut after: Option<String> = None;
        let mut items = Vec::new();
        let mut seen = std::collections::BTreeSet::new();
        loop {
            let (status, page) = f
                .http(
                    "GET",
                    &lot_trace_path(section, Some(review), after.as_deref(), 1),
                    "viewer-acceptance",
                    None,
                )
                .await;
            assert_eq!(status, StatusCode::OK, "{section}: {page}");
            assert_eq!(page["review"], review);
            assert_eq!(page["page_source_ok"], true, "{section}: {page}");
            assert!(chrono::DateTime::parse_from_rfc3339(
                page["checked_at"].as_str().expect("database clock")
            )
            .is_ok());
            for item in page["items"].as_array().expect("page records") {
                assert!(
                    seen.insert(json!([item["warehouse_id"], item["key"]]).to_string()),
                    "duplicate original trace record"
                );
                items.push(item.clone());
            }
            after = page["next_cursor"].as_str().map(str::to_owned);
            if after.is_none() {
                break;
            }
            assert!(seen.len() < 100, "fixture pagination did not advance");
        }
        assert_eq!(
            items.len() as i64,
            first["summary"]["sections"][section]
                .as_i64()
                .expect("complete section count")
        );
        gathered.insert(section, items);
    }
    let allocations = &gathered["allocations"];
    assert_eq!(allocations.len(), 1);
    assert_eq!(allocations[0]["document"]["order_id"], order);
    assert_eq!(allocations[0]["document"]["storage_reserved"], 1);
    assert_eq!(allocations[0]["document"]["staged"], 1);
    let packages = &gathered["shipments"];
    assert_eq!(packages.len(), 2);
    assert!(packages
        .iter()
        .any(|p| p["document"]["state"] == "packed" && p["document"]["quantity"] == 1));
    assert!(packages
        .iter()
        .any(|p| p["document"]["state"] == "handed_off"
            && p["document"]["quantity"] == 1
            && p["document"]["closure_event_id"].is_string()));
    assert_eq!(
        gathered["receipts"][0]["document"]["return_order_id"],
        order
    );
    let transit: i64 = gathered["transfers"]
        .iter()
        .map(|v| {
            v["document"]["dispatched"]
                .as_i64()
                .expect("isolated lot trace fixture and original evidence")
                - v["document"]["received"]
                    .as_i64()
                    .expect("isolated lot trace fixture and original evidence")
        })
        .sum();
    assert_eq!(transit, 4);
    assert_eq!(
        command_evidence_counts(&f).await,
        before,
        "read-only trace manufactured work"
    );
    let (status, foreign) = f
        .http(
            "GET",
            &lot_trace_path("facilities", None, None, 1),
            "outsider-acceptance",
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{foreign}");
    assert_eq!(foreign["summary"]["physical"], 0);
    assert_eq!(foreign["items"], json!([]));
    assert_ne!(
        foreign["review"], first["review"],
        "review must bind authenticated tenant even for matching labels"
    );
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn lot_trace_rejects_changed_restored_reviews_malformed_cursors_and_new_zero_stock_facility_phantoms(
) {
    let f = transfer_fixture().await;
    let (_, dispatch) = transfer_at(&f, "wh-main", dispatch_transfer()).await;
    let tid = entity(&dispatch["result"]);
    let (_, first) = f
        .http(
            "GET",
            &lot_trace_path("facilities", None, None, 1),
            "viewer-acceptance",
            None,
        )
        .await;
    let review = first["review"].as_str().expect("review");
    let cursor = first["next_cursor"].as_str().expect("second facility");
    let bad = lot_trace_path("positions", Some(review), Some(cursor), 1);
    assert_eq!(
        f.http("GET", &bad, "viewer-acceptance", None).await.0,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    for suffix in [
        "sku=SKU-A&lot=LOT-A&limit=0",
        "sku=SKU-A&lot=LOT-A&actor=forged",
        "sku=SKU-A&lot=LOT-A&section=unknown",
        "sku=SKU-A&lot=LOT-A&after=bad",
        "sku=SKU%00&lot=LOT-A",
        "sku=SKU-A&lot=%20LOT-A",
        "sku=SKU-A",
    ] {
        assert_eq!(
            f.http(
                "GET",
                &format!("/v1/lot-trace?{suffix}"),
                "viewer-acceptance",
                None
            )
            .await
            .0,
            StatusCode::UNPROCESSABLE_ENTITY,
            "{suffix}"
        );
    }
    let hold = f
        .command(Command::QuarantineLot {
            sku: "SKU-A".into(),
            lot: "LOT-A".into(),
            reason: "Changed then restored original availability".into(),
        })
        .await;
    f.command(Command::ReleaseLotHold {
        hold_id: entity(&hold),
        reason: "Release exact investigation hold".into(),
    })
    .await;
    assert_eq!(
        f.http(
            "GET",
            &lot_trace_path("facilities", Some(review), Some(cursor), 1),
            "viewer-acceptance",
            None
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    let (_, fresh) = f
        .http(
            "GET",
            &lot_trace_path("facilities", None, None, 1),
            "viewer-acceptance",
            None,
        )
        .await;
    let review = fresh["review"]
        .as_str()
        .expect("isolated lot trace fixture and original evidence");
    f.store
        .execute(
            &f.identity,
            "wh-third",
            &Request {
                serial_inputs: Vec::new(),
                unit_inputs: Vec::new(),
                request_id: id(),
                expected_version: 0,
                resource_versions: None,
                command: Command::CreateWarehouse {
                    code: "THIRD".into(),
                    name: "Synthetic third scope".into(),
                    city: "Vancouver".into(),
                    timezone: "America/Vancouver".into(),
                },
            },
        )
        .await
        .expect("isolated lot trace fixture and original evidence");
    f.store
        .execute(
            &f.identity,
            "wh-third",
            &Request {
                serial_inputs: Vec::new(),
                unit_inputs: Vec::new(),
                request_id: id(),
                expected_version: 1,
                resource_versions: None,
                command: Command::CreateProduct {
                    serial_controlled: false,
                    sku: "SKU-A".into(),
                    name: "Same SKU with no observed lot yet".into(),
                    category: "General".into(),
                    reorder_point: 0,
                },
            },
        )
        .await
        .expect("isolated lot trace fixture and original evidence");
    assert_eq!(
        f.http(
            "GET",
            &lot_trace_path("facilities", Some(review), None, 1),
            "viewer-acceptance",
            None
        )
        .await
        .0,
        StatusCode::OK,
        "unrelated empty catalog is not an observed lot"
    );
    f.store
        .execute(
            &f.identity,
            "wh-third",
            &Request {
                serial_inputs: Vec::new(),
                unit_inputs: Vec::new(),
                request_id: id(),
                expected_version: 2,
                resource_versions: None,
                command: Command::QuarantineLot {
                    sku: "SKU-A".into(),
                    lot: "LOT-A".into(),
                    reason: "Pre-receive restriction is an observed lot fact".into(),
                },
            },
        )
        .await
        .expect("isolated lot trace fixture and original evidence");
    assert_eq!(
        f.http(
            "GET",
            &lot_trace_path("facilities", Some(review), None, 1),
            "viewer-acceptance",
            None
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    let (_, fresh) = f
        .http(
            "GET",
            &lot_trace_path("facilities", None, None, 1),
            "viewer-acceptance",
            None,
        )
        .await;
    assert_eq!(fresh["summary"]["sections"]["facilities"], 3);
    let review = fresh["review"]
        .as_str()
        .expect("isolated lot trace fixture and original evidence");
    transfer_at(&f, "wh-east", transfer_arrival(&tid, "a", 1)).await;
    assert_eq!(
        f.http(
            "GET",
            &lot_trace_path("transfers", Some(review), None, 1),
            "viewer-acceptance",
            None
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn lot_trace_page_sources_reject_balanced_original_package_metadata_forgery_without_changing_custody(
) {
    let (f, order) = lot_trace_fixture().await;
    let mut tx = f
        .store
        .pool
        .begin()
        .await
        .expect("isolated lot trace fixture and original evidence");
    sqlx::raw_sql("ALTER TABLE wms_package_closures DISABLE TRIGGER USER")
        .execute(&mut *tx)
        .await
        .expect("isolated lot trace fixture and original evidence");
    sqlx::query("UPDATE wms_package_closures SET closed_at='2001-01-01T00:00:00Z' WHERE tenant=$1 AND warehouse_id='wh-main' AND order_id=$2 AND kind='handed_off'").bind(&f.identity.tenant).bind(&order).execute(&mut *tx).await.expect("isolated lot trace fixture and original evidence");
    let proof:bool=sqlx::query_scalar("SELECT bool_and(COALESCE(source_ok,false)) FROM wms_lot_trace_records($1,'SKU-A','LOT-A',ARRAY['wh-main','wh-east']) WHERE section='shipments'").bind(&f.identity.tenant).fetch_one(&mut *tx).await.expect("isolated lot trace fixture and original evidence");
    tx.rollback()
        .await
        .expect("isolated lot trace fixture and original evidence");
    assert!(
        !proof,
        "balanced custody cannot certify forged original handoff metadata"
    );
    let (status, page) = f
        .http(
            "GET",
            &lot_trace_path("shipments", None, None, 100),
            "viewer-acceptance",
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{page}");
    assert_eq!(page["page_source_ok"], true);
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn lot_trace_classic_shipment_source_binds_original_event_type_command_actor_and_receipt_even_with_balanced_custody(
) {
    let f = Fixture::new().await;
    let order = shipped_two_line_source(&f).await;
    let (status, page) = f
        .http(
            "GET",
            &lot_trace_path("shipments", None, None, 100),
            "viewer-acceptance",
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{page}");
    assert_eq!(page["page_source_ok"], true);
    assert_eq!(
        page["items"]
            .as_array()
            .expect("isolated lot trace fixture and original evidence")
            .len(),
        2
    );
    assert!(page["items"]
        .as_array()
        .expect("isolated lot trace fixture and original evidence")
        .iter()
        .all(|r| r["document"]["source_kind"] == "event" && r["document"]["event_id"].is_string()));
    let mut tx = f
        .store
        .pool
        .begin()
        .await
        .expect("isolated lot trace fixture and original evidence");
    sqlx::raw_sql("ALTER TABLE wms_events DISABLE TRIGGER USER")
        .execute(&mut *tx)
        .await
        .expect("isolated lot trace fixture and original evidence");
    sqlx::query("UPDATE wms_events SET payload=jsonb_set(payload,'{type}','\"fulfillment.packed\"') WHERE tenant=$1 AND warehouse_id='wh-main' AND id=(SELECT event_id FROM wms_shipment_evidence WHERE tenant=$1 AND warehouse_id='wh-main' AND order_id=$2)")
        .bind(&f.identity.tenant).bind(&order).execute(&mut *tx).await.expect("isolated lot trace fixture and original evidence");
    let (balanced,source):(bool,bool)=sqlx::query_as("SELECT wms_shipment_consistent($1,'wh-main',$2),wms_lot_trace_shipment_source($1,'wh-main',$2)").bind(&f.identity.tenant).bind(&order).fetch_one(&mut *tx).await.expect("isolated lot trace fixture and original evidence");
    tx.rollback()
        .await
        .expect("isolated lot trace fixture and original evidence");
    assert!(
        balanced,
        "retain the distinction from quantity/ownership reconstruction"
    );
    assert!(
        !source,
        "a stock-balanced event type forgery cannot become an original trace handoff"
    );
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn lot_trace_repeatable_read_keeps_one_policy_snapshot_during_a_committed_quarantine_and_fences_the_next_page(
) {
    use sqlx::Connection;
    let f = transfer_fixture().await;
    f.command(Command::RecordLotExpiry {
        sku: "SKU-A".into(),
        lot: "LOT-A".into(),
        expires_at: "2099-10-22T00:00:00Z".into(),
        reason: "Original label before controlled trace race".into(),
    })
    .await;
    transfer_at(&f, "wh-main", dispatch_transfer()).await;
    let schema = format!("trace_snapshot_{}", id().replace('-', "_"));
    let key: i64 = sqlx::query_scalar("SELECT hashtextextended($1,137671)")
        .bind(&schema)
        .fetch_one(&f.store.pool)
        .await
        .expect("isolated lot trace fixture and original evidence");
    // Shadow only one validation function in an owned reader schema. Writers
    // and other tests continue to use the original public schema/function.
    sqlx::raw_sql(&format!("CREATE SCHEMA {schema}; CREATE FUNCTION {schema}.wms_lot_change_valid(t TEXT,w TEXT,v BIGINT,n INTEGER) RETURNS BOOLEAN LANGUAGE plpgsql STABLE AS $$ BEGIN IF t='{}' AND w='wh-main' THEN PERFORM pg_advisory_xact_lock({key}); END IF; RETURN public.wms_lot_change_valid(t,w,v,n); END $$",f.identity.tenant)).execute(&f.store.pool).await.expect("isolated lot trace fixture and original evidence");
    let url = std::env::var("WMS_TEST_DATABASE_URL")
        .expect("isolated lot trace fixture and original evidence");
    let options = url
        .parse::<sqlx::postgres::PgConnectOptions>()
        .expect("isolated lot trace fixture and original evidence")
        .options([("search_path", format!("{schema},public"))]);
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(2)
        .after_connect(|c, _| {
            Box::pin(async move {
                sqlx::query("SET statement_timeout='5s'")
                    .execute(&mut *c)
                    .await?;
                Ok(())
            })
        })
        .connect_with(options)
        .await
        .expect("isolated lot trace fixture and original evidence");
    let scoped = Store { pool };
    let viewer = Identity {
        role: "viewer".into(),
        ..f.identity.clone()
    };
    let reader_fixture = Fixture {
        store: scoped.clone(),
        identity: f.identity.clone(),
        app: router(App {
            store: scoped.clone(),
            credentials: Arc::new(vec![("trace-snapshot-reader".into(), viewer)]),
            demo: false,
            connections: Connections::default(),
        }),
    };
    let mut blocker = sqlx::PgConnection::connect(&url)
        .await
        .expect("isolated lot trace fixture and original evidence");
    sqlx::query("SELECT pg_advisory_lock($1)")
        .bind(key)
        .execute(&mut blocker)
        .await
        .expect("isolated lot trace fixture and original evidence");
    let mut reader = tokio::spawn(async move {
        reader_fixture
            .http(
                "GET",
                &lot_trace_path("facilities", None, None, 1),
                "trace-snapshot-reader",
                None,
            )
            .await
    });
    let observed = observed_lot_advisory_waiter(&f.store.pool, key).await;
    let changed = if observed {
        let request = Request {
            serial_inputs: Vec::new(),
            unit_inputs: Vec::new(),
            request_id: id(),
            expected_version: f.state().await.version,
            resource_versions: None,
            command: Command::QuarantineLot {
                sku: "SKU-A".into(),
                lot: "LOT-A".into(),
                reason: "Committed while the original trace snapshot is paused".into(),
            },
        };
        Some(f.store.execute(&f.identity, "wh-main", &request).await)
    } else {
        None
    };
    let released = sqlx::query("SELECT pg_advisory_unlock($1)")
        .bind(key)
        .execute(&mut blocker)
        .await;
    let closed = blocker.close().await;
    let result = tokio::time::timeout(std::time::Duration::from_secs(6), &mut reader).await;
    if result.is_err() {
        reader.abort();
        let _ = reader.await;
    }
    scoped.pool.close().await;
    let removed = sqlx::raw_sql(&format!("DROP SCHEMA {schema} CASCADE"))
        .execute(&f.store.pool)
        .await;
    // Release the owned barrier/pool/schema before assertions, including failure.
    released.expect("isolated lot trace fixture and original evidence");
    closed.expect("isolated lot trace fixture and original evidence");
    removed.expect("isolated lot trace fixture and original evidence");
    assert!(
        observed,
        "did not observe the exact controlled reader barrier"
    );
    changed
        .expect("quarantine attempted only after real snapshot admission")
        .expect("writer must commit without waiting for the read-only trace");
    let (status, page) = result
        .expect("trace reader completed after barrier release")
        .expect("trace task");
    assert_eq!(status, StatusCode::OK, "{page}");
    assert_eq!(page["summary"]["usable_storage"],5,"one trace response must retain its original policy snapshot rather than mix the later quarantine with the earlier head version");
    let review = page["review"]
        .as_str()
        .expect("isolated lot trace fixture and original evidence");
    let cursor = page["next_cursor"]
        .as_str()
        .expect("second matched facility");
    assert_eq!(
        f.http(
            "GET",
            &lot_trace_path("facilities", Some(review), Some(cursor), 1),
            "viewer-acceptance",
            None
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    let (status, fresh) = f
        .http(
            "GET",
            &lot_trace_path("facilities", None, None, 100),
            "viewer-acceptance",
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{fresh}");
    assert_eq!(fresh["summary"]["usable_storage"], 0);
    assert_ne!(fresh["review"], page["review"]);
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn lot_trace_return_sources_reject_changed_authorization_expectations_while_physical_custody_stays_balanced(
) {
    let (f, order) = lot_trace_fixture().await;
    let mut tx = f
        .store
        .pool
        .begin()
        .await
        .expect("isolated forensic transaction");
    sqlx::raw_sql("ALTER TABLE wms_return_lines DISABLE TRIGGER USER; ALTER TABLE wms_receipt_lines DISABLE TRIGGER USER").execute(&mut *tx).await.expect("rollback-only original receipt guard bypass");
    sqlx::query("UPDATE wms_receipt_lines SET quantity=2 WHERE tenant=$1 AND warehouse_id='wh-main' AND receipt_id IN(SELECT receipt_id FROM wms_return_lines WHERE tenant=$1 AND warehouse_id='wh-main' AND order_id=$2)").bind(&f.identity.tenant).bind(&order).execute(&mut *tx).await.expect("forged expected quantity without physical receiving");
    sqlx::query("UPDATE wms_return_lines SET quantity=2 WHERE tenant=$1 AND warehouse_id='wh-main' AND order_id=$2").bind(&f.identity.tenant).bind(&order).execute(&mut *tx).await.expect("matching forged cached ownership");
    let balanced:bool=sqlx::query_scalar("SELECT bool_and(physical=ledger AND reserved=allocation_reserved) FROM wms_lot_trace_balances($1,'SKU-A','LOT-A')").bind(&f.identity.tenant).fetch_one(&mut *tx).await.expect("independent unchanged custody");
    let valid:bool=sqlx::query_scalar("SELECT bool_and(COALESCE(source_ok,false)) FROM wms_lot_trace_records($1,'SKU-A','LOT-A',ARRAY['wh-main','wh-east']) WHERE section='receipts'").bind(&f.identity.tenant).fetch_one(&mut *tx).await.expect("original return source proof");
    tx.rollback()
        .await
        .expect("restore original receipt, ownership and guards");
    assert!(
        balanced,
        "preserve the difference from physical reconstruction"
    );
    assert!(!valid,"a stock-balanced return cannot claim more units than the original handoff or its original authorization");
    let (status, page) = f
        .http(
            "GET",
            &lot_trace_path("receipts", None, None, 100),
            "viewer-acceptance",
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{page}");
    assert_eq!(page["page_source_ok"], true);
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn lot_transfer_proof_refuses_to_replace_missing_original_dispatch_policy_with_unknown_expiry(
) {
    let f = transfer_fixture().await;
    f.command(Command::RecordLotExpiry {
        sku: "SKU-A".into(),
        lot: "LOT-A".into(),
        expires_at: "2099-10-22T00:00:00Z".into(),
        reason: "Original source label".into(),
    })
    .await;
    let (_, accepted) = transfer_at(&f, "wh-main", dispatch_transfer()).await;
    let v = accepted["result"]["version"]
        .as_i64()
        .expect("original dispatch version");
    let mut tx = f
        .store
        .pool
        .begin()
        .await
        .expect("isolated forensic transaction");
    sqlx::query("ALTER TABLE wms_lot_decisions DISABLE TRIGGER USER")
        .execute(&mut *tx)
        .await
        .expect("inject privileged evidence deletion");
    sqlx::query("DELETE FROM wms_lot_decisions WHERE tenant=$1 AND warehouse_id='wh-main' AND version=$2 AND sku='SKU-A' AND lot='LOT-A'").bind(&f.identity.tenant).bind(v).execute(&mut *tx).await.expect("remove original frozen expiry");
    let valid: bool = sqlx::query_scalar(
        "SELECT COALESCE(wms_lot_dispatch_valid($1,'wh-main',$2,'SKU-A','LOT-A'),false)",
    )
    .bind(&f.identity.tenant)
    .bind(v)
    .fetch_one(&mut *tx)
    .await
    .expect("independent original dispatch check");
    assert!(
        !valid,
        "an arrival cannot downgrade missing modern evidence to legacy unknown"
    );
    tx.rollback().await.expect("restore original frozen expiry");
    transfer_at(
        &f,
        "wh-east",
        transfer_arrival(&entity(&accepted["result"]), "a", 1),
    )
    .await;
    assert_eq!(
        f.store
            .reconciliation(&f.identity.tenant, "wh-east")
            .await
            .expect("restored original source proof")["ok"],
        true
    );
}

async fn observed_lot_advisory_waiter(pool: &sqlx::PgPool, key: i64) -> bool {
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            let waiting: bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pg_locks WHERE locktype='advisory' AND NOT granted AND objsubid=1 AND classid::bigint=(($1::bigint >> 32) & 4294967295) AND objid::bigint=($1::bigint & 4294967295))")
                .bind(key).fetch_one(pool).await?;
            if waiting { return Ok::<bool, sqlx::Error>(true); }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    }).await.is_ok_and(|result| result.is_ok_and(|waiting|waiting))
}

async fn lot_quarantine_arrival_race(source_first: bool) {
    use sqlx::Connection;
    let f = transfer_fixture().await;
    let (_, dispatch) = transfer_at(&f, "wh-main", dispatch_transfer()).await;
    let tid = entity(&dispatch["result"]);
    let source_before = f.state().await;
    let destination_before = f
        .store
        .state(&f.identity.tenant, "wh-east")
        .await
        .expect("original destination");
    assert!(
        source_before.lot_controls.is_empty(),
        "the race must protect an initially absent policy row"
    );
    let quarantine = Request {
        serial_inputs: Vec::new(),
        unit_inputs: Vec::new(),
        request_id: id(),
        expected_version: source_before.version,
        resource_versions: None,
        command: Command::QuarantineLot {
            sku: "SKU-A".into(),
            lot: "LOT-A".into(),
            reason: "Actual source issue discovered while goods are in transit".into(),
        },
    };
    let arrival = Request {
        serial_inputs: Vec::new(),
        unit_inputs: Vec::new(),
        request_id: id(),
        expected_version: destination_before.version,
        resource_versions: None,
        command: transfer_arrival(&tid, "a", 1),
    };
    let trigger = format!("lot_race_{}", id().replace('-', ""));
    let barrier_key: i64 = sqlx::query_scalar("SELECT hashtextextended($1,713067)")
        .bind(&trigger)
        .fetch_one(&f.store.pool)
        .await
        .expect("unique barrier key");
    let source_key: i64 = sqlx::query_scalar("SELECT wms_lot_lock_key($1,'wh-main','SKU-A')")
        .bind(&f.identity.tenant)
        .fetch_one(&f.store.pool)
        .await
        .expect("exact source policy key");
    assert_ne!(barrier_key, source_key);
    let mut blocker = sqlx::PgConnection::connect(
        &std::env::var("WMS_TEST_DATABASE_URL").expect("isolated test database"),
    )
    .await
    .expect("owned barrier connection");
    sqlx::query("SELECT pg_advisory_lock($1)")
        .bind(barrier_key)
        .execute(&mut blocker)
        .await
        .expect("hold only the owned test barrier");
    let (warehouse, kind) = if source_first {
        ("wh-main", "lot.quarantined")
    } else {
        ("wh-east", "lot.arrival_restricted")
    };
    sqlx::raw_sql(&format!("CREATE FUNCTION {trigger}() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.tenant='{}' AND NEW.warehouse_id='{warehouse}' AND NEW.kind='{kind}' THEN PERFORM pg_advisory_xact_lock({barrier_key}); END IF; RETURN NEW; END $$; CREATE TRIGGER {trigger} BEFORE INSERT ON wms_lot_changes FOR EACH ROW EXECUTE FUNCTION {trigger}()",f.identity.tenant))
        .execute(&f.store.pool).await.expect("install tenant-only original-history admission barrier");
    let spawn = |w: &str, r: &Request| {
        let store = f.store.clone();
        let identity = f.identity.clone();
        let warehouse = w.to_string();
        let request = r.clone();
        tokio::spawn(async move { store.execute(&identity, &warehouse, &request).await })
    };
    let first = if source_first {
        spawn("wh-main", &quarantine)
    } else {
        spawn("wh-east", &arrival)
    };
    let first_observed = observed_lot_advisory_waiter(&f.store.pool, barrier_key).await;
    let second = if source_first {
        spawn("wh-east", &arrival)
    } else {
        spawn("wh-main", &quarantine)
    };
    let second_observed = observed_lot_advisory_waiter(&f.store.pool, source_key).await;
    // Release/close the owned barrier and remove the trigger before asserting
    // outcomes, including when an observation or operation failed.
    let unlocked: bool = sqlx::query_scalar("SELECT pg_advisory_unlock($1)")
        .bind(barrier_key)
        .fetch_one(&mut blocker)
        .await
        .expect("release owned test barrier");
    blocker
        .close()
        .await
        .expect("close owned barrier connection");
    let outcomes = tokio::time::timeout(std::time::Duration::from_secs(12), async {
        tokio::join!(first, second)
    })
    .await;
    sqlx::raw_sql(&format!(
        "DROP TRIGGER {trigger} ON wms_lot_changes; DROP FUNCTION {trigger}()"
    ))
    .execute(&f.store.pool)
    .await
    .expect("remove only owned test trigger");
    assert!(unlocked && first_observed && second_observed,"both precise advisory waits must be observed; a passing final state alone does not establish the race");
    let (a, b) = outcomes.expect("both operations settle after the owned barrier opens");
    let a = a.expect("first task").expect("first canonical command");
    let b = b.expect("second task").expect("second canonical command");
    let (held, arrived) = if source_first { (a, b) } else { (b, a) };
    let source = f.state().await;
    let destination = f
        .store
        .state(&f.identity.tenant, "wh-east")
        .await
        .expect("received custody");
    assert_eq!(
        source.version,
        source_before.version + 1,
        "the receiver must not acquire/update the source warehouse head after policy locks"
    );
    assert_eq!(
        source.version,
        held["result"]["version"]
            .as_i64()
            .expect("original source version")
    );
    assert_eq!(
        source.stocks, source_before.stocks,
        "quarantine and destination arrival cannot alter source custody"
    );
    assert_eq!(destination.stocks.iter().map(|s| s.on_hand).sum::<i64>(), 1);
    let control = destination
        .lot_controls
        .iter()
        .find(|c| c.sku == "SKU-A" && c.lot == "LOT-A")
        .expect("original arrival control");
    assert_eq!(!control.holds.is_empty(),source_first,"source restrictions must reflect the actual lock admission order, never future retroactive policy");
    let evidence: Value=sqlx::query_scalar("SELECT document FROM wms_lot_changes WHERE tenant=$1 AND warehouse_id='wh-east' AND version=$2 AND kind='lot.arrival_restricted'")
        .bind(&f.identity.tenant).bind(arrived["result"]["version"].as_i64().expect("arrival version")).fetch_one(&f.store.pool).await.expect("original source facts");
    assert_eq!(
        evidence["arrival"]["source_version"],
        if source_first {
            source.version
        } else {
            source_before.version
        }
    );
    assert_eq!(
        evidence["arrival"]["source"]["holds"]
            .as_array()
            .expect("original source holds")
            .len(),
        usize::from(source_first)
    );
    if source_first {
        assert_eq!(
            evidence["arrival"]["source"]["holds"][0]["id"],
            held["result"]["entity_id"]
        );
    } else {
        // Warehouse holds are not a tenant-wide recall. The already completed
        // arrival stays historical; the next observed arrival carries the hold.
        transfer_at(&f, "wh-east", transfer_arrival(&tid, "a", 1)).await;
        let next = f
            .store
            .state(&f.identity.tenant, "wh-east")
            .await
            .expect("subsequent arrival");
        assert_eq!(next.stocks.iter().map(|s| s.on_hand).sum::<i64>(), 2);
        assert_eq!(next.lot_controls[0].holds.len(), 1);
    }
    let retry = f
        .store
        .execute(&f.identity, "wh-main", &quarantine)
        .await
        .expect("original quarantine retry");
    assert_eq!(retry["result"], held["result"]);
    for w in ["wh-main", "wh-east"] {
        assert_eq!(
            f.store
                .reconciliation(&f.identity.tenant, w)
                .await
                .expect("full original transfer/policy/ledger proof")["ok"],
            true
        );
    }
}
#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn lot_quarantine_committed_first_propagates_across_a_real_blocked_arrival_and_absent_policy_row(
) {
    lot_quarantine_arrival_race(true).await;
}
#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn lot_arrival_admitted_first_retains_original_policy_and_later_arrival_propagates_new_quarantine(
) {
    lot_quarantine_arrival_race(false).await;
}

async fn preceding_lot_schema(f: &Fixture) -> sqlx::Transaction<'static, sqlx::Postgres> {
    let mut tx = preceding_shipment_schema_with_lot_compatibility(f, None, true, true).await;
    for migration in [
        include_str!("../migrations/013_shipment_evidence.sql"),
        include_str!("../migrations/014_facility_transfers.sql"),
        include_str!("../migrations/015_split_fulfillment.sql"),
        include_str!("../migrations/016_order_remainder_cancellation.sql"),
    ] {
        sqlx::raw_sql(migration)
            .execute(&mut *tx)
            .await
            .expect("actual preceding16 migrations");
    }
    tx
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn lot_upgrade_preserves_actual_preceding16_handoff_returns_and_original_receipts_without_invented_history(
) {
    let f = Fixture::new().await;
    let oid = shipped_two_line_source(&f).await;
    let mut tx = preceding_lot_schema(&f).await;
    let schema: String = sqlx::query_scalar("SELECT current_schema()")
        .fetch_one(&mut *tx)
        .await
        .expect("owned upgrade schema");
    let baseline: i64 = sqlx::query_scalar(
        "SELECT version FROM wms_warehouses WHERE tenant=$1 AND warehouse_id='wh-main'",
    )
    .bind(&f.identity.tenant)
    .fetch_one(&mut *tx)
    .await
    .expect("original head");
    let source: Value=sqlx::query_scalar("SELECT jsonb_build_object('document',s.document,'payload',e.payload,'receipt',c.result,'request',jsonb_build_object('request_id',c.request_id,'expected_version',e.version-1,'command',e.payload->'command')) FROM wms_shipment_evidence s JOIN wms_events e ON e.id=s.event_id JOIN wms_commands c ON c.tenant=e.tenant AND c.warehouse_id=e.warehouse_id AND c.request_id=e.payload->>'request_id' WHERE s.tenant=$1 AND s.order_id=$2")
        .bind(&f.identity.tenant).bind(&oid).fetch_one(&mut *tx).await.expect("actual preceding handoff and command source");
    assert!(source["payload"].get("lot_decisions").is_none());
    sqlx::raw_sql(include_str!("../migrations/017_lot_controls.sql"))
        .execute(&mut *tx)
        .await
        .expect("lot safety upgrade");
    let recorded_baseline: i64=sqlx::query_scalar("SELECT baseline_version FROM wms_lot_enforcement WHERE tenant=$1 AND warehouse_id='wh-main'").bind(&f.identity.tenant).fetch_one(&mut *tx).await.expect("explicit legacy baseline");
    assert_eq!(recorded_baseline, baseline);
    let invented: i64 = sqlx::query_scalar(
        "SELECT (SELECT count(*) FROM wms_lot_changes)+(SELECT count(*) FROM wms_lot_decisions)",
    )
    .fetch_one(&mut *tx)
    .await
    .expect("no retrospective quality claims");
    assert_eq!(invented, 0);
    let (document,payload,valid):(Value,Value,bool)=sqlx::query_as("SELECT s.document,e.payload,wms_shipment_consistent(s.tenant,s.warehouse_id,s.order_id) FROM wms_shipment_evidence s JOIN wms_events e ON e.id=s.event_id WHERE s.tenant=$1 AND s.order_id=$2")
        .bind(&f.identity.tenant).bind(&oid).fetch_one(&mut *tx).await.expect("unmodified original handoff");
    assert_eq!(document, source["document"]);
    assert_eq!(payload, source["payload"]);
    assert!(valid);
    // Explicit fixture row copies retain IDs, as pg_restore does. Restore the
    // copied serial heads before exercising new real application commands.
    for table in [
        "wms_orders",
        "wms_receipts",
        "wms_movements",
        "wms_activity",
    ] {
        sqlx::query(&format!("SELECT setval(pg_get_serial_sequence('{table}','seq'),COALESCE((SELECT max(seq) FROM {table}),1),EXISTS(SELECT 1 FROM {table}))"))
            .execute(&mut *tx).await.expect("original copied sequence continuity");
    }
    tx.commit()
        .await
        .expect("commit only owned upgraded schema");
    let search_path = format!("{schema},public");
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(2)
        .after_connect(move |c, _| {
            let sql = search_path.clone();
            Box::pin(async move {
                sqlx::query("SELECT set_config('search_path',$1,false)")
                    .bind(sql)
                    .execute(&mut *c)
                    .await?;
                sqlx::query("SET statement_timeout='10s'")
                    .execute(&mut *c)
                    .await?;
                Ok(())
            })
        })
        .connect(&std::env::var("WMS_TEST_DATABASE_URL").expect("isolated database"))
        .await
        .expect("canonical store in owned upgraded schema");
    let upgraded = Store { pool };
    let original_stock = upgraded
        .state(&f.identity.tenant, "wh-main")
        .await
        .expect("upgraded legacy custody")
        .stocks;
    let original_shipment = upgraded
        .detail(&f.identity.tenant, "wh-main", "orders", &oid)
        .await
        .expect("original order");
    for command in [
        Command::SetProductExpiryPolicy {
            sku: "SKU-A".into(),
            required: true,
            reason: "Require verified expiry from upgrade onward".into(),
        },
        Command::RecordLotExpiry {
            sku: "SKU-A".into(),
            lot: "LOT-A".into(),
            expires_at: "2000-01-01T00:00:00Z".into(),
            reason: "Read current exact original label after upgrade".into(),
        },
        Command::QuarantineLot {
            sku: "SKU-A".into(),
            lot: "LOT-A".into(),
            reason: "New issue must not rewrite old completed handoffs".into(),
        },
    ] {
        let state = upgraded
            .state(&f.identity.tenant, "wh-main")
            .await
            .expect("current upgrade review");
        upgraded
            .execute(
                &f.identity,
                "wh-main",
                &Request {
                    serial_inputs: Vec::new(),
                    unit_inputs: Vec::new(),
                    request_id: id(),
                    expected_version: state.version,
                    resource_versions: None,
                    command,
                },
            )
            .await
            .expect("actual new canonical quality command");
    }
    let now = upgraded
        .state(&f.identity.tenant, "wh-main")
        .await
        .expect("current held stock");
    assert_eq!(now.stocks, original_stock);
    assert_eq!(
        upgraded
            .detail(&f.identity.tenant, "wh-main", "orders", &oid)
            .await
            .expect("retained old order"),
        original_shipment
    );
    let request: Request =
        serde_json::from_value(source["request"].clone()).expect("actual original envelope");
    assert_eq!(
        upgraded
            .execute(&f.identity, "wh-main", &request)
            .await
            .expect("old receipt fast replay")["result"],
        source["receipt"]
    );
    let allowance = stateset_warehouse::returns::eligible(&upgraded, &f.identity, "wh-main", &oid)
        .await
        .expect("old actual handoff still authorizes returns");
    assert!(allowance["lines"]
        .as_array()
        .expect("original lines")
        .iter()
        .any(|l| l["sku"] == "SKU-A" && l["remaining"].as_i64().is_some_and(|q| q > 0)));
    let state = upgraded
        .state(&f.identity.tenant, "wh-main")
        .await
        .expect("return review");
    upgraded
        .execute(
            &f.identity,
            "wh-main",
            &Request {
                serial_inputs: Vec::new(),
                unit_inputs: Vec::new(),
                request_id: id(),
                expected_version: state.version,
                resource_versions: None,
                command: cancellation_return(&oid, "POST-UPGRADE-RETURN", "a", 1),
            },
        )
        .await
        .expect("current custody return may be authorized against original historic handoff");
    let proof = upgraded
        .reconciliation(&f.identity.tenant, "wh-main")
        .await
        .expect("full original and new quality proof");
    assert_eq!(proof["ok"], true, "{proof}");
    assert_eq!(proof["lot_controls"]["baseline_version"], baseline);
    assert_eq!(proof["lot_controls"]["policy_changes"], 3);
    assert_eq!(proof["lot_controls"]["eligibility_decisions"], 0);
    upgraded.pool.close().await;
    sqlx::raw_sql(&format!("DROP SCHEMA {schema} CASCADE"))
        .execute(&f.store.pool)
        .await
        .expect("remove only owned upgraded schema");
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn lot_upgrade_rejects_unattributed_preexisting_required_policy_in_the_actual16_schema() {
    let f = Fixture::new().await;
    shipped_two_line_source(&f).await;
    let mut tx = preceding_lot_schema(&f).await;
    sqlx::query("UPDATE wms_products SET data=data||'{\"expiry_required\":true}'::jsonb WHERE tenant=$1 AND sku='SKU-A'").bind(&f.identity.tenant).execute(&mut *tx).await.expect("unattributed older-schema field");
    let result = sqlx::raw_sql(include_str!("../migrations/017_lot_controls.sql"))
        .execute(&mut *tx)
        .await;
    assert_eq!(
        result
            .expect_err("unattributed policy must be repaired, not invented")
            .as_database_error()
            .and_then(|e| e.code())
            .as_deref(),
        Some("23514")
    );
    tx.rollback()
        .await
        .expect("remove only owned failed upgrade schema");
}

fn pack_definition(sku: &str, code: &str, factor: i64, enabled: bool) -> Command {
    Command::SetUnitPack {
        sku: sku.into(),
        code: code.into(),
        label: format!("Reviewed {code} pack"),
        each_per_pack: factor,
        enabled,
        reason: "Controller reviewed actual packaging label and each factor".into(),
    }
}
fn pack_input(
    definition: &str,
    sku: &str,
    code: &str,
    count: i64,
    factor: i64,
    target: &[&str],
) -> stateset_warehouse::units::Input {
    stateset_warehouse::units::Input {
        target: target.iter().map(|s| (*s).into()).collect(),
        sku: sku.into(),
        pack_code: code.into(),
        definition_id: definition.into(),
        pack_quantity: count,
        each_per_pack: factor,
    }
}
async fn packed_command(
    f: &Fixture,
    command: Command,
    inputs: Vec<stateset_warehouse::units::Input>,
) -> (Request, Value) {
    let request = Request {
        request_id: id(),
        expected_version: f.state().await.version,
        resource_versions: None,
        serial_inputs: Vec::new(),
        unit_inputs: inputs,
        command,
    };
    let token = if matches!(&request.command, Command::CountStock { .. }) {
        "admin-acceptance"
    } else {
        "operator-acceptance"
    };
    let (status, body) = f.submit(token, &request).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    (request, body)
}
#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn pack_units_preserve_original_receive_move_pick_unpick_count_and_plan_each_quantities_after_factor_changes(
) {
    let f = Fixture::new().await;
    f.seed_catalog().await;
    f.command(Command::CreateLocation {
        code: "B-01".into(),
        zone: "Storage".into(),
        kind: "storage".into(),
    })
    .await;
    let defined = f.command(pack_definition("SKU-A", "CASE", 12, true)).await;
    let def = entity(&defined);
    let (created, receipt) = packed_command(
        &f,
        Command::CreateReceipt {
            reference: "PACK-RECEIPT".into(),
            supplier: "Synthetic supplier".into(),
            lines: vec![ReceiptInput {
                id: "receipt-a".into(),
                sku: "SKU-A".into(),
                lot: "PACK-LOT".into(),
                quantity: 36,
            }],
        },
        vec![pack_input(&def, "SKU-A", "CASE", 3, 12, &["receipt-a"])],
    )
    .await;
    let rid = entity(&receipt["result"]);
    let receive = Command::Receive {
        receipt_id: rid.clone(),
        line_id: "receipt-a".into(),
        quantity: 24,
        location: "RECV-01".into(),
    };
    let (original, accepted) = packed_command(
        &f,
        receive.clone(),
        vec![pack_input(&def, "SKU-A", "CASE", 2, 12, &[])],
    )
    .await;
    packed_command(
        &f,
        Command::MoveStock {
            sku: "SKU-A".into(),
            lot: "PACK-LOT".into(),
            from: "RECV-01".into(),
            to: "A-01".into(),
            quantity: 24,
        },
        vec![pack_input(&def, "SKU-A", "CASE", 2, 12, &[])],
    )
    .await;
    let (_, order) = packed_command(
        &f,
        Command::CreateOrder {
            reference: "PACK-ORDER".into(),
            source_order_id: None,
            customer: "Synthetic customer".into(),
            destination: "Synthetic destination".into(),
            priority: "normal".into(),
            lines: vec![LineInput {
                id: "order-a".into(),
                sku: "SKU-A".into(),
                quantity: 24,
            }],
        },
        vec![pack_input(&def, "SKU-A", "CASE", 2, 12, &["order-a"])],
    )
    .await;
    let oid = entity(&order["result"]);
    f.command(Command::Allocate {
        order_id: oid.clone(),
    })
    .await;
    packed_command(
        &f,
        Command::Pick {
            order_id: oid.clone(),
            line_id: "order-a".into(),
            location: "A-01".into(),
            lot: "PACK-LOT".into(),
            quantity: 12,
        },
        vec![pack_input(&def, "SKU-A", "CASE", 1, 12, &[])],
    )
    .await;
    packed_command(
        &f,
        Command::Unpick {
            order_id: oid.clone(),
            line_id: "order-a".into(),
            location: "A-01".into(),
            lot: "PACK-LOT".into(),
            quantity: 12,
            reason: "Actual case returned from staging".into(),
        },
        vec![pack_input(&def, "SKU-A", "CASE", 1, 12, &[])],
    )
    .await;
    packed_command(
        &f,
        Command::CountStock {
            sku: "SKU-A".into(),
            lot: "PACK-LOT".into(),
            location: "A-01".into(),
            counted: 24,
            reason: "Two actual cases at this original bin".into(),
        },
        vec![pack_input(&def, "SKU-A", "CASE", 2, 12, &[])],
    )
    .await;
    packed_command(
        &f,
        Command::CountStock {
            sku: "SKU-A".into(),
            lot: "PACK-LOT".into(),
            location: "B-01".into(),
            counted: 0,
            reason: "No cases at this empty bin".into(),
        },
        vec![pack_input(&def, "SKU-A", "CASE", 0, 12, &[])],
    )
    .await;
    let before = f.state().await;
    assert_eq!(before.stocks.iter().map(|s| s.on_hand).sum::<i64>(), 24);
    assert_eq!(before.stocks.iter().map(|s| s.reserved).sum::<i64>(), 24);
    let changed = f.command(pack_definition("SKU-A", "CASE", 6, true)).await;
    let current = entity(&changed);
    for (request, accepted) in [(created, receipt), (original.clone(), accepted)] {
        let replay = f
            .store
            .execute(&f.identity, "wh-main", &request)
            .await
            .expect("original pack request replay");
        assert_eq!(replay["replayed"], true);
        assert_eq!(replay["result"], accepted["result"]);
    }
    let stale = Request {
        request_id: id(),
        expected_version: f.state().await.version,
        ..original
    };
    assert_eq!(
        f.store
            .execute(&f.identity, "wh-main", &stale)
            .await
            .expect_err("old factor cannot be freshened")
            .status,
        409
    );
    packed_command(
        &f,
        Command::Receive {
            receipt_id: rid,
            line_id: "receipt-a".into(),
            quantity: 12,
            location: "RECV-01".into(),
        },
        vec![pack_input(&current, "SKU-A", "CASE", 2, 6, &[])],
    )
    .await;
    let after = f.state().await;
    assert_eq!(after.stocks.iter().map(|s| s.on_hand).sum::<i64>(), 36);
    let proof = f
        .store
        .reconciliation(&f.identity.tenant, "wh-main")
        .await
        .expect("each and original conversion source");
    assert_eq!(proof["ok"], true);
    assert_eq!(proof["units"]["definition_changes"], 2);
    assert_eq!(proof["units"]["quantity_observations"], 9);
    let original_event:Value=sqlx::query_scalar("SELECT payload FROM wms_events WHERE tenant=$1 AND payload#>>'{command,type}'='receive' ORDER BY version LIMIT 1").bind(&f.identity.tenant).fetch_one(&f.store.pool).await.expect("immutable conversion event");
    assert_eq!(original_event["unit_inputs"][0]["pack_quantity"], 2);
    assert_eq!(original_event["unit_inputs"][0]["each_per_pack"], 12);
    assert_eq!(original_event["unit_inputs"][0]["definition_id"], def);
}
#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn pack_units_definition_aba_fences_old_scoped_reviews_but_another_sku_definition_does_not() {
    use stateset_warehouse::concurrency;
    let f = Fixture::new().await;
    f.seed_catalog().await;
    f.command(Command::CreateLocation {
        code: "B-01".into(),
        zone: "Storage".into(),
        kind: "storage".into(),
    })
    .await;
    f.command(opening_count(24)).await;
    let def = entity(&f.command(pack_definition("SKU-A", "CASE", 12, true)).await);
    let before = f.state().await;
    let command = Command::MoveStock {
        sku: "SKU-A".into(),
        lot: "LOT-A".into(),
        from: "A-01".into(),
        to: "B-01".into(),
        quantity: 12,
    };
    let original = Request {
        request_id: id(),
        expected_version: before.version,
        resource_versions: concurrency::reviewed_versions(&before, &command)
            .expect("exact resources"),
        serial_inputs: Vec::new(),
        unit_inputs: vec![pack_input(&def, "SKU-A", "CASE", 1, 12, &[])],
        command: command.clone(),
    };
    let _ = f.command(pack_definition("SKU-A", "INNER", 6, true)).await;
    assert_eq!(
        f.store
            .execute(&f.identity, "wh-main", &original)
            .await
            .expect_err("same product packaging changed")
            .status,
        409
    );
    let before = f.state().await;
    let reviewed = Request {
        request_id: id(),
        expected_version: before.version,
        resource_versions: concurrency::reviewed_versions(&before, &command)
            .expect("exact resources"),
        ..original.clone()
    };
    f.command(pack_definition("SKU-A", "CASE", 6, true)).await;
    f.command(pack_definition("SKU-A", "CASE", 12, true)).await;
    let error = f
        .store
        .execute(&f.identity, "wh-main", &reviewed)
        .await
        .expect_err("ABA cannot freshen a scan");
    assert_eq!(error.status, 409);
    assert!(error.command_outcome.is_some());
    let current = f.state().await;
    let forged = Request {
        request_id: id(),
        expected_version: current.version,
        resource_versions: concurrency::reviewed_versions(&current, &command)
            .expect("fresh resources"),
        ..reviewed
    };
    assert_eq!(
        f.store
            .execute(&f.identity, "wh-main", &forged)
            .await
            .expect_err("fresh resource map cannot change original definition")
            .status,
        409
    );
    f.command(Command::CreateProduct {
        serial_controlled: false,
        sku: "OTHER-PACK-SKU".into(),
        name: "Other product".into(),
        category: "Test".into(),
        reorder_point: 0,
    })
    .await;
    let state = f.state().await;
    let new_def = state
        .unit_packs
        .iter()
        .find(|p| p.sku == "SKU-A" && p.code == "CASE")
        .expect("current explicit definition")
        .id
        .clone();
    let next = Request {
        request_id: id(),
        expected_version: state.version,
        resource_versions: concurrency::reviewed_versions(&state, &command)
            .expect("captured exact resources"),
        serial_inputs: Vec::new(),
        unit_inputs: vec![pack_input(&new_def, "SKU-A", "CASE", 1, 12, &[])],
        command,
    };
    f.command(pack_definition("OTHER-PACK-SKU", "CASE", 24, true))
        .await;
    let accepted = f
        .store
        .execute(&f.identity, "wh-main", &next)
        .await
        .expect("unrelated product metadata permits original scoped work");
    assert_eq!(accepted["replayed"], false);
    assert_eq!(
        f.state()
            .await
            .stocks
            .iter()
            .find(|s| s.location == "B-01" && s.lot == "LOT-A")
            .expect("moved each")
            .on_hand,
        12
    );
    let after = f.state().await;
    f.command(pack_definition("SKU-A", "CASE", 12, false)).await;
    assert_eq!(
        after.stocks,
        f.state().await.stocks,
        "disabling a pack moves no custody"
    );
    let mut disabled = next.clone();
    disabled.request_id = id();
    disabled.expected_version = f.state().await.version;
    disabled.resource_versions = None;
    assert_eq!(
        f.store
            .execute(&f.identity, "wh-main", &disabled)
            .await
            .expect_err("disabled packaging cannot admit a new conversion")
            .status,
        409
    );
    assert_eq!(
        f.store
            .execute(&f.identity, "wh-main", &next)
            .await
            .expect("committed original still replays")["result"],
        accepted["result"]
    );
}
#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn pack_units_reject_wrong_targets_factors_skus_duplicates_overflow_and_operator_metadata_without_side_effects(
) {
    let f = Fixture::new().await;
    f.seed_catalog().await;
    let def = entity(&f.command(pack_definition("SKU-A", "CASE", 12, true)).await);
    let receipt = f
        .command(Command::CreateReceipt {
            reference: "UNIT-EXACT-LINES".into(),
            supplier: "Synthetic supplier".into(),
            lines: vec![
                ReceiptInput {
                    id: "a".into(),
                    sku: "SKU-A".into(),
                    lot: "L1".into(),
                    quantity: 24,
                },
                ReceiptInput {
                    id: "b".into(),
                    sku: "SKU-A".into(),
                    lot: "L2".into(),
                    quantity: 24,
                },
            ],
        })
        .await;
    let rid = entity(&receipt);
    let before = f.state().await;
    let counts = command_evidence_counts(&f).await;
    let valid = pack_input(&def, "SKU-A", "CASE", 1, 12, &[]);
    let command = Command::Receive {
        receipt_id: rid,
        line_id: "a".into(),
        quantity: 12,
        location: "RECV-01".into(),
    };
    let original = Request {
        request_id: id(),
        expected_version: before.version,
        resource_versions: None,
        serial_inputs: Vec::new(),
        unit_inputs: vec![valid.clone()],
        command,
    };
    let mut malformed = Vec::new();
    let mut v = valid.clone();
    v.target = vec!["b".into()];
    malformed.push(v);
    let mut v = valid.clone();
    v.sku = "OTHER-SKU".into();
    malformed.push(v);
    let mut v = valid.clone();
    v.each_per_pack = 6;
    malformed.push(v);
    let mut v = valid.clone();
    v.pack_quantity = 0;
    malformed.push(v);
    let mut v = valid.clone();
    v.pack_quantity = -1;
    malformed.push(v);
    let mut v = valid.clone();
    v.each_per_pack = 1_000_000;
    v.pack_quantity = 1_000_000;
    malformed.push(v);
    let mut v = valid.clone();
    v.pack_code = "EA".into();
    malformed.push(v);
    for v in malformed {
        let request = Request {
            request_id: id(),
            serial_inputs: Vec::new(),
            unit_inputs: vec![v],
            ..original.clone()
        };
        assert_eq!(
            f.store
                .execute(&f.identity, "wh-main", &request)
                .await
                .expect_err("invalid conversion")
                .status,
            422
        );
    }
    let duplicate = Request {
        request_id: id(),
        serial_inputs: Vec::new(),
        unit_inputs: vec![valid.clone(), valid],
        ..original.clone()
    };
    assert_eq!(
        f.store
            .execute(&f.identity, "wh-main", &duplicate)
            .await
            .expect_err("one observation per target")
            .status,
        422
    );
    let request = Request {
        request_id: id(),
        serial_inputs: Vec::new(),
        unit_inputs: Vec::new(),
        command: pack_definition("SKU-A", "CASE", 6, true),
        ..original
    };
    assert_eq!(
        f.submit("operator-acceptance", &request).await.0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(f.state().await, before);
    assert_eq!(command_evidence_counts(&f).await, counts);
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn pack_units_read_pages_bind_original_scope_review_and_source_with_roles_and_tenant_isolation(
) {
    let f = Fixture::new().await;
    f.seed_catalog().await;
    let def = entity(&f.command(pack_definition("SKU-A", "CASE", 12, true)).await);
    f.command(pack_definition("SKU-A", "INNER", 6, true)).await;
    let (_, created) = packed_command(
        &f,
        Command::CreateReceipt {
            reference: "PAGE-PACK".into(),
            supplier: "Synthetic".into(),
            lines: vec![ReceiptInput {
                id: "r1".into(),
                sku: "SKU-A".into(),
                lot: "PAGED-LOT".into(),
                quantity: 24,
            }],
        },
        vec![pack_input(&def, "SKU-A", "CASE", 2, 12, &["r1"])],
    )
    .await;
    let receipt = created["result"]["entity_id"]
        .as_str()
        .expect("isolated pack-unit fixture and original source evidence")
        .to_owned();
    packed_command(
        &f,
        Command::Receive {
            receipt_id: receipt,
            line_id: "r1".into(),
            location: "RECV-01".into(),
            quantity: 24,
        },
        vec![pack_input(&def, "SKU-A", "CASE", 2, 12, &[])],
    )
    .await;
    f.command(pack_definition("SKU-A", "CASE", 6, true)).await;
    f.command(pack_definition("SKU-A", "INNER", 6, false)).await;
    let version = f.state().await.version;
    let path = |endpoint: &str, pairs: &[(&str, String)]| {
        let mut u =
            reqwest::Url::parse(&format!("http://isolated/v1/warehouses/wh-main/{endpoint}"))
                .expect("isolated pack-unit fixture and original source evidence");
        u.query_pairs_mut()
            .extend_pairs(pairs.iter().map(|(k, v)| (*k, v.as_str())));
        format!(
            "{}?{}",
            u.path(),
            u.query()
                .expect("isolated pack-unit fixture and original source evidence")
        )
    };
    for (endpoint, history, total) in [
        ("unit-packs", None, 2),
        ("unit-packs", Some("true"), 4),
        ("unit-observations", None, 2),
    ] {
        let mut params = vec![
            ("sku", "SKU-A".into()),
            ("limit", "1".into()),
            ("review", version.to_string()),
        ];
        if let Some(h) = history {
            params.push(("history", h.into()));
        }
        let (status, first) = f
            .http("GET", &path(endpoint, &params), "viewer-acceptance", None)
            .await;
        assert_eq!(status, StatusCode::OK, "{first}");
        assert_eq!(first["total_records"], total);
        assert_eq!(first["page_source_ok"], true);
        assert_eq!(
            first["items"]
                .as_array()
                .expect("isolated pack-unit fixture and original source evidence")
                .len(),
            1
        );
        let cursor = first["next_cursor"]
            .as_str()
            .expect("isolated pack-unit fixture and original source evidence")
            .to_owned();
        let mut seen = vec![first["items"][0].clone()];
        let mut next = Some(cursor.clone());
        while let Some(c) = next {
            let mut more = params.clone();
            more.push(("after", c));
            let (status, p) = f
                .http("GET", &path(endpoint, &more), "operator-acceptance", None)
                .await;
            assert_eq!(status, StatusCode::OK, "{p}");
            assert_eq!(p["page_source_ok"], true);
            seen.extend(
                p["items"]
                    .as_array()
                    .expect("isolated pack-unit fixture and original source evidence")
                    .iter()
                    .cloned(),
            );
            next = p["next_cursor"].as_str().map(str::to_owned);
            assert!(seen.len() <= total as usize);
        }
        assert_eq!(seen.len(), total as usize);
        if endpoint == "unit-observations" {
            for row in &seen {
                assert_eq!(row["definition_id"], def);
                assert_eq!(row["each_per_pack"], 12);
                assert_eq!(row["each_quantity"], 24);
            }
        } else if history.is_none() {
            assert_eq!(seen[0]["code"], "CASE");
            assert_eq!(seen[0]["each_per_pack"], 6);
            assert_eq!(seen[1]["code"], "INNER");
            assert_eq!(seen[1]["enabled"], false);
        }
        for (key, value) in [
            ("sku", "SKU-B".to_string()),
            ("code", "CASE".to_string()),
            ("review", (version - 1).to_string()),
        ] {
            let mut wrong = params.clone();
            wrong.retain(|(k, _)| *k != key);
            wrong.push((key, value));
            wrong.push(("after", cursor.clone()));
            let (status, _) = f
                .http("GET", &path(endpoint, &wrong), "viewer-acceptance", None)
                .await;
            assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        }
    }
    for endpoint in ["unit-packs", "unit-observations"] {
        for bad in [
            vec![],
            vec![("sku", "SKU-A".into()), ("limit", "101".into())],
            vec![("sku", "SKU-A".into()), ("after", "{}".into())],
        ] {
            assert_eq!(
                f.http("GET", &path(endpoint, &bad), "viewer-acceptance", None)
                    .await
                    .0,
                StatusCode::UNPROCESSABLE_ENTITY
            );
        }
        let params = vec![("sku", "SKU-A".into())];
        assert_eq!(
            f.http("GET", &path(endpoint, &params), "outsider-acceptance", None)
                .await
                .0,
            StatusCode::NOT_FOUND
        );
    }
    let params = vec![("sku", "SKU-A".into()), ("limit", "1".into())];
    let (_, old) = f
        .http(
            "GET",
            &path("unit-packs", &params),
            "viewer-acceptance",
            None,
        )
        .await;
    f.command(pack_definition("SKU-A", "PALLET", 120, true))
        .await;
    let mut stale = params;
    stale.push((
        "after",
        old["next_cursor"]
            .as_str()
            .expect("isolated pack-unit fixture and original source evidence")
            .into(),
    ));
    assert_eq!(
        f.http(
            "GET",
            &path("unit-packs", &stale),
            "viewer-acceptance",
            None
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn pack_units_immutable_sources_detect_stock_balanced_forgery_and_missing_observations() {
    let f = Fixture::new().await;
    f.seed_catalog().await;
    let def = entity(&f.command(pack_definition("SKU-A", "CASE", 12, true)).await);
    packed_command(
        &f,
        Command::CountStock {
            sku: "SKU-A".into(),
            lot: "PACK".into(),
            location: "A-01".into(),
            counted: 24,
            reason: "Synthetic observed two cases".into(),
        },
        vec![pack_input(&def, "SKU-A", "CASE", 2, 12, &[])],
    )
    .await;
    for sql in [
        "UPDATE wms_unit_pack_changes SET reason='rewritten' WHERE tenant=$1",
        "DELETE FROM wms_unit_pack_changes WHERE tenant=$1",
        "UPDATE wms_unit_observations SET target='[\"forged\"]' WHERE tenant=$1",
        "DELETE FROM wms_unit_observations WHERE tenant=$1",
    ] {
        assert!(sqlx::query(sql)
            .bind(&f.identity.tenant)
            .execute(&f.store.pool)
            .await
            .is_err());
    }
    for (table,sql) in [
        ("wms_unit_pack_changes","UPDATE wms_unit_pack_changes SET each_per_pack=6 WHERE tenant=$1"),
        ("wms_unit_observations","UPDATE wms_unit_observations SET pack_quantity=4,each_per_pack=6 WHERE tenant=$1"),
        ("wms_unit_observations","DELETE FROM wms_unit_observations WHERE tenant=$1"),
        ("wms_events","UPDATE wms_events SET payload=jsonb_set(payload,'{unit_inputs,0,pack_quantity}','4') WHERE tenant=$1 AND payload?'unit_inputs'"),
        ("wms_events","UPDATE wms_events SET payload=jsonb_set(payload,'{command,type}','\"count_stock\"') WHERE tenant=$1 AND payload#>>'{command,type}'='set_unit_pack'"),
    ] {
        let mut tx=f.store.pool.begin().await.expect("isolated pack-unit fixture and original source evidence");
        sqlx::raw_sql(&format!("ALTER TABLE {table} DISABLE TRIGGER USER")).execute(&mut *tx).await.expect("isolated pack-unit fixture and original source evidence");
        sqlx::query(sql).bind(&f.identity.tenant).execute(&mut *tx).await.expect("isolated pack-unit fixture and original source evidence");
        let ok:bool=sqlx::query_scalar("SELECT wms_unit_scope_valid($1,'wh-main')").bind(&f.identity.tenant).fetch_one(&mut *tx).await.expect("isolated pack-unit fixture and original source evidence");
        assert!(!ok,"source corruption cannot be hidden by unchanged each balances: {sql}");
        tx.rollback().await.expect("isolated pack-unit fixture and original source evidence");
    }
    let proof = f
        .store
        .reconciliation(&f.identity.tenant, "wh-main")
        .await
        .expect("isolated pack-unit fixture and original source evidence");
    assert_eq!(proof["units"]["ok"], true);
    assert_eq!(proof["ok"], true);
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn pack_units_definitions_and_observations_roll_back_at_final_delivery_failure_and_retry_once(
) {
    let f = Fixture::new().await;
    f.seed_catalog().await;
    let mut def = String::new();
    for metadata in [true, false] {
        let before = f.state().await;
        let counts = command_evidence_counts(&f).await;
        let original = Request {
            request_id: id(),
            expected_version: before.version,
            resource_versions: None,
            serial_inputs: Vec::new(),
            unit_inputs: if metadata {
                vec![]
            } else {
                vec![pack_input(&def, "SKU-A", "CASE", 2, 12, &[])]
            },
            command: if metadata {
                pack_definition("SKU-A", "CASE", 12, true)
            } else {
                Command::CountStock {
                    sku: "SKU-A".into(),
                    lot: "PACK".into(),
                    location: "A-01".into(),
                    counted: 24,
                    reason: "Observed synthetic two cases".into(),
                }
            },
        };
        let trigger = format!("unit_fault_{}", id().replace('-', ""));
        sqlx::raw_sql(&format!("CREATE FUNCTION {trigger}() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.tenant='{}' THEN RAISE EXCEPTION 'injected unit delivery failure'; END IF; RETURN NEW; END $$; CREATE TRIGGER {trigger} BEFORE INSERT ON wms_outbox FOR EACH ROW EXECUTE FUNCTION {trigger}()",f.identity.tenant)).execute(&f.store.pool).await.expect("isolated pack-unit fixture and original source evidence");
        let failed = f.store.execute(&f.identity, "wh-main", &original).await;
        sqlx::raw_sql(&format!(
            "DROP TRIGGER {trigger} ON wms_outbox; DROP FUNCTION {trigger}()"
        ))
        .execute(&f.store.pool)
        .await
        .expect("isolated pack-unit fixture and original source evidence");
        assert_eq!(
            failed
                .expect_err("invalid pack-unit request must be rejected")
                .status,
            503
        );
        assert_eq!(before, f.state().await);
        assert_eq!(counts, command_evidence_counts(&f).await);
        let observations: i64 =
            sqlx::query_scalar("SELECT count(*) FROM wms_unit_observations WHERE tenant=$1")
                .bind(&f.identity.tenant)
                .fetch_one(&f.store.pool)
                .await
                .expect("isolated pack-unit fixture and original source evidence");
        assert_eq!(observations, 0);
        let saved = f
            .store
            .execute(&f.identity, "wh-main", &original)
            .await
            .expect("isolated pack-unit fixture and original source evidence");
        if metadata {
            def = entity(&saved["result"]);
        }
        let after = command_evidence_counts(&f).await;
        assert_eq!(
            saved["result"],
            f.store
                .execute(&f.identity, "wh-main", &original)
                .await
                .expect("isolated pack-unit fixture and original source evidence")["result"]
        );
        assert_eq!(after, command_evidence_counts(&f).await);
    }
    assert_eq!(
        f.state()
            .await
            .stocks
            .iter()
            .filter(|s| s.sku == "SKU-A")
            .map(|s| s.on_hand)
            .sum::<i64>(),
        24
    );
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn pack_units_changed_definition_and_observation_serialize_at_the_original_review() {
    let f = Fixture::new().await;
    f.seed_catalog().await;
    let def = entity(&f.command(pack_definition("SKU-A", "CASE", 12, true)).await);
    let before = f.state().await;
    let change = Request {
        request_id: id(),
        expected_version: before.version,
        resource_versions: None,
        serial_inputs: Vec::new(),
        unit_inputs: vec![],
        command: pack_definition("SKU-A", "CASE", 6, true),
    };
    let count = Request {
        request_id: id(),
        expected_version: before.version,
        resource_versions: None,
        serial_inputs: Vec::new(),
        unit_inputs: vec![pack_input(&def, "SKU-A", "CASE", 2, 12, &[])],
        command: Command::CountStock {
            sku: "SKU-A".into(),
            lot: "PACK".into(),
            location: "A-01".into(),
            counted: 24,
            reason: "Observed synthetic two original cases".into(),
        },
    };
    let (a, b) = tokio::join!(
        f.store.execute(&f.identity, "wh-main", &change),
        f.store.execute(&f.identity, "wh-main", &count)
    );
    assert_ne!(a.is_ok(), b.is_ok());
    if a.is_ok() {
        assert_eq!(
            b.expect_err("invalid pack-unit request must be rejected")
                .status,
            409
        );
        assert!(f.state().await.stocks.is_empty());
    } else {
        assert_eq!(
            a.expect_err("invalid pack-unit request must be rejected")
                .status,
            409
        );
        assert_eq!(
            f.state()
                .await
                .stocks
                .iter()
                .map(|s| s.on_hand)
                .sum::<i64>(),
            24
        );
    }
    assert_eq!(
        f.store
            .reconciliation(&f.identity.tenant, "wh-main")
            .await
            .expect("isolated pack-unit fixture and original source evidence")["units"]["ok"],
        true
    );
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn pack_units_sql_cap_rejects_twenty_first_enabled_definition_with_complete_original_source()
{
    use sha2::{Digest, Sha256};
    let f = Fixture::new().await;
    f.seed_catalog().await;
    let mut last = String::new();
    for n in 0..20 {
        last = entity(
            &f.command(pack_definition("SKU-A", &format!("PACK{n}"), 12, true))
                .await,
        );
    }
    let before = f.state().await;
    f.reject(
        pack_definition("SKU-A", "EXCESS", 12, true),
        StatusCode::UNPROCESSABLE_ENTITY,
    )
    .await;
    let mut record: Value = sqlx::query_scalar(
        "SELECT to_jsonb(p) FROM wms_unit_pack_changes p WHERE tenant=$1 AND id=$2",
    )
    .bind(&f.identity.tenant)
    .bind(&last)
    .fetch_one(&f.store.pool)
    .await
    .expect("isolated pack-unit fixture and original source evidence");
    let mut event: Value = sqlx::query_scalar("SELECT payload FROM wms_events WHERE id=$1")
        .bind(
            record["source_event_id"]
                .as_str()
                .expect("isolated pack-unit fixture and original source evidence"),
        )
        .fetch_one(&f.store.pool)
        .await
        .expect("isolated pack-unit fixture and original source evidence");
    let rid = id();
    let eid = id();
    let request_id = id();
    let version = before.version + 1;
    record["id"] = json!(rid);
    record["source_event_id"] = json!(eid);
    record["version"] = json!(version);
    record["code"] = json!("EXCESS");
    event["id"] = json!(eid);
    event["version"] = json!(version);
    event["request_id"] = json!(request_id);
    event["command"]["code"] = json!("EXCESS");
    event["result"] =
        json!({"entity_id":rid,"version":version,"type":"inventory.unit_pack_changed"});
    let mut doc = record.clone();
    doc.as_object_mut()
        .expect("isolated pack-unit fixture and original source evidence")
        .remove("tenant");
    event["unit_pack_change"] = doc;
    let r = Request {
        request_id: request_id.clone(),
        expected_version: before.version,
        resource_versions: None,
        serial_inputs: Vec::new(),
        unit_inputs: vec![],
        command: serde_json::from_value(event["command"].clone())
            .expect("isolated pack-unit fixture and original source evidence"),
    };
    let digest = format!(
        "{:x}",
        Sha256::digest(
            serde_json::to_vec(&r)
                .expect("isolated pack-unit fixture and original source evidence")
        )
    );
    let mut tx = f
        .store
        .pool
        .begin()
        .await
        .expect("isolated pack-unit fixture and original source evidence");
    sqlx::query("UPDATE wms_warehouses SET version=$2 WHERE tenant=$1 AND warehouse_id='wh-main'")
        .bind(&f.identity.tenant)
        .bind(version)
        .execute(&mut *tx)
        .await
        .expect("isolated pack-unit fixture and original source evidence");
    sqlx::query("INSERT INTO wms_commands(tenant,warehouse_id,request_id,actor,digest,result) VALUES($1,'wh-main',$2,$3,$4,$5)").bind(&f.identity.tenant).bind(&request_id).bind(&f.identity.actor).bind(digest).bind(&event["result"]).execute(&mut *tx).await.expect("isolated pack-unit fixture and original source evidence");
    sqlx::query("INSERT INTO wms_events(id,tenant,warehouse_id,version,payload) VALUES($1,$2,'wh-main',$3,$4)").bind(&eid).bind(&f.identity.tenant).bind(version).bind(event).execute(&mut *tx).await.expect("isolated pack-unit fixture and original source evidence");
    sqlx::query("INSERT INTO wms_unit_pack_changes SELECT (jsonb_populate_record(NULL::wms_unit_pack_changes,$1)).*").bind(record).execute(&mut *tx).await.expect("isolated pack-unit fixture and original source evidence");
    let sourced: bool = sqlx::query_scalar(
        "SELECT wms_unit_pack_original(p) FROM wms_unit_pack_changes p WHERE tenant=$1 AND id=$2",
    )
    .bind(&f.identity.tenant)
    .bind(&rid)
    .fetch_one(&mut *tx)
    .await
    .expect("isolated pack-unit fixture and original source evidence");
    assert!(
        sourced,
        "cap test must have an otherwise complete original source"
    );
    let rejected = sqlx::raw_sql("SET CONSTRAINTS wms_unit_pack_proof IMMEDIATE")
        .execute(&mut *tx)
        .await
        .expect_err("invalid pack-unit request must be rejected");
    assert_eq!(
        rejected
            .as_database_error()
            .and_then(|e| e.code())
            .as_deref(),
        Some("23514")
    );
    assert!(rejected
        .as_database_error()
        .expect("isolated pack-unit fixture and original source evidence")
        .message()
        .contains("twenty"));
    tx.rollback()
        .await
        .expect("isolated pack-unit fixture and original source evidence");
    assert_eq!(before, f.state().await);
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn pack_units_actual19_upgrade_preserves_legacy_receipts_events_and_each_without_inferred_packs(
) {
    let f = Fixture::new().await;
    f.seed_catalog().await;
    let (original, accepted) = packed_command(
        &f,
        Command::CountStock {
            sku: "SKU-A".into(),
            lot: "LEGACY".into(),
            location: "A-01".into(),
            counted: 24,
            reason: "Original witnessed each units".into(),
        },
        vec![],
    )
    .await;
    let schema = format!("unit_upgrade_{}", id().replace('-', "_"));
    let mut tx = f
        .store
        .pool
        .begin()
        .await
        .expect("isolated pack-unit fixture and original source evidence");
    sqlx::raw_sql(&format!(
        "CREATE SCHEMA {schema}; SET LOCAL search_path TO {schema},public"
    ))
    .execute(&mut *tx)
    .await
    .expect("isolated pack-unit fixture and original source evidence");
    let mut files: Vec<_> = std::fs::read_dir(concat!(env!("CARGO_MANIFEST_DIR"), "/migrations"))
        .expect("isolated pack-unit fixture and original source evidence")
        .map(|e| {
            e.expect("isolated pack-unit fixture and original source evidence")
                .path()
        })
        .filter(|p| {
            p.extension().is_some_and(|s| s == "sql")
                && p.file_name()
                    .expect("isolated pack-unit fixture and original source evidence")
                    .to_string_lossy()
                    .as_ref()
                    < "020_"
        })
        .collect();
    files.sort();
    assert_eq!(files.len(), 19);
    for p in files {
        sqlx::raw_sql(
            &std::fs::read_to_string(p)
                .expect("isolated pack-unit fixture and original source evidence"),
        )
        .execute(&mut *tx)
        .await
        .expect("isolated pack-unit fixture and original source evidence");
    }
    sqlx::raw_sql("SET LOCAL session_replication_role='replica'")
        .execute(&mut *tx)
        .await
        .expect("isolated pack-unit fixture and original source evidence");
    let tables:Vec<String>=sqlx::query_scalar("SELECT tablename FROM pg_tables p WHERE schemaname=$1 AND EXISTS(SELECT 1 FROM information_schema.columns c WHERE c.table_schema=p.schemaname AND c.table_name=p.tablename AND c.column_name='tenant') ORDER BY tablename").bind(&schema).fetch_all(&mut *tx).await.expect("isolated pack-unit fixture and original source evidence");
    assert_eq!(tables.len(), 39);
    for table in &tables {
        assert!(table.bytes().all(|b| b.is_ascii_lowercase() || b == b'_'));
        sqlx::query(&format!(
            "INSERT INTO {table} SELECT * FROM public.{table} WHERE tenant=$1"
        ))
        .bind(&f.identity.tenant)
        .execute(&mut *tx)
        .await
        .expect("isolated pack-unit fixture and original source evidence");
    }
    sqlx::raw_sql("SET LOCAL session_replication_role='origin'")
        .execute(&mut *tx)
        .await
        .expect("isolated pack-unit fixture and original source evidence");
    let mut before = Vec::new();
    for table in &tables {
        let image:Value=sqlx::query_scalar(&format!("SELECT COALESCE(jsonb_agg(to_jsonb(r) ORDER BY to_jsonb(r)::text),'[]') FROM {table} r")).fetch_one(&mut *tx).await.expect("isolated pack-unit fixture and original source evidence");
        before.push(image);
    }
    sqlx::raw_sql(include_str!("../migrations/020_pack_units.sql"))
        .execute(&mut *tx)
        .await
        .expect("isolated pack-unit fixture and original source evidence");
    for (table, expected) in tables.iter().zip(&before) {
        let image:Value=sqlx::query_scalar(&format!("SELECT COALESCE(jsonb_agg(to_jsonb(r) ORDER BY to_jsonb(r)::text),'[]') FROM {table} r")).fetch_one(&mut *tx).await.expect("isolated pack-unit fixture and original source evidence");
        assert_eq!(*expected, image, "upgrade changed original {table}");
    }
    let invented:i64=sqlx::query_scalar("SELECT (SELECT count(*) FROM wms_unit_pack_changes)+(SELECT count(*) FROM wms_unit_observations)").fetch_one(&mut *tx).await.expect("isolated pack-unit fixture and original source evidence");
    assert_eq!(invented, 0);
    tx.commit()
        .await
        .expect("isolated pack-unit fixture and original source evidence");
    let scope = schema.clone();
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(2)
        .after_connect(move |c, _| {
            let statement = format!("SET search_path TO {scope},public");
            Box::pin(async move {
                sqlx::query(&statement).execute(c).await?;
                Ok(())
            })
        })
        .connect(
            &std::env::var("WMS_TEST_DATABASE_URL")
                .expect("isolated pack-unit fixture and original source evidence"),
        )
        .await
        .expect("isolated pack-unit fixture and original source evidence");
    let upgraded = Store { pool };
    let replay = upgraded
        .execute(&f.identity, "wh-main", &original)
        .await
        .expect("isolated pack-unit fixture and original source evidence");
    assert_eq!(replay["replayed"], true);
    assert_eq!(replay["result"], accepted["result"]);
    assert_eq!(
        upgraded
            .state(&f.identity.tenant, "wh-main")
            .await
            .expect("isolated pack-unit fixture and original source evidence")
            .stocks
            .iter()
            .map(|s| s.on_hand)
            .sum::<i64>(),
        24
    );
    assert_eq!(
        upgraded
            .reconciliation(&f.identity.tenant, "wh-main")
            .await
            .expect("isolated pack-unit fixture and original source evidence")["units"]["ok"],
        true
    );
    upgraded.pool.close().await;
    sqlx::raw_sql(&format!("DROP SCHEMA {schema} CASCADE"))
        .execute(&f.store.pool)
        .await
        .expect("isolated pack-unit fixture and original source evidence");
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn pack_units_transfer_loss_and_arrival_use_each_facility_original_definition_without_stock_credit(
) {
    let f = transfer_fixture().await;
    let source = entity(&f.command(pack_definition("SKU-A", "CASE", 2, true)).await);
    let (_, dst) = transfer_admin(&f, pack_definition("SKU-A", "CASE", 1, true)).await;
    let destination = entity(&dst["result"]);
    let (_, dispatched) = packed_command(
        &f,
        dispatch_transfer(),
        vec![pack_input(&source, "SKU-A", "CASE", 1, 2, &["b"])],
    )
    .await;
    let tid = entity(&dispatched["result"]);
    let at = |command: Command, inputs: Vec<stateset_warehouse::units::Input>| async {
        let r = Request {
            request_id: id(),
            expected_version: f
                .store
                .state(&f.identity.tenant, "wh-east")
                .await
                .expect("isolated pack-unit fixture and original source evidence")
                .version,
            resource_versions: None,
            serial_inputs: Vec::new(),
            unit_inputs: inputs,
            command,
        };
        let (status, v) = f
            .http(
                "POST",
                "/v1/warehouses/wh-east/commands",
                "admin-acceptance",
                Some(json!(r)),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{v}");
        v
    };
    let loss = at(
        transfer_loss(&tid, "a", 2),
        vec![pack_input(&destination, "SKU-A", "CASE", 2, 1, &[])],
    )
    .await;
    let before = f
        .store
        .state(&f.identity.tenant, "wh-east")
        .await
        .expect("isolated pack-unit fixture and original source evidence")
        .stocks;
    at(
        Command::ReverseTransferLoss {
            loss_id: entity(&loss["result"]),
            quantity: 1,
            reason: "One actual missing unit located".into(),
        },
        vec![pack_input(&destination, "SKU-A", "CASE", 1, 1, &[])],
    )
    .await;
    assert_eq!(
        before,
        f.store
            .state(&f.identity.tenant, "wh-east")
            .await
            .expect("isolated pack-unit fixture and original source evidence")
            .stocks,
        "reversal cannot credit stock"
    );
    let wrong = Request {
        request_id: id(),
        expected_version: f
            .store
            .state(&f.identity.tenant, "wh-east")
            .await
            .expect("isolated pack-unit fixture and original source evidence")
            .version,
        resource_versions: None,
        serial_inputs: Vec::new(),
        unit_inputs: vec![pack_input(&source, "SKU-A", "CASE", 1, 2, &[])],
        command: transfer_arrival(&tid, "a", 2),
    };
    assert_eq!(
        f.http(
            "POST",
            "/v1/warehouses/wh-east/commands",
            "admin-acceptance",
            Some(json!(wrong))
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    at(
        transfer_arrival(&tid, "a", 1),
        vec![pack_input(&destination, "SKU-A", "CASE", 1, 1, &[])],
    )
    .await;
    for w in ["wh-main", "wh-east"] {
        assert_eq!(
            f.store
                .reconciliation(&f.identity.tenant, w)
                .await
                .expect("isolated pack-unit fixture and original source evidence")["units"]["ok"],
            true
        );
    }
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn pack_units_exact_package_tuple_and_return_authorization_line_keep_original_shipped_lot_allowance(
) {
    let f = Fixture::new().await;
    f.seed_catalog().await;
    f.command(opening_count(24)).await;
    let def = entity(&f.command(pack_definition("SKU-A", "CASE", 12, true)).await);
    let order = f
        .command(Command::CreateOrder {
            reference: "CASE-RETURN".into(),
            source_order_id: None,
            customer: "Synthetic".into(),
            destination: "Test".into(),
            priority: "normal".into(),
            lines: vec![LineInput {
                id: "a".into(),
                sku: "SKU-A".into(),
                quantity: 24,
            }],
        })
        .await;
    let oid = entity(&order);
    f.command(Command::Allocate {
        order_id: oid.clone(),
    })
    .await;
    packed_command(
        &f,
        Command::Pick {
            order_id: oid.clone(),
            line_id: "a".into(),
            location: "A-01".into(),
            lot: "LOT-A".into(),
            quantity: 24,
        },
        vec![pack_input(&def, "SKU-A", "CASE", 2, 12, &[])],
    )
    .await;
    let pack = Command::PackShipment {
        order_id: oid.clone(),
        reference: "CASE-PACKAGE".into(),
        lines: vec![stateset_warehouse::split_fulfillment::LineInput {
            line_id: "a".into(),
            location: "A-01".into(),
            lot: "LOT-A".into(),
            quantity: 12,
        }],
    };
    let wrong = Request {
        request_id: id(),
        expected_version: f.state().await.version,
        resource_versions: None,
        serial_inputs: Vec::new(),
        unit_inputs: vec![pack_input(
            &def,
            "SKU-A",
            "CASE",
            1,
            12,
            &["a", "A-01", "OTHER-LOT"],
        )],
        command: pack.clone(),
    };
    assert_eq!(
        f.submit("admin-acceptance", &wrong).await.0,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    let (_, packed) = packed_command(
        &f,
        pack,
        vec![pack_input(
            &def,
            "SKU-A",
            "CASE",
            1,
            12,
            &["a", "A-01", "LOT-A"],
        )],
    )
    .await;
    f.command(Command::HandoffShipment {
        shipment_id: entity(&packed["result"]),
        carrier: "Synthetic".into(),
        handoff_reference: "EXACT-CASE-HANDOFF".into(),
    })
    .await;
    let (_, returned) = packed_command(
        &f,
        Command::CreateReturn {
            order_id: oid,
            reference: "CASE-RMA".into(),
            reason: "Inspect original shipped case".into(),
            lines: vec![stateset_warehouse::returns::ReturnLineInput {
                id: "rma-a".into(),
                order_line_id: "a".into(),
                lot: "LOT-A".into(),
                quantity: 12,
            }],
        },
        vec![pack_input(&def, "SKU-A", "CASE", 1, 12, &["rma-a"])],
    )
    .await;
    packed_command(
        &f,
        Command::Receive {
            receipt_id: entity(&returned["result"]),
            line_id: "rma-a".into(),
            location: "RECV-01".into(),
            quantity: 12,
        },
        vec![pack_input(&def, "SKU-A", "CASE", 1, 12, &[])],
    )
    .await;
    let proof = f
        .store
        .reconciliation(&f.identity.tenant, "wh-main")
        .await
        .expect("isolated pack-unit fixture and original source evidence");
    assert_eq!(proof["ok"], true);
    assert_eq!(proof["units"]["quantity_observations"], 4);
}
