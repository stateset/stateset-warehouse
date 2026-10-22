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

/// Saved rules for the warehouse, in SKU/bin order.
async fn saved_rules(app: &App, tenant: &str, warehouse: &str) -> Result<Vec<Rule>, Error> {
    Ok(
        sqlx::query_as::<_, (String, String, i64, i64, Option<String>)>(
            "SELECT sku,destination,minimum,target,source_zone FROM wms_replenishment_rules
         WHERE tenant=$1 AND warehouse_id=$2 ORDER BY sku,destination",
        )
        .bind(tenant)
        .bind(warehouse)
        .fetch_all(&app.store.pool)
        .await?
        .into_iter()
        .map(|(sku, destination, minimum, target, source_zone)| Rule {
            sku,
            destination,
            minimum,
            target,
            source_zone,
        })
        .collect(),
    )
}

pub async fn rules(
    AppState(app): AppState<App>,
    Extension(identity): Extension<Identity>,
    Path(warehouse): Path<String>,
) -> Result<Json<Value>, Error> {
    if !["viewer", "operator", "admin"].contains(&identity.role.as_str()) {
        return Err(Error::new(
            403,
            "Reading replenishment rules requires a warehouse role",
        ));
    }
    let rules = saved_rules(&app, &identity.tenant, &warehouse).await?;
    Ok(Json(
        json!({"warehouse_id":warehouse,"rules":rules.iter().map(|r| json!({
        "sku":r.sku,"destination":r.destination,"minimum":r.minimum,"target":r.target,"source_zone":r.source_zone
    })).collect::<Vec<_>>()}),
    ))
}

/// Replace the warehouse's saved rules as one audited change. An empty list
/// clears them. Rules are validated exactly as an ad hoc plan would be.
pub async fn replace_rules(
    AppState(app): AppState<App>,
    Extension(identity): Extension<Identity>,
    Path(warehouse): Path<String>,
    Json(input): Json<PlanRequest>,
) -> Result<Json<Value>, Error> {
    if identity.role != "admin" {
        return Err(Error::new(
            403,
            "Only administrators may change replenishment rules",
        ));
    }
    let state = app.store.state(&identity.tenant, &warehouse).await?;
    if !input.rules.is_empty() {
        plan(&state, &input.rules).map_err(|e| Error::new(422, e))?;
    }
    save_rules(
        &app.store.pool,
        &identity.tenant,
        &warehouse,
        &identity.actor,
        &input.rules,
    )
    .await?;
    Ok(Json(
        json!({"warehouse_id":warehouse,"rules":input.rules.len()}),
    ))
}

/// The replenishment work due now under the saved rules.
pub async fn due(
    AppState(app): AppState<App>,
    Extension(identity): Extension<Identity>,
    Path(warehouse): Path<String>,
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
    let rules = saved_rules(&app, &identity.tenant, &warehouse).await?;
    if rules.is_empty() {
        return Ok(Json(
            json!({"schema_version":1,"warehouse_id":state.warehouse.id,
            "version":state.version,"generated_at":domain::now(),"rules":0,"tasks":[],"shortages":[]}),
        ));
    }
    let mut result = plan(&state, &rules).map_err(|e| Error::new(422, e))?;
    result["rules"] = json!(rules.len());
    Ok(Json(result))
}

/// Replace a warehouse's saved rules and audit the change in one
/// transaction. Callers validate the rules first.
pub async fn save_rules(
    pool: &sqlx::PgPool,
    tenant: &str,
    warehouse: &str,
    actor: &str,
    rules: &[Rule],
) -> Result<(), Error> {
    let mut tx = pool.begin().await?;
    sqlx::query("SELECT 1 FROM wms_warehouses WHERE tenant=$1 AND warehouse_id=$2 FOR UPDATE")
        .bind(tenant)
        .bind(warehouse)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(|| Error::new(404, "Warehouse not found"))?;
    sqlx::query("DELETE FROM wms_replenishment_rules WHERE tenant=$1 AND warehouse_id=$2")
        .bind(tenant)
        .bind(warehouse)
        .execute(&mut *tx)
        .await?;
    for rule in rules {
        sqlx::query("INSERT INTO wms_replenishment_rules(tenant,warehouse_id,sku,destination,minimum,target,source_zone,updated_by) VALUES($1,$2,$3,$4,$5,$6,$7,$8)")
            .bind(tenant).bind(warehouse).bind(&rule.sku).bind(&rule.destination)
            .bind(rule.minimum).bind(rule.target).bind(&rule.source_zone).bind(actor)
            .execute(&mut *tx).await?;
    }
    let reference = json!({"warehouse_id":warehouse,"rules":rules.len()}).to_string();
    sqlx::query("INSERT INTO wms_admin_audit(id,tenant,actor,action,reference) VALUES($1,$2,$3,'replenishment.rules',$4)")
        .bind(domain::id()).bind(tenant).bind(actor).bind(reference)
        .execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(())
}
