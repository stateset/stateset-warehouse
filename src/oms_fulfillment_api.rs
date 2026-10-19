//! Conditional OMS writeback from immutable physical handoff evidence.
//! Remote requests are always preceded by a committed intent and delivery lease.
use crate::{
    api::App,
    domain::{self, Order},
    native::NativeIntegrations,
    native_store::{self, Claim, Freeze, NativeRecord},
    oms_client::{validate_acknowledgement, Acknowledgement, PreparedFulfillment},
    store::{Error, Identity, Store},
};
use axum::{
    extract::{Path, State},
    Extension, Json,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::{PgConnection, Row};

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FulfillmentRequest {
    pub request_id: String,
    pub expected_version: i64,
}
fn record(row: sqlx::postgres::PgRow) -> Result<NativeRecord, Error> {
    Ok(NativeRecord {
        request_id: row.try_get("request_id")?,
        kind: row.try_get("kind")?,
        actor: row.try_get("actor")?,
        digest: row.try_get("digest")?,
        expected_version: row.try_get("expected_version")?,
        order_id: row.try_get("order_id")?,
        status: row.try_get("status")?,
        attempts: row.try_get("attempts")?,
        frozen: row.try_get("frozen")?,
        source_snapshot: row.try_get("source_snapshot")?,
        response: row.try_get("response")?,
        last_error: row.try_get("last_error")?,
        lease_active: row.try_get("lease_active")?,
    })
}
async fn committed_shipment(
    c: &mut PgConnection,
    i: &Identity,
    warehouse: &str,
    order_id: &str,
    event_id: Option<&str>,
) -> Result<(String, Order), Error> {
    let (order, provenance) = crate::shipments::source(c, &i.tenant, warehouse, order_id)
        .await
        .map_err(|error| {
            if error.status == 422 {
                Error::new(
                    409,
                    "Conditional OMS fulfillment requires a completed physical handoff",
                )
            } else {
                error
            }
        })?;
    if order.status != "shipped" {
        return Err(Error::new(409,"Conditional OMS fulfillment requires every original order line to be physically handed off"));
    }
    let id = provenance["event_id"]
        .as_str()
        .filter(|_| provenance["kind"] == "event")
        .ok_or_else(|| {
            Error::new(
                409,
                "The current OMS receiver requires one complete original handoff; multiple shipment handoffs require a separately qualified receiver contract",
            )
        })?;
    if event_id.is_some_and(|expected| expected != id) {
        return Err(Error::new(
            409,
            "Retained OMS handoff references another physical shipment",
        ));
    }
    Ok((id.into(), order))
}
/// Claim the exact retained intent. Callers must finish the lease with the CAS
/// journal operation; no database transaction is held across remote HTTP.
pub async fn claim_fulfillment(
    store: &Store,
    i: &Identity,
    warehouse: &str,
    request: &str,
    digest: &str,
) -> Result<Claim, Error> {
    let mut tx = store.pool.begin().await?;
    let _: i64 = sqlx::query_scalar(
        "SELECT version FROM wms_warehouses WHERE tenant=$1 AND warehouse_id=$2 FOR UPDATE",
    )
    .bind(&i.tenant)
    .bind(warehouse)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(|| Error::new(404, "Warehouse not found"))?;
    let value=sqlx::query("SELECT *,COALESCE(lease_until>now(),false) AS lease_active FROM wms_native_requests WHERE tenant=$1 AND warehouse_id=$2 AND request_id=$3 FOR UPDATE")
        .bind(&i.tenant).bind(warehouse).bind(request).fetch_optional(&mut *tx).await?.ok_or_else(||Error::new(404,"Native fulfillment request not found"))?;
    let retained = record(value)?;
    retained.matches(i, "oms_fulfillment", digest)?;
    if ["completed", "rejected"].contains(&retained.status.as_str()) || retained.lease_active {
        tx.commit().await?;
        return Ok(Claim::Existing(retained));
    }
    let prepared: PreparedFulfillment = serde_json::from_value(retained.frozen.clone())
        .map_err(|_| Error::new(503, "Stored conditional OMS fulfillment requires repair"))?;
    prepared
        .validate(&i.tenant)
        .map_err(|e| Error::new(503, e))?;
    if prepared.tenant != i.tenant
        || prepared.warehouse_id != warehouse
        || retained.order_id.as_deref() != Some(prepared.order_id.as_str())
    {
        return Err(Error::new(
            503,
            "Stored conditional OMS fulfillment identity requires repair",
        ));
    }
    if retained.attempts == 0 {
        let (_, order) = committed_shipment(
            &mut tx,
            i,
            warehouse,
            &prepared.order_id,
            Some(&prepared.event_id),
        )
        .await?;
        if prepared.payload.carrier != order.carrier
            || prepared.payload.handoff_reference != order.handoff_reference
            || order.shipped_at.as_deref() != Some(prepared.payload.handed_off_at.as_str())
            || order.source_order_id.as_deref()
                != Some(
                    format!(
                        "oms:{}:{}",
                        prepared.recipient_tenant_id, prepared.payload.order_id
                    )
                    .as_str(),
                )
            || prepared.payload.lines.len() != order.lines.len()
            || prepared.payload.lines.iter().any(|line| {
                !order.lines.iter().any(|actual| {
                    actual.id == line.line_id
                        && actual.sku == line.sku
                        && actual.quantity == line.quantity_each
                })
            })
        {
            return Err(Error::new(
                409,
                "Retained OMS handoff facts differ from the committed physical shipment",
            ));
        }
    }
    sqlx::query("UPDATE wms_native_attempts SET outcome='uncertain',detail='Lease expired before outcome was committed; remote acceptance is uncertain',finished_at=now() WHERE tenant=$1 AND warehouse_id=$2 AND request_id=$3 AND outcome='started'")
        .bind(&i.tenant).bind(warehouse).bind(request).execute(&mut *tx).await?;
    let lease = domain::id();
    sqlx::query(
        "INSERT INTO wms_native_attempts(id,tenant,warehouse_id,request_id) VALUES($1,$2,$3,$4)",
    )
    .bind(&lease)
    .bind(&i.tenant)
    .bind(warehouse)
    .bind(request)
    .execute(&mut *tx)
    .await?;
    sqlx::query("UPDATE wms_native_requests SET status='sending',attempts=attempts+1,lease_token=$1,lease_until=now()+interval '30 seconds',updated_at=now() WHERE tenant=$2 AND warehouse_id=$3 AND request_id=$4")
        .bind(&lease).bind(&i.tenant).bind(warehouse).bind(request).execute(&mut *tx).await?;
    let value=sqlx::query("SELECT *,COALESCE(lease_until>now(),false) AS lease_active FROM wms_native_requests WHERE tenant=$1 AND warehouse_id=$2 AND request_id=$3")
        .bind(&i.tenant).bind(warehouse).bind(request).fetch_one(&mut *tx).await?;
    let record = record(value)?;
    tx.commit().await?;
    Ok(Claim::Ready { record, lease })
}
/// Record only an exact, validated receiver acknowledgment as a terminal
/// outcome. A transport error or unrecognized body retains uncertainty.
pub async fn finish_fulfillment(
    store: &Store,
    i: &Identity,
    warehouse: &str,
    request: &str,
    lease: &str,
    outcome: Result<Value, String>,
) -> Result<NativeRecord, Error> {
    let mut tx = store.pool.begin().await?;
    let value = sqlx::query("SELECT *,COALESCE(lease_until>now(),false) AS lease_active FROM wms_native_requests WHERE tenant=$1 AND warehouse_id=$2 AND request_id=$3 FOR UPDATE")
        .bind(&i.tenant).bind(warehouse).bind(request).fetch_optional(&mut *tx).await?
        .ok_or_else(||Error::new(503,"Native OMS fulfillment receipt unavailable; recover the original request"))?;
    let active_lease: Option<String> = value.try_get("lease_token")?;
    let retained = record(value)?;
    retained.matches(i, "oms_fulfillment", &retained.digest)?;
    if retained.status != "sending" || active_lease.as_deref() != Some(lease) {
        tx.commit().await?;
        return Ok(retained);
    }
    let validated = outcome.and_then(|value| {
        let prepared: PreparedFulfillment = serde_json::from_value(retained.frozen.clone())
            .map_err(|_| {
                "Stored conditional OMS fulfillment requires repair; retain the original request"
            })?;
        if prepared.warehouse_id != warehouse
            || retained.order_id.as_deref() != Some(prepared.order_id.as_str())
        {
            return Err(
                "Stored OMS fulfillment scope differs; retain the original handoff request".into(),
            );
        }
        let acknowledgement = validate_acknowledgement(&i.tenant, &prepared, &value)?;
        Ok((value, acknowledgement))
    });
    let (status, response, message) = match validated {
        Ok((value, Acknowledgement::Rejected)) => ("rejected", Some(value), String::new()),
        Ok((value, Acknowledgement::Fulfilled)) => ("completed", Some(value), String::new()),
        Err(error) => ("uncertain", None, error),
    };
    let n=sqlx::query("UPDATE wms_native_requests SET status=$1,response=$2,last_error=$3,lease_token=NULL,lease_until=NULL,updated_at=now() WHERE tenant=$4 AND warehouse_id=$5 AND request_id=$6 AND actor=$7 AND lease_token=$8 AND status='sending' AND kind='oms_fulfillment'")
        .bind(status).bind(&response).bind(&message).bind(&i.tenant).bind(warehouse).bind(request).bind(&i.actor).bind(lease).execute(&mut *tx).await?.rows_affected();
    if n == 1 {
        sqlx::query("UPDATE wms_native_attempts SET outcome=$1,response=$2,detail=$3,finished_at=now() WHERE id=$4 AND outcome='started'")
            .bind(match status {"completed"=>"accepted","rejected"=>"rejected",_=>"uncertain"}).bind(&response).bind(&message).bind(lease).execute(&mut *tx).await?;
    }
    let value=sqlx::query("SELECT *,COALESCE(lease_until>now(),false) AS lease_active FROM wms_native_requests WHERE tenant=$1 AND warehouse_id=$2 AND request_id=$3")
        .bind(&i.tenant).bind(warehouse).bind(request).fetch_optional(&mut *tx).await?.ok_or_else(||Error::new(503,"Native OMS fulfillment receipt unavailable; recover the original request"))?;
    let record = record(value)?;
    tx.commit().await?;
    Ok(record)
}
pub async fn fulfill(
    State(app): State<App>,
    Extension(i): Extension<Identity>,
    Path((warehouse, order_id)): Path<(String, String)>,
    Json(body): Json<FulfillmentRequest>,
) -> Result<Json<Value>, Error> {
    if !["operator", "admin"].contains(&i.role.as_str()) {
        return Err(Error::new(
            403,
            "OMS fulfillment requires a warehouse operator or administrator",
        ));
    }
    if app.demo {
        return Err(Error::new(
            409,
            "Native integrations are disabled for synthetic demo warehouses",
        ));
    }
    native_store::request_id(&body.request_id)?;
    if body.expected_version < 0 {
        return Err(Error::new(
            422,
            "Expected warehouse version must be nonnegative",
        ));
    }
    let version = native_store::warehouse_version(&app.store, &i.tenant, &warehouse).await?;
    let input = serde_json::to_value(&body)
        .map_err(|_| Error::new(422, "Invalid native fulfillment input"))?;
    let digest = native_store::digest("oms_fulfillment", &warehouse, Some(&order_id), &input)?;
    let existing = native_store::lookup(&app.store, &i, &warehouse, &body.request_id).await?;
    let replayed = existing.is_some();
    if let Some(record) = existing {
        record.matches(&i, "oms_fulfillment", &digest)?;
        if ["completed", "rejected"].contains(&record.status.as_str()) || record.lease_active {
            return Ok(Json(record.public(true)));
        }
    } else {
        if version != body.expected_version {
            return Err(Error::new(
                409,
                "Warehouse changed; refresh and review before starting OMS fulfillment",
            ));
        }
        let occupied:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM wms_native_requests WHERE tenant=$1 AND warehouse_id=$2 AND order_id=$3 AND kind='oms_fulfillment' AND status<>'rejected')")
            .bind(&i.tenant).bind(&warehouse).bind(&order_id).fetch_one(&app.store.pool).await?;
        if occupied {
            return Err(Error::new(409,"This physical shipment already has a retained OMS fulfillment; recover its original request instead of creating another"));
        }
        let mut tx = app.store.pool.begin().await?;
        sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY")
            .execute(&mut *tx)
            .await?;
        let (event_id, order) =
            committed_shipment(&mut tx, &i, &warehouse, &order_id, None).await?;
        tx.commit().await?;
        let native = NativeIntegrations::from_env().map_err(|e| Error::new(503, e))?;
        let (source, prepared) = native
            .prepare_conditional_fulfillment(&i.tenant, &warehouse, &event_id, &order)
            .await
            .map_err(|e| Error::new(424, format!("OMS preparation failed; no OMS POST was attempted by this preparation: {e}")))?;
        let frozen = serde_json::to_value(prepared)
            .map_err(|_| Error::new(503, "Conditional OMS fulfillment could not be retained"))?;
        native_store::freeze(
            &app.store,
            &i,
            &warehouse,
            Freeze {
                kind: "oms_fulfillment",
                request_id: &body.request_id,
                digest: &digest,
                input: &input,
                expected_version: body.expected_version,
                order_id: Some(&order_id),
                source_snapshot: Some(&source),
                frozen: &frozen,
            },
        )
        .await?;
    }
    match claim_fulfillment(&app.store, &i, &warehouse, &body.request_id, &digest).await? {
        Claim::Existing(record) => Ok(Json(record.public(true))),
        Claim::Ready { record, lease } => {
            let prepared: PreparedFulfillment = serde_json::from_value(record.frozen.clone())
                .map_err(|_| {
                    Error::new(503, "Stored conditional OMS fulfillment requires repair")
                })?;
            let outcome = match NativeIntegrations::from_env() {
                Ok(native) => {
                    native
                        .execute_conditional_fulfillment(&i.tenant, &prepared)
                        .await
                }
                Err(error) => Err(error),
            };
            let record = finish_fulfillment(
                &app.store,
                &i,
                &warehouse,
                &body.request_id,
                &lease,
                outcome,
            )
            .await?;
            Ok(Json(record.public(replayed)))
        }
    }
}
