//! Adversarial regressions for normalized persistence and the legacy cutover.
//! Run only against an isolated database whose name contains `test`.
use serde_json::{json, Value};
use stateset_warehouse::{
    domain::{id, Command, LineInput, ReceiptInput, State, Warehouse},
    store::{Identity, Request, Store},
};

struct Fixture {
    store: Store,
    identity: Identity,
}

impl Fixture {
    async fn new() -> Self {
        let url = std::env::var("WMS_TEST_DATABASE_URL")
            .expect("WMS_TEST_DATABASE_URL must identify an isolated Postgres database");
        assert!(
            reqwest::Url::parse(&url)
                .expect("Postgres URL")
                .path()
                .contains("test"),
            "Never run persistence guards against a production database"
        );
        let store = Store::connect(&url)
            .await
            .expect("real Postgres connection");
        let identity = Identity {
            tenant: format!("guards-{}", id()),
            actor: "guards:admin".into(),
            role: "admin".into(),
        };
        store
            .provision(&identity.tenant, false)
            .await
            .expect("isolated warehouse");
        let fixture = Self { store, identity };
        fixture
            .command(Command::CreateProduct {
                serial_controlled: false,
                sku: "SKU".into(),
                name: "Guard test product".into(),
                category: "General".into(),
                reorder_point: 0,
            })
            .await;
        fixture
    }

    async fn state(&self) -> State {
        self.store
            .state(&self.identity.tenant, "wh-main")
            .await
            .expect("warehouse state")
    }

    async fn command(&self, command: Command) -> String {
        let request = Request {
            serial_inputs: Vec::new(),
            unit_inputs: Vec::new(),
            request_id: id(),
            expected_version: self.state().await.version,
            resource_versions: None,
            command,
        };
        self.store
            .execute(&self.identity, "wh-main", &request)
            .await
            .expect("fixture command")["result"]["entity_id"]
            .as_str()
            .expect("entity ID")
            .into()
    }

    async fn evidence_counts(&self) -> Value {
        sqlx::query_scalar(
            "SELECT jsonb_build_object(
              'commands',(SELECT count(*) FROM wms_commands WHERE tenant=$1),
              'events',(SELECT count(*) FROM wms_events WHERE tenant=$1),
              'outbox',(SELECT count(*) FROM wms_outbox WHERE tenant=$1),
              'activity',(SELECT count(*) FROM wms_activity WHERE tenant=$1),
              'movements',(SELECT count(*) FROM wms_movements WHERE tenant=$1))",
        )
        .bind(&self.identity.tenant)
        .fetch_one(&self.store.pool)
        .await
        .expect("evidence counts")
    }
}

fn order(reference: &str, source: &str) -> Command {
    Command::CreateOrder {
        reference: reference.into(),
        source_order_id: Some(source.into()),
        customer: "Customer".into(),
        destination: "Vancouver".into(),
        priority: "normal".into(),
        lines: vec![LineInput {
            id: "line-1".into(),
            sku: "SKU".into(),
            quantity: 1,
        }],
    }
}

fn receipt(reference: &str) -> Command {
    Command::CreateReceipt {
        reference: reference.into(),
        supplier: "Supplier".into(),
        lines: vec![ReceiptInput {
            id: "receipt-line-1".into(),
            sku: "SKU".into(),
            lot: "LOT".into(),
            quantity: 1,
        }],
    }
}

