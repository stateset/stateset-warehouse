//! Proposed event receiver contract. No native OMS/TMS compatibility is implied.
use crate::store::{Error, Result, Store};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use sqlx::Row;
use std::{collections::BTreeMap, sync::Arc, time::Duration};

const MAX_DELIVERY_ATTEMPTS: i64 = 8;

#[derive(Clone, Default)]
pub struct Connections(pub Arc<BTreeMap<(String, String), (String, String)>>);
impl Connections {
    pub fn status(&self, tenant: &str) -> Value {
        json!([{"id":"oms","name":"StateSet Sync","role":"Order management","configured":self.0.contains_key(&(tenant.into(),"oms".into()))},{"id":"tms","name":"StateSet Freight","role":"Transportation management","configured":self.0.contains_key(&(tenant.into(),"tms".into()))},{"id":"edi","name":"StateSet EDI","role":"Document exchange","configured":false}])
    }
    pub async fn dispatch_one(&self, store: &Store) -> Result<bool> {
        let configured: Vec<_> = self
            .0
            .keys()
            .map(|(tenant, destination)| json!({"tenant":tenant,"destination":destination}))
            .collect();
        if configured.is_empty() {
            return Ok(false);
        }
        let mut tx = store.pool.begin().await?;
        let rows=sqlx::query("SELECT o.id,o.event_id,o.tenant,o.destination,o.attempts,o.recipient_fingerprint,EXISTS(
            SELECT 1
            FROM wms_delivery_attempts a
            WHERE a.outbox_id=o.id) AS attempted,e.payload
            FROM wms_outbox o
            JOIN wms_events e ON e.id=o.event_id
            WHERE EXISTS (
            SELECT 1
            FROM jsonb_to_recordset($1::jsonb) AS c(tenant text,destination text)
            WHERE c.tenant=o.tenant AND c.destination=o.destination) AND ((o.status='pending' AND o.due_at<=now()) OR (o.status='delivering' AND o.lease_until<now()))
            ORDER BY o.due_at,o.id FOR
            UPDATE OF o SKIP LOCKED
            LIMIT 1")
            .bind(json!(configured)).fetch_all(&mut *tx).await?;
        let row = rows.into_iter().find(|r| {
            let tenant = r.get::<String, _>("tenant");
            let dest = r.get::<String, _>("destination");
            self.0.contains_key(&(tenant, dest))
        });
        let Some(row) = row else {
            return Ok(false);
        };
        let oid: String = row.try_get("id")?;
        let event: String = row.try_get("event_id")?;
        let tenant: String = row.try_get("tenant")?;
        let destination: String = row.try_get("destination")?;
        let attempts: i64 = row.try_get("attempts")?;
        let payload: Value = row.try_get("payload")?;
        let (url, token) = self
            .0
            .get(&(tenant, destination))
            .ok_or_else(|| Error::new(503, "Receiver configuration changed"))?;
        let fingerprint = format!("{:x}", Sha256::digest(url.as_bytes()));
        let pinned: Option<String> = row.try_get("recipient_fingerprint")?;
        let attempted: bool = row.try_get("attempted")?;
        sqlx::query("UPDATE wms_delivery_attempts
            SET outcome='uncertain',detail='Delivery lease expired before its outcome was committed',finished_at=now()
            WHERE outbox_id=$1 AND outcome='started'").bind(&oid).execute(&mut *tx).await?;
        let hold = match pinned.as_deref() {
            Some("unverified") => Some("Original receiver is unknown for a historical delivery; independent receiver reconciliation is required before recovery"),
            Some(original) if original != fingerprint => Some("Configured receiver differs from the original delivery; restore the original receiver before an audited retry"),
            None if attempts > 0 || attempted => Some("Original receiver is unknown for a historical delivery; independent receiver reconciliation is required before recovery"),
            _ => None,
        };
        if let Some(reason) = hold {
            sqlx::query("UPDATE wms_outbox SET status='dead_letter',last_error=$1,lease_token=NULL,lease_until=NULL WHERE id=$2")
                .bind(reason).bind(&oid).execute(&mut *tx).await?;
            tx.commit().await?;
            return Ok(true);
        }
        // A sender may disappear after HTTP but before committing its outcome.
        // Count its claimed attempt against the budget before sending again;
        // expiry does not prove the receiver rejected the original event.
        if attempts >= MAX_DELIVERY_ATTEMPTS {
            sqlx::query("UPDATE wms_outbox
                SET status='dead_letter',last_error='Automatic delivery attempt limit reached; receiver acceptance is uncertain; review the original event before an audited retry',lease_token=NULL,lease_until=NULL
                WHERE id=$1")
                .bind(&oid).execute(&mut *tx).await?;
            tx.commit().await?;
            return Ok(true);
        }
        if pinned.is_none() {
            sqlx::query("UPDATE wms_outbox SET recipient_fingerprint=$1 WHERE id=$2")
                .bind(&fingerprint)
                .bind(&oid)
                .execute(&mut *tx)
                .await?;
        }
        sqlx::query("SELECT set_config('wms.delivery_recipient',$1,true)")
            .bind(&fingerprint)
            .execute(&mut *tx)
            .await?;
        let lease = crate::domain::id();
        sqlx::query("INSERT INTO wms_delivery_attempts (id,outbox_id) VALUES ($1,$2)")
            .bind(&lease)
            .bind(&oid)
            .execute(&mut *tx)
            .await?;
        sqlx::query("UPDATE wms_outbox SET status='delivering',attempts=attempts+1,lease_token=$1,lease_until=now()+interval '30 seconds' WHERE id=$2")
            .bind(&lease).bind(&oid).execute(&mut *tx).await?;
        tx.commit().await?;
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|_| Error::new(503, "Failed to configure delivery client"))?;
        let response = client
            .post(url)
            .bearer_auth(token)
            .header("Idempotency-Key", &event)
            .json(&payload)
            .send()
            .await;
        let (delivered, message) = match response {
            Ok(r) if r.status().is_success() => (true, String::new()),
            Ok(r) => (
                false,
                format!("Receiver returned HTTP {}", r.status().as_u16()),
            ),
            Err(_) => (
                false,
                "Delivery timed out or transport failed; receiver acceptance is uncertain".into(),
            ),
        };
        let status = if delivered {
            "delivered"
        } else if attempts + 1 >= MAX_DELIVERY_ATTEMPTS {
            "dead_letter"
        } else {
            "pending"
        };
        let delay = 2_i64.pow((attempts + 1).min(10) as u32);
        let mut tx = store.pool.begin().await?;
        let updated=sqlx::query("UPDATE wms_outbox
            SET status=$1,last_error=$2,due_at=now()+($3::bigint*interval '1 second'),lease_token=NULL,lease_until=NULL,delivered_at=CASE WHEN $4 THEN now() ELSE delivered_at END
            WHERE id=$5 AND lease_token=$6 AND status='delivering'")
            .bind(status).bind(&message).bind(delay).bind(delivered).bind(&oid).bind(&lease).execute(&mut *tx).await?.rows_affected();
        if updated == 1 {
            sqlx::query(
                "UPDATE wms_delivery_attempts
                SET outcome=$1,detail=$2,finished_at=now()
                WHERE id=$3 AND outcome='started'",
            )
            .bind(if delivered { "accepted" } else { "failed" })
            .bind(message)
            .bind(lease)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(true)
    }
}
