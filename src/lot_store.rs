//! Database-owned policy history and fresh admission checks. No caller clock,
//! cached snapshot or MCP transport can choose the persisted eligibility time.
use crate::{
    domain::State,
    persistence::decode,
    store::{Error, Result},
};
use serde_json::Value;
use sqlx::{PgConnection, Row};

pub(crate) async fn load(c: &mut PgConnection, t: &str, w: &str, state: &mut State) -> Result<()> {
    let skus: Vec<_> = state.products.iter().map(|p| p.sku.as_str()).collect();
    let valid: bool = sqlx::query_scalar("SELECT NOT EXISTS(SELECT 1 FROM wms_lot_changes h WHERE h.tenant=$1 AND h.warehouse_id=$2 AND h.sku=ANY($3) AND NOT COALESCE(wms_lot_change_valid(h.tenant,h.warehouse_id,h.version,h.ordinal),false)) AND NOT EXISTS(SELECT 1 FROM wms_products p WHERE p.tenant=$1 AND p.warehouse_id=$2 AND p.sku=ANY($3) AND (COALESCE((p.data->>'expiry_required')::boolean,false) IS DISTINCT FROM wms_lot_required(p.tenant,p.warehouse_id,p.sku,9223372036854775807)))")
        .bind(t).bind(w).bind(&skus).fetch_one(&mut *c).await?;
    if !valid {
        return Err(Error::new(
            503,
            "Lot policy source disagrees; availability and new fulfillment held",
        ));
    }
    state.lot_controls = sqlx::query("SELECT wms_lot_document($1,$2,sku,lot,$4) AS data FROM (SELECT DISTINCT sku,lot FROM wms_lot_changes WHERE tenant=$1 AND warehouse_id=$2 AND sku=ANY($3) AND lot IS NOT NULL AND version<=$4) l ORDER BY sku,lot")
        .bind(t).bind(w).bind(&skus).bind(state.version).fetch_all(&mut *c).await?
        .into_iter().map(|r| decode(r.get("data"))).collect::<Result<_>>()?;
    Ok(())
}

pub(crate) async fn lock(
    c: &mut PgConnection,
    t: &str,
    state: &State,
    command: &crate::domain::Command,
) -> Result<()> {
    use crate::domain::Command;
    let mut skus = std::collections::BTreeSet::new();
    match command {
        Command::SetProductExpiryPolicy { sku, .. }
        | Command::RecordLotExpiry { sku, .. }
        | Command::CorrectLotExpiry { sku, .. }
        | Command::QuarantineLot { sku, .. } => {
            skus.insert(sku.as_str());
        }
        Command::ReleaseLotHold { hold_id, .. } => {
            skus.extend(
                state
                    .lot_controls
                    .iter()
                    .filter(|l| l.holds.iter().any(|h| h.id == *hold_id))
                    .map(|l| l.sku.as_str()),
            );
        }
        Command::Allocate { order_id }
        | Command::AllocateAvailable { order_id }
        | Command::Pick { order_id, .. }
        | Command::Pack { order_id }
        | Command::Ship { order_id, .. }
        | Command::PackShipment { order_id, .. } => {
            if let Some(order) = state.orders.iter().find(|o| o.id == *order_id) {
                skus.extend(order.lines.iter().map(|l| l.sku.as_str()));
            }
        }
        Command::HandoffShipment { .. } => {
            // The canonical package owner was loaded before admission.
            skus.extend(
                state
                    .orders
                    .iter()
                    .flat_map(|o| o.allocations.iter().map(|a| a.sku.as_str())),
            );
        }
        Command::DispatchTransfer { lines, .. } => {
            skus.extend(lines.iter().map(|l| l.sku.as_str()));
        }
        _ => {}
    }
    let mut keys: Vec<_> = skus
        .into_iter()
        .map(|sku| serde_json::json!([state.warehouse.id, sku]))
        .collect();
    if let crate::domain::Command::ReceiveTransfer {
        transfer_id,
        line_id,
        ..
    } = command
    {
        let source: Option<(String,String)> = sqlx::query_as("SELECT h.source_warehouse_id,l.sku FROM wms_transfers h JOIN wms_transfer_lines l ON l.tenant=h.tenant AND l.transfer_id=h.id WHERE h.tenant=$1 AND h.id=$2 AND h.destination_warehouse_id=$3 AND l.id=$4")
            .bind(t).bind(transfer_id).bind(&state.warehouse.id).bind(line_id).fetch_optional(&mut *c).await?;
        if let Some((w, s)) = source {
            keys.push(serde_json::json!([state.warehouse.id, s]));
            keys.push(serde_json::json!([w, s]));
        }
    }
    // The ordered subquery feeds volatile lock calls in numeric-key order.
    // One round trip avoids a catalog-sized series of lock queries.
    sqlx::query("SELECT pg_advisory_xact_lock(key) FROM (SELECT DISTINCT wms_lot_lock_key($1,k->>0,k->>1) AS key FROM jsonb_array_elements($2) k ORDER BY key) ordered_keys ORDER BY key")
        .bind(t).bind(serde_json::json!(keys)).execute(&mut *c).await?;
    Ok(())
}