#[tokio::test]
#[ignore = "requires isolated Postgres; WMS_TEST_DATABASE_URL"]
async fn malformed_json_child_projections_cannot_override_canonical_state_detail_or_history() {
    let f = Fixture::new().await;
    f.command(Command::CreateLocation {
        code: "A-01".into(),
        zone: "Storage".into(),
        kind: "storage".into(),
    })
    .await;
    f.command(Command::CountStock {
        sku: "SKU".into(),
        lot: "LOT".into(),
        location: "A-01".into(),
        counted: 5,
        reason: "Opening physical fixture".into(),
    })
    .await;
    let order_id = f.command(order("SO-CANONICAL", "oms-canonical")).await;
    f.command(Command::Allocate {
        order_id: order_id.clone(),
    })
    .await;
    let receipt_id = f.command(receipt("ASN-CANONICAL")).await;
    f.command(Command::Receive {
        receipt_id: receipt_id.clone(),
        line_id: "receipt-line-1".into(),
        quantity: 1,
        location: "RECV-01".into(),
    })
    .await;
    let original = f.state().await;
    let counts = f.evidence_counts().await;

    // Only replace mutable JSON projections, including malformed types. Canonical
    // lines, allocations, physical stock, receipts and their guards stay intact.
    sqlx::query("UPDATE wms_orders SET data=data || $3::jsonb WHERE tenant=$1 AND warehouse_id='wh-main' AND id=$2")
        .bind(&f.identity.tenant).bind(&order_id)
        .bind(json!({"lines":"stale malformed shadow","allocations":{"quantity":999}}))
        .execute(&f.store.pool).await.expect("corrupt only order child projection");
    sqlx::query("UPDATE wms_receipts SET data=data || $3::jsonb WHERE tenant=$1 AND warehouse_id='wh-main' AND id=$2")
        .bind(&f.identity.tenant).bind(&receipt_id)
        .bind(json!({"lines":[{"quantity":"malformed shadow","received":999}]}))
        .execute(&f.store.pool).await.expect("corrupt only receipt child projection");

    let canonical = f.state().await;
    assert_eq!(canonical, original, "state trusted a JSON child shadow");
    let order = canonical
        .orders
        .iter()
        .find(|o| o.id == order_id)
        .expect("canonical order");
    let receipt = canonical
        .receipts
        .iter()
        .find(|r| r.id == receipt_id)
        .expect("canonical receipt");
    for (kind, entity_id, expected) in [
        ("orders", order_id.as_str(), json!(order)),
        ("receipts", receipt_id.as_str(), json!(receipt)),
    ] {
        let detail = f
            .store
            .detail(&f.identity.tenant, "wh-main", kind, entity_id)
            .await
            .expect("canonical detail");
        assert_eq!(detail, expected, "detail trusted a JSON child shadow");
        let page = f
            .store
            .history(&f.identity.tenant, "wh-main", kind, None, 1)
            .await
            .expect("canonical bounded history");
        assert_eq!(page["items"].as_array().expect("history items").len(), 1);
        assert_eq!(
            page["items"][0]["data"], expected,
            "history trusted a JSON child shadow"
        );
    }
    assert_eq!(
        f.evidence_counts().await,
        counts,
        "canonical reads wrote new execution evidence"
    );

    let outside = Identity {
        tenant: format!("guards-outside-{}", id()),
        actor: f.identity.actor.clone(),
        role: "admin".into(),
    };
    f.store
        .provision(&outside.tenant, false)
        .await
        .expect("other tenant");
    for (kind, entity_id) in [
        ("orders", order_id.as_str()),
        ("receipts", receipt_id.as_str()),
    ] {
        let hidden = f
            .store
            .detail(&outside.tenant, "wh-main", kind, entity_id)
            .await
            .expect_err("foreign record must stay hidden");
        assert_eq!(hidden.status, 404);
        let page = f
            .store
            .history(&outside.tenant, "wh-main", kind, None, 1)
            .await
            .expect("empty scoped page");
        assert!(page["items"].as_array().expect("items").is_empty());
    }
    // A valid physical command can still execute while unrelated mutable child
    // shadows remain malformed; every validator uses the normalized authority.
    f.command(Command::MoveStock {
        sku: "SKU".into(),
        lot: "LOT".into(),
        from: "RECV-01".into(),
        to: "A-01".into(),
        quantity: 1,
    })
    .await;
    let proof = f
        .store
        .reconciliation(&f.identity.tenant, "wh-main")
        .await
        .expect("independent proof");
    assert_eq!(proof["ok"], true);
    assert_eq!(f.state().await.version, original.version + 1);
}

