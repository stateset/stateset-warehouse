//! Close an unshipped commercial remainder without changing original demand or
//! inventing a physical return. All staged units must already be recovered.
use crate::domain::{self, Order, State};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct CancelledLine {
    pub sku: String,
    pub ordered: i64,
    pub shipped: i64,
    pub cancelled: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Cancellation {
    pub reason: String,
    pub actor: String,
    pub cancelled_at: String,
    pub lines: BTreeMap<String, CancelledLine>,
}

fn text(value: &str) -> bool {
    !value.trim().is_empty()
        && value.trim() == value
        && value.len() <= 240
        && !value.chars().any(char::is_control)
}

pub(crate) fn cancel(
    state: &mut State,
    order_id: &str,
    reason: &str,
    actor: &str,
) -> domain::Result<String> {
    if !text(reason) || !text(actor) {
        return Err(
            "Cancellation requires an attributable actor and a 1–240 character reason".into(),
        );
    }
    let index = state
        .orders
        .iter()
        .position(|o| o.id == order_id)
        .ok_or("Order not found")?;
    let order = &state.orders[index];
    if order.shipment_mode.as_deref() != Some("packages")
        || order.status != "partially_shipped"
        || order.remainder_cancellation.is_some()
    {
        return Err("Remainder cancellation requires an open partly shipped package order".into());
    }
    let context = state
        .shipment_context
        .as_ref()
        .filter(|c| c.ok && c.order_id == order_id)
        .ok_or("Verified original shipment ownership is required before closing the remainder")?;
    if context.packages.iter().any(|p| p.closure.is_none())
        || order.allocations.iter().any(|a| a.picked != a.shipped)
    {
        return Err("Unpack and physically return every unshipped staged unit before cancelling the remainder".into());
    }
    let lines = order
        .lines
        .iter()
        .map(|line| {
            let shipped: i64 = order
                .allocations
                .iter()
                .filter(|a| a.line_id == line.id)
                .map(|a| a.shipped)
                .sum();
            (
                line.id.clone(),
                CancelledLine {
                    sku: line.sku.clone(),
                    ordered: line.quantity,
                    shipped,
                    cancelled: line.quantity - shipped,
                },
            )
        })
        .collect();
    let cancellation = Cancellation {
        reason: reason.into(),
        actor: actor.into(),
        cancelled_at: domain::now(),
        lines,
    };
    // Cancellation releases only units still in their original storage bins.
    // No ledger movement or change to original shipped counters occurs here.
    for allocation in state.orders[index].allocations.clone() {
        state
            .bucket(&allocation.sku, &allocation.lot, &allocation.location)
            .reserved -= allocation.quantity - allocation.released - allocation.shipped;
    }
    for allocation in &mut state.orders[index].allocations {
        allocation.released = allocation.quantity - allocation.shipped;
    }
    state.orders[index].remainder_cancellation = Some(cancellation);
    state.orders[index].status = "closed_partial".into();
    validate(&state.orders[index])?;
    Ok(order_id.into())
}

pub fn validate(order: &Order) -> domain::Result<()> {
    let Some(cancellation) = &order.remainder_cancellation else {
        if order.status == "closed_partial" {
            return Err(
                "Closed partial fulfillment requires original remainder-cancellation evidence"
                    .into(),
            );
        }
        return Ok(());
    };
    if order.shipment_mode.as_deref() != Some("packages")
        || order.status != "closed_partial"
        || !text(&cancellation.reason)
        || !text(&cancellation.actor)
        || chrono::DateTime::parse_from_rfc3339(&cancellation.cancelled_at).is_err()
        || cancellation.lines.len() != order.lines.len()
        || !cancellation.lines.values().any(|l| l.shipped > 0)
        || !cancellation.lines.values().any(|l| l.cancelled > 0)
        || order
            .allocations
            .iter()
            .any(|a| a.picked != a.shipped || a.released != a.quantity - a.shipped)
    {
        return Err("Invalid partial-order remainder cancellation evidence".into());
    }
    for line in &order.lines {
        let original = cancellation
            .lines
            .get(&line.id)
            .ok_or("Cancellation must retain every original order line")?;
        let shipped: i64 = order
            .allocations
            .iter()
            .filter(|a| a.line_id == line.id)
            .map(|a| a.shipped)
            .sum();
        if original.sku != line.sku
            || original.ordered != line.quantity
            || original.shipped != shipped
            || original.cancelled != line.quantity - shipped
            || original.shipped < 0
            || original.cancelled < 0
        {
            return Err(
                "Cancelled demand disagrees with its exact original line and actual shipped units"
                    .into(),
            );
        }
    }
    Ok(())
}

pub fn cancelled(order: &Order, line_id: &str) -> i64 {
    if order.status == "cancelled" {
        return order
            .lines
            .iter()
            .find(|line| line.id == line_id)
            .map_or(0, |line| line.quantity);
    }
    order
        .remainder_cancellation
        .as_ref()
        .and_then(|c| c.lines.get(line_id))
        .map_or(0, |l| l.cancelled)
}

pub(crate) async fn load(
    c: &mut sqlx::PgConnection,
    tenant: &str,
    warehouse: &str,
    orders: &mut [Order],
) -> crate::store::Result<()> {
    use sqlx::Row;
    let ids: Vec<_> = orders.iter().map(|o| o.id.clone()).collect();
    let rows=sqlx::query("SELECT order_id,wms_order_cancellation_document($1,$2,order_id) AS document FROM wms_order_cancellations WHERE tenant=$1 AND warehouse_id=$2 AND order_id=ANY($3)")
        .bind(tenant).bind(warehouse).bind(ids).fetch_all(c).await?;
    let mut cancellations = BTreeMap::new();
    for row in rows {
        cancellations.insert(
            row.try_get::<String, _>("order_id")?,
            crate::persistence::decode(row.try_get("document")?)?,
        );
    }
    for order in orders {
        order.remainder_cancellation = cancellations.remove(&order.id);
    }
    Ok(())
}

pub(crate) async fn persist(
    c: &mut sqlx::PgConnection,
    tenant: &str,
    warehouse: &str,
    old: &State,
    next: &State,
    event_id: &str,
) -> crate::store::Result<()> {
    for order in &next.orders {
        let Some(cancellation) = &order.remainder_cancellation else {
            continue;
        };
        if old
            .orders
            .iter()
            .any(|o| o.id == order.id && o.remainder_cancellation.is_some())
        {
            continue;
        }
        sqlx::query("INSERT INTO wms_order_cancellations(tenant,warehouse_id,order_id,reason,actor,cancelled_at,source_event_id,source_version,document) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9)")
            .bind(tenant).bind(warehouse).bind(&order.id).bind(&cancellation.reason).bind(&cancellation.actor)
            .bind(&cancellation.cancelled_at).bind(event_id).bind(next.version).bind(serde_json::json!(cancellation))
            .execute(&mut *c).await?;
        for (id, line) in &cancellation.lines {
            sqlx::query("INSERT INTO wms_order_cancelled_lines(tenant,warehouse_id,order_id,line_id,sku,ordered,shipped,cancelled) VALUES($1,$2,$3,$4,$5,$6,$7,$8)")
                .bind(tenant).bind(warehouse).bind(&order.id).bind(id).bind(&line.sku).bind(line.ordered)
                .bind(line.shipped).bind(line.cancelled).execute(&mut *c).await?;
        }
    }
    Ok(())
}
