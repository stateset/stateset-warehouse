use crate::domain::{self, Command, State, Warehouse};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use sqlx::{postgres::PgPoolOptions, PgPool, Row};
use std::time::Duration;

#[derive(Debug)]
pub struct Error {
    pub status: u16,
    pub message: String,
}
impl Error {
    pub fn new(status: u16, message: impl Into<String>) -> Self {
        Self {
            status,
            message: message.into(),
        }
    }
}
impl From<sqlx::Error> for Error {
    fn from(e: sqlx::Error) -> Self {
        tracing::error!(error=%e,"warehouse database operation failed");
        Self::new(
            503,
            "Database operation unavailable; retry with the same request ID",
        )
    }
}
pub type Result<T> = std::result::Result<T, Error>;
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub request_id: String,
    pub expected_version: i64,
    pub command: Command,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Identity {
    pub tenant: String,
    pub actor: String,
    pub role: String,
}
#[derive(Clone)]
pub struct Store {
    pub pool: PgPool,
}
impl Store {
    pub async fn connect(url: &str) -> Result<Self> {
        let pool = PgPoolOptions::new()
            .max_connections(8)
            .acquire_timeout(Duration::from_secs(5))
            .after_connect(|c, _| {
                Box::pin(async move {
                    sqlx::query("SET statement_timeout = '10s'")
                        .execute(&mut *c)
                        .await?;
                    sqlx::query("SET lock_timeout = '5s'")
                        .execute(&mut *c)
                        .await?;
                    Ok(())
                })
            })
            .connect(url)
            .await?;
        let mut tx = pool.begin().await?;
        sqlx::query("SELECT pg_advisory_xact_lock(84721821)")
            .execute(&mut *tx)
            .await?;
        sqlx::raw_sql(include_str!("../migrations/001_initial.sql"))
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(Self { pool })
    }
    pub async fn provision(&self, tenant: &str, demo: bool) -> Result<()> {
        let state = if demo {
            domain::demo_state()
        } else {
            State::empty(Warehouse {
                id: "wh-main".into(),
                code: "MAIN".into(),
                name: "Main Warehouse".into(),
                city: "Configure facility before live operations".into(),
                timezone: "UTC".into(),
            })
        };
        sqlx::query("INSERT INTO wms_workspaces (tenant,warehouse_id,version,state) VALUES ($1,$2,$3,$4) ON CONFLICT DO NOTHING")
            .bind(tenant).bind(&state.warehouse.id).bind(state.version).bind(json!(state)).execute(&self.pool).await?;
        Ok(())
    }
    pub async fn list(&self, tenant: &str) -> Result<Vec<Value>> {
        let rows=sqlx::query("SELECT state->'warehouse' AS warehouse,version FROM wms_workspaces WHERE tenant=$1 ORDER BY warehouse_id LIMIT 100")
            .bind(tenant).fetch_all(&self.pool).await?;
        rows.iter().map(|r|Ok(json!({"warehouse":r.try_get::<Value,_>("warehouse")?,"version":r.try_get::<i64,_>("version")?}))).collect()
    }
    pub async fn state(&self, tenant: &str, warehouse: &str) -> Result<State> {
        let row =
            sqlx::query("SELECT state FROM wms_workspaces WHERE tenant=$1 AND warehouse_id=$2")
                .bind(tenant)
                .bind(warehouse)
                .fetch_optional(&self.pool)
                .await?
                .ok_or_else(|| Error::new(404, "Warehouse not found"))?;
        serde_json::from_value(row.try_get("state")?)
            .map_err(|_| Error::new(503, "Stored warehouse requires repair; writes are blocked"))
    }
    pub async fn execute(
        &self,
        identity: &Identity,
        warehouse: &str,
        request: &Request,
    ) -> Result<Value> {
        if !request.command.allowed(&identity.role) {
            return Err(Error::new(403, "Your role does not permit this action"));
        }
        if request.request_id.len() > 100
            || request.request_id.len() < 8
            || request
                .request_id
                .chars()
                .any(|c| !c.is_ascii_alphanumeric() && c != '-' && c != '_')
        {
            return Err(Error::new(
                422,
                "Request ID must contain 8–100 letters, digits, hyphens or underscores",
            ));
        }
        let digest = format!(
            "{:x}",
            Sha256::digest(
                serde_json::to_vec(request).map_err(|_| Error::new(422, "Invalid command"))?
            )
        );
        let mut tx = self.pool.begin().await?;
        let row=sqlx::query("SELECT state,version FROM wms_workspaces WHERE tenant=$1 AND warehouse_id=$2 FOR UPDATE")
            .bind(&identity.tenant).bind(warehouse).fetch_optional(&mut *tx).await?.ok_or_else(||Error::new(404,"Warehouse not found"))?;
        if let Some(receipt)=sqlx::query("SELECT actor,digest,result FROM wms_commands WHERE tenant=$1 AND warehouse_id=$2 AND request_id=$3")
            .bind(&identity.tenant).bind(warehouse).bind(&request.request_id).fetch_optional(&mut *tx).await? {
            if receipt.try_get::<String,_>("digest")?!=digest || receipt.try_get::<String,_>("actor")?!=identity.actor {
                return Err(Error::new(409,"Request ID already belongs to a different payload or operator"));
            }
            return Ok(json!({"replayed":true,"result":receipt.try_get::<Value,_>("result")?}));
        }
        let version: i64 = row.try_get("version")?;
        if request.expected_version != version {
            return Err(Error::new(409,format!("Warehouse changed (version {version}). Refresh and review before submitting a new request.")));
        }
        let state: State = serde_json::from_value(row.try_get("state")?)
            .map_err(|_| Error::new(503, "Stored warehouse requires repair"))?;
        if identity.role == "oms" {
            if let Command::CancelOrder { order_id, .. } = &request.command {
                if !state
                    .orders
                    .iter()
                    .any(|o| o.id == *order_id && o.source_order_id.is_some())
                {
                    return Err(Error::new(403, "OMS can cancel only source-linked orders"));
                }
            }
        }
        state.validate().map_err(|_| {
            Error::new(
                503,
                "Warehouse reconciliation failed; writes are blocked pending repair",
            )
        })?;
        if state.version != version {
            return Err(Error::new(
                503,
                "Warehouse version mismatch; repair required",
            ));
        }
        let (next, result) = state
            .execute(&request.command, &identity.actor)
            .map_err(|m| Error::new(422, m))?;
        let bytes =
            serde_json::to_vec(&next).map_err(|_| Error::new(500, "Failed to encode warehouse"))?;
        if bytes.len() > 8 * 1024 * 1024 {
            return Err(Error::new(409,"Warehouse reached this version's 8 MiB aggregate limit; migrate history before accepting more work"));
        }
        let event_id = domain::id();
        let order = next.orders.iter().find(|o| result["entity_id"] == o.id);
        let event = json!({"schema_version":1,"id":event_id,"tenant":identity.tenant,"warehouse_id":warehouse,"version":next.version,"type":request.command.kind(),"actor":identity.actor,"occurred_at":domain::now(),"request_id":request.request_id,"result":result,"order":order,"inventory":next.stocks,"command":request.command});
        sqlx::query(
            "UPDATE wms_workspaces SET state=$1,version=$2 WHERE tenant=$3 AND warehouse_id=$4",
        )
        .bind(json!(next))
        .bind(next.version)
        .bind(&identity.tenant)
        .bind(warehouse)
        .execute(&mut *tx)
        .await?;
        sqlx::query("INSERT INTO wms_events (id,tenant,warehouse_id,version,payload) VALUES ($1,$2,$3,$4,$5)")
            .bind(&event_id).bind(&identity.tenant).bind(warehouse).bind(next.version).bind(&event).execute(&mut *tx).await?;
        sqlx::query("INSERT INTO wms_commands (tenant,warehouse_id,request_id,actor,digest,result) VALUES ($1,$2,$3,$4,$5,$6)")
            .bind(&identity.tenant).bind(warehouse).bind(&request.request_id).bind(&identity.actor).bind(digest).bind(&result).execute(&mut *tx).await?;
        let destinations = if matches!(request.command, Command::Pack { .. } | Command::Ship { .. })
        {
            vec!["oms", "tms"]
        } else {
            vec!["oms"]
        };
        for destination in destinations {
            sqlx::query(
                "INSERT INTO wms_outbox (id,event_id,tenant,destination) VALUES ($1,$2,$3,$4)",
            )
            .bind(domain::id())
            .bind(&event_id)
            .bind(&identity.tenant)
            .bind(destination)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(json!({"replayed":false,"result":result}))
    }
    pub async fn receipt(
        &self,
        identity: &Identity,
        warehouse: &str,
        request: &str,
    ) -> Result<Value> {
        let row=sqlx::query("SELECT result FROM wms_commands WHERE tenant=$1 AND warehouse_id=$2 AND request_id=$3 AND actor=$4")
            .bind(&identity.tenant).bind(warehouse).bind(request).bind(&identity.actor).fetch_optional(&self.pool).await?;
        match row {
            Some(r) => Ok(json!({"found":true,"result":r.try_get::<Value,_>("result")?})),
            None => Ok(json!({"found":false})),
        }
    }
    pub async fn outbox(&self, tenant: &str, warehouse: &str) -> Result<Vec<Value>> {
        let rows=sqlx::query("SELECT o.id,o.destination,o.status,o.attempts,o.last_error,o.event_id,e.payload->>'type' AS kind,e.payload->>'occurred_at' AS occurred_at FROM wms_outbox o JOIN wms_events e ON e.id=o.event_id WHERE o.tenant=$1 AND e.warehouse_id=$2 ORDER BY e.created_at DESC,o.id LIMIT 200")
            .bind(tenant).bind(warehouse).fetch_all(&self.pool).await?;
        rows.iter().map(|r|Ok(json!({"id":r.try_get::<String,_>("id")?,"destination":r.try_get::<String,_>("destination")?,"status":r.try_get::<String,_>("status")?,"attempts":r.try_get::<i64,_>("attempts")?,"last_error":r.try_get::<String,_>("last_error")?,"event_id":r.try_get::<String,_>("event_id")?,"type":r.try_get::<String,_>("kind")?,"occurred_at":r.try_get::<String,_>("occurred_at")?}))).collect()
    }
    pub async fn retry(&self, identity: &Identity, outbox_id: &str) -> Result<Value> {
        if identity.role != "admin" {
            return Err(Error::new(
                403,
                "Only administrators may retry dead letters",
            ));
        }
        let mut tx = self.pool.begin().await?;
        let n=sqlx::query("UPDATE wms_outbox SET status='pending',due_at=now(),attempts=0,lease_token=NULL,lease_until=NULL WHERE tenant=$1 AND id=$2 AND status='dead_letter'")
            .bind(&identity.tenant).bind(outbox_id).execute(&mut *tx).await?.rows_affected();
        if n != 1 {
            return Err(Error::new(
                409,
                "Only an existing dead letter can be retried",
            ));
        }
        sqlx::query("INSERT INTO wms_admin_audit (id,tenant,actor,action,reference) VALUES ($1,$2,$3,'outbox.retry',$4)").bind(domain::id()).bind(&identity.tenant).bind(&identity.actor).bind(outbox_id).execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(json!({"queued":true}))
    }
}
