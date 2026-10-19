//! Physical customer returns are receipts, never a second shipment or refund.
use crate::{
    domain::{ReceiptInput, State},
    store::{Error, Identity, Store},
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sqlx::{PgConnection, Row};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ReturnSource {
    pub order_id: String,
    pub reason: String,
    /// Return receipt-line ID -> original order-line ID. Same-SKU lines stay distinct.
    pub line_ids: BTreeMap<String, String>,
}
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ReturnLineInput {
    pub id: String,
    pub order_line_id: String,
    pub lot: String,
    pub quantity: i64,
}
pub fn prepare(
    state: &State,
    order_id: &str,
    reason: &str,
    lines: &[ReturnLineInput],
) -> crate::domain::Result<(Vec<ReceiptInput>, ReturnSource, String)> {
    let order = state
        .orders
        .iter()
        .find(|o| {
            o.id == order_id
                && ["shipped", "partially_shipped", "closed_partial"].contains(&o.status.as_str())
        })
        .ok_or("Returns require an original physically shipped order")?;
    if lines.is_empty() || lines.len() > 100 {
        return Err("A return requires 1–100 lines".into());
    }
    let mut requested = BTreeMap::<(&str, &str), i64>::new();
    let mut inputs = vec![];
    let mut owners = BTreeMap::new();
    for line in lines {
        if !(1..=1_000_000).contains(&line.quantity) {
            return Err("Return quantity must be 1–1,000,000 whole units".into());
        }
        let original = order
            .lines
            .iter()
            .find(|l| l.id == line.order_line_id)
            .ok_or("Return must identify the exact original order line")?;
        if owners
            .insert(line.id.clone(), line.order_line_id.clone())
            .is_some()
        {
            return Err("Return line IDs must be unique".into());
        }
        *requested
            .entry((&line.order_line_id, &line.lot))
            .or_default() += line.quantity;
        inputs.push(ReceiptInput {
            id: line.id.clone(),
            sku: original.sku.clone(),
            lot: line.lot.clone(),
            quantity: line.quantity,
        });
    }
    for ((line_id, lot), quantity) in requested {
        let shipped: i64 = order
            .allocations
            .iter()
            .filter(|a| a.line_id == line_id && a.lot == lot)
            .map(|a| a.shipped)
            .sum();
        let authorized: i64 = state
            .receipts
            .iter()
            .filter_map(|r| r.return_source.as_ref().map(|s| (r, s)))
            .filter(|(_, s)| s.order_id == order_id)
            .flat_map(|(r, s)| {
                r.lines
                    .iter()
                    .filter(move |l| {
                        l.lot == lot && s.line_ids.get(&l.id).is_some_and(|id| id == line_id)
                    })
                    .map(move |line| (r, line))
            })
            .map(|(receipt, line)| line.quantity - crate::receipts::cancelled(receipt, &line.id))
            .sum();
        if quantity + authorized > shipped {
            return Err("Return exceeds the original line/lot's remaining shipped quantity".into());
        }
    }
    Ok((
        inputs,
        ReturnSource {
            order_id: order_id.into(),
            reason: reason.into(),
            line_ids: owners,
        },
        order.reference.clone(),
    ))
}

/// Independently check source ownership whenever the original shipment is in
/// this domain snapshot. Persistence additionally checks all archived RMAs in
/// SQL; an operational snapshot need not load every historical order.
pub fn validate_loaded(state: &State) -> crate::domain::Result<()> {
    let mut authorized = BTreeMap::<(&str, &str, &str), i64>::new();
    for receipt in &state.receipts {
        let Some(source) = &receipt.return_source else {
            continue;
        };
        for line in &receipt.lines {
            let original_line = source
                .line_ids
                .get(&line.id)
                .ok_or("Return line lacks source ownership")?;
            if let Some(order) = state.orders.iter().find(|o| o.id == source.order_id) {
                if !["shipped", "partially_shipped", "closed_partial"]
                    .contains(&order.status.as_str())
                    || !order
                        .lines
                        .iter()
                        .any(|l| l.id == *original_line && l.sku == line.sku)
                {
                    return Err("Return does not match its exact shipped order line".into());
                }
            }
            *authorized
                .entry((&source.order_id, original_line, &line.lot))
                .or_default() += line.quantity - crate::receipts::cancelled(receipt, &line.id);
        }
    }
    for ((order_id, line_id, lot), quantity) in authorized {
        if let Some(order) = state.orders.iter().find(|o| o.id == order_id) {
            let shipped: i64 = order
                .allocations
                .iter()
                .filter(|a| a.line_id == line_id && a.lot == lot)
                .map(|a| a.shipped)
                .sum();
            if quantity > shipped {
                return Err("Authorized returns exceed shipped line/lot quantity".into());
            }
        }
    }
    Ok(())
}

pub async fn availability(
    c: &mut PgConnection,
    tenant: &str,
    warehouse: &str,
    order: &str,
) -> Result<Vec<Value>, Error> {
    crate::shipments::source(c, tenant, warehouse, order).await?;
    let rows = sqlx::query(include_str!("return_availability.sql"))
        .bind(tenant)
        .bind(warehouse)
        .bind(order)
        .fetch_all(&mut *c)
        .await?;
    rows.into_iter().map(|r| {
        let shipped: i64 = r.try_get("shipped")?;
        let authorized: i64 = r.try_get("authorized")?;
        Ok(json!({"order_line_id":r.try_get::<String,_>("line_id")?,"sku":r.try_get::<String,_>("sku")?,
            "lot":r.try_get::<String,_>("lot")?,"shipped":shipped,"authorized":authorized,"remaining":shipped-authorized}))
    }).collect()
}
pub async fn eligible(
    store: &Store,
    identity: &Identity,
    warehouse: &str,
    order: &str,
) -> Result<Value, Error> {
    let mut tx = store.pool.begin().await?;
    sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY")
        .execute(&mut *tx)
        .await?;
    let status: Option<String> = sqlx::query_scalar(
        "SELECT status FROM wms_orders WHERE tenant=$1 AND warehouse_id=$2 AND id=$3",
    )
    .bind(&identity.tenant)
    .bind(warehouse)
    .bind(order)
    .fetch_optional(&mut *tx)
    .await?;
    match status.as_deref() {
        None => return Err(Error::new(404, "Original order not found")),
        Some("shipped" | "partially_shipped" | "closed_partial") => {}
        _ => {
            return Err(Error::new(
                409,
                "Returns require an original physical handoff",
            ))
        }
    }
    let (_, shipment_source) =
        crate::shipments::source(&mut tx, &identity.tenant, warehouse, order).await?;
    let lines = availability(&mut tx, &identity.tenant, warehouse, order).await?;
    let version: i64 = sqlx::query_scalar(
        "SELECT version FROM wms_warehouses WHERE tenant=$1 AND warehouse_id=$2",
    )
    .bind(&identity.tenant)
    .bind(warehouse)
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(json!({"order_id":order,"version":version,"lines":lines,"shipment_source":shipment_source}))
}
