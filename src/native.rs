//! Native adapters grounded in the checked-in OMS and TMS HTTP implementations.
//!
//! The OMS fulfillment endpoint has no conditional update. It is intentionally
//! exported as a held proposal rather than allowed to overwrite a concurrent
//! cancellation or delivery. Freight's source-verified import creates a planned
//! load only; warehouse readiness never becomes a transportation observation.
use crate::domain::{Command, LineInput, Order, State};
use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Number, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    time::Duration,
};
use uuid::Uuid;

pub type Result<T> = std::result::Result<T, String>;
pub const MAX_NATIVE_RESPONSE_BYTES: usize = 2 * 1024 * 1024;
pub const OMS_WRITEBACK_HOLD: &str = "Native OMS fulfillment writeback held: the current OMS endpoint unconditionally overwrites order status and cannot enforce a source revision; handoff evidence is retained for reconciliation";

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OmsConnection {
    pub base_url: String,
    pub tenant_id: String,
    pub api_key: String,
    #[serde(default)]
    pub shipment_writeback: ShipmentWriteback,
}
#[derive(Clone, Copy, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ShipmentWriteback {
    #[default]
    Held,
    ConditionalV1,
}
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TmsConnection {
    pub base_url: String,
    pub tenant_id: String,
    pub bearer_token: String,
}
#[derive(Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeConnection {
    pub oms: Option<OmsConnection>,
    pub tms: Option<TmsConnection>,
}
#[derive(Clone, Default)]
pub struct NativeIntegrations {
    pub tenants: BTreeMap<String, NativeConnection>,
}

fn identifier(value: &str, label: &str) -> Result<()> {
    if value.is_empty()
        || value.len() > 100
        || !value
            .bytes()
            .all(|v| v.is_ascii_alphanumeric() || b"-_".contains(&v))
    {
        return Err(format!(
            "{label} must contain 1–100 letters, numbers, hyphens, or underscores"
        ));
    }
    Ok(())
}
fn text(value: &str, label: &str, max: usize) -> Result<()> {
    if value.is_empty()
        || value.len() > max
        || value.trim() != value
        || value.chars().any(char::is_control)
    {
        return Err(format!(
            "{label} must contain 1–{max} characters without surrounding whitespace or controls"
        ));
    }
    Ok(())
}
fn validate_base(value: &str) -> Result<()> {
    let url = reqwest::Url::parse(value).map_err(|_| "Invalid native integration URL")?;
    if !["http", "https"].contains(&url.scheme())
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err("Native URL must use HTTP(S) without credentials, query, or fragment".into());
    }
    if url.scheme() == "http"
        && !matches!(
            url.host_str(),
            Some(
                "localhost"
                    | "127.0.0.1"
                    | "[::1]"
                    | "sync-server"
                    | "freight"
                    | "stateset-sync-server"
                    | "stateset-freight"
            )
        )
    {
        return Err("Remote native integrations require HTTPS".into());
    }
    Ok(())
}
pub(crate) fn client() -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(8))
        .connect_timeout(Duration::from_secs(3))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|_| "Native HTTP client unavailable".into())
}
fn normalized_base(value: &str) -> Result<String> {
    validate_base(value)?;
    Ok(reqwest::Url::parse(value)
        .map_err(|_| "Invalid native recipient URL")?
        .to_string()
        .trim_end_matches('/')
        .to_string())
}
pub(crate) async fn response_json(mut response: reqwest::Response, service: &str) -> Result<Value> {
    if !response.status().is_success() {
        return Err(format!(
            "{service} returned HTTP {}",
            response.status().as_u16()
        ));
    }
    if response
        .content_length()
        .is_some_and(|n| n > MAX_NATIVE_RESPONSE_BYTES as u64)
    {
        return Err(format!("{service} response exceeds 2 MiB limit"));
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|_| {
        format!("{service} response interrupted; remote acceptance may be uncertain")
    })? {
        if chunk.len() > MAX_NATIVE_RESPONSE_BYTES.saturating_sub(bytes.len()) {
            return Err(format!("{service} response exceeds 2 MiB limit"));
        }
        bytes.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&bytes).map_err(|_| format!("{service} returned invalid JSON"))
}

