//! Populated legacy, exception and proposed-event delivery recovery evidence.
//! All receiver effects are confined to an owned loopback listener/artifact file.
use super::*;
use axum::{
    http::{HeaderMap, StatusCode},
    routing::post,
    Json, Router,
};
use stateset_warehouse::{
    domain::{State, Warehouse},
    integrations::Connections,
};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use tokio::sync::{Mutex, Notify};
const LEGACY: &str = "restore-legacy-history";
const LEGACY_WH: &str = "wh-restore-legacy";
const HISTORY: &str = "restore-operational-history";
const RECEIVER_SEED: &str = "loopback-receiver-seed.json";
const RECEIVER_RECOVERED: &str = "loopback-receiver-recovered.json";

fn actor() -> Identity {
    Identity {
        tenant: HISTORY.into(),
        actor: "rehearsal:history-admin".into(),
        role: "admin".into(),
    }
}
fn text<'a>(v: &'a Value, field: &str) -> Result<&'a str> {
    v[field]
        .as_str()
        .ok_or_else(|| format!("Missing history field {field}").into())
}

/// Write the actual initial schema before the application migrates/imports it.
/// No current trigger is disabled and no legacy row is inserted after cutover.
pub async fn seed_legacy() -> Result<Value> {
    let options = PgConnectOptions::from_str(&env::var("DATABASE_URL")?)?;
    let name = options.get_database().ok_or("Source DB name required")?;
    safe_database(name)?;
    require(
        name.starts_with("wms_restore_source_test_"),
        "Legacy fixture requires the newly created source rehearsal DB",
    )?;
    let mut c = PgConnection::connect_with(&options).await?;
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM pg_tables WHERE schemaname='public'")
        .fetch_one(&mut c)
        .await?;
    require(
        count == 0,
        "Legacy fixture requires a completely empty public schema",
    )?;
    let mut state = State::empty(Warehouse {
        id: LEGACY_WH.into(),
        code: "LEGACY".into(),
        name: "Synthetic original aggregate".into(),
        city: "Restore rehearsal".into(),
        timezone: "UTC".into(),
    });
    for command in [
        json!({"type":"create_product","sku":"LEGACY-EACH","name":"Original aggregate item","category":"Synthetic","reorder_point":0}),
        json!({"type":"create_location","code":"LEG-A","kind":"storage","zone":"Synthetic"}),
        json!({"type":"count_stock","sku":"LEGACY-EACH","lot":"LEG-LOT","location":"LEG-A","counted":3,"reason":"Synthetic original physical count"}),
        json!({"type":"create_order","reference":"LEGACY-SHIPPED","source_order_id":null,"customer":"Synthetic","destination":"Synthetic","priority":"normal","lines":[{"id":"original-line","sku":"LEGACY-EACH","quantity":1}]}),
    ] {
        state = state
            .execute(&serde_json::from_value(command)?, "rehearsal:legacy")
            .map_err(|e| format!("Legacy fixture: {e}"))?
            .0;
    }
    let order = state.orders[0].id.clone();
    for command in [
        json!({"type":"allocate","order_id":order}),
        json!({"type":"pick","order_id":order,"line_id":"original-line","location":"LEG-A","lot":"LEG-LOT","quantity":1}),
        json!({"type":"pack","order_id":order}),
        json!({"type":"ship","order_id":order,"carrier":"Synthetic","handoff_reference":"LEGACY-ORIGINAL-HANDOFF"}),
    ] {
        state = state
            .execute(&serde_json::from_value(command)?, "rehearsal:legacy")
            .map_err(|e| format!("Legacy fixture: {e}"))?
            .0;
    }
    let initial = include_str!("../../migrations/001_initial.sql");
    let mut tx = c.begin().await?;
    sqlx::raw_sql(initial).execute(&mut *tx).await?;
    sqlx::query(
        "INSERT INTO wms_workspaces(tenant,warehouse_id,version,state) VALUES($1,$2,$3,$4)",
    )
    .bind(LEGACY)
    .bind(LEGACY_WH)
    .bind(state.version)
    .bind(json!(state))
    .execute(&mut *tx)
    .await?;
    sqlx::query("CREATE TABLE wms_schema_migrations(name TEXT PRIMARY KEY,digest TEXT NOT NULL,applied_at TIMESTAMPTZ NOT NULL DEFAULT now())").execute(&mut *tx).await?;
    sqlx::query("INSERT INTO wms_schema_migrations(name,digest) VALUES('001_initial',$1)")
        .bind(format!("{:x}", Sha256::digest(initial.as_bytes())))
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    c.close().await?;
    Ok(
        json!({"tenant":LEGACY,"warehouse_id":LEGACY_WH,"state":state,"shipped_order_id":order,"source_schema":1,"initial_digest":format!("{:x}",Sha256::digest(initial.as_bytes()))}),
    )
}

