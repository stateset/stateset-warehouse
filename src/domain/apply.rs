//! The command reducer: how each canonical command changes warehouse state.
use super::*;

impl State {
    pub(super) fn apply(&mut self, command: &Command, actor: &str) -> Result<String> {
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
}
