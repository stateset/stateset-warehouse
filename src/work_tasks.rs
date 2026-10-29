//! Putaway ownership and reviewed execution. Inventory writes always use Store::execute.
use crate::{
    api::App,
    concurrency,
    domain::Command,
    master_data::Capacity,
    putaway,
    store::{Error, Identity, Request, Store},
};
use axum::{
    extract::{Path, Query, State},
    Extension, Json,
};
use serde::Deserialize;
use serde_json::{json, Value};
use sqlx::{PgConnection, Row};
use std::collections::BTreeMap;

fn role(i: &Identity, write: bool) -> Result<(), Error> {
    if ["admin", "operator"].contains(&i.role.as_str()) || !write && i.role == "viewer" {
        Ok(())
    } else {
        Err(Error::new(
            403,
            "Your role does not permit this work action",
        ))
    }
}
async fn lock(c: &mut PgConnection, i: &Identity, w: &str) -> Result<(), Error> {
    sqlx::query(
        "SELECT version FROM wms_warehouses WHERE tenant=$1 AND warehouse_id=$2 FOR UPDATE",
    )
    .bind(&i.tenant)
    .bind(w)
    .fetch_optional(c)
    .await?
    .ok_or_else(|| Error::new(404, "Warehouse not found"))?;
    Ok(())
}
async fn task(
    c: &mut PgConnection,
    i: &Identity,
    w: &str,
    id: &str,
    revision: i64,
) -> Result<Value, Error> {
    let t: Value = sqlx::query_scalar("SELECT to_jsonb(t)-'tenant' FROM wms_work_tasks t WHERE tenant=$1 AND warehouse_id=$2 AND id=$3")
        .bind(&i.tenant).bind(w).bind(id).fetch_optional(c).await?.ok_or_else(|| Error::new(404, "Work task not found"))?;
    if t["revision"] != revision {
        return Err(Error::new(409, "Work changed. Refresh before acting."));
    }
    if ["completed", "cancelled"].contains(&t["status"].as_str().unwrap_or_default()) {
        return Err(Error::new(409, "Work is already closed"));
    }
    Ok(t)
}
#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Page {
    view: Option<String>,
    offset: Option<i64>,
}
pub async fn list(
    State(app): State<App>,
    Extension(i): Extension<Identity>,
    Path(w): Path<String>,
    Query(q): Query<Page>,
) -> Result<Json<Value>, Error> {
    role(&i, false)?;
    let view = q.view.as_deref().unwrap_or("mine");
    let offset = q.offset.unwrap_or(0);
    if !["mine", "available", "all", "completed"].contains(&view)
        || !(0..=1_000_000).contains(&offset)
    {
        return Err(Error::new(422, "Invalid work page"));
    }
    let exists: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM wms_warehouses WHERE tenant=$1 AND warehouse_id=$2)",
    )
    .bind(&i.tenant)
    .bind(&w)
    .fetch_one(&app.store.pool)
    .await?;
    if !exists {
        return Err(Error::new(404, "Warehouse not found"));
    }
    let data: Value = sqlx::query_scalar("WITH scoped AS (SELECT * FROM wms_work_tasks WHERE tenant=$1 AND warehouse_id=$2), filtered AS (SELECT * FROM scoped WHERE CASE $4 WHEN 'mine' THEN owner=$3 AND status NOT IN('completed','cancelled') WHEN 'available' THEN status='ready' WHEN 'completed' THEN status IN('completed','cancelled') ELSE status NOT IN('completed','cancelled') END), page AS (SELECT * FROM filtered ORDER BY priority DESC,due_at NULLS LAST,seq LIMIT 50 OFFSET $5) SELECT jsonb_build_object('tasks',COALESCE((SELECT jsonb_agg(to_jsonb(p)-'tenant' ORDER BY priority DESC,due_at NULLS LAST,seq) FROM page p),'[]'::jsonb),'total',(SELECT count(*) FROM filtered),'counts',jsonb_build_object('ready',(SELECT count(*) FROM scoped WHERE status='ready'),'mine',(SELECT count(*) FROM scoped WHERE owner=$3 AND status NOT IN('completed','cancelled')),'blocked',(SELECT count(*) FROM scoped WHERE status='blocked'),'active',(SELECT count(*) FROM scoped WHERE status NOT IN('completed','cancelled'))))")
        .bind(&i.tenant).bind(&w).bind(&i.actor).bind(view).bind(offset).fetch_one(&app.store.pool).await?;
    Ok(Json(data))
}
pub async fn discover(
    State(app): State<App>,
    Extension(i): Extension<Identity>,
    Path(w): Path<String>,
) -> Result<Json<Value>, Error> {
    role(&i, true)?;
    let mut tx = app.store.pool.begin().await?;
    lock(&mut tx, &i, &w).await?;
    // All open work, including paused/blocked work, already accounts for these units.
    let rows = sqlx::query("WITH candidates AS (SELECT s.sku,s.lot,s.location,s.on_hand-s.reserved-COALESCE((SELECT sum(t.quantity-t.completed_quantity) FROM wms_work_tasks t WHERE t.tenant=s.tenant AND t.warehouse_id=s.warehouse_id AND t.sku=s.sku AND t.lot=s.lot AND t.from_location=s.location AND t.status NOT IN('completed','cancelled')),0)::bigint AS remaining FROM wms_stock s JOIN wms_locations l ON (l.tenant,l.warehouse_id,l.code)=(s.tenant,s.warehouse_id,s.location) WHERE s.tenant=$1 AND s.warehouse_id=$2 AND l.data->>'kind'='receiving') SELECT * FROM candidates WHERE remaining>0 ORDER BY location,sku,lot LIMIT 200")
        .bind(&i.tenant).bind(&w).fetch_all(&mut *tx).await?;
    let mut created = 0;
    for row in rows {
        let remaining: i64 = row.get("remaining");
        if remaining <= 0 {
            continue;
        }
        if created == 200 {
            break;
        }
        sqlx::query("INSERT INTO wms_work_tasks(tenant,warehouse_id,id,sku,lot,from_location,quantity,created_by,updated_by) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$8)")
            .bind(&i.tenant).bind(&w).bind(crate::domain::id()).bind(row.get::<String,_>("sku")).bind(row.get::<String,_>("lot")).bind(row.get::<String,_>("location")).bind(remaining).bind(&i.actor).execute(&mut *tx).await?;
        created += 1;
    }
    tx.commit().await?;
    Ok(Json(json!({"created":created,"limit":200})))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Action {
    revision: i64,
    action: String,
    reason: Option<String>,
    priority: Option<i32>,
}
pub async fn action(
    State(app): State<App>,
    Extension(i): Extension<Identity>,
    Path((w, id)): Path<(String, String)>,
    Json(a): Json<Action>,
) -> Result<Json<Value>, Error> {
    role(&i, true)?;
    let mut tx = app.store.pool.begin().await?;
    lock(&mut tx, &i, &w).await?;
    let t = task(&mut tx, &i, &w, &id, a.revision).await?;
    let status = t["status"].as_str().unwrap_or_default();
    let own = t["owner"] == i.actor;
    let mut owner = t["owner"].as_str().map(str::to_owned);
    let mut next = status;
    let mut priority = t["priority"].as_i64().unwrap_or(0) as i32;
    let reason = a.reason.as_deref().map(str::trim).filter(|s| !s.is_empty());
    if reason.is_some_and(|s| s.chars().count() > 500) {
        return Err(Error::new(422, "Reason must be at most 500 characters"));
    }
    match a.action.as_str() {
        "claim" if status == "ready" => {
            next = "claimed";
            owner = Some(i.actor.clone());
        }
        "pause" if own && status == "claimed" => next = "paused",
        "block" if own && ["claimed", "paused"].contains(&status) => next = "blocked",
        "resume" if own && ["paused", "blocked"].contains(&status) => next = "claimed",
        "release" if own || i.role == "admin" => {
            next = "ready";
            owner = None;
        }
        "cancel" if i.role == "admin" => next = "cancelled",
        "priority" if i.role == "admin" && a.priority.is_some_and(|p| (0..=2).contains(&p)) => {
            priority = a.priority.unwrap_or(0)
        }
        _ => {
            return Err(Error::new(
                409,
                "Action is unavailable for this task and operator",
            ))
        }
    }
    if ["block", "cancel"].contains(&a.action.as_str()) && reason.is_none() {
        return Err(Error::new(422, "Explain why this work cannot continue"));
    }
    let next_reason = if a.action == "priority" {
        t["reason"].as_str()
    } else {
        reason
    };
    let result: Value = sqlx::query_scalar("UPDATE wms_work_tasks t SET status=$4,owner=$5,priority=$6,reason=$7,updated_by=$8,last_action=$9 WHERE tenant=$1 AND warehouse_id=$2 AND id=$3 RETURNING to_jsonb(t)-'tenant'")
        .bind(&i.tenant).bind(&w).bind(&id).bind(next).bind(owner).bind(priority).bind(next_reason).bind(&i.actor).bind(&a.action).fetch_one(&mut *tx).await?;
    tx.commit().await?;
    Ok(Json(result))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Review {
    revision: i64,
    quantity: Option<i64>,
}
pub async fn review(
    State(app): State<App>,
    Extension(i): Extension<Identity>,
    Path((w, id)): Path<(String, String)>,
    Json(input): Json<Review>,
) -> Result<Json<Value>, Error> {
    role(&i, true)?;
    let quantity = input.quantity.unwrap_or(10_000);
    if !(1..=10_000).contains(&quantity) {
        return Err(Error::new(422, "Review 1–10,000 each units at a time"));
    }
    let mut tx = app.store.pool.begin().await?;
    lock(&mut tx, &i, &w).await?;
    let t = task(&mut tx, &i, &w, &id, input.revision).await?;
    if t["owner"] != i.actor || t["status"] != "claimed" {
        return Err(Error::new(
            409,
            "Claim and resume this work before reviewing a move",
        ));
    }
    let capacity: BTreeMap<String, Capacity> = sqlx::query_as::<_,(String,i64,Option<i32>)>("SELECT code,max_units,max_skus FROM wms_location_capacity WHERE tenant=$1 AND warehouse_id=$2")
        .bind(&i.tenant).bind(&w).fetch_all(&mut *tx).await?.into_iter().map(|(code,max_units,max_skus)| (code,Capacity {max_units,max_skus:max_skus.map(i64::from)})).collect();
    let remaining =
        t["quantity"].as_i64().unwrap_or(0) - t["completed_quantity"].as_i64().unwrap_or(0);
    let state = Store::load(&mut tx, &i.tenant, &w, None, false).await?;
    let plan = putaway::plan_source(
        &state,
        &putaway::PlanRequest {
            from: vec![t["from_location"].as_str().unwrap_or_default().into()],
            skus: vec![t["sku"].as_str().unwrap_or_default().into()],
            ..Default::default()
        },
        &capacity,
        Some((
            t["lot"].as_str().unwrap_or_default(),
            remaining.min(quantity),
        )),
    )
    .map_err(|e| Error::new(422, e))?;
    let suggested = plan["tasks"].as_array().and_then(|items| items.iter().find(|p| p["command"]["lot"] == t["lot"])).ok_or_else(|| Error::new(409,"No available stock or eligible destination. Refresh stock or block this task with a reason."))?;
    let mut command: Command = serde_json::from_value(suggested["command"].clone())
        .map_err(|_| Error::new(503, "Invalid putaway plan"))?;
    if let Command::MoveStock { quantity, .. } = &mut command {
        *quantity = (*quantity).min(remaining).min(10_000);
    }
    let revision: i64 = sqlx::query_scalar("UPDATE wms_work_tasks SET updated_by=$4,last_action='reviewed' WHERE tenant=$1 AND warehouse_id=$2 AND id=$3 RETURNING revision")
        .bind(&i.tenant).bind(&w).bind(&id).bind(&i.actor).fetch_one(&mut *tx).await?;
    let mut versions = concurrency::reviewed_versions(&state, &command)
        .map_err(|e| Error::new(422, e))?
        .ok_or_else(|| Error::new(503, "Missing move review"))?;
    let to = suggested["command"]["to"].as_str().unwrap_or_default();
    for key in [
        concurrency::key(&["work_task", &id]),
        concurrency::key(&["putaway_bin", to]),
    ] {
        let value: Option<i64> = sqlx::query_scalar("SELECT revision FROM wms_resource_versions WHERE tenant=$1 AND warehouse_id=$2 AND resource=$3").bind(&i.tenant).bind(&w).bind(&key).fetch_optional(&mut *tx).await?;
        versions.insert(key, value.unwrap_or(0));
    }
    let request = Request {
        request_id: crate::domain::id(),
        expected_version: state.version,
        resource_versions: Some(versions),
        unit_inputs: vec![],
        serial_inputs: vec![],
        command,
    };
    sqlx::query("INSERT INTO wms_work_executions(tenant,warehouse_id,request_id,task_id,task_revision,actor,command,expected_version,resource_versions) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9)")
        .bind(&i.tenant).bind(&w).bind(&request.request_id).bind(&id).bind(revision).bind(&i.actor).bind(json!(request.command)).bind(request.expected_version).bind(json!(request.resource_versions)).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(Json(
        json!({"task_id":id,"revision":revision,"request":request,"reason":suggested["reason"],"capacity_limited":suggested["capacity_limited"]}),
    ))
}
/// The standard command journal carries the registered request ID unchanged.
/// The existing resource fence proves a stale review can never execute later.
pub(crate) async fn admit(
    c: &mut PgConnection,
    i: &Identity,
    w: &str,
    r: &Request,
    required: &mut BTreeMap<String, i64>,
) -> Result<(), Error> {
    // Only moves execute putaway work. The receipt trigger independently
    // rejects a registered review ID attached to any other command type.
    if !matches!(r.command, Command::MoveStock { .. }) {
        return Ok(());
    }
    let execution = sqlx::query("SELECT actor,command,expected_version,resource_versions,task_id FROM wms_work_executions WHERE tenant=$1 AND warehouse_id=$2 AND request_id=$3")
        .bind(&i.tenant).bind(w).bind(&r.request_id).fetch_optional(c).await?;
    let Some(x) = execution else {
        return Ok(());
    };
    let versions: BTreeMap<String, i64> =
        serde_json::from_value(x.get::<Value, _>("resource_versions"))
            .map_err(|_| Error::new(503, "Stored work review requires repair"))?;
    if x.get::<String, _>("actor") != i.actor
        || x.get::<Value, _>("command") != json!(r.command)
        || x.get::<i64, _>("expected_version") != r.expected_version
        || r.resource_versions.as_ref() != Some(&versions)
        || !r.unit_inputs.is_empty()
    {
        return Err(Error::new(
            422,
            "Use the original work review, operator and request",
        ));
    }
    if let Command::MoveStock { to, .. } = &r.command {
        let id: String = x.get("task_id");
        for key in [
            concurrency::key(&["work_task", &id]),
            concurrency::key(&["putaway_bin", to]),
        ] {
            let value = versions
                .get(&key)
                .ok_or_else(|| Error::new(503, "Stored work review requires repair"))?;
            required.insert(key, *value);
        }
    }
    Ok(())
}
