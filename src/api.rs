use crate::{
    integrations::Connections,
    store::{Error, Identity, Request, Store},
};
use axum::{
    extract::{DefaultBodyLimit, MatchedPath, Path, Query, Request as HttpRequest, State},
    http::{HeaderMap, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{any, get, post, put},
    Extension, Json, Router,
};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{sync::Arc, time::Duration};
use subtle::ConstantTimeEq;
use tower_http::{
    services::{ServeDir, ServeFile},
    trace::TraceLayer,
};

#[derive(Clone)]
pub struct App {
    pub store: Store,
    pub credentials: Arc<Vec<(String, Identity)>>,
    pub demo: bool,
    pub connections: Connections,
}
impl IntoResponse for Error {
    fn into_response(self) -> Response {
        let mut payload = json!({"error": self.message});
        if let Some(outcome) = self.command_outcome {
            payload["command_outcome"] = json!(outcome);
        }
        (
            StatusCode::from_u16(self.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
            Json(payload),
        )
            .into_response()
    }
}
async fn authenticate(
    State((app, workos)): State<(App, Option<crate::auth::WorkOs>)>,
    headers: HeaderMap,
    mut request: HttpRequest,
    next: Next,
) -> Response {
    let token = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .unwrap_or_default();
    let digest = Sha256::digest(token.as_bytes());
    let identity = app
        .credentials
        .iter()
        .find(|(key, _)| bool::from(Sha256::digest(key.as_bytes()).ct_eq(&digest)))
        .map(|(_, i)| i.clone());
    let identity = match (identity, workos) {
        (Some(identity), _) => Some(identity),
        (None, Some(auth)) if !token.is_empty() => {
            match tokio::time::timeout(Duration::from_secs(4), auth.authenticate(token)).await {
                Ok(Ok(identity)) => Some(identity),
                Ok(Err(error)) => return error.into_response(),
                Err(_) => {
                    return Error::new(503, "Sign-in verification is temporarily unavailable")
                        .into_response()
                }
            }
        }
        _ => None,
    };
    let Some(identity) = identity else {
        return Error::new(401, "A valid warehouse API credential is required").into_response();
    };
    request.extensions_mut().insert(identity);
    let mut response = next.run(request).await;
    response.headers_mut().insert(
        "cache-control",
        axum::http::HeaderValue::from_static("no-store"),
    );
    response
}
async fn response_policy(
    State(auth_origin): State<Option<String>>,
    mut request: HttpRequest,
    next: Next,
) -> Response {
    // Correlation IDs are generated here, never trusted from client headers.
    let request_id = axum::http::HeaderValue::from_str(&uuid::Uuid::new_v4().to_string())
        .expect("UUID is a valid response header");
    request
        .headers_mut()
        .insert("x-request-id", request_id.clone());
    let path = request.uri().path();
    let no_store = path == "/mcp"
        || path == "/v1"
        || path.starts_with("/v1/")
        || ["/", "/login", "/ready", "/health", "/auth/config"].contains(&path);
    let mut response = next.run(request).await;
    let headers = response.headers_mut();
    headers.insert("x-request-id", request_id);
    for (name, value) in [
        ("x-content-type-options", "nosniff"),
        ("x-frame-options", "DENY"),
        ("referrer-policy", "no-referrer"),
        ("content-security-policy", "default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data:; connect-src 'self'; object-src 'none'; base-uri 'none'; frame-ancestors 'none'; form-action 'self'"),
    ] {
        headers.insert(name, axum::http::HeaderValue::from_static(value));
    }
    if let Some(origin) = auth_origin {
        let policy = format!("default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data:; connect-src 'self' {origin}; object-src 'none'; base-uri 'none'; frame-ancestors 'none'; form-action 'self'");
        headers.insert(
            "content-security-policy",
            axum::http::HeaderValue::from_str(&policy).expect("validated HTTPS origin"),
        );
    }
    if no_store {
        headers.insert(
            "cache-control",
            axum::http::HeaderValue::from_static("no-store"),
        );
    }
    response
}

async fn ready(State(app): State<App>) -> StatusCode {
    match tokio::time::timeout(
        Duration::from_secs(2),
        sqlx::query("SELECT 1").execute(&app.store.pool),
    )
    .await
    {
        Ok(Ok(_)) => StatusCode::OK,
        _ => StatusCode::SERVICE_UNAVAILABLE,
    }
}

async fn unknown_api() -> Error {
    Error::new(404, "Warehouse API route not found")
}
async fn session(State(app): State<App>, Extension(i): Extension<Identity>) -> Json<Value> {
    let native = crate::native::NativeIntegrations::from_env()
        .map(|c| c.summary(&i.tenant))
        .unwrap_or_else(|_| json!({"error":"Native integration configuration requires repair"}));
    Json(
        json!({"identity":i,"demo":app.demo,"default_warehouse_id":if app.demo {"wh-pdx"} else {"wh-main"},"integrations":app.connections.status(&i.tenant),"native_integrations":native}),
    )
}
async fn warehouses(
    State(app): State<App>,
    Extension(i): Extension<Identity>,
    query: Result<Query<WarehousesQuery>, axum::extract::rejection::QueryRejection>,
) -> Result<Json<Value>, Error> {
    let Query(query) = query.map_err(|_| Error::new(422, "Invalid warehouse page query"))?;
    Ok(Json(
        app.store
            .warehouse_page(
                &i.tenant,
                query.after.as_deref().unwrap_or(""),
                query.limit.unwrap_or(100),
            )
            .await?,
    ))
}
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct WarehousesQuery {
    after: Option<String>,
    limit: Option<i64>,
}
async fn state(
    State(app): State<App>,
    Extension(i): Extension<Identity>,
    Path(w): Path<String>,
) -> Result<Json<crate::domain::OperationalSnapshot>, Error> {
    Ok(Json(app.store.snapshot(&i.tenant, &w).await?))
}
async fn reconcile(
    State(app): State<App>,
    Extension(i): Extension<Identity>,
    Path(w): Path<String>,
) -> Result<Json<Value>, Error> {
    Ok(Json(app.store.reconciliation(&i.tenant, &w).await?))
}
#[derive(serde::Deserialize)]
struct HistoryQuery {
    before: Option<i64>,
    limit: Option<i64>,
}
async fn history(
    State(app): State<App>,
    Extension(i): Extension<Identity>,
    Path((w, kind)): Path<(String, String)>,
    Query(q): Query<HistoryQuery>,
) -> Result<Json<Value>, Error> {
    Ok(Json(
        app.store
            .history(&i.tenant, &w, &kind, q.before, q.limit.unwrap_or(50))
            .await?,
    ))
}
async fn order_detail(
    State(app): State<App>,
    Extension(i): Extension<Identity>,
    Path((w, id)): Path<(String, String)>,
) -> Result<Json<Value>, Error> {
    Ok(Json(app.store.detail(&i.tenant, &w, "orders", &id).await?))
}
async fn receipt_detail(
    State(app): State<App>,
    Extension(i): Extension<Identity>,
    Path((w, id)): Path<(String, String)>,
) -> Result<Json<Value>, Error> {
    Ok(Json(
        app.store.detail(&i.tenant, &w, "receipts", &id).await?,
    ))
}
async fn return_eligibility(
    State(app): State<App>,
    Extension(i): Extension<Identity>,
    Path((w, id)): Path<(String, String)>,
) -> Result<Json<Value>, Error> {
    Ok(Json(
        crate::returns::eligible(&app.store, &i, &w, &id).await?,
    ))
}
async fn command(
    State(app): State<App>,
    Extension(i): Extension<Identity>,
    Path(w): Path<String>,
    Json(request): Json<Request>,
) -> Result<Json<Value>, Error> {
    Ok(Json(app.store.execute(&i, &w, &request).await?))
}
async fn receipt(
    State(app): State<App>,
    Extension(i): Extension<Identity>,
    Path((w, r)): Path<(String, String)>,
) -> Result<Json<Value>, Error> {
    Ok(Json(app.store.receipt(&i, &w, &r).await?))
}
async fn outbox(
    State(app): State<App>,
    Extension(i): Extension<Identity>,
    Path(w): Path<String>,
) -> Result<Json<Value>, Error> {
    Ok(Json(json!({"items":app.store.outbox(&i.tenant,&w).await?})))
}
async fn retry(
    State(app): State<App>,
    Extension(i): Extension<Identity>,
    Path(id): Path<String>,
) -> Result<Json<Value>, Error> {
    Ok(Json(app.store.retry(&i, &id).await?))
}
async fn event(
    State(app): State<App>,
    Extension(i): Extension<Identity>,
    Path(id): Path<String>,
) -> Result<Json<Value>, Error> {
    use sqlx::Row;
    let row = sqlx::query("SELECT payload FROM wms_events WHERE tenant=$1 AND id=$2")
        .bind(&i.tenant)
        .bind(id)
        .fetch_optional(&app.store.pool)
        .await?
        .ok_or_else(|| Error::new(404, "Event not found"))?;
    Ok(Json(row.try_get("payload")?))
}
pub fn router(app: App) -> Router {
    router_with_auth(app, None)
}
pub fn router_with_auth(app: App, workos: Option<crate::auth::WorkOs>) -> Router {
    router_with_auth_and_mcp(app, workos, crate::mcp::Config::default())
}
pub fn router_with_auth_and_mcp(
    app: App,
    workos: Option<crate::auth::WorkOs>,
    mcp_config: crate::mcp::Config,
) -> Router {
    router_complete(app, workos, mcp_config, crate::console::Assets::from_env())
}
pub fn router_with_console_assets(app: App, assets: crate::console::Assets) -> Router {
    router_complete(app, None, crate::mcp::Config::default(), assets)
}
fn router_complete(
    app: App,
    workos: Option<crate::auth::WorkOs>,
    mcp_config: crate::mcp::Config,
    assets: crate::console::Assets,
) -> Router {
    let public_auth = workos
        .as_ref()
        .map(|w| w.config.public())
        .unwrap_or(json!({"provider":"api_key"}));
    let auth_origin = workos.as_ref().map(|w| w.config.api_origin.clone());
    let protected = business_routes_with_console(assets)
        .merge(crate::mcp::router(app.clone(), mcp_config))
        .route_layer(middleware::from_fn_with_state(
            (app.clone(), workos),
            authenticate,
        ));
    Router::new()
        .merge(protected)
        .route(
            "/auth/config",
            get(move || async move { Json(public_auth) }),
        )
        .route(
            "/health",
            get(|| async { Json(json!({"status":"ok","service":"stateset-warehouse"})) }),
        )
        .route("/ready", get(ready))
        .fallback_service(
            ServeDir::new("dashboard/dist")
                .not_found_service(ServeFile::new("dashboard/dist/index.html")),
        )
        .layer(DefaultBodyLimit::max(256 * 1024))
        .layer(
            TraceLayer::new_for_http().make_span_with(|request: &HttpRequest| {
                let route = request
                    .extensions()
                    .get::<MatchedPath>()
                    .map(MatchedPath::as_str)
                    .unwrap_or("unmatched");
                let request_id = request
                    .headers()
                    .get("x-request-id")
                    .and_then(|value| value.to_str().ok())
                    .unwrap_or("unavailable");
                tracing::info_span!("warehouse_http", method=%request.method(), route, request_id)
            }),
        )
        .layer(middleware::from_fn_with_state(auth_origin, response_policy))
        .with_state(app)
}

/// Private trusted dispatch after the outer transport authenticates Identity.
pub(crate) fn business_routes() -> Router<App> {
    business_routes_with_console(crate::console::Assets::from_env())
}
fn business_routes_with_console(assets: crate::console::Assets) -> Router<App> {
    Router::new()
        .route("/v1/session", get(session))
        .route("/v1/warehouses", get(warehouses))
        .route(
            "/v1/warehouses/{warehouse}/icommerce/preview",
            post(crate::icommerce::preview),
        )
        .route(
            "/v1/warehouses/{warehouse}/icommerce/inventory",
            post(crate::icommerce::inventory_endpoint),
        )
        .route("/v1/lot-trace", get(crate::lot_trace::page))
        .route("/v1/serials", get(crate::serial_reads::current))
        .route(
            "/v1/warehouses/{warehouse}/serial-observation-plan",
            post(crate::serial_plan::plan),
        )
        .route("/v1/serial-history", get(crate::serial_reads::history))
        .route(
            "/v1/serial-observations",
            get(crate::serial_reads::observations),
        )
        .route(
            "/v1/warehouses/{warehouse}/unit-packs",
            get(crate::units::page),
        )
        .route(
            "/v1/warehouses/{warehouse}/unit-observations",
            get(crate::units::observations),
        )
        .route(
            "/v1/warehouses/{warehouse}/transfers/{id}/losses",
            get(crate::transfers::losses),
        )
        .route("/v1/warehouses/{warehouse}", get(state))
        .route("/v1/warehouses/{warehouse}/reconciliation", get(reconcile))
        .route(
            "/v1/warehouses/{warehouse}/reconciliation/runs",
            post(crate::reconciliation_runs::start),
        )
        .route(
            "/v1/warehouses/{warehouse}/reconciliation/runs/{id}",
            get(crate::reconciliation_runs::show),
        )
        .route("/v1/warehouses/{warehouse}/history/{kind}", get(history))
        .route("/v1/warehouses/{warehouse}/orders/{id}", get(order_detail))
        .route(
            "/v1/warehouses/{warehouse}/orders/{id}/shipments",
            get(crate::split_fulfillment::endpoint),
        )
        .route(
            "/v1/warehouses/{warehouse}/orders/{id}/return-eligibility",
            get(return_eligibility),
        )
        .route(
            "/v1/warehouses/{warehouse}/receipts/{id}",
            get(receipt_detail),
        )
        .route(
            "/v1/warehouses/{warehouse}/transfers",
            get(crate::transfers::page),
        )
        .route(
            "/v1/warehouses/{warehouse}/lots",
            get(crate::lot_store::page),
        )
        .route("/v1/warehouses/{warehouse}/commands", post(command))
        .route(
            "/v1/warehouses/{warehouse}/replenishment/plan",
            post(crate::replenishment::endpoint),
        )
        .route(
            "/v1/warehouses/{warehouse}/replenishment/rules",
            get(crate::replenishment::rules).put(crate::replenishment::replace_rules),
        )
        .route(
            "/v1/warehouses/{warehouse}/replenishment/due",
            get(crate::replenishment::due),
        )
        .route(
            "/v1/warehouses/{warehouse}/putaway/plan",
            post(crate::putaway::endpoint),
        )
        .route(
            "/v1/warehouses/{warehouse}/work",
            get(crate::work_tasks::list),
        )
        .route(
            "/v1/warehouses/{warehouse}/work/discover",
            post(crate::work_tasks::discover),
        )
        .route(
            "/v1/warehouses/{warehouse}/work/{task}/action",
            post(crate::work_tasks::action),
        )
        .route(
            "/v1/warehouses/{warehouse}/work/{task}/review",
            post(crate::work_tasks::review),
        )
        .route(
            "/v1/warehouses/{warehouse}/picking/batch-plan",
            post(crate::batch_pick::endpoint),
        )
        .route(
            "/v1/warehouses/{warehouse}/counts/plan",
            post(crate::cycle_counts::endpoint),
        )
        .route(
            "/v1/warehouses/{warehouse}/labels",
            post(crate::labels::endpoint),
        )
        .route(
            "/v1/warehouses/{warehouse}/scans/parse",
            post(crate::master_data::parse_scan),
        )
        .route(
            "/v1/warehouses/{warehouse}/products/{sku}/gtins",
            get(crate::master_data::product_gtins).put(crate::master_data::replace_product_gtins),
        )
        .route(
            "/v1/warehouses/{warehouse}/capacity",
            get(crate::master_data::capacities),
        )
        .route(
            "/v1/warehouses/{warehouse}/locations/{code}/capacity",
            put(crate::master_data::set_capacity),
        )
        .route(
            "/v1/warehouses/{warehouse}/packages/{package}/carrier-labels",
            get(crate::carrier_labels::list).post(crate::carrier_labels::record),
        )
        .route(
            "/v1/warehouses/{warehouse}/packages/{package}/carrier-labels/{label}/void",
            post(crate::carrier_labels::void),
        )
        .route(
            "/v1/warehouses/{warehouse}/commands/{request}",
            get(receipt),
        )
        .route("/v1/warehouses/{warehouse}/outbox", get(outbox))
        .route("/v1/outbox/{id}/retry", post(retry))
        .route("/v1/events/{id}", get(event))
        .merge(crate::native_api::router())
        .merge(crate::operations::router())
        .merge(crate::console::router_with_assets(assets))
        .route("/v1", any(unknown_api))
        .route("/v1/", any(unknown_api))
        .route("/v1/{*path}", any(unknown_api))
}
