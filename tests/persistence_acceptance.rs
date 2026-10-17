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
            request_id: id(),
            expected_version: self.state().await.version,
            command,
        };
        let (status, response) = self.submit(token, &request).await;
        assert_eq!(status, StatusCode::OK, "{response}");
        response["result"].clone()
    }

    async fn reject(&self, command: Command, expected: StatusCode) {
        let before = self.state().await;
        let request = Request {
            request_id: id(),
            expected_version: before.version,
            command,
        };
        let (status, response) = self.submit("admin-acceptance", &request).await;
        assert_eq!(status, expected, "{response}");
        assert_eq!(self.state().await, before, "rejected work mutated state");
    }

    async fn seed_catalog(&self) {
        self.command(Command::CreateProduct {
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
        request_id: id(),
        expected_version: f.state().await.version,
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
        request_id: id(),
        expected_version: f.state().await.version,
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
