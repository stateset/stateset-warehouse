//! Stateless MCP over the canonical authenticated WMS business routes.
//! The transport never chooses a tenant, actor, reviewed version or retry key.
use crate::{
    api::App,
    domain::Command,
    store::{Identity, Request},
};
use axum::{
    body::{to_bytes, Body},
    extract::DefaultBodyLimit,
    http::{request::Parts, Request as HttpRequest},
    Router,
};
use rmcp::{
    model::{
        CallToolRequestParams, CallToolResponse, CallToolResult, Implementation, ListToolsResult,
        PaginatedRequestParams, ServerCapabilities, ServerConfig, Tool, ToolAnnotations,
    },
    service::RequestContext,
    transport::streamable_http_server::{
        session::local::LocalSessionManager, StreamableHttpServerConfig, StreamableHttpService,
    },
    ErrorData, RoleServer, ServerHandler,
};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{json, Map, Value};
use std::{
    collections::BTreeSet,
    env,
    sync::{Arc, OnceLock},
};
use tower::ServiceExt;
use tracing::Instrument;

const INSTRUCTIONS: &str = "Tools act as the authenticated warehouse principal. Tool results contain http_status and the original REST body. Read canonical work and its reviewed version before writing. Physical commands record actually observed warehouse work, never a plan or an assumption. Preserve warehouse_id and the complete original request, including request_id, expected_version and resource_versions, through uncertain responses or cancellation. The MCP JSON-RPC id is not the warehouse request_id. Use wms_command_receipt for physical commands and wms_native_receipt for native operations. A 503 or missing receipt does not prove rejection. Never generate a new request or refresh an old envelope merely to resolve uncertainty. Exact retries return the original committed result; stale conflict proofs and original actor ownership remain authoritative. Native fulfillment and transport may contact configured external services. Multiple partial handoffs remain held by the current native OMS contract.";

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub allowed_hosts: Vec<String>,
    #[serde(default)]
    pub allowed_origins: Vec<String>,
}
impl Default for Config {
    fn default() -> Self {
        Self {
            allowed_hosts: vec!["localhost".into(), "127.0.0.1".into(), "[::1]".into()],
            allowed_origins: vec![],
        }
    }
}
impl Config {
    pub fn parse(raw: &str) -> Result<Self, String> {
        let value: Self = serde_json::from_str(raw).map_err(|_| "Invalid WMS_MCP_JSON schema")?;
        if value.allowed_hosts.is_empty()
            || value.allowed_hosts.len() > 32
            || value.allowed_origins.len() > 32
        {
            return Err("MCP requires 1–32 explicit allowed hosts and at most 32 origins".into());
        }
        for host in &value.allowed_hosts {
            let authority = host
                .parse::<axum::http::uri::Authority>()
                .map_err(|_| "Invalid MCP host authority")?;
            let u =
                reqwest::Url::parse(&format!("https://{host}/")).map_err(|_| "Invalid MCP host")?;
            let hostname = u.host_str().ok_or("Invalid MCP hostname")?;
            let canonical = match authority.port_u16() {
                Some(port) => format!("{hostname}:{port}"),
                None => hostname.to_owned(),
            };
            if host.is_empty()
                || host.contains('*')
                || !u.username().is_empty()
                || u.password().is_some()
                || u.path() != "/"
                || u.query().is_some()
                || u.fragment().is_some()
                || canonical != *host
            {
                return Err("MCP hosts require canonical host[:port] authorities without credentials, wildcards or paths".into());
            }
        }
        for origin in &value.allowed_origins {
            let u = reqwest::Url::parse(origin).map_err(|_| "Invalid MCP origin")?;
            let local = matches!(u.host_str(), Some("localhost" | "127.0.0.1" | "[::1]"));
            if (u.scheme() != "https" && !(u.scheme() == "http" && local))
                || origin != &u.origin().ascii_serialization()
                || !u.username().is_empty()
                || u.password().is_some()
                || u.query().is_some()
                || u.fragment().is_some()
            {
                return Err(
                    "MCP origins require canonical HTTPS origins (HTTP only for loopback)".into(),
                );
            }
        }
        Ok(value)
    }
    pub fn from_env() -> Result<Self, String> {
        match env::var("WMS_MCP_JSON") {
            Ok(s) => Self::parse(&s),
            Err(env::VarError::NotPresent) => Ok(Self::default()),
            Err(_) => Err("WMS_MCP_JSON must be UTF-8".into()),
        }
    }
}
#[derive(Clone)]
enum Rights {
    Any,
    Worker,
    Admin,
    Import,
    Command(String),
}
impl Rights {
    fn visible(&self, role: &str) -> bool {
        match self {
            Self::Any => true,
            Self::Worker => matches!(role, "admin" | "operator"),
            Self::Admin => role == "admin",
            Self::Import => matches!(role, "admin" | "operator" | "oms"),
            // Discovery hints only; the shared REST handlers enforce authorization.
            Self::Command(name) => {
                role == "admin"
                    || role == "operator"
                        && ![
                            "set_product_expiry_policy",
                            "set_unit_pack",
                            "correct_lot_expiry",
                            "release_lot_hold",
                            "record_transfer_loss",
                            "reverse_transfer_loss",
                            "create_warehouse",
                            "update_warehouse",
                            "create_product",
                            "create_location",
                            "count_stock",
                        ]
                        .contains(&name.as_str())
                    || role == "oms"
                        && ["create_order", "cancel_order", "cancel_remainder"]
                            .contains(&name.as_str())
            }
        }
    }
}
#[derive(Clone)]
struct Definition {
    tool: Tool,
    method: &'static str,
    path: &'static str,
    rights: Rights,
    command: Option<String>,
}
fn object(value: Value) -> Map<String, Value> {
    value.as_object().expect("static schema object").clone()
}
fn schema<T: JsonSchema>() -> Value {
    serde_json::to_value(schemars::schema_for!(T)).expect("typed JSON schema")
}
fn string() -> Value {
    json!({"type":"string","minLength":1,"maxLength":240})
}
fn integer(min: i64, max: i64) -> Value {
    json!({"type":"integer","minimum":min,"maximum":max})
}
fn schema_references(value: &Value, pending: &mut Vec<String>) {
    match value {
        Value::Object(fields) => {
            if let Some(reference) = fields
                .get("$ref")
                .and_then(Value::as_str)
                .and_then(|s| s.strip_prefix("#/$defs/"))
            {
                let name = reference.split('/').next().expect("definition reference");
                pending.push(name.replace("~1", "/").replace("~0", "~"));
            }
            for field in fields.values() {
                schema_references(field, pending);
            }
        }
        Value::Array(items) => {
            for item in items {
                schema_references(item, pending);
            }
        }
        _ => (),
    }
}
fn reachable_definitions(body: &Value, definitions: Value) -> Option<Value> {
    let Value::Object(mut definitions) = definitions else {
        panic!("typed schema definitions must be objects")
    };
    let mut pending = Vec::new();
    schema_references(body, &mut pending);
    let mut reached = BTreeSet::new();
    while let Some(name) = pending.pop() {
        if reached.insert(name.clone()) {
            if let Some(definition) = definitions.get(&name) {
                schema_references(definition, &mut pending);
            }
        }
    }
    definitions.retain(|name, _| reached.contains(name));
    (!definitions.is_empty()).then_some(Value::Object(definitions))
}
// A catalog row declares both its HTTP target and its client-visible semantics.
#[allow(clippy::too_many_arguments)]
fn definition(
    name: &str,
    description: &str,
    method: &'static str,
    path: &'static str,
    rights: Rights,
    query: &[(&str, Value)],
    body: Option<Value>,
    read_only: bool,
    open_world: bool,
    idempotent: bool,
) -> Definition {
    let mut properties = Map::new();
    let mut required = Vec::new();
    for fragment in path.split('{').skip(1) {
        let key = fragment.split('}').next().expect("path placeholder");
        properties.insert(key.into(), string());
        required.push(Value::String(key.into()));
    }
    for (name, schema) in query {
        properties.insert((*name).into(), schema.clone());
    }
    let mut defs = None;
    if let Some(mut body) = body {
        // $refs in a subschema still resolve against the whole tool input.
        // Hoist Rust's definitions before nesting the canonical request.
        defs = body
            .as_object_mut()
            .and_then(|o| o.remove("$defs"))
            .and_then(|defs| reachable_definitions(&body, defs));
        properties.insert("request".into(), body);
        required.push(json!("request"));
    }
    let mut input = object(
        json!({"type":"object","properties":properties,"required":required,"additionalProperties":false}),
    );
    if let Some(defs) = defs {
        input.insert("$defs".into(), defs);
    }
    let annotations = ToolAnnotations::new()
        .read_only(read_only)
        .destructive(!read_only)
        .idempotent(idempotent)
        .open_world(open_world);
    Definition {
        tool: Tool::new(name.to_owned(), description.to_owned(), input)
            .with_annotations(annotations),
        method,
        path,
        rights,
        command: None,
    }
}
fn catalog() -> &'static Vec<Definition> {
    static CATALOG: OnceLock<Vec<Definition>> = OnceLock::new();
    CATALOG.get_or_init(build_catalog)
}
fn build_catalog() -> Vec<Definition> {
    let mut tools = vec![
        definition(
            "wms_icommerce_preview",
            "Validate an actual iCommerce MoveInventory or CreateInventoryItem against this facility without saving. Explicit mappings retain original labels. Whole each quantities only. Preserve a complete canonical WMS request before execution; never retranslate uncertain work.",
            "POST", "/v1/warehouses/{warehouse_id}/icommerce/preview", Rights::Worker,
            &[], Some(crate::icommerce::preview_schema()), true, false, true,
        ),
        definition(
            "wms_icommerce_inventory",
            "Read actual iCommerce StockLevel projections using explicit numeric location mappings. Original lot positions and controls remain included. Physical minus reserved does not establish expiry/quarantine eligibility. This tool never writes inventory.",
            "POST", "/v1/warehouses/{warehouse_id}/icommerce/inventory", Rights::Any,
            &[], Some(crate::icommerce::mappings_schema()), true, false, true,
        ),
        definition(
            "wms_session",
            "Read your authenticated tenant, actor, role and default warehouse. Identity cannot be chosen in tool arguments.",
            "GET",
            "/v1/session",
            Rights::Any,
            &[],
            None,
            true,
            false,
            true,
        ),
        definition(
            "wms_warehouses",
            "Page the authenticated tenant's facility directory.",
            "GET",
            "/v1/warehouses",
            Rights::Any,
            &[("after",string()),("limit",integer(1,100))],
            None,
            true,
            false,
            true,
        ),
        definition(
            "wms_snapshot",
            "Read a coherent warehouse snapshot, reviewed warehouse/resource versions and operational balances. Do not reuse it after affected work changes.",
            "GET",
            "/v1/warehouses/{warehouse_id}",
            Rights::Any,
            &[],
            None,
            true,
            false,
            true,
        ),
        definition(
            "wms_order",
            "Read the canonical order by its original ID, including archived work. Use shipment review for a version-bound fulfillment review.",
            "GET",
            "/v1/warehouses/{warehouse_id}/orders/{order_id}",
            Rights::Any,
            &[],
            None,
            true,
            false,
            true,
        ),
        definition(
            "wms_receipt",
            "Read a canonical receiving or return receipt by ID, including archived work.",
            "GET",
            "/v1/warehouses/{warehouse_id}/receipts/{receipt_id}",
            Rights::Any,
            &[],
            None,
            true,
            false,
            true,
        ),
        definition(
            "wms_shipments",
            "Review original order lines, packages, closures, backorders and the captured warehouse version in one coherent read.",
            "GET",
            "/v1/warehouses/{warehouse_id}/orders/{order_id}/shipments",
            Rights::Any,
            &[],
            None,
            true,
            false,
            true,
        ),
        definition(
            "wms_return_eligibility",
            "Review remaining return allowance from actual original shipped line/lot facts and its captured version. Unshipped units are excluded.",
            "GET",
            "/v1/warehouses/{warehouse_id}/orders/{order_id}/return-eligibility",
            Rights::Any,
            &[],
            None,
            true,
            false,
            true,
        ),
        definition(
            "wms_history",
            "Page retained orders, receipts, movements, activity or exceptions. before is an exclusive descending sequence cursor.",
            "GET",
            "/v1/warehouses/{warehouse_id}/history/{kind}",
            Rights::Any,
            &[("before",integer(0,i64::MAX)),("limit",integer(1,100))],
            None,
            true,
            false,
            true,
        ),
        definition(
            "wms_transfers",
            "Page immutable inter-facility dispatch and arrival ownership; unreceived units remain in transit.",
            "GET",
            "/v1/warehouses/{warehouse_id}/transfers",
            Rights::Any,
            &[("after",integer(0,i64::MAX)),("limit",integer(1,100)),("open_only",json!({"type":"boolean"}))],
            None,
            true,
            false,
            true,
        ),
        definition(
            "wms_lot_trace",
            "Review an exact SKU/lot across the authenticated tenant's facilities, retained allocations/receipts, packages, original shipment baselines, transfers, movements and lot policy/decisions. Preserve the original review and section cursor across pages; a 409 requires a new complete review. This read does not quarantine, infer batch genealogy or establish a physical recall.",
            "GET",
            "/v1/lot-trace",
            Rights::Any,
            &[("sku",string()),("lot",string()),("section",schema::<crate::lot_trace::Section>()),
              ("review",json!({"type":"string","minLength":64,"maxLength":64})),
              ("after",json!({"type":"string","minLength":1,"maxLength":4096})),("limit",integer(1,100))],
            None,
            true,
            false,
            true,
        ),
        definition(
            "wms_transfer_losses",
            "Page original destination loss settlements and capped reversals for a transfer. Preserve review and next_cursor. Losses/reversals move no stock; recovered goods still require an actual receiving action.",
            "GET",
            "/v1/warehouses/{warehouse_id}/transfers/{transfer_id}/losses",
            Rights::Any,
            &[("after",integer(0,i64::MAX)),("limit",integer(1,100)),("review",integer(0,i64::MAX))],
            None,true,false,true,
        ),
        definition(
            "wms_unit_packs",
            "Review original SKU pack definitions, including disabled codes or immutable history. Retain review and next_cursor across pages; a 409 requires a fresh review. Stock remains exact each units.",
            "GET", "/v1/warehouses/{warehouse_id}/unit-packs", Rights::Any,
            &[("sku",string()),("code",string()),("history",json!({"type":"boolean"})),("after",json!({"type":"string","minLength":1,"maxLength":2048})),("limit",integer(1,100)),("review",integer(0,i64::MAX))],
            None,true,false,true,
        ),
        definition(
            "wms_serials",
            "Review bounded current literal serial custody for an exact SKU across the tenant. Preserve the global SKU review and original cursor; destination activity invalidates source pages. Optional warehouse_id includes physical ownership and both original transfer facilities. Source flags validate selected records, not a physical count or full reconciliation.",
            "GET", "/v1/serials", Rights::Any,
            &[("sku",string()),("serial",string()),("warehouse_id",string()),("status",json!({
                "type":"string",
                "enum":["on_hand","staged","shipped","in_transit","lost","missing"]
            })),("after",json!({"type":"string","minLength":1,"maxLength":4096})),("limit",integer(1,100)),("review",integer(0,i64::MAX))],
            None,true,false,true,
        ),
        definition(
            "wms_serial_history",
            "Page original per-unit serial steps for an exact SKU and literal serial, including original source/destination actions. Keep the global SKU review and original cursor. History and current-source flags hold corrupted evidence; this read creates no physical work.",
            "GET", "/v1/serial-history", Rights::Any,
            &[("sku",string()),("serial",string()),("warehouse_id",string()),("after",json!({"type":"string","minLength":1,"maxLength":4096})),("limit",integer(1,100)),("review",integer(0,i64::MAX))],
            None,true,false,true,
        ),
        definition(
            "wms_serial_observations",
            "Page original serial observations as individual literal-label occurrences or explicit empty groups. Unchanged and zero counts remain visible without unbounded UID arrays. Preserve global review/cursor and original event/group/member references; warehouse_id filters the original observing facility. This is source evidence, not physical confirmation.",
            "GET", "/v1/serial-observations", Rights::Any,
            &[("sku",string()),("serial",string()),("warehouse_id",string()),("after",json!({"type":"string","minLength":1,"maxLength":4096})),("limit",integer(1,100)),("review",integer(0,i64::MAX))],
            None,true,false,true,
        ),
        definition(
            "wms_unit_observations",
            "Page original SKU pack observations and their original each conversion definitions. Never substitute a current pack size for a recorded observation. Retain review and next_cursor across pages.",
            "GET", "/v1/warehouses/{warehouse_id}/unit-observations", Rights::Any,
            &[("sku",string()),("code",string()),("after",json!({"type":"string","minLength":1,"maxLength":2048})),("limit",integer(1,100)),("review",integer(0,i64::MAX))],
            None,true,false,true,
        ),
        definition(
            "wms_lots",
            "Page lot expiry, independent active quarantine holds and current availability at the returned checked_at instant. Physical and reserved custody remain separate from usable storage units; retain this warehouse version before policy changes.",
            "GET",
            "/v1/warehouses/{warehouse_id}/lots",
            Rights::Any,
            &[("after",json!({"type":"string","minLength":1,"maxLength":2048})),("limit",integer(1,100)),("sku",string())],
            None,
            true,
            false,
            true,
        ),
        definition(
            "wms_reconciliation",
            "Read independent full-history stock, reservation, receipt, shipment, package and transfer proofs. A failed proof holds further work.",
            "GET",
            "/v1/warehouses/{warehouse_id}/reconciliation",
            Rights::Any,
            &[],
            None,
            true,
            false,
            true,
        ),
        definition(
            "wms_command_receipt",
            "Look up the original physical command by its warehouse request_id, under its authenticated actor. Missing does not prove noncommit; preserve the original envelope.",
            "GET",
            "/v1/warehouses/{warehouse_id}/commands/{request_id}",
            Rights::Any,
            &[],
            None,
            true,
            false,
            true,
        ),
        definition(
            "wms_outbox",
            "Read this tenant/facility's retained integration delivery evidence.",
            "GET",
            "/v1/warehouses/{warehouse_id}/outbox",
            Rights::Any,
            &[],
            None,
            true,
            false,
            true,
        ),
        definition(
            "wms_event",
            "Read one retained tenant-scoped original event.",
            "GET",
            "/v1/events/{event_id}",
            Rights::Any,
            &[],
            None,
            true,
            false,
            true,
        ),
        definition(
            "wms_operations",
            "Read the bounded tenant operations snapshot. A stale snapshot is not healthy operating evidence.",
            "GET",
            "/v1/operations/health",
            Rights::Worker,
            &[("after",string()),("limit",integer(1,100))],
            None,
            true,
            false,
            true,
        ),
        definition(
            "wms_metrics",
            "Read administrator-only Prometheus operational metrics.",
            "GET",
            "/v1/operations/metrics",
            Rights::Admin,
            &[],
            None,
            true,
            false,
            true,
        ),
        definition(
            "wms_replenishment_plan",
            "Plan bounded pick-face replenishment from explicit rules. This does not reserve or move stock; physical work uses a reviewed move_stock command.",
            "POST",
            "/v1/warehouses/{warehouse_id}/replenishment/plan",
            Rights::Worker,
            &[],
            Some(schema::<crate::replenishment::PlanRequest>()),
            true,
            false,
            true,
        ),
        definition(
            "wms_replenishment_rules",
            "Read the warehouse's saved pick-face replenishment rules.",
            "GET",
            "/v1/warehouses/{warehouse_id}/replenishment/rules",
            Rights::Any,
            &[],
            None,
            true,
            false,
            true,
        ),
        definition(
            "wms_set_replenishment_rules",
            "Replace the warehouse's saved replenishment rules as one audited change (an empty list clears them). Moves no stock.",
            "PUT",
            "/v1/warehouses/{warehouse_id}/replenishment/rules",
            Rights::Admin,
            &[],
            Some(schema::<crate::replenishment::PlanRequest>()),
            false,
            false,
            true,
        ),
        definition(
            "wms_replenishment_due",
            "Plan the replenishment work due now under the saved rules. Reserves and moves nothing; each task is a reviewed move_stock command.",
            "GET",
            "/v1/warehouses/{warehouse_id}/replenishment/due",
            Rights::Worker,
            &[],
            None,
            true,
            false,
            true,
        ),
        definition(
            "wms_putaway_plan",
            "Suggest storage bins for stock waiting in receiving: same lot, then same SKU, then empty bins. Reserves and moves nothing; each task is a reviewed move_stock command. No bin capacity model.",
            "POST",
            "/v1/warehouses/{warehouse_id}/putaway/plan",
            Rights::Worker,
            &[],
            Some(schema::<crate::putaway::PlanRequest>()),
            true,
            false,
            true,
        ),
        definition(
            "wms_batch_pick_plan",
            "Plan one walk that picks several orders: stops sorted by bin path, one tote per order. Reserves and moves nothing; each pick is the line's reviewed pick command.",
            "POST",
            "/v1/warehouses/{warehouse_id}/picking/batch-plan",
            Rights::Worker,
            &[],
            Some(schema::<crate::batch_pick::PlanRequest>()),
            true,
            false,
            true,
        ),
        definition(
            "wms_cycle_count_plan",
            "List bins and lots due for a cycle count by ABC class of picked volume. Blind by default; each task becomes a count_stock command only when the counter enters the observed quantity.",
            "POST",
            "/v1/warehouses/{warehouse_id}/counts/plan",
            Rights::Worker,
            &[],
            Some(schema::<crate::cycle_counts::PlanRequest>()),
            true,
            false,
            true,
        ),
        definition(
            "wms_print_labels",
            "Render ZPL labels (203 dpi) for bins, products, packages, serial units or a package's carrier label. Printing changes nothing; values a Code 128 barcode cannot encode are rejected, never altered.",
            "POST",
            "/v1/warehouses/{warehouse_id}/labels",
            Rights::Any,
            &[],
            Some(schema::<crate::labels::PrintRequest>()),
            true,
            false,
            true,
        ),
        definition(
            "wms_carrier_labels",
            "List a package's carrier labels, including voided ones.",
            "GET",
            "/v1/warehouses/{warehouse_id}/packages/{package_id}/carrier-labels",
            Rights::Any,
            &[],
            None,
            true,
            false,
            true,
        ),
        definition(
            "wms_record_carrier_label",
            "Attach the actual carrier label (carrier, service, tracking number, exact decimal rate, observed carton, ZPL or HTTPS label) to an open package. Moves no inventory and does not hand off the package.",
            "POST",
            "/v1/warehouses/{warehouse_id}/packages/{package_id}/carrier-labels",
            Rights::Worker,
            &[],
            Some(schema::<crate::carrier_labels::RecordRequest>()),
            false,
            false,
            false,
        ),
        definition(
            "wms_void_carrier_label",
            "Void a package's active carrier label with a reason, so a replacement can be recorded.",
            "POST",
            "/v1/warehouses/{warehouse_id}/packages/{package_id}/carrier-labels/{label_id}/void",
            Rights::Worker,
            &[],
            Some(schema::<crate::carrier_labels::VoidRequest>()),
            false,
            false,
            false,
        ),
        definition(
            "wms_oms_orders",
            "Page canonical orders from the tenant's explicitly configured OMS. Contacts the configured service.",
            "GET",
            "/v1/integrations/oms/orders",
            Rights::Worker,
            &[("page",integer(1,u32::MAX.into()))],
            None,
            true,
            true,
            true,
        ),
        definition(
            "wms_readiness",
            "Read warehouse/source readiness without creating a Freight load or recording a carrier pickup.",
            "GET",
            "/v1/warehouses/{warehouse_id}/orders/{order_id}/readiness",
            Rights::Worker,
            &[],
            None,
            true,
            false,
            true,
        ),
        definition(
            "wms_oms_proposal",
            "Review the original complete physical handoff and conditional OMS proposal. Contacts the configured OMS; creates no fulfillment.",
            "GET",
            "/v1/warehouses/{warehouse_id}/orders/{order_id}/oms-fulfillment",
            Rights::Worker,
            &[],
            None,
            true,
            true,
            true,
        ),
        definition(
            "wms_native_receipt",
            "Recover a retained native import/transport/fulfillment request under its original actor. Retry its original input; never replace an uncertain intent.",
            "GET",
            "/v1/warehouses/{warehouse_id}/native-requests/{request_id}",
            Rights::Import,
            &[],
            None,
            true,
            false,
            true,
        ),
        definition(
            "wms_import_oms",
            "Import a reviewed canonical OMS order using its exact source revision and original request. Contacts the configured OMS; source eligibility remains authoritative.",
            "POST",
            "/v1/warehouses/{warehouse_id}/imports/oms",
            Rights::Import,
            &[],
            Some(schema::<crate::native_api::ImportRequest>()),
            false,
            true,
            true,
        ),
        definition(
            "wms_transport",
            "Create/recover a planned Freight load from an explicit reviewed plan. This does not establish actual pickup, dispatch or delivery. Preserve the original native request.",
            "POST",
            "/v1/warehouses/{warehouse_id}/orders/{order_id}/transport",
            Rights::Worker,
            &[],
            Some(schema::<crate::native_api::TransportRequest>()),
            false,
            true,
            true,
        ),
        definition(
            "wms_oms_fulfillment",
            "Record/recover an actual complete original handoff in a qualified conditional OMS receiver. Partial/multiple handoffs remain held by the current receiver contract. Preserve the exact native request.",
            "POST",
            "/v1/warehouses/{warehouse_id}/orders/{order_id}/oms-fulfillment",
            Rights::Worker,
            &[],
            Some(schema::<crate::oms_fulfillment_api::FulfillmentRequest>()),
            false,
            true,
            true,
        ),
        definition(
            "wms_requeue_delivery",
            "Administrator requeue of a retained dead-letter delivery after evidence review. A lost response requires queue-state review; do not repeatedly requeue as recovery.",
            "POST",
            "/v1/outbox/{outbox_id}/retry",
            Rights::Admin,
            &[],
            None,
            false,
            true,
            false,
        ),
    ];
    // The wire command types and fields come from the same serde types as REST.
    let request_schema = schema::<Request>();
    let command_schema = schema::<Command>();
    let variants = command_schema
        .get("oneOf")
        .or_else(|| command_schema.get("anyOf"))
        .and_then(Value::as_array)
        .expect("tagged command schema variants");
    for variant in variants {
        let tag = &variant["properties"]["type"];
        let name = tag["const"]
            .as_str()
            .or_else(|| tag["enum"][0].as_str())
            .expect("command discriminator");
        let mut input = request_schema.clone();
        input["properties"]["command"] = variant.clone();
        if let Some(defs) = command_schema.get("$defs") {
            for (k, v) in defs.as_object().expect("schema definitions") {
                input["$defs"][k] = v.clone();
            }
        }
        let description=format!("Record the {name} warehouse action using the original canonical REST request. Supply a reviewed expected_version (and exact resource_versions only when supported), an original request_id, and command.type={name}. Values and physical evidence must come from observed work. Uncertain saves use wms_command_receipt and the exact original envelope. Warehouse permissions, source proofs, quantity caps and idempotency are enforced by the shared command path.");
        let mut d = definition(
            &format!("wms_{name}"),
            &description,
            "POST",
            "/v1/warehouses/{warehouse_id}/commands",
            Rights::Command(name.into()),
            &[],
            Some(input),
            false,
            true,
            true,
        );
        d.command = Some(name.into());
        tools.push(d);
    }
    let history = tools
        .iter_mut()
        .find(|d| d.tool.name == "wms_history")
        .expect("history tool");
    Arc::make_mut(&mut history.tool.input_schema)["properties"]["kind"] =
        json!({"type":"string","enum":["orders","receipts","movements","activity","exceptions"]});
    let trace = tools
        .iter_mut()
        .find(|d| d.tool.name == "wms_lot_trace")
        .expect("lot trace tool");
    Arc::make_mut(&mut trace.tool.input_schema)["required"] = json!(["sku", "lot"]);
    for name in ["wms_unit_packs", "wms_unit_observations"] {
        let d = tools
            .iter_mut()
            .find(|d| d.tool.name == name)
            .expect("unit read tool");
        Arc::make_mut(&mut d.tool.input_schema)["required"] = json!(["warehouse_id", "sku"]);
    }
    for name in [
        "wms_serials",
        "wms_serial_history",
        "wms_serial_observations",
    ] {
        let d = tools
            .iter_mut()
            .find(|d| d.tool.name == name)
            .expect("serial read tool");
        Arc::make_mut(&mut d.tool.input_schema)["required"] = if name == "wms_serial_history" {
            json!(["sku", "serial"])
        } else {
            json!(["sku"])
        };
    }
    tools
}
#[derive(Clone)]
struct WarehouseMcp {
    backend: Router,
}
fn identity(context: &RequestContext<RoleServer>) -> Result<Identity, ErrorData> {
    context
        .extensions
        .get::<Parts>()
        .and_then(|p| p.extensions.get::<Identity>())
        .cloned()
        .ok_or_else(|| ErrorData::internal_error("Authenticated HTTP identity missing", None))
}
fn failure(status: u16, message: &str) -> CallToolResponse {
    CallToolResult::structured_error(json!({"http_status":status,"body":{"error":message}})).into()
}
fn segment(value: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut result = String::new();
    for b in value.bytes() {
        if b.is_ascii_alphanumeric() || b"-_".contains(&b) {
            result.push(b as char);
        } else {
            result.push('%');
            result.push(HEX[(b >> 4) as usize] as char);
            result.push(HEX[(b & 15) as usize] as char);
        }
    }
    result
}
impl WarehouseMcp {
    async fn forward(
        &self,
        d: &Definition,
        mut args: Map<String, Value>,
        principal: Identity,
    ) -> CallToolResponse {
        let schema = &d.tool.input_schema;
        if args.keys().any(|k| schema["properties"].get(k).is_none())
            || schema["required"]
                .as_array()
                .expect("required schema")
                .iter()
                .any(|k| !args.contains_key(k.as_str().expect("field")))
        {
            return failure(422,"Missing or unexpected tool argument. Identity and URL targets cannot be supplied by the caller.");
        }
        for (key, value) in &args {
            if key == "request" {
                if !value.is_object() {
                    return failure(422, "The original request must be an object");
                }
                continue;
            }
            let p = &schema["properties"][key];
            let valid = match p["type"].as_str() {
                Some("string") => value.as_str().is_some_and(|s| {
                    s.len() >= p["minLength"].as_u64().unwrap_or(1) as usize
                        && s.len() <= p["maxLength"].as_u64().unwrap_or(240) as usize
                        && !s.chars().any(char::is_control)
                }),
                Some("integer") => value.as_i64().is_some_and(|n| {
                    n >= p["minimum"].as_i64().unwrap_or(i64::MIN)
                        && n <= p["maximum"].as_i64().unwrap_or(i64::MAX)
                }),
                Some("boolean") => value.is_boolean(),
                _ => false,
            };
            if !valid
                || p.get("enum")
                    .and_then(Value::as_array)
                    .is_some_and(|e| !e.contains(value))
            {
                return failure(422, "Tool argument is outside its declared type or bounds");
            }
        }
        let mut path = d.path.to_owned();
        while let Some(start) = path.find('{') {
            let end = path[start..].find('}').expect("static route") + start;
            let key = &path[start + 1..end];
            let Some(value) = args.remove(key).and_then(|v| v.as_str().map(str::to_owned)) else {
                return failure(422, "Missing canonical resource identity");
            };
            path.replace_range(start..=end, &segment(&value));
        }
        let body = args.remove("request").unwrap_or(json!(null));
        if let Some(command) = &d.command {
            let parsed: Request = match serde_json::from_value(body.clone()) {
                Ok(r) => r,
                Err(_) => return failure(422, "Invalid original warehouse request envelope"),
            };
            if serde_json::to_value(&parsed.command).expect("command JSON")["type"] != *command {
                return failure(422, "Tool name and original command type disagree");
            }
            if !parsed.command.allowed(&principal.role) {
                return failure(
                    403,
                    "This warehouse action is not permitted for the authenticated role",
                );
            }
        }
        if !args.is_empty() {
            path.push('?');
            for (index, (key, value)) in args.iter().enumerate() {
                if index > 0 {
                    path.push('&');
                }
                path.push_str(&segment(key));
                path.push('=');
                path.push_str(&segment(
                    &value
                        .as_str()
                        .map(str::to_owned)
                        .unwrap_or_else(|| value.to_string()),
                ));
            }
        }
        let mut request = HttpRequest::builder()
            .method(d.method)
            .uri(path)
            .header("content-type", "application/json")
            .body(if body.is_null() {
                Body::empty()
            } else {
                Body::from(body.to_string())
            })
            .expect("canonical internal request");
        request.extensions_mut().insert(principal);
        let response = match self.backend.clone().oneshot(request).await {
            Ok(r) => r,
            Err(never) => match never {},
        };
        let status = response.status().as_u16();
        let bytes=match to_bytes(response.into_body(),8*1024*1024).await {Ok(b)=>b,Err(_)=>return failure(502,"Warehouse response exceeds the MCP limit. Use paged history or canonical detail tools; no partial snapshot was returned.")};
        let value: Value = serde_json::from_slice(&bytes)
            .unwrap_or_else(|_| json!({"text":String::from_utf8_lossy(&bytes)}));
        let structured = json!({"http_status":status,"body":value});
        if status >= 400 {
            CallToolResult::structured_error(structured).into()
        } else {
            CallToolResult::structured(structured).into()
        }
    }
}
impl ServerHandler for WarehouseMcp {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new(
                "stateset-warehouse",
                env!("CARGO_PKG_VERSION"),
            ))
            .with_instructions(INSTRUCTIONS)
    }
    fn get_tool(&self, name: &str) -> Option<Tool> {
        catalog()
            .iter()
            .find(|d| d.tool.name == name)
            .map(|d| d.tool.clone())
    }
    async fn list_tools(
        &self,
        request: Option<PaginatedRequestParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        let principal = identity(&context)?;
        if request.as_ref().and_then(|r| r.cursor.as_ref()).is_some() {
            return Err(ErrorData::invalid_params(
                "This complete tool catalog has no cursor",
                None,
            ));
        }
        let tools = catalog()
            .iter()
            .filter(|d| d.rights.visible(&principal.role))
            .map(|d| d.tool.clone())
            .collect();
        Ok(ListToolsResult::with_all_items(tools))
    }
    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        let principal = identity(&context)?;
        let Some(d) = catalog().iter().find(|d| d.tool.name == request.name) else {
            return Err(ErrorData::new(
                rmcp::model::ErrorCode::METHOD_NOT_FOUND,
                "Unknown warehouse tool",
                None,
            ));
        };
        if !d.rights.visible(&principal.role) {
            return Ok(failure(
                403,
                "This tool requires a different warehouse role",
            ));
        }
        let correlation = context
            .extensions
            .get::<Parts>()
            .and_then(|p| p.headers.get("x-request-id"))
            .and_then(|v| v.to_str().ok())
            .unwrap_or("unavailable");
        let span = tracing::info_span!("warehouse_mcp",tool=%request.name,request_id=correlation);
        Ok(self
            .forward(d, request.arguments.unwrap_or_default(), principal)
            .instrument(span)
            .await)
    }
}
pub(crate) fn router(app: App, config: Config) -> Router<App> {
    let backend = crate::api::business_routes()
        .layer(DefaultBodyLimit::max(256 * 1024))
        .with_state(app);
    let service = StreamableHttpService::new(
        move || {
            Ok(WarehouseMcp {
                backend: backend.clone(),
            })
        },
        Arc::new(LocalSessionManager::default()),
        StreamableHttpServerConfig::default()
            .with_legacy_session_mode(false)
            .with_json_response(true)
            .with_max_request_body_bytes(512 * 1024)
            .with_allowed_hosts(config.allowed_hosts)
            .with_allowed_origins(config.allowed_origins)
            .enforce_origin_validation(),
    );
    Router::new().route_service("/mcp", service)
}
