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
}

mod apply;
mod demo;
mod validate;
pub use demo::demo_state;
