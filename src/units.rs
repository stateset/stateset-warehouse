//! Reviewed packaging conversions. Custody and every canonical command remain
//! exact each quantities; the original pack observation travels with the request.
use crate::store::Identity;
use crate::{
    api::App,
    domain::{self, Command, State},
    persistence::decode,
    store::{Error, Request, Result},
};
use axum::{
    extract::{Path, Query, State as AppState},
    Extension, Json,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sqlx::{PgConnection, Row};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Input {
    /// Empty for one root quantity; original line ID for line arrays; exact
    /// line/bin/lot tuple for package packing. Never an inferred SKU position.
    pub target: Vec<String>,
    pub sku: String,
    pub pack_code: String,
    pub definition_id: String,
    pub pack_quantity: i64,
    pub each_per_pack: i64,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Pack {
    pub id: String,
    pub sku: String,
    pub code: String,
    pub label: String,
    pub each_per_pack: i64,
}
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Change {
    pub pack: Pack,
    pub enabled: bool,
    pub reason: String,
    pub actor: String,
}
fn text(s: &str) -> bool {
    !s.trim().is_empty() && s.trim() == s && s.len() <= 240 && !s.chars().any(char::is_control)
}
fn code(s: &str) -> bool {
    (1..=32).contains(&s.len())
        && s != "EA"
        && s.bytes()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
}
pub(crate) fn validate(state: &State) -> domain::Result<()> {
    if state.unit_packs.is_empty() {
        return Ok(());
    }
    let products: BTreeSet<_> = state.products.iter().map(|p| p.sku.as_str()).collect();
    let mut counts = BTreeMap::new();
    let mut keys = BTreeSet::new();
    let mut ids = BTreeSet::new();
    for p in &state.unit_packs {
        if !text(&p.id)
            || !text(&p.sku)
            || !code(&p.code)
            || !text(&p.label)
            || !(1..=1_000_000).contains(&p.each_per_pack)
            || !products.contains(p.sku.as_str())
            || !keys.insert((&p.sku, &p.code))
            || !ids.insert(&p.id)
        {
            return Err(
                "Unit packs require unique original definitions for configured products".into(),
            );
        }
        let count = counts.entry(p.sku.as_str()).or_insert(0usize);
        *count += 1;
        if *count > 20 {
            return Err("Too many enabled unit packs for one product".into());
        }
    }
    Ok(())
}
pub(crate) async fn proof(c: &mut PgConnection, t: &str, w: &str) -> Result<Value> {
    let row = sqlx::query(
        "SELECT wms_unit_scope_valid($1,$2) AS ok,(
        SELECT count(*)
        FROM wms_unit_pack_changes
        WHERE tenant=$1 AND warehouse_id=$2) AS definitions,(
        SELECT count(*)
        FROM wms_unit_observations
        WHERE tenant=$1 AND warehouse_id=$2) AS observations",
    )
    .bind(t)
    .bind(w)
    .fetch_one(c)
    .await?;
    Ok(json!({
        "ok":row.get::<bool,
        _>("ok"),
        "definition_changes":row.get::<i64,
        _>("definitions"),
        "quantity_observations":row.get::<i64,
        _>("observations"),
        "method":"immutable_definition_and_original_command_each_conversion"
    }))
}
pub(crate) fn apply(state: &mut State, command: &Command, actor: &str) -> domain::Result<String> {
    let Command::SetUnitPack {
        sku,
        code: c,
        label,
        each_per_pack,
        enabled,
        reason,
    } = command
    else {
        return Err("Unsupported unit definition".into());
    };
    if !text(sku)
        || !code(c)
        || !text(label)
        || !text(reason)
        || !text(actor)
        || !(1..=1_000_000).contains(each_per_pack)
    {
        return Err("Unit packs require an exact SKU,1–32 uppercase/digit/hyphen/underscore code other than EA,1–1,000,000 each per pack, label and attributed reason".into());
    }
    if !state.products.iter().any(|p| p.sku == *sku) {
        return Err("Configure the product before its packaging units".into());
    }
    let existing = state
        .unit_packs
        .iter()
        .any(|p| p.sku == *sku && p.code == *c);
    if !*enabled && !existing {
        return Err("Disable only a currently enabled unit pack".into());
    }
    if *enabled && !existing && state.unit_packs.iter().filter(|p| p.sku == *sku).count() >= 20 {
        return Err("A product can have at most twenty enabled pack definitions".into());
    }
    let pack = Pack {
        id: domain::id(),
        sku: sku.clone(),
        code: c.clone(),
        label: label.clone(),
        each_per_pack: *each_per_pack,
    };
    state.unit_packs.retain(|p| p.sku != *sku || p.code != *c);
    if *enabled {
        state.unit_packs.push(pack.clone());
        state
            .unit_packs
            .sort_by(|a, b| (&a.sku, &a.code).cmp(&(&b.sku, &b.code)));
    }
    state.unit_pack_change = Some(Change {
        pack: pack.clone(),
        enabled: *enabled,
        reason: reason.clone(),
        actor: actor.into(),
    });
    Ok(pack.id)
}
pub(crate) async fn load(c: &mut PgConnection, t: &str, w: &str, state: &mut State) -> Result<()> {
    let skus: Vec<_> = state.products.iter().map(|p| p.sku.as_str()).collect();
    let valid:bool=sqlx::query_scalar("SELECT NOT EXISTS(SELECT 1 FROM wms_unit_pack_changes p WHERE tenant=$1 AND warehouse_id=$2 AND sku=ANY($3) AND (version>$4 OR NOT wms_unit_pack_original(p)))")
        .bind(t).bind(w).bind(&skus).bind(state.version).fetch_one(&mut *c).await?;
    if !valid {
        return Err(Error::new(
            503,
            "Original pack definitions disagree; unit review and new work held",
        ));
    }
    state.unit_packs=sqlx::query("SELECT jsonb_build_object('id',id,'sku',sku,'code',code,'label',label,'each_per_pack',each_per_pack) AS data
        FROM (
        SELECT DISTINCT ON(sku,code) *
        FROM wms_unit_pack_changes
        WHERE tenant=$1 AND warehouse_id=$2 AND sku=ANY($3) AND version<=$4
        ORDER BY sku,code,version DESC) p
        WHERE enabled
        ORDER BY sku,code")
        .bind(t).bind(w).bind(&skus).bind(state.version).fetch_all(&mut *c).await?.into_iter().map(|r|decode(r.get("data"))).collect::<Result<_>>()?;
    validate(state).map_err(|e| Error::new(503, e))?;
    Ok(())
}
pub(crate) async fn admit(c: &mut PgConnection, t: &str, w: &str, r: &Request) -> Result<()> {
    if r.unit_inputs.len() > 100 {
        return Err(Error::new(
            422,
            "At most one hundred original unit observations are allowed",
        ));
    }
    if r.unit_inputs.is_empty() {
        return Ok(());
    }
    let targets =
        sqlx::query("SELECT target,sku,each_quantity,allow_zero FROM wms_unit_targets($1,$2,$3)")
            .bind(t)
            .bind(w)
            .bind(json!(r.command))
            .fetch_all(&mut *c)
            .await?;
    let mut seen = BTreeSet::new();
    for input in &r.unit_inputs {
        if input.target.len() > 3
            || input.target.iter().any(|s| !text(s))
            || !text(&input.sku)
            || !code(&input.pack_code)
            || !text(&input.definition_id)
            || !(0..=1_000_000).contains(&input.pack_quantity)
            || !(1..=1_000_000).contains(&input.each_per_pack)
            || !seen.insert(input.target.clone())
        {
            return Err(Error::new(422,"Unit observation requires one unique original target, SKU, pack definition and exact whole pack count"));
        }
        let target=targets.iter().find(|r|r.get::<Value,_>("target")==json!(input.target) && r.get::<String,_>("sku")==input.sku)
            .ok_or_else(||Error::new(422,"Unit observation does not belong to this command's exact original quantity target"))?;
        let quantity = input
            .pack_quantity
            .checked_mul(input.each_per_pack)
            .ok_or_else(|| Error::new(422, "Converted each quantity exceeds the exact range"))?;
        if quantity > 1_000_000
            || quantity != target.get::<i64, _>("each_quantity")
            || (quantity == 0 && !target.get::<bool, _>("allow_zero"))
        {
            return Err(Error::new(422,"Original pack count and factor must equal this command's exact each quantity; no rounding or implicit conversion is allowed"));
        }
        let definition: Option<(String, i64, bool, bool)> = sqlx::query_as(
            "SELECT id,each_per_pack,enabled,wms_unit_pack_original(p)
            FROM wms_unit_pack_changes p
            WHERE tenant=$1 AND warehouse_id=$2 AND sku=$3 AND code=$4
            ORDER BY version DESC
            LIMIT 1",
        )
        .bind(t)
        .bind(w)
        .bind(&input.sku)
        .bind(&input.pack_code)
        .fetch_optional(&mut *c)
        .await?;
        let Some((id, factor, enabled, valid)) = definition else {
            return Err(Error::new(
                409,
                "Original pack definition is unavailable; explicitly refresh and review",
            ));
        };
        if !valid {
            return Err(Error::new(
                503,
                "Original pack definition source needs reconciliation",
            ));
        }
        if !enabled || id != input.definition_id || factor != input.each_per_pack {
            return Err(Error::new(409,"Pack definition changed or was disabled; retain an uncertain original request and explicitly review fresh work"));
        }
    }
    Ok(())
}
pub(crate) async fn persist(
    c: &mut PgConnection,
    t: &str,
    w: &str,
    r: &Request,
    next: &State,
    eid: &str,
) -> Result<()> {
    if let Some(change) = &next.unit_pack_change {
        let p = &change.pack;
        sqlx::query("INSERT INTO wms_unit_pack_changes(tenant,warehouse_id,id,version,sku,code,label,each_per_pack,enabled,reason,actor,recorded_at,source_event_id)
            VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13)")
            .bind(t).bind(w).bind(&p.id).bind(next.version).bind(&p.sku).bind(&p.code).bind(&p.label).bind(p.each_per_pack).bind(change.enabled).bind(&change.reason).bind(&change.actor).bind(next.quality_time().to_rfc3339()).bind(eid).execute(&mut *c).await?;
    }
    for (n, input) in r.unit_inputs.iter().enumerate() {
        let quantity = input
            .pack_quantity
            .checked_mul(input.each_per_pack)
            .ok_or_else(|| Error::new(422, "Converted quantity exceeds the exact range"))?;
        sqlx::query("INSERT INTO wms_unit_observations(tenant,warehouse_id,event_id,version,ordinal,target,sku,pack_code,definition_id,pack_quantity,each_per_pack,each_quantity)
            VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12)")
            .bind(t).bind(w).bind(eid).bind(next.version).bind((n+1) as i32).bind(json!(input.target)).bind(&input.sku).bind(&input.pack_code).bind(&input.definition_id).bind(input.pack_quantity).bind(input.each_per_pack).bind(quantity).execute(&mut *c).await?;
    }
    Ok(())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PageQuery {
    pub sku: String,
    pub code: Option<String>,
    pub history: Option<bool>,
    pub after: Option<String>,
    pub limit: Option<i64>,
    pub review: Option<i64>,
}
#[derive(Serialize, Deserialize, PartialEq, Clone, Copy)]
#[serde(rename_all = "snake_case")]
enum View {
    Current,
    History,
    Observations,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Cursor {
    schema_version: i32,
    warehouse_id: String,
    sku: String,
    code: Option<String>,
    review: i64,
    view: View,
    key: Vec<String>,
}
fn page_error() -> Error {
    Error::new(
        422,
        "Unit pages require an exact SKU, original cursor/review and limit 1–100",
    )
}
pub async fn page(
    AppState(app): AppState<App>,
    Extension(i): Extension<Identity>,
    Path(w): Path<String>,
    q: std::result::Result<Query<PageQuery>, axum::extract::rejection::QueryRejection>,
) -> Result<Json<Value>> {
    let Query(q) = q.map_err(|_| page_error())?;
    let view = if q.history.unwrap_or(false) {
        View::History
    } else {
        View::Current
    };
    read_page(app, i, w, q, view).await
}
pub async fn observations(
    AppState(app): AppState<App>,
    Extension(i): Extension<Identity>,
    Path(w): Path<String>,
    q: std::result::Result<Query<PageQuery>, axum::extract::rejection::QueryRejection>,
) -> Result<Json<Value>> {
    let Query(q) = q.map_err(|_| page_error())?;
    if q.history.is_some() {
        return Err(page_error());
    }
    read_page(app, i, w, q, View::Observations).await
}
async fn read_page(
    app: App,
    i: Identity,
    w: String,
    q: PageQuery,
    view: View,
) -> Result<Json<Value>> {
    let limit = q.limit.unwrap_or(50);
    if !text(&q.sku)
        || q.code.as_deref().is_some_and(|s| !code(s))
        || !(1..=100).contains(&limit)
        || q.review.is_some_and(|r| r < 0)
    {
        return Err(page_error());
    }
    let cursor: Option<Cursor> = q
        .after
        .as_ref()
        .map(|s| {
            if s.len() > 2048 {
                return Err(page_error());
            }
            let c: Cursor = serde_json::from_str(s).map_err(|_| page_error())?;
            if c.schema_version != 1
                || c.warehouse_id != w
                || c.sku != q.sku
                || c.code != q.code
                || c.view != view
                || c.review < 0
                || q.review.is_some_and(|r| r != c.review)
                || c.key.len() != if view == View::Observations { 2 } else { 1 }
                || c.key.iter().any(|s| !text(s))
            {
                return Err(page_error());
            }
            if view == View::Current {
                if !code(&c.key[0]) {
                    return Err(page_error());
                }
            } else {
                for s in &c.key {
                    let n = s.parse::<i64>().map_err(|_| page_error())?;
                    if n <= 0 || n.to_string() != *s {
                        return Err(page_error());
                    }
                }
            }
            Ok(c)
        })
        .transpose()?;
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
    let product: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM wms_products WHERE tenant=$1 AND warehouse_id=$2 AND sku=$3)",
    )
    .bind(&i.tenant)
    .bind(&w)
    .bind(&q.sku)
    .fetch_one(&mut *tx)
    .await?;
    if !product {
        return Err(Error::new(404, "Product not found"));
    }
    if q.review.is_some_and(|r| r != version)
        || cursor.as_ref().is_some_and(|c| c.review != version)
    {
        return Err(Error::new(
            409,
            "Warehouse changed; discard original unit pages and explicitly refresh the review",
        ));
    }
    let count: i64 = if view == View::Observations {
        sqlx::query_scalar(
            "SELECT count(*)
            FROM wms_unit_observations
            WHERE tenant=$1 AND warehouse_id=$2 AND sku=$3 AND ($4::text IS NULL OR pack_code=$4)",
        )
        .bind(&i.tenant)
        .bind(&w)
        .bind(&q.sku)
        .bind(&q.code)
        .fetch_one(&mut *tx)
        .await?
    } else if view == View::History {
        sqlx::query_scalar(
            "SELECT count(*)
            FROM wms_unit_pack_changes
            WHERE tenant=$1 AND warehouse_id=$2 AND sku=$3 AND ($4::text IS NULL OR code=$4)",
        )
        .bind(&i.tenant)
        .bind(&w)
        .bind(&q.sku)
        .bind(&q.code)
        .fetch_one(&mut *tx)
        .await?
    } else {
        sqlx::query_scalar(
            "SELECT count(DISTINCT code)
            FROM wms_unit_pack_changes
            WHERE tenant=$1 AND warehouse_id=$2 AND sku=$3 AND ($4::text IS NULL OR code=$4)",
        )
        .bind(&i.tenant)
        .bind(&w)
        .bind(&q.sku)
        .bind(&q.code)
        .fetch_one(&mut *tx)
        .await?
    };
    let rows = if view == View::Observations {
        let v = cursor
            .as_ref()
            .map(|c| c.key[0].parse::<i64>().expect("validated cursor"))
            .unwrap_or(0);
        let ordinal = cursor
            .as_ref()
            .map(|c| c.key[1].parse::<i32>().map_err(|_| page_error()))
            .transpose()?
            .unwrap_or(0);
        sqlx::query("SELECT to_jsonb(r)-'tenant'||jsonb_build_object('actor',e.payload->'actor','recorded_at',e.payload->'occurred_at','request_id',e.payload->'request_id','command',e.payload->'command','definition_label',p.label,'source_ok',wms_unit_observation_original(r)) AS data
            FROM wms_unit_observations r
            JOIN wms_events e ON e.id=r.event_id
            JOIN wms_unit_pack_changes p ON p.tenant=r.tenant AND p.warehouse_id=r.warehouse_id AND p.id=r.definition_id
            WHERE r.tenant=$1 AND r.warehouse_id=$2 AND r.sku=$3 AND ($4::text IS NULL OR r.pack_code=$4) AND (r.version,r.ordinal)>($5,$6)
            ORDER BY r.version,r.ordinal
            LIMIT $7")
            .bind(&i.tenant).bind(&w).bind(&q.sku).bind(&q.code).bind(v).bind(ordinal).bind(limit+1).fetch_all(&mut *tx).await?
    } else if view == View::History {
        let v = cursor
            .as_ref()
            .map(|c| c.key[0].parse::<i64>().expect("validated cursor"))
            .unwrap_or(0);
        sqlx::query("SELECT to_jsonb(p)-'tenant'||jsonb_build_object('source_ok',wms_unit_pack_original(p)) AS data
            FROM wms_unit_pack_changes p
            WHERE tenant=$1 AND warehouse_id=$2 AND sku=$3 AND ($4::text IS NULL OR code=$4) AND version>$5
            ORDER BY version
            LIMIT $6")
            .bind(&i.tenant).bind(&w).bind(&q.sku).bind(&q.code).bind(v).bind(limit+1).fetch_all(&mut *tx).await?
    } else {
        let c = cursor.as_ref().map(|c| c.key[0].as_str()).unwrap_or("");
        sqlx::query("SELECT to_jsonb(p)-'tenant'||jsonb_build_object('source_ok',wms_unit_pack_original(p)) AS data
            FROM (
            SELECT DISTINCT ON(code) *
            FROM wms_unit_pack_changes
            WHERE tenant=$1 AND warehouse_id=$2 AND sku=$3 AND ($4::text IS NULL OR code=$4)
            ORDER BY code,version DESC) p
            WHERE code COLLATE \"C\">$5 COLLATE \"C\"
            ORDER BY code COLLATE \"C\"
            LIMIT $6")
            .bind(&i.tenant).bind(&w).bind(&q.sku).bind(&q.code).bind(c).bind(limit+1).fetch_all(&mut *tx).await?
    };
    let items: Vec<Value> = rows
        .iter()
        .take(limit as usize)
        .map(|r| r.get("data"))
        .collect();
    let next = if rows.len() > limit as usize {
        let last = items.last().ok_or_else(page_error)?;
        let key = match view {
            View::Current => vec![last["code"].as_str().ok_or_else(page_error)?.into()],
            View::History => vec![last["version"].to_string()],
            View::Observations => vec![last["version"].to_string(), last["ordinal"].to_string()],
        };
        Some(
            serde_json::to_string(&Cursor {
                schema_version: 1,
                warehouse_id: w.clone(),
                sku: q.sku.clone(),
                code: q.code.clone(),
                review: version,
                view,
                key,
            })
            .map_err(|_| page_error())?,
        )
    } else {
        None
    };
    let ok = items.iter().all(|i| i["source_ok"] == true);
    tx.commit().await?;
    Ok(Json(json!({
        "schema_version":1,
        "warehouse_id":w,
        "version":version,
        "sku":q.sku,
        "code":q.code,
        "view":view,
        "items":items,
        "total_records":count,
        "next_cursor":next,
        "page_source_ok":ok,
        "source_checks":"selected_original_pack_definition_or_quantity_observation_sources"
    })))
}