#[tokio::test]
#[ignore = "requires isolated Postgres; WMS_TEST_DATABASE_URL"]
async fn completed_business_references_return_conflict_without_retaining_a_command_receipt() {
    let f = Fixture::new().await;
    let original_order = f.command(order("SO-ARCHIVED", "oms-archived")).await;
    f.command(Command::CancelOrder {
        order_id: original_order.clone(),
        reason: "Complete this historical work before duplicate submission".into(),
    })
    .await;
    let original_receipt = f.command(receipt("RCV-COMPLETED")).await;
    f.command(Command::Receive {
        receipt_id: original_receipt,
        line_id: "receipt-line-1".into(),
        quantity: 1,
        location: "RECV-01".into(),
    })
    .await;
    // Cross the public state's 20-completed-order window. The execution loader
    // must check the complete table rather than just whichever rows it loaded.
    for index in 0..20 {
        let peer = f
            .command(order(
                &format!("SO-PEER-{index}"),
                &format!("oms-peer-{index}"),
            ))
            .await;
        f.command(Command::CancelOrder {
            order_id: peer,
            reason: "Complete archived fixture".into(),
        })
        .await;
    }
    let before = f.state().await;
    assert!(!before.orders.iter().any(|order| order.id == original_order));
    let evidence_before = f.evidence_counts().await;
    for command in [
        order("SO-ARCHIVED", "oms-new-source"),
        order("SO-NEW-REFERENCE", "oms-archived"),
        receipt("RCV-COMPLETED"),
    ] {
        let request = Request {
            serial_inputs: Vec::new(),
            unit_inputs: Vec::new(),
            request_id: id(),
            expected_version: before.version,
            resource_versions: None,
            command,
        };
        for _ in 0..2 {
            let error = f
                .store
                .execute(&f.identity, "wh-main", &request)
                .await
                .expect_err("completed identity must remain unique");
            assert_eq!(
                error.status, 409,
                "permanent conflict must not look retryable"
            );
            assert_eq!(f.state().await, before);
            assert_eq!(f.evidence_counts().await, evidence_before);
            assert_eq!(
                f.store
                    .receipt(&f.identity, "wh-main", &request.request_id)
                    .await
                    .expect("receipt lookup"),
                json!({ "found": false })
            );
        }
    }
    // Rejected duplicates must leave the same warehouse version usable.
    f.command(order("SO-VALID-AFTER-CONFLICT", "oms-valid-after-conflict"))
        .await;
    assert_eq!(f.state().await.version, before.version + 1);
}

fn assert_constraint(error: sqlx::Error) {
    assert_eq!(
        error
            .as_database_error()
            .and_then(|error| error.code())
            .as_deref(),
        Some("23514"),
        "expected database invariant rejection: {error}"
    );
}

#[tokio::test]
#[ignore = "requires isolated Postgres; WMS_TEST_DATABASE_URL"]
async fn relational_migration_preserves_legacy_reads_and_fences_every_old_writer_operation() {
    let f = Fixture::new().await;
    let schema = format!("guards_{}", id().replace('-', "_"));
    let mut tx = f
        .store
        .pool
        .begin()
        .await
        .expect("isolated migration transaction");
    sqlx::raw_sql(&format!(
        "CREATE SCHEMA \"{schema}\"; SET LOCAL search_path TO \"{schema}\",public"
    ))
    .execute(&mut *tx)
    .await
    .expect("isolated schema");
    sqlx::raw_sql(include_str!("../migrations/001_initial.sql"))
        .execute(&mut *tx)
        .await
        .expect("original migration");
    let legacy = State::empty(Warehouse {
        id: "legacy-warehouse".into(),
        code: "LEGACY".into(),
        name: "Legacy warehouse".into(),
        city: "Vancouver".into(),
        timezone: "UTC".into(),
    });
    sqlx::query("INSERT INTO wms_workspaces(tenant,warehouse_id,version,state) VALUES('old-binary','legacy-warehouse',0,$1)")
        .bind(json!(legacy))
        .execute(&mut *tx)
        .await
        .expect("original binary wrote its snapshot before cutover");
    sqlx::raw_sql(include_str!("../migrations/002_relational_execution.sql"))
        .execute(&mut *tx)
        .await
        .expect("relational migration");
    for statement in [
        "UPDATE wms_workspaces SET state=state,version=version WHERE tenant='old-binary'",
        "DELETE FROM wms_workspaces WHERE tenant='old-binary'",
        "INSERT INTO wms_workspaces SELECT 'old-binary-new',warehouse_id,version,state FROM wms_workspaces WHERE tenant='old-binary'",
    ] {
        sqlx::query("SAVEPOINT old_writer_attempt")
            .execute(&mut *tx)
            .await
            .expect("savepoint");
        assert_constraint(
            sqlx::query(statement)
                .execute(&mut *tx)
                .await
                .expect_err("old binary must not maintain a second inventory authority"),
        );
        sqlx::query("ROLLBACK TO SAVEPOINT old_writer_attempt")
            .execute(&mut *tx)
            .await
            .expect("restore test transaction");
        let snapshot: Value = sqlx::query_scalar("SELECT state FROM wms_workspaces WHERE tenant='old-binary'")
            .fetch_one(&mut *tx)
            .await
            .expect("legacy evidence remains readable");
        assert_eq!(snapshot, json!(legacy));
    }
    tx.rollback()
        .await
        .expect("remove isolated schema and fixture");
}

