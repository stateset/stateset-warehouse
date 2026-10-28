use crate::domain::{self, Allocation, Command, OrderLine, ReceiptLine, State, Stock, Warehouse};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use sqlx::{postgres::PgPoolOptions, PgConnection, PgPool, Row};
use std::collections::{HashMap, HashSet};
use std::time::Duration;

#[derive(Debug)]
pub struct Error {
    pub status: u16,
    pub message: String,
    pub command_outcome: Option<Box<CommandOutcome>>,
}
/// Definitive evidence is available only after serializing against physical
/// writes and checking the original command receipt. Other failures remain
/// ambiguous and must retain their original request for recovery.
#[derive(Debug, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum CommandOutcome {
    NotCommitted {
        reason: NotCommittedReason,
        tenant: String,
        actor: String,
        warehouse_id: String,
        request_id: String,
        expected_version: i64,
        current_version: i64,
        #[serde(skip_serializing_if = "Option::is_none")]
        expected_resource_versions: Option<std::collections::BTreeMap<String, i64>>,
        #[serde(skip_serializing_if = "Option::is_none")]
        current_resource_versions: Option<std::collections::BTreeMap<String, i64>>,
    },
}
#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum NotCommittedReason {
    StaleVersion,
    ChangedResources,
}
impl Error {
    pub fn new(status: u16, message: impl Into<String>) -> Self {
        Self {
            status,
            message: message.into(),
            command_outcome: None,
        }
    }

    fn stale_command(
        identity: &Identity,
        warehouse: &str,
        request: &Request,
        version: i64,
    ) -> Self {
        Self {
            status: 409,
            message: format!("Warehouse changed (version {version}). Refresh and review before submitting a new request."),
            command_outcome: Some(Box::new(CommandOutcome::NotCommitted {
                reason: NotCommittedReason::StaleVersion,
                tenant: identity.tenant.clone(),
                actor: identity.actor.clone(),
                warehouse_id: warehouse.into(),
                request_id: request.request_id.clone(),
                expected_version: request.expected_version,
                current_version: version,
                expected_resource_versions: None,
                current_resource_versions: None,
            })),
        }
    }
}
impl From<sqlx::Error> for Error {
    fn from(e: sqlx::Error) -> Self {
        tracing::error!(error=%e,"warehouse database operation failed");
        if e.as_database_error().and_then(|e| e.code()).as_deref() == Some("23505") {
            return Self::new(409,"A warehouse business reference or source identity already exists; review the original record");
        }
        Self::new(
            503,
            "Database operation unavailable; retry with the same request ID",
        )
    }
}
pub type Result<T> = std::result::Result<T, Error>;
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub request_id: String,
    pub expected_version: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resource_versions: Option<std::collections::BTreeMap<String, i64>>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unit_inputs: Vec<crate::units::Input>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub serial_inputs: Vec<crate::serials::Input>,
    pub command: Command,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Identity {
    pub tenant: String,
    pub actor: String,
    pub role: String,
}
#[derive(Clone)]
pub struct Store {
    pub pool: PgPool,
}
pub(crate) fn decode<T: DeserializeOwned>(v: Value) -> Result<T> {
    serde_json::from_value(v)
        .map_err(|_| Error::new(503, "Stored warehouse evidence requires repair"))
}
fn changed<T: PartialEq>(v: &T, old: Option<&T>) -> bool {
    old.is_none_or(|o| o != v)
}
fn stock_key(stock: &Stock) -> (&str, &str, &str) {
    (&stock.sku, &stock.lot, &stock.location)
}
fn target(c: Option<&Command>) -> (String, String, String) {
    match c {
        Some(Command::Receive { receipt_id, .. } | Command::CancelReceipt { receipt_id, .. }) => {
            (String::new(), receipt_id.clone(), String::new())
        }
        Some(
            Command::CreateReturn { order_id, .. }
            | Command::AllocateAvailable { order_id }
            | Command::ReleaseUnpicked { order_id, .. }
            | Command::CancelRemainder { order_id, .. }
            | Command::PackShipment { order_id, .. },
        ) => (order_id.clone(), String::new(), String::new()),
        Some(
            Command::Allocate { order_id }
            | Command::Pick { order_id, .. }
            | Command::Pack { order_id }
            | Command::Ship { order_id, .. }
            | Command::CancelOrder { order_id, .. }
            | Command::Unpick { order_id, .. }
            | Command::ResetAllocation { order_id, .. },
        ) => (order_id.clone(), String::new(), String::new()),
        Some(Command::ReportException {
            order_id: Some(id), ..
        }) => (id.clone(), String::new(), String::new()),
        Some(Command::ResolveException { exception_id, .. }) => {
            (String::new(), String::new(), exception_id.clone())
        }
        _ => Default::default(),
    }
}

