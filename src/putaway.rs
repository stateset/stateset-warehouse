//! Directed putaway: suggest a storage bin for stock waiting in receiving.
//! Suggestions reserve and move nothing; each task is an ordinary reviewed
//! `move_stock` command that an operator confirms through the command journal.
//! There is no bin capacity model, so suggestions never claim physical fit.
use crate::{
    api::App,
    concurrency,
    domain::{self, Command, State},
    store::{Error, Identity},
};
use axum::{
    extract::{Path, State as AppState},
    Extension, Json,
};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};

/// Most tasks in one plan.
pub const MAX_TASKS: usize = 500;

#[derive(Debug, Clone, Default, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PlanRequest {
    /// Only plan these receiving bins; default: every receiving bin.
    #[serde(default)]
    pub from: Vec<String>,
    /// Only plan these SKUs; default: every SKU waiting in receiving.
    #[serde(default)]
    pub skus: Vec<String>,
    /// Prefer storage bins in this zone when choosing an empty bin.
    #[serde(default)]
    pub zone: Option<String>,
    /// Never suggest these storage bins.
    #[serde(default)]
    pub exclude: Vec<String>,
    /// When no same-SKU or empty bin exists, allow a bin that already holds
    /// other SKUs, preferring the fewest (default true).
    #[serde(default)]
    pub allow_mixed: Option<bool>,
}

/// Why a destination was chosen, strongest first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Reason {
    SameLot,
    SameSku,
    PreferredZone,
    SkuZone,
    Empty,
    Mixed,
}
impl Reason {
    fn as_str(self) -> &'static str {
        match self {
            Reason::SameLot => "consolidate_same_lot",
            Reason::SameSku => "consolidate_same_sku",
            Reason::SkuZone => "empty_bin_in_sku_zone",
            Reason::PreferredZone => "empty_bin_in_requested_zone",
            Reason::Empty => "empty_bin",
            Reason::Mixed => "mixed_sku_bin",
        }
    }
}