async fn legacy_checks(store: &Store, original: &Value) -> Result<()> {
    let retained: Value =
        sqlx::query_scalar("SELECT state FROM wms_workspaces WHERE tenant=$1 AND warehouse_id=$2")
            .bind(LEGACY)
            .bind(LEGACY_WH)
            .fetch_one(&store.pool)
            .await?;
    require(
        retained == original["state"],
        "Original aggregate snapshot changed",
    )?;
    let events: i64 = sqlx::query_scalar("SELECT count(*) FROM wms_events WHERE tenant=$1")
        .bind(LEGACY)
        .fetch_one(&store.pool)
        .await?;
    require(events == 0, "Legacy import invented execution events")?;
    let (kind, event, valid): (String, Option<String>, bool) = sqlx::query_as("SELECT source_kind,event_id,wms_shipment_consistent(tenant,warehouse_id,order_id) FROM wms_shipment_evidence WHERE tenant=$1 AND warehouse_id=$2 AND order_id=$3").bind(LEGACY).bind(LEGACY_WH).bind(text(original,"shipped_order_id")?).fetch_one(&store.pool).await?;
    require(
        kind == "legacy_snapshot" && event.is_none() && valid,
        "Legacy shipment lost its original snapshot source",
    )?;
    let before = table_evidence(store).await?;
    for statement in [
        "UPDATE wms_workspaces SET state=state,version=version WHERE tenant=$1",
        "DELETE FROM wms_workspaces WHERE tenant=$1",
        "INSERT INTO wms_workspaces SELECT tenant||'-old-writer',warehouse_id,version,state FROM wms_workspaces WHERE tenant=$1",
    ] {
        let mut tx = store.pool.begin().await?;
        let e = sqlx::query(statement).bind(LEGACY).execute(&mut *tx).await.err().ok_or("Old aggregate writer was not fenced")?;
        require(e.as_database_error().and_then(|e|e.code()).as_deref() == Some("23514"), "Old writer failed without the expected invariant fence")?;
        tx.rollback().await?;
    }
    require(
        table_evidence(store).await? == before,
        "Rejected old writers changed evidence",
    )?;
    require(
        application(store.reconciliation(LEGACY, LEGACY_WH).await)?["ok"] == true,
        "Imported legacy warehouse does not reconcile",
    )
}
async fn request(store: &Store, key: &str, command: Command) -> Result<Request> {
    Ok(Request {
        request_id: key.into(),
        expected_version: application(store.state(HISTORY, WAREHOUSE).await)?.version,
        resource_versions: None,
        serial_inputs: vec![],
        unit_inputs: vec![],
        command,
    })
}
async fn execute(
    store: &Store,
    originals: &mut Vec<Value>,
    key: &str,
    command: Command,
) -> Result<Value> {
    let r = request(store, key, command).await?;
    let result = application(store.execute(&actor(), WAREHOUSE, &r).await)?;
    originals.push(json!({"request":r,"response":result}));
    Ok(result)
}
// Database timestamps represent instants. Normalize JSON evidence inside a
// read-only transaction without changing the deliberately different sessions.
async fn historical_json(store: &Store, sql: &str, bindings: &[&str]) -> Result<Value> {
    let mut tx = store.pool.begin().await?;
    sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY")
        .execute(&mut *tx)
        .await?;
    sqlx::query("SET LOCAL TIME ZONE 'UTC'")
        .execute(&mut *tx)
        .await?;
    let mut query = sqlx::query_scalar(sql);
    for parameter in bindings {
        query = query.bind(*parameter);
    }
    let value: Value = query.fetch_one(&mut *tx).await?;
    tx.commit().await?;
    Ok(value)
}
async fn row(store: &Store, id: &str) -> Result<Value> {
    historical_json(
        store,
        "SELECT to_jsonb(o) FROM wms_outbox o WHERE id=$1 AND tenant=$2",
        &[id, HISTORY],
    )
    .await
}
async fn attempts(store: &Store, id: &str) -> Result<Value> {
    historical_json(store,"SELECT COALESCE(jsonb_agg(to_jsonb(a) ORDER BY created_at,id),'[]'::jsonb) FROM wms_delivery_attempts a WHERE outbox_id=$1",&[id]).await
}
async fn audit(store: &Store) -> Result<Value> {
    historical_json(store,"SELECT COALESCE(jsonb_agg(to_jsonb(a) ORDER BY created_at,id),'[]'::jsonb) FROM wms_admin_audit a WHERE tenant=$1",&[HISTORY]).await
}
async fn exceptions(store: &Store) -> Result<Value> {
    historical_json(
        store,
        "SELECT jsonb_agg(to_jsonb(e) ORDER BY seq) FROM wms_exceptions e WHERE tenant=$1",
        &[HISTORY],
    )
    .await
}
async fn choose(store: &Store, id: &str) -> Result<()> {
    // Test-only scheduler advancement; no queue/event is deleted or reassigned.
    sqlx::query("UPDATE wms_outbox SET due_at=CASE WHEN id=$1 THEN now()-interval '1 second' ELSE now()+interval '1 day' END WHERE tenant=$2 AND status='pending'").bind(id).bind(HISTORY).execute(&store.pool).await?;
    Ok(())
}
async fn event_queue(store: &Store, r: &Value) -> Result<String> {
    Ok(sqlx::query_scalar("SELECT o.id FROM wms_outbox o JOIN wms_events e ON e.id=o.event_id WHERE o.tenant=$1 AND e.payload->>'request_id'=$2 AND o.destination='oms'").bind(HISTORY).bind(text(&r["request"],"request_id")?).fetch_one(&store.pool).await?)
}
fn connections(url: &str, token: &str) -> Connections {
    Connections(Arc::new(BTreeMap::from([(
        (HISTORY.into(), "oms".into()),
        (url.into(), token.into()),
    )])))
}

