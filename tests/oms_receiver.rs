//! Execute the actual companion OMS SQL receiver in isolated schemas and real
//! concurrent PostgreSQL sessions. No WMS or existing OMS application rows are used.
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use sqlx::{postgres::PgPoolOptions, Connection, PgConnection, PgPool};
use std::{future::Future, time::Duration};
use tokio::{sync::oneshot, task::JoinHandle};
use uuid::Uuid;

const TENANT: &str = "receiver-test";
const REVISION: &str = "2026-10-17T00:00:00Z";

#[derive(Clone)]
struct Fixture {
    url: String,
    schema: String,
    pool: PgPool,
    order: String,
    body: Value,
}
fn canonical(value: &Value) -> String {
    match value {
        Value::Object(object) => {
            let mut keys: Vec<_> = object.keys().collect();
            keys.sort();
            format!(
                "{{{}}}",
                keys.into_iter()
                    .map(|key| format!(
                        "{}:{}",
                        serde_json::to_string(key).expect("JSON key"),
                        canonical(&object[key])
                    ))
                    .collect::<Vec<_>>()
                    .join(",")
            )
        }
        Value::Array(items) => format!(
            "[{}]",
            items.iter().map(canonical).collect::<Vec<_>>().join(",")
        ),
        _ => serde_json::to_string(value).expect("JSON scalar"),
    }
}
fn digest(body: &Value) -> String {
    format!("{:x}", Sha256::digest(canonical(body).as_bytes()))
}
fn line_identity(source: &str) -> String {
    format!(
        "oms-line-{:x}",
        Sha256::digest(format!("source:{source}").as_bytes())
    )
}
async fn apply(connection: &mut PgConnection, body: &Value) -> Result<Value, sqlx::Error> {
    sqlx::query_scalar("SELECT apply_warehouse_fulfillment($1,$2,$3)")
        .bind(TENANT)
        .bind(body)
        .bind(digest(body))
        .fetch_one(connection)
        .await
}
async fn backend(connection: &mut PgConnection) -> i32 {
    sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(connection)
        .await
        .expect("session identity")
}
impl Fixture {
    async fn new() -> Self {
        let url = std::env::var("WMS_TEST_DATABASE_URL")
            .expect("WMS_TEST_DATABASE_URL must identify an isolated PostgreSQL test database");
        assert!(
            reqwest::Url::parse(&url)
                .expect("PostgreSQL URL")
                .path()
                .contains("test"),
            "Never run receiver qualification against a production database"
        );
        let schema = format!("wms_receiver_{}", Uuid::new_v4().simple());
        let mut admin = PgConnection::connect(&url)
            .await
            .expect("test database connection");
        // This identifier is generated locally from a fixed prefix and UUID hex.
        sqlx::query(&format!("CREATE SCHEMA {schema}"))
            .execute(&mut admin)
            .await
            .expect("isolated receiver schema");
        drop(admin);
        let search_path = format!("{schema},pg_catalog");
        let pool = PgPoolOptions::new()
            .max_connections(6)
            .after_connect(move |connection, _| {
                let search_path = search_path.clone();
                Box::pin(async move {
                    sqlx::query("SELECT set_config('search_path',$1,false)")
                        .bind(search_path)
                        .execute(&mut *connection)
                        .await?;
                    sqlx::query("SET statement_timeout='10s'")
                        .execute(&mut *connection)
                        .await?;
                    Ok(())
                })
            })
            .connect(&url)
            .await
            .expect("isolated receiver sessions");
        sqlx::raw_sql(include_str!(
            "../integrations/stateset-sync-server/proof.sql"
        ))
        .execute(&pool)
        .await
        .expect("canonical minimal OMS table");
        sqlx::raw_sql(include_str!("../integrations/stateset-sync-server/migrations/20261017000000_warehouse_fulfillment_receipts.sql"))
            .execute(&pool).await.expect("actual companion receiver migration");
        let order = Uuid::new_v4().to_string();
        sqlx::query("INSERT INTO orders(id,tenant_id,status,line_items,updated_at,fulfillment_status,netsuite_id,tracking,source_order_payload) VALUES($1::uuid,$2,'PROCESSING',$3,$4::timestamptz,'UNFULFILLED','NS-UNCHANGED',$5,$6)")
            .bind(&order).bind(TENANT)
            .bind(json!([{"shopifyLineItemId":"a","sku":"SKU-A","quantity":2.0,"price":19.95},
                {"shopifyLineItemId":"b","sku":"SKU-A","quantity":3.0,"price":19.95}]))
            .bind(REVISION).bind(json!({"trackingNumber":"TRACK-UNCHANGED"}))
            .bind(json!({"fulfillment_status":"unfulfilled","unrelated":"SOURCE-UNCHANGED"}))
            .execute(&pool).await.expect("source order");
        let body = json!({"order_id":order,"request_id":Uuid::new_v4().to_string(),
            "event_id":Uuid::new_v4().to_string(),"expected_updated_at":REVISION,
            "warehouse_id":"wh-main","warehouse_order_id":Uuid::new_v4().to_string(),
            "carrier":"Verified physical carrier","handoff_reference":"Signed dock handoff 42",
            "handed_off_at":"2026-10-17T01:00:00Z","lines":[
                {"line_id":line_identity("a"),"sku":"SKU-A","quantity_each":2},
                {"line_id":line_identity("b"),"sku":"SKU-A","quantity_each":3}]});
        Self {
            url,
            schema,
            pool,
            order,
            body,
        }
    }
    async fn cleanup(self) {
        self.pool.close().await;
        let mut admin = PgConnection::connect(&self.url)
            .await
            .expect("schema cleanup connection");
        sqlx::query(&format!("DROP SCHEMA {} CASCADE", self.schema))
            .execute(&mut admin)
            .await
            .expect("remove only owned receiver schema");
    }
    async fn invoke(&self, body: &Value) -> Value {
        let mut connection = self.pool.acquire().await.expect("receiver connection");
        apply(&mut connection, body).await.expect("receiver result")
    }
    async fn source(&self) -> Value {
        sqlx::query_scalar("SELECT to_jsonb(o) FROM orders o WHERE id=$1::uuid AND tenant_id=$2")
            .bind(&self.order)
            .bind(TENANT)
            .fetch_one(&self.pool)
            .await
            .expect("canonical source state")
    }
    async fn receipts(&self) -> Value {
        sqlx::query_scalar("SELECT jsonb_build_object('total',count(*),'fulfilled',count(*) FILTER(WHERE response->>'status'='fulfilled'),'rejected',count(*) FILTER(WHERE response->>'status'='rejected')) FROM warehouse_fulfillment_receipts")
            .fetch_one(&self.pool).await.expect("receipt counts")
    }
    async fn spawn_apply(&self, body: Value) -> (i32, JoinHandle<Result<Value, sqlx::Error>>) {
        let pool = self.pool.clone();
        let (send, receive) = oneshot::channel();
        let task = tokio::spawn(async move {
            let mut connection = pool.acquire().await?;
            let pid = backend(&mut connection).await;
            send.send(pid).expect("observer remains available");
            apply(&mut connection, &body).await
        });
        (receive.await.expect("worker session started"), task)
    }
    async fn wait_for_row_lock(&self, waiter: i32, blocker: i32) {
        // Observe the database's actual blocking relationship. There is no
        // timing assumption or arbitrary sleep before releasing the first writer.
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let blocked: bool =
                    sqlx::query_scalar("SELECT $1::integer=ANY(pg_blocking_pids($2::integer))")
                        .bind(blocker)
                        .bind(waiter)
                        .fetch_one(&self.pool)
                        .await
                        .expect("owned session lock observation");
                if blocked {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("second session must actually wait on the source row");
    }
}
async fn run<F, Fut>(body: F)
where
    F: FnOnce(Fixture) -> Fut,
    Fut: Future<Output = ()> + Send + 'static,
{
    let fixture = Fixture::new().await;
    let cleanup = fixture.clone();
    let result = tokio::spawn(body(fixture)).await;
    cleanup.cleanup().await;
    if let Err(error) = result {
        if error.is_panic() {
            std::panic::resume_unwind(error.into_panic());
        }
        panic!("Receiver qualification task was cancelled");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires isolated WMS_TEST_DATABASE_URL PostgreSQL"]
async fn concurrent_exact_handoffs_commit_once_and_replay_the_same_durable_receipt() {
    run(|fixture| async move {
        let mut connection = fixture.pool.acquire().await.expect("first writer");
        let blocker = backend(&mut connection).await;
        let mut transaction = connection.begin().await.expect("first transaction");
        let first = apply(&mut transaction, &fixture.body)
            .await
            .expect("first uncommitted handoff");
        assert_eq!(first["status"], "fulfilled");
        assert_eq!(first["replayed"], false);
        let (waiter, task) = fixture.spawn_apply(fixture.body.clone()).await;
        fixture.wait_for_row_lock(waiter, blocker).await;
        transaction.commit().await.expect("first handoff commit");
        let replay = task.await.expect("second worker").expect("exact replay");
        let mut expected = first.clone();
        expected["replayed"] = json!(true);
        assert_eq!(replay, expected);
        assert_eq!(
            fixture.receipts().await,
            json!({"total":1,"fulfilled":1,"rejected":0})
        );
        let source = fixture.source().await;
        assert_eq!(source["status"], "FULFILLED");
        assert_eq!(source["netsuite_id"], "NS-UNCHANGED");
        assert_eq!(source["tracking"]["trackingNumber"], "TRACK-UNCHANGED");
        assert_eq!(
            source["source_order_payload"]["unrelated"],
            "SOURCE-UNCHANGED"
        );
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires isolated WMS_TEST_DATABASE_URL PostgreSQL"]
async fn concurrent_changed_evidence_under_one_request_key_conflicts_without_a_second_effect() {
    run(|fixture| async move {
        let mut connection = fixture.pool.acquire().await.expect("first writer");
        let blocker = backend(&mut connection).await;
        let mut transaction = connection.begin().await.expect("first transaction");
        let first = apply(&mut transaction, &fixture.body)
            .await
            .expect("first handoff");
        let mut changed = fixture.body.clone();
        changed["carrier"] = json!("Unverified replacement carrier");
        let (waiter, task) = fixture.spawn_apply(changed).await;
        fixture.wait_for_row_lock(waiter, blocker).await;
        transaction.commit().await.expect("original commit");
        let error = task
            .await
            .expect("conflicting worker")
            .expect_err("same request key cannot change its evidence");
        assert_eq!(
            error
                .as_database_error()
                .and_then(|error| error.code())
                .as_deref(),
            Some("40001")
        );
        assert_eq!(
            fixture.receipts().await,
            json!({"total":1,"fulfilled":1,"rejected":0})
        );
        let retained: Value =
            sqlx::query_scalar("SELECT request FROM warehouse_fulfillment_receipts")
                .fetch_one(&fixture.pool)
                .await
                .expect("retained original body");
        assert_eq!(retained, fixture.body);
        assert_eq!(
            fixture.source().await["updated_at"],
            first["applied_updated_at"]
        );
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires isolated WMS_TEST_DATABASE_URL PostgreSQL"]
async fn cancellation_or_revision_commit_first_and_block_stale_fulfillment_with_durable_rejection()
{
    for cancellation in [true, false] {
        run(move |fixture| async move {
            let mut connection=fixture.pool.acquire().await.expect("source writer");
            let blocker=backend(&mut connection).await;
            let mut transaction=connection.begin().await.expect("source transaction");
            sqlx::query("UPDATE orders SET status=CASE WHEN $2 THEN 'CANCELLED' ELSE status END,updated_at=updated_at+interval '1 second' WHERE id=$1::uuid")
                .bind(&fixture.order).bind(cancellation).execute(&mut *transaction).await.expect("source mutation");
            let expected:Value=sqlx::query_scalar("SELECT to_jsonb(o) FROM orders o WHERE id=$1::uuid")
                .bind(&fixture.order).fetch_one(&mut *transaction).await.expect("source writer snapshot");
            let (waiter,task)=fixture.spawn_apply(fixture.body.clone()).await;
            fixture.wait_for_row_lock(waiter,blocker).await;
            transaction.commit().await.expect("source commit before handoff");
            let rejection=task.await.expect("blocked handoff worker").expect("durable precondition result");
            assert_eq!(rejection["status"],"rejected");
            assert_eq!(rejection["reason"],if cancellation {"source_state_not_eligible"}else{"source_revision_changed"});
            assert_eq!(rejection["observed_updated_at"],expected["updated_at"]);
            assert_eq!(fixture.source().await,expected,"Rejected handoff must preserve the source writer's complete state");
            assert_eq!(fixture.receipts().await,json!({"total":1,"fulfilled":0,"rejected":1}));
        }).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires isolated WMS_TEST_DATABASE_URL PostgreSQL"]
async fn fulfillment_then_waiting_cancellation_preserves_later_terminal_state_on_exact_replay() {
    run(|fixture| async move {
        let mut connection=fixture.pool.acquire().await.expect("handoff writer");
        let blocker=backend(&mut connection).await;
        let mut transaction=connection.begin().await.expect("handoff transaction");
        let original=apply(&mut transaction,&fixture.body).await.expect("handoff before cancellation");
        let pool=fixture.pool.clone();let order=fixture.order.clone();
        let (send,receive)=oneshot::channel();
        let cancellation=tokio::spawn(async move {
            let mut connection=pool.acquire().await.expect("cancellation session");
            send.send(backend(&mut connection).await).expect("cancellation observer");
            sqlx::query_scalar::<_,Value>("UPDATE orders SET status='CANCELLED',updated_at=updated_at+interval '1 second' WHERE id=$1::uuid RETURNING to_jsonb(orders)")
                .bind(order).fetch_one(&mut *connection).await.expect("later cancellation")
        });
        let waiter=receive.await.expect("cancellation started");
        fixture.wait_for_row_lock(waiter,blocker).await;
        transaction.commit().await.expect("handoff commit");
        let cancelled=cancellation.await.expect("cancellation worker");
        assert_eq!(cancelled["status"],"CANCELLED");
        let replay=fixture.invoke(&fixture.body).await;
        let mut expected=original;expected["replayed"]=json!(true);
        assert_eq!(replay,expected);
        assert_eq!(fixture.source().await,cancelled,"Receipt replay must not revive a later terminal source state");
        assert_eq!(fixture.receipts().await,json!({"total":1,"fulfilled":1,"rejected":0}));
    }).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires isolated WMS_TEST_DATABASE_URL PostgreSQL"]
async fn late_original_rejection_replays_after_fresh_review_succeeds_without_changing_the_source() {
    run(|fixture| async move {
        sqlx::query(
            "UPDATE orders SET updated_at=updated_at+interval '1 second' WHERE id=$1::uuid",
        )
        .bind(&fixture.order)
        .execute(&fixture.pool)
        .await
        .expect("unrelated new source revision");
        let old = fixture.invoke(&fixture.body).await;
        assert_eq!(old["status"], "rejected");
        assert_eq!(old["reason"], "source_revision_changed");
        let mut reviewed = fixture.body.clone();
        reviewed["request_id"] = json!(Uuid::new_v4().to_string());
        reviewed["expected_updated_at"] = fixture.source().await["updated_at"].clone();
        let mut connection = fixture.pool.acquire().await.expect("reviewed writer");
        let blocker = backend(&mut connection).await;
        let mut transaction = connection.begin().await.expect("reviewed transaction");
        let accepted = apply(&mut transaction, &reviewed)
            .await
            .expect("reviewed request accepted");
        assert_eq!(accepted["status"], "fulfilled");
        let expected: Value =
            sqlx::query_scalar("SELECT to_jsonb(o) FROM orders o WHERE id=$1::uuid")
                .bind(&fixture.order)
                .fetch_one(&mut *transaction)
                .await
                .expect("accepted source state");
        let (waiter, task) = fixture.spawn_apply(fixture.body.clone()).await;
        fixture.wait_for_row_lock(waiter, blocker).await;
        transaction
            .commit()
            .await
            .expect("reviewed acceptance commit");
        let replay = task
            .await
            .expect("late original worker")
            .expect("negative receipt replay");
        let mut negative = old;
        negative["replayed"] = json!(true);
        assert_eq!(replay, negative);
        assert_eq!(fixture.source().await, expected);
        assert_eq!(
            fixture.receipts().await,
            json!({"total":2,"fulfilled":1,"rejected":1})
        );
    })
    .await;
}
