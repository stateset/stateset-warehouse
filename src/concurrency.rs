//! Exact resources reviewed by a physical command. Versions are database-owned,
//! monotonic counters: changing a balance and changing it back still fences scans.
use crate::domain::{Command, Result, State};
use std::collections::{BTreeMap, BTreeSet};

pub fn key(parts: &[&str]) -> String {
    // JSON arrays avoid collisions when warehouse identifiers contain separators.
    serde_json::to_string(parts).expect("string array serialization")
}
pub fn stock(sku: &str, lot: &str, location: &str) -> String {
    key(&["stock", sku, lot, location])
}
pub fn visible_keys(state: &State) -> Vec<String> {
    let mut keys = BTreeSet::new();
    for s in &state.stocks {
        keys.insert(stock(&s.sku, &s.lot, &s.location));
        keys.insert(key(&["lot", &s.sku, &s.lot]));
    }
    for lot in &state.lot_controls {
        keys.insert(key(&["lot", &lot.sku, &lot.lot]));
    }
    for p in &state.products {
        keys.insert(key(&["inventory", &p.sku]));
        keys.insert(key(&["product", &p.sku]));
    }
    for location in &state.locations {
        keys.insert(key(&["location", &location.code]));
    }
    for o in &state.orders {
        keys.insert(key(&["order", &o.id]));
    }
    for r in &state.receipts {
        keys.insert(key(&["receipt", &r.id]));
    }
    keys.into_iter().collect()
}

/// None means the command retains the original warehouse-wide version fence.
/// The caller cannot omit a resource or choose which dependencies to enforce.
pub fn reviewed_versions(
    state: &State,
    command: &Command,
) -> Result<Option<BTreeMap<String, i64>>> {
    let mut keys = BTreeSet::new();
    match command {
        Command::MoveStock {
            sku, lot, from, to, ..
        } => {
            keys.insert(stock(sku, lot, from));
            keys.insert(stock(sku, lot, to));
        }
        Command::CountStock {
            sku, lot, location, ..
        } => {
            keys.insert(stock(sku, lot, location));
        }
        Command::Receive {
            receipt_id,
            line_id,
            location,
            ..
        } => {
            let receipt = state
                .receipts
                .iter()
                .find(|r| r.id == *receipt_id)
                .ok_or("Receipt not found")?;
            let line = receipt
                .lines
                .iter()
                .find(|l| l.id == *line_id)
                .ok_or("Receipt line not found")?;
            keys.insert(key(&["receipt", receipt_id]));
            keys.insert(stock(&line.sku, &line.lot, location));
        }
        Command::Allocate { order_id }
        | Command::Pack { order_id }
        | Command::Ship { order_id, .. }
        | Command::CancelOrder { order_id, .. }
        | Command::ResetAllocation { order_id, .. }
        | Command::Pick { order_id, .. }
        | Command::Unpick { order_id, .. } => {
            let order = state
                .orders
                .iter()
                .find(|o| o.id == *order_id)
                .ok_or("Order not found")?;
            keys.insert(key(&["order", order_id]));
            if matches!(
                command,
                Command::Pick { .. } | Command::Pack { .. } | Command::Ship { .. }
            ) {
                for a in &order.allocations {
                    let selected = match command {
                        Command::Pick {
                            line_id,
                            location,
                            lot,
                            ..
                        } => a.line_id == *line_id && a.location == *location && a.lot == *lot,
                        _ => true,
                    };
                    if selected {
                        keys.insert(key(&["lot", &a.sku, &a.lot]));
                        keys.insert(key(&["product", &a.sku]));
                    }
                }
            }
            match command {
                Command::Allocate { .. } => {
                    // SKU-wide counters also fence newly created bin/lot buckets.
                    for line in &order.lines {
                        keys.insert(key(&["inventory", &line.sku]));
                    }
                }
                Command::Pack { .. } => {}
                Command::Pick {
                    line_id,
                    location,
                    lot,
                    ..
                }
                | Command::Unpick {
                    line_id,
                    location,
                    lot,
                    ..
                } => {
                    let allocation = order
                        .allocations
                        .iter()
                        .find(|a| a.line_id == *line_id && a.location == *location && a.lot == *lot)
                        .ok_or("Allocation not found")?;
                    keys.insert(stock(&allocation.sku, lot, location));
                    keys.insert(stock(&allocation.sku, lot, "STAGE-01"));
                }
                _ => {
                    for a in &order.allocations {
                        keys.insert(stock(&a.sku, &a.lot, &a.location));
                        keys.insert(stock(&a.sku, &a.lot, "STAGE-01"));
                    }
                }
            }
        }
        _ => return Ok(None),
    }
    // Metadata creation must also fence a previously rejected physical command.
    // In particular, a missing destination cannot become valid under an old scan.
    let stock_keys: Vec<_> = keys.iter().cloned().collect();
    for resource in stock_keys {
        let parts: Vec<String> = serde_json::from_str(&resource).expect("resource key");
        if parts[0] == "stock" {
            keys.insert(key(&["product", &parts[1]]));
            keys.insert(key(&["location", &parts[3]]));
        }
    }
    Ok(Some(
        keys.into_iter()
            .map(|k| {
                let version = state.resource_versions.get(&k).copied().unwrap_or(0);
                (k, version)
            })
            .collect(),
    ))
}
