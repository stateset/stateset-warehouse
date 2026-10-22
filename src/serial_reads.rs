//! Bounded tenant/SKU identity review. No read changes custody or replaces a
//! physical observation. Global SKU heads fence pages across facility changes.
use crate::{
    api::App,
    store::{Error, Identity, Result},
};
use axum::{
    extract::{Query, State},
    Extension, Json,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sqlx::{PgConnection, Row};
use std::collections::BTreeMap;

const PAGE_BYTES: usize = 128 * 1024;
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum View {
    Current,
    History,
    Observations,
}
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    OnHand,
    Staged,
    Shipped,
    InTransit,
    Lost,
    Missing,
}
impl Status {
    fn name(self) -> &'static str {
        match self {
            Self::OnHand => "on_hand",
            Self::Staged => "staged",
            Self::Shipped => "shipped",
            Self::InTransit => "in_transit",
            Self::Lost => "lost",
            Self::Missing => "missing",
        }
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PageQuery {
    sku: String,
    serial: Option<String>,
    warehouse_id: Option<String>,
    status: Option<Status>,
    limit: Option<i64>,
    review: Option<i64>,
    after: Option<String>,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Cursor {
    schema_version: u8,
    tenant: String,
    sku: String,
    serial: Option<String>,
    warehouse_id: Option<String>,
    status: Option<Status>,
    view: View,
    review: i64,
    key: Vec<String>,
}
struct Candidate {
    data: Value,
    key: Vec<String>,
    source_key: Vec<String>,
}
fn text(s: &str) -> bool {
    !s.is_empty() && s.len() <= 240 && s.trim() == s && !s.chars().any(char::is_control)
}
fn invalid() -> Error {
    Error::new(422,"Serial pages require an exact SKU, original scope/cursor/review and limit 1–100; history also requires an exact literal serial")
}
fn number(s: &str, zero: bool) -> Result<i64> {
    let n = s.parse::<i64>().map_err(|_| invalid())?;
    if n < if zero { 0 } else { 1 } || n.to_string() != s {
        return Err(invalid());
    }
    Ok(n)
}
fn cursor(q: &PageQuery, i: &Identity, view: View) -> Result<Option<Cursor>> {
    q.after
        .as_ref()
        .map(|raw| {
            if raw.len() > 4096 {
                return Err(invalid());
            }
            let c: Cursor = serde_json::from_str(raw).map_err(|_| invalid())?;
            if c.schema_version != 1
                || c.tenant != i.tenant
                || c.sku != q.sku
                || c.serial != q.serial
                || c.warehouse_id != q.warehouse_id
                || c.status != q.status
                || c.view != view
                || c.review < 0
                || q.review.is_some_and(|r| r != c.review)
                || c.key.len() != if view == View::Observations { 3 } else { 1 }
            {
                return Err(invalid());
            }
            if view == View::Current {
                if !text(&c.key[0]) || q.serial.as_ref().is_some_and(|s| s != &c.key[0]) {
                    return Err(invalid());
                }
            } else {
                number(&c.key[0], false)?;
                if view == View::Observations
                    && (number(&c.key[1], false)? > 100 || number(&c.key[2], true)? > 10_000)
                {
                    return Err(invalid());
                }
            }
            Ok(c)
        })
        .transpose()
}

pub async fn current(
    State(app): State<App>,
    Extension(i): Extension<Identity>,
    q: std::result::Result<Query<PageQuery>, axum::extract::rejection::QueryRejection>,
) -> Result<Json<Value>> {
    read(app, i, q.map_err(|_| invalid())?.0, View::Current).await
}
pub async fn history(
    State(app): State<App>,
    Extension(i): Extension<Identity>,
    q: std::result::Result<Query<PageQuery>, axum::extract::rejection::QueryRejection>,
) -> Result<Json<Value>> {
    read(app, i, q.map_err(|_| invalid())?.0, View::History).await
}
pub async fn observations(
    State(app): State<App>,
    Extension(i): Extension<Identity>,
    q: std::result::Result<Query<PageQuery>, axum::extract::rejection::QueryRejection>,
) -> Result<Json<Value>> {
    read(app, i, q.map_err(|_| invalid())?.0, View::Observations).await
}

async fn read(app: App, i: Identity, q: PageQuery, view: View) -> Result<Json<Value>> {
    let limit = q.limit.unwrap_or(50);
    if !text(&q.sku)
        || q.serial.as_deref().is_some_and(|s| !text(s))
        || q.warehouse_id.as_deref().is_some_and(|s| !text(s))
        || !(1..=100).contains(&limit)
        || q.review.is_some_and(|r| r < 0)
        || (view == View::History && q.serial.is_none())
        || (view != View::Current && q.status.is_some())
    {
        return Err(invalid());
    }
    let after = cursor(&q, &i, view)?;
    let mut tx = app.store.pool.begin().await?;
    sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY")
        .execute(&mut *tx)
        .await?;
    if let Some(w) = &q.warehouse_id {
        let exists: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM wms_warehouses WHERE tenant=$1 AND warehouse_id=$2)",
        )
        .bind(&i.tenant)
        .bind(w)
        .fetch_one(&mut *tx)
        .await?;
        if !exists {
            return Err(Error::new(404, "Warehouse not found"));
        }
    }
    let head=sqlx::query("SELECT m.controlled,wms_serial_mode_original(m) AS mode_ok,h.version,h.last_event_id,CASE WHEN h.sku IS NULL THEN false ELSE wms_serial_head_bound(h) END AS head_ok FROM wms_serial_modes m LEFT JOIN wms_serial_heads h ON h.tenant=m.tenant AND h.sku=m.sku WHERE m.tenant=$1 AND m.sku=$2")
        .bind(&i.tenant).bind(&q.sku).fetch_optional(&mut *tx).await?.ok_or_else(||Error::new(404,"Product not found"))?;
    if !head.try_get::<bool, _>("controlled")? {
        return Err(Error::new(
            422,
            "This tenant/SKU uses untracked each quantities",
        ));
    }
    if !head.try_get::<bool, _>("mode_ok")? || !head.try_get::<bool, _>("head_ok")? {
        return Err(Error::new(
            503,
            "Original serial mode or global review disagrees; identity pages held",
        ));
    }
    let version: i64 = head.try_get("version")?;
    if q.review.is_some_and(|r| r != version) || after.as_ref().is_some_and(|c| c.review != version)
    {
        return Err(Error::new(409,"Global SKU identity changed; discard retained serial pages and explicitly refresh the complete review"));
    }
    // A retained history row must not disappear merely because its projection
    // was removed. This check is SKU/global, independent of a custody filter.
    let missing: bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM wms_serial_steps s WHERE s.tenant=$1 AND s.sku=$2 AND ($3::text IS NULL OR s.serial=$3) AND NOT EXISTS(SELECT 1 FROM wms_serial_units u WHERE u.tenant=s.tenant AND u.sku=s.sku AND u.serial=s.serial))")
        .bind(&i.tenant).bind(&q.sku).bind(&q.serial).fetch_one(&mut *tx).await?;
    let missing_observed: bool = if let Some(serial) = &q.serial {
        sqlx::query_scalar("SELECT NOT EXISTS(SELECT 1 FROM wms_serial_units WHERE tenant=$1 AND sku=$2 AND serial=$3) AND EXISTS(SELECT 1 FROM wms_serial_observations WHERE tenant=$1 AND sku=$2 AND serials @> jsonb_build_array($3::text))")
            .bind(&i.tenant).bind(&q.sku).bind(serial).fetch_one(&mut *tx).await?
    } else {
        false
    };
    if missing || missing_observed {
        return Err(Error::new(
            503,
            "Original serial evidence lacks its current projection; identity pages held",
        ));
    }
    let current_source_ok: Option<bool> = if view == View::History {
        let state=sqlx::query("SELECT wms_serial_unit_bound(u) AND wms_serial_place_references_valid(u.tenant,u.sku,u.lot,u.place) AS ok,u.revision,(SELECT count(*) FROM wms_serial_steps s WHERE s.tenant=u.tenant AND s.sku=u.sku AND s.serial=u.serial) AS steps FROM wms_serial_units u WHERE u.tenant=$1 AND u.sku=$2 AND u.serial=$3")
            .bind(&i.tenant).bind(&q.sku).bind(&q.serial).fetch_optional(&mut *tx).await?.ok_or_else(||Error::new(404,"Serial identity not found"))?;
        Some(
            state.try_get::<bool, _>("ok")?
                && state.try_get::<i64, _>("revision")? == state.try_get::<i64, _>("steps")?,
        )
    } else {
        None
    };
    if let Some(c) = &after {
        if !cursor_exists(&mut tx, &i.tenant, &q, view, &c.key).await? {
            return Err(invalid());
        }
    }
    let (total, candidates) = candidates(
        &mut tx,
        &i.tenant,
        &q,
        view,
        after.as_ref().map(|c| c.key.as_slice()),
        limit,
    )
    .await?;
    let mut selected = Vec::new();
    let mut bytes = 2usize;
    for item in candidates.iter().take(limit as usize) {
        let size = serde_json::to_vec(&item.data)
            .map_err(|_| Error::new(503, "Serial record cannot be encoded"))?
            .len()
            + 1;
        if bytes + size > PAGE_BYTES {
            if selected.is_empty() {
                return Err(Error::new(
                    503,
                    "Original serial record exceeds the bounded page budget",
                ));
            }
            break;
        }
        bytes += size;
        selected.push(item);
    }
    let checks = checks(&mut tx, &i.tenant, &q, view, &selected).await?;
    let mut items = Vec::new();
    for item in &selected {
        let mut data = item.data.clone();
        data["source_ok"] = json!(checks.get(&item.source_key).copied().unwrap_or(false));
        items.push(data);
    }
    let next = if selected.len() < candidates.len() {
        let key = selected.last().ok_or_else(invalid)?.key.clone();
        Some(
            serde_json::to_string(&Cursor {
                schema_version: 1,
                tenant: i.tenant.clone(),
                sku: q.sku.clone(),
                serial: q.serial.clone(),
                warehouse_id: q.warehouse_id.clone(),
                status: q.status,
                view,
                review: version,
                key,
            })
            .map_err(|_| invalid())?,
        )
    } else {
        None
    };
    let ok = current_source_ok != Some(false) && items.iter().all(|item| item["source_ok"] == true);
    tx.commit().await?;
    Ok(Json(
        json!({"schema_version":1,"sku":q.sku,"serial":q.serial,"warehouse_id":q.warehouse_id,"status":q.status,"view":view,"review":version,"sku_version":version,"version_scope":"tenant_sku_identity","head_event_id":head.try_get::<Option<String>,_>("last_event_id")?,"items":items,"total_records":total,"next_cursor":next,"page_source_ok":ok,"current_source_ok":current_source_ok,"source_checks":"original_mode_global_head_and_selected_record_event_sources; full_reconciliation_is_separate","observation_records":"one_literal_label_occurrence_or_explicit_empty_group","page_byte_budget":PAGE_BYTES}),
    ))
}

