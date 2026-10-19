//! Tenant-scoped lot review. Reads never quarantine, move stock, infer a common
//! manufacturer batch, or turn a retained legacy shipment into an observed event.
use crate::{
    api::App,
    domain::{State, Warehouse},
    lot_control::{self, Availability},
    persistence::decode,
    store::{Error, Identity, Result},
};
use axum::{extract::Query, Extension, Json};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use sqlx::Row;
use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Section {
    Facilities,
    Positions,
    Allocations,
    Receipts,
    Shipments,
    Transfers,
    Movements,
    Policies,
    Decisions,
}
impl Section {
    fn name(self) -> &'static str {
        match self {
            Self::Facilities => "facilities",
            Self::Positions => "positions",
            Self::Allocations => "allocations",
            Self::Receipts => "receipts",
            Self::Shipments => "shipments",
            Self::Transfers => "transfers",
            Self::Movements => "movements",
            Self::Policies => "policies",
            Self::Decisions => "decisions",
        }
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PageQuery {
    pub sku: String,
    pub lot: String,
    pub section: Option<Section>,
    pub limit: Option<i64>,
    pub review: Option<String>,
    pub after: Option<String>,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Cursor {
    schema_version: u8,
    review: String,
    section: Section,
    warehouse_id: String,
    key: Vec<String>,
}
fn digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn invalid() -> Error {
    Error::new(
        422,
        "Lot trace requires an exact SKU/lot, original section/cursor/review and limit 1–100",
    )
}
fn add(target: &mut i64, n: i64) -> Result<()> {
    *target = target
        .checked_add(n)
        .ok_or_else(|| Error::new(503, "Lot trace quantity exceeds the supported exact range"))?;
    Ok(())
}
struct Facility {
    state: State,
    availability: Availability,
    baseline_version: i64,
}
pub async fn page(
    axum::extract::State(app): axum::extract::State<App>,
    Extension(identity): Extension<Identity>,
    query: std::result::Result<Query<PageQuery>, axum::extract::rejection::QueryRejection>,
) -> Result<Json<Value>> {
    let Query(q) = query.map_err(|_| invalid())?;
    let section = q.section.unwrap_or(Section::Facilities);
    let limit = q.limit.unwrap_or(50);
    if !lot_control::text(&q.sku)
        || !lot_control::text(&q.lot)
        || !(1..=100).contains(&limit)
        || q.review.as_deref().is_some_and(|v| !digest(v))
    {
        return Err(invalid());
    }
    let cursor: Option<Cursor> = q
        .after
        .as_ref()
        .map(|raw| {
            if raw.len() > 4096 {
                return Err(invalid());
            }
            let c: Cursor = serde_json::from_str(raw).map_err(|_| invalid())?;
            let key_count = match section {
                Section::Facilities => 0,
                Section::Positions | Section::Movements => 1,
                Section::Allocations => 3,
                Section::Receipts | Section::Transfers | Section::Policies | Section::Decisions => {
                    2
                }
                Section::Shipments => 4,
            };
            if c.schema_version != 1
                || c.section != section
                || !digest(&c.review)
                || !lot_control::text(&c.warehouse_id)
                || c.key.len() != key_count
                || c.key.iter().any(|v| !lot_control::text(v))
                || q.review.as_ref().is_some_and(|v| v != &c.review)
            {
                return Err(invalid());
            }
            Ok(c)
        })
        .transpose()?;
    let mut tx = app.store.pool.begin().await?;
    sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY")
        .execute(&mut *tx)
        .await?;
    let checked_at: String = sqlx::query_scalar(
        r#"SELECT to_char(clock_timestamp() AT TIME ZONE 'UTC','YYYY-MM-DD"T"HH24:MI:SS.US"Z"')"#,
    )
    .fetch_one(&mut *tx)
    .await?;
    let at = chrono::DateTime::parse_from_rfc3339(&checked_at)
        .map_err(|_| Error::new(503, "Database clock unavailable"))?
        .with_timezone(&chrono::Utc);
    let heads = sqlx::query(include_str!("lot_trace_heads.sql"))
        .bind(&identity.tenant)
        .bind(&q.sku)
        .bind(&q.lot)
        .fetch_all(&mut *tx)
        .await?;
    let versions: Vec<(String, i64)> = heads
        .iter()
        .map(|row| Ok((row.try_get("warehouse_id")?, row.try_get("version")?)))
        .collect::<Result<_>>()?;
    let review = format!(
        "{:x}",
        Sha256::digest(
            serde_json::to_vec(&json!([1, identity.tenant, q.sku, q.lot, versions]))
                .map_err(|_| Error::new(503, "Lot review unavailable"))?
        )
    );
    if q.review.as_ref().is_some_and(|v| v != &review)
        || cursor.as_ref().is_some_and(|c| c.review != review)
    {
        return Err(Error::new(
            409,
            "Lot trace source changed; discard the old pages and explicitly refresh the complete review",
        ));
    }
    let warehouse_ids: Vec<_> = versions.iter().map(|v| v.0.clone()).collect();
    let mut facilities = BTreeMap::new();
    for row in &heads {
        let warehouse: Warehouse = decode(row.try_get("metadata")?)?;
        let mut state = State::empty(warehouse);
        state.version = row.try_get("version")?;
        let product: Option<Value> = sqlx::query_scalar(
            "SELECT data FROM wms_products WHERE tenant=$1 AND warehouse_id=$2 AND sku=$3",
        )
        .bind(&identity.tenant)
        .bind(&state.warehouse.id)
        .bind(&q.sku)
        .fetch_optional(&mut *tx)
        .await?;
        state.products.push(decode(product.ok_or_else(|| {
            Error::new(503, "Lot trace product evidence requires repair")
        })?)?);
        crate::lot_store::load(
            &mut tx,
            &identity.tenant,
            &state.warehouse.id.clone(),
            &mut state,
        )
        .await?;
        lot_control::validate(&state).map_err(|e| Error::new(503, e))?;
        let availability = lot_control::availability_at(&state, &q.sku, &q.lot, at);
        let baseline_version: i64 = sqlx::query_scalar(
            "SELECT baseline_version FROM wms_lot_enforcement WHERE tenant=$1 AND warehouse_id=$2",
        )
        .bind(&identity.tenant)
        .bind(&state.warehouse.id)
        .fetch_one(&mut *tx)
        .await?;
        if baseline_version > state.version {
            return Err(Error::new(
                503,
                "Lot trace historical eligibility baseline exceeds its original warehouse history",
            ));
        }
        facilities.insert(
            state.warehouse.id.clone(),
            Facility {
                state,
                availability,
                baseline_version,
            },
        );
    }
    // Aggregate only small per-facility rows; return positions through a keyset
    // page rather than materializing an unbounded history/position response.
    let balances = sqlx::query(include_str!("lot_trace_balances.sql"))
        .bind(&identity.tenant)
        .bind(&q.sku)
        .bind(&q.lot)
        .fetch_all(&mut *tx)
        .await?;
    let (mut physical, mut reserved, mut usable, mut receiving, mut staged, mut positions) =
        (0_i64, 0_i64, 0_i64, 0_i64, 0_i64, 0_i64);
    let mut physical_ok = true;
    let mut facility_balances = BTreeMap::new();
    for row in balances {
        let w: String = row.try_get("warehouse_id")?;
        let p: i64 = row.try_get("physical")?;
        let r: i64 = row.try_get("reserved")?;
        let f: i64 = row.try_get("free_storage")?;
        let ok: bool = row.try_get("ok")?;
        let eligible = facilities
            .get(&w)
            .is_some_and(|f| f.availability == Availability::Available);
        let u = if eligible && ok { f } else { 0 };
        add(&mut physical, p)?;
        add(&mut reserved, r)?;
        add(&mut usable, u)?;
        add(&mut receiving, row.try_get("receiving")?)?;
        add(&mut staged, row.try_get("staged")?)?;
        add(&mut positions, row.try_get("positions")?)?;
        physical_ok &= ok;
        facility_balances.insert(
            w,
            json!({"physical":p,"reserved":r,"usable_storage":u,"physical_reconciliation_ok":ok}),
        );
    }
    let counts = sqlx::query("SELECT section,count(*)::bigint AS count FROM wms_lot_trace_records($1,$2,$3,$4) GROUP BY section")
        .bind(&identity.tenant).bind(&q.sku).bind(&q.lot).bind(&warehouse_ids).fetch_all(&mut *tx).await?;
    let mut sections = BTreeMap::from([
        ("facilities".to_string(), facilities.len() as i64),
        ("positions".to_string(), positions),
        ("allocations".into(), 0),
        ("receipts".into(), 0),
        ("shipments".into(), 0),
        ("transfers".into(), 0),
        ("movements".into(), 0),
        ("policies".into(), 0),
        ("decisions".into(), 0),
    ]);
    for row in counts {
        sections.insert(row.try_get::<String, _>("section")?, row.try_get("count")?);
    }
    let last_warehouse = cursor
        .as_ref()
        .map(|c| c.warehouse_id.as_str())
        .unwrap_or("");
    let last_key = cursor.as_ref().map(|c| json!(c.key)).unwrap_or(json!([]));
    let mut items = Vec::new();
    match section {
        Section::Facilities => {
            for (w, f) in facilities
                .iter()
                .filter(|(w, _)| w.as_str() > last_warehouse)
                .take(limit as usize + 1)
            {
                let balance = facility_balances.get(w).cloned().unwrap_or_else(|| json!({"physical":0,"reserved":0,"usable_storage":0,"physical_reconciliation_ok":true}));
                items.push(json!({"warehouse_id":w,"key":[],"source_ok":balance["physical_reconciliation_ok"],"document":{"warehouse":f.state.warehouse,"version":f.state.version,"availability":f.availability,"expiry_required":f.state.products[0].expiry_required,"control":f.state.lot_controls.iter().find(|c|c.sku==q.sku&&c.lot==q.lot),"historic_eligibility_unknown_through_version":f.baseline_version,"balance":balance}}));
            }
        }
        Section::Positions => {
            let rows=sqlx::query("SELECT * FROM wms_lot_trace_balances($1,$2,$3) WHERE (warehouse_id COLLATE \"C\",jsonb_build_array(location))>($4 COLLATE \"C\",$5::jsonb) ORDER BY warehouse_id COLLATE \"C\",jsonb_build_array(location) LIMIT $6").bind(&identity.tenant).bind(&q.sku).bind(&q.lot).bind(last_warehouse).bind(&last_key).bind(limit+1).fetch_all(&mut *tx).await?;
            for row in rows {
                let w: String = row.try_get("warehouse_id")?;
                let location: String = row.try_get("location")?;
                let kind: Option<String> = row.try_get("kind")?;
                let p: i64 = row.try_get("physical")?;
                let r: i64 = row.try_get("reserved")?;
                let ledger: i64 = row.try_get("ledger")?;
                let held: i64 = row.try_get("allocation_reserved")?;
                let ok = kind.is_some() && p >= 0 && r >= 0 && r <= p && p == ledger && r == held;
                let availability = facilities
                    .get(&w)
                    .map(|f| f.availability)
                    .unwrap_or(Availability::Invalid);
                let u = if ok
                    && kind.as_deref() == Some("storage")
                    && availability == Availability::Available
                {
                    p - r
                } else {
                    0
                };
                items.push(json!({"warehouse_id":w,"key":[location],"source_ok":ok,"document":{"location":location,"kind":kind,"physical":p,"reserved":r,"ledger":ledger,"allocation_reserved":held,"usable_storage":u,"availability":availability,"source_kind":"original_movements_and_allocation_ownership"}}));
            }
        }
        _ => {
            let rows=sqlx::query("SELECT warehouse_id,sort_key,document,COALESCE(source_ok,false) AS source_ok FROM wms_lot_trace_records($1,$2,$3,$4) WHERE section=$5 AND (warehouse_id COLLATE \"C\",sort_key)>($6 COLLATE \"C\",$7::jsonb) ORDER BY warehouse_id COLLATE \"C\",sort_key LIMIT $8")
                .bind(&identity.tenant).bind(&q.sku).bind(&q.lot).bind(&warehouse_ids).bind(section.name()).bind(last_warehouse).bind(last_key).bind(limit+1).fetch_all(&mut *tx).await?;
            for row in rows {
                items.push(json!({"warehouse_id":row.try_get::<String,_>("warehouse_id")?,"key":row.try_get::<Value,_>("sort_key")?,"document":row.try_get::<Value,_>("document")?,"source_ok":row.try_get::<bool,_>("source_ok")?}));
            }
        }
    }
    let more = items.len() > limit as usize;
    items.truncate(limit as usize);
    let next_cursor = if more {
        let last = items
            .last()
            .ok_or_else(|| Error::new(503, "Lot cursor unavailable"))?;
        Some(
            serde_json::to_string(&Cursor {
                schema_version: 1,
                review: review.clone(),
                section,
                warehouse_id: decode(last["warehouse_id"].clone())?,
                key: decode(last["key"].clone())?,
            })
            .map_err(|_| Error::new(503, "Lot cursor unavailable"))?,
        )
    } else {
        None
    };
    let page_ok = items.iter().all(|v| v["source_ok"] == true);
    tx.commit().await?;
    Ok(Json(
        json!({"schema_version":1,"sku":q.sku,"lot":q.lot,"review":review,"checked_at":checked_at,"section":section,"items":items,"next_cursor":next_cursor,"summary":{"physical":physical,"reserved":reserved,"usable_storage":usable,"receiving":receiving,"staged":staged,"physical_reconciliation_ok":physical_ok,"sections":sections},"page_source_ok":page_ok,"source_checks":"physical_and_reservation_reconstruction_plus_selected_page_sources","limits":["Exact SKU/lot labels do not establish common manufacturer batch genealogy","Each page uses a fresh database instant; expiry can change without a warehouse version change","Review fencing covers committed warehouse work; no multi-page physical observation or tenant-wide recall is implied","Legacy/synthetic shipment baselines remain distinct from actual original handoff events"]}),
    ))
}