struct Receiver {
    url: String,
    journal: Arc<Mutex<Value>>,
    entered: Arc<Notify>,
    release: Arc<Notify>,
    server: tokio::task::JoinHandle<()>,
}
impl Drop for Receiver {
    fn drop(&mut self) {
        self.server.abort();
    }
}
impl Receiver {
    async fn start(
        address: &str,
        path: PathBuf,
        initial: Value,
        fail_event: Option<String>,
        hold_event: Option<String>,
        token: &'static str,
    ) -> Result<Self> {
        let listener = tokio::net::TcpListener::bind(address).await?;
        require(
            listener.local_addr()?.ip().is_loopback(),
            "Receiver must bind loopback",
        )?;
        let url = format!("http://{}/original", listener.local_addr()?);
        let journal = Arc::new(Mutex::new(initial));
        let entered = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        let router = Router::new().route("/original",post({
            let journal=journal.clone();let entered=entered.clone();let release=release.clone();
            move |headers:HeaderMap, Json(payload):Json<Value>| {
                let journal=journal.clone();let entered=entered.clone();let release=release.clone();let path=path.clone();let fail_event=fail_event.clone();let hold_event=hold_event.clone();
                async move {
                    let Some(id)=headers.get("idempotency-key").and_then(|h|h.to_str().ok()) else { return StatusCode::BAD_REQUEST; };
                    if payload["id"] != id || payload["tenant"] != HISTORY || headers.get("authorization").and_then(|h|h.to_str().ok()) != Some(format!("Bearer {token}").as_str()) { return StatusCode::UNAUTHORIZED; }
                    let mut j=journal.lock().await;
                    let first=!j["calls"].as_array().is_some_and(|a|a.iter().any(|c|c["event_id"]==id));
                    let fail=fail_event.as_deref()==Some(id) && first;
                    let duplicate=j["accepted"].get(id).is_some();
                    if duplicate && j["accepted"][id] != payload { return StatusCode::CONFLICT; }
                    if !fail && !duplicate { j["accepted"][id]=payload.clone(); }
                    j["calls"].as_array_mut().expect("fixture journal array").push(json!({"event_id":id,"payload":payload,"status":if fail {503}else{202},"duplicate":duplicate}));
                    if fs::write(&path,serde_json::to_vec_pretty(&*j).expect("fixture JSON")).is_err() { return StatusCode::INTERNAL_SERVER_ERROR; }
                    drop(j);
                    if hold_event.as_deref()==Some(id) { entered.notify_one();release.notified().await; }
                    if fail { StatusCode::SERVICE_UNAVAILABLE } else { StatusCode::ACCEPTED }
                }
            }
        })).route("/replacement",post({let journal=journal.clone();move ||{let journal=journal.clone();async move{let mut j=journal.lock().await;let n=j["replacement_calls"].as_u64().unwrap_or(0);j["replacement_calls"]=json!(n+1);StatusCode::ACCEPTED}}}));
        let server = tokio::spawn(async move {
            let _ = axum::serve(listener, router).await;
        });
        Ok(Self {
            url,
            journal,
            entered,
            release,
            server,
        })
    }
    async fn value(&self) -> Value {
        self.journal.lock().await.clone()
    }
}
fn journal_path(manifest_path: &str, name: &str) -> Result<PathBuf> {
    let parent = Path::new(manifest_path)
        .parent()
        .ok_or("Manifest artifact directory required")?;
    Ok(parent.join(name))
}