async fn cursor_exists(
    c: &mut PgConnection,
    t: &str,
    q: &PageQuery,
    view: View,
    key: &[String],
) -> Result<bool> {
    let status = q.status.map(Status::name);
    Ok(match view {
        View::Current=>sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM wms_serial_units u WHERE tenant=$1 AND sku=$2 AND serial=$3 AND ($4::text IS NULL OR place->>'warehouse_id'=$4 OR place->>'source_warehouse_id'=$4 OR place->>'destination_warehouse_id'=$4) AND ($5::text IS NULL OR place->>'status'=$5))")
            .bind(t).bind(&q.sku).bind(&key[0]).bind(&q.warehouse_id).bind(status).fetch_one(c).await?,
        View::History=>sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM wms_serial_steps WHERE tenant=$1 AND sku=$2 AND serial=$3 AND ($4::text IS NULL OR warehouse_id=$4) AND revision=$5)")
            .bind(t).bind(&q.sku).bind(&q.serial).bind(&q.warehouse_id).bind(number(&key[0],false)?).fetch_one(c).await?,
        View::Observations=>sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM wms_serial_observations WHERE tenant=$1 AND sku=$2 AND ($3::text IS NULL OR warehouse_id=$3) AND sku_version=$4 AND ordinal=$5 AND (($6=0 AND jsonb_array_length(serials)=0 AND $7::text IS NULL) OR ($6>0 AND $6<=jsonb_array_length(serials) AND ($7::text IS NULL OR serials->>($6-1)=$7))))")
            .bind(t).bind(&q.sku).bind(&q.warehouse_id).bind(number(&key[0],false)?).bind(i32::try_from(number(&key[1],false)?).map_err(|_|invalid())?).bind(i32::try_from(number(&key[2],true)?).map_err(|_|invalid())?).bind(&q.serial).fetch_one(c).await?,
    })
}