/// Decimal parsing is exact; serde_json's arbitrary_precision feature preserves
/// fractions that an f64 would round to an integer before validation.
pub fn whole_each(number: &Number) -> Result<i64> {
    let raw = number.to_string();
    if raw.len() > 4096 {
        return Err("Quantity decimal is too long".into());
    }
    let (mantissa, exponent) = match raw.split_once(['e', 'E']) {
        Some((m, e)) => (
            m,
            e.parse::<i32>()
                .map_err(|_| "Quantity exponent is unsupported")?,
        ),
        None => (raw.as_str(), 0),
    };
    if mantissa.starts_with('-') || !(-1000..=1000).contains(&exponent) {
        return Err("Quantity must be an exact whole each between 1 and 1,000,000".into());
    }
    let (integer, fraction) = mantissa.split_once('.').unwrap_or((mantissa, ""));
    let mut digits = format!("{integer}{fraction}");
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return Err("Invalid quantity decimal".into());
    }
    let shift = exponent - i32::try_from(fraction.len()).map_err(|_| "Quantity is too long")?;
    if shift < 0 {
        let remove = (-shift) as usize;
        if remove >= digits.len() || !digits[digits.len() - remove..].bytes().all(|b| b == b'0') {
            return Err("Quantity must be an exact whole each between 1 and 1,000,000".into());
        }
        digits.truncate(digits.len() - remove);
    } else {
        if shift > 7 {
            return Err("Quantity exceeds whole each range".into());
        }
        digits.extend(std::iter::repeat_n('0', shift as usize));
    }
    let quantity = digits
        .parse::<i64>()
        .map_err(|_| "Quantity exceeds whole each range")?;
    if !(1..=1_000_000).contains(&quantity) {
        return Err("Quantity must be an exact whole each between 1 and 1,000,000".into());
    }
    Ok(quantity)
}