pub(crate) async fn persist(c: &mut PgConnection, t: &str, next: &State, eid: &str) -> Result<()> {
    for (n, change) in next.lot_changes.iter().enumerate() {
        sqlx::query("INSERT INTO wms_lot_changes(tenant,warehouse_id,version,ordinal,sku,lot,kind,source_event_id,document) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9)")
            .bind(t).bind(&next.warehouse.id).bind(next.version).bind(n as i32).bind(&change.sku).bind(&change.lot).bind(&change.kind).bind(eid).bind(serde_json::json!(change)).execute(&mut *c).await?;
    }
    Ok(())
}

pub(crate) async fn decisions(c: &mut PgConnection, t: &str, w: &str, v: i64) -> Result<Value> {
    Ok(sqlx::query_scalar("SELECT COALESCE(jsonb_agg(wms_lot_decision_document(kind,sku,lot,checked_at,required,document) ORDER BY kind,sku,lot),'[]'::jsonb) FROM wms_lot_decisions WHERE tenant=$1 AND warehouse_id=$2 AND version=$3")
        .bind(t).bind(w).bind(v).fetch_one(c).await?)
}

pub(crate) async fn proof(c: &mut PgConnection, t: &str, w: &str) -> Result<Value> {
    Ok(sqlx::query_scalar("SELECT jsonb_build_object('ok',NOT EXISTS(SELECT 1 FROM wms_events e WHERE e.tenant=$1 AND e.warehouse_id=$2 AND NOT COALESCE(wms_lot_event_valid(e.tenant,e.warehouse_id,e.version),false)) AND NOT EXISTS(SELECT 1 FROM wms_lot_changes h WHERE h.tenant=$1 AND h.warehouse_id=$2 AND NOT COALESCE(wms_lot_change_valid(h.tenant,h.warehouse_id,h.version,h.ordinal),false)) AND NOT EXISTS(SELECT 1 FROM wms_lot_decisions d WHERE d.tenant=$1 AND d.warehouse_id=$2 AND NOT COALESCE(wms_lot_decision_valid(d.tenant,d.warehouse_id,d.version,d.kind,d.sku,d.lot),false)), 'baseline_version',(SELECT baseline_version FROM wms_lot_enforcement WHERE tenant=$1 AND warehouse_id=$2),'policy_changes',(SELECT count(*) FROM wms_lot_changes WHERE tenant=$1 AND warehouse_id=$2),'eligibility_decisions',(SELECT count(*) FROM wms_lot_decisions WHERE tenant=$1 AND warehouse_id=$2))")
        .bind(t).bind(w).fetch_one(c).await?)
}

