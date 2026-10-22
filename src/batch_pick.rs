//! Batch (cluster) picking: pick several orders in one walk through the
//! warehouse. The plan groups every remaining reserved pick by bin, sorts the
//! stops along the bin path and assigns each order a tote. It reserves and
//! moves nothing: every pick is the order line's ordinary `pick` command,
//! confirmed one at a time through the command journal.
use crate::{
    api::App,
    concurrency,
    domain::{self, Command, Order, State},
    lot_control::{self, Availability},
    store::{Error, Identity},
};
use axum::{
    extract::{Path, State as AppState},
    Extension, Json,
};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::BTreeMap;

pub const DEFAULT_ORDERS: usize = 20;
pub const MAX_ORDERS: usize = 50;
pub const MAX_PICKS: usize = 500;

#[derive(Debug, Clone, Default, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PlanRequest {
    /// Exact orders to batch, in tote order. Default: the most urgent
    /// pickable orders, oldest first.
    #[serde(default)]
    pub order_ids: Vec<String>,
    /// Orders to select when `order_ids` is empty (1–50, default 20).
    #[serde(default)]
    pub max_orders: Option<usize>,
    /// Only pick from bins in this zone.
    #[serde(default)]
    pub zone: Option<String>,
}

fn priority_rank(priority: &str) -> u8 {
    match priority {
        "urgent" => 0,
        "high" => 1,
        _ => 2,
    }
}

/// Bin codes compare with embedded numbers as numbers: A-2 before A-10.
pub fn natural_key(code: &str) -> Vec<(u8, u64, String)> {
    let mut parts = vec![];
    let mut chars = code.chars().peekable();
    while let Some(&c) = chars.peek() {
        let mut run = String::new();
        let digits = c.is_ascii_digit();
        while let Some(&c) = chars.peek() {
            if c.is_ascii_digit() != digits {
                break;
            }
            run.push(c);
            chars.next();
        }
        parts.push(if digits {
            (0, run.parse().unwrap_or(u64::MAX), run)
        } else {
            (1, 0, run.to_lowercase())
        });
    }
    parts
}

/// Why an order in the request cannot be picked now.
fn hold(state: &State, order: &Order) -> Option<&'static str> {
    if state
        .exceptions
        .iter()
        .any(|e| e.status == "open" && e.order_id.as_deref() == Some(order.id.as_str()))
    {
        return Some("blocking_exception");
    }
    let pickable_status = if order.shipment_mode.is_some() {
        !["cancelled", "shipped", "closed_partial"].contains(&order.status.as_str())
    } else {
        ["allocated", "picking"].contains(&order.status.as_str())
    };
    if !pickable_status {
        return Some("not_ready_to_pick");
    }
    if !order
        .allocations
        .iter()
        .any(|a| a.quantity - a.picked - a.released > 0)
    {
        return Some("nothing_left_to_pick");
    }
    None
}

