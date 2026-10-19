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
    pub async fn connect(url: &str) -> Result<Self> {
        let pool = PgPoolOptions::new()
            .max_connections(8)
            .acquire_timeout(Duration::from_secs(5))
            .after_connect(|c, _| {
                Box::pin(async move {
                    sqlx::query("SET statement_timeout='10s'")
                        .execute(&mut *c)
                        .await?;
                    sqlx::query("SET lock_timeout='5s'")
                        .execute(&mut *c)
                        .await?;
                    Ok(())
                })
            })
            .connect(url)
            .await?;
        let mut tx = pool.begin().await?;
        sqlx::query("SELECT pg_advisory_xact_lock(84721821)")
            .execute(&mut *tx)
            .await?;
        sqlx::query("CREATE TABLE IF NOT EXISTS wms_schema_migrations(name TEXT PRIMARY KEY,digest TEXT NOT NULL,applied_at TIMESTAMPTZ NOT NULL DEFAULT now())").execute(&mut *tx).await?;
        for (name, sql) in [
            ("001_initial", include_str!("../migrations/001_initial.sql")),
            (
                "002_relational_execution",
                include_str!("../migrations/002_relational_execution.sql"),
            ),
            (
                "003_native_operations",
                include_str!("../migrations/003_native_operations.sql"),
            ),
            (
                "004_native_source_ownership",
                include_str!("../migrations/004_native_source_ownership.sql"),
            ),
            (
                "005_conditional_oms_fulfillment",
                include_str!("../migrations/005_conditional_oms_fulfillment.sql"),
            ),
            (
                "006_oms_rejections",
                include_str!("../migrations/006_oms_rejections.sql"),
            ),
            (
                "007_operations_indexes",
                include_str!("../migrations/007_operations_indexes.sql"),
            ),
            (
                "008_delivery_recipient",
                include_str!("../migrations/008_delivery_recipient.sql"),
            ),
            (
                "009_resource_versions",
                include_str!("../migrations/009_resource_versions.sql"),
            ),
            (
                "010_customer_returns",
                include_str!("../migrations/010_customer_returns.sql"),
            ),
            (
                "011_immutable_command_receipts",
                include_str!("../migrations/011_immutable_command_receipts.sql"),
            ),
            (
                "012_receipt_cancellation",
                include_str!("../migrations/012_receipt_cancellation.sql"),
            ),
            (
                "013_shipment_evidence",
                include_str!("../migrations/013_shipment_evidence.sql"),
            ),
            (
                "014_facility_transfers",
                include_str!("../migrations/014_facility_transfers.sql"),
            ),
            (
                "015_split_fulfillment",
                include_str!("../migrations/015_split_fulfillment.sql"),
            ),
            (
                "016_order_remainder_cancellation",
                include_str!("../migrations/016_order_remainder_cancellation.sql"),
            ),
            (
                "017_lot_controls",
                include_str!("../migrations/017_lot_controls.sql"),
            ),
            (
                "018_lot_trace",
                include_str!("../migrations/018_lot_trace.sql"),
            ),
            (
                "019_transfer_loss",
                include_str!("../migrations/019_transfer_loss.sql"),
            ),
            (
                "020_pack_units",
                include_str!("../migrations/020_pack_units.sql"),
            ),
            (
                "021_serial_tracking",
                include_str!("../migrations/021_serial_tracking.sql"),
            ),
            (
                "022_serial_restore_search_path",
                include_str!("../migrations/022_serial_restore_search_path.sql"),
            ),
            (
                "023_serial_target_resolution",
                include_str!("../migrations/023_serial_target_resolution.sql"),
            ),
            (
                "024_serial_event_payload_reuse",
                include_str!("../migrations/024_serial_event_payload_reuse.sql"),
            ),
            (
                "025_serial_resolved_read_sources",
                include_str!("../migrations/025_serial_resolved_read_sources.sql"),
            ),
            (
                "026_serial_event_batch_index",
                include_str!("../migrations/026_serial_event_batch_index.sql"),
            ),
            (
                "027_serial_reference_classes",
                include_str!("../migrations/027_serial_reference_classes.sql"),
            ),
            (
                "028_serial_read_source",
                include_str!("../migrations/028_serial_read_source.sql"),
            ),
            (
                "029_serial_scope_source_sets",
                include_str!("../migrations/029_serial_scope_source_sets.sql"),
            ),
        ] {
            let digest = format!("{:x}", Sha256::digest(sql.as_bytes()));
            let old: Option<String> =
                sqlx::query_scalar("SELECT digest FROM wms_schema_migrations WHERE name=$1")
                    .bind(name)
                    .fetch_optional(&mut *tx)
                    .await?;
            if let Some(old) = old {
                if old != digest {
                    return Err(Error::new(
                        503,
                        format!("Applied migration {name} changed; add a new migration instead"),
                    ));
                }
            } else {
                sqlx::raw_sql(sql).execute(&mut *tx).await?;
                sqlx::query("INSERT INTO wms_schema_migrations(name,digest) VALUES($1,$2)")
                    .bind(name)
                    .bind(digest)
                    .execute(&mut *tx)
                    .await?;
            }
        }
        let legacy=sqlx::query("SELECT tenant,state FROM wms_workspaces s WHERE NOT EXISTS(SELECT 1 FROM wms_warehouses w WHERE w.tenant=s.tenant AND w.warehouse_id=s.warehouse_id) FOR UPDATE").fetch_all(&mut *tx).await?;
        for row in legacy {
            let tenant: String = row.try_get("tenant")?;
            let mut s: State = decode(row.try_get("state")?)?;
            s.upgrade_legacy_receipt_evidence()
                .map_err(|e| Error::new(503, format!("Legacy evidence migration held: {e}")))?;
            s.validate()
                .map_err(|e| Error::new(503, format!("Legacy reconciliation failed: {e}")))?;
            Self::save(&mut tx, &tenant, None, &s).await?;
            crate::shipments::initialize(&mut tx, &tenant, &s.warehouse.id).await?;
        }
        tx.commit().await?;
        Ok(Self { pool })
    }
    pub async fn provision(&self, tenant: &str, demo: bool) -> Result<()> {
        let s = if demo {
            domain::demo_state()
        } else {
            State::empty(Warehouse {
                id: "wh-main".into(),
                code: "MAIN".into(),
                name: "Main Warehouse".into(),
                city: "Configure facility before live operations".into(),
                timezone: "UTC".into(),
            })
        };
        let mut tx = self.pool.begin().await?;
        let n=sqlx::query("INSERT INTO wms_warehouses(tenant,warehouse_id,version,metadata) VALUES($1,$2,$3,$4) ON CONFLICT DO NOTHING").bind(tenant).bind(&s.warehouse.id).bind(s.version).bind(json!(s.warehouse)).execute(&mut *tx).await?.rows_affected();
        if n == 1 {
            Self::save(&mut tx, tenant, None, &s).await?;
            crate::shipments::initialize(&mut tx, tenant, &s.warehouse.id).await?;
        }
        tx.commit().await?;
        Ok(())
    }
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
    pub(crate) async fn order_children(
        c: &mut PgConnection,
        t: &str,
        w: &str,
        orders: &mut [domain::Order],
    ) -> Result<()> {
        crate::order_cancellation::load(c, t, w, orders).await?;
        let ids: Vec<_> = orders.iter().map(|o| o.id.clone()).collect();
        let lines=sqlx::query("SELECT order_id,id,sku,quantity FROM wms_order_lines WHERE tenant=$1 AND warehouse_id=$2 AND order_id=ANY($3) ORDER BY order_id,id").bind(t).bind(w).bind(&ids).fetch_all(&mut *c).await?;
        let allocations=sqlx::query("SELECT order_id,line_id,sku,lot,location,quantity,picked,shipped,released FROM wms_allocations WHERE tenant=$1 AND warehouse_id=$2 AND order_id=ANY($3) ORDER BY order_id,line_id,location,lot").bind(t).bind(w).bind(&ids).fetch_all(&mut *c).await?;
        let mut lines_by_order: HashMap<String, Vec<OrderLine>> = HashMap::new();
        for r in lines {
            lines_by_order
                .entry(r.try_get("order_id")?)
                .or_default()
                .push(OrderLine {
                    id: r.try_get("id")?,
                    sku: r.try_get("sku")?,
                    quantity: r.try_get("quantity")?,
                });
        }
        let mut allocations_by_order: HashMap<String, Vec<Allocation>> = HashMap::new();
        for r in allocations {
            allocations_by_order
                .entry(r.try_get("order_id")?)
                .or_default()
                .push(Allocation {
                    line_id: r.try_get("line_id")?,
                    sku: r.try_get("sku")?,
                    lot: r.try_get("lot")?,
                    location: r.try_get("location")?,
                    quantity: r.try_get("quantity")?,
                    picked: r.try_get("picked")?,
                    shipped: r.try_get("shipped")?,
                    released: r.try_get("released")?,
                });
        }
        for o in orders {
            // Preserve canonical child ordering from SQL while decoding/joining
            // each row once, independent of the number of active orders.
            o.lines = lines_by_order.remove(&o.id).unwrap_or_default();
            o.allocations = allocations_by_order.remove(&o.id).unwrap_or_default();
        }
        Ok(())
    }
    pub(crate) async fn receipt_children(
        c: &mut PgConnection,
        t: &str,
        w: &str,
        receipts: &mut [domain::Receipt],
    ) -> Result<()> {
        let ids: Vec<_> = receipts.iter().map(|r| r.id.clone()).collect();
        let lines=sqlx::query("SELECT receipt_id,id,sku,lot,quantity,received FROM wms_receipt_lines WHERE tenant=$1 AND warehouse_id=$2 AND receipt_id=ANY($3) ORDER BY receipt_id,id").bind(t).bind(w).bind(&ids).fetch_all(&mut *c).await?;
        let mut lines_by_receipt: HashMap<String, Vec<ReceiptLine>> = HashMap::new();
        for l in lines {
            lines_by_receipt
                .entry(l.try_get("receipt_id")?)
                .or_default()
                .push(ReceiptLine {
                    id: l.try_get("id")?,
                    sku: l.try_get("sku")?,
                    lot: l.try_get("lot")?,
                    quantity: l.try_get("quantity")?,
                    received: l.try_get("received")?,
                });
        }
        let returns = sqlx::query("SELECT receipt_id,line_id,order_id,order_line_id,reason FROM wms_return_lines WHERE tenant=$1 AND warehouse_id=$2 AND receipt_id=ANY($3) ORDER BY receipt_id,line_id")
            .bind(t).bind(w).bind(&ids).fetch_all(&mut *c).await?;
        let mut sources: HashMap<String, crate::returns::ReturnSource> = HashMap::new();
        for row in returns {
            let receipt_id: String = row.try_get("receipt_id")?;
            let source = sources
                .entry(receipt_id)
                .or_insert(crate::returns::ReturnSource {
                    order_id: row.try_get("order_id")?,
                    reason: row.try_get("reason")?,
                    line_ids: Default::default(),
                });
            source
                .line_ids
                .insert(row.try_get("line_id")?, row.try_get("order_line_id")?);
        }
        crate::receipts::load(c, t, w, receipts).await?;
        for r in receipts {
            r.lines = lines_by_receipt.remove(&r.id).unwrap_or_default();
            r.return_source = sources.remove(&r.id);
        }
        Ok(())
    }
    async fn canonical_records(
        c: &mut PgConnection,
        t: &str,
        w: &str,
        kind: &str,
        records: Vec<Value>,
    ) -> Result<Vec<Value>> {
        if records.is_empty() {
            return Ok(records);
        }
        let headers = records
            .into_iter()
            .map(|mut value| {
                let object = value
                    .as_object_mut()
                    .ok_or_else(|| Error::new(503, "Stored warehouse header requires repair"))?;
                object.insert("lines".into(), json!([]));
                if kind == "orders" {
                    object.insert("allocations".into(), json!([]));
                } else if kind == "receipts" {
                    object.remove("return_source");
                    object.remove("cancellation");
                }
                Ok(value)
            })
            .collect::<Result<Vec<_>>>()?;
        match kind {
            "orders" => {
                let mut orders = headers
                    .into_iter()
                    .map(decode)
                    .collect::<Result<Vec<domain::Order>>>()?;
                Self::order_children(c, t, w, &mut orders).await?;
                Ok(orders.into_iter().map(|order| json!(order)).collect())
            }
            "receipts" => {
                let mut receipts = headers
                    .into_iter()
                    .map(decode)
                    .collect::<Result<Vec<domain::Receipt>>>()?;
                Self::receipt_children(c, t, w, &mut receipts).await?;
                Ok(receipts.into_iter().map(|receipt| json!(receipt)).collect())
            }
            _ => Err(Error::new(404, "Unknown warehouse record")),
        }
    }
    async fn load(
        c: &mut PgConnection,
        t: &str,
        w: &str,
        command: Option<&Command>,
        display: bool,
    ) -> Result<State> {
        let h = sqlx::query(
            "SELECT metadata,version FROM wms_warehouses WHERE tenant=$1 AND warehouse_id=$2",
        )
        .bind(t)
        .bind(w)
        .fetch_optional(&mut *c)
        .await?
        .ok_or_else(|| Error::new(404, "Warehouse not found"))?;
        let mut s = State::empty(decode(h.try_get("metadata")?)?);
        s.version = h.try_get("version")?;
        let (oid, rid, eid) = target(command);
        s.products = sqlx::query(
            "SELECT data FROM wms_products WHERE tenant=$1 AND warehouse_id=$2 ORDER BY sku",
        )
        .bind(t)
        .bind(w)
        .fetch_all(&mut *c)
        .await?
        .into_iter()
        .map(|r| decode(r.get("data")))
        .collect::<Result<_>>()?;
        crate::lot_store::load(c, t, w, &mut s).await?;
        crate::units::load(c, t, w, &mut s).await?;
        s.locations = sqlx::query(
            "SELECT data FROM wms_locations WHERE tenant=$1 AND warehouse_id=$2 ORDER BY code",
        )
        .bind(t)
        .bind(w)
        .fetch_all(&mut *c)
        .await?
        .into_iter()
        .map(|r| decode(r.get("data")))
        .collect::<Result<_>>()?;
        s.stocks=sqlx::query("SELECT sku,lot,location,on_hand,reserved FROM wms_stock WHERE tenant=$1 AND warehouse_id=$2 ORDER BY sku,lot,location").bind(t).bind(w).fetch_all(&mut *c).await?.iter().map(|r|Ok(Stock{sku:r.try_get("sku")?,lot:r.try_get("lot")?,location:r.try_get("location")?,on_hand:r.try_get("on_hand")?,reserved:r.try_get("reserved")?})).collect::<Result<_>>()?;
        s.inventory_baseline=sqlx::query("SELECT sku,lot,location,quantity FROM wms_stock_ledger WHERE tenant=$1 AND warehouse_id=$2 ORDER BY sku,lot,location").bind(t).bind(w).fetch_all(&mut *c).await?.iter().map(|r|Ok(Stock{sku:r.try_get("sku")?,lot:r.try_get("lot")?,location:r.try_get("location")?,on_hand:r.try_get("quantity")?,reserved:0})).collect::<Result<_>>()?;
        if display {
            s.movements=sqlx::query("SELECT data FROM wms_movements WHERE tenant=$1 AND warehouse_id=$2 ORDER BY seq DESC LIMIT 100").bind(t).bind(w).fetch_all(&mut *c).await?.into_iter().map(|r|decode(r.get("data"))).collect::<Result<_>>()?;
            s.movements.reverse();
            s.activity=sqlx::query("SELECT data FROM wms_activity WHERE tenant=$1 AND warehouse_id=$2 ORDER BY seq DESC LIMIT 100").bind(t).bind(w).fetch_all(&mut *c).await?.into_iter().map(|r|decode(r.get("data"))).collect::<Result<_>>()?;
            s.activity.reverse();
        }
        s.orders=sqlx::query("SELECT data || '{\"lines\":[],\"allocations\":[]}'::jsonb AS data FROM wms_orders WHERE tenant=$1 AND warehouse_id=$2 AND (status NOT IN('shipped','cancelled','closed_partial') OR id=$3 OR ($4 AND seq IN(SELECT seq FROM wms_orders WHERE tenant=$1 AND warehouse_id=$2 AND status IN('shipped','cancelled','closed_partial') ORDER BY seq DESC LIMIT 20))) ORDER BY seq").bind(t).bind(w).bind(oid).bind(display).fetch_all(&mut *c).await?.into_iter().map(|r|decode(r.get("data"))).collect::<Result<_>>()?;
        Self::order_children(c, t, w, &mut s.orders).await?;
        let movement_receipts: Vec<_> = s
            .movements
            .iter()
            .filter_map(|m| m.receipt_id.clone())
            .collect();
        s.receipts=sqlx::query("SELECT data || '{\"lines\":[],\"return_source\":null,\"cancellation\":null}'::jsonb AS data FROM wms_receipts WHERE tenant=$1 AND warehouse_id=$2 AND (status IN('expected','partial') OR id=$3 OR id=ANY($5) OR ($4 AND seq IN(SELECT seq FROM wms_receipts WHERE tenant=$1 AND warehouse_id=$2 AND status IN('received','cancelled') ORDER BY seq DESC LIMIT 20))) ORDER BY seq").bind(t).bind(w).bind(rid).bind(display).bind(movement_receipts).fetch_all(&mut *c).await?.into_iter().map(|r|decode(r.get("data"))).collect::<Result<_>>()?;
        Self::receipt_children(c, t, w, &mut s.receipts).await?;
        let ids: Vec<_> = s.receipts.iter().map(|r| r.id.clone()).collect();
        for r in &s.receipts {
            for l in &r.lines {
                s.receipt_baseline
                    .entry(r.id.clone())
                    .or_default()
                    .insert(l.id.clone(), 0);
            }
        }
        let proofs=sqlx::query("SELECT receipt_id,line_id,quantity FROM wms_receipt_evidence WHERE tenant=$1 AND warehouse_id=$2 AND receipt_id=ANY($3)").bind(t).bind(w).bind(&ids).fetch_all(&mut *c).await?;
        for p in proofs {
            s.receipt_baseline
                .entry(p.get("receipt_id"))
                .or_default()
                .insert(p.get("line_id"), p.get("quantity"));
        }
        s.exceptions=sqlx::query("SELECT data FROM wms_exceptions WHERE tenant=$1 AND warehouse_id=$2 AND (status='open' OR id=$3 OR ($4 AND seq IN(SELECT seq FROM wms_exceptions WHERE tenant=$1 AND warehouse_id=$2 AND status='resolved' ORDER BY seq DESC LIMIT 20))) ORDER BY seq").bind(t).bind(w).bind(eid).bind(display).fetch_all(&mut *c).await?.into_iter().map(|r|decode(r.get("data"))).collect::<Result<_>>()?;
        for m in &s.movements {
            for (location, delta) in [
                (m.from.as_deref(), m.quantity),
                (m.to.as_deref(), -m.quantity),
            ] {
                if let Some(location) = location {
                    if let Some(b) = s
                        .inventory_baseline
                        .iter_mut()
                        .find(|b| b.sku == m.sku && b.lot == m.lot && b.location == location)
                    {
                        b.on_hand += delta;
                    } else {
                        s.inventory_baseline.push(Stock {
                            sku: m.sku.clone(),
                            lot: m.lot.clone(),
                            location: location.into(),
                            on_hand: delta,
                            reserved: 0,
                        });
                    }
                }
            }
            if let (Some(rid), Some(lid)) = (&m.receipt_id, &m.receipt_line_id) {
                *s.receipt_baseline
                    .entry(rid.clone())
                    .or_default()
                    .entry(lid.clone())
                    .or_default() -= m.quantity;
            }
        }
        let mut keys = crate::concurrency::visible_keys(&s);
        if let Some(command) = command {
            if let Ok(Some(versions)) = crate::concurrency::reviewed_versions(&s, command) {
                keys.extend(versions.into_keys());
            }
        }
        s.resource_versions = keys.iter().map(|key| (key.clone(), 0)).collect();
        let versions = sqlx::query("SELECT resource,revision FROM wms_resource_versions WHERE tenant=$1 AND warehouse_id=$2 AND resource=ANY($3)")
            .bind(t).bind(w).bind(&keys).fetch_all(&mut *c).await?;
        for row in versions {
            s.resource_versions
                .insert(row.try_get("resource")?, row.try_get("revision")?);
        }
        Ok(s)
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
    async fn save(c: &mut PgConnection, t: &str, old: Option<&State>, s: &State) -> Result<()> {
        let w = &s.warehouse.id;
        // Build identity indexes once. Typed equality avoids serializing every
        // unchanged operational row just to discover that nothing changed.
        let previous_products: HashMap<_, _> = old
            .into_iter()
            .flat_map(|s| s.products.iter())
            .map(|p| (p.sku.as_str(), p))
            .collect();
        let previous_locations: HashMap<_, _> = old
            .into_iter()
            .flat_map(|s| s.locations.iter())
            .map(|l| (l.code.as_str(), l))
            .collect();
        let previous_stocks: HashMap<_, _> = old
            .into_iter()
            .flat_map(|s| s.stocks.iter())
            .map(|b| (stock_key(b), b))
            .collect();
        let previous_receipts: HashMap<_, _> = old
            .into_iter()
            .flat_map(|s| s.receipts.iter())
            .map(|r| (r.id.as_str(), r))
            .collect();
        let previous_orders: HashMap<_, _> = old
            .into_iter()
            .flat_map(|s| s.orders.iter())
            .map(|o| (o.id.as_str(), o))
            .collect();
        let previous_exceptions: HashMap<_, _> = old
            .into_iter()
            .flat_map(|s| s.exceptions.iter())
            .map(|e| (e.id.as_str(), e))
            .collect();
        let previous_movements: HashSet<_> = old
            .into_iter()
            .flat_map(|s| s.movements.iter())
            .map(|m| m.id.as_str())
            .collect();
        let previous_activity: HashSet<_> = old
            .into_iter()
            .flat_map(|s| s.activity.iter())
            .map(|a| a.id.as_str())
            .collect();
        sqlx::query("INSERT INTO wms_warehouses(tenant,warehouse_id,version,metadata) VALUES($1,$2,$3,$4) ON CONFLICT(tenant,warehouse_id) DO UPDATE SET version=EXCLUDED.version,metadata=EXCLUDED.metadata").bind(t).bind(w).bind(s.version).bind(json!(s.warehouse)).execute(&mut *c).await?;
        for p in &s.products {
            if changed(p, previous_products.get(p.sku.as_str()).copied()) {
                sqlx::query("INSERT INTO wms_products VALUES($1,$2,$3,$4) ON CONFLICT(tenant,warehouse_id,sku) DO UPDATE SET data=EXCLUDED.data").bind(t).bind(w).bind(&p.sku).bind(json!(p)).execute(&mut *c).await?;
            }
        }
        for l in &s.locations {
            if changed(l, previous_locations.get(l.code.as_str()).copied()) {
                sqlx::query("INSERT INTO wms_locations VALUES($1,$2,$3,$4,$5) ON CONFLICT(tenant,warehouse_id,code) DO UPDATE SET kind=EXCLUDED.kind,data=EXCLUDED.data").bind(t).bind(w).bind(&l.code).bind(&l.kind).bind(json!(l)).execute(&mut *c).await?;
            }
        }
        for b in &s.stocks {
            if changed(b, previous_stocks.get(&stock_key(b)).copied()) {
                sqlx::query("INSERT INTO wms_stock VALUES($1,$2,$3,$4,$5,$6,$7) ON CONFLICT(tenant,warehouse_id,sku,lot,location) DO UPDATE SET on_hand=EXCLUDED.on_hand,reserved=EXCLUDED.reserved").bind(t).bind(w).bind(&b.sku).bind(&b.lot).bind(&b.location).bind(b.on_hand).bind(b.reserved).execute(&mut *c).await?;
            }
        }
        for r in &s.receipts {
            if changed(r, previous_receipts.get(r.id.as_str()).copied()) {
                sqlx::query("INSERT INTO wms_receipts(tenant,warehouse_id,id,reference,status,data) VALUES($1,$2,$3,$4,$5,$6) ON CONFLICT(tenant,warehouse_id,id) DO UPDATE SET status=EXCLUDED.status,data=EXCLUDED.data").bind(t).bind(w).bind(&r.id).bind(&r.reference).bind(&r.status).bind(json!(r)).execute(&mut *c).await?;
                for l in &r.lines {
                    sqlx::query("INSERT INTO wms_receipt_lines VALUES($1,$2,$3,$4,$5,$6,$7,$8) ON CONFLICT(tenant,warehouse_id,receipt_id,id) DO UPDATE SET received=EXCLUDED.received").bind(t).bind(w).bind(&r.id).bind(&l.id).bind(&l.sku).bind(&l.lot).bind(l.quantity).bind(l.received).execute(&mut *c).await?;
                    if let Some(source) = &r.return_source {
                        if !previous_receipts.contains_key(r.id.as_str()) {
                            sqlx::query("INSERT INTO wms_return_lines(tenant,warehouse_id,receipt_id,line_id,order_id,order_line_id,sku,lot,quantity,reason) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10)")
                                .bind(t).bind(w).bind(&r.id).bind(&l.id).bind(&source.order_id)
                                .bind(source.line_ids.get(&l.id).ok_or_else(|| Error::new(422, "Missing original return line"))?)
                                .bind(&l.sku).bind(&l.lot).bind(l.quantity).bind(&source.reason).execute(&mut *c).await?;
                        }
                    }
                }
            }
        }
        for r in &s.receipts {
            if r.cancellation.is_some()
                && previous_receipts
                    .get(r.id.as_str())
                    .is_none_or(|old| old.cancellation.is_none())
            {
                crate::receipts::save(c, t, w, r).await?;
            }
        }
        for o in &s.orders {
            if changed(o, previous_orders.get(o.id.as_str()).copied()) {
                sqlx::query("INSERT INTO wms_orders(tenant,warehouse_id,id,reference,source_order_id,status,data) VALUES($1,$2,$3,$4,$5,$6,$7) ON CONFLICT(tenant,warehouse_id,id) DO UPDATE SET status=EXCLUDED.status,data=EXCLUDED.data").bind(t).bind(w).bind(&o.id).bind(&o.reference).bind(&o.source_order_id).bind(&o.status).bind(json!(o)).execute(&mut *c).await?;
                for l in &o.lines {
                    sqlx::query("INSERT INTO wms_order_lines VALUES($1,$2,$3,$4,$5,$6) ON CONFLICT DO NOTHING").bind(t).bind(w).bind(&o.id).bind(&l.id).bind(&l.sku).bind(l.quantity).execute(&mut *c).await?;
                }
                if let Some(previous) = previous_orders.get(o.id.as_str()) {
                    for a in &previous.allocations {
                        if !o.allocations.iter().any(|n| {
                            n.line_id == a.line_id && n.location == a.location && n.lot == a.lot
                        }) {
                            sqlx::query("DELETE FROM wms_allocations WHERE tenant=$1 AND warehouse_id=$2 AND order_id=$3 AND line_id=$4 AND location=$5 AND lot=$6").bind(t).bind(w).bind(&o.id).bind(&a.line_id).bind(&a.location).bind(&a.lot).execute(&mut *c).await?;
                        }
                    }
                }
                for a in &o.allocations {
                    sqlx::query(
                        "INSERT INTO wms_allocations VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11) ON CONFLICT(tenant,warehouse_id,order_id,line_id,location,lot) DO UPDATE SET quantity=EXCLUDED.quantity,picked=EXCLUDED.picked,shipped=EXCLUDED.shipped,released=EXCLUDED.released",
                    )
                    .bind(t)
                    .bind(w)
                    .bind(&o.id)
                    .bind(&a.line_id)
                    .bind(&a.sku)
                    .bind(&a.lot)
                    .bind(&a.location)
                    .bind(a.quantity)
                    .bind(a.picked)
                    .bind(a.shipped)
                    .bind(a.released)
                    .execute(&mut *c)
                    .await?;
                }
            }
        }
        for e in &s.exceptions {
            if changed(e, previous_exceptions.get(e.id.as_str()).copied()) {
                sqlx::query("INSERT INTO wms_exceptions(tenant,warehouse_id,id,status,order_id,data) VALUES($1,$2,$3,$4,$5,$6) ON CONFLICT(tenant,warehouse_id,id) DO UPDATE SET status=EXCLUDED.status,data=EXCLUDED.data").bind(t).bind(w).bind(&e.id).bind(&e.status).bind(&e.order_id).bind(json!(e)).execute(&mut *c).await?;
            }
        }
        for m in &s.movements {
            if !previous_movements.contains(m.id.as_str()) {
                sqlx::query("INSERT INTO wms_movements(id,tenant,warehouse_id,sku,lot,from_location,to_location,quantity,kind,reference,receipt_id,receipt_line_id,data,transfer_id,transfer_line_id,transfer_event_id,package_id,order_line_id,allocation_location,shipment_event_id) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17,$18,$19,$20)").bind(&m.id).bind(t).bind(w).bind(&m.sku).bind(&m.lot).bind(&m.from).bind(&m.to).bind(m.quantity).bind(&m.kind).bind(&m.reference).bind(&m.receipt_id).bind(&m.receipt_line_id).bind(json!(m)).bind(&m.transfer_id).bind(&m.transfer_line_id).bind(&m.transfer_event_id).bind(&m.package_id).bind(&m.order_line_id).bind(&m.allocation_location).bind(&m.shipment_event_id).execute(&mut *c).await?;
            }
        }
        for a in &s.activity {
            if !previous_activity.contains(a.id.as_str()) {
                sqlx::query("INSERT INTO wms_activity(id,tenant,warehouse_id,kind,data) VALUES($1,$2,$3,$4,$5)").bind(&a.id).bind(t).bind(w).bind(&a.kind).bind(json!(a)).execute(&mut *c).await?;
            }
        }
        Ok(())
    }
    pub async fn execute(&self, i: &Identity, w: &str, r: &Request) -> Result<Value> {
        if !r.command.allowed(&i.role) {
            return Err(Error::new(403, "Your role does not permit this action"));
        }
        if !(8..=100).contains(&r.request_id.len())
            || r.request_id
                .chars()
                .any(|c| !c.is_ascii_alphanumeric() && c != '-' && c != '_')
        {
            return Err(Error::new(
                422,
                "Request ID must contain 8–100 letters, digits, hyphens or underscores",
            ));
        }
        let digest = format!(
            "{:x}",
            Sha256::digest(serde_json::to_vec(r).map_err(|_| Error::new(422, "Invalid command"))?)
        );
        // Immutable committed receipts need no warehouse write lock. Recheck
        // under the lock below when absent, so racing first submissions still
        // have one physical effect. Authorization always precedes both lookups.
        if let Some(receipt) = sqlx::query("SELECT c.actor,c.digest,c.result FROM wms_commands c JOIN wms_warehouses w ON w.tenant=c.tenant AND w.warehouse_id=c.warehouse_id WHERE c.tenant=$1 AND c.warehouse_id=$2 AND c.request_id=$3")
            .bind(&i.tenant).bind(w).bind(&r.request_id).fetch_optional(&self.pool).await? {
            if receipt.get::<String,_>("digest") != digest || receipt.get::<String,_>("actor") != i.actor {
                return Err(Error::new(409,"Request ID already belongs to a different payload or operator"));
            }
            return Ok(json!({"replayed":true,"result":receipt.get::<Value,_>("result")}));
        }
        let mut tx = self.pool.begin().await?;
        crate::transfers::lock_destination(&mut tx, &i.tenant, w, &r.command).await?;
        let mut created = None;
        if let Command::CreateWarehouse {
            code,
            name,
            city,
            timezone,
        } = &r.command
        {
            if w.is_empty()
                || w.len() > 80
                || !w.as_bytes()[0].is_ascii_alphanumeric()
                || w.bytes()
                    .any(|c| !c.is_ascii_alphanumeric() && c != b'-' && c != b'_')
                || r.expected_version != 0
                || r.resource_versions.is_some()
            {
                return Err(Error::new(422, "New warehouses require a 1–80 character letter/digit/hyphen/underscore ID, version zero and no resource map"));
            }
            let state = State::empty(Warehouse {
                id: w.into(),
                code: code.clone(),
                name: name.clone(),
                city: city.clone(),
                timezone: timezone.clone(),
            });
            // Check metadata before admission. Invalid work leaves no header,
            // default bins, command receipt or delivery intent behind.
            state
                .execute(&r.command, &i.actor)
                .map_err(|e| Error::new(422, e))?;
            let inserted = sqlx::query("INSERT INTO wms_warehouses(tenant,warehouse_id,version,metadata) VALUES($1,$2,0,$3) ON CONFLICT DO NOTHING")
                .bind(&i.tenant).bind(w).bind(json!(state.warehouse)).execute(&mut *tx).await?.rows_affected();
            if inserted == 1 {
                created = Some(state);
            }
        }
        let h = sqlx::query(
            "SELECT version FROM wms_warehouses WHERE tenant=$1 AND warehouse_id=$2 FOR UPDATE",
        )
        .bind(&i.tenant)
        .bind(w)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(|| Error::new(404, "Warehouse not found"))?;
        if let Some(receipt)=sqlx::query("SELECT actor,digest,result FROM wms_commands WHERE tenant=$1 AND warehouse_id=$2 AND request_id=$3").bind(&i.tenant).bind(w).bind(&r.request_id).fetch_optional(&mut *tx).await?{if receipt.get::<String,_>("digest")!=digest||receipt.get::<String,_>("actor")!=i.actor{return Err(Error::new(409,"Request ID already belongs to a different payload or operator"));}return Ok(json!({"replayed":true,"result":receipt.get::<Value,_>("result")}));}
        let version: i64 = h.try_get("version")?;
        if r.resource_versions.is_none() && r.expected_version < version {
            // The lock also fences a delayed copy of this exact request: the
            // warehouse version only increases, so it can never commit later.
            return Err(Error::stale_command(i, w, r, version));
        }
        if r.expected_version < 0
            || r.expected_version > version
            || (r.resource_versions.is_none() && r.expected_version != version)
        {
            return Err(Error::new(409,format!("Warehouse changed (version {version}). Refresh and review before submitting a new request.")));
        }
        if matches!(r.command, Command::CreateWarehouse { .. }) && created.is_none() {
            return Err(Error::new(
                409,
                "Warehouse ID already exists; choose a new facility identity",
            ));
        }
        if let Some(expected) = &r.resource_versions {
            if expected.is_empty() || expected.len() > 2000 || expected.values().any(|v| *v < 0) {
                return Err(Error::new(422, "Invalid resource review"));
            }
            let keys: Vec<_> = expected.keys().cloned().collect();
            let mut actual: std::collections::BTreeMap<_, _> =
                keys.iter().map(|k| (k.clone(), 0_i64)).collect();
            let rows = sqlx::query("SELECT resource,revision FROM wms_resource_versions WHERE tenant=$1 AND warehouse_id=$2 AND resource=ANY($3)")
                .bind(&i.tenant).bind(w).bind(&keys).fetch_all(&mut *tx).await?;
            for row in rows {
                actual.insert(
                    row.try_get::<String, _>("resource")?,
                    row.try_get::<i64, _>("revision")?,
                );
            }
            if *expected != actual {
                let mut error = Error::new(
                    409,
                    "Reviewed stock or work changed. Refresh and review before saving.",
                );
                if expected.iter().all(|(key, value)| actual[key] >= *value) {
                    error.command_outcome = Some(Box::new(CommandOutcome::NotCommitted {
                        reason: NotCommittedReason::ChangedResources,
                        tenant: i.tenant.clone(),
                        actor: i.actor.clone(),
                        warehouse_id: w.into(),
                        request_id: r.request_id.clone(),
                        expected_version: r.expected_version,
                        current_version: version,
                        expected_resource_versions: Some(expected.clone()),
                        current_resource_versions: Some(actual),
                    }));
                }
                return Err(error);
            }
        }
        let package_order =
            crate::split_fulfillment::order_id(&mut tx, &i.tenant, w, &r.command).await?;
        let load_command = package_order.as_ref().map(|order_id| Command::Pack {
            order_id: order_id.clone(),
        });
        let selective = if r.resource_versions.is_some() {
            crate::command_state::load(&mut tx, &i.tenant, w, &r.command).await?
        } else {
            None
        };
        let is_new = created.is_some();
        let mut state = match (created, selective) {
            (Some(state), _) => state,
            (_, Some(state)) => state,
            (_, None) => {
                Self::load(
                    &mut tx,
                    &i.tenant,
                    w,
                    Some(load_command.as_ref().unwrap_or(&r.command)),
                    false,
                )
                .await?
            }
        };
        crate::lot_store::lock(&mut tx, &i.tenant, &state, &r.command).await?;
        // Re-read after cross-facility policy locks; an arrival must observe the
        // source policy that won admission, including an initially absent row.
        crate::lot_store::load(&mut tx, &i.tenant, w, &mut state).await?;
        let shipment_order_id = package_order.or_else(|| match &r.command {
            Command::AllocateAvailable { order_id }
            | Command::ReleaseUnpicked { order_id, .. }
            | Command::CancelRemainder { order_id, .. }
            | Command::PackShipment { order_id, .. }
            | Command::Pick { order_id, .. }
            | Command::Unpick { order_id, .. }
            | Command::CancelOrder { order_id, .. }
            | Command::Allocate { order_id }
            | Command::Pack { order_id }
            | Command::Ship { order_id, .. }
            | Command::ResetAllocation { order_id, .. } => Some(order_id.clone()),
            _ => None,
        });
        if let Some(order_id) = &shipment_order_id {
            if let Some(order) = state.orders.iter().find(|o| o.id == *order_id) {
                let context = crate::split_fulfillment::load(&mut tx, &i.tenant, w, order).await?;
                if !context.ok {
                    return Err(Error::new(
                        409,
                        "Shipment packing or handoff ownership disagrees; writes held",
                    ));
                }
                state.shipment_context = Some(context);
            }
        }
        if let Some(expected) = &r.resource_versions {
            let required = crate::concurrency::reviewed_versions(&state, &r.command)
                .map_err(|message| Error::new(422, message))?
                .ok_or_else(|| Error::new(422, "This command requires a warehouse version"))?;
            if expected != &required {
                return Err(Error::new(
                    422,
                    "Review must include exactly this command's resources",
                ));
            }
        }
        match &r.command {
            Command::CreateOrder {
                reference,
                source_order_id,
                ..
            } => {
                let duplicate:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM wms_orders WHERE tenant=$1 AND warehouse_id=$2 AND (reference=$3 OR ($4::text IS NOT NULL AND source_order_id=$4)))").bind(&i.tenant).bind(w).bind(reference).bind(source_order_id).fetch_one(&mut *tx).await?;
                if duplicate {
                    return Err(Error::new(
                        409,
                        "Order reference or OMS source already exists, including completed work",
                    ));
                }
            }
            Command::CreateReceipt { reference, .. } | Command::CreateReturn { reference, .. } => {
                let duplicate:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM wms_receipts WHERE tenant=$1 AND warehouse_id=$2 AND reference=$3)").bind(&i.tenant).bind(w).bind(reference).fetch_one(&mut *tx).await?;
                if duplicate {
                    return Err(Error::new(
                        409,
                        "Receipt reference already exists, including completed receipts",
                    ));
                }
            }
            _ => {}
        }
        if let Command::CreateReturn {
            order_id, lines, ..
        } = &r.command
        {
            let available = crate::returns::availability(&mut tx, &i.tenant, w, order_id).await?;
            let mut requested = std::collections::BTreeMap::<(&str, &str), i64>::new();
            for line in lines {
                if !(1..=1_000_000).contains(&line.quantity) {
                    return Err(Error::new(422, "Invalid return quantity"));
                }
                *requested
                    .entry((&line.order_line_id, &line.lot))
                    .or_default() += line.quantity;
            }
            for ((line, lot), quantity) in requested {
                let remaining = available
                    .iter()
                    .find(|a| a["order_line_id"] == line && a["lot"] == lot)
                    .and_then(|a| a["remaining"].as_i64())
                    .unwrap_or(0);
                if quantity > remaining {
                    return Err(Error::new(
                        422,
                        "Return exceeds the original line/lot's remaining shipped quantity",
                    ));
                }
            }
        }
        state.transfer_context =
            crate::transfers::reviewed_transfer(&mut tx, &i.tenant, w, &r.command).await?;
        crate::units::admit(&mut tx, &i.tenant, w, r).await?;
        let serials = crate::serial_store::admit(&mut tx, &i.tenant, w, r).await?;
        if i.role == "oms" {
            if let Command::CancelOrder { order_id, .. }
            | Command::CancelRemainder { order_id, .. } = &r.command
            {
                if !state
                    .orders
                    .iter()
                    .any(|o| o.id == *order_id && o.source_order_id.is_some())
                {
                    return Err(Error::new(403, "OMS can cancel only source-linked orders"));
                }
            }
        }
        crate::lot_store::guard_order(
            &mut tx,
            &i.tenant,
            w,
            shipment_order_id.as_deref(),
            &r.command,
        )
        .await?;
        state.validate().map_err(|e| {
            Error::new(
                503,
                format!("Warehouse reconciliation failed; writes held: {e}"),
            )
        })?;
        let admitted_at: String = sqlx::query_scalar(r#"SELECT to_char(clock_timestamp() AT TIME ZONE 'UTC','YYYY-MM-DD"T"HH24:MI:SS.US"Z"')"#)
            .fetch_one(&mut *tx).await?;
        let admitted_at = chrono::DateTime::parse_from_rfc3339(&admitted_at)
            .map_err(|_| Error::new(503, "Database admission clock unavailable"))?
            .with_timezone(&chrono::Utc);
        let (mut next, result) = state
            .execute_at(&r.command, &i.actor, admitted_at)
            .map_err(|e| Error::new(422, e))?;
        crate::lot_store::prepare_arrival(&mut tx, &i.tenant, &mut next, &r.command, &i.actor)
            .await?;
        let eid = domain::id();
        let serial_changes = serials.plan(
            &state,
            &next,
            r,
            result["entity_id"].as_str().unwrap_or_default(),
            &eid,
        )?;
        crate::serial_store::create_mode(&mut tx, &i.tenant, w, &r.command, &eid).await?;
        for movement in &mut next.movements {
            if movement.package_id.is_some() && movement.shipment_event_id.is_none() {
                movement.shipment_event_id = Some(eid.clone());
            }
            if movement.transfer_id.is_some() && movement.transfer_event_id.is_none() {
                movement.transfer_event_id = Some(eid.clone());
            }
        }
        // Forward SQL guards record the upcoming immutable event version, even
        // when package facts are inserted before the operational projection.
        sqlx::query("UPDATE wms_warehouses SET version=$3 WHERE tenant=$1 AND warehouse_id=$2")
            .bind(&i.tenant)
            .bind(w)
            .bind(next.version)
            .execute(&mut *tx)
            .await?;
        crate::lot_store::persist(&mut tx, &i.tenant, &next, &eid).await?;
        crate::split_fulfillment::persist(&mut tx, &i.tenant, &state, &next, &eid).await?;
        crate::transfers::persist(
            &mut tx,
            i,
            w,
            &r.command,
            &next,
            result["entity_id"].as_str().unwrap_or_default(),
            &eid,
        )
        .await?;
        crate::order_cancellation::persist(&mut tx, &i.tenant, w, &state, &next, &eid).await?;
        Self::save(
            &mut tx,
            &i.tenant,
            if is_new { None } else { Some(&state) },
            &next,
        )
        .await?;
        crate::units::persist(&mut tx, &i.tenant, w, r, &next, &eid).await?;
        let serial_evidence = serials
            .persist(&mut tx, i, &next, r, &serial_changes, &eid)
            .await?;
        let order = next.orders.iter().find(|o| {
            shipment_order_id.as_deref() == Some(o.id.as_str()) || result["entity_id"] == o.id
        });
        let previous_stocks: HashMap<_, _> =
            state.stocks.iter().map(|b| (stock_key(b), b)).collect();
        let inventory: Vec<_> = next
            .stocks
            .iter()
            .filter(|b| changed(*b, previous_stocks.get(&stock_key(b)).copied()))
            .map(|b| json!({"stock":b,"version":next.version}))
            .collect();
        let mut event = json!({"schema_version":2,"id":eid,"tenant":i.tenant,"warehouse_id":w,"version":next.version,"type":r.command.kind(),"actor":i.actor,"occurred_at":next.quality_time().to_rfc3339(),"request_id":r.request_id,"result":result,"order":order,"inventory_changes":inventory,"command":r.command});
        event["previous_order"] =
            json!(order.and_then(|o| state.orders.iter().find(|old| old.id == o.id)));
        event["lot_changes"] = json!(next.lot_changes);
        if let Some(fields) = serial_evidence.as_object() {
            for (key, value) in fields {
                event[key] = value.clone();
            }
        }
        if !r.unit_inputs.is_empty() {
            event["unit_inputs"] = json!(r.unit_inputs);
        }
        if next.unit_pack_change.is_some() {
            event["unit_pack_change"]=sqlx::query_scalar("SELECT to_jsonb(p)-'tenant' FROM wms_unit_pack_changes p WHERE tenant=$1 AND warehouse_id=$2 AND source_event_id=$3")
                .bind(&i.tenant).bind(w).bind(&eid).fetch_one(&mut *tx).await?;
        }
        if matches!(
            r.command,
            Command::RecordTransferLoss { .. } | Command::ReverseTransferLoss { .. }
        ) {
            event["transfer_loss_record"]=sqlx::query_scalar("SELECT to_jsonb(r)-'tenant' FROM wms_transfer_loss_records r WHERE tenant=$1 AND warehouse_id=$2 AND id=$3")
                .bind(&i.tenant).bind(w).bind(result["entity_id"].as_str().unwrap_or_default()).fetch_one(&mut *tx).await?;
        }
        event["lot_decisions"] =
            crate::lot_store::decisions(&mut tx, &i.tenant, w, next.version).await?;
        if matches!(
            r.command,
            Command::DispatchTransfer { .. } | Command::ReceiveTransfer { .. }
        ) {
            event["transfer"] = sqlx::query_scalar("SELECT wms_transfer_document($1,$2)")
                .bind(&i.tenant)
                .bind(result["entity_id"].as_str().unwrap_or_default())
                .fetch_one(&mut *tx)
                .await?;
        }
        if matches!(
            r.command,
            Command::PackShipment { .. }
                | Command::UnpackShipment { .. }
                | Command::HandoffShipment { .. }
        ) {
            event["shipment"] = sqlx::query_scalar("SELECT wms_package_document($1,$2,$3)")
                .bind(&i.tenant)
                .bind(w)
                .bind(result["entity_id"].as_str().unwrap_or_default())
                .fetch_one(&mut *tx)
                .await?;
        }
        if matches!(
            r.command,
            Command::CancelReceipt { .. } | Command::CreateReturn { .. }
        ) {
            event["receipt"] = json!(next
                .receipts
                .iter()
                .find(|receipt| result["entity_id"] == receipt.id));
        }
        sqlx::query(
            "INSERT INTO wms_events(id,tenant,warehouse_id,version,payload) VALUES($1,$2,$3,$4,$5)",
        )
        .bind(&eid)
        .bind(&i.tenant)
        .bind(w)
        .bind(next.version)
        .bind(event)
        .execute(&mut *tx)
        .await?;
        sqlx::query("INSERT INTO wms_commands(tenant,warehouse_id,request_id,actor,digest,result) VALUES($1,$2,$3,$4,$5,$6)").bind(&i.tenant).bind(w).bind(&r.request_id).bind(&i.actor).bind(digest).bind(&result).execute(&mut *tx).await?;
        let destinations = if matches!(
            r.command,
            Command::Pack { .. } | Command::Ship { .. } | Command::HandoffShipment { .. }
        ) {
            vec!["oms", "tms"]
        } else {
            vec!["oms"]
        };
        for d in destinations {
            sqlx::query(
                "INSERT INTO wms_outbox(id,event_id,tenant,destination) VALUES($1,$2,$3,$4)",
            )
            .bind(domain::id())
            .bind(&eid)
            .bind(&i.tenant)
            .bind(d)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(json!({"replayed":false,"result":result}))
    }
    pub async fn receipt(&self, i: &Identity, w: &str, key: &str) -> Result<Value> {
        let r=sqlx::query("SELECT result FROM wms_commands WHERE tenant=$1 AND warehouse_id=$2 AND request_id=$3 AND actor=$4").bind(&i.tenant).bind(w).bind(key).bind(&i.actor).fetch_optional(&self.pool).await?;
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
        let mut tx = self.pool.begin().await?;
        sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY")
            .execute(&mut *tx)
            .await?;
        let s = Self::load(&mut tx, t, w, None, false).await?;
        let rows = sqlx::query(include_str!("reconciliation.sql"))
            .bind(t)
            .bind(w)
            .fetch_all(&mut *tx)
            .await?;
        let receipts=sqlx::query("SELECT r.receipt_id,r.id,r.received,COALESCE(sum(m.quantity),0)::bigint AS evidence FROM wms_receipt_lines r LEFT JOIN wms_movements m ON m.tenant=r.tenant AND m.warehouse_id=r.warehouse_id AND m.receipt_id=r.receipt_id AND m.receipt_line_id=r.id AND m.kind='receipt' WHERE r.tenant=$1 AND r.warehouse_id=$2 GROUP BY r.receipt_id,r.id,r.received ORDER BY r.receipt_id,r.id").bind(t).bind(w).fetch_all(&mut *tx).await?;
        let proofs:Vec<_>=rows.iter().map(|r|json!({"sku":r.get::<String,_>("sku"),"lot":r.get::<String,_>("lot"),"location":r.get::<String,_>("location"),"on_hand":r.get::<i64,_>("on_hand"),"ledger":r.get::<i64,_>("ledger"),"reserved":r.get::<i64,_>("reserved"),"allocation_reserved":r.get::<i64,_>("allocation_reserved"),"difference":r.get::<i64,_>("on_hand")-r.get::<i64,_>("ledger"),"reserved_difference":r.get::<i64,_>("reserved")-r.get::<i64,_>("allocation_reserved"),"ok":r.get::<i64,_>("on_hand")==r.get::<i64,_>("ledger")&&r.get::<i64,_>("reserved")==r.get::<i64,_>("allocation_reserved")})).collect();
        let receipt_proofs:Vec<_>=receipts.iter().map(|r|json!({"receipt_id":r.get::<String,_>("receipt_id"),"line_id":r.get::<String,_>("id"),"received":r.get::<i64,_>("received"),"evidence":r.get::<i64,_>("evidence"),"ok":r.get::<i64,_>("received")==r.get::<i64,_>("evidence")})).collect();
        let returns = sqlx::query("SELECT r.order_id,r.order_line_id,r.sku,r.lot,sum(r.quantity-COALESCE(c.cancelled,0))::bigint AS authorized,sum(l.received)::bigint AS received,COALESCE((SELECT sum(s.quantity) FROM wms_actual_shipped(r.tenant,r.warehouse_id,r.order_id) s WHERE s.line_id=r.order_line_id AND s.sku=r.sku AND s.lot=r.lot),0)::bigint AS shipped,bool_and(r.sku=l.sku AND r.lot=l.lot AND r.quantity=l.quantity AND o.status IN('shipped','partially_shipped','closed_partial')) AS identity_ok FROM wms_return_lines r JOIN wms_receipt_lines l ON l.tenant=r.tenant AND l.warehouse_id=r.warehouse_id AND l.receipt_id=r.receipt_id AND l.id=r.line_id JOIN wms_orders o ON o.tenant=r.tenant AND o.warehouse_id=r.warehouse_id AND o.id=r.order_id LEFT JOIN wms_receipt_cancelled_lines c ON c.tenant=r.tenant AND c.warehouse_id=r.warehouse_id AND c.receipt_id=r.receipt_id AND c.line_id=r.line_id WHERE r.tenant=$1 AND r.warehouse_id=$2 GROUP BY r.tenant,r.warehouse_id,r.order_id,r.order_line_id,r.sku,r.lot ORDER BY r.order_id,r.order_line_id,r.lot")
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
        Ok(
            json!({"version":s.version,"ok":ok,"rows":proofs,"receipts":receipt_proofs,"returns":return_proofs,"receipt_cancellations":cancellation_proofs,"shipments":shipment_proofs,"transfers":transfer_proofs,"packages":package_proofs,"lot_controls":lot_proof,"units":unit_proof,"serials":serial_proof,"method":"full_append_only_ledger_and_order_line_allocations"}),
        )
    }
    pub async fn outbox(&self, t: &str, w: &str) -> Result<Vec<Value>> {
        let rows=sqlx::query("SELECT o.id,o.destination,o.status,o.attempts,o.last_error,o.event_id,e.payload->>'type' AS kind,e.payload->>'occurred_at' AS occurred_at FROM wms_outbox o JOIN wms_events e ON e.id=o.event_id WHERE o.tenant=$1 AND e.warehouse_id=$2 ORDER BY e.created_at DESC,o.id LIMIT 200").bind(t).bind(w).fetch_all(&self.pool).await?;
        rows.iter().map(|r|Ok(json!({"id":r.try_get::<String,_>("id")?,"destination":r.try_get::<String,_>("destination")?,"status":r.try_get::<String,_>("status")?,"attempts":r.try_get::<i64,_>("attempts")?,"last_error":r.try_get::<String,_>("last_error")?,"event_id":r.try_get::<String,_>("event_id")?,"type":r.try_get::<String,_>("kind")?,"occurred_at":r.try_get::<String,_>("occurred_at")?}))).collect()
    }
    pub async fn retry(&self, i: &Identity, id: &str) -> Result<Value> {
        if i.role != "admin" {
            return Err(Error::new(
                403,
                "Only administrators may retry dead letters",
            ));
        }
        let mut tx = self.pool.begin().await?;
        let n=sqlx::query("UPDATE wms_outbox SET status='pending',due_at=now(),attempts=0,lease_token=NULL,lease_until=NULL WHERE tenant=$1 AND id=$2 AND status='dead_letter'").bind(&i.tenant).bind(id).execute(&mut *tx).await?.rows_affected();
        if n != 1 {
            return Err(Error::new(
                409,
                "Only an existing dead letter can be retried",
            ));
        }
        sqlx::query("INSERT INTO wms_admin_audit(id,tenant,actor,action,reference) VALUES($1,$2,$3,'outbox.retry',$4)").bind(domain::id()).bind(&i.tenant).bind(&i.actor).bind(id).execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(json!({"queued":true}))
    }
}
