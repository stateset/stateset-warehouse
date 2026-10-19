//! Read-only resolution of the original command's observation groups. A plan
//! neither validates a physical action nor replaces its captured write fences.
use crate::{
    api::App,
    domain::Command,
    store::{Error, Identity, Result},
};
use axum::{
    extract::{Path, State},
    Extension, Json,
};
use serde::Deserialize;
use serde_json::{json, Value};
use sqlx::Row;
use std::collections::BTreeSet;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlanRequest {
    plan_id: String,
    command: Command,
}
pub async fn plan(
    State(app): State<App>,
    Extension(i): Extension<Identity>,
    Path(w): Path<String>,
    Json(r): Json<PlanRequest>,
) -> Result<Json<Value>> {
    if !r.command.allowed(&i.role) {
        return Err(Error::new(403, "Your role does not permit this action"));
    }
    if uuid::Uuid::parse_str(&r.plan_id).is_err() {
        return Err(Error::new(
            422,
            "Observation plan requires its original correlation ID",
        ));
    }
    let c = json!(&r.command);
    let expected = match &r.command {
        Command::CountStock { .. }
        | Command::MoveStock { .. }
        | Command::Receive { .. }
        | Command::Pick { .. }
        | Command::Unpick { .. }
        | Command::ReceiveTransfer { .. }
        | Command::RecordTransferLoss { .. }
        | Command::ReverseTransferLoss { .. } => 1,
        Command::CreateReturn { lines, .. } => lines.len(),
        Command::PackShipment { lines, .. } => lines.len(),
        Command::DispatchTransfer { lines, .. } => lines.len(),
        _ => {
            return Err(Error::new(
                422,
                "This command does not take explicit serial observations",
            ))
        }
    };
    if !(1..=100).contains(&expected) {
        return Err(Error::new(
            422,
            "Observe one to 100 original quantity targets in a command",
        ));
    }
    let mut tx = app.store.pool.begin().await?;
    sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY")
        .execute(&mut *tx)
        .await?;
    let version: i64 = sqlx::query_scalar(
        "SELECT version FROM wms_warehouses WHERE tenant=$1 AND warehouse_id=$2",
    )
    .bind(&i.tenant)
    .bind(&w)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(|| Error::new(404, "Warehouse not found"))?;
    let rows=sqlx::query("SELECT q.*,m.controlled,p.data->'serial_controlled'='true'::jsonb AS product_controlled,wms_serial_mode_original(m) AS mode_ok,CASE WHEN m.controlled THEN wms_serial_head_bound(h) ELSE true END AS head_ok FROM wms_serial_targets($1,$2,$3) q LEFT JOIN wms_products p ON p.tenant=$1 AND p.warehouse_id=$2 AND p.sku=q.sku LEFT JOIN wms_serial_modes m ON m.tenant=p.tenant AND m.sku=p.sku LEFT JOIN wms_serial_heads h ON h.tenant=m.tenant AND h.sku=m.sku ORDER BY q.target LIMIT 101")
        .bind(&i.tenant).bind(&w).bind(&c).fetch_all(&mut *tx).await?;
    if rows.len() != expected {
        return Err(Error::new(
            404,
            "An original quantity target was not found in this facility",
        ));
    }
    let mut seen = BTreeSet::new();
    let mut items = Vec::new();
    let mut labels = 0i64;
    for row in rows {
        let target: Value = row.try_get("target")?;
        let sku: String = row.try_get("sku")?;
        if !seen.insert(target.to_string()) {
            return Err(Error::new(
                422,
                "Each original quantity target must be unique",
            ));
        }
        let controlled = row
            .try_get::<Option<bool>, _>("controlled")?
            .ok_or_else(|| Error::new(404, "Original target product not found"))?;
        if row.try_get::<Option<bool>, _>("mode_ok")? != Some(true)
            || row.try_get::<Option<bool>, _>("head_ok")? != Some(true)
            || row
                .try_get::<Option<bool>, _>("product_controlled")?
                .unwrap_or(false)
                != controlled
        {
            return Err(Error::new(
                503,
                "Original serial mode or identity review disagrees; observation plan held",
            ));
        }
        let n: i64 = row.try_get("each_quantity")?;
        if n < 0 || n == 0 && !row.try_get::<bool, _>("allow_zero")? {
            return Err(Error::new(422, "Observe a valid whole each quantity"));
        }
        if controlled {
            labels = labels
                .checked_add(n)
                .ok_or_else(|| Error::new(422, "Observation exceeds the supported complete set"))?;
            if labels > 10_000 {
                return Err(Error::new(422,"Complete serial observation exceeds 10000 labels; reviewed large-count sessions are not available"));
            }
            items.push(json!({"target":target,"sku":sku,"each_quantity":n}));
        }
    }
    tx.commit().await?;
    Ok(Json(
        json!({"schema_version":1,"plan_id":r.plan_id,"warehouse_id":w,"command_type":c["type"],"targets":items,"required_labels":labels,"resolved_targets":expected,"observed_warehouse_version":version,"write_review":"retain_original_warehouse_and_resource_versions; this_plan_does_not_authorize_a_save"}),
    ))
}
