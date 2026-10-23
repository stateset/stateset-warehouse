//! Independent balance reconciliation and structural validation of state.
use super::*;

impl State {
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
