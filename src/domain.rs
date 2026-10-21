//! Pure warehouse transitions. Persistence commits a validated result atomically.
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use uuid::Uuid;

pub type Result<T> = std::result::Result<T, String>;
pub fn id() -> String {
    Uuid::new_v4().to_string()
}
pub fn now() -> String {
    Utc::now().to_rfc3339()
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Product {
    #[serde(default, skip_serializing_if = "is_false")]
    pub serial_controlled: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub expiry_required: bool,
    pub sku: String,
    pub name: String,
    pub category: String,
    pub reorder_point: i64,
}
fn is_false(value: &bool) -> bool {
    !value
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Location {
    pub code: String,
    pub zone: String,
    pub kind: String,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Stock {
    pub sku: String,
    pub lot: String,
    pub location: String,
    pub on_hand: i64,
    pub reserved: i64,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ReceiptLine {
    pub id: String,
    pub sku: String,
    pub lot: String,
    pub quantity: i64,
    pub received: i64,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Receipt {
    pub id: String,
    pub reference: String,
    pub supplier: String,
    pub status: String,
    pub lines: Vec<ReceiptLine>,
    pub created_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub return_source: Option<crate::returns::ReturnSource>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cancellation: Option<crate::receipts::Cancellation>,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct OrderLine {
    pub id: String,
    pub sku: String,
    pub quantity: i64,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Allocation {
    pub line_id: String,
    pub sku: String,
    pub lot: String,
    pub location: String,
    pub quantity: i64,
    pub picked: i64,
    pub shipped: i64,
    pub released: i64,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Order {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remainder_cancellation: Option<crate::order_cancellation::Cancellation>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shipment_mode: Option<String>,
    pub id: String,
    pub reference: String,
    pub source_order_id: Option<String>,
    pub customer: String,
    pub destination: String,
    pub priority: String,
    pub status: String,
    pub lines: Vec<OrderLine>,
    pub allocations: Vec<Allocation>,
    pub carrier: String,
    pub handoff_reference: String,
    pub created_at: String,
    pub shipped_at: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Movement {
    pub id: String,
    pub kind: String,
    pub sku: String,
    pub lot: String,
    pub from: Option<String>,
    pub to: Option<String>,
    pub quantity: i64,
    pub reference: String,
    #[serde(default)]
    pub receipt_id: Option<String>,
    #[serde(default)]
    pub receipt_line_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transfer_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transfer_line_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transfer_event_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub package_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub order_line_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allocation_location: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shipment_event_id: Option<String>,
    pub reason: String,
    pub actor: String,
    pub at: String,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Exception {
    pub id: String,
    pub order_id: Option<String>,
    pub category: String,
    pub detail: String,
    pub status: String,
    pub resolution: String,
    pub actor: String,
    pub resolved_by: Option<String>,
    pub created_at: String,
    pub resolved_at: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Activity {
    pub id: String,
    pub kind: String,
    pub reference: String,
    pub detail: String,
    pub actor: String,
    pub at: String,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Warehouse {
    pub id: String,
    pub code: String,
    pub name: String,
    pub city: String,
    pub timezone: String,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct State {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unit_packs: Vec<crate::units::Pack>,
    #[serde(skip)]
    pub(crate) unit_pack_change: Option<crate::units::Change>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub lot_controls: Vec<crate::lot_control::Lot>,
    #[serde(skip)]
    pub(crate) lot_changes: Vec<crate::lot_control::Change>,
    #[serde(skip)]
    pub(crate) execution_time: Option<chrono::DateTime<Utc>>,
    pub warehouse: Warehouse,
    pub version: i64,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub resource_versions: BTreeMap<String, i64>,
    pub products: Vec<Product>,
    pub locations: Vec<Location>,
    pub stocks: Vec<Stock>,
    /// Physical opening balances when persistence loads only new movement deltas.
    /// Reserved balances use exact allocations and the other-owner baseline.
    #[serde(default)]
    pub inventory_baseline: Vec<Stock>,
    /// SQL-derived holds owned by orders omitted from a selective command load.
    /// Never accepted from or exposed in serialized warehouse snapshots.
    #[serde(skip)]
    pub reservation_baseline: Vec<Stock>,
    /// Opening received quantities when only new receipt movements are loaded.
    #[serde(default)]
    pub receipt_baseline: BTreeMap<String, BTreeMap<String, i64>>,
    pub receipts: Vec<Receipt>,
    pub orders: Vec<Order>,
    pub movements: Vec<Movement>,
    pub exceptions: Vec<Exception>,
    pub activity: Vec<Activity>,
    /// Database-owned dispatch and arrivals; never accepted from JSON.
    #[serde(skip)]
    pub(crate) transfer_context: Option<crate::transfers::Transfer>,
    #[serde(skip)]
    pub(crate) shipment_context: Option<crate::split_fulfillment::Context>,
    #[serde(skip)]
    pub(crate) shipment_change: Option<crate::split_fulfillment::Change>,
}

/// The public read model is serialized directly, without an intermediate JSON
/// tree. Proof rows are computed once for both the validity flag and the body.
#[derive(Debug, Serialize)]
pub struct OperationalSnapshot {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub availability_checked_at: Option<String>,
    #[serde(flatten)]
    state: State,
    reconciliation: OperationalReconciliation,
}

impl OperationalSnapshot {
    pub(crate) fn console_source(&self) -> (&State, bool) {
        (&self.state, self.reconciliation.ok)
    }
}

#[derive(Debug, Serialize)]
struct OperationalReconciliation {
    version: i64,
    ok: bool,
    rows: Vec<BalanceProof>,
}

#[derive(Debug, Serialize)]
struct BalanceProof {
    sku: String,
    lot: String,
    location: String,
    on_hand: i64,
    ledger: i64,
    reserved: i64,
    allocation_reserved: i64,
    difference: i64,
    reserved_difference: i64,
    ok: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LineInput {
    pub id: String,
    pub sku: String,
    pub quantity: i64,
}
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ReceiptInput {
    pub id: String,
    pub sku: String,
    pub lot: String,
    pub quantity: i64,
}
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Command {
    SetUnitPack {
        sku: String,
        code: String,
        label: String,
        each_per_pack: i64,
        enabled: bool,
        reason: String,
    },
    SetProductExpiryPolicy {
        sku: String,
        required: bool,
        reason: String,
    },
    RecordLotExpiry {
        sku: String,
        lot: String,
        #[schemars(with = "chrono::DateTime<Utc>")]
        expires_at: String,
        reason: String,
    },
    CorrectLotExpiry {
        sku: String,
        lot: String,
        #[schemars(with = "Option<chrono::DateTime<Utc>>")]
        expires_at: Option<String>,
        reason: String,
    },
    QuarantineLot {
        sku: String,
        lot: String,
        reason: String,
    },
    ReleaseLotHold {
        hold_id: String,
        reason: String,
    },
    CancelRemainder {
        order_id: String,
        reason: String,
    },
    AllocateAvailable {
        order_id: String,
    },
    ReleaseUnpicked {
        order_id: String,
        reason: String,
    },
    PackShipment {
        order_id: String,
        reference: String,
        lines: Vec<crate::split_fulfillment::LineInput>,
    },
    UnpackShipment {
        shipment_id: String,
        reason: String,
    },
    HandoffShipment {
        shipment_id: String,
        carrier: String,
        handoff_reference: String,
    },
    DispatchTransfer {
        reference: String,
        destination_warehouse_id: String,
        carrier: String,
        handoff_reference: String,
        lines: Vec<crate::transfers::LineInput>,
    },
    ReceiveTransfer {
        transfer_id: String,
        line_id: String,
        quantity: i64,
        location: String,
        arrival_reference: String,
    },
    RecordTransferLoss {
        transfer_id: String,
        line_id: String,
        quantity: i64,
        incident_reference: String,
        reason: String,
    },
    ReverseTransferLoss {
        loss_id: String,
        quantity: i64,
        reason: String,
    },
    CreateWarehouse {
        code: String,
        name: String,
        city: String,
        timezone: String,
    },
    UpdateWarehouse {
        code: String,
        name: String,
        city: String,
        timezone: String,
    },
    CreateProduct {
        #[serde(default, skip_serializing_if = "is_false")]
        serial_controlled: bool,
        sku: String,
        name: String,
        category: String,
        reorder_point: i64,
    },
    CreateLocation {
        code: String,
        zone: String,
        kind: String,
    },
    CreateReceipt {
        reference: String,
        supplier: String,
        lines: Vec<ReceiptInput>,
    },
    CreateReturn {
        reference: String,
        order_id: String,
        reason: String,
        lines: Vec<crate::returns::ReturnLineInput>,
    },
    CancelReceipt {
        receipt_id: String,
        reason: String,
    },
    Receive {
        receipt_id: String,
        line_id: String,
        quantity: i64,
        location: String,
    },
    MoveStock {
        sku: String,
        lot: String,
        from: String,
        to: String,
        quantity: i64,
    },
    CountStock {
        sku: String,
        lot: String,
        location: String,
        counted: i64,
        reason: String,
    },
    CreateOrder {
        reference: String,
        source_order_id: Option<String>,
        customer: String,
        destination: String,
        priority: String,
        lines: Vec<LineInput>,
    },
    Allocate {
        order_id: String,
    },
    Pick {
        order_id: String,
        line_id: String,
        location: String,
        lot: String,
        quantity: i64,
    },
    Unpick {
        order_id: String,
        line_id: String,
        location: String,
        lot: String,
        quantity: i64,
        reason: String,
    },
    ResetAllocation {
        order_id: String,
        reason: String,
    },
    Pack {
        order_id: String,
    },
    Ship {
        order_id: String,
        carrier: String,
        handoff_reference: String,
    },
    CancelOrder {
        order_id: String,
        reason: String,
    },
    ReportException {
        order_id: Option<String>,
        category: String,
        detail: String,
    },
    ResolveException {
        exception_id: String,
        resolution: String,
    },
}
impl Command {
    pub fn kind(&self) -> &'static str {
        match self {
            Self::SetUnitPack { .. } => "inventory.unit_pack_changed",
            Self::SetProductExpiryPolicy { .. } => "inventory.expiry_policy_changed",
            Self::RecordLotExpiry { .. } => "lot.expiry_recorded",
            Self::CorrectLotExpiry { .. } => "lot.expiry_corrected",
            Self::QuarantineLot { .. } => "lot.quarantined",
            Self::ReleaseLotHold { .. } => "lot.hold_released",
            Self::CancelRemainder { .. } => "fulfillment.remainder_cancelled",
            Self::AllocateAvailable { .. } => "fulfillment.available_allocated",
            Self::ReleaseUnpicked { .. } => "fulfillment.unpicked_released",
            Self::PackShipment { .. } => "shipment.packed",
            Self::UnpackShipment { .. } => "shipment.unpacked",
            Self::HandoffShipment { .. } => "shipment.handed_off",
            Self::DispatchTransfer { .. } => "transfer.dispatched",
            Self::ReceiveTransfer { .. } => "transfer.received",
            Self::RecordTransferLoss { .. } => "transfer.loss_recorded",
            Self::ReverseTransferLoss { .. } => "transfer.loss_reversed",
            Self::CreateWarehouse { .. } => "warehouse.created",
            Self::UpdateWarehouse { .. } => "warehouse.updated",
            Self::CreateProduct { .. } => "product.created",
            Self::CreateLocation { .. } => "location.created",
            Self::CreateReceipt { .. } => "receipt.created",
            Self::CreateReturn { .. } => "return.authorized",
            Self::CancelReceipt { .. } => "receipt.cancelled",
            Self::Receive { .. } => "receipt.received",
            Self::MoveStock { .. } => "inventory.moved",
            Self::CountStock { .. } => "inventory.counted",
            Self::CreateOrder { .. } => "fulfillment.created",
            Self::Allocate { .. } => "fulfillment.allocated",
            Self::Pick { .. } => "fulfillment.picked",
            Self::Unpick { .. } => "fulfillment.unpicked",
            Self::ResetAllocation { .. } => "fulfillment.allocation_reset",
            Self::Pack { .. } => "fulfillment.packed",
            Self::Ship { .. } => "fulfillment.shipped",
            Self::CancelOrder { .. } => "fulfillment.cancelled",
            Self::ReportException { .. } => "exception.opened",
            Self::ResolveException { .. } => "exception.resolved",
        }
    }
    pub fn allowed(&self, role: &str) -> bool {
        match role {
            "admin" => true,
            "operator" => !matches!(
                self,
                Self::SetProductExpiryPolicy { .. }
                    | Self::SetUnitPack { .. }
                    | Self::RecordTransferLoss { .. }
                    | Self::ReverseTransferLoss { .. }
                    | Self::CorrectLotExpiry { .. }
                    | Self::ReleaseLotHold { .. }
                    | Self::CreateWarehouse { .. }
                    | Self::UpdateWarehouse { .. }
                    | Self::CreateProduct { .. }
                    | Self::CreateLocation { .. }
                    | Self::CountStock { .. }
            ),
            "oms" => matches!(
                self,
                Self::CreateOrder {
                    source_order_id: Some(_),
                    ..
                } | Self::CancelOrder { .. }
                    | Self::CancelRemainder { .. }
            ),
            _ => false,
        }
    }
}
fn text(s: &str, field: &str) -> Result<()> {
    if s.trim().is_empty() || s.len() > 240 || s.trim() != s || s.chars().any(char::is_control) {
        return Err(format!(
            "{field} must be 1–240 characters without surrounding whitespace or control characters"
        ));
    }
    Ok(())
}
fn quantity(q: i64) -> Result<()> {
    if !(1..=1_000_000).contains(&q) {
        return Err("Quantity must be a whole number between 1 and 1,000,000".into());
    }
    Ok(())
}
impl State {
    pub fn empty(warehouse: Warehouse) -> Self {
        Self {
            unit_packs: Vec::new(),
            unit_pack_change: None,
            lot_controls: vec![],
            lot_changes: vec![],
            execution_time: None,
            warehouse,
            version: 0,
            resource_versions: BTreeMap::new(),
            products: vec![],
            locations: vec![
                Location {
                    code: "RECV-01".into(),
                    zone: "Receiving".into(),
                    kind: "receiving".into(),
                },
                Location {
                    code: "STAGE-01".into(),
                    zone: "Outbound".into(),
                    kind: "staging".into(),
                },
            ],
            stocks: vec![],
            inventory_baseline: vec![],
            reservation_baseline: vec![],
            receipt_baseline: BTreeMap::new(),
            receipts: vec![],
            orders: vec![],
            movements: vec![],
            exceptions: vec![],
            activity: vec![],
            transfer_context: None,
            shipment_context: None,
            shipment_change: None,
        }
    }
    /// Upgrade original snapshots only when receipt ownership is unambiguous.
    /// Duplicate SKU/lot lines require original receipt evidence, never a guess.
    pub fn upgrade_legacy_receipt_evidence(&mut self) -> Result<()> {
        let mut assignments = Vec::new();
        for (index, movement) in self
            .movements
            .iter()
            .enumerate()
            .filter(|(_, m)| m.kind == "receipt")
        {
            if movement.receipt_id.is_some() || movement.receipt_line_id.is_some() {
                continue;
            }
            let candidates: Vec<_> = self
                .receipts
                .iter()
                .filter(|receipt| receipt.reference == movement.reference)
                .flat_map(|receipt| {
                    receipt.lines.iter().filter_map(|line| {
                        (line.sku == movement.sku && line.lot == movement.lot)
                            .then_some((receipt.id.clone(), line.id.clone()))
                    })
                })
                .collect();
            let [(receipt_id, line_id)] = candidates.as_slice() else {
                return Err(format!(
                    "Receipt movement {} requires original line identity evidence",
                    movement.id
                ));
            };
            assignments.push((index, receipt_id.clone(), line_id.clone()));
        }
        for (index, receipt_id, line_id) in assignments {
            self.movements[index].receipt_id = Some(receipt_id);
            self.movements[index].receipt_line_id = Some(line_id);
        }
        Ok(())
    }
    fn product(&self, sku: &str) -> Result<()> {
        if !self.products.iter().any(|p| p.sku == sku) {
            return Err(format!("Unknown SKU: {sku}"));
        }
        Ok(())
    }
    fn location(&self, code: &str) -> Result<&Location> {
        self.locations
            .iter()
            .find(|l| l.code == code)
            .ok_or_else(|| format!("Unknown location: {code}"))
    }
    pub(crate) fn order_index(&self, id: &str) -> Result<usize> {
        self.orders
            .iter()
            .position(|o| o.id == id)
            .ok_or_else(|| "Fulfillment order not found".into())
    }
    pub(crate) fn unblocked(&self, order: &str) -> Result<()> {
        if self
            .exceptions
            .iter()
            .any(|e| e.status == "open" && e.order_id.as_deref() == Some(order))
        {
            return Err("Resolve the blocking order exception before continuing".into());
        }
        Ok(())
    }
    pub(crate) fn bucket(&mut self, sku: &str, lot: &str, location: &str) -> &mut Stock {
        let pos = match self
            .stocks
            .iter()
            .position(|s| s.sku == sku && s.lot == lot && s.location == location)
        {
            Some(i) => i,
            None => {
                self.stocks.push(Stock {
                    sku: sku.into(),
                    lot: lot.into(),
                    location: location.into(),
                    on_hand: 0,
                    reserved: 0,
                });
                self.stocks.len() - 1
            }
        };
        &mut self.stocks[pos]
    }
    // Keep all required movement evidence at one construction point.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn movement(
        &mut self,
        kind: &str,
        sku: &str,
        lot: &str,
        from: Option<&str>,
        to: Option<&str>,
        q: i64,
        reference: &str,
        reason: &str,
        actor: &str,
    ) {
        self.movements.push(Movement {
            id: id(),
            kind: kind.into(),
            sku: sku.into(),
            lot: lot.into(),
            from: from.map(str::to_owned),
            to: to.map(str::to_owned),
            quantity: q,
            reference: reference.into(),
            receipt_id: None,
            receipt_line_id: None,
            transfer_id: None,
            transfer_line_id: None,
            transfer_event_id: None,
            package_id: None,
            order_line_id: None,
            allocation_location: None,
            shipment_event_id: None,
            reason: reason.into(),
            actor: actor.into(),
            at: now(),
        });
    }
    pub fn execute(&self, command: &Command, actor: &str) -> Result<(Self, Value)> {
        self.execute_at(command, actor, Utc::now())
    }
    pub fn execute_at(
        &self,
        command: &Command,
        actor: &str,
        at: chrono::DateTime<Utc>,
    ) -> Result<(Self, Value)> {
        let mut next = self.clone();
        next.unit_pack_change = None;
        next.execution_time = Some(at);
        next.lot_changes.clear();
        next.shipment_change = None;
        let entity = next.apply(command, actor)?;
        next.version = next
            .version
            .checked_add(1)
            .ok_or("Warehouse version limit reached")?;
        next.activity.push(Activity {
            id: id(),
            kind: command.kind().into(),
            reference: entity.clone(),
            detail: match command {
                Command::SetProductExpiryPolicy { reason, .. }
                | Command::RecordLotExpiry { reason, .. }
                | Command::CorrectLotExpiry { reason, .. }
                | Command::QuarantineLot { reason, .. }
                | Command::ReleaseLotHold { reason, .. }
                | Command::Unpick { reason, .. }
                | Command::ResetAllocation { reason, .. }
                | Command::CancelReceipt { reason, .. }
                | Command::CancelRemainder { reason, .. }
                | Command::CancelOrder { reason, .. } => {
                    format!("{}: {reason}", command.kind().replace('.', " "))
                }
                _ => command.kind().replace('.', " "),
            },
            actor: actor.into(),
            at: now(),
        });
        next.validate()?;
        let result =
            json!({ "entity_id": entity, "version": next.version, "type": command.kind() });
        Ok((next, result))
    }
    pub(crate) fn quality_time(&self) -> chrono::DateTime<Utc> {
        self.execution_time.unwrap_or_else(Utc::now)
    }
    fn apply(&mut self, command: &Command, actor: &str) -> Result<String> {
        if matches!(command, Command::CreateWarehouse { .. })
            && (self.version != 0
                || !self.products.is_empty()
                || !self.orders.is_empty()
                || !self.receipts.is_empty()
                || !self.movements.is_empty()
                || !self.activity.is_empty())
        {
            return Err("Warehouse creation requires a new empty facility".into());
        }
        match command {
            Command::SetUnitPack { .. } => crate::units::apply(self, command, actor),
            Command::SetProductExpiryPolicy { .. }
            | Command::RecordLotExpiry { .. }
            | Command::CorrectLotExpiry { .. }
            | Command::QuarantineLot { .. }
            | Command::ReleaseLotHold { .. } => crate::lot_control::apply(self, command, actor),
            Command::CancelRemainder { order_id, reason } => {
                crate::order_cancellation::cancel(self, order_id, reason, actor)
            }
            Command::AllocateAvailable { .. }
            | Command::ReleaseUnpicked { .. }
            | Command::PackShipment { .. }
            | Command::UnpackShipment { .. }
            | Command::HandoffShipment { .. } => {
                crate::split_fulfillment::apply(self, command, actor)
            }
            Command::DispatchTransfer { .. }
            | Command::ReceiveTransfer { .. }
            | Command::RecordTransferLoss { .. }
            | Command::ReverseTransferLoss { .. } => crate::transfers::apply(self, command, actor),
            Command::CreateWarehouse {
                code,
                name,
                city,
                timezone,
            }
            | Command::UpdateWarehouse {
                code,
                name,
                city,
                timezone,
            } => {
                for (s, f) in [
                    (code, "Warehouse code"),
                    (name, "Warehouse name"),
                    (city, "City"),
                    (timezone, "Timezone"),
                ] {
                    text(s, f)?;
                }
                timezone
                    .parse::<chrono_tz::Tz>()
                    .map_err(|_| "Use a valid IANA timezone such as America/Los_Angeles")?;
                self.warehouse.code = code.clone();
                self.warehouse.name = name.clone();
                self.warehouse.city = city.clone();
                self.warehouse.timezone = timezone.clone();
                Ok(self.warehouse.id.clone())
            }
            Command::CreateProduct {
                serial_controlled,
                sku,
                name,
                category,
                reorder_point,
            } => {
                for (s, f) in [(sku, "SKU"), (name, "Product name"), (category, "Category")] {
                    text(s, f)?;
                }
                if !(0..=1_000_000).contains(reorder_point) {
                    return Err("Reorder point must be 0–1,000,000".into());
                }
                if self.products.iter().any(|p| p.sku == *sku) {
                    return Err("SKU already exists".into());
                }
                self.products.push(Product {
                    serial_controlled: *serial_controlled,
                    expiry_required: false,
                    sku: sku.clone(),
                    name: name.clone(),
                    category: category.clone(),
                    reorder_point: *reorder_point,
                });
                Ok(sku.clone())
            }
            Command::CreateLocation { code, zone, kind } => {
                text(code, "Location code")?;
                text(zone, "Zone")?;
                if kind != "storage" && kind != "receiving" {
                    return Err(
                        "New locations must be storage or receiving; staging is system-managed"
                            .into(),
                    );
                }
                if self.locations.iter().any(|l| l.code == *code) {
                    return Err("Location already exists".into());
                }
                self.locations.push(Location {
                    code: code.clone(),
                    zone: zone.clone(),
                    kind: kind.clone(),
                });
                Ok(code.clone())
            }
            Command::CreateReceipt {
                reference,
                supplier,
                lines,
            } => {
                text(reference, "Receipt reference")?;
                text(supplier, "Supplier")?;
                if self.receipts.iter().any(|r| r.reference == *reference) {
                    return Err("Receipt reference already exists".into());
                }
                if lines.is_empty() || lines.len() > 100 {
                    return Err("A receipt requires 1–100 lines".into());
                }
                let mut seen = BTreeSet::new();
                for l in lines {
                    text(&l.id, "Line ID")?;
                    self.product(&l.sku)?;
                    text(&l.lot, "Lot")?;
                    quantity(l.quantity)?;
                    if !seen.insert(&l.id) {
                        return Err("Receipt line IDs must be unique".into());
                    }
                }
                let entity = id();
                self.receipts.push(Receipt {
                    id: entity.clone(),
                    reference: reference.clone(),
                    supplier: supplier.clone(),
                    status: "expected".into(),
                    lines: lines
                        .iter()
                        .map(|l| ReceiptLine {
                            id: l.id.clone(),
                            sku: l.sku.clone(),
                            lot: l.lot.clone(),
                            quantity: l.quantity,
                            received: 0,
                        })
                        .collect(),
                    created_at: now(),
                    return_source: None,
                    cancellation: None,
                });
                Ok(entity)
            }
            Command::CreateReturn {
                reference,
                order_id,
                reason,
                lines,
            } => {
                text(reason, "Return reason / inspection evidence")?;
                let (inputs, source, _original_reference) =
                    crate::returns::prepare(self, order_id, reason, lines)?;
                let receipt_id = self.apply(
                    &Command::CreateReceipt {
                        reference: reference.clone(),
                        supplier: "Customer return".into(),
                        lines: inputs,
                    },
                    actor,
                )?;
                self.receipts
                    .iter_mut()
                    .find(|r| r.id == receipt_id)
                    .ok_or("Return receipt missing")?
                    .return_source = Some(source);
                Ok(receipt_id)
            }
            Command::CancelReceipt { receipt_id, reason } => {
                text(reason, "Cancellation reason")?;
                let receipt = self
                    .receipts
                    .iter_mut()
                    .find(|r| r.id == *receipt_id)
                    .ok_or("Receipt not found")?;
                crate::receipts::cancel(receipt, reason, actor)?;
                Ok(receipt_id.clone())
            }
            Command::Receive {
                receipt_id,
                line_id,
                quantity: q,
                location,
            } => {
                quantity(*q)?;
                if self.location(location)?.kind != "receiving" {
                    return Err(
                        "Receive goods into a receiving location, then put them away".into(),
                    );
                }
                let receipt = self
                    .receipts
                    .iter_mut()
                    .find(|r| r.id == *receipt_id)
                    .ok_or("Receipt not found")?;
                if receipt.cancellation.is_some() || receipt.status == "cancelled" {
                    return Err("Cancelled receipt cannot receive additional goods".into());
                }
                let line = receipt
                    .lines
                    .iter_mut()
                    .find(|l| l.id == *line_id)
                    .ok_or("Receipt line not found")?;
                if line.received + q > line.quantity {
                    return Err("Receiving exceeds the expected quantity".into());
                }
                line.received += q;
                let (sku, lot, reference) = (
                    line.sku.clone(),
                    line.lot.clone(),
                    receipt.reference.clone(),
                );
                receipt.status = if receipt.lines.iter().all(|l| l.received == l.quantity) {
                    "received"
                } else {
                    "partial"
                }
                .into();
                self.bucket(&sku, &lot, location).on_hand += q;
                self.movement(
                    "receipt",
                    &sku,
                    &lot,
                    None,
                    Some(location),
                    *q,
                    &reference,
                    "Supplier receipt",
                    actor,
                );
                if let Some(movement) = self.movements.last_mut() {
                    movement.receipt_id = Some(receipt_id.clone());
                    movement.receipt_line_id = Some(line_id.clone());
                }
                Ok(receipt_id.clone())
            }
            Command::MoveStock {
                sku,
                lot,
                from,
                to,
                quantity: q,
            } => {
                quantity(*q)?;
                self.product(sku)?;
                text(lot, "Lot")?;
                if from == to {
                    return Err("Source and destination must differ".into());
                }
                if self.location(from)?.kind == "staging" || self.location(to)?.kind != "storage" {
                    return Err(
                        "Putaway/moves must end in storage and cannot move staged order stock"
                            .into(),
                    );
                }
                let source = self.bucket(sku, lot, from);
                if source.on_hand - source.reserved < *q {
                    return Err("Insufficient unreserved stock at source".into());
                }
                source.on_hand -= q;
                self.bucket(sku, lot, to).on_hand += q;
                self.movement(
                    "putaway",
                    sku,
                    lot,
                    Some(from),
                    Some(to),
                    *q,
                    "transfer",
                    "Bin transfer",
                    actor,
                );
                Ok(format!("{from} → {to}"))
            }
            Command::CountStock {
                sku,
                lot,
                location,
                counted,
                reason,
            } => {
                self.product(sku)?;
                text(lot, "Lot")?;
                text(reason, "Count reason")?;
                if !(0..=1_000_000).contains(counted) {
                    return Err("Count must be 0–1,000,000 whole units".into());
                }
                if self.location(location)?.kind == "staging" {
                    return Err("Staging stock is controlled by fulfillment allocations".into());
                }
                let b = self.bucket(sku, lot, location);
                if *counted < b.reserved {
                    return Err(
                        "Count is below reserved stock; reconcile affected allocations first"
                            .into(),
                    );
                }
                let delta = counted - b.on_hand;
                b.on_hand = *counted;
                if delta != 0 {
                    self.movement(
                        "count",
                        sku,
                        lot,
                        if delta < 0 { Some(location) } else { None },
                        if delta > 0 { Some(location) } else { None },
                        delta.abs(),
                        "cycle count",
                        reason,
                        actor,
                    );
                }
                Ok(format!("{sku} / {location}"))
            }
            Command::CreateOrder {
                reference,
                source_order_id,
                customer,
                destination,
                priority,
                lines,
            } => {
                for (s, f) in [
                    (reference, "Order reference"),
                    (customer, "Customer"),
                    (destination, "Destination"),
                ] {
                    text(s, f)?;
                }
                if let Some(source) = source_order_id {
                    text(source, "OMS order ID")?;
                }
                if !["normal", "high", "urgent"].contains(&priority.as_str()) {
                    return Err("Priority must be normal, high, or urgent".into());
                }
                if self.orders.iter().any(|o| {
                    o.reference == *reference
                        || (source_order_id.is_some() && o.source_order_id == *source_order_id)
                }) {
                    return Err("Order reference or OMS order ID already exists".into());
                }
                if lines.is_empty() || lines.len() > 100 {
                    return Err("An order requires 1–100 lines".into());
                }
                let mut seen = BTreeSet::new();
                for l in lines {
                    text(&l.id, "Line ID")?;
                    self.product(&l.sku)?;
                    quantity(l.quantity)?;
                    if !seen.insert(&l.id) {
                        return Err("Order line IDs must be unique".into());
                    }
                }
                let entity = id();
                self.orders.push(Order {
                    remainder_cancellation: None,
                    shipment_mode: None,
                    id: entity.clone(),
                    reference: reference.clone(),
                    source_order_id: source_order_id.clone(),
                    customer: customer.clone(),
                    destination: destination.clone(),
                    priority: priority.clone(),
                    status: "pending".into(),
                    lines: lines
                        .iter()
                        .map(|l| OrderLine {
                            id: l.id.clone(),
                            sku: l.sku.clone(),
                            quantity: l.quantity,
                        })
                        .collect(),
                    allocations: vec![],
                    carrier: String::new(),
                    handoff_reference: String::new(),
                    created_at: now(),
                    shipped_at: None,
                });
                Ok(entity)
            }
            Command::Allocate { order_id } => {
                crate::split_fulfillment::require_legacy(self, order_id)?;
                self.unblocked(order_id)?;
                let i = self.order_index(order_id)?;
                if self.orders[i].status != "pending" {
                    return Err("Only pending orders can be allocated".into());
                }
                let lines = self.orders[i].lines.clone();
                let mut allocations = vec![];
                for l in lines {
                    let mut remaining = l.quantity;
                    let storage: BTreeSet<_> = self
                        .locations
                        .iter()
                        .filter(|x| x.kind == "storage")
                        .map(|x| x.code.as_str())
                        .collect();
                    let mut bins: Vec<_> = self
                        .stocks
                        .iter()
                        .enumerate()
                        .filter(|(_, s)| {
                            s.sku == l.sku
                                && storage.contains(s.location.as_str())
                                && s.on_hand > s.reserved
                                && crate::lot_control::eligible(self, &s.sku, &s.lot)
                        })
                        .map(|(j, s)| (j, s.location.clone(), s.lot.clone()))
                        .collect();
                    bins.sort_by_key(|a| {
                        (
                            crate::lot_control::fefo(self, &l.sku, &a.2),
                            a.1.clone(),
                            a.2.clone(),
                        )
                    });
                    for (j, _, _) in bins {
                        let stock = &mut self.stocks[j];
                        let q = remaining.min(stock.on_hand - stock.reserved);
                        if q == 0 {
                            continue;
                        }
                        stock.reserved += q;
                        remaining -= q;
                        allocations.push(Allocation {
                            line_id: l.id.clone(),
                            sku: l.sku.clone(),
                            lot: stock.lot.clone(),
                            location: stock.location.clone(),
                            quantity: q,
                            picked: 0,
                            shipped: 0,
                            released: 0,
                        });
                        if remaining == 0 {
                            break;
                        }
                    }
                    if remaining > 0 {
                        return Err(format!(
                            "Insufficient available storage stock for {} ({} units short)",
                            l.sku, remaining
                        ));
                    }
                }
                self.orders[i].allocations = allocations;
                self.orders[i].status = "allocated".into();
                Ok(order_id.clone())
            }
            Command::Pick {
                order_id,
                line_id,
                location,
                lot,
                quantity: q,
            } => {
                if self.orders[self.order_index(order_id)?]
                    .shipment_mode
                    .is_some()
                {
                    return crate::split_fulfillment::pick(self, command, actor);
                }
                quantity(*q)?;
                self.unblocked(order_id)?;
                let i = self.order_index(order_id)?;
                if !["allocated", "picking"].contains(&self.orders[i].status.as_str()) {
                    return Err("Order is not ready to pick".into());
                }
                let sku = self.orders[i]
                    .allocations
                    .iter()
                    .find(|a| a.line_id == *line_id && a.location == *location && a.lot == *lot)
                    .ok_or("Original allocation not found")?
                    .sku
                    .clone();
                crate::lot_control::require(self, &sku, lot)?;
                let order = &mut self.orders[i];
                let a = order
                    .allocations
                    .iter_mut()
                    .find(|a| a.line_id == *line_id && a.location == *location && a.lot == *lot)
                    .ok_or("No reservation matches this line, location and lot")?;
                if a.quantity - a.picked - a.released < *q {
                    return Err("Pick exceeds remaining reservation".into());
                }
                a.picked += q;
                let sku = a.sku.clone();
                let reference = order.reference.clone();
                order.status = if order.allocations.iter().all(|a| a.picked == a.quantity) {
                    "picked"
                } else {
                    "picking"
                }
                .into();
                let source = self.bucket(&sku, lot, location);
                source.on_hand -= q;
                source.reserved -= q;
                let staging = self.bucket(&sku, lot, "STAGE-01");
                staging.on_hand += q;
                staging.reserved += q;
                self.movement(
                    "pick",
                    &sku,
                    lot,
                    Some(location),
                    Some("STAGE-01"),
                    *q,
                    &reference,
                    "Order line pick",
                    actor,
                );
                Ok(order_id.clone())
            }
            Command::Unpick {
                order_id,
                line_id,
                location,
                lot,
                quantity: q,
                reason,
            } => {
                if self.orders[self.order_index(order_id)?]
                    .shipment_mode
                    .is_some()
                {
                    return crate::split_fulfillment::pick(self, command, actor);
                }
                quantity(*q)?;
                text(reason, "Unpick reason")?;
                let i = self.order_index(order_id)?;
                if !["picking", "picked", "packed"].contains(&self.orders[i].status.as_str()) {
                    return Err("Only physically picked, unshipped stock can be returned".into());
                }
                // Recovery is deliberately allowed while an exception blocks forward work.
                // Return only this line's units to the allocation's original bin and lot.
                let order = &mut self.orders[i];
                let allocation = order
                    .allocations
                    .iter_mut()
                    .find(|a| a.line_id == *line_id && a.location == *location && a.lot == *lot)
                    .ok_or("No picked allocation matches this line, location and lot")?;
                if *q > allocation.picked - allocation.shipped {
                    return Err("Return exceeds this allocation's staged quantity".into());
                }
                allocation.picked -= q;
                let sku = allocation.sku.clone();
                let reference = order.reference.clone();
                order.status = if order.allocations.iter().any(|a| a.picked != 0) {
                    "picking"
                } else {
                    "allocated"
                }
                .into();
                let staged = self.bucket(&sku, lot, "STAGE-01");
                staged.on_hand -= q;
                staged.reserved -= q;
                let source = self.bucket(&sku, lot, location);
                source.on_hand += q;
                source.reserved += q;
                self.movement(
                    "unpick",
                    &sku,
                    lot,
                    Some("STAGE-01"),
                    Some(location),
                    *q,
                    &reference,
                    reason,
                    actor,
                );
                Ok(order_id.clone())
            }
            Command::ResetAllocation { order_id, reason } => {
                crate::split_fulfillment::require_legacy(self, order_id)?;
                text(reason, "Allocation reset reason")?;
                let i = self.order_index(order_id)?;
                if !["allocated", "picking"].contains(&self.orders[i].status.as_str()) {
                    return Err("Only allocated work can be reset".into());
                }
                let allocations = self.orders[i].allocations.clone();
                if allocations.iter().any(|a| a.picked != 0 || a.shipped != 0) {
                    return Err("Return every staged unit before resetting allocations".into());
                }
                for a in allocations {
                    self.bucket(&a.sku, &a.lot, &a.location).reserved -= a.quantity - a.released;
                }
                self.orders[i].allocations.clear();
                self.orders[i].status = "pending".into();
                Ok(order_id.clone())
            }
            Command::Pack { order_id } => {
                crate::split_fulfillment::require_legacy(self, order_id)?;
                self.unblocked(order_id)?;
                let i = self.order_index(order_id)?;
                if self.orders[i].status != "picked" {
                    return Err("Every line must be picked before packing".into());
                }
                for allocation in &self.orders[i].allocations {
                    crate::lot_control::require(self, &allocation.sku, &allocation.lot)?;
                }
                self.orders[i].status = "packed".into();
                Ok(order_id.clone())
            }
            Command::Ship {
                order_id,
                carrier,
                handoff_reference,
            } => {
                crate::split_fulfillment::require_legacy(self, order_id)?;
                text(carrier, "Carrier")?;
                text(handoff_reference, "Carrier handoff reference")?;
                self.unblocked(order_id)?;
                let i = self.order_index(order_id)?;
                if self.orders[i].status != "packed" {
                    return Err("Order must be packed before carrier handoff".into());
                }
                for allocation in &self.orders[i].allocations {
                    crate::lot_control::require(self, &allocation.sku, &allocation.lot)?;
                }
                let allocations = self.orders[i].allocations.clone();
                let reference = self.orders[i].reference.clone();
                for a in allocations {
                    let b = self.bucket(&a.sku, &a.lot, "STAGE-01");
                    b.on_hand -= a.quantity;
                    b.reserved -= a.quantity;
                    self.movement(
                        "shipment",
                        &a.sku,
                        &a.lot,
                        Some("STAGE-01"),
                        None,
                        a.quantity,
                        &reference,
                        handoff_reference,
                        actor,
                    );
                }
                let order = &mut self.orders[i];
                for a in &mut order.allocations {
                    a.shipped = a.quantity;
                }
                order.status = "shipped".into();
                order.carrier = carrier.clone();
                order.handoff_reference = handoff_reference.clone();
                order.shipped_at = Some(now());
                Ok(order_id.clone())
            }
            Command::CancelOrder { order_id, reason } => {
                text(reason, "Cancellation reason")?;
                let i = self.order_index(order_id)?;
                if !["pending", "allocated"].contains(&self.orders[i].status.as_str()) {
                    return Err(
                        "Cancellation is allowed only before physical picking starts".into(),
                    );
                }
                let allocations = self.orders[i].allocations.clone();
                for a in allocations {
                    self.bucket(&a.sku, &a.lot, &a.location).reserved -= a.quantity - a.released;
                }
                for a in &mut self.orders[i].allocations {
                    a.released = a.quantity;
                }
                self.orders[i].status = "cancelled".into();
                Ok(order_id.clone())
            }
            Command::ReportException {
                order_id,
                category,
                detail,
            } => {
                text(category, "Exception category")?;
                text(detail, "Exception detail")?;
                if let Some(order) = order_id {
                    let i = self.order_index(order)?;
                    if ["shipped", "cancelled", "closed_partial"]
                        .contains(&self.orders[i].status.as_str())
                    {
                        return Err("Cannot block a completed fulfillment".into());
                    }
                }
                let entity = id();
                self.exceptions.push(Exception {
                    id: entity.clone(),
                    order_id: order_id.clone(),
                    category: category.clone(),
                    detail: detail.clone(),
                    status: "open".into(),
                    resolution: String::new(),
                    actor: actor.into(),
                    resolved_by: None,
                    created_at: now(),
                    resolved_at: None,
                });
                Ok(entity)
            }
            Command::ResolveException {
                exception_id,
                resolution,
            } => {
                text(resolution, "Resolution evidence")?;
                let e = self
                    .exceptions
                    .iter_mut()
                    .find(|e| e.id == *exception_id)
                    .ok_or("Exception not found")?;
                if e.status != "open" {
                    return Err("Exception is already resolved".into());
                }
                e.status = "resolved".into();
                e.resolution = resolution.clone();
                e.resolved_by = Some(actor.into());
                e.resolved_at = Some(now());
                Ok(exception_id.clone())
            }
        }
    }

    /// Rebuild physical and reserved balances from independent recorded evidence.
    pub fn reconciliation(&self) -> Vec<Value> {
        self.balance_proof()
            .into_iter()
            .map(|row| json!(row))
            .collect()
    }

    pub fn into_snapshot(self) -> OperationalSnapshot {
        let rows = self.balance_proof();
        let ok = self.validate_structure().is_ok() && rows.iter().all(|row| row.ok);
        OperationalSnapshot {
            availability_checked_at: None,
            reconciliation: OperationalReconciliation {
                version: self.version,
                ok,
                rows,
            },
            state: self,
        }
    }

    fn balance_proof(&self) -> Vec<BalanceProof> {
        // Borrow identities while accumulating all evidence in one ordered map.
        // The tuple holds actual physical/reserved and evidence physical/reserved.
        type Key<'a> = (&'a str, &'a str, &'a str);
        let mut balances: BTreeMap<Key<'_>, (i64, i64, i64, i64)> = BTreeMap::new();
        for b in &self.reservation_baseline {
            balances.entry((&b.sku, &b.lot, &b.location)).or_default().3 += b.reserved;
        }
        for b in &self.inventory_baseline {
            balances.entry((&b.sku, &b.lot, &b.location)).or_default().2 += b.on_hand;
        }
        for m in &self.movements {
            if let Some(from) = &m.from {
                balances.entry((&m.sku, &m.lot, from)).or_default().2 -= m.quantity;
            }
            if let Some(to) = &m.to {
                balances.entry((&m.sku, &m.lot, to)).or_default().2 += m.quantity;
            }
        }
        for o in &self.orders {
            for a in &o.allocations {
                balances.entry((&a.sku, &a.lot, &a.location)).or_default().3 +=
                    a.quantity - a.picked - a.released;
                balances.entry((&a.sku, &a.lot, "STAGE-01")).or_default().3 += a.picked - a.shipped;
            }
        }
        for s in &self.stocks {
            let actual = balances.entry((&s.sku, &s.lot, &s.location)).or_default();
            actual.0 = s.on_hand;
            actual.1 = s.reserved;
        }
        balances
            .into_iter()
            .map(
                |((sku, lot, location), (on_hand, reserved, ledger, allocation_reserved))| {
                    BalanceProof {
                        sku: sku.into(),
                        lot: lot.into(),
                        location: location.into(),
                        on_hand,
                        reserved,
                        ledger,
                        allocation_reserved,
                        difference: on_hand - ledger,
                        reserved_difference: reserved - allocation_reserved,
                        ok: on_hand == ledger && reserved == allocation_reserved,
                    }
                },
            )
            .collect()
    }
    pub fn validate(&self) -> Result<()> {
        self.validate_structure()?;
        if self.balance_proof().iter().any(|row| !row.ok) {
            return Err("Ledger or reservation reconciliation failed".into());
        }
        Ok(())
    }

    fn validate_structure(&self) -> Result<()> {
        crate::lot_control::validate(self)?;
        crate::units::validate(self)?;
        if self.version < 0 {
            return Err("Invalid warehouse version".into());
        }
        let mut products = BTreeSet::new();
        for p in &self.products {
            text(&p.sku, "SKU")?;
            if !products.insert(p.sku.as_str()) {
                return Err("Duplicate product identity".into());
            }
        }
        let mut locations = BTreeMap::new();
        for l in &self.locations {
            if locations.insert(l.code.as_str(), l).is_some()
                || !["receiving", "storage", "staging"].contains(&l.kind.as_str())
            {
                return Err("Invalid or duplicate location".into());
            }
            if l.kind == "staging" && l.code != "STAGE-01" {
                return Err("Unknown staging location".into());
            }
        }
        let product = |sku: &str| -> Result<()> {
            products
                .contains(sku)
                .then_some(())
                .ok_or_else(|| format!("Unknown SKU: {sku}"))
        };
        let location = |code: &str| -> Result<&Location> {
            locations
                .get(code)
                .copied()
                .ok_or_else(|| format!("Unknown location: {code}"))
        };
        if location("STAGE-01")?.kind != "staging" {
            return Err("Outbound staging location is missing".into());
        }
        let mut baseline_keys = BTreeSet::new();
        for balance in &self.inventory_baseline {
            product(&balance.sku)?;
            location(&balance.location)?;
            if !baseline_keys.insert((&balance.sku, &balance.lot, &balance.location))
                || !(0..=1_000_000_000).contains(&balance.on_hand)
            {
                return Err("Invalid physical inventory baseline".into());
            }
        }
        let mut receipt_refs = BTreeSet::new();
        let mut reservation_keys = BTreeSet::new();
        for balance in &self.reservation_baseline {
            product(&balance.sku)?;
            location(&balance.location)?;
            if !reservation_keys.insert((&balance.sku, &balance.lot, &balance.location))
                || balance.on_hand != 0
                || !(0..=1_000_000_000).contains(&balance.reserved)
            {
                return Err("Invalid other-owner reservation baseline".into());
            }
        }
        let mut receipt_ids = BTreeSet::new();
        let mut received_evidence: BTreeMap<(&str, &str), i64> = BTreeMap::new();
        for (receipt_id, lines) in &self.receipt_baseline {
            let receipt = self
                .receipts
                .iter()
                .find(|receipt| receipt.id == *receipt_id)
                .ok_or("Receipt baseline has no matching receipt")?;
            for (line_id, received) in lines {
                let line = receipt
                    .lines
                    .iter()
                    .find(|line| line.id == *line_id)
                    .ok_or("Receipt baseline has no matching line")?;
                if *received < 0 || *received > line.quantity {
                    return Err("Invalid received quantity baseline".into());
                }
                received_evidence.insert((receipt_id, line_id), *received);
            }
        }
        for movement in self.movements.iter().filter(|m| m.kind == "receipt") {
            let receipt_id = movement
                .receipt_id
                .as_deref()
                .ok_or("Receipt movement requires exact receipt identity")?;
            let line_id = movement
                .receipt_line_id
                .as_deref()
                .ok_or("Receipt movement requires exact line identity")?;
            let receipt = self
                .receipts
                .iter()
                .find(|receipt| receipt.id == receipt_id)
                .ok_or("Receipt movement has no matching receipt")?;
            let line = receipt
                .lines
                .iter()
                .find(|line| line.id == line_id)
                .ok_or("Receipt movement has no matching receipt line")?;
            if receipt.reference != movement.reference
                || line.sku != movement.sku
                || line.lot != movement.lot
                || movement.from.is_some()
                || !movement
                    .to
                    .as_deref()
                    .is_some_and(|to| location(to).is_ok_and(|l| l.kind == "receiving"))
            {
                return Err("Receipt movement disagrees with its exact receipt line".into());
            }
            let total = received_evidence.entry((receipt_id, line_id)).or_default();
            *total = total
                .checked_add(movement.quantity)
                .ok_or("Receipt evidence quantity overflow")?;
        }
        for r in &self.receipts {
            if !receipt_refs.insert(&r.reference)
                || !receipt_ids.insert(&r.id)
                || r.lines.is_empty()
            {
                return Err("Invalid receipt identity".into());
            }
            let mut lines = BTreeSet::new();
            if let Some(source) = &r.return_source {
                text(&source.order_id, "Original shipment")?;
                text(&source.reason, "Return reason")?;
                if source.line_ids.len() != r.lines.len()
                    || r.lines.iter().any(|l| !source.line_ids.contains_key(&l.id))
                {
                    return Err(
                        "Every return line requires exact original order-line ownership".into(),
                    );
                }
            }
            for l in &r.lines {
                product(&l.sku)?;
                quantity(l.quantity)?;
                if !lines.insert(&l.id) || l.received < 0 || l.received > l.quantity {
                    return Err("Receipt quantity invariant violated".into());
                }
                if received_evidence
                    .get(&(r.id.as_str(), l.id.as_str()))
                    .copied()
                    .unwrap_or(0)
                    != l.received
                {
                    return Err(
                        "Receipt line quantity disagrees with received movement evidence".into(),
                    );
                }
            }
            let expected = if r.lines.iter().all(|l| l.received == 0) {
                "expected"
            } else if r.lines.iter().all(|l| l.received == l.quantity) {
                "received"
            } else {
                "partial"
            };
            crate::receipts::validate(r)?;
            if r.cancellation.is_none() && r.status != expected {
                return Err("Receipt state disagrees with received quantities".into());
            }
        }
        let mut movement_ids = BTreeSet::new();
        for m in &self.movements {
            product(&m.sku)?;
            if !movement_ids.insert(&m.id)
                || m.quantity <= 0
                || m.quantity > 1_000_000_000
                || m.from == m.to
            {
                return Err("Invalid physical movement".into());
            }
            if m.kind != "receipt" && (m.receipt_id.is_some() || m.receipt_line_id.is_some()) {
                return Err("Only receipt movements can own a receipt line".into());
            }
            let transfer = ["transfer_dispatch", "transfer_receive"].contains(&m.kind.as_str());
            if transfer != (m.transfer_id.is_some() && m.transfer_line_id.is_some())
                || (!transfer
                    && (m.transfer_id.is_some()
                        || m.transfer_line_id.is_some()
                        || m.transfer_event_id.is_some()))
            {
                return Err("Transfer movements require exact original line ownership".into());
            }
            if let Some(from) = &m.from {
                location(from)?;
            }
            if let Some(to) = &m.to {
                location(to)?;
            }
        }
        let mut keys = BTreeSet::new();
        for s in &self.stocks {
            product(&s.sku)?;
            location(&s.location)?;
            if !keys.insert((&s.sku, &s.lot, &s.location)) {
                return Err("Duplicate stock identity".into());
            }
            if s.reserved < 0 || s.on_hand < s.reserved || s.on_hand > 1_000_000_000 {
                return Err("Inventory balance invariant violated".into());
            }
        }
        let mut order_refs = BTreeSet::new();
        let mut order_ids = BTreeSet::new();
        let mut source_refs = BTreeSet::new();
        for o in &self.orders {
            if !order_refs.insert(&o.reference)
                || !order_ids.insert(&o.id)
                || o.lines.is_empty()
                || ![
                    "pending",
                    "allocated",
                    "picking",
                    "partially_shipped",
                    "closed_partial",
                    "picked",
                    "packed",
                    "shipped",
                    "cancelled",
                ]
                .contains(&o.status.as_str())
            {
                return Err("Invalid order identity or status".into());
            }
            if let Some(source) = &o.source_order_id {
                if !source_refs.insert(source) {
                    return Err("Duplicate OMS source identity".into());
                }
            }
            let mut lines = BTreeSet::new();
            for l in &o.lines {
                product(&l.sku)?;
                quantity(l.quantity)?;
                if !lines.insert(&l.id) {
                    return Err("Duplicate order line identity".into());
                }
            }
            let split = o.shipment_mode.as_deref() == Some("packages");
            crate::order_cancellation::validate(o)?;
            if o.shipment_mode.is_some() && !split {
                return Err("Unknown shipment mode".into());
            }
            if split && o.status != "cancelled" && o.status != crate::split_fulfillment::status(o) {
                return Err("Split fulfillment status disagrees with physical progress".into());
            }
            if !split && o.status == "pending" && !o.allocations.is_empty() {
                return Err("Pending orders cannot have allocations".into());
            }
            if o.status == "allocated" && o.allocations.iter().any(|a| a.picked != 0) {
                return Err("Allocated orders cannot already contain picked stock".into());
            }
            if !split
                && o.status == "picking"
                && (!o.allocations.iter().any(|a| a.picked > 0)
                    || o.allocations.iter().all(|a| a.picked == a.quantity))
            {
                return Err("Picking orders require a physically partial pick".into());
            }
            if !split
                && ["picked", "packed", "shipped"].contains(&o.status.as_str())
                && o.allocations.iter().any(|a| a.picked != a.quantity)
            {
                return Err("Completed picks require every reserved unit".into());
            }
            if !split
                && o.status == "shipped"
                && (o.carrier.is_empty()
                    || o.handoff_reference.is_empty()
                    || o.shipped_at.is_none()
                    || o.allocations.iter().any(|a| a.shipped != a.quantity))
            {
                return Err("Shipment requires physical handoff evidence".into());
            }
            if !split && o.status != "shipped" && o.allocations.iter().any(|a| a.shipped != 0) {
                return Err("Unshipped order has shipped allocations".into());
            }
            if o.status == "cancelled"
                && o.allocations
                    .iter()
                    .any(|a| a.released != a.quantity || a.picked != 0)
            {
                return Err("Cancelled order must release all unpicked stock".into());
            }
            let mut allocation_keys = BTreeSet::new();
            for a in &o.allocations {
                if !allocation_keys.insert((&a.line_id, &a.location, &a.lot)) {
                    return Err("Duplicate allocation ownership identity".into());
                }
                if !split && o.status != "cancelled" && a.released != 0 {
                    return Err("Active allocation cannot contain released reservations".into());
                }
                if location(&a.location)?.kind != "storage" {
                    return Err("Reservations must originate in storage".into());
                }
                if a.quantity <= 0
                    || a.quantity > 1_000_000
                    || a.picked < 0
                    || a.picked > a.quantity
                    || a.shipped < 0
                    || a.released < 0
                    || a.released > a.quantity
                    || a.picked + a.released > a.quantity
                    || a.shipped > a.picked
                {
                    return Err("Allocation quantity invariant violated".into());
                }
                if !o.lines.iter().any(|l| l.id == a.line_id && l.sku == a.sku) {
                    return Err("Allocation must belong to its exact order line".into());
                }
            }
            for l in &o.lines {
                let allocated: i64 = o
                    .allocations
                    .iter()
                    .filter(|a| a.line_id == l.id)
                    .map(|a| a.quantity)
                    .sum();
                if (split
                    && o.allocations
                        .iter()
                        .filter(|a| a.line_id == l.id)
                        .map(|a| a.quantity - a.released)
                        .sum::<i64>()
                        > l.quantity)
                    || (!split
                        && o.status != "pending"
                        && !(o.status == "cancelled" && allocated == 0)
                        && allocated != l.quantity)
                {
                    return Err("Line reservation total must equal ordered quantity".into());
                }
            }
        }
        crate::returns::validate_loaded(self)?;
        Ok(())
    }
}

pub fn demo_state() -> State {
    let mut state = State::empty(Warehouse {
        id: "wh-pdx".into(),
        code: "PDX-01".into(),
        name: "Portland Distribution Center".into(),
        city: "Portland, OR".into(),
        timezone: "America/Los_Angeles".into(),
    });
    let actor = "fixture:synthetic";
    let mut apply = |c: Command| {
        state = state
            .execute(&c, actor)
            .expect("valid synthetic warehouse fixture")
            .0;
    };
    for (sku, name, category, reorder) in [
        ("SS-TEE-001", "Everyday Cotton Tee", "Apparel", 50),
        ("SS-HOOD-002", "Essential Hoodie", "Apparel", 30),
        ("SS-TOTE-003", "Utility Canvas Tote", "Accessories", 25),
        ("SS-CAP-004", "Five Panel Cap", "Accessories", 40),
        ("SS-BTL-005", "Insulated Bottle", "Lifestyle", 20),
        ("SS-SOCK-006", "Crew Socks · 3 Pack", "Apparel", 30),
    ] {
        apply(Command::CreateProduct {
            serial_controlled: false,
            sku: sku.into(),
            name: name.into(),
            category: category.into(),
            reorder_point: reorder,
        });
    }
    for (code, zone) in [
        ("A-01-01", "Apparel"),
        ("A-01-02", "Apparel"),
        ("A-02-01", "Apparel"),
        ("B-01-01", "Accessories"),
        ("B-01-02", "Accessories"),
        ("C-01-01", "Lifestyle"),
    ] {
        apply(Command::CreateLocation {
            code: code.into(),
            zone: zone.into(),
            kind: "storage".into(),
        });
    }
    for (n, (sku, q, bin)) in [
        ("SS-TEE-001", 680, "A-01-01"),
        ("SS-HOOD-002", 240, "A-01-02"),
        ("SS-TOTE-003", 125, "B-01-01"),
        ("SS-CAP-004", 28, "B-01-02"),
        ("SS-BTL-005", 180, "C-01-01"),
        ("SS-SOCK-006", 360, "A-02-01"),
    ]
    .iter()
    .enumerate()
    {
        let c = Command::CreateReceipt {
            reference: format!("RCV-20{}", n + 1),
            supplier: "Northstar Supply Co.".into(),
            lines: vec![ReceiptInput {
                id: "1".into(),
                sku: (*sku).into(),
                lot: "LOT-2610".into(),
                quantity: *q,
            }],
        };
        let (s, r) = state.execute(&c, actor).expect("fixture receipt");
        state = s;
        state = state
            .execute(
                &Command::Receive {
                    receipt_id: r["entity_id"].as_str().expect("receipt ID").into(),
                    line_id: "1".into(),
                    quantity: *q,
                    location: "RECV-01".into(),
                },
                actor,
            )
            .expect("fixture receive")
            .0;
        state = state
            .execute(
                &Command::MoveStock {
                    sku: (*sku).into(),
                    lot: "LOT-2610".into(),
                    from: "RECV-01".into(),
                    to: (*bin).into(),
                    quantity: *q,
                },
                actor,
            )
            .expect("fixture putaway")
            .0;
    }
    for (n, (customer, status, sku, q, priority, city)) in [
        (
            "Alder & Ash",
            "pending",
            "SS-TEE-001",
            24,
            "high",
            "Seattle, WA",
        ),
        (
            "Fieldwork Goods",
            "allocated",
            "SS-HOOD-002",
            12,
            "normal",
            "San Francisco, CA",
        ),
        (
            "The Good Collective",
            "picked",
            "SS-TOTE-003",
            18,
            "urgent",
            "Austin, TX",
        ),
        (
            "Evergreen Market",
            "packed",
            "SS-BTL-005",
            8,
            "normal",
            "Denver, CO",
        ),
        (
            "Sunday Supply",
            "shipped",
            "SS-SOCK-006",
            36,
            "normal",
            "Los Angeles, CA",
        ),
        (
            "Common Ground",
            "pending",
            "SS-CAP-004",
            40,
            "high",
            "Vancouver, BC",
        ),
    ]
    .iter()
    .enumerate()
    {
        let (s, r) = state
            .execute(
                &Command::CreateOrder {
                    reference: format!("SO-104{}", n + 1),
                    source_order_id: Some(format!("oms-demo-{}", n + 1)),
                    customer: (*customer).into(),
                    destination: (*city).into(),
                    priority: (*priority).into(),
                    lines: vec![LineInput {
                        id: "1".into(),
                        sku: (*sku).into(),
                        quantity: *q,
                    }],
                },
                actor,
            )
            .expect("fixture order");
        state = s;
        let oid = r["entity_id"].as_str().expect("order ID").to_string();
        if *status != "pending" {
            state = state
                .execute(
                    &Command::Allocate {
                        order_id: oid.clone(),
                    },
                    actor,
                )
                .expect("fixture allocate")
                .0;
            if *status != "allocated" {
                let a = state
                    .orders
                    .iter()
                    .find(|o| o.id == oid)
                    .expect("fixture order")
                    .allocations[0]
                    .clone();
                state = state
                    .execute(
                        &Command::Pick {
                            order_id: oid.clone(),
                            line_id: "1".into(),
                            location: a.location,
                            lot: a.lot,
                            quantity: *q,
                        },
                        actor,
                    )
                    .expect("fixture pick")
                    .0;
                if *status != "picked" {
                    state = state
                        .execute(
                            &Command::Pack {
                                order_id: oid.clone(),
                            },
                            actor,
                        )
                        .expect("fixture pack")
                        .0;
                }
                if *status == "shipped" {
                    state = state
                        .execute(
                            &Command::Ship {
                                order_id: oid,
                                carrier: "UPS".into(),
                                handoff_reference: "DEMO-HANDOFF-1001".into(),
                            },
                            actor,
                        )
                        .expect("fixture ship")
                        .0;
                }
            }
        }
    }
    state = state
        .execute(
            &Command::CreateReceipt {
                reference: "RCV-207".into(),
                supplier: "Pacific Textile Works".into(),
                lines: vec![ReceiptInput {
                    id: "1".into(),
                    sku: "SS-CAP-004".into(),
                    lot: "LOT-2611".into(),
                    quantity: 200,
                }],
            },
            actor,
        )
        .expect("fixture open receipt")
        .0;
    let oid = state.orders.last().expect("fixture last order").id.clone();
    state = state
        .execute(
            &Command::ReportException {
                order_id: Some(oid),
                category: "Stock shortage".into(),
                detail: "12 caps short. Awaiting receipt RCV-207 before allocation.".into(),
            },
            actor,
        )
        .expect("fixture exception")
        .0;
    state
}
