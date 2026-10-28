//! Background full reconciliation. A run executes the same complete proof as
//! `GET .../reconciliation` in one read-only snapshot, on the API process that
//! accepted it, under its own statement deadline, so a warehouse whose history
//! no longer fits the 10-second request deadline still reconciles. Clients
//! start a run and poll it; every request stays short. Runs change no stock
//! or history, and the proof and its verdict are exactly the synchronous ones.
use crate::{
    api::App,
    domain,
    store::{Error, Identity, Store},
};
use axum::{
    extract::{Path, State as AppState},
    http::StatusCode,
    Extension, Json,
};
use serde_json::Value;
use std::{sync::OnceLock, time::Duration};
use tokio::sync::Semaphore;

/// Deadline for each statement of a run's proof.
pub const STATEMENT_TIMEOUT: Duration = Duration::from_secs(120);
/// An unfinished run is reported as abandoned after this: the process that
/// accepted it stopped, or it waited too long for a proof slot.
pub const RUN_DEADLINE: Duration = Duration::from_secs(600);
/// Unfinished runs one warehouse may have at once.
const ACTIVE_PER_WAREHOUSE: i64 = 2;
/// Finished runs kept per warehouse; older ones are removed when a run starts.
const KEEP_FINISHED: i64 = 20;

/// Proofs this process executes at once. Each holds one pooled connection for
/// up to its statement deadline, so most of the pool stays free for requests.
fn slots() -> &'static Semaphore {
    static SLOTS: OnceLock<Semaphore> = OnceLock::new();
    SLOTS.get_or_init(|| Semaphore::new(2))
}

fn reader(identity: &Identity) -> Result<(), Error> {
    if !["viewer", "operator", "admin"].contains(&identity.role.as_str()) {
        return Err(Error::new(
            403,
            "Reconciliation runs require a warehouse role",
        ));
    }
    Ok(())
}

// An unfinished run past its deadline reads as failed; nothing rewrites it.
const RUN: &str = "SELECT jsonb_build_object('id',id,'warehouse_id',warehouse_id,'requested_by',requested_by,
 'status',CASE WHEN status IN('queued','running') AND deadline_at<=now() THEN 'failed' ELSE status END,
 'requested_at',requested_at,'started_at',started_at,'finished_at',finished_at,'deadline_at',deadline_at,
 'version',version,'result',result,
 'error',CASE WHEN status IN('queued','running') AND deadline_at<=now()
  THEN 'Abandoned: the run did not finish before its deadline' ELSE error END)
 FROM wms_reconciliation_runs WHERE tenant=$1 AND warehouse_id=$2 AND id=$3";

pub async fn start(
    AppState(app): AppState<App>,
    Extension(identity): Extension<Identity>,
    Path(warehouse): Path<String>,
) -> Result<(StatusCode, Json<Value>), Error> {
    reader(&identity)?;
    Ok((
        StatusCode::ACCEPTED,
        Json(begin(&app.store, &identity, &warehouse).await?),
    ))
}

pub async fn show(
    AppState(app): AppState<App>,
    Extension(identity): Extension<Identity>,
    Path((warehouse, id)): Path<(String, String)>,
) -> Result<Json<Value>, Error> {
    reader(&identity)?;
    find(&app.store, &identity.tenant, &warehouse, &id)
        .await?
        .map(Json)
        .ok_or_else(|| Error::new(404, "Reconciliation run not found"))
}

pub async fn find(
    store: &Store,
    tenant: &str,
    warehouse: &str,
    id: &str,
) -> Result<Option<Value>, Error> {
    Ok(sqlx::query_scalar(RUN)
        .bind(tenant)
        .bind(warehouse)
        .bind(id)
        .fetch_optional(&store.pool)
        .await?)
}

