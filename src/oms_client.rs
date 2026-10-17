//! Opt-in conditional OMS handoff protocol. The legacy fulfillment endpoint is
//! never used. Persist PreparedFulfillment before dispatching it.
use crate::{
    domain::{Command, Order},
    native::{self, ImportDetails, NativeIntegrations, OmsConnection, Result, ShipmentWriteback},
};
use chrono::DateTime;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use uuid::Uuid;

pub const CONTRACT: &str = "stateset.wms-fulfillment.v1";
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FulfillmentLine {
    pub line_id: String,
    pub sku: String,
    pub quantity_each: i64,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FulfillmentPayload {
    pub request_id: String,
    pub order_id: String,
    pub event_id: String,
    pub expected_updated_at: String,
    pub warehouse_id: String,
    pub warehouse_order_id: String,
    pub carrier: String,
    pub handoff_reference: String,
    pub handed_off_at: String,
    pub lines: Vec<FulfillmentLine>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PreparedFulfillment {
    pub tenant: String,
    pub warehouse_id: String,
    pub order_id: String,
    pub event_id: String,
    pub recipient_base_url: String,
    pub recipient_tenant_id: String,
    pub idempotency_key: String,
    pub payload: FulfillmentPayload,
    pub digest: String,
}
pub fn payload_digest(payload: &FulfillmentPayload) -> Result<String> {
    // Convert through Value for lexicographically ordered object keys, shared
    // with the receiver. Integer quantities need no floating-point conversion.
    fn canonical(value: Value) -> Value {
        match value {
            Value::Object(fields) => Value::Object(
                fields
                    .into_iter()
                    .map(|(key, value)| (key, canonical(value)))
                    .collect::<std::collections::BTreeMap<_, _>>()
                    .into_iter()
                    .collect(),
            ),
            Value::Array(items) => Value::Array(items.into_iter().map(canonical).collect()),
            value => value,
        }
    }
    let value = canonical(serde_json::to_value(payload).map_err(|_| "Invalid OMS handoff")?);
    let bytes = serde_json::to_vec(&value).map_err(|_| "Invalid OMS handoff")?;
    Ok(format!("{:x}", Sha256::digest(bytes)))
}
fn normalized_base(raw: &str) -> Result<String> {
    let url = reqwest::Url::parse(raw).map_err(|_| "Invalid OMS recipient URL")?;
    Ok(url.as_str().trim_end_matches('/').to_string())
}
fn connection<'a>(native: &'a NativeIntegrations, tenant: &str) -> Result<&'a OmsConnection> {
    let cfg = native
        .tenants
        .get(tenant)
        .and_then(|c| c.oms.as_ref())
        .ok_or("Native OMS is not configured for this warehouse tenant")?;
    if cfg.shipment_writeback != ShipmentWriteback::ConditionalV1 {
        return Err(native::OMS_WRITEBACK_HOLD.into());
    }
    Ok(cfg)
}
impl PreparedFulfillment {
    pub fn validate(&self, tenant: &str) -> Result<()> {
        if self.tenant != tenant
            || self.warehouse_id != self.payload.warehouse_id
            || self.order_id != self.payload.warehouse_order_id
            || self.event_id != self.payload.event_id
            || self.idempotency_key != self.payload.request_id
            || payload_digest(&self.payload)? != self.digest
            || self.recipient_tenant_id.is_empty()
            || normalized_base(&self.recipient_base_url)? != self.recipient_base_url
        {
            return Err("Retained OMS handoff identity, recipient or fingerprint mismatch".into());
        }
        for id in [
            &self.order_id,
            &self.event_id,
            &self.payload.order_id,
            &self.payload.request_id,
        ] {
            if Uuid::parse_str(id)
                .map_err(|_| "OMS handoff identities must be UUIDs")?
                .to_string()
                != *id
            {
                return Err("OMS handoff identities must use canonical UUID spelling".into());
            }
        }
        for id in [&self.warehouse_id, &self.recipient_tenant_id] {
            if id.is_empty()
                || id.len() > 100
                || !id
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || b"-_".contains(&c))
            {
                return Err("OMS warehouse and recipient identities must contain 1–100 letters, numbers, hyphens or underscores".into());
            }
        }
        for text in [&self.payload.carrier, &self.payload.handoff_reference] {
            if text.is_empty()
                || text.len() > 240
                || text.trim() != text
                || text.chars().any(char::is_control)
            {
                return Err("OMS handoff carrier and reference require 1–240 characters without surrounding whitespace or controls".into());
            }
        }
        for time in [
            &self.payload.expected_updated_at,
            &self.payload.handed_off_at,
        ] {
            if time.len() > 64 || time.trim() != time {
                return Err("Invalid OMS handoff timestamp".into());
            }
            DateTime::parse_from_rfc3339(time).map_err(|_| "Invalid OMS handoff timestamp")?;
        }
        if self.payload.lines.is_empty() || self.payload.lines.len() > 100 {
            return Err("OMS handoff requires 1–100 complete order lines".into());
        }
        let mut seen = std::collections::BTreeSet::new();
        for line in &self.payload.lines {
            if !seen.insert(&line.line_id)
                || !(1..=1_000_000).contains(&line.quantity_each)
                || !line.line_id.strip_prefix("oms-line-").is_some_and(|hash| {
                    hash.len() == 64
                        && hash
                            .bytes()
                            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
                })
                || line.sku.is_empty()
                || line.sku.len() > 100
                || line.sku.trim() != line.sku
                || line.sku.chars().any(char::is_control)
            {
                return Err("Invalid or duplicate OMS handoff line".into());
            }
        }
        Ok(())
    }
    fn path(&self) -> String {
        format!(
            "{}/v1/tenants/{}/warehouse-fulfillments",
            self.recipient_base_url, self.recipient_tenant_id
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Acknowledgement {
    Fulfilled,
    Rejected,
}

/// A terminal outcome is evidence only for this exact immutable recipient,
/// request, physical event and payload. Reuse this check at the durable journal
/// boundary as well as at HTTP ingress so another caller cannot release an
/// uncertain shipment with an incomplete or unrelated acknowledgment.
pub(crate) fn validate_acknowledgement(
    tenant: &str,
    prepared: &PreparedFulfillment,
    response: &Value,
) -> Result<Acknowledgement> {
    prepared.validate(tenant)?;
    let data = &response["data"];
    if data["contract"] != CONTRACT
        || data["tenant_id"] != prepared.recipient_tenant_id
        || data["order_id"] != prepared.payload.order_id
        || data["event_id"] != prepared.event_id
        || data["request_id"] != prepared.payload.request_id
        || data["request_digest"] != prepared.digest
        || !data["replayed"].is_boolean()
    {
        return Err("OMS receipt identity or fingerprint differs from the retained handoff; acceptance is uncertain".into());
    }
    let timestamp = |field: &str| {
        data[field]
            .as_str()
            .filter(|value| value.len() <= 64)
            .and_then(|value| DateTime::parse_from_rfc3339(value).ok())
    };
    if data["status"] == "rejected" {
        if !matches!(
            data["reason"].as_str(),
            Some(
                "source_revision_changed"
                    | "source_state_not_eligible"
                    | "source_lines_changed"
                    | "order_already_fulfilled"
            )
        ) || timestamp("observed_updated_at").is_none()
            || !data["applied_updated_at"].is_null()
        {
            return Err(
                "OMS rejection is not a valid durable receipt; retain the original handoff request"
                    .into(),
            );
        }
        return Ok(Acknowledgement::Rejected);
    }
    let expected = DateTime::parse_from_rfc3339(&prepared.payload.expected_updated_at)
        .map_err(|_| "Invalid retained source revision")?;
    if data["status"] != "fulfilled"
        || timestamp("applied_updated_at").is_none_or(|time| time <= expected)
    {
        return Err(
            "OMS did not confirm the exact retained handoff; acceptance is uncertain".into(),
        );
    }
    Ok(Acknowledgement::Fulfilled)
}

impl NativeIntegrations {
    pub async fn prepare_conditional_fulfillment(
        &self,
        tenant: &str,
        warehouse: &str,
        event_id: &str,
        order: &Order,
    ) -> Result<(Value, PreparedFulfillment)> {
        let cfg = connection(self, tenant)?;
        if order.status != "shipped"
            || order.shipped_at.is_none()
            || order.carrier.trim().is_empty()
            || order.handoff_reference.trim().is_empty()
            || order.allocations.is_empty()
            || order.lines.is_empty()
            || order.lines.len() > 100
            || order.allocations.iter().any(|a| {
                a.quantity <= 0
                    || a.quantity != a.picked
                    || a.quantity != a.shipped
                    || a.released != 0
                    || !order
                        .lines
                        .iter()
                        .any(|line| line.id == a.line_id && line.sku == a.sku)
            })
            || order.lines.iter().any(|line| {
                order
                    .allocations
                    .iter()
                    .filter(|a| a.line_id == line.id)
                    .try_fold(0_i64, |sum, a| sum.checked_add(a.shipped))
                    != Some(line.quantity)
            })
        {
            return Err(
                "Conditional OMS handoff requires fully shipped physical warehouse work".into(),
            );
        }
        let prefix = format!("oms:{}:", cfg.tenant_id);
        let source = order
            .source_order_id
            .as_deref()
            .and_then(|s| s.strip_prefix(&prefix))
            .ok_or("Warehouse order does not belong to the configured OMS source tenant")?;
        let (raw, current) = self.fetch_order_snapshot(tenant, source).await?;
        let command = self.import_command(
            tenant,
            &current,
            &ImportDetails {
                customer: order.customer.clone(),
                destination: order.destination.clone(),
                priority: order.priority.clone(),
            },
        )?;
        let Command::CreateOrder { lines, .. } = command else {
            return Err("Invalid OMS source translation".into());
        };
        if lines.len() != order.lines.len()
            || lines.iter().any(|line| {
                !order
                    .lines
                    .iter()
                    .any(|l| l.id == line.id && l.sku == line.sku && l.quantity == line.quantity)
            })
        {
            return Err(
                "OMS source line identities or quantities changed; reconcile the physical handoff"
                    .into(),
            );
        }
        let mut lines: Vec<_> = order
            .lines
            .iter()
            .map(|l| FulfillmentLine {
                line_id: l.id.clone(),
                sku: l.sku.clone(),
                quantity_each: l.quantity,
            })
            .collect();
        lines.sort_by(|a, b| a.line_id.cmp(&b.line_id));
        let payload = FulfillmentPayload {
            request_id: Uuid::new_v4().to_string(),
            order_id: source.into(),
            event_id: event_id.into(),
            expected_updated_at: current.updated_at.to_rfc3339(),
            warehouse_id: warehouse.into(),
            warehouse_order_id: order.id.clone(),
            carrier: order.carrier.clone(),
            handoff_reference: order.handoff_reference.clone(),
            handed_off_at: order
                .shipped_at
                .clone()
                .ok_or("Physical handoff time required")?,
            lines,
        };
        let prepared = PreparedFulfillment {
            tenant: tenant.into(),
            warehouse_id: warehouse.into(),
            order_id: order.id.clone(),
            event_id: event_id.into(),
            recipient_base_url: normalized_base(&cfg.base_url)?,
            recipient_tenant_id: cfg.tenant_id.clone(),
            idempotency_key: payload.request_id.clone(),
            digest: payload_digest(&payload)?,
            payload,
        };
        prepared.validate(tenant)?;
        self.verify_conditional_fulfillment(tenant, &prepared)
            .await?;
        Ok((raw, prepared))
    }
    pub async fn verify_conditional_fulfillment(
        &self,
        tenant: &str,
        prepared: &PreparedFulfillment,
    ) -> Result<()> {
        prepared.validate(tenant)?;
        let cfg = connection(self, tenant)?;
        if normalized_base(&cfg.base_url)? != prepared.recipient_base_url
            || cfg.tenant_id != prepared.recipient_tenant_id
        {
            return Err("OMS recipient authority changed; restore the retained endpoint and tenant before retrying".into());
        }
        let response = native::client()?
            .get(prepared.path())
            .header("x-stateset-api-key", &cfg.api_key)
            .send()
            .await
            .map_err(|_| "OMS conditional contract verification failed; no fulfillment was sent")?;
        let response = native::response_json(response, "OMS conditional contract").await?;
        let data = &response["data"];
        if data["contract"] != CONTRACT
            || data["tenant_id"] != prepared.recipient_tenant_id
            || data["conditional_revision"] != true
            || data["terminal_state_fence"] != true
            || data["durable_idempotency"] != true
            || data["durable_rejections"] != true
        {
            return Err("OMS receiver does not attest the required tenant-bound conditional fulfillment contract".into());
        }
        Ok(())
    }
    pub async fn execute_conditional_fulfillment(
        &self,
        tenant: &str,
        prepared: &PreparedFulfillment,
    ) -> Result<Value> {
        self.verify_conditional_fulfillment(tenant, prepared)
            .await?;
        let cfg = connection(self, tenant)?;
        let response=native::client()?.post(prepared.path()).header("x-stateset-api-key",&cfg.api_key).header("Idempotency-Key",&prepared.idempotency_key).json(&prepared.payload).send().await.map_err(|_|"OMS fulfillment response unavailable; acceptance is uncertain; recover the original request")?;
        let response = native::response_json(response, "OMS conditional fulfillment").await?;
        validate_acknowledgement(tenant, prepared, &response)?;
        Ok(response)
    }
}