impl Store {
    pub async fn list(&self, t: &str) -> Result<Vec<Value>> {
        let rows=sqlx::query("SELECT metadata,version FROM wms_warehouses WHERE tenant=$1 ORDER BY warehouse_id LIMIT 100").bind(t).fetch_all(&self.pool).await?;
        rows.iter().map(|r|Ok(json!({"warehouse":r.try_get::<Value,_>("metadata")?,"version":r.try_get::<i64,_>("version")?}))).collect()
    }
    pub async fn warehouse_page(&self, tenant: &str, after: &str, limit: i64) -> Result<Value> {
        if !(1..=100).contains(&limit) || after.len() > 240 || after.chars().any(char::is_control) {
            return Err(Error::new(
                422,
                "Warehouse pages require limit 1–100 and a valid cursor",
            ));
        }
        let mut rows = sqlx::query("SELECT metadata,version,warehouse_id FROM wms_warehouses WHERE tenant=$1 AND warehouse_id>$2 ORDER BY warehouse_id LIMIT $3")
            .bind(tenant).bind(after).bind(limit + 1).fetch_all(&self.pool).await?;
        let more = rows.len() > limit as usize;
        rows.truncate(limit as usize);
        let next = if more {
            rows.last().map(|r| r.get::<String, _>("warehouse_id"))
        } else {
            None
        };
        let items: Vec<Value> = rows.into_iter().map(|r| Ok(json!({"warehouse":r.try_get::<Value,_>("metadata")?,"version":r.try_get::<i64,_>("version")?}))).collect::<Result<_>>()?;
        Ok(json!({"warehouses":items,"next_after":next}))
    }
    pub async fn state(&self, t: &str, w: &str) -> Result<State> {
        let mut tx = self.pool.begin().await?;
        sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY")
            .execute(&mut *tx)
            .await?;
        let s = Self::load(&mut tx, t, w, None, true).await?;
        tx.commit().await?;
        Ok(s)
    }
    pub async fn snapshot(&self, t: &str, w: &str) -> Result<crate::domain::OperationalSnapshot> {
        let mut tx = self.pool.begin().await?;
        sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY")
            .execute(&mut *tx)
            .await?;
        let s = Self::load(&mut tx, t, w, None, true).await?;
        let at: String = sqlx::query_scalar(r#"SELECT to_char(clock_timestamp() AT TIME ZONE 'UTC','YYYY-MM-DD"T"HH24:MI:SS.US"Z"')"#).fetch_one(&mut *tx).await?;
        tx.commit().await?;
        let mut snapshot = s.into_snapshot();
        snapshot.availability_checked_at = Some(at);
        Ok(snapshot)
    }
    pub async fn receipt(&self, i: &Identity, w: &str, key: &str) -> Result<Value> {
        let r = sqlx::query(
            "SELECT result
            FROM wms_commands
            WHERE tenant=$1 AND warehouse_id=$2 AND request_id=$3 AND actor=$4",
        )
        .bind(&i.tenant)
        .bind(w)
        .bind(key)
        .bind(&i.actor)
        .fetch_optional(&self.pool)
        .await?;
        Ok(match r {
            Some(r) => json!({"found":true,"result":r.get::<Value,_>("result")}),
            None => json!({"found":false}),
        })
    }
    pub async fn detail(&self, t: &str, w: &str, kind: &str, id: &str) -> Result<Value> {
        let q = match kind {
            "orders" => "SELECT data FROM wms_orders WHERE tenant=$1 AND warehouse_id=$2 AND id=$3",
            "receipts" => {
                "SELECT data FROM wms_receipts WHERE tenant=$1 AND warehouse_id=$2 AND id=$3"
            }
            _ => return Err(Error::new(404, "Unknown warehouse record")),
        };
        let mut tx = self.pool.begin().await?;
        sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY")
            .execute(&mut *tx)
            .await?;
        let r = sqlx::query(q)
            .bind(t)
            .bind(w)
            .bind(id)
            .fetch_optional(&mut *tx)
            .await?
            .ok_or_else(|| Error::new(404, "Warehouse record not found"))?;
        let mut records = Self::canonical_records(&mut tx, t, w, kind, vec![r.get("data")]).await?;
        let record = records
            .pop()
            .ok_or_else(|| Error::new(503, "Canonical warehouse record unavailable"))?;
        tx.commit().await?;
        Ok(record)
    }
    pub async fn history(
        &self,
        t: &str,
        w: &str,
        kind: &str,
        before: Option<i64>,
        limit: i64,
    ) -> Result<Value> {
        let table = match kind {
            "orders" => "wms_orders",
            "receipts" => "wms_receipts",
            "movements" => "wms_movements",
            "activity" => "wms_activity",
            "exceptions" => "wms_exceptions",
            _ => return Err(Error::new(404, "Unknown history type")),
        };
        if !(1..=100).contains(&limit) || before.is_some_and(|v| v < 1) {
            return Err(Error::new(
                422,
                "History limit must be 1–100 and cursor positive",
            ));
        }
        let q=format!("SELECT seq,data FROM {table} WHERE tenant=$1 AND warehouse_id=$2 AND seq<$3 ORDER BY seq DESC LIMIT $4");
        let mut tx = self.pool.begin().await?;
        sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY")
            .execute(&mut *tx)
            .await?;
        let rows = sqlx::query(&q)
            .bind(t)
            .bind(w)
            .bind(before.unwrap_or(i64::MAX))
            .bind(limit + 1)
            .fetch_all(&mut *tx)
            .await?;
        let mut records: Vec<Value> = rows
            .iter()
            .take(limit as usize)
            .map(|r| r.get("data"))
            .collect();
        if matches!(kind, "orders" | "receipts") {
            records = Self::canonical_records(&mut tx, t, w, kind, records).await?;
        }
        let items: Vec<_> = rows
            .iter()
            .take(limit as usize)
            .zip(records)
            .map(|(r, data)| json!({"cursor":r.get::<i64,_>("seq"),"data":data}))
            .collect();
        let cursor = if rows.len() > limit as usize {
            items.last().map(|r| r["cursor"].clone())
        } else {
            None
        };
        tx.commit().await?;
        Ok(json!({"items":items,"next_cursor":cursor}))
    }
    pub async fn reconciliation(&self, t: &str, w: &str) -> Result<Value> {
        self.reconciliation_within(t, w, None).await
    }
    /// The complete proof in one read-only snapshot. `statement_timeout`
    /// replaces the connection's 10-second deadline for this transaction only;
    /// background reconciliation runs use it, request handlers do not.
    pub async fn reconciliation_within(
        &self,
        t: &str,
        w: &str,
        statement_timeout: Option<Duration>,
    ) -> Result<Value> {
        let mut tx = self.pool.begin().await?;
        sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY")
            .execute(&mut *tx)
            .await?;
        if let Some(limit) = statement_timeout {
            // An integer millisecond count; never caller text.
            sqlx::query(&format!(
                "SET LOCAL statement_timeout = '{}ms'",
                limit.as_millis()
            ))
            .execute(&mut *tx)
            .await?;
        }
        // Past roughly 5,500 serial commands the serial proof's estimated cost
        // crosses PostgreSQL's JIT optimization threshold, and compiling its
        // ~940 expressions alone took 12-16s against the 10s deadline. Below
        // that threshold JIT changed the time by no more than run-to-run noise.
        sqlx::query("SET LOCAL jit = off").execute(&mut *tx).await?;
        let s = Self::load(&mut tx, t, w, None, false).await?;
        let rows = sqlx::query(include_str!("reconciliation.sql"))
            .bind(t)
            .bind(w)
            .fetch_all(&mut *tx)
            .await?;
        let receipts=sqlx::query("SELECT r.receipt_id,r.id,r.received,COALESCE(sum(m.quantity),0)::bigint AS evidence
            FROM wms_receipt_lines r
            LEFT JOIN wms_movements m ON m.tenant=r.tenant AND m.warehouse_id=r.warehouse_id AND m.receipt_id=r.receipt_id AND m.receipt_line_id=r.id AND m.kind='receipt'
            WHERE r.tenant=$1 AND r.warehouse_id=$2
            GROUP BY r.receipt_id,r.id,r.received
            ORDER BY r.receipt_id,r.id").bind(t).bind(w).fetch_all(&mut *tx).await?;
        let proofs: Vec<_> = rows
            .iter()
            .map(|r| {
                json!({
                    "sku":r.get::<String,
                    _>("sku"),
                    "lot":r.get::<String,
                    _>("lot"),
                    "location":r.get::<String,
                    _>("location"),
                    "on_hand":r.get::<i64,
                    _>("on_hand"),
                    "ledger":r.get::<i64,
                    _>("ledger"),
                    "reserved":r.get::<i64,
                    _>("reserved"),
                    "allocation_reserved":r.get::<i64,
                    _>("allocation_reserved"),
                    "difference":r.get::<i64,
                    _>("on_hand")-r.get::<i64,
                    _>("ledger"),
                    "reserved_difference":r.get::<i64,
                    _>("reserved")-r.get::<i64,
                    _>("allocation_reserved"),
                    "ok":r.get::<i64,
                    _>("on_hand")==r.get::<i64,
                    _>("ledger")&&r.get::<i64,
                    _>("reserved")==r.get::<i64,
                    _>("allocation_reserved")
                })
            })
            .collect();
        let receipt_proofs: Vec<_> = receipts
            .iter()
            .map(|r| {
                json!({
                    "receipt_id":r.get::<String,
                    _>("receipt_id"),
                    "line_id":r.get::<String,
                    _>("id"),
                    "received":r.get::<i64,
                    _>("received"),
                    "evidence":r.get::<i64,
                    _>("evidence"),
                    "ok":r.get::<i64,
                    _>("received")==r.get::<i64,
                    _>("evidence")
                })
            })
            .collect();
        let returns = sqlx::query("SELECT r.order_id,r.order_line_id,r.sku,r.lot,sum(r.quantity-COALESCE(c.cancelled,0))::bigint AS authorized,sum(l.received)::bigint AS received,COALESCE((
            SELECT sum(s.quantity)
            FROM wms_actual_shipped(r.tenant,r.warehouse_id,r.order_id) s
            WHERE s.line_id=r.order_line_id AND s.sku=r.sku AND s.lot=r.lot),0)::bigint AS shipped,bool_and(r.sku=l.sku AND r.lot=l.lot AND r.quantity=l.quantity AND o.status IN('shipped','partially_shipped','closed_partial')) AS identity_ok
            FROM wms_return_lines r
            JOIN wms_receipt_lines l ON l.tenant=r.tenant AND l.warehouse_id=r.warehouse_id AND l.receipt_id=r.receipt_id AND l.id=r.line_id
            JOIN wms_orders o ON o.tenant=r.tenant AND o.warehouse_id=r.warehouse_id AND o.id=r.order_id
            LEFT JOIN wms_receipt_cancelled_lines c ON c.tenant=r.tenant AND c.warehouse_id=r.warehouse_id AND c.receipt_id=r.receipt_id AND c.line_id=r.line_id
            WHERE r.tenant=$1 AND r.warehouse_id=$2
            GROUP BY r.tenant,r.warehouse_id,r.order_id,r.order_line_id,r.sku,r.lot
            ORDER BY r.order_id,r.order_line_id,r.lot")
            .bind(t).bind(w).fetch_all(&mut *tx).await?;
        let return_proofs: Vec<_> = returns.iter().map(|r| {
            let shipped: i64 = r.get("shipped"); let authorized: i64 = r.get("authorized"); let received: i64 = r.get("received");
            json!({"order_id":r.get::<String,_>("order_id"),"order_line_id":r.get::<String,_>("order_line_id"),
                "sku":r.get::<String,_>("sku"),"lot":r.get::<String,_>("lot"),"shipped":shipped,"authorized":authorized,
                "received":received,"ok":r.get::<bool,_>("identity_ok") && authorized<=shipped && received<=authorized})
        }).collect();
        let cancellation_proofs = crate::receipts::proof(&mut tx, t, w).await?;
        let shipment_proofs = crate::shipments::proof(&mut tx, t, w).await?;
        let transfer_proofs = crate::transfers::proof(&mut tx, t, w).await?;
        let package_proofs = crate::split_fulfillment::proof(&mut tx, t, w).await?;
        let lot_proof = crate::lot_store::proof(&mut tx, t, w).await?;
        let unit_proof = crate::units::proof(&mut tx, t, w).await?;
        let serial_proof: Value = sqlx::query_scalar(include_str!("serial_reconciliation.sql"))
            .bind(t)
            .bind(w)
            .fetch_one(&mut *tx)
            .await?;
        let ok = serial_proof["ok"] == true
            && unit_proof["ok"] == true
            && lot_proof["ok"] == true
            && s.validate().is_ok()
            && package_proofs.iter().all(|p| p["ok"] == true)
            && transfer_proofs.iter().all(|p| p["ok"] == true)
            && shipment_proofs.iter().all(|p| p["ok"] == true)
            && cancellation_proofs.iter().all(|p| p["ok"] == true)
            && proofs.iter().all(|p| p["ok"] == true)
            && receipt_proofs.iter().all(|p| p["ok"] == true)
            && return_proofs.iter().all(|p| p["ok"] == true);
        tx.commit().await?;
        Ok(json!({
            "version":s.version,
            "ok":ok,
            "rows":proofs,
            "receipts":receipt_proofs,
            "returns":return_proofs,
            "receipt_cancellations":cancellation_proofs,
            "shipments":shipment_proofs,
            "transfers":transfer_proofs,
            "packages":package_proofs,
            "lot_controls":lot_proof,
            "units":unit_proof,
            "serials":serial_proof,
            "method":"full_append_only_ledger_and_order_line_allocations"
        }))
    }
    pub async fn outbox(&self, t: &str, w: &str) -> Result<Vec<Value>> {
        let rows=sqlx::query("SELECT o.id,o.destination,o.status,o.attempts,o.last_error,o.event_id,e.payload->>'type' AS kind,e.payload->>'occurred_at' AS occurred_at
            FROM wms_outbox o
            JOIN wms_events e ON e.id=o.event_id
            WHERE o.tenant=$1 AND e.warehouse_id=$2
            ORDER BY e.created_at DESC,o.id
            LIMIT 200").bind(t).bind(w).fetch_all(&self.pool).await?;
        rows.iter()
            .map(|r| {
                Ok(json!({
                    "id":r.try_get::<String,
                    _>("id")?,
                    "destination":r.try_get::<String,
                    _>("destination")?,
                    "status":r.try_get::<String,
                    _>("status")?,
                    "attempts":r.try_get::<i64,
                    _>("attempts")?,
                    "last_error":r.try_get::<String,
                    _>("last_error")?,
                    "event_id":r.try_get::<String,
                    _>("event_id")?,
                    "type":r.try_get::<String,
                    _>("kind")?,
                    "occurred_at":r.try_get::<String,
                    _>("occurred_at")?
                }))
            })
            .collect()
    }
    pub async fn retry(&self, i: &Identity, id: &str) -> Result<Value> {
        if i.role != "admin" {
            return Err(Error::new(
                403,
                "Only administrators may retry dead letters",
            ));
        }
        let mut tx = self.pool.begin().await?;
        let n = sqlx::query(
            "UPDATE wms_outbox
            SET status='pending',due_at=now(),attempts=0,lease_token=NULL,lease_until=NULL
            WHERE tenant=$1 AND id=$2 AND status='dead_letter'",
        )
        .bind(&i.tenant)
        .bind(id)
        .execute(&mut *tx)
        .await?
        .rows_affected();
        if n != 1 {
            return Err(Error::new(
                409,
                "Only an existing dead letter can be retried",
            ));
        }
        sqlx::query(
            "INSERT INTO wms_admin_audit(id,tenant,actor,action,reference)
            VALUES($1,$2,$3,'outbox.retry',$4)",
        )
        .bind(domain::id())
        .bind(&i.tenant)
        .bind(&i.actor)
        .bind(id)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(json!({"queued":true}))
    }
}

mod execute;
mod load;
mod save;
mod startup;
