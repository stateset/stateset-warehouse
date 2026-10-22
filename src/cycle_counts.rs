//! Scheduled cycle counts. SKUs are classed A/B/C by picked volume over a
//! lookback window (a Pareto split), and each bin/lot is due when its last
//! count is older than its class interval. Last-counted times come from the
//! original `count_stock` events, because a count that matches stock creates
//! no movement. The plan changes nothing: each task is filled in by the
//! counter and confirmed as an ordinary reviewed `count_stock` command.
use crate::{
    api::App,
    domain::{self, State},
    store::{Error, Identity},
};
use axum::{
    extract::{Path, State as AppState},
    Extension, Json,
};
use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::{json, Value};
use sqlx::Row;
use std::collections::BTreeMap;

pub const MAX_TASKS: usize = 500;

#[derive(Debug, Clone, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PlanRequest {
    /// Days between counts for A, B and C SKUs (default 7, 30, 90).
    #[serde(default = "default_intervals")]
    pub interval_days: [i64; 3],
    /// Share of picked volume covered by A, then A+B (default 0.8, 0.95).
    #[serde(default = "default_shares")]
    pub cumulative_shares: [f64; 2],
    /// Days of pick history used for velocity (1–365, default 90).
    #[serde(default = "default_lookback")]
    pub lookback_days: i64,
    /// Most tasks to return (1–500, default 200).
    #[serde(default = "default_limit")]
    pub limit: usize,
    /// Hide expected quantities so counts are not biased (default true).
    #[serde(default = "default_blind")]
    pub blind: bool,
}
fn default_intervals() -> [i64; 3] {
    [7, 30, 90]
}
fn default_shares() -> [f64; 2] {
    [0.8, 0.95]
}
fn default_lookback() -> i64 {
    90
}
fn default_limit() -> usize {
    200
}
fn default_blind() -> bool {
    true
}
impl Default for PlanRequest {
    fn default() -> Self {
        Self {
            interval_days: default_intervals(),
            cumulative_shares: default_shares(),
            lookback_days: default_lookback(),
            limit: default_limit(),
            blind: default_blind(),
        }
    }
}

/// Pareto ABC classes from picked units per SKU. SKUs with no picks are C.
pub fn classify(
    skus: &[String],
    picked: &BTreeMap<String, i64>,
    shares: [f64; 2],
) -> BTreeMap<String, char> {
    let mut ranked: Vec<(&String, i64)> = skus
        .iter()
        .map(|s| (s, picked.get(s).copied().unwrap_or(0)))
        .collect();
    ranked.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
    let total: i64 = ranked.iter().map(|r| r.1).sum();
    let mut classes = BTreeMap::new();
    let mut running = 0i64;
    for (sku, units) in ranked {
        // Class by the share reached *before* this SKU, so the SKU that
        // crosses a boundary stays in the higher class.
        let before = if total == 0 {
            1.0
        } else {
            running as f64 / total as f64
        };
        let class = if units == 0 {
            'C'
        } else if before < shares[0] {
            'A'
        } else if before < shares[1] {
            'B'
        } else {
            'C'
        };
        running += units;
        classes.insert(sku.clone(), class);
    }
    classes
}

pub struct Inputs {
    pub picked: BTreeMap<String, i64>,
    pub last_counted: BTreeMap<(String, String, String), DateTime<Utc>>,
    pub now: DateTime<Utc>,
}

pub fn plan(state: &State, request: &PlanRequest, inputs: &Inputs) -> domain::Result<Value> {
    state.validate()?;
    if request
        .interval_days
        .iter()
        .any(|d| !(1..=3650).contains(d))
        || request.interval_days[0] > request.interval_days[1]
        || request.interval_days[1] > request.interval_days[2]
    {
        return Err("Intervals must be 1–3,650 days with A ≤ B ≤ C".into());
    }
    let [a, b] = request.cumulative_shares;
    if !(a > 0.0 && a <= b && b <= 1.0) {
        return Err("Cumulative shares must satisfy 0 < A ≤ A+B ≤ 1".into());
    }
    if !(1..=365).contains(&request.lookback_days) {
        return Err("lookback_days must be 1–365".into());
    }
    if !(1..=MAX_TASKS).contains(&request.limit) {
        return Err(format!("limit must be 1–{MAX_TASKS}"));
    }
    let skus: Vec<String> = state.products.iter().map(|p| p.sku.clone()).collect();
    let classes = classify(&skus, &inputs.picked, request.cumulative_shares);
    let countable: Vec<_> = state
        .stocks
        .iter()
        .filter(|s| {
            s.on_hand > 0
                && state
                    .locations
                    .iter()
                    .any(|l| l.code == s.location && l.kind != "staging")
        })
        .collect();
    let mut due = vec![];
    for s in countable {
        let class = classes.get(&s.sku).copied().unwrap_or('C');
        let interval = request.interval_days[match class {
            'A' => 0,
            'B' => 1,
            _ => 2,
        }];
        let last = inputs
            .last_counted
            .get(&(s.sku.clone(), s.lot.clone(), s.location.clone()));
        let overdue_days = match last {
            None => i64::MAX,
            Some(at) => (inputs.now - *at).num_days() - interval,
        };
        if overdue_days < 0 {
            continue;
        }
        let serial = state
            .products
            .iter()
            .any(|p| p.sku == s.sku && p.serial_controlled);
        let mut task = json!({
            "class": class.to_string(),
            "interval_days": interval,
            "sku": s.sku,
            "lot": s.lot,
            "location": s.location,
            "last_counted_at": last.map(|t| t.to_rfc3339()),
            "overdue_days": if overdue_days == i64::MAX { Value::Null } else { json!(overdue_days) },
            "reserved_minimum": s.reserved,
            "serial_labels_required": serial,
            "command": {"type":"count_stock","sku":s.sku,"lot":s.lot,"location":s.location,
                "counted":null,"reason":format!("Scheduled class {class} cycle count")},
        });
        if !request.blind {
            task["expected_on_hand"] = json!(s.on_hand);
        }
        due.push((last.is_some(), -overdue_days.min(i64::MAX - 1), class, task));
    }
    // Never-counted first, then most overdue, then by class.
    due.sort_by(|x, y| (x.0, x.1, x.2).cmp(&(y.0, y.1, y.2)));
    let total_due = due.len();
    let tasks: Vec<Value> = due.into_iter().take(request.limit).map(|d| d.3).collect();
    let mut summary = BTreeMap::new();
    for class in classes.values() {
        *summary.entry(class.to_string()).or_insert(0) += 1;
    }
    Ok(json!({
        "schema_version": 1,
        "warehouse_id": state.warehouse.id,
        "version": state.version,
        "generated_at": domain::now(),
        "lookback_days": request.lookback_days,
        "sku_classes": summary,
        "due": total_due,
        "tasks": tasks,
        "limits": ["Each task becomes a count_stock command only when the counter enters the observed quantity",
                   "A count below reserved stock is rejected by the command; reserved_minimum shows that floor"],
    }))
}