pub fn plan(state: &State, request: &PlanRequest) -> domain::Result<Value> {
    state.validate()?;
    let receiving: BTreeSet<&str> = state
        .locations
        .iter()
        .filter(|l| l.kind == "receiving")
        .map(|l| l.code.as_str())
        .collect();
    for bin in &request.from {
        if !receiving.contains(bin.as_str()) {
            return Err(format!("{bin} is not a receiving bin"));
        }
    }
    for sku in &request.skus {
        if !state.products.iter().any(|p| p.sku == *sku) {
            return Err(format!("Product {sku} not found"));
        }
    }
    if let Some(zone) = &request.zone {
        if !state
            .locations
            .iter()
            .any(|l| l.kind == "storage" && l.zone == *zone)
        {
            return Err(format!("No storage bins in zone {zone}"));
        }
    }
    if request.from.len() > 100 || request.skus.len() > 100 || request.exclude.len() > 1000 {
        return Err("Select at most 100 bins, 100 SKUs and 1,000 exclusions".into());
    }
    let exclude: BTreeSet<&str> = request.exclude.iter().map(String::as_str).collect();
    let storage: Vec<_> = state
        .locations
        .iter()
        .filter(|l| l.kind == "storage" && !exclude.contains(l.code.as_str()))
        .collect();
    // Stock already planned into a bin by this plan counts as occupying it.
    let mut occupied: BTreeMap<&str, BTreeSet<(String, String)>> = BTreeMap::new();
    for s in &state.stocks {
        if s.on_hand > 0 {
            occupied
                .entry(s.location.as_str())
                .or_default()
                .insert((s.sku.clone(), s.lot.clone()));
        }
    }
    let mut sources: Vec<_> = state
        .stocks
        .iter()
        .filter(|s| {
            receiving.contains(s.location.as_str())
                && s.on_hand > s.reserved
                && (request.from.is_empty() || request.from.contains(&s.location))
                && (request.skus.is_empty() || request.skus.contains(&s.sku))
        })
        .collect();
    sources.sort_by(|a, b| (&a.location, &a.sku, &a.lot).cmp(&(&b.location, &b.sku, &b.lot)));
    let mut tasks = vec![];
    let mut unplaced = vec![];
    for stock in sources {
        let quantity = stock.on_hand - stock.reserved;
        let sku_zones: BTreeSet<&str> = storage
            .iter()
            .filter(|l| {
                occupied
                    .get(l.code.as_str())
                    .is_some_and(|items| items.iter().any(|(sku, _)| *sku == stock.sku))
            })
            .map(|l| l.zone.as_str())
            .collect();
        let allow_mixed = request.allow_mixed.unwrap_or(true);
        let mut best: Option<(Reason, usize, crate::batch_pick::NaturalKey, &str)> = None;
        for bin in &storage {
            let items = occupied.get(bin.code.as_str());
            let mut skus = 0;
            let reason = match items {
                Some(items) if items.contains(&(stock.sku.clone(), stock.lot.clone())) => {
                    Reason::SameLot
                }
                Some(items) if items.iter().any(|(sku, _)| *sku == stock.sku) => Reason::SameSku,
                Some(items) if !items.is_empty() => {
                    if !allow_mixed {
                        continue;
                    }
                    skus = items
                        .iter()
                        .map(|(sku, _)| sku)
                        .collect::<BTreeSet<_>>()
                        .len();
                    Reason::Mixed
                }
                _ if request.zone.as_deref() == Some(bin.zone.as_str()) => Reason::PreferredZone,
                _ if sku_zones.contains(bin.zone.as_str()) => Reason::SkuZone,
                _ => Reason::Empty,
            };
            // Strongest reason first; for mixed bins the fewest SKUs; then the
            // first bin in natural walk order (A-2 before A-10), as in batch
            // picking.
            let key = (
                reason,
                skus,
                crate::batch_pick::natural_key(&bin.code),
                bin.code.as_str(),
            );
            if best.as_ref().is_none_or(|b| key < *b) {
                best = Some(key);
            }
        }
        let Some((reason, _, _, to)) = best else {
            unplaced.push(json!({"sku":stock.sku,"lot":stock.lot,"from":stock.location,"quantity":quantity,"reason":"no_eligible_storage_bin"}));
            continue;
        };
        occupied
            .entry(to)
            .or_default()
            .insert((stock.sku.clone(), stock.lot.clone()));
        let command = Command::MoveStock {
            sku: stock.sku.clone(),
            lot: stock.lot.clone(),
            from: stock.location.clone(),
            to: to.to_string(),
            quantity,
        };
        let serial = state
            .products
            .iter()
            .any(|p| p.sku == stock.sku && p.serial_controlled);
        tasks.push(json!({
            "command": command,
            "resource_versions": concurrency::reviewed_versions(state, &command)?,
            "reason": reason.as_str(),
            "zone": storage.iter().find(|l| l.code == to).map(|l| l.zone.clone()),
            "serial_labels_required": serial,
        }));
        if tasks.len() > MAX_TASKS {
            return Err("Plan exceeds 500 movements; select fewer bins or SKUs".into());
        }
    }
    Ok(json!({
        "schema_version": 1,
        "warehouse_id": state.warehouse.id,
        "version": state.version,
        "generated_at": domain::now(),
        "tasks": tasks,
        "unplaced": unplaced,
        "limits": ["No bin capacity model: confirm physical fit before moving",
                   "mixed_sku_bin tasks put the stock beside other SKUs; set allow_mixed to false to avoid them",
                   "Suggestions reserve nothing; each task is a reviewed move_stock command"],
    }))
}

pub async fn endpoint(
    AppState(app): AppState<App>,
    Extension(identity): Extension<Identity>,
    Path(warehouse): Path<String>,
    Json(input): Json<PlanRequest>,
) -> Result<Json<Value>, Error> {
    if !["operator", "admin"].contains(&identity.role.as_str()) {
        return Err(Error::new(403, "Putaway planning requires an operator"));
    }
    let state = app.store.state(&identity.tenant, &warehouse).await?;
    state.validate().map_err(|_| {
        Error::new(
            503,
            "Warehouse inventory requires reconciliation; putaway held",
        )
    })?;
    Ok(Json(plan(&state, &input).map_err(|e| Error::new(422, e))?))
}
