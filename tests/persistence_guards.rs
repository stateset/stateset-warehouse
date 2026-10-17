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
            request_id: id(),
            expected_version: self.state().await.version,
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
            request_id: id(),
            expected_version: before.version,
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