async fn inputs(
    app: &App,
    tenant: &str,
    warehouse: &str,
    lookback_days: i64,
) -> Result<Inputs, Error> {
    let picked = sqlx::query(
        "SELECT sku,sum(quantity)::bigint AS units FROM wms_movements
         WHERE tenant=$1 AND warehouse_id=$2 AND kind='pick'
         AND data->>'at' ~ '^[0-9]{4}-[0-9]{2}-[0-9]{2}T' AND (data->>'at')::timestamptz > now()-make_interval(days=>$3::int) GROUP BY sku",
    )
    .bind(tenant)
    .bind(warehouse)
    .bind(lookback_days as i32)
    .fetch_all(&app.store.pool)
    .await?
    .into_iter()
    .map(|r| (r.get("sku"), r.get("units")))
    .collect();
    let last_counted = sqlx::query(
        "SELECT payload#>>'{command,sku}' AS sku,payload#>>'{command,lot}' AS lot,
         payload#>>'{command,location}' AS location,floor(extract(epoch FROM max(created_at)))::bigint AS at FROM wms_events
         WHERE tenant=$1 AND warehouse_id=$2 AND payload#>>'{command,type}'='count_stock'
         GROUP BY 1,2,3",
    )
    .bind(tenant)
    .bind(warehouse)
    .fetch_all(&app.store.pool)
    .await?
    .into_iter()
    .filter_map(|r| {
        Some((
            (r.get::<Option<String>, _>("sku")?, r.get::<Option<String>, _>("lot")?, r.get::<Option<String>, _>("location")?),
            DateTime::<Utc>::from_timestamp(r.get::<i64, _>("at"), 0)?,
        ))
    })
    .collect();
    let now: i64 = sqlx::query_scalar("SELECT floor(extract(epoch FROM now()))::bigint")
        .fetch_one(&app.store.pool)
        .await?;
    let now = DateTime::<Utc>::from_timestamp(now, 0)
        .ok_or_else(|| Error::new(503, "Database clock is out of range"))?;
    Ok(Inputs {
        picked,
        last_counted,
        now,
    })
}

pub async fn endpoint(
    AppState(app): AppState<App>,
    Extension(identity): Extension<Identity>,
    Path(warehouse): Path<String>,
    Json(input): Json<PlanRequest>,
) -> Result<Json<Value>, Error> {
    if !["operator", "admin"].contains(&identity.role.as_str()) {
        return Err(Error::new(403, "Cycle count planning requires an operator"));
    }
    let state = app.store.state(&identity.tenant, &warehouse).await?;
    state.validate().map_err(|_| {
        Error::new(
            503,
            "Warehouse inventory requires reconciliation; cycle counts held",
        )
    })?;
    if !(1..=365).contains(&input.lookback_days) {
        return Err(Error::new(422, "lookback_days must be 1–365"));
    }
    let data = inputs(&app, &identity.tenant, &warehouse, input.lookback_days).await?;
    Ok(Json(
        plan(&state, &input, &data).map_err(|e| Error::new(422, e))?,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pareto_classes_keep_the_crossing_sku_in_the_higher_class() {
        let skus: Vec<String> = ["A", "B", "C", "D", "E"].map(String::from).to_vec();
        let picked: BTreeMap<String, i64> = [("A", 70), ("B", 15), ("C", 10), ("D", 5)]
            .map(|(s, n)| (s.to_string(), n))
            .into();
        let classes = classify(&skus, &picked, [0.8, 0.95]);
        assert_eq!(classes["A"], 'A');
        assert_eq!(classes["B"], 'A'); // starts at 70% < 80%
        assert_eq!(classes["C"], 'B'); // starts at 85%
        assert_eq!(classes["D"], 'C'); // starts at 95%
        assert_eq!(classes["E"], 'C'); // never picked
    }

    #[test]
    fn no_picks_means_every_sku_is_class_c() {
        let skus = vec!["A".to_string(), "B".to_string()];
        let classes = classify(&skus, &BTreeMap::new(), [0.8, 0.95]);
        assert!(classes.values().all(|c| *c == 'C'));
    }
}