async fn candidates(
    c: &mut PgConnection,
    t: &str,
    q: &PageQuery,
    view: View,
    after: Option<&[String]>,
    limit: i64,
) -> Result<(i64, Vec<Candidate>)> {
    let rows;
    let total;
    let status = q.status.map(Status::name);
    match view {
        View::Current => {
            total=sqlx::query_scalar("SELECT count(*) FROM wms_serial_units WHERE tenant=$1 AND sku=$2 AND ($3::text IS NULL OR serial=$3) AND ($4::text IS NULL OR place->>'warehouse_id'=$4 OR place->>'source_warehouse_id'=$4 OR place->>'destination_warehouse_id'=$4) AND ($5::text IS NULL OR place->>'status'=$5)")
                .bind(t).bind(&q.sku).bind(&q.serial).bind(&q.warehouse_id).bind(status).fetch_one(&mut *c).await?;
            // As with observations, choose the bounded literal page before
            // joining steps and decoding each distinct original event once.
            rows=sqlx::query("WITH records AS MATERIALIZED (SELECT * FROM wms_serial_units WHERE tenant=$1 AND sku=$2 AND ($3::text IS NULL OR serial=$3) AND ($4::text IS NULL OR place->>'warehouse_id'=$4 OR place->>'source_warehouse_id'=$4 OR place->>'destination_warehouse_id'=$4) AND ($5::text IS NULL OR place->>'status'=$5) AND serial COLLATE \"C\">$6 COLLATE \"C\" ORDER BY serial COLLATE \"C\" LIMIT $7), events AS MATERIALIZED (SELECT e.id,e.payload->'request_id' AS request_id,e.payload#>'{command,type}' AS command_type FROM wms_events e JOIN (SELECT DISTINCT last_event_id FROM records) k ON k.last_event_id=e.id WHERE e.tenant=$1) SELECT to_jsonb(u)-'tenant'-'last_event_id'||jsonb_build_object('event_id',u.last_event_id,'last_source',CASE WHEN s.event_id IS NULL THEN NULL ELSE jsonb_build_object('sku_version',s.sku_version,'warehouse_id',s.warehouse_id,'warehouse_version',s.warehouse_version,'actor',s.actor,'recorded_at',s.recorded_at,'target',s.target,'request_id',e.request_id,'command_type',e.command_type) END,'source_ok',false) AS data,ARRAY[u.serial] AS key FROM records u LEFT JOIN wms_serial_steps s ON s.tenant=u.tenant AND s.sku=u.sku AND s.serial=u.serial AND s.revision=u.revision AND s.event_id=u.last_event_id AND s.after_place=u.place LEFT JOIN events e ON e.id=s.event_id ORDER BY u.serial COLLATE \"C\"")
                .bind(t).bind(&q.sku).bind(&q.serial).bind(&q.warehouse_id).bind(status).bind(after.map(|k|k[0].as_str()).unwrap_or("")).bind(limit+1).fetch_all(&mut *c).await?;
        }
        View::History => {
            total=sqlx::query_scalar("SELECT count(*) FROM wms_serial_steps WHERE tenant=$1 AND sku=$2 AND serial=$3 AND ($4::text IS NULL OR warehouse_id=$4)")
                .bind(t).bind(&q.sku).bind(&q.serial).bind(&q.warehouse_id).fetch_one(&mut *c).await?;
            rows=sqlx::query("SELECT to_jsonb(s)-'tenant'||jsonb_build_object('request_id',e.payload->'request_id','command_type',e.payload#>'{command,type}','source_ok',false) AS data,ARRAY[s.revision::text] AS key FROM wms_serial_steps s LEFT JOIN wms_events e ON e.tenant=s.tenant AND e.id=s.event_id WHERE s.tenant=$1 AND s.sku=$2 AND s.serial=$3 AND ($4::text IS NULL OR s.warehouse_id=$4) AND s.revision>$5 ORDER BY s.revision LIMIT $6")
                .bind(t).bind(&q.sku).bind(&q.serial).bind(&q.warehouse_id).bind(after.map(|k|number(&k[0],false)).transpose()?.unwrap_or(0)).bind(limit+1).fetch_all(&mut *c).await?;
        }
        View::Observations => {
            total=sqlx::query_scalar("SELECT COALESCE(sum(CASE WHEN $3::text IS NULL THEN GREATEST(jsonb_array_length(serials),1) ELSE 1 END),0)::bigint FROM wms_serial_observations WHERE tenant=$1 AND sku=$2 AND ($3::text IS NULL OR serials @> jsonb_build_array($3)) AND ($4::text IS NULL OR warehouse_id=$4)")
                .bind(t).bind(&q.sku).bind(&q.serial).bind(&q.warehouse_id).fetch_one(&mut *c).await?;
            let key = after
                .map(|k| -> Result<[i64; 3]> {
                    Ok([
                        number(&k[0], false)?,
                        number(&k[1], false)?,
                        number(&k[2], true)?,
                    ])
                })
                .transpose()?
                .unwrap_or([0, 0, 0]);
            // Bound the literal page before decoding its original event metadata.
            // Otherwise a sort can expand/decode a large original count payload
            // for every observed label even when only one page is requested.
            rows=sqlx::query("WITH records AS MATERIALIZED (SELECT r.*,x.value AS label,COALESCE(x.ordinality,0) AS serial_ordinal FROM wms_serial_observations r LEFT JOIN LATERAL jsonb_array_elements_text(r.serials) WITH ORDINALITY AS x(value,ordinality) ON true WHERE r.tenant=$1 AND r.sku=$2 AND ($3::text IS NULL OR x.value=$3) AND ($4::text IS NULL OR r.warehouse_id=$4) AND (r.sku_version,r.ordinal)>=($5,$6) AND (r.sku_version,r.ordinal,COALESCE(x.ordinality,0))>($5,$6,$7) ORDER BY r.sku_version,r.ordinal,COALESCE(x.ordinality,0) LIMIT $8), events AS MATERIALIZED (SELECT e.id,e.payload->'actor' AS actor,e.payload->'occurred_at' AS recorded_at,e.payload->'request_id' AS request_id,e.payload#>'{command,type}' AS command_type FROM wms_events e JOIN (SELECT DISTINCT event_id FROM records) k ON k.event_id=e.id WHERE e.tenant=$1) SELECT jsonb_build_object('sku',r.sku,'sku_version',r.sku_version,'warehouse_id',r.warehouse_id,'event_id',r.event_id,'observation_ordinal',r.ordinal,'target',r.target,'observed_serial',r.label,'serial_ordinal',r.serial_ordinal,'observed_serial_count',jsonb_array_length(r.serials),'actor',e.actor,'recorded_at',e.recorded_at,'request_id',e.request_id,'command_type',e.command_type,'source_ok',false) AS data,ARRAY[r.sku_version::text,r.ordinal::text,r.serial_ordinal::text] AS key FROM records r LEFT JOIN events e ON e.id=r.event_id ORDER BY r.sku_version,r.ordinal,r.serial_ordinal")
                .bind(t).bind(&q.sku).bind(&q.serial).bind(&q.warehouse_id).bind(key[0]).bind(key[1]).bind(key[2]).bind(limit+1).fetch_all(&mut *c).await?;
        }
    }
    let rows = rows
        .into_iter()
        .map(|row| {
            let key: Vec<String> = row.try_get("key")?;
            let source_key = if view == View::Observations {
                key[..2].to_vec()
            } else {
                key.clone()
            };
            Ok(Candidate {
                data: row.try_get("data")?,
                key,
                source_key,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    Ok((total, rows))
}

async fn checks(
    c: &mut PgConnection,
    t: &str,
    q: &PageQuery,
    view: View,
    items: &[&Candidate],
) -> Result<BTreeMap<Vec<String>, bool>> {
    if items.is_empty() {
        return Ok(BTreeMap::new());
    }
    let mut keys = BTreeMap::new();
    for item in items {
        keys.entry(item.source_key.clone()).or_insert_with(||json!({"key":item.source_key,"serial":item.data.get("serial").cloned(),"event_id":item.data["event_id"],"revision":item.data["revision"],"sku_version":item.data["sku_version"],"ordinal":item.data["observation_ordinal"]}));
    }
    let mut keys: Vec<Value> = keys.into_values().collect();
    if view == View::History {
        for key in &mut keys {
            key["serial"] = json!(&q.serial);
        }
    }
    // Resolve each selected event through the complete snapshot-local read
    // validator, sharing its original receipt and batches across all steps.
    // The standalone write validator repeats that work for every step and can
    // time out even a small page referencing a large original count. Keep current
    // projection/latest-revision and record/event scope checks independently.
    let sql=match view {
        View::Current=>"WITH k AS MATERIALIZED(SELECT * FROM jsonb_to_recordset($3) AS x(key JSONB,serial TEXT,event_id TEXT)),events AS MATERIALIZED(SELECT e.id,e.warehouse_id,wms_serial_event_valid_read_source(e.tenant,e.warehouse_id,e.id) AS ok FROM wms_events e JOIN (SELECT DISTINCT event_id FROM k) selected ON selected.event_id=e.id WHERE e.tenant=$1) SELECT k.key,EXISTS(SELECT 1 FROM wms_serial_steps s WHERE s.tenant=u.tenant AND s.sku=u.sku AND s.serial=u.serial AND s.lot=u.lot AND s.revision=u.revision AND s.event_id=u.last_event_id AND s.after_place=u.place AND s.warehouse_id=events.warehouse_id) AND NOT EXISTS(SELECT 1 FROM wms_serial_steps later WHERE later.tenant=u.tenant AND later.sku=u.sku AND later.serial=u.serial AND later.revision>u.revision) AND wms_serial_place_references_valid(u.tenant,u.sku,u.lot,u.place) AND COALESCE(events.ok,false) AS ok FROM k JOIN wms_serial_units u ON u.tenant=$1 AND u.sku=$2 AND u.serial=k.serial LEFT JOIN events ON events.id=k.event_id AND events.id=u.last_event_id",
        View::History=>"WITH k AS MATERIALIZED(SELECT * FROM jsonb_to_recordset($3) AS x(key JSONB,serial TEXT,event_id TEXT,revision BIGINT)),events AS MATERIALIZED(SELECT e.id,e.warehouse_id,wms_serial_event_valid_read_source(e.tenant,e.warehouse_id,e.id) AS ok FROM wms_events e JOIN (SELECT DISTINCT event_id FROM k) selected ON selected.event_id=e.id WHERE e.tenant=$1) SELECT k.key,COALESCE(events.ok,false) AS ok FROM k JOIN wms_serial_steps s ON s.tenant=$1 AND s.sku=$2 AND s.serial=k.serial AND s.revision=k.revision LEFT JOIN events ON events.id=k.event_id AND events.id=s.event_id AND events.warehouse_id=s.warehouse_id",
        View::Observations=>"WITH k AS MATERIALIZED(SELECT * FROM jsonb_to_recordset($3) AS x(key JSONB,event_id TEXT,sku_version BIGINT,ordinal INTEGER)),events AS MATERIALIZED(SELECT e.id,e.warehouse_id,wms_serial_event_valid_read_source(e.tenant,e.warehouse_id,e.id) AS ok FROM wms_events e JOIN (SELECT DISTINCT event_id FROM k) selected ON selected.event_id=e.id WHERE e.tenant=$1) SELECT k.key,COALESCE(events.ok,false) AS ok FROM k JOIN wms_serial_observations o ON o.tenant=$1 AND o.sku=$2 AND o.sku_version=k.sku_version AND o.event_id=k.event_id AND o.ordinal=k.ordinal LEFT JOIN events ON events.id=k.event_id AND events.id=o.event_id AND events.warehouse_id=o.warehouse_id",
    };
    let rows = sqlx::query(sql)
        .bind(t)
        .bind(&q.sku)
        .bind(json!(keys))
        .fetch_all(c)
        .await?;
    rows.into_iter()
        .map(|row| {
            Ok((
                serde_json::from_value(row.try_get("key")?)
                    .map_err(|_| Error::new(503, "Stored serial page source key is invalid"))?,
                row.try_get("ok")?,
            ))
        })
        .collect()
}
