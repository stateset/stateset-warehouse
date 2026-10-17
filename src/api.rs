use crate::{
    integrations::Connections,
    store::{Error, Identity, Request, Store},
};
use axum::{
    extract::{DefaultBodyLimit, Path, Request as HttpRequest, State},
    http::{HeaderMap, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
    Extension, Json, Router,
};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::sync::Arc;
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
        (
            StatusCode::from_u16(self.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
            Json(json!({"error":self.message})),
        )
            .into_response()
    }
}
async fn authenticate(
    State(app): State<App>,
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
async fn session(State(app): State<App>, Extension(i): Extension<Identity>) -> Json<Value> {
    Json(json!({"identity":i,"demo":app.demo,"integrations":app.connections.status(&i.tenant)}))
}
async fn warehouses(
    State(app): State<App>,
    Extension(i): Extension<Identity>,
) -> Result<Json<Value>, Error> {
    Ok(Json(json!({"warehouses":app.store.list(&i.tenant).await?})))
}
async fn state(
    State(app): State<App>,
    Extension(i): Extension<Identity>,
    Path(w): Path<String>,
) -> Result<Json<Value>, Error> {
    let s = app.store.state(&i.tenant, &w).await?;
    let mut value = json!(s);
    value["reconciliation"] =
        json!({"version":s.version,"ok":s.validate().is_ok(),"rows":s.reconciliation()});
    Ok(Json(value))
}
async fn reconcile(
    State(app): State<App>,
    Extension(i): Extension<Identity>,
    Path(w): Path<String>,
) -> Result<Json<Value>, Error> {
    let s = app.store.state(&i.tenant, &w).await?;
    let rows = s.reconciliation();
    let valid = s.validate().is_ok();
    Ok(Json(json!({"version":s.version,"ok":valid,"rows":rows})))
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
    let protected = Router::new()
        .route("/v1/session", get(session))
        .route("/v1/warehouses", get(warehouses))
        .route("/v1/warehouses/{warehouse}", get(state))
        .route("/v1/warehouses/{warehouse}/reconciliation", get(reconcile))
        .route("/v1/warehouses/{warehouse}/commands", post(command))
        .route(
            "/v1/warehouses/{warehouse}/commands/{request}",
            get(receipt),
        )
        .route("/v1/warehouses/{warehouse}/outbox", get(outbox))
        .route("/v1/outbox/{id}/retry", post(retry))
        .route("/v1/events/{id}", get(event))
        .route_layer(middleware::from_fn_with_state(app.clone(), authenticate));
    Router::new()
        .merge(protected)
        .route(
            "/health",
            get(|| async { Json(json!({"status":"ok","service":"stateset-warehouse"})) }),
        )
        .route(
            "/ready",
            get({
                let pool = app.store.pool.clone();
                move || {
                    let pool = pool.clone();
                    async move {
                        match sqlx::query("SELECT 1").execute(&pool).await {
                            Ok(_) => StatusCode::OK,
                            Err(_) => StatusCode::SERVICE_UNAVAILABLE,
                        }
                    }
                }
            }),
        )
        .fallback_service(
            ServeDir::new("dashboard/dist")
                .not_found_service(ServeFile::new("dashboard/dist/index.html")),
        )
        .layer(DefaultBodyLimit::max(256 * 1024))
        .layer(TraceLayer::new_for_http())
        .with_state(app)
}
