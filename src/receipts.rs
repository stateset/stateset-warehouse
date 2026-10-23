//! Closing inbound work preserves received stock and freezes its unreceived remainder.
use crate::domain::{now, Receipt, Result};
use serde::{Deserialize, Serialize};
use sqlx::{PgConnection, Row};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct CancelledLine {
    pub sku: String,
    pub lot: String,
    pub quantity: i64,
    pub received: i64,
    pub cancelled: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Cancellation {
    pub reason: String,
    pub actor: String,
    pub cancelled_at: String,
    pub lines: BTreeMap<String, CancelledLine>,
}

pub fn cancel(receipt: &mut Receipt, reason: &str, actor: &str) -> Result<()> {
    if receipt.cancellation.is_some() || receipt.status == "cancelled" {
        return Err("Receipt remainder is already cancelled".into());
    }
    if !receipt
        .lines
        .iter()
        .any(|line| line.received < line.quantity)
    {
        return Err("A fully received receipt has no remainder to cancel".into());
    }
    let lines = receipt
        .lines
        .iter()
        .map(|line| {
            (
                line.id.clone(),
                CancelledLine {
                    sku: line.sku.clone(),
                    lot: line.lot.clone(),
                    quantity: line.quantity,
                    received: line.received,
                    cancelled: line.quantity - line.received,
                },
            )
        })
        .collect();
    receipt.cancellation = Some(Cancellation {
        reason: reason.into(),
        actor: actor.into(),
        cancelled_at: now(),
        lines,
    });
    receipt.status = "cancelled".into();
    Ok(())
}

pub fn validate(receipt: &Receipt) -> Result<()> {
    let Some(cancellation) = &receipt.cancellation else {
        if receipt.status == "cancelled" {
            return Err("Cancelled receipt requires immutable cancellation evidence".into());
        }
        return Ok(());
    };
    if receipt.status != "cancelled"
        || cancellation.reason.trim().is_empty()
        || cancellation.reason.trim() != cancellation.reason
        || cancellation.reason.len() > 240
        || cancellation.reason.chars().any(char::is_control)
        || cancellation.actor.trim().is_empty()
        || chrono::DateTime::parse_from_rfc3339(&cancellation.cancelled_at).is_err()
        || cancellation.lines.len() != receipt.lines.len()
        || !cancellation.lines.values().any(|line| line.cancelled > 0)
    {
        return Err("Invalid receipt cancellation evidence".into());
    }
    for line in &receipt.lines {
        let frozen = cancellation
            .lines
            .get(&line.id)
            .ok_or("Cancellation must cover every exact receipt line")?;
        if frozen.sku != line.sku
            || frozen.lot != line.lot
            || frozen.quantity != line.quantity
            || frozen.received != line.received
            || frozen.cancelled != line.quantity - line.received
            || frozen.cancelled < 0
        {
            return Err("Cancelled receipt line differs from its frozen remainder".into());
        }
    }
    Ok(())
}

pub fn cancelled(receipt: &Receipt, line_id: &str) -> i64 {
    receipt
        .cancellation
        .as_ref()
        .and_then(|c| c.lines.get(line_id))
        .map_or(0, |line| line.cancelled)
}

pub(crate) async fn load(
    c: &mut PgConnection,
    tenant: &str,
    warehouse: &str,
    receipts: &mut [Receipt],
) -> crate::store::Result<()> {
    let ids: Vec<_> = receipts.iter().map(|r| r.id.clone()).collect();
    let rows = sqlx::query("SELECT c.receipt_id,c.reason,c.actor,c.cancelled_at,l.line_id,l.sku,l.lot,l.quantity,l.received,l.cancelled
        FROM wms_receipt_cancellations c
        LEFT JOIN wms_receipt_cancelled_lines l ON l.tenant=c.tenant AND l.warehouse_id=c.warehouse_id AND l.receipt_id=c.receipt_id
        WHERE c.tenant=$1 AND c.warehouse_id=$2 AND c.receipt_id=ANY($3)
        ORDER BY c.receipt_id,l.line_id")
        .bind(tenant).bind(warehouse).bind(ids).fetch_all(&mut *c).await?;
    let mut cancellations = BTreeMap::<String, Cancellation>::new();
    for row in rows {
        let receipt_id: String = row.try_get("receipt_id")?;
        let cancellation = cancellations.entry(receipt_id).or_insert(Cancellation {
            reason: row.try_get("reason")?,
            actor: row.try_get("actor")?,
            cancelled_at: row.try_get("cancelled_at")?,
            lines: BTreeMap::new(),
        });
        if let Some(line_id) = row.try_get::<Option<String>, _>("line_id")? {
            cancellation.lines.insert(
                line_id,
                CancelledLine {
                    sku: row.try_get("sku")?,
                    lot: row.try_get("lot")?,
                    quantity: row.try_get("quantity")?,
                    received: row.try_get("received")?,
                    cancelled: row.try_get("cancelled")?,
                },
            );
        }
    }
    for receipt in receipts {
        receipt.cancellation = cancellations.remove(&receipt.id);
    }
    Ok(())
}

pub(crate) async fn save(
    c: &mut PgConnection,
    tenant: &str,
    warehouse: &str,
    receipt: &Receipt,
) -> crate::store::Result<()> {
    let Some(cancellation) = &receipt.cancellation else {
        return Ok(());
    };
    sqlx::query("INSERT INTO wms_receipt_cancellations(tenant,warehouse_id,receipt_id,reason,actor,cancelled_at) VALUES($1,$2,$3,$4,$5,$6)")
        .bind(tenant).bind(warehouse).bind(&receipt.id).bind(&cancellation.reason)
        .bind(&cancellation.actor).bind(&cancellation.cancelled_at).execute(&mut *c).await?;
    for (line_id, line) in &cancellation.lines {
        sqlx::query("INSERT INTO wms_receipt_cancelled_lines(tenant,warehouse_id,receipt_id,line_id,sku,lot,quantity,received,cancelled) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9)")
            .bind(tenant).bind(warehouse).bind(&receipt.id).bind(line_id).bind(&line.sku).bind(&line.lot)
            .bind(line.quantity).bind(line.received).bind(line.cancelled).execute(&mut *c).await?;
    }
    Ok(())
}

pub(crate) async fn proof(
    c: &mut PgConnection,
    tenant: &str,
    warehouse: &str,
) -> crate::store::Result<Vec<serde_json::Value>> {
    let rows = sqlx::query(include_str!("receipt_cancellation_proof.sql"))
        .bind(tenant)
        .bind(warehouse)
        .fetch_all(&mut *c)
        .await?;
    rows.into_iter().map(|r| Ok(r.try_get("proof")?)).collect()
}
