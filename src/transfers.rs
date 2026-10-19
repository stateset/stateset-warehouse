//! Physical facility handoffs and partial arrivals. Goods in transit are never
//! available to allocate in either warehouse, and are never commercial shipments.
use crate::{
    api::App,
    domain::{self, Command, Movement, State, Stock},
    persistence::decode,
    store::{Error, Identity, Result},
};
use axum::{
    extract::{Path, Query, State as AppState},
    Extension, Json,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sqlx::{PgConnection, Row};
use std::collections::BTreeSet;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LineInput {
    pub id: String,
    pub sku: String,
    pub lot: String,
    pub location: String,
    pub quantity: i64,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Line {
    pub id: String,
    pub sku: String,
    pub lot: String,
    pub location: String,
    pub quantity: i64,
    pub movement_id: String,
    pub received: i64,
    pub lost: i64,
    pub loss_record_count: i64,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Loss {
    pub id: String,
    pub quantity: i64,
    pub reversed: i64,
    pub incident_reference: String,
    pub reason: String,
    pub actor: String,
    pub recorded_at: String,
    pub event_id: String,
    pub version: i64,
    pub line_id: String,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Transfer {
    pub id: String,
    pub seq: i64,
    pub source_warehouse_id: String,
    pub destination_warehouse_id: String,
    pub reference: String,
    pub carrier: String,
    pub handoff_reference: String,
    pub actor: String,
    pub dispatched_at: String,
    pub source_version: i64,
    pub source_event_id: String,
    pub lines: Vec<Line>,
    #[serde(skip)]
    pub reviewed_loss: Option<Loss>,
    pub ok: bool,
}
fn text(value: &str) -> domain::Result<()> {
    if value.trim().is_empty()
        || value.trim() != value
        || value.len() > 240
        || value.chars().any(char::is_control)
    {
        return Err("Transfer fields require 1–240 characters without surrounding whitespace or control characters".into());
    }
    Ok(())
}
fn quantity(value: i64) -> domain::Result<()> {
    if !(1..=1_000_000).contains(&value) {
        return Err("Transfer quantity must be 1–1,000,000 whole each units".into());
    }
    Ok(())
}
pub(crate) fn apply(state: &mut State, command: &Command, actor: &str) -> domain::Result<String> {
    match command {
        Command::RecordTransferLoss {
            transfer_id,
            line_id,
            quantity: q,
            incident_reference,
            reason,
        } => {
            for v in [
                transfer_id.as_str(),
                line_id,
                incident_reference,
                reason,
                actor,
            ] {
                text(v)?;
            }
            quantity(*q)?;
            let t = state
                .transfer_context
                .as_ref()
                .filter(|t| {
                    t.ok && t.id == *transfer_id && t.destination_warehouse_id == state.warehouse.id
                })
                .ok_or(
                    "Loss settlement requires verified original destination transfer evidence",
                )?;
            let line = t
                .lines
                .iter()
                .find(|l| l.id == *line_id)
                .ok_or("Original transfer line not found")?;
            if *q > line.quantity - line.received - line.lost {
                return Err(
                    "Loss exceeds this original line's remaining in-transit quantity".into(),
                );
            }
            return Ok(domain::id());
        }
        Command::ReverseTransferLoss {
            loss_id,
            quantity: q,
            reason,
        } => {
            for v in [loss_id.as_str(), reason, actor] {
                text(v)?;
            }
            quantity(*q)?;
            let t = state
                .transfer_context
                .as_ref()
                .filter(|t| t.ok && t.destination_warehouse_id == state.warehouse.id)
                .ok_or("Reversal requires verified original destination transfer evidence")?;
            let loss = t
                .reviewed_loss
                .as_ref()
                .filter(|loss| loss.id == *loss_id)
                .ok_or("Original loss not found")?;
            if *q > loss.quantity - loss.reversed {
                return Err(
                    "Reversal exceeds this exact original loss's remaining quantity".into(),
                );
            }
            return Ok(domain::id());
        }
        _ => {}
    }
    let (transfer_id, entries) = match command {
        Command::DispatchTransfer {
            reference,
            destination_warehouse_id,
            carrier,
            handoff_reference,
            lines,
        } => {
            for v in [
                reference,
                destination_warehouse_id,
                carrier,
                handoff_reference,
            ] {
                text(v)?;
            }
            if destination_warehouse_id == &state.warehouse.id {
                return Err("A transfer requires a different destination facility".into());
            }
            if lines.is_empty() || lines.len() > 100 {
                return Err("A transfer requires 1–100 exact lines".into());
            }
            let mut ids = BTreeSet::new();
            for line in lines {
                for v in [&line.id, &line.sku, &line.lot, &line.location] {
                    text(v)?;
                }
                quantity(line.quantity)?;
                if !ids.insert(&line.id) {
                    return Err("Transfer line IDs must be unique".into());
                }
                if !state.products.iter().any(|p| p.sku == line.sku)
                    || !state
                        .locations
                        .iter()
                        .any(|l| l.code == line.location && l.kind == "storage")
                {
                    return Err("Transfer dispatch requires a known SKU at a storage bin".into());
                }
                crate::lot_control::require(state, &line.sku, &line.lot)?;
                let stock = state
                    .stocks
                    .iter_mut()
                    .find(|b| b.sku == line.sku && b.lot == line.lot && b.location == line.location)
                    .ok_or("No source stock for this transfer line")?;
                if stock.on_hand - stock.reserved < line.quantity {
                    return Err("Transfer exceeds unreserved source stock".into());
                }
                stock.on_hand -= line.quantity;
            }
            let id = domain::id();
            let entries = lines
                .iter()
                .map(|l| {
                    (
                        l.clone(),
                        Some(l.location.clone()),
                        None,
                        handoff_reference.clone(),
                        "transfer_dispatch",
                    )
                })
                .collect::<Vec<_>>();
            (id, entries)
        }
        Command::ReceiveTransfer {
            transfer_id,
            line_id,
            quantity: q,
            location,
            arrival_reference,
        } => {
            for v in [transfer_id, line_id, location, arrival_reference] {
                text(v)?;
            }
            quantity(*q)?;
            let transfer = state
                .transfer_context
                .as_ref()
                .filter(|t| {
                    t.id == *transfer_id && t.destination_warehouse_id == state.warehouse.id && t.ok
                })
                .ok_or("Destination receiving requires verified original transfer evidence")?;
            let line = transfer
                .lines
                .iter()
                .find(|l| l.id == *line_id)
                .ok_or("Original transfer line not found")?;
            if *q > line.quantity - line.received - line.lost {
                return Err(
                    "Arrival exceeds this original transfer line's in-transit quantity".into(),
                );
            }
            if !state
                .locations
                .iter()
                .any(|l| l.code == *location && l.kind == "receiving")
            {
                return Err("Transfer arrivals must enter a receiving bin".into());
            }
            let input = LineInput {
                id: line.id.clone(),
                sku: line.sku.clone(),
                lot: line.lot.clone(),
                location: line.location.clone(),
                quantity: line.quantity,
            };
            if !state.products.iter().any(|p| p.sku == input.sku) {
                return Err("Destination SKU is not configured".into());
            }
            let index = state
                .stocks
                .iter()
                .position(|b| b.sku == input.sku && b.lot == input.lot && b.location == *location);
            if let Some(index) = index {
                state.stocks[index].on_hand += q;
            } else {
                state.stocks.push(Stock {
                    sku: input.sku.clone(),
                    lot: input.lot.clone(),
                    location: location.clone(),
                    on_hand: *q,
                    reserved: 0,
                });
            }
            let mut arrived = input;
            arrived.quantity = *q;
            (
                transfer_id.clone(),
                vec![(
                    arrived,
                    None,
                    Some(location.clone()),
                    arrival_reference.clone(),
                    "transfer_receive",
                )],
            )
        }
        _ => return Err("Unsupported transfer action".into()),
    };
    for (line, from, to, reason, kind) in entries {
        state.movements.push(Movement {
            id: domain::id(),
            kind: kind.into(),
            sku: line.sku,
            lot: line.lot,
            from,
            to,
            quantity: line.quantity,
            reference: transfer_id.clone(),
            receipt_id: None,
            receipt_line_id: None,
            transfer_id: Some(transfer_id.clone()),
            transfer_line_id: Some(line.id),
            transfer_event_id: None,
            package_id: None,
            order_line_id: None,
            allocation_location: None,
            shipment_event_id: None,
            reason,
            actor: actor.into(),
            at: domain::now(),
        });
    }
    Ok(transfer_id)
}

pub(crate) async fn load(c: &mut PgConnection, t: &str, w: &str, id: &str) -> Result<Transfer> {
    let data:Value=sqlx::query_scalar("SELECT wms_transfer_document($1,id)||jsonb_build_object('ok',wms_transfer_consistent($1,id)) FROM wms_transfers WHERE tenant=$1 AND (source_warehouse_id=$2 OR destination_warehouse_id=$2) AND id=$3")
        .bind(t).bind(w).bind(id).fetch_optional(c).await?.ok_or_else(||Error::new(404,"Transfer not found in this facility"))?;
    decode(data)
}
pub(crate) async fn lock_destination(
    c: &mut PgConnection,
    t: &str,
    w: &str,
    command: &Command,
) -> Result<()> {
    if let Command::DispatchTransfer {
        destination_warehouse_id,
        lines,
        ..
    } = command
    {
        if w == destination_warehouse_id {
            return Err(Error::new(
                422,
                "A transfer requires a different destination facility",
            ));
        }
        let rows=sqlx::query("SELECT warehouse_id FROM wms_warehouses WHERE tenant=$1 AND warehouse_id=ANY($2) ORDER BY warehouse_id FOR UPDATE")
            .bind(t).bind(vec![w,destination_warehouse_id.as_str()]).fetch_all(&mut *c).await?;
        if rows.len() != 2 {
            return Err(Error::new(404, "Source or destination facility not found"));
        }
        let skus: Vec<_> = lines
            .iter()
            .map(|l| l.sku.as_str())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        let available: Vec<String> = sqlx::query_scalar(
            "SELECT sku FROM wms_products WHERE tenant=$1 AND warehouse_id=$2 AND sku=ANY($3)",
        )
        .bind(t)
        .bind(destination_warehouse_id)
        .bind(&skus)
        .fetch_all(c)
        .await?;
        if available.len() != skus.len() {
            return Err(Error::new(
                422,
                "Configure every transfer SKU at the destination before dispatch",
            ));
        }
    }
    Ok(())
}
pub(crate) async fn persist(
    c: &mut PgConnection,
    identity: &Identity,
    w: &str,
    command: &Command,
    next: &State,
    entity: &str,
    eid: &str,
) -> Result<()> {
    let t = &identity.tenant;
    let actor = &identity.actor;
    if matches!(
        command,
        Command::RecordTransferLoss { .. } | Command::ReverseTransferLoss { .. }
    ) {
        let transfer = next
            .transfer_context
            .as_ref()
            .ok_or_else(|| Error::new(503, "Original transfer review missing"))?;
        let (line, kind, q, original, incident, reason) = match command {
            Command::RecordTransferLoss {
                line_id,
                quantity,
                incident_reference,
                reason,
                ..
            } => (
                line_id.as_str(),
                "loss",
                *quantity,
                None,
                Some(incident_reference.as_str()),
                reason.as_str(),
            ),
            Command::ReverseTransferLoss {
                loss_id,
                quantity,
                reason,
            } => {
                let line = transfer
                    .reviewed_loss
                    .as_ref()
                    .filter(|loss| loss.id == *loss_id)
                    .ok_or_else(|| Error::new(503, "Original loss review missing"))?;
                (
                    line.line_id.as_str(),
                    "reversal",
                    *quantity,
                    Some(loss_id.as_str()),
                    None,
                    reason.as_str(),
                )
            }
            _ => unreachable!(),
        };
        sqlx::query("INSERT INTO wms_transfer_loss_records(tenant,id,transfer_id,line_id,warehouse_id,kind,quantity,original_loss_id,incident_reference,reason,actor,recorded_at,version,event_id) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14)")
            .bind(t).bind(entity).bind(&transfer.id).bind(line).bind(w).bind(kind).bind(q).bind(original).bind(incident).bind(reason).bind(actor).bind(next.quality_time().to_rfc3339()).bind(next.version).bind(eid).execute(&mut *c).await?;
    }
    if let Command::DispatchTransfer {
        reference,
        destination_warehouse_id,
        carrier,
        handoff_reference,
        lines,
    } = command
    {
        let movements: Vec<_> = next
            .movements
            .iter()
            .filter(|m| {
                m.transfer_id.as_deref() == Some(entity)
                    && m.transfer_event_id.as_deref() == Some(eid)
            })
            .collect();
        let at = &movements
            .first()
            .ok_or_else(|| Error::new(503, "Original transfer movements are missing"))?
            .at;
        sqlx::query("INSERT INTO wms_transfers(tenant,id,source_warehouse_id,destination_warehouse_id,reference,carrier,handoff_reference,actor,dispatched_at,source_version,source_event_id) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11)")
            .bind(t).bind(entity).bind(w).bind(destination_warehouse_id).bind(reference).bind(carrier).bind(handoff_reference).bind(actor).bind(at).bind(next.version).bind(eid).execute(&mut *c).await?;
        for line in lines {
            let movement = movements
                .iter()
                .find(|m| m.transfer_line_id.as_deref() == Some(&line.id))
                .ok_or_else(|| Error::new(503, "Original transfer line movement missing"))?;
            sqlx::query("INSERT INTO wms_transfer_lines(tenant,transfer_id,id,source_warehouse_id,destination_warehouse_id,sku,lot,location,quantity,movement_id) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10)")
                .bind(t).bind(entity).bind(&line.id).bind(w).bind(destination_warehouse_id).bind(&line.sku).bind(&line.lot).bind(&line.location).bind(line.quantity).bind(&movement.id).execute(&mut *c).await?;
        }
    }
    Ok(())
}
pub(crate) async fn reviewed_transfer(
    c: &mut PgConnection,
    t: &str,
    w: &str,
    command: &Command,
) -> Result<Option<Transfer>> {
    let id=match command {
        Command::ReceiveTransfer{transfer_id,..}|Command::RecordTransferLoss{transfer_id,..}=>Some(transfer_id.clone()),
        Command::ReverseTransferLoss{loss_id,..}=>Some(sqlx::query_scalar::<_,String>("SELECT transfer_id FROM wms_transfer_loss_records WHERE tenant=$1 AND warehouse_id=$2 AND id=$3 AND kind='loss'")
            .bind(t).bind(w).bind(loss_id).fetch_optional(&mut *c).await?.ok_or_else(||Error::new(404,"Original loss not found in this facility"))?),
        _=>None,
    };
    if let Some(id) = id {
        let mut transfer = load(c, t, w, &id).await?;
        if !transfer.ok {
            return Err(Error::new(
                409,
                "Original transfer evidence disagrees; receiving and loss settlement held",
            ));
        }
        if let Command::ReverseTransferLoss { loss_id, .. } = command {
            let document:Value=sqlx::query_scalar("SELECT to_jsonb(r)-'tenant'||jsonb_build_object('reversed',COALESCE((SELECT sum(x.quantity) FROM wms_transfer_loss_records x WHERE x.tenant=$1 AND x.original_loss_id=r.id),0)) FROM wms_transfer_loss_records r WHERE tenant=$1 AND warehouse_id=$2 AND id=$3 AND kind='loss'")
                .bind(t).bind(w).bind(loss_id).fetch_one(&mut *c).await?;
            transfer.reviewed_loss = Some(decode(document)?);
        }
        Ok(Some(transfer))
    } else {
        Ok(None)
    }
}
pub(crate) async fn proof(c: &mut PgConnection, t: &str, w: &str) -> Result<Vec<Value>> {
    let rows=sqlx::query("SELECT id,wms_transfer_document($1,id) AS document,wms_transfer_consistent($1,id) AS ok FROM wms_transfers WHERE tenant=$1 AND (source_warehouse_id=$2 OR destination_warehouse_id=$2) ORDER BY seq")
        .bind(t).bind(w).fetch_all(c).await?;
    rows.into_iter().map(|r| {
        let d:Value=r.try_get("document")?;let lines=d["lines"].as_array().ok_or_else(||Error::new(503,"Transfer lines require repair"))?;
        Ok(json!({"transfer_id":r.try_get::<String,_>("id")?,"source_warehouse_id":d["source_warehouse_id"],"destination_warehouse_id":d["destination_warehouse_id"],"source_event_id":d["source_event_id"],"ok":r.try_get::<bool,_>("ok")?,"lines":lines.iter().map(|l|json!({"line_id":l["id"],"sku":l["sku"],"lot":l["lot"],"dispatched":l["quantity"],"received":l["received"],"lost":l["lost"],"loss_record_count":l["loss_record_count"],"in_transit":l["quantity"].as_i64().unwrap_or(0)-l["received"].as_i64().unwrap_or(0)-l["lost"].as_i64().unwrap_or(0)})).collect::<Vec<_>>()}))
    }).collect()
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LossPageQuery {
    pub after: Option<i64>,
    pub limit: Option<i64>,
    pub review: Option<i64>,
}
pub async fn losses(
    AppState(app): AppState<App>,
    Extension(i): Extension<Identity>,
    Path((w, id)): Path<(String, String)>,
    query: std::result::Result<Query<LossPageQuery>, axum::extract::rejection::QueryRejection>,
) -> Result<Json<Value>> {
    let Query(q) = query.map_err(|_| Error::new(422, "Invalid loss journal page"))?;
    let after = q.after.unwrap_or(0);
    let limit = q.limit.unwrap_or(50);
    if after < 0
        || (after > 0 && q.review.is_none())
        || !(1..=100).contains(&limit)
        || q.review.is_some_and(|r| r < 0)
    {
        return Err(Error::new(422,"Loss journal requires an original destination review, nonnegative cursor and limit 1–100"));
    }
    let mut tx = app.store.pool.begin().await?;
    sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY")
        .execute(&mut *tx)
        .await?;
    let transfer = load(&mut tx, &i.tenant, &w, &id).await?;
    let rows = sqlx::query(
        "SELECT warehouse_id,version FROM wms_warehouses WHERE tenant=$1 AND warehouse_id=ANY($2)",
    )
    .bind(&i.tenant)
    .bind(vec![w.as_str(), transfer.destination_warehouse_id.as_str()])
    .fetch_all(&mut *tx)
    .await?;
    let version = rows
        .iter()
        .find(|r| r.get::<String, _>("warehouse_id") == w)
        .map(|r| r.get::<i64, _>("version"))
        .ok_or_else(|| Error::new(404, "Warehouse not found"))?;
    let review = rows
        .iter()
        .find(|r| r.get::<String, _>("warehouse_id") == transfer.destination_warehouse_id)
        .map(|r| r.get::<i64, _>("version"))
        .ok_or_else(|| Error::new(404, "Destination not found"))?;
    if q.review.is_some_and(|r| r != review) {
        return Err(Error::new(409,"Destination changed; discard original loss pages and explicitly refresh the transfer review"));
    }
    let records=sqlx::query("SELECT r.version,to_jsonb(r)-'tenant'||jsonb_build_object('reversed',CASE WHEN r.kind='loss' THEN COALESCE((SELECT sum(x.quantity) FROM wms_transfer_loss_records x WHERE x.tenant=$1 AND x.original_loss_id=r.id),0) ELSE 0 END,'source_ok',wms_transfer_loss_original(r)) AS data FROM wms_transfer_loss_records r WHERE tenant=$1 AND transfer_id=$2 AND version>$3 ORDER BY version LIMIT $4")
        .bind(&i.tenant).bind(&id).bind(after).bind(limit+1).fetch_all(&mut *tx).await?;
    let items: Vec<Value> = records
        .iter()
        .take(limit as usize)
        .map(|r| r.get("data"))
        .collect();
    let next = if records.len() > limit as usize {
        items.last().map(|r| r["version"].clone())
    } else {
        None
    };
    let page_ok = transfer.ok && items.iter().all(|r| r["source_ok"] == true);
    tx.commit().await?;
    Ok(Json(
        json!({"warehouse_id":w,"version":version,"destination_warehouse_id":transfer.destination_warehouse_id,"transfer_id":id,"review":review,"items":items,"next_cursor":next,"page_source_ok":page_ok}),
    ))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PageQuery {
    pub after: Option<i64>,
    pub limit: Option<i64>,
    pub open_only: Option<bool>,
}
pub async fn page(
    AppState(app): AppState<App>,
    Extension(i): Extension<Identity>,
    Path(w): Path<String>,
    query: std::result::Result<Query<PageQuery>, axum::extract::rejection::QueryRejection>,
) -> Result<Json<Value>> {
    let Query(query) = query.map_err(|_| Error::new(422, "Invalid transfer page"))?;
    let after = query.after.unwrap_or(0);
    let limit = query.limit.unwrap_or(50);
    if after < 0 || !(1..=100).contains(&limit) {
        return Err(Error::new(
            422,
            "Transfer page requires nonnegative cursor and limit 1–100",
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
    let rows=sqlx::query("SELECT seq,wms_transfer_document($1,id)||jsonb_build_object('ok',wms_transfer_consistent($1,id)) AS data FROM wms_transfers WHERE tenant=$1 AND (source_warehouse_id=$2 OR destination_warehouse_id=$2) AND seq>$3 AND (NOT $4 OR wms_transfer_open($1,id)) ORDER BY seq LIMIT $5")
        .bind(&i.tenant).bind(&w).bind(after).bind(query.open_only.unwrap_or(true)).bind(limit+1).fetch_all(&mut *tx).await?;
    let more = rows.len() > limit as usize;
    let items = rows
        .iter()
        .take(limit as usize)
        .map(|r| json!({"cursor":r.get::<i64,_>("seq"),"data":r.get::<Value,_>("data")}))
        .collect::<Vec<_>>();
    let next = if more {
        items.last().map(|v| v["cursor"].clone())
    } else {
        None
    };
    tx.commit().await?;
    Ok(Json(
        json!({"warehouse_id":w,"version":version,"items":items,"next_cursor":next}),
    ))
}