pub async fn seed(store: &Store, legacy: Value, manifest_path: &str) -> Result<Value> {
    legacy_checks(store, &legacy).await?;
    application(store.provision(HISTORY, false).await)?;
    let mut originals = vec![];
    for (key, command) in [
        (
            "history-product",
            json!({"type":"create_product","sku":"HISTORY-EACH","name":"History fixture","category":"Synthetic","reorder_point":0}),
        ),
        (
            "history-bin",
            json!({"type":"create_location","code":"HIST-A","kind":"storage","zone":"Synthetic"}),
        ),
        (
            "history-stock",
            json!({"type":"count_stock","sku":"HISTORY-EACH","lot":"HIST-LOT","location":"HIST-A","counted":2,"reason":"Observed synthetic history fixture"}),
        ),
        (
            "history-order",
            json!({"type":"create_order","reference":"HISTORY-ORDER","source_order_id":null,"customer":"Synthetic","destination":"Synthetic","priority":"normal","lines":[{"id":"original","sku":"HISTORY-EACH","quantity":1}]}),
        ),
    ] {
        execute(store, &mut originals, key, serde_json::from_value(command)?).await?;
    }
    let order = entity(&originals[3]["response"])?;
    let resolved = execute(
        store,
        &mut originals,
        "history-open-resolved",
        Command::ReportException {
            order_id: Some(order.clone()),
            category: "Synthetic damage review".into(),
            detail: "Original observation retained across restore".into(),
        },
    )
    .await?;
    execute(
        store,
        &mut originals,
        "history-resolve",
        Command::ResolveException {
            exception_id: entity(&resolved)?,
            resolution: "Synthetic supervisor reviewed original evidence".into(),
        },
    )
    .await?;
    let open = execute(
        store,
        &mut originals,
        "history-open-held",
        Command::ReportException {
            order_id: Some(order.clone()),
            category: "Synthetic custody review".into(),
            detail: "Remain blocked until separately reviewed after restore".into(),
        },
    )
    .await?;
    let blocked = request(
        store,
        "history-blocked-allocate",
        Command::Allocate {
            order_id: order.clone(),
        },
    )
    .await?;
    blocked_check(store, &blocked).await?;
    let failed_id = event_queue(store, &originals[4]).await?;
    let crash_id = event_queue(store, &originals[6]).await?;
    let failed = row(store, &failed_id).await?;
    let crash = row(store, &crash_id).await?;
    let path = journal_path(manifest_path, RECEIVER_SEED)?;
    require(!path.exists(), "Receiver evidence must not be overwritten")?;
    let receiver = Receiver::start(
        "127.0.0.1:0",
        path.clone(),
        json!({"schema_version":1,"synthetic":true,"calls":[],"accepted":{},"replacement_calls":0}),
        Some(text(&failed, "event_id")?.into()),
        Some(text(&crash, "event_id")?.into()),
        "synthetic-seed-token",
    )
    .await?;
    let original = connections(&receiver.url, "synthetic-seed-token");
    let wrong = connections(
        &receiver.url.replace("/original", "/replacement"),
        "synthetic-seed-token",
    );
    choose(store, &failed_id).await?;
    require(
        application(original.dispatch_one(store).await)?,
        "First delivery did no work",
    )?;
    require(
        row(store, &failed_id).await?["status"] == "pending"
            && attempts(store, &failed_id).await?[0]["outcome"] == "failed",
        "Synthetic503 did not retain failed attempt",
    )?;
    choose(store, &failed_id).await?;
    require(
        application(wrong.dispatch_one(store).await)?,
        "Receiver mismatch was not held",
    )?;
    for attempt in 0..2 {
        application(store.retry(&actor(), &failed_id).await)?;
        require(
            application(if attempt == 0 {
                wrong.dispatch_one(store).await
            } else {
                original.dispatch_one(store).await
            })?,
            "Audited retry did no work",
        )?;
    }
    require(
        row(store, &failed_id).await?["status"] == "delivered",
        "Original receiver could not complete the audited retry",
    )?;
    choose(store, &crash_id).await?;
    let sender = tokio::spawn({
        let original = original.clone();
        let store = store.clone();
        async move { original.dispatch_one(&store).await }
    });
    tokio::time::timeout(Duration::from_secs(5), receiver.entered.notified()).await?;
    sender.abort();
    let stopped = sender.await;
    require(
        stopped.is_err_and(|e| e.is_cancelled()),
        "Sender did not stop at its actual claimed HTTP boundary",
    )?;
    receiver.release.notify_one();
    let crash = row(store, &crash_id).await?;
    require(
        crash["status"] == "delivering"
            && crash["attempts"] == 1
            && attempts(store, &crash_id).await?[0]["outcome"] == "started",
        "Sender interruption lost its original started claim",
    )?;
    let journal = receiver.value().await;
    require(
        journal["replacement_calls"] == 0
            && journal["accepted"]
                .as_object()
                .is_some_and(|a| a.len() == 2),
        "Loopback receiver effects differ",
    )?;
    let url = receiver.url.clone();
    drop(receiver);
    let audit = audit(store).await?;
    require(
        audit.as_array().is_some_and(|a| a.len() == 2),
        "Audited original retries not retained",
    )?;
    let exceptions = exceptions(store).await?;
    Ok(
        json!({"legacy":legacy,"originals":originals,"order_id":order,"open_exception_id":entity(&open)?,"resolved_exception_id":entity(&resolved)?,"blocked_request":blocked,"exceptions":exceptions,"failed_id":failed_id,"crash_id":crash_id,"failed_queue":row(store,&failed_id).await?,"crash_queue":crash,"failed_attempts":attempts(store,&failed_id).await?,"crash_attempts":attempts(store,&crash_id).await?,"audit":audit,"receiver_url":url,"receiver_seed_filename":RECEIVER_SEED,"receiver_seed_sha256":format!("{:x}",Sha256::digest(fs::read(path)?)),"receiver_journal":journal,"sender_cancelled_after_claim_and_receiver_effect":true}),
    )
}
async fn blocked_check(store: &Store, request: &Request) -> Result<()> {
    let before = table_evidence(store).await?;
    let error = store
        .execute(&actor(), WAREHOUSE, request)
        .await
        .err()
        .ok_or("Original blocking exception did not block allocation")?;
    require(
        error.status == 422 && error.message.contains("blocking order exception"),
        "Allocation was rejected without checking its original exception",
    )?;
    require(
        table_evidence(store).await? == before,
        "Blocked allocation changed committed evidence",
    )
}