/// Raw field names and types follow sync-server/src/domain/mod.rs::Order.
/// Unknown upstream fields are retained by the journal's original response;
/// only fulfillment-relevant fields are translated here.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OmsOrder {
    pub id: String,
    pub order_number: String,
    pub shopify_order_id: String,
    pub status: String,
    pub line_items: Vec<OmsOrderLine>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fulfillment_status: Option<String>,
    #[serde(
        default,
        alias = "source_order_payload",
        skip_serializing_if = "Option::is_none"
    )]
    pub source_order_payload: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tenant_id: Option<String>,
}
fn require_unfulfilled(order: &OmsOrder) -> Result<()> {
    let valid = |value: Option<&str>| {
        value.is_none_or(|v| {
            matches!(
                v.trim().to_ascii_lowercase().as_str(),
                "" | "unfulfilled" | "pending"
            )
        })
    };
    if !valid(order.fulfillment_status.as_deref()) {
        return Err("Complete warehouse orders require an unfulfilled OMS source; partial or unknown fulfillment requires reconciliation".into());
    }
    if let Some(source) = &order.source_order_payload {
        for key in ["cancelled_at", "cancelledAt"] {
            if let Some(value) = source.get(key).filter(|v| !v.is_null()) {
                if value.as_str() != Some("") {
                    return Err("Upstream source records cancellation; reconcile before starting warehouse or transport work".into());
                }
            }
        }
        for key in ["fulfillment_status", "fulfillmentStatus"] {
            if let Some(value) = source.get(key).filter(|v| !v.is_null()) {
                if !value.is_string() || !valid(value.as_str()) {
                    return Err("Upstream source fulfillment is partial, completed or unknown; reconcile before full-order warehouse work".into());
                }
            }
        }
    }
    Ok(())
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OmsOrderLine {
    pub title: String,
    pub quantity: Number,
    pub price: Number,
    pub sku: Option<String>,
    pub variant_id: Option<String>,
    pub shopify_line_item_id: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OmsPagination {
    pub page: u32,
    pub page_size: u32,
    pub has_next: bool,
    pub has_previous: bool,
    pub total_items: Option<u64>,
    pub total_pages: Option<u32>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OmsOrderPage {
    pub items: Vec<OmsOrder>,
    pub pagination: OmsPagination,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImportDetails {
    pub customer: String,
    pub destination: String,
    pub priority: String,
}

fn source_reference(upstream_tenant: &str, order_id: &str) -> Result<String> {
    identifier(upstream_tenant, "OMS tenant")?;
    let id = Uuid::parse_str(order_id).map_err(|_| "OMS order ID must be a UUID")?;
    Ok(format!("oms:{upstream_tenant}:{id}"))
}
fn source_id(reference: &str, upstream_tenant: &str) -> Result<String> {
    let prefix = format!("oms:{upstream_tenant}:");
    let id = reference
        .strip_prefix(&prefix)
        .ok_or("Order source belongs to a different OMS tenant or is not a native OMS import")?;
    let id = Uuid::parse_str(id).map_err(|_| "Native OMS source ID is invalid")?;
    Ok(id.to_string())
}
fn translated_lines(order: &OmsOrder) -> Result<Vec<LineInput>> {
    if order.line_items.is_empty() || order.line_items.len() > 100 {
        return Err("Native OMS import requires 1–100 lines".into());
    }
    let mut seen = BTreeSet::new();
    order
        .line_items
        .iter()
        .enumerate()
        .map(|(index, line)| {
            let sku = line.sku.as_deref().ok_or("OMS line requires a SKU")?;
            text(sku, "OMS SKU", 100)?;
            let identity = match line.shopify_line_item_id.as_deref() {
                Some(id) => {
                    text(id, "OMS line identity", 255)?;
                    format!("source:{id}")
                }
                None => format!("position:{index}"),
            };
            if !seen.insert(identity.clone()) {
                return Err("OMS line identities must be unique".into());
            }
            let id = format!("oms-line-{:x}", Sha256::digest(identity.as_bytes()));
            Ok(LineInput {
                id,
                sku: sku.into(),
                quantity: whole_each(&line.quantity)?,
            })
        })
        .collect()
}
fn check_oms_identity(
    order: &OmsOrder,
    upstream_tenant: &str,
    requested: Option<&str>,
) -> Result<()> {
    let id = Uuid::parse_str(&order.id).map_err(|_| "OMS returned a non-UUID order identity")?;
    if requested.is_some_and(|v| Uuid::parse_str(v).ok() != Some(id)) {
        return Err("OMS response order identity mismatch".into());
    }
    if order
        .tenant_id
        .as_deref()
        .is_some_and(|v| v != upstream_tenant)
    {
        return Err("OMS response tenant identity mismatch".into());
    }
    text(&order.order_number, "OMS order reference", 240)?;
    if order.updated_at < order.created_at {
        return Err("OMS revision predates creation".into());
    }
    Ok(())
}

impl NativeIntegrations {
    pub fn from_env() -> Result<Self> {
        let raw = std::env::var("WMS_NATIVE_INTEGRATIONS_JSON").unwrap_or_else(|_| "{}".into());
        Self::from_json(if raw.trim().is_empty() { "{}" } else { &raw })
    }
    pub fn from_json(raw: &str) -> Result<Self> {
        let tenants: BTreeMap<String, NativeConnection> = serde_json::from_str(raw)
            .map_err(|_| "Invalid WMS_NATIVE_INTEGRATIONS_JSON configuration")?;
        for (tenant, connection) in &tenants {
            identifier(tenant, "Warehouse tenant")?;
            if let Some(oms) = &connection.oms {
                validate_base(&oms.base_url)?;
                identifier(&oms.tenant_id, "OMS tenant")?;
                text(&oms.api_key, "OMS API key", 4096)?;
            }
            if let Some(tms) = &connection.tms {
                validate_base(&tms.base_url)?;
                identifier(&tms.tenant_id, "TMS tenant")?;
                text(&tms.bearer_token, "TMS bearer token", 4096)?;
                if connection.oms.is_none() {
                    return Err("Native TMS imports require an explicit OMS tenant mapping".into());
                }
            }
        }
        Ok(Self { tenants })
    }
    fn oms(&self, tenant: &str) -> Result<&OmsConnection> {
        self.tenants
            .get(tenant)
            .and_then(|v| v.oms.as_ref())
            .ok_or_else(|| "Native OMS is not configured for this warehouse tenant".into())
    }
    fn tms(&self, tenant: &str) -> Result<&TmsConnection> {
        self.tenants
            .get(tenant)
            .and_then(|v| v.tms.as_ref())
            .ok_or_else(|| "Native TMS is not configured for this warehouse tenant".into())
    }
    pub fn source_reference(&self, tenant: &str, order_id: &str) -> Result<String> {
        source_reference(&self.oms(tenant)?.tenant_id, order_id)
    }
    pub fn summary(&self, tenant: &str) -> Value {
        let connection = self.tenants.get(tenant);
        let conditional = connection
            .and_then(|c| c.oms.as_ref())
            .is_some_and(|c| c.shipment_writeback == ShipmentWriteback::ConditionalV1);
        json!({"oms":{"configured":connection.is_some_and(|c|c.oms.is_some()),"read_contract":"GET /v1/tenants/{tenant}/orders","shipment_writeback":if conditional{"conditional_v1"}else{"held_missing_remote_precondition"}},"tms":{"configured":connection.is_some_and(|c|c.tms.is_some()),"write_contract":"POST /v1/sync/import","behavior":"source_verified_planned_load","requires_operator_transport_plan":true}})
    }
    pub async fn list_orders(&self, tenant: &str, page: u32) -> Result<OmsOrderPage> {
        if page == 0 {
            return Err("OMS page must be at least one".into());
        }
        let c = self.oms(tenant)?;
        let response = client()?
            .get(format!(
                "{}/v1/tenants/{}/orders",
                c.base_url.trim_end_matches('/'),
                c.tenant_id
            ))
            .header("x-stateset-api-key", &c.api_key)
            .query(&[("page", page.to_string()), ("page_size", "100".into())])
            .send()
            .await
            .map_err(|_| "OMS request failed or timed out")?;
        let value = response_json(response, "OMS").await?;
        let result: OmsOrderPage = serde_json::from_value(value["data"].clone())
            .map_err(|_| "OMS page does not match the native data.items contract")?;
        if result.items.len() > 100
            || result.pagination.page != page
            || result.pagination.page_size == 0
            || result.pagination.page_size > 100
        {
            return Err("OMS returned invalid pagination bounds".into());
        }
        for order in &result.items {
            check_oms_identity(order, &c.tenant_id, None)?;
        }
        Ok(result)
    }
    pub async fn fetch_order(&self, tenant: &str, order_id: &str) -> Result<OmsOrder> {
        self.fetch_order_snapshot(tenant, order_id)
            .await
            .map(|(_, order)| order)
    }
    /// Preserve the complete upstream envelope alongside the typed projection.
    /// The operation journal retains fields unknown to this adapter for audits.
    pub async fn fetch_order_snapshot(
        &self,
        tenant: &str,
        order_id: &str,
    ) -> Result<(Value, OmsOrder)> {
        let order_id = Uuid::parse_str(order_id)
            .map_err(|_| "OMS order ID must be a UUID")?
            .to_string();
        let c = self.oms(tenant)?;
        let response = client()?
            .get(format!(
                "{}/v1/tenants/{}/orders/{}",
                c.base_url.trim_end_matches('/'),
                c.tenant_id,
                order_id
            ))
            .header("x-stateset-api-key", &c.api_key)
            .send()
            .await
            .map_err(|_| "OMS request failed or timed out")?;
        let value = response_json(response, "OMS").await?;
        let order: OmsOrder = serde_json::from_value(value["data"].clone())
            .map_err(|_| "OMS response does not match the native data.Order contract")?;
        check_oms_identity(&order, &c.tenant_id, Some(&order_id))?;
        Ok((value, order))
    }
    pub fn import_command(
        &self,
        tenant: &str,
        order: &OmsOrder,
        details: &ImportDetails,
    ) -> Result<Command> {
        let c = self.oms(tenant)?;
        check_oms_identity(order, &c.tenant_id, None)?;
        if !["pending", "processing"].contains(&order.status.as_str()) {
            return Err(
                "Only pending or processing native OMS orders may create forward warehouse work"
                    .into(),
            );
        }
        require_unfulfilled(order)?;
        text(&details.customer, "Customer", 240)?;
        text(&details.destination, "Destination", 240)?;
        if !["normal", "high", "urgent"].contains(&details.priority.as_str()) {
            return Err("Priority must be normal, high, or urgent".into());
        }
        Ok(Command::CreateOrder {
            reference: order.order_number.clone(),
            source_order_id: Some(source_reference(&c.tenant_id, &order.id)?),
            customer: details.customer.clone(),
            destination: details.destination.clone(),
            priority: details.priority.clone(),
            lines: translated_lines(order)?,
        })
    }
    /// Prepare once, journal this exact value before HTTP, and retry it unchanged.
    pub fn prepare_transport(
        &self,
        tenant: &str,
        state: &State,
        order_id: &str,
        plan: &TransportPlan,
    ) -> Result<PreparedTransport> {
        let tms = self.tms(tenant)?;
        let oms = self.oms(tenant)?;
        plan.validate()?;
        let readiness = ready_export(tenant, state, order_id)?;
        let order = state
            .orders
            .iter()
            .find(|o| o.id == order_id)
            .ok_or("Warehouse order not found")?;
        let upstream_id = source_id(
            order
                .source_order_id
                .as_deref()
                .ok_or("Transport import requires a native OMS source")?,
            &oms.tenant_id,
        )?;
        let payload = json!({"order_id":upstream_id,"load":plan});
        let key = transport_key(tenant, &state.warehouse.id, order_id);
        Ok(PreparedTransport {
            tenant: tenant.into(),
            warehouse_id: state.warehouse.id.clone(),
            order_id: order_id.into(),
            idempotency_key: key,
            recipient_base_url: normalized_base(&tms.base_url)?,
            recipient_tenant_id: tms.tenant_id.clone(),
            payload,
            readiness,
        })
    }
    /// Perform this preflight for a new transport intent. Retry an already
    /// journaled uncertain request with execute_transport so Freight can replay
    /// its original receipt even if the source changed after acceptance.
    pub async fn verify_transport_source(
        &self,
        tenant: &str,
        prepared: &PreparedTransport,
    ) -> Result<OmsOrder> {
        self.verify_transport_source_snapshot(tenant, prepared)
            .await
            .map(|(_, order)| order)
    }
    pub async fn verify_transport_source_snapshot(
        &self,
        tenant: &str,
        prepared: &PreparedTransport,
    ) -> Result<(Value, OmsOrder)> {
        let oms = self.oms(tenant)?;
        prepared.validate(tenant, &oms.tenant_id)?;
        self.verify_tms_recipient(tenant, prepared).await?;
        let source = prepared.payload["order_id"]
            .as_str()
            .ok_or("Prepared OMS source required")?;
        let (snapshot, order) = self.fetch_order_snapshot(tenant, source).await?;
        if !["pending", "processing"].contains(&order.status.as_str()) {
            return Err(
                "Transport preparation requires a pending or processing canonical OMS order".into(),
            );
        }
        check_readiness_source(&prepared.readiness, &order)?;
        require_unfulfilled(&order)?;
        Ok((snapshot, order))
    }
    async fn verify_tms_recipient(&self, tenant: &str, prepared: &PreparedTransport) -> Result<()> {
        let tms = self.tms(tenant)?;
        if normalized_base(&tms.base_url)? != prepared.recipient_base_url
            || tms.tenant_id != prepared.recipient_tenant_id
        {
            return Err("TMS recipient authority changed; restore the retained endpoint and tenant before retrying the original intent".into());
        }
        let response = client()?
            .get(format!("{}/v1/access", prepared.recipient_base_url))
            .bearer_auth(&tms.bearer_token)
            .send()
            .await
            .map_err(|_| {
                "TMS access verification failed or timed out; no native import was sent"
            })?;
        let access = response_json(response, "TMS access").await?;
        if access["tenant_id"].as_str() != Some(prepared.recipient_tenant_id.as_str()) {
            return Err("TMS credential resolved to another tenant; original transport intent retained without sending a native import".into());
        }
        if !access["access"]["capabilities"]
            .as_array()
            .is_some_and(|values| values.iter().any(|v| v == "operations.write"))
        {
            return Err(
                "TMS credential requires operations.write to import a native planned load".into(),
            );
        }
        Ok(())
    }
    /// Freight re-fetches the canonical OMS order and creates a planned load.
    /// The upstream operation requires OperationsWrite, not IntegrationReceive.
    pub async fn execute_transport(
        &self,
        tenant: &str,
        prepared: &PreparedTransport,
    ) -> Result<Value> {
        let c = self.tms(tenant)?;
        let oms = self.oms(tenant)?;
        prepared.validate(tenant, &oms.tenant_id)?;
        self.verify_tms_recipient(tenant, prepared).await?;
        let response = client()?.post(format!("{}/v1/sync/import",prepared.recipient_base_url)).bearer_auth(&c.bearer_token).header("Idempotency-Key", &prepared.idempotency_key).json(&prepared.payload).send().await.map_err(|_| "TMS import timed out or transport failed; acceptance is uncertain; retry the same prepared request")?;
        let result = response_json(response, "TMS").await?;
        let load = &result["data"];
        if !result["replayed"].is_boolean()
            || load["id"]
                .as_str()
                .is_none_or(|v| v.is_empty() || v.len() > 240)
            || load["sync_order_id"] != prepared.payload["order_id"]
            || load["reference"] != prepared.payload["load"]["reference"]
            || load["status"] != "planned"
        {
            return Err("TMS response did not confirm the exact planned OMS-linked load; acceptance is uncertain".into());
        }
        if load["source_order"]["tenantId"]
            .as_str()
            .is_some_and(|v| v != oms.tenant_id)
        {
            return Err("TMS source tenant identity mismatch; acceptance is uncertain".into());
        }
        let source: OmsOrder = serde_json::from_value(load["source_order"].clone())
            .map_err(|_| "TMS did not retain the canonical OMS source; acceptance is uncertain")?;
        check_oms_identity(
            &source,
            &oms.tenant_id,
            prepared.payload["order_id"].as_str(),
        )?;
        check_readiness_source(&prepared.readiness, &source)
            .map_err(|_| "TMS retained a different source order revision or line quantities; acceptance is uncertain and the planned load requires review")?;
        // Freight fetches OMS again after our preflight. Check the snapshot
        // retained with its possible load, including on exact receipt replay;
        // reading today's OMS state would reinterpret an earlier acceptance.
        if !["pending", "processing"].contains(&source.status.as_str())
            || require_unfulfilled(&source).is_err()
        {
            return Err("TMS retained an ineligible OMS source; acceptance is uncertain and the planned load requires review".into());
        }
        Ok(result)
    }
    pub async fn create_transport(
        &self,
        tenant: &str,
        state: &State,
        order_id: &str,
        plan: &TransportPlan,
    ) -> Result<Value> {
        let prepared = self.prepare_transport(tenant, state, order_id, plan)?;
        self.execute_transport(tenant, &prepared).await
    }
    /// Returns a native fulfillment proposal for the journal/export. Calling the
    /// existing remote mutation would lose its source guard between GET and POST.
    pub async fn prepare_oms_fulfillment(
        &self,
        tenant: &str,
        warehouse: &str,
        event_id: &str,
        order: &Order,
    ) -> Result<Value> {
        let c = self.oms(tenant)?;
        let source = source_id(
            order
                .source_order_id
                .as_deref()
                .ok_or("Native OMS source required")?,
            &c.tenant_id,
        )?;
        if order.status != "shipped" || order.shipped_at.is_none() {
            return Err("OMS fulfillment evidence requires complete physical handoff".into());
        }
        let current = self.fetch_order(tenant, &source).await?;
        let expected = translated_lines(&current)?;
        if expected.len() != order.lines.len()
            || expected.iter().any(|l| {
                !order
                    .lines
                    .iter()
                    .any(|o| o.id == l.id && o.sku == l.sku && o.quantity == l.quantity)
            })
        {
            return Err(
                "OMS order lines changed; retained warehouse handoff requires reconciliation"
                    .into(),
            );
        }
        if !["pending", "processing", "fulfilled"].contains(&current.status.as_str()) {
            return Err(
                "OMS source status cannot be overwritten by warehouse fulfillment evidence".into(),
            );
        }
        text(&order.carrier, "Handoff carrier", 240)?;
        text(&order.handoff_reference, "Physical handoff reference", 240)?;
        Ok(
            json!({"state":"held","reason":OMS_WRITEBACK_HOLD,"source_revision":current.updated_at,"path":format!("/v1/tenants/{}/orders/{source}/fulfillment",c.tenant_id),"idempotency_key":format!("wms-fulfillment-{:x}",Sha256::digest(format!("{tenant}\0{warehouse}\0{event_id}").as_bytes())),"payload":{"status":"FULFILLED","netsuiteCashSaleId":null,"fulfillmentData":{"source":"stateset-warehouse","tenant":tenant,"warehouse_id":warehouse,"warehouse_order_id":order.id,"event_id":event_id,"carrier":order.carrier,"handoff_reference":order.handoff_reference,"handed_off_at":order.shipped_at,"lines":order.lines}}}),
        )
    }
    /// There is deliberately no HTTP mutation in this method.
    pub async fn dispatch_oms_shipment(
        &self,
        tenant: &str,
        warehouse: &str,
        event_id: &str,
        order_snapshot: &Value,
    ) -> Result<()> {
        let order: Order = serde_json::from_value(order_snapshot.clone())
            .map_err(|_| "Invalid warehouse shipment snapshot")?;
        self.prepare_oms_fulfillment(tenant, warehouse, event_id, &order)
            .await?;
        Err(OMS_WRITEBACK_HOLD.into())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TransportStop {
    pub facility: String,
    pub city: String,
    pub state: String,
    pub date: NaiveDate,
    pub address_lines: Vec<String>,
    pub postal_code: String,
    pub country: String,
    pub contact: String,
    pub phone: String,
    pub instructions: String,
}
impl TransportStop {
    fn validate(&self) -> Result<()> {
        for (value, label, max) in [
            (&self.facility, "Facility", 60),
            (&self.city, "City", 30),
            (&self.postal_code, "Postal code", 15),
        ] {
            wire_text(value, label, max)?;
        }
        for (value, label) in [(&self.state, "State"), (&self.country, "Country")] {
            if value.len() != 2 || !value.bytes().all(|b| b.is_ascii_uppercase()) {
                return Err(format!("{label} requires a two-letter uppercase code"));
            }
        }
        if self.address_lines.is_empty() || self.address_lines.len() > 2 {
            return Err("Transport stop requires one or two street address lines".into());
        }
        for line in &self.address_lines {
            wire_text(line, "Street address", 55)?;
        }
        for (value, label, max) in [
            (&self.contact, "Stop contact", 200),
            (&self.phone, "Stop phone", 100),
            (&self.instructions, "Stop instructions", 1000),
        ] {
            if value.len() > max || value.chars().any(|c| c.is_control() && c != '\n') {
                return Err(format!(
                    "{label} contains controls or exceeds {max} characters"
                ));
            }
        }
        Ok(())
    }
}
fn wire_text(value: &str, label: &str, max: usize) -> Result<()> {
    if value.is_empty()
        || value.len() > max
        || value.trim() != value
        || !value.is_ascii()
        || value
            .bytes()
            .any(|b| b.is_ascii_control() || b"*~:^>".contains(&b))
    {
        return Err(format!(
            "{label} requires 1–{max} ASCII characters without X12 delimiters"
        ));
    }
    Ok(())
}
/// Exact direct-route subset of Freight NewLoad. Prices, weight, mileage,
/// facilities and dates must come from an operator; the WMS derives none.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TransportPlan {
    pub reference: String,
    pub customer_id: String,
    pub mode: String,
    pub equipment: String,
    pub commodity: String,
    pub weight_lbs: u32,
    pub miles: u32,
    pub origin: TransportStop,
    pub destination: TransportStop,
    pub buy_cents: i64,
    pub sell_cents: i64,
}
impl TransportPlan {
    pub fn validate(&self) -> Result<()> {
        text(&self.reference, "Transport reference", 200)?;
        if !self
            .reference
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_".contains(&b))
        {
            return Err(
                "Transport reference requires letters, numbers, hyphens, or underscores".into(),
            );
        }
        text(&self.customer_id, "TMS customer identity", 200)?;
        text(&self.commodity, "Commodity", 200)?;
        if !["FTL", "LTL"].contains(&self.mode.as_str())
            || !["Dry van", "Reefer", "Flatbed"].contains(&self.equipment.as_str())
        {
            return Err("Unsupported TMS mode or equipment".into());
        }
        if !(1..=80_000).contains(&self.weight_lbs) || !(1..=10_000).contains(&self.miles) {
            return Err("Transport weight or mileage is outside TMS supported bounds".into());
        }
        if !(0..=100_000_000).contains(&self.buy_cents)
            || !(0..=100_000_000).contains(&self.sell_cents)
        {
            return Err("Transport prices require USD cents within TMS supported bounds".into());
        }
        self.origin.validate()?;
        self.destination.validate()?;
        if self.destination.date < self.origin.date {
            return Err("Planned delivery cannot precede planned pickup".into());
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReadinessLine {
    pub line_id: String,
    pub sku: String,
    pub quantity_each: i64,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WarehouseReadiness {
    pub schema_version: u32,
    pub kind: String,
    pub tenant: String,
    pub warehouse_id: String,
    pub warehouse_version: i64,
    pub order_id: String,
    pub order_reference: String,
    pub source_order_ref: Option<String>,
    pub warehouse_status: String,
    pub lines: Vec<ReadinessLine>,
}
pub fn ready_export(tenant: &str, state: &State, order_id: &str) -> Result<WarehouseReadiness> {
    identifier(tenant, "Warehouse tenant")?;
    state
        .validate()
        .map_err(|_| "Warehouse reconciliation failed; transport preparation is blocked")?;
    let order = state
        .orders
        .iter()
        .find(|o| o.id == order_id)
        .ok_or("Warehouse order not found")?;
    if order.status != "packed" {
        return Err(
            "Transport readiness requires a fully packed order awaiting physical handoff".into(),
        );
    }
    Ok(WarehouseReadiness {
        schema_version: 1,
        kind: "warehouse.order_ready".into(),
        tenant: tenant.into(),
        warehouse_id: state.warehouse.id.clone(),
        warehouse_version: state.version,
        order_id: order.id.clone(),
        order_reference: order.reference.clone(),
        source_order_ref: order.source_order_id.clone(),
        warehouse_status: order.status.clone(),
        lines: order
            .lines
            .iter()
            .map(|l| ReadinessLine {
                line_id: l.id.clone(),
                sku: l.sku.clone(),
                quantity_each: l.quantity,
            })
            .collect(),
    })
}
fn transport_key(tenant: &str, warehouse: &str, order: &str) -> String {
    format!(
        "wms-transport-{:x}",
        Sha256::digest(format!("{tenant}\0{warehouse}\0{order}").as_bytes())
    )
}
fn check_readiness_source(readiness: &WarehouseReadiness, source: &OmsOrder) -> Result<()> {
    let lines = translated_lines(source)?;
    if lines.len() != readiness.lines.len()
        || lines.iter().any(|line| {
            !readiness.lines.iter().any(|ready| {
                ready.line_id == line.id
                    && ready.sku == line.sku
                    && ready.quantity_each == line.quantity
            })
        })
    {
        return Err(
            "Canonical OMS line identities or quantities differ from warehouse readiness".into(),
        );
    }
    Ok(())
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PreparedTransport {
    pub tenant: String,
    pub warehouse_id: String,
    pub order_id: String,
    pub idempotency_key: String,
    pub recipient_base_url: String,
    pub recipient_tenant_id: String,
    pub payload: Value,
    pub readiness: WarehouseReadiness,
}
impl PreparedTransport {
    fn validate(&self, tenant: &str, upstream_tenant: &str) -> Result<()> {
        identifier(&self.recipient_tenant_id, "Retained TMS tenant")?;
        if normalized_base(&self.recipient_base_url)? != self.recipient_base_url {
            return Err("Retained TMS recipient URL is not canonical".into());
        }
        if self.tenant != tenant
            || self.readiness.tenant != tenant
            || self.readiness.warehouse_id != self.warehouse_id
            || self.readiness.order_id != self.order_id
            || self.idempotency_key != transport_key(tenant, &self.warehouse_id, &self.order_id)
        {
            return Err("Prepared transport identity or idempotency scope mismatch".into());
        }
        if self.readiness.schema_version != 1
            || self.readiness.kind != "warehouse.order_ready"
            || self.readiness.warehouse_status != "packed"
            || self.readiness.warehouse_version < 0
            || self.readiness.lines.is_empty()
            || self.readiness.lines.len() > 100
        {
            return Err("Prepared transport readiness contract is invalid".into());
        }
        let mut seen = BTreeSet::new();
        for line in &self.readiness.lines {
            text(&line.line_id, "Readiness line identity", 240)?;
            text(&line.sku, "Readiness SKU", 100)?;
            if !(1..=1_000_000).contains(&line.quantity_each) || !seen.insert(&line.line_id) {
                return Err("Prepared readiness quantities or line identities are invalid".into());
            }
        }
        let source = source_id(
            self.readiness
                .source_order_ref
                .as_deref()
                .ok_or("Prepared transport lacks OMS source")?,
            upstream_tenant,
        )?;
        if self.payload["order_id"] != source {
            return Err("Prepared transport source identity mismatch".into());
        }
        let plan: TransportPlan = serde_json::from_value(self.payload["load"].clone())
            .map_err(|_| "Prepared transport payload does not match Freight NewLoad")?;
        plan.validate()?;
        if self.payload.as_object().is_none_or(|m| m.len() != 2) {
            return Err("Unexpected prepared transport fields".into());
        }
        Ok(())
    }
}