#[tokio::test]
#[ignore = "requires isolated Postgres; WMS_TEST_DATABASE_URL"]
async fn deferred_balance_and_receipt_proofs_reject_sql_tampering_and_history_is_immutable() {
    let f = Fixture::new().await;
    let rid = f.command(receipt("RCV-PROOF")).await;
    f.command(Command::Receive {
        receipt_id: rid,
        line_id: "receipt-line-1".into(),
        quantity: 1,
        location: "RECV-01".into(),
    })
    .await;
    let before = f.state().await;
    for statement in [
        "UPDATE wms_stock SET on_hand=on_hand+1 WHERE tenant=$1 AND location='RECV-01'",
        "UPDATE wms_stock_ledger SET quantity=quantity+1 WHERE tenant=$1 AND location='RECV-01'",
        "UPDATE wms_receipt_lines SET received=0 WHERE tenant=$1",
        "UPDATE wms_receipt_evidence SET quantity=0 WHERE tenant=$1",
    ] {
        let mut tx = f.store.pool.begin().await.expect("tamper transaction");
        assert_eq!(
            sqlx::query(statement)
                .bind(&f.identity.tenant)
                .execute(&mut *tx)
                .await
                .expect("deferred checks run at commit")
                .rows_affected(),
            1,
            "tamper attempt must target existing evidence"
        );
        assert_constraint(tx.commit().await.expect_err("deferred independent proof"));
        assert_eq!(f.state().await, before);
    }
    for statement in [
        "UPDATE wms_movements SET reference='forged' WHERE tenant=$1",
        "DELETE FROM wms_movements WHERE tenant=$1",
        "UPDATE wms_activity SET kind='forged' WHERE tenant=$1",
        "DELETE FROM wms_events WHERE tenant=$1",
    ] {
        assert_constraint(
            sqlx::query(statement)
                .bind(&f.identity.tenant)
                .execute(&f.store.pool)
                .await
                .expect_err("committed warehouse history must be immutable"),
        );
    }
    assert_eq!(f.state().await, before);
    assert_eq!(
        f.store
            .reconciliation(&f.identity.tenant, "wh-main")
            .await
            .expect("independent reconciliation")["ok"],
        true
    );
}

