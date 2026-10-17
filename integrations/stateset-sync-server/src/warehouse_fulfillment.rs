//! Conditional complete-order physical handoff receiver for StateSet WMS.
//! Mounted behind the existing tenant authentication and orders RBAC middleware.
use crate::http::{
    error::{ApiError, ApiResult},
    extractor::TenantRequest,
    response::ApiSuccess,
    state::AppState,
};
use axum::{extract::State, Json};
use chrono::DateTime;
use serde::{Deserialize, Serialize};
#[cfg(test)]
use serde_json::json;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use uuid::Uuid;

pub const CONTRACT: &str = "stateset.wms-fulfillment.v1";

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct HandoffLine {
    pub line_id: String,
    pub sku: String,
    pub quantity_each: i64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct WarehouseHandoff {
    pub order_id: String,
    pub request_id: String,
    pub event_id: String,
    pub expected_updated_at: String,
    pub warehouse_id: String,
    pub warehouse_order_id: String,
    pub carrier: String,
    pub handoff_reference: String,
    pub handed_off_at: String,
    pub lines: Vec<HandoffLine>,
}

fn text(value: &str, field: &str, max: usize) -> ApiResult<()> {
    if value.trim().is_empty()
        || value.trim() != value
        || value.len() > max
        || value.chars().any(char::is_control)
    {
        return Err(ApiError::validation(format!(
            "{field} is required and must contain at most {max} bytes without control characters"
        )));
    }
    Ok(())
}

impl WarehouseHandoff {
    fn validate(&self) -> ApiResult<()> {
        for (value, field) in [
            (&self.order_id, "order_id"),
            (&self.request_id, "request_id"),
            (&self.event_id, "event_id"),
            (&self.warehouse_order_id, "warehouse_order_id"),
        ] {
            let parsed = Uuid::parse_str(value)
                .map_err(|_| ApiError::validation(format!("{field} must be a UUID")))?;
            if parsed.to_string() != *value {
                return Err(ApiError::validation(format!(
                    "{field} must use canonical UUID spelling"
                )));
            }
        }
        for (value, field) in [
            (&self.expected_updated_at, "expected_updated_at"),
            (&self.handed_off_at, "handed_off_at"),
        ] {
            if value.len() > 64 || DateTime::parse_from_rfc3339(value).is_err() {
                return Err(ApiError::validation(format!(
                    "{field} must be an RFC3339 timestamp"
                )));
            }
        }
        text(&self.warehouse_id, "warehouse_id", 100)?;
        if !self
            .warehouse_id
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
        {
            return Err(ApiError::validation(
                "warehouse_id must contain ASCII letters, digits, hyphens or underscores",
            ));
        }
        text(&self.carrier, "carrier", 240)?;
        text(&self.handoff_reference, "handoff_reference", 240)?;
        if self.lines.is_empty() || self.lines.len() > 100 {
            return Err(ApiError::validation(
                "A handoff requires 1-100 exact order lines",
            ));
        }
        let mut identities = BTreeSet::new();
        for line in &self.lines {
            text(&line.sku, "sku", 100)?;
            let hash = line.line_id.strip_prefix("oms-line-");
            if !hash.is_some_and(|h| {
                h.len() == 64
                    && h.bytes()
                        .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
            }) || !identities.insert(&line.line_id)
                || !(1..=1_000_000).contains(&line.quantity_each)
            {
                return Err(ApiError::validation("Handoff lines require unique canonical line identities and positive whole-each quantities"));
            }
        }
        Ok(())
    }
}

// Sort explicitly, even if another upstream dependency enables preserve_order.
// Strings/timestamps retain their original spelling; no float-valued fields exist.
fn canonical_json(value: &Value) -> ApiResult<String> {
    match value {
        Value::Object(object) => {
            let mut keys: Vec<_> = object.keys().collect();
            keys.sort();
            let fields = keys
                .into_iter()
                .map(|key| {
                    let key_json = serde_json::to_string(key)
                        .map_err(|_| ApiError::validation("Invalid handoff key"))?;
                    Ok(format!("{key_json}:{}", canonical_json(&object[key])?))
                })
                .collect::<ApiResult<Vec<_>>>()?;
            Ok(format!("{{{}}}", fields.join(",")))
        }
        Value::Array(array) => Ok(format!(
            "[{}]",
            array
                .iter()
                .map(canonical_json)
                .collect::<ApiResult<Vec<_>>>()?
                .join(",")
        )),
        _ => {
            serde_json::to_string(value).map_err(|_| ApiError::validation("Invalid handoff scalar"))
        }
    }
}

fn database_error(error: sqlx::Error) -> ApiError {
    let database = error.as_database_error();
    match database.and_then(|e| e.code()).as_deref() {
        Some("40001" | "23505") => {
            ApiError::conflict(database.map_or("Warehouse handoff conflicts", |e| e.message()))
        }
        Some("P0002") => ApiError::not_found("Order does not exist in this tenant"),
        Some("22023" | "22007" | "22008" | "22P02") => {
            ApiError::validation(database.map_or("Invalid warehouse handoff", |e| e.message()))
        }
        _ => ApiError::service_unavailable(
            "Warehouse handoff storage unavailable; retry the exact original request",
            Some(5),
        ),
    }
}

pub async fn capabilities(
    State(state): State<AppState>,
    req: TenantRequest<String>,
) -> ApiResult<ApiSuccess<Value>> {
    let database = state.database.as_ref().ok_or_else(|| {
        ApiError::service_unavailable("Warehouse handoff requires a database", None)
    })?;
    let capability = sqlx::query_scalar::<_, Value>(
        "SELECT warehouse_fulfillment_contract() || jsonb_build_object('tenant_id',$1::text)",
    )
    .bind(&req.tenant_id)
    .fetch_one(database.pool())
    .await
    .map_err(database_error)?;
    if capability["contract"] != CONTRACT || capability["durable_rejections"] != true {
        return Err(ApiError::service_unavailable(
            "Warehouse handoff migration is not installed",
            None,
        ));
    }
    Ok(ApiSuccess::from_ctx(req.ctx, capability))
}

pub async fn receive(
    State(state): State<AppState>,
    req: TenantRequest<String>,
    Json(body): Json<WarehouseHandoff>,
) -> ApiResult<ApiSuccess<Value>> {
    body.validate()?;
    let value = serde_json::to_value(&body)
        .map_err(|_| ApiError::validation("Invalid warehouse handoff envelope"))?;
    let digest = format!("{:x}", Sha256::digest(canonical_json(&value)?.as_bytes()));
    let database = state.database.as_ref().ok_or_else(|| {
        ApiError::service_unavailable("Warehouse handoff requires a database", None)
    })?;
    // The function's row lock, canonical source-line comparison, status/revision
    // fence, order update and immutable receipt are one database transaction.
    let answer = sqlx::query_scalar::<_, Value>("SELECT apply_warehouse_fulfillment($1,$2,$3)")
        .bind(&req.tenant_id)
        .bind(value)
        .bind(digest)
        .fetch_one(database.pool())
        .await
        .map_err(database_error)?;
    Ok(ApiSuccess::from_ctx(req.ctx, answer))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn body() -> Value {
        json!({
            "order_id":"00000000-0000-0000-0000-000000000001",
            "request_id":"00000000-0000-0000-0000-000000000004",
            "event_id":"00000000-0000-0000-0000-000000000002",
            "expected_updated_at":"2026-10-17T00:00:00Z",
            "warehouse_id":"wh-main",
            "warehouse_order_id":"00000000-0000-0000-0000-000000000003",
            "carrier":"Carrier","handoff_reference":"H-1",
            "handed_off_at":"2026-10-17T01:00:00Z",
            "lines":[{"line_id":format!("oms-line-{:x}",Sha256::digest(b"source:line-1")),"sku":"SKU-A","quantity_each":2}]
        })
    }

    #[test]
    fn rejects_unknown_fields_at_both_levels() {
        let mut value = body();
        value["netsuiteCashSaleId"] = json!("must-not-write");
        assert!(serde_json::from_value::<WarehouseHandoff>(value).is_err());
        let mut value = body();
        value["lines"][0]["quantity"] = json!(2);
        assert!(serde_json::from_value::<WarehouseHandoff>(value).is_err());
    }

    #[test]
    fn rejects_fractional_duplicate_and_invalid_line_identity() {
        let mut value = body();
        value["lines"][0]["quantity_each"] = json!(1.5);
        assert!(serde_json::from_value::<WarehouseHandoff>(value).is_err());
        let mut parsed: WarehouseHandoff = serde_json::from_value(body()).expect("fixture");
        assert!(parsed.validate().is_ok());
        parsed.lines.push(parsed.lines[0].clone());
        assert!(parsed.validate().is_err());
        parsed.lines.pop();
        parsed.lines[0].line_id = "unrelated".into();
        assert!(parsed.validate().is_err());
    }

    #[test]
    fn canonical_json_matches_compact_sorted_value_without_normalizing_timestamps() {
        let value = body();
        let parsed: WarehouseHandoff = serde_json::from_value(value.clone()).expect("fixture");
        let retained = serde_json::to_value(parsed).expect("serialize");
        assert_eq!(retained["handed_off_at"], "2026-10-17T01:00:00Z");
        assert_eq!(
            canonical_json(&retained).expect("canonical"),
            serde_json::to_string(&value).expect("value")
        );
    }

    async fn apply(pool: &sqlx::PgPool, value: &Value) -> Result<Value, sqlx::Error> {
        let digest = format!(
            "{:x}",
            Sha256::digest(canonical_json(value).expect("canonical fixture").as_bytes())
        );
        sqlx::query_scalar("SELECT apply_warehouse_fulfillment($1,$2,$3)")
            .bind("tenant-a")
            .bind(value)
            .bind(digest)
            .fetch_one(pool)
            .await
    }

    #[tokio::test]
    #[ignore = "requires isolated WMS_TEST_DATABASE_URL; companion check-upstream.py --postgres"]
    async fn postgres_races_rejections_and_explicit_review_preserve_one_physical_fact() {
        let url = std::env::var("WMS_TEST_DATABASE_URL").expect("isolated test database URL");
        let admin = sqlx::PgPool::connect(&url)
            .await
            .expect("database connection");
        let database: String = sqlx::query_scalar("SELECT current_database()")
            .fetch_one(&admin)
            .await
            .expect("database name");
        assert!(database.contains("test"), "database name must contain test");
        let schema = format!("oms_receiver_{}", Uuid::new_v4().simple());
        sqlx::query(&format!("CREATE SCHEMA {schema}"))
            .execute(&admin)
            .await
            .expect("isolated receiver schema");
        let search_path = format!("SET search_path={schema},public");
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(4)
            .after_connect(move |connection, _| {
                let search_path = search_path.clone();
                Box::pin(async move {
                    sqlx::query(&search_path).execute(connection).await?;
                    Ok(())
                })
            })
            .connect(&url)
            .await
            .expect("isolated pool");
        sqlx::raw_sql("CREATE TABLE orders(id UUID PRIMARY KEY,tenant_id TEXT NOT NULL,status TEXT NOT NULL,line_items JSONB NOT NULL,updated_at TIMESTAMPTZ NOT NULL,fulfillment_status TEXT,netsuite_id TEXT,tracking JSONB,source_order_payload JSONB)")
            .execute(&pool).await.expect("canonical source fixture");
        sqlx::raw_sql(include_str!(
            "../migrations/20261017000000_warehouse_fulfillment_receipts.sql"
        ))
        .execute(&pool)
        .await
        .expect("actual companion migration");
        for order in [1_u128, 10, 11, 12, 13] {
            sqlx::query("INSERT INTO orders(id,tenant_id,status,line_items,updated_at) VALUES($1,'tenant-a','PENDING',$2,'2026-10-17T00:00:00Z')")
                .bind(Uuid::from_u128(order))
                .bind(json!([{"shopifyLineItemId":"line-1","sku":"SKU-A","quantity":2}]))
                .execute(&pool).await.expect("source order");
        }
        let first = body();
        let mut competing = first.clone();
        competing["request_id"] = json!(Uuid::new_v4().to_string());
        competing["event_id"] = json!(Uuid::new_v4().to_string());
        let (a, b) = tokio::join!(apply(&pool, &first), apply(&pool, &competing));
        let answers = [a.expect("first response"), b.expect("competing response")];
        assert_eq!(
            answers
                .iter()
                .filter(|v| v["status"] == "fulfilled")
                .count(),
            1
        );
        assert_eq!(
            answers
                .iter()
                .filter(|v| v["status"] == "rejected" && v["reason"] == "order_already_fulfilled")
                .count(),
            1
        );
        for (request, answer) in [(&first, &answers[0]), (&competing, &answers[1])] {
            let mut expected = answer.clone();
            expected["replayed"] = json!(true);
            assert_eq!(apply(&pool, request).await.expect("exact replay"), expected);
        }
        let mut stale = body();
        stale["order_id"] = json!(Uuid::from_u128(10).to_string());
        stale["request_id"] = json!(Uuid::new_v4().to_string());
        stale["event_id"] = json!(Uuid::new_v4().to_string());
        stale["expected_updated_at"] = json!("2026-10-16T00:00:00Z");
        let rejected = apply(&pool, &stale).await.expect("durable stale rejection");
        assert_eq!(rejected["status"], "rejected");
        assert_eq!(rejected["reason"], "source_revision_changed");
        let mut reviewed = stale.clone();
        reviewed["request_id"] = json!(Uuid::new_v4().to_string());
        reviewed["expected_updated_at"] = json!("2026-10-17T00:00:00Z");
        assert_eq!(
            apply(&pool, &reviewed).await.expect("reviewed replacement")["status"],
            "fulfilled"
        );
        let mut original_rejection = rejected;
        original_rejection["replayed"] = json!(true);
        assert_eq!(
            apply(&pool, &stale).await.expect("late original rejection"),
            original_rejection
        );

        // Distinct rows can race with the same physical event; the accepted-only
        // event index prevents a second application even before either sees it.
        let mut cross_a = body();
        cross_a["order_id"] = json!(Uuid::from_u128(11).to_string());
        cross_a["request_id"] = json!(Uuid::new_v4().to_string());
        cross_a["event_id"] = json!(Uuid::new_v4().to_string());
        let mut cross_b = cross_a.clone();
        cross_b["order_id"] = json!(Uuid::from_u128(12).to_string());
        cross_b["request_id"] = json!(Uuid::new_v4().to_string());
        let (a, b) = tokio::join!(apply(&pool, &cross_a), apply(&pool, &cross_b));
        let a = match a {
            Ok(v) => v,
            Err(e) => {
                assert_eq!(
                    e.as_database_error().and_then(|e| e.code()).as_deref(),
                    Some("23505")
                );
                apply(&pool, &cross_a)
                    .await
                    .expect("same request after unique conflict")
            }
        };
        let b = match b {
            Ok(v) => v,
            Err(e) => {
                assert_eq!(
                    e.as_database_error().and_then(|e| e.code()).as_deref(),
                    Some("23505")
                );
                apply(&pool, &cross_b)
                    .await
                    .expect("same request after unique conflict")
            }
        };
        assert_eq!(
            [a, b].iter().filter(|v| v["status"] == "fulfilled").count(),
            1
        );

        // A canonical cancellation holding the row lock wins before a queued
        // handoff can inspect or mutate the same order.
        let mut cancel = pool.begin().await.expect("cancellation transaction");
        sqlx::query("UPDATE orders SET status='CANCELLED',updated_at=updated_at+interval '1 second' WHERE id=$1")
            .bind(Uuid::from_u128(13)).execute(&mut *cancel).await.expect("canonical cancellation");
        let mut queued = body();
        queued["order_id"] = json!(Uuid::from_u128(13).to_string());
        queued["request_id"] = json!(Uuid::new_v4().to_string());
        queued["event_id"] = json!(Uuid::new_v4().to_string());
        let queued_pool = pool.clone();
        let task = tokio::spawn(async move { apply(&queued_pool, &queued).await });
        tokio::task::yield_now().await;
        cancel.commit().await.expect("cancellation commit");
        let ack = task
            .await
            .expect("queued handoff task")
            .expect("queued handoff result");
        assert_eq!(ack["status"], "rejected");
        assert_eq!(ack["reason"], "source_state_not_eligible");
        let applied:i64=sqlx::query_scalar("SELECT count(*) FROM warehouse_fulfillment_receipts WHERE response->>'status'='fulfilled'").fetch_one(&pool).await.expect("accepted receipt count");
        assert_eq!(applied, 3);
        let rejected:i64=sqlx::query_scalar("SELECT count(*) FROM warehouse_fulfillment_receipts WHERE response->>'status'='rejected'").fetch_one(&pool).await.expect("rejected receipt count");
        assert_eq!(rejected, 4);
        pool.close().await;
        sqlx::query(&format!("DROP SCHEMA {schema} CASCADE"))
            .execute(&admin)
            .await
            .expect("remove isolated proof schema");
        admin.close().await;
    }
}