pub fn plan(state: &State, request: &PlanRequest) -> domain::Result<Value> {
    state.validate()?;
    if request.order_ids.len() > MAX_ORDERS {
        return Err(format!("Batch at most {MAX_ORDERS} orders"));
    }
    let limit = request.max_orders.unwrap_or(DEFAULT_ORDERS);
    if !(1..=MAX_ORDERS).contains(&limit) {
        return Err(format!("max_orders must be 1–{MAX_ORDERS}"));
    }
    if let Some(zone) = &request.zone {
        if !state.locations.iter().any(|l| l.zone == *zone) {
            return Err(format!("No bins in zone {zone}"));
        }
    }
    let mut held = vec![];
    let selected: Vec<&Order> = if request.order_ids.is_empty() {
        let mut candidates: Vec<&Order> = state
            .orders
            .iter()
            .filter(|o| hold(state, o).is_none())
            .collect();
        candidates.sort_by(|a, b| {
            (priority_rank(&a.priority), &a.created_at, &a.id).cmp(&(
                priority_rank(&b.priority),
                &b.created_at,
                &b.id,
            ))
        });
        candidates.into_iter().take(limit).collect()
    } else {
        let mut seen = std::collections::BTreeSet::new();
        let mut chosen = vec![];
        for id in &request.order_ids {
            if !seen.insert(id) {
                return Err(format!("Order {id} is listed twice"));
            }
            let order = state
                .orders
                .iter()
                .find(|o| o.id == *id)
                .ok_or_else(|| format!("Order {id} not found"))?;
            match hold(state, order) {
                Some(reason) => held.push(json!({"order_id":id,"reason":reason})),
                None => chosen.push(order),
            }
        }
        chosen
    };
    let at = chrono::Utc::now();
    let zone_of = |code: &str| {
        state
            .locations
            .iter()
            .find(|l| l.code == code)
            .map(|l| l.zone.clone())
            .unwrap_or_default()
    };
    // (zone, natural bin, sku, lot) -> picks at that stop.
    type StopKey = (String, Vec<(u8, u64, String)>, String, String, String);
    let mut stops: BTreeMap<StopKey, Vec<Value>> = BTreeMap::new();
    let mut unavailable = vec![];
    let mut picks = 0;
    let mut totes = vec![];
    for (index, order) in selected.iter().enumerate() {
        let tote = index + 1;
        totes.push(
            json!({"tote":tote,"order_id":order.id,"reference":order.reference,
            "priority":order.priority,"customer":order.customer}),
        );
        for a in &order.allocations {
            let remaining = a.quantity - a.picked - a.released;
            if remaining <= 0 {
                continue;
            }
            let zone = zone_of(&a.location);
            if request.zone.as_ref().is_some_and(|z| *z != zone) {
                continue;
            }
            let availability = lot_control::availability_at(state, &a.sku, &a.lot, at);
            if availability != Availability::Available {
                unavailable.push(json!({"order_id":order.id,"line_id":a.line_id,"sku":a.sku,
                    "lot":a.lot,"location":a.location,"quantity":remaining,
                    "reason":format!("{availability:?}").to_lowercase()}));
                continue;
            }
            let command = Command::Pick {
                order_id: order.id.clone(),
                line_id: a.line_id.clone(),
                location: a.location.clone(),
                lot: a.lot.clone(),
                quantity: remaining,
            };
            stops
                .entry((
                    zone,
                    natural_key(&a.location),
                    a.location.clone(),
                    a.sku.clone(),
                    a.lot.clone(),
                ))
                .or_default()
                .push(json!({
                    "tote": tote,
                    "order_id": order.id,
                    "line_id": a.line_id,
                    "quantity": remaining,
                    "command": command,
                    "resource_versions": concurrency::reviewed_versions(state, &command)?,
                }));
            picks += 1;
            if picks > MAX_PICKS {
                return Err(format!(
                    "Batch exceeds {MAX_PICKS} picks; select fewer orders"
                ));
            }
        }
    }
    let stops: Vec<Value> = stops
        .into_iter()
        .enumerate()
        .map(|(i, ((zone, _, location, sku, lot), picks))| {
            let total: i64 = picks.iter().filter_map(|p| p["quantity"].as_i64()).sum();
            let serial = state
                .products
                .iter()
                .any(|p| p.sku == sku && p.serial_controlled);
            json!({"sequence":i+1,"zone":zone,"location":location,"sku":sku,"lot":lot,
                "total":total,"serial_labels_required":serial,"picks":picks})
        })
        .collect();
    Ok(json!({
        "schema_version": 1,
        "warehouse_id": state.warehouse.id,
        "version": state.version,
        "generated_at": domain::now(),
        "totes": totes,
        "stops": stops,
        "pick_count": picks,
        "held_orders": held,
        "unavailable": unavailable,
        "limits": [
            "Picks reserve nothing new; each is the line's reviewed pick command",
            "Picks that share a bin, SKU and lot advance the same resource version: refresh the review after each committed pick at that stop",
        ],
    }))
}

pub async fn endpoint(
    AppState(app): AppState<App>,
    Extension(identity): Extension<Identity>,
    Path(warehouse): Path<String>,
    Json(input): Json<PlanRequest>,
) -> Result<Json<Value>, Error> {
    if !["operator", "admin"].contains(&identity.role.as_str()) {
        return Err(Error::new(403, "Batch pick planning requires an operator"));
    }
    let state = app.store.state(&identity.tenant, &warehouse).await?;
    state.validate().map_err(|_| {
        Error::new(
            503,
            "Warehouse inventory requires reconciliation; batch picking held",
        )
    })?;
    Ok(Json(plan(&state, &input).map_err(|e| Error::new(422, e))?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bin_codes_sort_numbers_numerically() {
        let mut bins = vec!["A-10", "A-2", "B-1", "A-1", "a-3"];
        bins.sort_by_key(|b| natural_key(b));
        assert_eq!(bins, vec!["A-1", "A-2", "a-3", "A-10", "B-1"]);
    }

    #[test]
    fn urgent_orders_rank_first() {
        assert!(priority_rank("urgent") < priority_rank("high"));
        assert!(priority_rank("high") < priority_rank("normal"));
    }
}