pub async fn verify_and_replay(store: &Store, fixture: &Value) -> Result<()> {
    legacy_checks(store, &fixture["legacy"]).await?;
    require(
        audit(store).await? == fixture["audit"]
            && exceptions(store).await? == fixture["exceptions"],
        "Restored original exception/audit history differs",
    )?;
    let before = table_evidence(store).await?;
    for original in fixture["originals"]
        .as_array()
        .ok_or("Missing original history requests")?
    {
        let request: Request = serde_json::from_value(original["request"].clone())?;
        let replay = application(store.execute(&actor(), WAREHOUSE, &request).await)?;
        require(
            replay["replayed"] == true && replay["result"] == original["response"]["result"],
            "Original exception/catalog request failed exact replay",
        )?;
    }
    blocked_check(
        store,
        &serde_json::from_value(fixture["blocked_request"].clone())?,
    )
    .await?;
    require(
        row(store, text(fixture, "failed_id")?).await? == fixture["failed_queue"]
            && row(store, text(fixture, "crash_id")?).await? == fixture["crash_queue"],
        "Restored original queue claims differ",
    )?;
    require(
        attempts(store, text(fixture, "failed_id")?).await? == fixture["failed_attempts"]
            && attempts(store, text(fixture, "crash_id")?).await? == fixture["crash_attempts"],
        "Restored attempt histories differ",
    )?;
    require(
        table_evidence(store).await? == before,
        "Original history replay changed restored evidence",
    )
}