/// Record a queued run, then start its proof on this process. Returns the run.
pub async fn begin(store: &Store, identity: &Identity, warehouse: &str) -> Result<Value, Error> {
    let id = domain::id();
    let mut tx = store.pool.begin().await?;
    // Admission is serialized per warehouse so the active limit holds.
    sqlx::query("SELECT pg_advisory_xact_lock(84721822,hashtext($1||'/'||$2))")
        .bind(&identity.tenant)
        .bind(warehouse)
        .execute(&mut *tx)
        .await?;
    let known: Option<i32> =
        sqlx::query_scalar("SELECT 1 FROM wms_warehouses WHERE tenant=$1 AND warehouse_id=$2")
            .bind(&identity.tenant)
            .bind(warehouse)
            .fetch_optional(&mut *tx)
            .await?;
    if known.is_none() {
        return Err(Error::new(404, "Warehouse not found"));
    }
    let active: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM wms_reconciliation_runs WHERE tenant=$1 AND warehouse_id=$2
         AND status IN('queued','running') AND deadline_at>now()",
    )
    .bind(&identity.tenant)
    .bind(warehouse)
    .fetch_one(&mut *tx)
    .await?;
    if active >= ACTIVE_PER_WAREHOUSE {
        return Err(Error::new(
            429,
            "Two reconciliation runs are already in progress for this warehouse; poll them",
        ));
    }
    sqlx::query(
        "DELETE FROM wms_reconciliation_runs WHERE tenant=$1 AND warehouse_id=$2 AND id IN(
         SELECT id FROM wms_reconciliation_runs WHERE tenant=$1 AND warehouse_id=$2
         AND (status IN('succeeded','failed') OR deadline_at<=now())
         ORDER BY requested_at DESC OFFSET $3)",
    )
    .bind(&identity.tenant)
    .bind(warehouse)
    .bind(KEEP_FINISHED)
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "INSERT INTO wms_reconciliation_runs(tenant,warehouse_id,id,requested_by,status,deadline_at)
         VALUES($1,$2,$3,$4,'queued',now()+make_interval(secs=>$5))",
    )
    .bind(&identity.tenant)
    .bind(warehouse)
    .bind(&id)
    .bind(&identity.actor)
    .bind(RUN_DEADLINE.as_secs_f64())
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    tokio::spawn(execute(
        store.clone(),
        identity.tenant.clone(),
        warehouse.to_string(),
        id.clone(),
    ));
    find(store, &identity.tenant, warehouse, &id)
        .await?
        .ok_or_else(|| Error::new(503, "Reconciliation run was not recorded"))
}

async fn execute(store: Store, tenant: String, warehouse: String, id: String) {
    let Ok(_slot) = slots().acquire().await else {
        return;
    };
    // Claim the run; one already past its deadline stays abandoned.
    let claimed = sqlx::query(
        "UPDATE wms_reconciliation_runs SET status='running',started_at=now()
         WHERE tenant=$1 AND warehouse_id=$2 AND id=$3 AND status='queued' AND deadline_at>now()",
    )
    .bind(&tenant)
    .bind(&warehouse)
    .bind(&id)
    .execute(&store.pool)
    .await;
    if !matches!(&claimed, Ok(done) if done.rows_affected() == 1) {
        return;
    }
    let finished = match store
        .reconciliation_within(&tenant, &warehouse, Some(STATEMENT_TIMEOUT))
        .await
    {
        Ok(report) => {
            sqlx::query(
                "UPDATE wms_reconciliation_runs SET status='succeeded',finished_at=now(),version=$4,result=$5
                 WHERE tenant=$1 AND warehouse_id=$2 AND id=$3 AND status='running'",
            )
            .bind(&tenant)
            .bind(&warehouse)
            .bind(&id)
            .bind(report["version"].as_i64())
            .bind(&report)
            .execute(&store.pool)
            .await
        }
        Err(error) => {
            sqlx::query(
                "UPDATE wms_reconciliation_runs SET status='failed',finished_at=now(),error=$4
                 WHERE tenant=$1 AND warehouse_id=$2 AND id=$3 AND status='running'",
            )
            .bind(&tenant)
            .bind(&warehouse)
            .bind(&id)
            .bind(format!("The proof did not complete: {}", error.message))
            .execute(&store.pool)
            .await
        }
    };
    // An unrecorded outcome leaves the run to read as abandoned at its deadline.
    if let Err(error) = finished {
        tracing::error!(error=%error, run=%id, "reconciliation run outcome was not recorded");
    }
}
