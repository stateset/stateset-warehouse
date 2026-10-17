//! Durable native intent and lease journal. No transaction spans remote HTTP.
use crate::{
    domain,
    native::PreparedTransport,
    store::{Error, Identity, Result, Store},
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use sqlx::{PgConnection, Row};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NativeRecord {
    pub request_id: String,
    pub kind: String,
    pub actor: String,
    pub digest: String,
    pub expected_version: i64,
    pub order_id: Option<String>,
    pub status: String,
    pub attempts: i64,
    pub frozen: Value,
    pub source_snapshot: Option<Value>,
    pub response: Option<Value>,
    pub last_error: String,
    pub lease_active: bool,
}
impl NativeRecord {
    pub fn public(&self, replayed: bool) -> Value {
        json!({"found":true,"request_id":self.request_id,"kind":self.kind,"status":self.status,"order_id":self.order_id,"attempts":self.attempts,"replayed":replayed,"response":self.response,"last_error":self.last_error,"in_progress":self.lease_active,"source_snapshot":self.source_snapshot,"evidence":self.frozen})
    }
    pub fn matches(&self, i: &Identity, kind: &str, digest: &str) -> Result<()> {
        if self.actor != i.actor || self.kind != kind || self.digest != digest {
            return Err(Error::new(
                409,
                "Native request ID already belongs to a different payload or operator",
            ));
        }
        Ok(())
    }
}
fn record(row: sqlx::postgres::PgRow) -> Result<NativeRecord> {
    Ok(NativeRecord {
        request_id: row.try_get("request_id")?,
        kind: row.try_get("kind")?,
        actor: row.try_get("actor")?,
        digest: row.try_get("digest")?,
        expected_version: row.try_get("expected_version")?,
        order_id: row.try_get("order_id")?,
        status: row.try_get("status")?,
        attempts: row.try_get("attempts")?,
        frozen: row.try_get("frozen")?,
        source_snapshot: row.try_get("source_snapshot")?,
        response: row.try_get("response")?,
        last_error: row.try_get("last_error")?,
        lease_active: row.try_get("lease_active")?,
    })
}
pub fn digest(kind: &str, warehouse: &str, order: Option<&str>, input: &Value) -> Result<String> {
    let bytes = serde_json::to_vec(&json!([kind, warehouse, order, input]))
        .map_err(|_| Error::new(422, "Invalid native request"))?;
    Ok(format!("{:x}", Sha256::digest(bytes)))
}
pub fn import_command_id(tenant: &str, warehouse: &str, request: &str) -> String {
    format!(
        "native-import-{:x}",
        Sha256::digest(format!("{tenant}\0{warehouse}\0{request}").as_bytes())
    )
}
pub fn request_id(value: &str) -> Result<()> {
    if !(8..=100).contains(&value.len())
        || !value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_".contains(&b))
    {
        return Err(Error::new(
            422,
            "Native request ID must contain 8–100 letters, digits, hyphens or underscores",
        ));
    }
    Ok(())
}
pub async fn warehouse_version(store: &Store, tenant: &str, warehouse: &str) -> Result<i64> {
    sqlx::query_scalar("SELECT version FROM wms_warehouses WHERE tenant=$1 AND warehouse_id=$2")
        .bind(tenant)
        .bind(warehouse)
        .fetch_optional(&store.pool)
        .await?
        .ok_or_else(|| Error::new(404, "Warehouse not found"))
}
async fn get(
    c: &mut PgConnection,
    tenant: &str,
    warehouse: &str,
    request: &str,
    lock: bool,
) -> Result<Option<NativeRecord>> {
    let sql = if lock {
        "SELECT *,COALESCE(lease_until>now(),false) AS lease_active FROM wms_native_requests WHERE tenant=$1 AND warehouse_id=$2 AND request_id=$3 FOR UPDATE"
    } else {
        "SELECT *,COALESCE(lease_until>now(),false) AS lease_active FROM wms_native_requests WHERE tenant=$1 AND warehouse_id=$2 AND request_id=$3"
    };
    sqlx::query(sql)
        .bind(tenant)
        .bind(warehouse)
        .bind(request)
        .fetch_optional(&mut *c)
        .await?
        .map(record)
        .transpose()
}
pub async fn lookup(
    store: &Store,
    i: &Identity,
    warehouse: &str,
    request: &str,
) -> Result<Option<NativeRecord>> {
    let mut c = store.pool.acquire().await?;
    get(&mut c, &i.tenant, warehouse, request, false).await
}
pub async fn attempts(
    store: &Store,
    i: &Identity,
    warehouse: &str,
    request: &str,
) -> Result<Vec<Value>> {
    let rows=sqlx::query("SELECT jsonb_build_object('id',id,'outcome',outcome,'detail',detail,'response',response,'started_at',started_at,'finished_at',finished_at) AS evidence FROM wms_native_attempts WHERE tenant=$1 AND warehouse_id=$2 AND request_id=$3 ORDER BY started_at,id LIMIT 100").bind(&i.tenant).bind(warehouse).bind(request).fetch_all(&store.pool).await?;
    rows.into_iter()
        .map(|r| Ok(r.try_get("evidence")?))
        .collect()
}
pub struct Freeze<'a> {
    pub kind: &'a str,
    pub request_id: &'a str,
    pub digest: &'a str,
    pub input: &'a Value,
    pub expected_version: i64,
    pub order_id: Option<&'a str>,
    pub source_snapshot: Option<&'a Value>,
    pub frozen: &'a Value,
}
async fn import_source_available(
    c: &mut PgConnection,
    i: &Identity,
    warehouse: &str,
    request: &str,
    source: &str,
) -> Result<()> {
    let occupied:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM wms_orders WHERE tenant=$1 AND source_order_id=$2) OR EXISTS(SELECT 1 FROM wms_native_requests WHERE tenant=$1 AND kind='oms_import' AND status IN('frozen','completed') AND frozen#>>'{command_request,command,source_order_id}'=$2 AND (warehouse_id<>$3 OR request_id<>$4))").bind(&i.tenant).bind(source).bind(warehouse).bind(request).fetch_one(&mut *c).await?;
    if occupied {
        return Err(Error::new(409,"This native OMS source already belongs to a warehouse order or retained import intent; recover the original request instead of importing it again"));
    }
    Ok(())
}
pub async fn check_import_source(
    store: &Store,
    i: &Identity,
    warehouse: &str,
    request: &str,
    source: &str,
) -> Result<()> {
    let mut c = store.pool.acquire().await?;
    import_source_available(&mut c, i, warehouse, request, source).await
}
pub async fn freeze(
    store: &Store,
    i: &Identity,
    warehouse: &str,
    f: Freeze<'_>,
) -> Result<(NativeRecord, bool)> {
    let mut tx = store.pool.begin().await?;
    let version: i64 = sqlx::query_scalar(
        "SELECT version FROM wms_warehouses WHERE tenant=$1 AND warehouse_id=$2 FOR UPDATE",
    )
    .bind(&i.tenant)
    .bind(warehouse)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(|| Error::new(404, "Warehouse not found"))?;
    if let Some(existing) = get(&mut tx, &i.tenant, warehouse, f.request_id, true).await? {
        existing.matches(i, f.kind, f.digest)?;
        tx.commit().await?;
        return Ok((existing, true));
    }
    if version != f.expected_version {
        return Err(Error::new(409,format!("Warehouse changed (version {version}); refresh and review before starting a new native request")));
    }
    if f.kind == "oms_import" {
        if let Some(source) = f.frozen["command_request"]["command"]["source_order_id"]
            .as_str()
            .filter(|v| v.starts_with("oms:"))
        {
            // The warehouse head serializes local operations; this source lock
            // also serializes full imports competing in different warehouses.
            let scope = json!([&i.tenant, source]).to_string();
            sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1,0))")
                .bind(scope)
                .execute(&mut *tx)
                .await?;
            import_source_available(&mut tx, i, warehouse, f.request_id, source).await?;
        }
    }
    if f.kind == "tms_transport" {
        let order = f
            .order_id
            .ok_or_else(|| Error::new(422, "Transport order identity required"))?;
        ensure_ready(&mut tx, &i.tenant, warehouse, order).await?;
        let existing:Option<String>=sqlx::query_scalar("SELECT request_id FROM wms_native_requests WHERE tenant=$1 AND warehouse_id=$2 AND kind='tms_transport' AND order_id=$3").bind(&i.tenant).bind(warehouse).bind(order).fetch_optional(&mut *tx).await?;
        if existing.is_some() {
            return Err(Error::new(409,"This order already has a transport intent; recover its original request instead of creating another"));
        }
    }
    sqlx::query("INSERT INTO wms_native_requests(tenant,warehouse_id,request_id,kind,actor,digest,input,expected_version,order_id,source_snapshot,frozen) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11)").bind(&i.tenant).bind(warehouse).bind(f.request_id).bind(f.kind).bind(&i.actor).bind(f.digest).bind(f.input).bind(f.expected_version).bind(f.order_id).bind(f.source_snapshot).bind(f.frozen).execute(&mut *tx).await?;
    let result = get(&mut tx, &i.tenant, warehouse, f.request_id, false)
        .await?
        .ok_or_else(|| Error::new(503, "Native intent could not be retained"))?;
    tx.commit().await?;
    Ok((result, false))
}
async fn ensure_ready(
    c: &mut PgConnection,
    tenant: &str,
    warehouse: &str,
    order: &str,
) -> Result<()> {
    let status: Option<String> = sqlx::query_scalar(
        "SELECT status FROM wms_orders WHERE tenant=$1 AND warehouse_id=$2 AND id=$3",
    )
    .bind(tenant)
    .bind(warehouse)
    .bind(order)
    .fetch_optional(&mut *c)
    .await?;
    if status.as_deref() != Some("packed") {
        return Err(Error::new(
            409,
            "Transport requires a fully packed warehouse order awaiting physical handoff",
        ));
    }
    let blocked:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM wms_exceptions WHERE tenant=$1 AND warehouse_id=$2 AND order_id=$3 AND status='open')").bind(tenant).bind(warehouse).bind(order).fetch_one(&mut *c).await?;
    if blocked {
        return Err(Error::new(
            409,
            "Resolve this order's open exception before transportation planning",
        ));
    }
    Ok(())
}
pub async fn complete_import(
    store: &Store,
    i: &Identity,
    warehouse: &str,
    request: &str,
    response: &Value,
) -> Result<NativeRecord> {
    sqlx::query("UPDATE wms_native_requests SET status='completed',response=$1,order_id=$2,last_error='',updated_at=now() WHERE tenant=$3 AND warehouse_id=$4 AND request_id=$5 AND actor=$6 AND kind='oms_import' AND status<>'completed'").bind(response).bind(response["result"]["entity_id"].as_str()).bind(&i.tenant).bind(warehouse).bind(request).bind(&i.actor).execute(&store.pool).await?;
    lookup(store, i, warehouse, request)
        .await?
        .ok_or_else(|| Error::new(503, "Native import receipt unavailable"))
}
pub async fn local_conflict(
    store: &Store,
    i: &Identity,
    warehouse: &str,
    request: &str,
    message: &str,
) -> Result<()> {
    sqlx::query("UPDATE wms_native_requests SET status='local_conflict',last_error=$1,updated_at=now() WHERE tenant=$2 AND warehouse_id=$3 AND request_id=$4 AND actor=$5 AND kind='oms_import' AND status<>'completed'").bind(message).bind(&i.tenant).bind(warehouse).bind(request).bind(&i.actor).execute(&store.pool).await?;
    Ok(())
}
pub enum Claim {
    Ready { record: NativeRecord, lease: String },
    Existing(NativeRecord),
}
pub async fn claim_transport(
    store: &Store,
    i: &Identity,
    warehouse: &str,
    request: &str,
    digest: &str,
) -> Result<Claim> {
    let mut tx = store.pool.begin().await?;
    let _version: i64 = sqlx::query_scalar(
        "SELECT version FROM wms_warehouses WHERE tenant=$1 AND warehouse_id=$2 FOR UPDATE",
    )
    .bind(&i.tenant)
    .bind(warehouse)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(|| Error::new(404, "Warehouse not found"))?;
    let record = get(&mut tx, &i.tenant, warehouse, request, true)
        .await?
        .ok_or_else(|| Error::new(404, "Native transport request not found"))?;
    record.matches(i, "tms_transport", digest)?;
    if record.status == "completed" || record.lease_active {
        tx.commit().await?;
        return Ok(Claim::Existing(record));
    }
    if record.attempts == 0 {
        ensure_ready(
            &mut tx,
            &i.tenant,
            warehouse,
            record
                .order_id
                .as_deref()
                .ok_or_else(|| Error::new(503, "Stored transport lacks order identity"))?,
        )
        .await?;
        // Other inventory work may advance the warehouse after preparation.
        // Recheck the exact order facts rather than block on unrelated writes.
        let prepared = prepared(&record)?;
        let order=sqlx::query("SELECT reference,source_order_id FROM wms_orders WHERE tenant=$1 AND warehouse_id=$2 AND id=$3").bind(&i.tenant).bind(warehouse).bind(&prepared.order_id).fetch_one(&mut *tx).await?;
        if order.try_get::<String, _>("reference")? != prepared.readiness.order_reference
            || order.try_get::<Option<String>, _>("source_order_id")?
                != prepared.readiness.source_order_ref
        {
            return Err(Error::new(409,"Warehouse order identity changed before first transport send; retained intent needs review"));
        }
        let lines=sqlx::query("SELECT id,sku,quantity FROM wms_order_lines WHERE tenant=$1 AND warehouse_id=$2 AND order_id=$3").bind(&i.tenant).bind(warehouse).bind(&prepared.order_id).fetch_all(&mut *tx).await?;
        if lines.len() != prepared.readiness.lines.len()
            || lines.iter().any(|line| {
                !prepared.readiness.lines.iter().any(|r| {
                    r.line_id == line.get::<String, _>("id")
                        && r.sku == line.get::<String, _>("sku")
                        && r.quantity_each == line.get::<i64, _>("quantity")
                })
            })
        {
            return Err(Error::new(409,"Warehouse order lines changed before first transport send; retained intent needs review"));
        }
    }
    sqlx::query("UPDATE wms_native_attempts SET outcome='uncertain',detail='Lease expired before outcome was committed; remote acceptance is uncertain',finished_at=now() WHERE tenant=$1 AND warehouse_id=$2 AND request_id=$3 AND outcome='started'").bind(&i.tenant).bind(warehouse).bind(request).execute(&mut *tx).await?;
    let lease = domain::id();
    sqlx::query(
        "INSERT INTO wms_native_attempts(id,tenant,warehouse_id,request_id) VALUES($1,$2,$3,$4)",
    )
    .bind(&lease)
    .bind(&i.tenant)
    .bind(warehouse)
    .bind(request)
    .execute(&mut *tx)
    .await?;
    sqlx::query("UPDATE wms_native_requests SET status='sending',attempts=attempts+1,lease_token=$1,lease_until=now()+interval '30 seconds',updated_at=now() WHERE tenant=$2 AND warehouse_id=$3 AND request_id=$4").bind(&lease).bind(&i.tenant).bind(warehouse).bind(request).execute(&mut *tx).await?;
    let record = get(&mut tx, &i.tenant, warehouse, request, false)
        .await?
        .ok_or_else(|| Error::new(503, "Transport claim unavailable"))?;
    tx.commit().await?;
    Ok(Claim::Ready { record, lease })
}
pub fn prepared(record: &NativeRecord) -> Result<PreparedTransport> {
    serde_json::from_value(record.frozen.clone())
        .map_err(|_| Error::new(503, "Stored native transport plan requires repair"))
}
pub async fn finish_transport(
    store: &Store,
    i: &Identity,
    warehouse: &str,
    request: &str,
    lease: &str,
    outcome: std::result::Result<Value, String>,
) -> Result<NativeRecord> {
    let (status, response, message) = match outcome {
        Ok(value) => ("completed", Some(value), String::new()),
        Err(error) => ("uncertain", None, error),
    };
    let mut tx = store.pool.begin().await?;
    let n=sqlx::query("UPDATE wms_native_requests SET status=$1,response=$2,last_error=$3,lease_token=NULL,lease_until=NULL,updated_at=now() WHERE tenant=$4 AND warehouse_id=$5 AND request_id=$6 AND actor=$7 AND lease_token=$8 AND status='sending'").bind(status).bind(&response).bind(&message).bind(&i.tenant).bind(warehouse).bind(request).bind(&i.actor).bind(lease).execute(&mut *tx).await?.rows_affected();
    if n == 1 {
        sqlx::query("UPDATE wms_native_attempts SET outcome=$1,response=$2,detail=$3,finished_at=now() WHERE id=$4 AND outcome='started'").bind(if status=="completed"{"accepted"}else{"uncertain"}).bind(&response).bind(&message).bind(lease).execute(&mut *tx).await?;
    }
    let result = get(&mut tx, &i.tenant, warehouse, request, false)
        .await?
        .ok_or_else(|| {
            Error::new(
                503,
                "Native transport receipt unavailable; recover the same request",
            )
        })?;
    tx.commit().await?;
    Ok(result)
}