pub(crate) async fn prepare_arrival(
    c: &mut PgConnection,
    t: &str,
    next: &mut State,
    command: &crate::domain::Command,
    actor: &str,
) -> Result<()> {
    let crate::domain::Command::ReceiveTransfer {
        transfer_id,
        line_id,
        ..
    } = command
    else {
        return Ok(());
    };
    let row=sqlx::query("SELECT h.source_warehouse_id,h.source_version,l.sku,l.lot,w.version,w.metadata FROM wms_transfers h JOIN wms_transfer_lines l ON l.tenant=h.tenant AND l.transfer_id=h.id JOIN wms_warehouses w ON w.tenant=h.tenant AND w.warehouse_id=h.source_warehouse_id WHERE h.tenant=$1 AND h.id=$2 AND h.destination_warehouse_id=$3 AND l.id=$4")
        .bind(t).bind(transfer_id).bind(&next.warehouse.id).bind(line_id).fetch_one(&mut *c).await?;
    let source_warehouse_id: String = row.try_get("source_warehouse_id")?;
    let sku: String = row.try_get("sku")?;
    let lot: String = row.try_get("lot")?;
    let source_version: i64 = row.try_get("version")?;
    let mut source = State::empty(decode(row.try_get("metadata")?)?);
    source.version = source_version;
    let data: Value = sqlx::query_scalar(
        "SELECT data FROM wms_products WHERE tenant=$1 AND warehouse_id=$2 AND sku=$3",
    )
    .bind(t)
    .bind(&source_warehouse_id)
    .bind(&sku)
    .fetch_one(&mut *c)
    .await?;
    source.products.push(decode(data)?);
    load(c, t, &source_warehouse_id, &mut source).await?;
    let control = source
        .lot_controls
        .into_iter()
        .find(|c| c.sku == sku && c.lot == lot)
        .unwrap_or(crate::lot_control::Lot {
            sku: sku.clone(),
            lot: lot.clone(),
            expiry_recorded: false,
            expires_at: None,
            holds: vec![],
        });
    let dispatch_valid: bool =
        sqlx::query_scalar("SELECT COALESCE(wms_lot_dispatch_valid($1,$2,$3,$4,$5),false)")
            .bind(t)
            .bind(&source_warehouse_id)
            .bind(row.try_get::<i64, _>("source_version")?)
            .bind(&sku)
            .bind(&lot)
            .fetch_one(&mut *c)
            .await?;
    if !dispatch_valid {
        return Err(Error::new(
            503,
            "Original transfer lot eligibility disagrees; receiving requires evidence repair",
        ));
    }
    let dispatched: Option<Value>=sqlx::query_scalar("SELECT document FROM wms_lot_decisions WHERE tenant=$1 AND warehouse_id=$2 AND version=$3 AND kind='transfer_dispatch' AND sku=$4 AND lot=$5")
        .bind(t).bind(&source_warehouse_id).bind(row.try_get::<i64,_>("source_version")?).bind(&sku).bind(&lot).fetch_optional(&mut *c).await?;
    crate::lot_control::merge_arrival(
        next,
        crate::lot_control::Arrival {
            transfer_id: transfer_id.clone(),
            line_id: line_id.clone(),
            source_warehouse_id,
            source_version,
            source: control,
            source_required: source.products[0].expiry_required,
            dispatched: dispatched.map(decode).transpose()?,
            expiry_recorded: false,
        },
        actor,
    )
    .map_err(|e| Error::new(503, format!("Transfer lot evidence requires repair: {e}")))
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PageQuery {
    after: Option<String>,
    limit: Option<i64>,
    sku: Option<String>,
}
pub async fn page(
    axum::extract::State(app): axum::extract::State<crate::api::App>,
    axum::Extension(i): axum::Extension<crate::store::Identity>,
    axum::extract::Path(w): axum::extract::Path<String>,
    query: std::result::Result<
        axum::extract::Query<PageQuery>,
        axum::extract::rejection::QueryRejection,
    >,
) -> Result<axum::Json<Value>> {
    let axum::extract::Query(q) = query.map_err(|_| Error::new(422, "Invalid lot page"))?;
    let limit = q.limit.unwrap_or(50);
    let supplied_cursor = q.after.is_some();
    let after: Vec<String> = match q.after {
        None => vec![String::new(), String::new()],
        Some(v) if v.len() <= 2048 => {
            serde_json::from_str(&v).map_err(|_| Error::new(422, "Invalid original lot cursor"))?
        }
        _ => return Err(Error::new(422, "Invalid original lot cursor")),
    };
    if !(1..=100).contains(&limit)
        || after.len() != 2
        || q.sku.as_ref().is_some_and(|s| !crate::lot_control::text(s))
        || (supplied_cursor && after.iter().any(|s| !crate::lot_control::text(s)))
    {
        return Err(Error::new(
            422,
            "Lot page requires its original cursor, an exact optional SKU and limit 1–100",
        ));
    }
    let mut tx = app.store.pool.begin().await?;
    sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY")
        .execute(&mut *tx)
        .await?;
    let head=sqlx::query(r#"SELECT metadata,version,to_char(clock_timestamp() AT TIME ZONE 'UTC','YYYY-MM-DD"T"HH24:MI:SS.US"Z"') AS checked_at FROM wms_warehouses WHERE tenant=$1 AND warehouse_id=$2"#)
        .bind(&i.tenant).bind(&w).fetch_optional(&mut *tx).await?.ok_or_else(||Error::new(404,"Warehouse not found"))?;
    let mut state = State::empty(decode(head.try_get("metadata")?)?);
    state.version = head.try_get("version")?;
    let checked_at: String = head.try_get("checked_at")?;
    let at = chrono::DateTime::parse_from_rfc3339(&checked_at)
        .map_err(|_| Error::new(503, "Database clock unavailable"))?
        .with_timezone(&chrono::Utc);
    let rows=sqlx::query("WITH keys AS(SELECT sku,lot FROM wms_stock WHERE tenant=$1 AND warehouse_id=$2 UNION SELECT sku,lot FROM wms_lot_changes WHERE tenant=$1 AND warehouse_id=$2 AND lot IS NOT NULL) SELECT k.sku,k.lot,p.data,(SELECT COALESCE(sum(s.on_hand),0)::bigint FROM wms_stock s WHERE s.tenant=$1 AND s.warehouse_id=$2 AND s.sku=k.sku AND s.lot=k.lot) AS physical,(SELECT COALESCE(sum(s.reserved),0)::bigint FROM wms_stock s WHERE s.tenant=$1 AND s.warehouse_id=$2 AND s.sku=k.sku AND s.lot=k.lot) AS reserved,(SELECT COALESCE(sum(s.on_hand-s.reserved),0)::bigint FROM wms_stock s JOIN wms_locations l ON l.tenant=s.tenant AND l.warehouse_id=s.warehouse_id AND l.code=s.location AND l.kind='storage' WHERE s.tenant=$1 AND s.warehouse_id=$2 AND s.sku=k.sku AND s.lot=k.lot) AS free_storage FROM keys k JOIN wms_products p ON p.tenant=$1 AND p.warehouse_id=$2 AND p.sku=k.sku WHERE ($3::text IS NULL OR k.sku=$3) AND (k.sku,k.lot)>($4,$5) ORDER BY k.sku,k.lot LIMIT $6")
        .bind(&i.tenant).bind(&w).bind(q.sku).bind(&after[0]).bind(&after[1]).bind(limit+1).fetch_all(&mut *tx).await?;
    for row in rows.iter().take(limit as usize) {
        let product: crate::domain::Product = decode(row.try_get("data")?)?;
        if !state.products.iter().any(|p| p.sku == product.sku) {
            state.products.push(product);
        }
    }
    load(&mut tx, &i.tenant, &w, &mut state).await?;
    crate::lot_control::validate(&state).map_err(|e| Error::new(503, e))?;
    let mut items = Vec::new();
    for row in rows.iter().take(limit as usize) {
        let sku: String = row.try_get("sku")?;
        let lot: String = row.try_get("lot")?;
        let availability = crate::lot_control::availability_at(&state, &sku, &lot, at);
        let physical: i64 = row.try_get("physical")?;
        let reserved: i64 = row.try_get("reserved")?;
        let usable = if availability == crate::lot_control::Availability::Available {
            row.try_get::<i64, _>("free_storage")?
        } else {
            0
        };
        items.push(serde_json::json!({"cursor":serde_json::to_string(&[&sku,&lot]).expect("lot cursor"),"sku":sku,"lot":lot,"physical":physical,"reserved":reserved,"unreserved":physical-reserved,"usable":usable,"availability":availability,"expiry_required":state.products.iter().find(|p|p.sku==sku).is_some_and(|p|p.expiry_required),"control":state.lot_controls.iter().find(|c|c.sku==sku && c.lot==lot)}));
    }
    let next = if rows.len() > limit as usize {
        items.last().map(|v| v["cursor"].clone())
    } else {
        None
    };
    tx.commit().await?;
    Ok(axum::Json(
        serde_json::json!({"warehouse_id":w,"version":state.version,"checked_at":checked_at,"items":items,"next_cursor":next}),
    ))
}

pub(crate) async fn guard_order(
    c: &mut PgConnection,
    t: &str,
    w: &str,
    order_id: Option<&str>,
    command: &crate::domain::Command,
) -> Result<()> {
    use crate::domain::Command;
    if matches!(
        command,
        Command::Allocate { .. }
            | Command::AllocateAvailable { .. }
            | Command::Pick { .. }
            | Command::Pack { .. }
            | Command::PackShipment { .. }
            | Command::Ship { .. }
            | Command::HandoffShipment { .. }
    ) {
        if let Some(order_id) = order_id {
            let valid: bool =
                sqlx::query_scalar("SELECT COALESCE(wms_lot_order_valid($1,$2,$3),false)")
                    .bind(t)
                    .bind(w)
                    .bind(order_id)
                    .fetch_one(c)
                    .await?;
            if !valid {
                return Err(Error::new(503,"Original order lot eligibility disagrees; new fulfillment held for evidence repair"));
            }
        }
    }
    Ok(())
}
