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
    pub sku: String,
    pub name: String,
    pub category: String,
    pub reorder_point: i64,
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
    pub warehouse: Warehouse,
    pub version: i64,
    pub products: Vec<Product>,
    pub locations: Vec<Location>,
    pub stocks: Vec<Stock>,
    /// Physical opening balances when persistence loads only new movement deltas.
    /// Reserved balances are always rebuilt from current allocation ownership.
    #[serde(default)]
    pub inventory_baseline: Vec<Stock>,
    /// Opening received quantities when only new receipt movements are loaded.
    #[serde(default)]
    pub receipt_baseline: BTreeMap<String, BTreeMap<String, i64>>,
    pub receipts: Vec<Receipt>,
    pub orders: Vec<Order>,
    pub movements: Vec<Movement>,
    pub exceptions: Vec<Exception>,
    pub activity: Vec<Activity>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LineInput {
    pub id: String,
    pub sku: String,
    pub quantity: i64,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReceiptInput {
    pub id: String,
    pub sku: String,
    pub lot: String,
    pub quantity: i64,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Command {
    UpdateWarehouse {
        code: String,
        name: String,
        city: String,
        timezone: String,
    },
    CreateProduct {
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
            Self::UpdateWarehouse { .. } => "warehouse.updated",
            Self::CreateProduct { .. } => "product.created",
            Self::CreateLocation { .. } => "location.created",
            Self::CreateReceipt { .. } => "receipt.created",
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
                Self::UpdateWarehouse { .. }
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
            warehouse,
            version: 0,
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
            receipt_baseline: BTreeMap::new(),
            receipts: vec![],
            orders: vec![],
            movements: vec![],
            exceptions: vec![],
            activity: vec![],
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
    fn order_index(&self, id: &str) -> Result<usize> {
        self.orders
            .iter()
            .position(|o| o.id == id)
            .ok_or_else(|| "Fulfillment order not found".into())
    }
    fn unblocked(&self, order: &str) -> Result<()> {
        if self
            .exceptions
            .iter()
            .any(|e| e.status == "open" && e.order_id.as_deref() == Some(order))
        {
            return Err("Resolve the blocking order exception before continuing".into());
        }
        Ok(())
    }
    fn bucket(&mut self, sku: &str, lot: &str, location: &str) -> &mut Stock {
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
    fn movement(
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
            reason: reason.into(),
            actor: actor.into(),
            at: now(),
        });
    }
    pub fn execute(&self, command: &Command, actor: &str) -> Result<(Self, Value)> {
        let mut next = self.clone();
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
                Command::Unpick { reason, .. }
                | Command::ResetAllocation { reason, .. }
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
    fn apply(&mut self, command: &Command, actor: &str) -> Result<String> {
        match command {
            Command::UpdateWarehouse {
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
                });
                Ok(entity)
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
                        })
                        .map(|(j, s)| (j, s.location.clone(), s.lot.clone()))
                        .collect();
                    bins.sort_by(|a, b| (&a.1, &a.2).cmp(&(&b.1, &b.2)));
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
                quantity(*q)?;
                self.unblocked(order_id)?;
                let i = self.order_index(order_id)?;
                if !["allocated", "picking"].contains(&self.orders[i].status.as_str()) {
                    return Err("Order is not ready to pick".into());
                }
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
                self.unblocked(order_id)?;
                let i = self.order_index(order_id)?;
                if self.orders[i].status != "picked" {
                    return Err("Every line must be picked before packing".into());
                }
                self.orders[i].status = "packed".into();
                Ok(order_id.clone())
            }
            Command::Ship {
                order_id,
                carrier,
                handoff_reference,
            } => {
                text(carrier, "Carrier")?;
                text(handoff_reference, "Carrier handoff reference")?;
                self.unblocked(order_id)?;
                let i = self.order_index(order_id)?;
                if self.orders[i].status != "packed" {
                    return Err("Order must be packed before carrier handoff".into());
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
                    self.bucket(&a.sku, &a.lot, &a.location).reserved -= a.quantity;
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
                    if ["shipped", "cancelled"].contains(&self.orders[i].status.as_str()) {
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
        type Key = (String, String, String);
        let mut physical: BTreeMap<Key, i64> = BTreeMap::new();
        let mut reserved: BTreeMap<Key, i64> = BTreeMap::new();
        for balance in &self.inventory_baseline {
            *physical
                .entry((
                    balance.sku.clone(),
                    balance.lot.clone(),
                    balance.location.clone(),
                ))
                .or_default() += balance.on_hand;
        }
        for m in &self.movements {
            if let Some(from) = &m.from {
                *physical
                    .entry((m.sku.clone(), m.lot.clone(), from.clone()))
                    .or_default() -= m.quantity;
            }
            if let Some(to) = &m.to {
                *physical
                    .entry((m.sku.clone(), m.lot.clone(), to.clone()))
                    .or_default() += m.quantity;
            }
        }
        for o in &self.orders {
            for a in &o.allocations {
                *reserved
                    .entry((a.sku.clone(), a.lot.clone(), a.location.clone()))
                    .or_default() += a.quantity - a.picked - a.released;
                *reserved
                    .entry((a.sku.clone(), a.lot.clone(), "STAGE-01".into()))
                    .or_default() += a.picked - a.shipped;
            }
        }
        let mut actual: BTreeMap<Key, (i64, i64)> = BTreeMap::new();
        for s in &self.stocks {
            actual.insert(
                (s.sku.clone(), s.lot.clone(), s.location.clone()),
                (s.on_hand, s.reserved),
            );
        }
        let keys: BTreeSet<_> = physical
            .keys()
            .chain(reserved.keys())
            .chain(actual.keys())
            .cloned()
            .collect();
        keys.into_iter().map(|key| {
            let (on_hand,held)=actual.get(&key).copied().unwrap_or_default();
            let ledger=physical.get(&key).copied().unwrap_or_default();
            let expected=reserved.get(&key).copied().unwrap_or_default();
            json!({"sku":key.0,"lot":key.1,"location":key.2,"on_hand":on_hand,"ledger":ledger,"reserved":held,"allocation_reserved":expected,"difference":on_hand-ledger,"reserved_difference":held-expected,"ok":on_hand==ledger && held==expected})
        }).collect()
    }
    pub fn validate(&self) -> Result<()> {
        if self.version < 0 {
            return Err("Invalid warehouse version".into());
        }
        let mut products = BTreeSet::new();
        for p in &self.products {
            text(&p.sku, "SKU")?;
            if !products.insert(&p.sku) {
                return Err("Duplicate product identity".into());
            }
        }
        let mut locations = BTreeSet::new();
        for l in &self.locations {
            if !locations.insert(&l.code)
                || !["receiving", "storage", "staging"].contains(&l.kind.as_str())
            {
                return Err("Invalid or duplicate location".into());
            }
            if l.kind == "staging" && l.code != "STAGE-01" {
                return Err("Unknown staging location".into());
            }
        }
        if self.location("STAGE-01")?.kind != "staging" {
            return Err("Outbound staging location is missing".into());
        }
        let mut baseline_keys = BTreeSet::new();
        for balance in &self.inventory_baseline {
            self.product(&balance.sku)?;
            self.location(&balance.location)?;
            if !baseline_keys.insert((&balance.sku, &balance.lot, &balance.location))
                || !(0..=1_000_000_000).contains(&balance.on_hand)
            {
                return Err("Invalid physical inventory baseline".into());
            }
        }
        let mut receipt_refs = BTreeSet::new();
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
                    .is_some_and(|to| self.location(to).is_ok_and(|l| l.kind == "receiving"))
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
            for l in &r.lines {
                self.product(&l.sku)?;
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
            if r.status != expected {
                return Err("Receipt state disagrees with received quantities".into());
            }
        }
        let mut movement_ids = BTreeSet::new();
        for m in &self.movements {
            self.product(&m.sku)?;
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
            if let Some(from) = &m.from {
                self.location(from)?;
            }
            if let Some(to) = &m.to {
                self.location(to)?;
            }
        }
        let mut keys = BTreeSet::new();
        for s in &self.stocks {
            self.product(&s.sku)?;
            self.location(&s.location)?;
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
                self.product(&l.sku)?;
                quantity(l.quantity)?;
                if !lines.insert(&l.id) {
                    return Err("Duplicate order line identity".into());
                }
            }
            if o.status == "pending" && !o.allocations.is_empty() {
                return Err("Pending orders cannot have allocations".into());
            }
            if o.status == "allocated" && o.allocations.iter().any(|a| a.picked != 0) {
                return Err("Allocated orders cannot already contain picked stock".into());
            }
            if o.status == "picking"
                && (!o.allocations.iter().any(|a| a.picked > 0)
                    || o.allocations.iter().all(|a| a.picked == a.quantity))
            {
                return Err("Picking orders require a physically partial pick".into());
            }
            if ["picked", "packed", "shipped"].contains(&o.status.as_str())
                && o.allocations.iter().any(|a| a.picked != a.quantity)
            {
                return Err("Completed picks require every reserved unit".into());
            }
            if o.status == "shipped"
                && (o.carrier.is_empty()
                    || o.handoff_reference.is_empty()
                    || o.shipped_at.is_none()
                    || o.allocations.iter().any(|a| a.shipped != a.quantity))
            {
                return Err("Shipment requires physical handoff evidence".into());
            }
            if o.status != "shipped" && o.allocations.iter().any(|a| a.shipped != 0) {
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
                if o.status != "cancelled" && a.released != 0 {
                    return Err("Active allocation cannot contain released reservations".into());
                }
                if self.location(&a.location)?.kind != "storage" {
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
                if o.status != "pending"
                    && !(o.status == "cancelled" && allocated == 0)
                    && allocated != l.quantity
                {
                    return Err("Line reservation total must equal ordered quantity".into());
                }
            }
        }
        if self.reconciliation().iter().any(|r| r["ok"] != true) {
            return Err("Ledger or reservation reconciliation failed".into());
        }
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
