//! Custody equations: observed serial changes must reproduce the exact
//! before/after stock, staged ownership and package counts.
use super::*;

fn add<K: Ord>(map: &mut BTreeMap<K, i64>, key: K, quantity: i64) -> Result<()> {
    let value = map.entry(key).or_default();
    *value = value
        .checked_add(quantity)
        .ok_or("Serial custody count exceeds the exact range")?;
    Ok(())
}
fn stock(state: &State, tracked: &BTreeSet<String>) -> BTreeMap<StockKey, i64> {
    state
        .stocks
        .iter()
        .filter(|s| tracked.contains(&s.sku))
        .map(|s| {
            (
                (
                    state.warehouse.id.clone(),
                    s.sku.clone(),
                    s.lot.clone(),
                    s.location.clone(),
                ),
                s.on_hand,
            )
        })
        .collect()
}
fn staged(state: &State, tracked: &BTreeSet<String>) -> Result<BTreeMap<AllocationKey, i64>> {
    let mut counts = BTreeMap::new();
    for order in &state.orders {
        for a in &order.allocations {
            if tracked.contains(&a.sku) {
                add(
                    &mut counts,
                    (
                        state.warehouse.id.clone(),
                        order.id.clone(),
                        a.line_id.clone(),
                        a.location.clone(),
                        a.sku.clone(),
                        a.lot.clone(),
                    ),
                    a.picked - a.shipped,
                )?;
            }
        }
    }
    Ok(counts)
}
fn packed(state: &State, tracked: &BTreeSet<String>) -> Result<BTreeMap<PackageKey, i64>> {
    // The canonical transition already updates its loaded package context.
    // shipment_change is the persistence delta, not another business effect.
    let packages = state
        .shipment_context
        .as_ref()
        .map(|c| c.packages.as_slice())
        .unwrap_or_default();
    let mut counts = BTreeMap::new();
    for package in packages.iter().filter(|p| p.closure.is_none()) {
        for line in &package.lines {
            if tracked.contains(&line.sku) {
                add(
                    &mut counts,
                    (
                        state.warehouse.id.clone(),
                        package.id.clone(),
                        line.line_id.clone(),
                        line.location.clone(),
                        line.sku.clone(),
                        line.lot.clone(),
                    ),
                    line.quantity,
                )?;
            }
        }
    }
    Ok(counts)
}
fn equal_counts<K: Ord>(left: &BTreeMap<K, i64>, right: &BTreeMap<K, i64>) -> bool {
    left.iter()
        .all(|(key, value)| *value >= 0 && right.get(key).copied().unwrap_or(0) == *value)
        && right
            .iter()
            .all(|(key, value)| *value >= 0 && left.get(key).copied().unwrap_or(0) == *value)
}
pub(super) fn check_equations(
    before: &State,
    after: &State,
    context: &Context,
    changes: &[Change],
) -> Result<()> {
    let w = before.warehouse.id.as_str();
    let orders: BTreeSet<_> = before
        .orders
        .iter()
        .chain(&after.orders)
        .map(|o| o.id.as_str())
        .collect();
    let package_scope: BTreeSet<_> = before
        .shipment_context
        .iter()
        .flat_map(|c| c.packages.iter())
        .chain(
            after
                .shipment_context
                .iter()
                .flat_map(|c| c.packages.iter()),
        )
        .map(|p| p.id.as_str())
        .collect();
    let mut physical: BTreeMap<_, _> = context
        .physical
        .iter()
        .filter(|(k, _)| k.0 == w && context.tracked_skus.contains(&k.1))
        .map(|(k, v)| (k.clone(), *v))
        .collect();
    let mut ownership: BTreeMap<_, _> = context
        .staged
        .iter()
        .filter(|(k, _)| k.0 == w && orders.contains(k.1.as_str()))
        .map(|(k, v)| (k.clone(), *v))
        .collect();
    let mut packing: BTreeMap<_, _> = context
        .packed
        .iter()
        .filter(|(k, _)| k.0 == w && package_scope.contains(k.1.as_str()))
        .map(|(k, v)| (k.clone(), *v))
        .collect();
    require(
        equal_counts(&physical, &stock(before, &context.tracked_skus)),
        "Current serial custody disagrees with original physical stock",
    )?;
    require(
        equal_counts(&ownership, &staged(before, &context.tracked_skus)?),
        "Current staged serials disagree with exact original allocation owners",
    )?;
    require(
        equal_counts(&packing, &packed(before, &context.tracked_skus)?),
        "Current serial package claims disagree with original active packing",
    )?;
    for change in changes {
        for (place, delta) in [(change.before.as_ref(), -1), (Some(&change.after), 1)] {
            let Some(place) = place else { continue };
            if let Some(key) = place
                .physical(&change.sku, &change.lot)
                .filter(|k| k.0 == w)
            {
                add(&mut physical, key, delta)?;
            }
            if let Some(key) = place.staged(&change.sku, &change.lot).filter(|k| k.0 == w) {
                add(&mut ownership, key, delta)?;
            }
            if let Some(key) = place.packed(&change.sku, &change.lot).filter(|k| k.0 == w) {
                add(&mut packing, key, delta)?;
            }
        }
    }
    require(
        equal_counts(&physical, &stock(after, &context.tracked_skus)),
        "Observed serial changes do not equal the canonical physical stock transition",
    )?;
    require(
        equal_counts(&ownership, &staged(after, &context.tracked_skus)?),
        "Observed serial changes do not equal exact staged line/bin ownership",
    )?;
    require(
        equal_counts(&packing, &packed(after, &context.tracked_skus)?),
        "Observed serial changes do not equal original package ownership",
    )?;
    Ok(())
}