#[tokio::test]
#[ignore = "requires isolated Postgres; WMS_TEST_DATABASE_URL"]
async fn full_reconciliation_rebuilds_history_independently_of_operational_balance_caches() {
    let f = Fixture::new().await;
    let rid = f.command(receipt("RCV-INDEPENDENT-PROOF")).await;
    f.command(Command::Receive {
        receipt_id: rid,
        line_id: "receipt-line-1".into(),
        quantity: 1,
        location: "RECV-01".into(),
    })
    .await;
    // Simulate coordinated administrative corruption of both operational
    // counters. A cached balance check cannot establish their independence.
    let mut tx = f.store.pool.begin().await.expect("isolated corrupt tenant");
    sqlx::query("UPDATE wms_stock SET on_hand=2 WHERE tenant=$1 AND location='RECV-01'")
        .bind(&f.identity.tenant)
        .execute(&mut *tx)
        .await
        .expect("alter isolated operational balance");
    sqlx::query("UPDATE wms_stock_ledger SET quantity=2 WHERE tenant=$1 AND location='RECV-01'")
        .bind(&f.identity.tenant)
        .execute(&mut *tx)
        .await
        .expect("alter matching operational cache");
    tx.commit()
        .await
        .expect("cached values agree with each other");
    let proof = f
        .store
        .reconciliation(&f.identity.tenant, "wh-main")
        .await
        .expect("full historical proof");
    assert_eq!(proof["ok"], false);
    let row = proof["rows"]
        .as_array()
        .expect("physical proof rows")
        .iter()
        .find(|row| row["location"] == "RECV-01")
        .expect("affected receiving bin");
    assert_eq!(row["on_hand"], 2);
    assert_eq!(
        row["ledger"], 1,
        "immutable receipt remains the physical evidence"
    );
    assert_eq!(row["difference"], 1);
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn cancellation_migration_blocks_existing_return_quantity_drift_and_freezes_valid_expectations(
) {
    let f = Fixture::new().await;
    let schema = format!("cancel_upgrade_{}", id().replace('-', "_"));
    let mut tx = f.store.pool.begin().await.expect("upgrade transaction");
    sqlx::raw_sql(&format!(
        "CREATE SCHEMA {schema}; SET LOCAL search_path TO {schema},public"
    ))
    .execute(&mut *tx)
    .await
    .expect("isolated upgrade schema");
    for migration in [
        include_str!("../migrations/001_initial.sql"),
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
    ] {
        sqlx::raw_sql(migration)
            .execute(&mut *tx)
            .await
            .expect("actual preceding migration");
    }
    // Explicit SQL fixture for upgrade admission; physical workflows are separately
    // exercised through actual HTTP/Store commands in persistence_acceptance.
    sqlx::raw_sql(r#"
        INSERT INTO wms_warehouses VALUES('upgrade','wh',0,'{"id":"wh","code":"TEST","name":"Test","city":"Test","timezone":"UTC"}');
        INSERT INTO wms_products VALUES('upgrade','wh','SKU','{"sku":"SKU","name":"Test","category":"Test","reorder_point":0}');
        INSERT INTO wms_locations VALUES('upgrade','wh','A','storage','{"code":"A","kind":"storage","zone":"A"}');
        INSERT INTO wms_orders(tenant,warehouse_id,id,reference,status,data) VALUES('upgrade','wh','source','SOURCE','shipped','{"id":"source","reference":"SOURCE","status":"shipped","source_order_id":null,"carrier":"Carrier","handoff_reference":"HANDOFF","shipped_at":"2026-10-18T00:00:00Z"}');
        INSERT INTO wms_order_lines VALUES('upgrade','wh','source','source-line','SKU',3);
        INSERT INTO wms_allocations VALUES('upgrade','wh','source','source-line','SKU','LOT','A',3,3,3,0);
        INSERT INTO wms_receipts(tenant,warehouse_id,id,reference,status,data) VALUES('upgrade','wh','rma','RMA','expected','{"id":"rma","reference":"RMA","status":"expected"}');
        INSERT INTO wms_receipt_lines VALUES('upgrade','wh','rma','return-line','SKU','LOT',3,0);
        INSERT INTO wms_return_lines VALUES('upgrade','wh','rma','return-line','source','source-line','SKU','LOT',3,'Physical return');
        SET CONSTRAINTS ALL IMMEDIATE;
        UPDATE wms_receipt_lines SET quantity=4 WHERE tenant='upgrade';
        SAVEPOINT before_cancellation_upgrade;
    "#).execute(&mut *tx).await.expect("preceding schema permits normalized quantity drift");
    assert_constraint(
        sqlx::raw_sql(include_str!("../migrations/012_receipt_cancellation.sql"))
            .execute(&mut *tx)
            .await
            .expect_err(
                "must not invent cancelled allowance from inconsistent original authorization",
            ),
    );
    sqlx::raw_sql("ROLLBACK TO SAVEPOINT before_cancellation_upgrade; UPDATE wms_receipt_lines SET quantity=3 WHERE tenant='upgrade'").execute(&mut *tx).await.expect("restore reviewed original authorization");
    sqlx::raw_sql(include_str!("../migrations/012_receipt_cancellation.sql"))
        .execute(&mut *tx)
        .await
        .expect("upgrade valid prior schema");
    sqlx::query("SAVEPOINT immutable_expectation")
        .execute(&mut *tx)
        .await
        .expect("savepoint");
    assert_constraint(
        sqlx::query("UPDATE wms_receipt_lines SET quantity=4 WHERE tenant='upgrade'")
            .execute(&mut *tx)
            .await
            .expect_err("upgraded expectation must stay frozen"),
    );
    sqlx::query("ROLLBACK TO SAVEPOINT immutable_expectation")
        .execute(&mut *tx)
        .await
        .expect("restore transaction");
    let quantity: i64 =
        sqlx::query_scalar("SELECT quantity FROM wms_receipt_lines WHERE tenant='upgrade'")
            .fetch_one(&mut *tx)
            .await
            .expect("preserved expectation");
    assert_eq!(quantity, 3);
    tx.rollback().await.expect("remove own upgrade fixture");
}
