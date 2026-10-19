//! Bounded pick-face planning. Suggestions do not reserve or move inventory;
//! physical confirmation uses the normal journaled, versioned move command.
use crate::{
    api::App,
    concurrency,
    domain::{self, Command, State, Stock},
    store::{Error, Identity},
};
use axum::{
    extract::{Path, State as AppState},
    Extension, Json,
};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::BTreeSet;

#[derive(Debug, Clone, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Rule {
    pub sku: String,
    pub destination: String,
    pub minimum: i64,
    pub target: i64,
    pub source_zone: Option<String>,
}
#[derive(Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PlanRequest {
    pub rules: Vec<Rule>,
}

pub fn plan(state: &State, rules: &[Rule]) -> domain::Result<Value> {
    state.validate()?;
    if rules.is_empty() || rules.len() > 100 {
        return Err("Supply 1–100 replenishment rules".into());
    }
    let at = chrono::Utc::now();
    let usable = |stock: &Stock| {
        crate::lot_control::availability_at(state, &stock.sku, &stock.lot, at)
            == crate::lot_control::Availability::Available
    };
    let mut identities = BTreeSet::new();
    // Never drain another configured pick face, even for a different rule.
    let mut faces = BTreeSet::new();
    for rule in rules {
        if !state.products.iter().any(|p| p.sku == rule.sku)
            || !state
                .locations
                .iter()
                .any(|l| l.code == rule.destination && l.kind == "storage")
            || rule.minimum < 1
            || rule.target < rule.minimum
            || rule.target > 1_000_000
            || rule.source_zone.as_ref().is_some_and(|z| {
                z.is_empty()
                    || z.len() > 240
                    || !state
                        .locations
                        .iter()
                        .any(|l| l.kind == "storage" && l.zone == *z)
            })
            || !identities.insert((&rule.sku, &rule.destination))
        {
            return Err("Rules require an existing SKU/storage bin, a unique SKU/bin and 1 ≤ minimum ≤ target ≤ 1,000,000".into());
        }
        faces.insert(rule.destination.as_str());
    }
    let mut ordered: Vec<_> = rules.iter().collect();
    ordered.sort_by_key(|r| (&r.sku, &r.destination));
    let mut virtual_stock = state.stocks.clone();
    let mut tasks = vec![];
    let mut shortages = vec![];
    for rule in ordered {
        let available: i64 = virtual_stock
            .iter()
            .filter(|s| s.sku == rule.sku && s.location == rule.destination && usable(s))
            .map(|s| s.on_hand - s.reserved)
            .sum();
        if available >= rule.minimum {
            continue;
        }
        let needed = rule.target - available;
        let mut remaining = needed;
        let mut sources: Vec<_> = virtual_stock
            .iter()
            .enumerate()
            .filter(|(_, s)| {
                s.sku == rule.sku
                    && usable(s)
                    && s.on_hand > s.reserved
                    && !faces.contains(s.location.as_str())
                    && state.locations.iter().any(|l| {
                        l.code == s.location
                            && l.kind == "storage"
                            && rule.source_zone.as_ref().is_none_or(|z| l.zone == *z)
                    })
            })
            .map(|(i, s)| (i, s.location.clone(), s.lot.clone()))
            .collect();
        sources.sort_by_key(|a| {
            (
                crate::lot_control::fefo(state, &rule.sku, &a.2),
                a.1.clone(),
                a.2.clone(),
            )
        });
        for (index, from, lot) in sources {
            let destination = virtual_stock
                .iter()
                .position(|s| s.sku == rule.sku && s.lot == lot && s.location == rule.destination);
            let capacity = 1_000_000_000 - destination.map_or(0, |i| virtual_stock[i].on_hand);
            let source = &virtual_stock[index];
            let quantity = remaining
                .min(source.on_hand - source.reserved)
                .min(capacity);
            if quantity == 0 {
                continue;
            }
            virtual_stock[index].on_hand -= quantity;
            if let Some(i) = destination {
                virtual_stock[i].on_hand += quantity;
            } else {
                virtual_stock.push(Stock {
                    sku: rule.sku.clone(),
                    lot: lot.clone(),
                    location: rule.destination.clone(),
                    on_hand: quantity,
                    reserved: 0,
                });
            }
            remaining -= quantity;
            let command = Command::MoveStock {
                sku: rule.sku.clone(),
                lot,
                from,
                to: rule.destination.clone(),
                quantity,
            };
            tasks.push(json!({"command":command,"resource_versions":concurrency::reviewed_versions(state,&command)?,"quantity":quantity}));
            if tasks.len() > 500 {
                return Err("Plan exceeds 500 movements; narrow the selected rules".into());
            }
            if remaining == 0 {
                break;
            }
        }
        shortages.push(
            json!({"sku":rule.sku,"destination":rule.destination,"available":available,
            "needed":needed,"planned":needed-remaining,"unfilled":remaining}),
        );
    }
    Ok(
        json!({"schema_version":1,"warehouse_id":state.warehouse.id,"version":state.version,
        "generated_at":domain::now(),"tasks":tasks,"shortages":shortages}),
    )
}

pub async fn endpoint(
    AppState(app): AppState<App>,
    Extension(identity): Extension<Identity>,
    Path(warehouse): Path<String>,
    Json(input): Json<PlanRequest>,
) -> Result<Json<Value>, Error> {
    if !["operator", "admin"].contains(&identity.role.as_str()) {
        return Err(Error::new(
            403,
            "Replenishment planning requires an operator",
        ));
    }
    let state = app.store.state(&identity.tenant, &warehouse).await?;
    state.validate().map_err(|_| {
        Error::new(
            503,
            "Warehouse inventory requires reconciliation; replenishment held",
        )
    })?;
    Ok(Json(
        plan(&state, &input.rules).map_err(|e| Error::new(422, e))?,
    ))
}