pub async fn recover(store: &Store, fixture: &Value, manifest_path: &str) -> Result<Value> {
    let path = journal_path(manifest_path, RECEIVER_SEED)?;
    require(
        format!("{:x}", Sha256::digest(fs::read(&path)?)) == text(fixture, "receiver_seed_sha256")?,
        "Original local receiver journal checksum changed",
    )?;
    let initial: Value = serde_json::from_slice(&fs::read(&path)?)?;
    require(
        initial == fixture["receiver_journal"],
        "Original loopback receiver journal differs",
    )?;
    let url = reqwest::Url::parse(text(fixture, "receiver_url")?)?;
    require(
        url.scheme() == "http"
            && url.host_str() == Some("127.0.0.1")
            && url.path() == "/original"
            && url.username().is_empty()
            && url.password().is_none()
            && url.query().is_none()
            && url.fragment().is_none(),
        "Restored receiver must be the owned original loopback endpoint",
    )?;
    let address = format!(
        "127.0.0.1:{}",
        url.port().ok_or("Original receiver port required")?
    );
    let recovered_path = journal_path(manifest_path, RECEIVER_RECOVERED)?;
    require(
        !recovered_path.exists(),
        "Recovered receiver evidence must not be overwritten",
    )?;
    let receiver = Receiver::start(
        &address,
        recovered_path.clone(),
        initial.clone(),
        None,
        None,
        "synthetic-rotated-token",
    )
    .await?;
    require(
        receiver.url == url.as_str(),
        "Restored listener did not bind its original receiver identity",
    )?;
    let id = text(fixture, "crash_id")?;
    let original_queue = row(store, id).await?;
    sqlx::query("UPDATE wms_outbox SET lease_until=now()-interval '1 second' WHERE id=$1 AND status='delivering'").bind(id).execute(&store.pool).await?;
    let wrong = connections(
        &receiver.url.replace("/original", "/replacement"),
        "synthetic-rotated-token",
    );
    require(
        application(wrong.dispatch_one(store).await)?,
        "Expired restored claim did no work",
    )?;
    let held = row(store, id).await?;
    let history = attempts(store, id).await?;
    require(
        held["status"] == "dead_letter"
            && held["recipient_fingerprint"] == original_queue["recipient_fingerprint"]
            && history[0]["outcome"] == "uncertain"
            && !history[0]["finished_at"].is_null(),
        "Expired claim was not retained as uncertain under the original receiver hold",
    )?;
    let before = table_evidence(store).await?;
    let mut operator = actor();
    operator.role = "operator".into();
    require(
        store
            .retry(&operator, id)
            .await
            .err()
            .is_some_and(|e| e.status == 403),
        "Operator bypassed audited retry role",
    )?;
    let mut other = actor();
    other.tenant = "restore-other-tenant".into();
    require(
        store
            .retry(&other, id)
            .await
            .err()
            .is_some_and(|e| e.status == 409),
        "Another tenant requeued the original claim",
    )?;
    let late=sqlx::query("UPDATE wms_outbox SET status='delivered',lease_token=NULL,lease_until=NULL,delivered_at=now() WHERE id=$1 AND lease_token=$2 AND status='delivering'").bind(id).bind(text(&original_queue,"lease_token")?).execute(&store.pool).await?.rows_affected();
    require(
        late == 0 && table_evidence(store).await? == before,
        "Expired sender completion or denied retry changed evidence",
    )?;
    application(store.retry(&actor(), id).await)?;
    let original = connections(&receiver.url, "synthetic-rotated-token");
    require(
        application(original.dispatch_one(store).await)?,
        "Restored audited original delivery did no work",
    )?;
    let audits = audit(store).await?;
    let old_audits = fixture["audit"]
        .as_array()
        .ok_or("Missing original audit rows")?;
    let rows = audits.as_array().ok_or("Missing continued audit rows")?;
    require(
        rows.len() == old_audits.len() + 1
            && old_audits.iter().all(|old| rows.contains(old))
            && rows.iter().any(|r| {
                r["reference"] == id && r["actor"] == actor().actor && r["action"] == "outbox.retry"
            }),
        "Audited recovery lost original rows or its new actor-bound retry",
    )?;
    let before_audit_fences = table_evidence(store).await?;
    for sql in [
        "UPDATE wms_admin_audit SET reference=reference WHERE tenant=$1",
        "DELETE FROM wms_admin_audit WHERE tenant=$1",
    ] {
        let mut tx = store.pool.begin().await?;
        let error = sqlx::query(sql)
            .bind(HISTORY)
            .execute(&mut *tx)
            .await
            .err()
            .ok_or("Restored audit history was mutable")?;
        require(
            error.as_database_error().and_then(|e| e.code()).as_deref() == Some("23514"),
            "Audit write did not hit its original invariant fence",
        )?;
        tx.rollback().await?;
    }
    require(
        table_evidence(store).await? == before_audit_fences,
        "Rejected audit edits changed evidence",
    )?;
    let final_queue = row(store, id).await?;
    let final_history = attempts(store, id).await?;
    require(
        final_queue["status"] == "delivered"
            && final_queue["event_id"] == original_queue["event_id"]
            && final_queue["recipient_fingerprint"] == original_queue["recipient_fingerprint"]
            && final_history.as_array().is_some_and(|a| a.len() == 2)
            && final_history[0] == history[0]
            && final_history[1]["outcome"] == "accepted",
        "Audited recovery changed original identity or uncertain history",
    )?;
    let journal = receiver.value().await;
    require(
        journal["accepted"] == initial["accepted"]
            && journal["replacement_calls"] == 0
            && journal["calls"].as_array().is_some_and(|a| a.len() == 4)
            && journal["calls"][3]["duplicate"] == true
            && journal["calls"][3]["payload"]
                == initial["accepted"][text(&original_queue, "event_id")?],
        "Receiver recovery duplicated an effect or changed the original payload",
    )?;
    require(
        row(store, text(fixture, "failed_id")?).await? == fixture["failed_queue"]
            && attempts(store, text(fixture, "failed_id")?).await? == fixture["failed_attempts"],
        "Recovery changed unrelated delivered history",
    )?;
    let mut continued = vec![];
    execute(
        store,
        &mut continued,
        "history-restored-resolution",
        Command::ResolveException {
            exception_id: text(fixture, "open_exception_id")?.into(),
            resolution: "Original restored exception reviewed by synthetic supervisor".into(),
        },
    )
    .await?;
    execute(
        store,
        &mut continued,
        "history-restored-allocation",
        Command::Allocate {
            order_id: text(fixture, "order_id")?.into(),
        },
    )
    .await?;
    for original in &continued {
        let request: Request = serde_json::from_value(original["request"].clone())?;
        let before = table_evidence(store).await?;
        let replay = application(store.execute(&actor(), WAREHOUSE, &request).await)?;
        require(
            replay["replayed"] == true
                && replay["result"] == original["response"]["result"]
                && table_evidence(store).await? == before,
            "Continued exception requests did not replay unchanged",
        )?;
    }
    let legacy_request = Request {
        request_id: "history-restored-legacy-count".into(),
        expected_version: application(store.state(LEGACY, LEGACY_WH).await)?.version,
        resource_versions: None,
        unit_inputs: vec![],
        serial_inputs: vec![],
        command: Command::CountStock {
            sku: "LEGACY-EACH".into(),
            lot: "LEG-LOT".into(),
            location: "LEG-A".into(),
            counted: 2,
            reason: "Observed original remaining units after restore".into(),
        },
    };
    let legacy_actor = Identity {
        tenant: LEGACY.into(),
        ..actor()
    };
    let result = application(
        store
            .execute(&legacy_actor, LEGACY_WH, &legacy_request)
            .await,
    )?;
    let before = table_evidence(store).await?;
    let replay = application(
        store
            .execute(&legacy_actor, LEGACY_WH, &legacy_request)
            .await,
    )?;
    require(
        replay["replayed"] == true
            && replay["result"] == result["result"]
            && table_evidence(store).await? == before,
        "New relational legacy count did not recover exactly",
    )?;
    let retained: Value =
        sqlx::query_scalar("SELECT state FROM wms_workspaces WHERE tenant=$1 AND warehouse_id=$2")
            .bind(LEGACY)
            .bind(LEGACY_WH)
            .fetch_one(&store.pool)
            .await?;
    require(
        retained == fixture["legacy"]["state"],
        "New relational execution rewrote original aggregate evidence",
    )?;
    for (tenant, warehouse) in [(LEGACY, LEGACY_WH), (HISTORY, WAREHOUSE)] {
        require(
            application(store.reconciliation(tenant, warehouse).await)?["ok"] == true,
            "Continued historical warehouse does not reconcile",
        )?;
    }
    Ok(
        json!({"ok":true,"legacy_snapshot_and_shipment_source_retained":true,"old_writer_three_operations_fenced_without_changes":true,"legacy_import_invented_no_events":true,"new_relational_count_and_exact_replay_preserved_legacy_snapshot":true,"open_and_resolved_exceptions_restored":true,"blocked_allocation_changed_no_tables":true,"resolved_original_exception_then_allocated_and_replayed":true,"started_delivery_lease_and_failed_accepted_histories_restored":true,"expired_started_attempt_retained_as_uncertain":true,"replacement_receiver_http_calls":0,"late_completion_and_wrong_role_tenant_retries_changed_no_tables":true,"audited_original_retry_with_rotated_token_preserved_event_and_payload":true,"original_audit_rows_retained_and_edits_fenced":true,"local_receiver_duplicate_added_no_effect":true,"original_uncertain_attempt_retained_after_accepted_retry":true,"receiver_seed_sha256":fixture["receiver_seed_sha256"],"receiver_recovered_sha256":format!("{:x}",Sha256::digest(fs::read(recovered_path)?)),"receiver_calls_before":3,"receiver_calls_after":4,"receiver_effects_before":2,"receiver_effects_after":2,"scope":"synthetic proposed-event outbox contract over owned loopback HTTP; no native/deployed receiver acceptance"}),
    )
}
