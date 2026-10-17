//! Authenticated native workflows. Mount beneath the main identity middleware.
use crate::{
    api::App,
    domain::Order,
    native::{self, ImportDetails, NativeIntegrations, TransportPlan},
    native_store::{self, Freeze},
    store::{Error, Identity, Request},
};
use axum::{
    extract::{Path, Query, State},
    routing::{get, post},
    Extension, Json, Router,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImportRequest {
    pub request_id: String,
    pub expected_version: i64,
    pub oms_order_id: String,
    pub source_updated_at: DateTime<Utc>,
    pub details: ImportDetails,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TransportRequest {
    pub request_id: String,
    pub expected_version: i64,
    pub plan: TransportPlan,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Page {
    #[serde(default = "first_page")]
    page: u32,
}
fn first_page() -> u32 {
    1
}
fn role(i: &Identity, allow_oms: bool) -> Result<(), Error> {
    if !(["admin", "operator"].contains(&i.role.as_str()) || allow_oms && i.role == "oms") {
        return Err(Error::new(
            403,
            "This native operation requires a warehouse operator or administrator",
        ));
    }
    Ok(())
}
fn live(app: &App) -> Result<(), Error> {
    if app.demo {
        Err(Error::new(
            409,
            "Native integrations are disabled for synthetic demo warehouses",
        ))
    } else {
        Ok(())
    }
}
fn integrations() -> Result<NativeIntegrations, Error> {
    NativeIntegrations::from_env().map_err(|e| Error::new(503, e))
}
fn invalid_version(version: i64) -> Result<(), Error> {
    if version < 0 {
        Err(Error::new(
            422,
            "Expected warehouse version must be nonnegative",
        ))
    } else {
        Ok(())
    }
}
fn encode<T: Serialize>(v: &T) -> Result<Value, Error> {
    serde_json::to_value(v).map_err(|_| Error::new(422, "Invalid native input"))
}
fn external(message: String) -> Error {
    Error::new(502, message)
}

async fn oms_orders(
    State(app): State<App>,
    Extension(i): Extension<Identity>,
    Query(page): Query<Page>,
) -> Result<Json<Value>, Error> {
    role(&i, false)?;
    live(&app)?;
    if page.page == 0 {
        return Err(Error::new(422, "OMS page must be at least one"));
    }
    Ok(Json(encode(
        &integrations()?
            .list_orders(&i.tenant, page.page)
            .await
            .map_err(external)?,
    )?))
}
async fn import_oms(
    State(app): State<App>,
    Extension(i): Extension<Identity>,
    Path(w): Path<String>,
    Json(body): Json<ImportRequest>,
) -> Result<Json<Value>, Error> {
    role(&i, true)?;
    live(&app)?;
    native_store::request_id(&body.request_id)?;
    invalid_version(body.expected_version)?;
    Uuid::parse_str(&body.oms_order_id)
        .map_err(|_| Error::new(422, "OMS order ID must be a UUID"))?;
    native_store::warehouse_version(&app.store, &i.tenant, &w).await?;
    let input = encode(&body)?;
    let digest = native_store::digest("oms_import", &w, None, &input)?;
    let existing = native_store::lookup(&app.store, &i, &w, &body.request_id).await?;
    let (record, replayed) = if let Some(record) = existing {
        record.matches(&i, "oms_import", &digest)?;
        (record, true)
    } else {
        let integrations = integrations()?;
        let source = integrations
            .source_reference(&i.tenant, &body.oms_order_id)
            .map_err(external)?;
        native_store::check_import_source(&app.store, &i, &w, &body.request_id, &source).await?;
        let (raw, order) = integrations
            .fetch_order_snapshot(&i.tenant, &body.oms_order_id)
            .await
            .map_err(external)?;
        if order.updated_at != body.source_updated_at {
            return Err(Error::new(409,"OMS source revision changed; refresh and review before creating a new import request"));
        }
        let command = integrations
            .import_command(&i.tenant, &order, &body.details)
            .map_err(|e| Error::new(422, e))?;
        let command_request = Request {
            request_id: native_store::import_command_id(&i.tenant, &w, &body.request_id),
            expected_version: body.expected_version,
            command,
        };
        let frozen = json!({"command_request":command_request,"oms_order":order});
        native_store::freeze(
            &app.store,
            &i,
            &w,
            Freeze {
                kind: "oms_import",
                request_id: &body.request_id,
                digest: &digest,
                input: &input,
                expected_version: body.expected_version,
                order_id: None,
                source_snapshot: Some(&raw),
                frozen: &frozen,
            },
        )
        .await?
    };
    if record.status == "completed" {
        return Ok(Json(record.public(true)));
    }
    let command_request: Request = serde_json::from_value(record.frozen["command_request"].clone())
        .map_err(|_| Error::new(503, "Stored native import command requires repair"))?;
    // execute verifies the frozen command digest and actor before replaying its
    // receipt, including a crash after command commit but before journal commit.
    let response = match app.store.execute(&i, &w, &command_request).await {
        Ok(response) => response,
        Err(error) => {
            if error.status == 409 || error.status == 422 {
                native_store::local_conflict(&app.store, &i, &w, &body.request_id, &error.message)
                    .await?;
            }
            return Err(error);
        }
    };
    let record =
        native_store::complete_import(&app.store, &i, &w, &body.request_id, &response).await?;
    Ok(Json(record.public(replayed)))
}
async fn readiness(
    State(app): State<App>,
    Extension(i): Extension<Identity>,
    Path((w, order)): Path<(String, String)>,
) -> Result<Json<Value>, Error> {
    role(&i, false)?;
    native_store::warehouse_version(&app.store, &i.tenant, &w).await?;
    let state = app.store.state(&i.tenant, &w).await?;
    let ready = native::ready_export(&i.tenant, &state, &order).map_err(|e| Error::new(409, e))?;
    if state
        .exceptions
        .iter()
        .any(|e| e.status == "open" && e.order_id.as_deref() == Some(&order))
    {
        return Err(Error::new(
            409,
            "Resolve this order's open exception before transportation planning",
        ));
    }
    Ok(Json(
        json!({"readiness":ready,"oms_writeback":{"state":"held","reason":native::OMS_WRITEBACK_HOLD}}),
    ))
}
async fn transport(
    State(app): State<App>,
    Extension(i): Extension<Identity>,
    Path((w, order_id)): Path<(String, String)>,
    Json(body): Json<TransportRequest>,
) -> Result<Json<Value>, Error> {
    role(&i, false)?;
    live(&app)?;
    native_store::request_id(&body.request_id)?;
    invalid_version(body.expected_version)?;
    body.plan.validate().map_err(|e| Error::new(422, e))?;
    native_store::warehouse_version(&app.store, &i.tenant, &w).await?;
    let input = encode(&body)?;
    let digest = native_store::digest("tms_transport", &w, Some(&order_id), &input)?;
    let existing = native_store::lookup(&app.store, &i, &w, &body.request_id).await?;
    let replayed = existing.is_some();
    if let Some(record) = existing {
        record.matches(&i, "tms_transport", &digest)?;
        if record.status == "completed" || record.lease_active {
            return Ok(Json(record.public(true)));
        }
    } else {
        let state = app.store.state(&i.tenant, &w).await?;
        if state.version != body.expected_version {
            return Err(Error::new(
                409,
                "Warehouse changed; refresh and review before transportation planning",
            ));
        }
        if state
            .exceptions
            .iter()
            .any(|e| e.status == "open" && e.order_id.as_deref() == Some(&order_id))
        {
            return Err(Error::new(
                409,
                "Resolve this order's open exception before transportation planning",
            ));
        }
        let integrations = integrations()?;
        let prepared = integrations
            .prepare_transport(&i.tenant, &state, &order_id, &body.plan)
            .map_err(|e| Error::new(409, e))?;
        let (snapshot, _) = integrations
            .verify_transport_source_snapshot(&i.tenant, &prepared)
            .await
            .map_err(external)?;
        native_store::freeze(
            &app.store,
            &i,
            &w,
            Freeze {
                kind: "tms_transport",
                request_id: &body.request_id,
                digest: &digest,
                input: &input,
                expected_version: body.expected_version,
                order_id: Some(&order_id),
                source_snapshot: Some(&snapshot),
                frozen: &encode(&prepared)?,
            },
        )
        .await?;
    }
    match native_store::claim_transport(&app.store, &i, &w, &body.request_id, &digest).await? {
        native_store::Claim::Existing(record) => Ok(Json(record.public(true))),
        native_store::Claim::Ready { record, lease } => {
            let prepared = native_store::prepared(&record)?;
            // From this durable sending state onward every failure means the
            // remote application may have accepted the exact retained intent.
            let outcome = match integrations() {
                Ok(c) => c.execute_transport(&i.tenant, &prepared).await,
                Err(error) => Err(error.message),
            };
            let record = native_store::finish_transport(
                &app.store,
                &i,
                &w,
                &body.request_id,
                &lease,
                outcome,
            )
            .await?;
            Ok(Json(record.public(replayed)))
        }
    }
}
async fn receipt(
    State(app): State<App>,
    Extension(i): Extension<Identity>,
    Path((w, request)): Path<(String, String)>,
) -> Result<Json<Value>, Error> {
    role(&i, true)?;
    native_store::request_id(&request)?;
    native_store::warehouse_version(&app.store, &i.tenant, &w).await?;
    let record = native_store::lookup(&app.store, &i, &w, &request).await?;
    let result = match record {
        Some(record) if record.actor == i.actor => {
            let mut value = record.public(true);
            value["attempt_history"] =
                json!(native_store::attempts(&app.store, &i, &w, &request).await?);
            value
        }
        _ => json!({"found":false}),
    };
    Ok(Json(result))
}
async fn oms_proposal(
    State(app): State<App>,
    Extension(i): Extension<Identity>,
    Path((w, order_id)): Path<(String, String)>,
) -> Result<Json<Value>, Error> {
    role(&i, false)?;
    live(&app)?;
    native_store::warehouse_version(&app.store, &i.tenant, &w).await?;
    let value = app.store.detail(&i.tenant, &w, "orders", &order_id).await?;
    let order: Order = serde_json::from_value(value)
        .map_err(|_| Error::new(503, "Stored order requires repair"))?;
    let event_id: String=sqlx::query_scalar("SELECT id FROM wms_events WHERE tenant=$1 AND warehouse_id=$2 AND payload->>'type'='fulfillment.shipped' AND payload->'order'->>'id'=$3 ORDER BY version DESC LIMIT 1").bind(&i.tenant).bind(&w).bind(&order.id).fetch_optional(&app.store.pool).await?.ok_or_else(||Error::new(409,"A committed physical handoff event is required for an OMS fulfillment proposal"))?;
    let proposal = integrations()?
        .prepare_oms_fulfillment(&i.tenant, &w, &event_id, &order)
        .await
        .map_err(external)?;
    Ok(Json(proposal))
}
pub fn router() -> Router<App> {
    Router::new()
        .route("/v1/integrations/oms/orders", get(oms_orders))
        .route("/v1/warehouses/{warehouse}/imports/oms", post(import_oms))
        .route(
            "/v1/warehouses/{warehouse}/orders/{order}/readiness",
            get(readiness),
        )
        .route(
            "/v1/warehouses/{warehouse}/orders/{order}/transport",
            post(transport),
        )
        .route(
            "/v1/warehouses/{warehouse}/orders/{order}/oms-fulfillment",
            get(oms_proposal).post(crate::oms_fulfillment_api::fulfill),
        )
        .route(
            "/v1/warehouses/{warehouse}/native-requests/{request}",
            get(receipt),
        )
}
