//! Exact unit identity transitions. This model consumes canonical before/after
//! warehouse states and server-owned custody counts. HTTP/database admission is
//! deliberately separate: a client must never supply authoritative Context.
use crate::domain::{self, Command, State};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

pub const MAX_OBSERVATIONS: usize = 10_000;
pub type Result<T> = domain::Result<T>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Input {
    /// [] for a single physical quantity; [input line] for returns/dispatch;
    /// [order line, original allocation bin, lot] for package packing.
    pub target: Vec<String>,
    pub sku: String,
    pub serials: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum Place {
    OnHand {
        warehouse_id: String,
        location: String,
    },
    Staged {
        warehouse_id: String,
        order_id: String,
        line_id: String,
        allocation_location: String,
        package_id: Option<String>,
    },
    Shipped {
        warehouse_id: String,
        order_id: String,
        line_id: String,
        package_id: Option<String>,
        handoff_event_id: String,
        return_receipt_id: Option<String>,
    },
    InTransit {
        source_warehouse_id: String,
        destination_warehouse_id: String,
        transfer_id: String,
        line_id: String,
    },
    Lost {
        source_warehouse_id: String,
        destination_warehouse_id: String,
        transfer_id: String,
        line_id: String,
        loss_id: String,
    },
    Missing {
        warehouse_id: String,
        location: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Unit {
    pub sku: String,
    pub serial: String,
    pub lot: String,
    pub place: Place,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Change {
    pub sku: String,
    pub serial: String,
    pub lot: String,
    pub target: Vec<String>,
    pub before: Option<Place>,
    pub after: Place,
}

/// warehouse, SKU, lot, physical bin
pub type StockKey = (String, String, String, String);
/// warehouse, order, order line, original allocation bin, SKU, lot
pub type AllocationKey = (String, String, String, String, String, String);
/// warehouse, package, order line, original allocation bin, SKU, lot
pub type PackageKey = (String, String, String, String, String, String);

/// A database adapter must supply complete grouped counts for the canonical
/// stock/orders/package scope, plus requested units and all units needed for
/// derived closures or full bucket counts. Partial unit lookups alone cannot
/// establish a zero balance. These facts are not a request/read-model format.
#[derive(Debug, Clone, Default)]
pub struct Context {
    pub tracked_skus: BTreeSet<String>,
    pub units: Vec<Unit>,
    pub physical: BTreeMap<StockKey, i64>,
    pub staged: BTreeMap<AllocationKey, i64>,
    pub packed: BTreeMap<PackageKey, i64>,
}

pub(crate) fn validate_inputs(inputs: &[Input]) -> Result<()> {
    let context = Context::default();
    Planner::new("serial-admission", &context, inputs).map(|_| ())
}

pub(crate) fn validate_context(state: &State, context: &Context) -> Result<()> {
    check_equations(state, state, context, &[])
}

fn text(s: &str) -> bool {
    !s.is_empty() && s.len() <= 240 && s.trim() == s && !s.chars().any(char::is_control)
}
fn require(ok: bool, message: &str) -> Result<()> {
    if ok {
        Ok(())
    } else {
        Err(message.into())
    }
}
impl Place {
    fn physical(&self, sku: &str, lot: &str) -> Option<StockKey> {
        match self {
            Self::OnHand {
                warehouse_id,
                location,
            } => Some((
                warehouse_id.clone(),
                sku.into(),
                lot.into(),
                location.clone(),
            )),
            Self::Staged { warehouse_id, .. } => Some((
                warehouse_id.clone(),
                sku.into(),
                lot.into(),
                "STAGE-01".into(),
            )),
            _ => None,
        }
    }
    fn staged(&self, sku: &str, lot: &str) -> Option<AllocationKey> {
        if let Self::Staged {
            warehouse_id,
            order_id,
            line_id,
            allocation_location,
            ..
        } = self
        {
            Some((
                warehouse_id.clone(),
                order_id.clone(),
                line_id.clone(),
                allocation_location.clone(),
                sku.into(),
                lot.into(),
            ))
        } else {
            None
        }
    }
    fn packed(&self, sku: &str, lot: &str) -> Option<PackageKey> {
        if let Self::Staged {
            warehouse_id,
            line_id,
            allocation_location,
            package_id: Some(package_id),
            ..
        } = self
        {
            Some((
                warehouse_id.clone(),
                package_id.clone(),
                line_id.clone(),
                allocation_location.clone(),
                sku.into(),
                lot.into(),
            ))
        } else {
            None
        }
    }
    fn valid(&self) -> bool {
        let (fields, optional): (Vec<&str>, Vec<&Option<String>>) = match self {
            Self::OnHand {
                warehouse_id,
                location,
            }
            | Self::Missing {
                warehouse_id,
                location,
            } => (vec![warehouse_id, location], vec![]),
            Self::Staged {
                warehouse_id,
                order_id,
                line_id,
                allocation_location,
                package_id,
            } => (
                vec![warehouse_id, order_id, line_id, allocation_location],
                vec![package_id],
            ),
            Self::Shipped {
                warehouse_id,
                order_id,
                line_id,
                package_id,
                handoff_event_id,
                return_receipt_id,
            } => (
                vec![warehouse_id, order_id, line_id, handoff_event_id],
                vec![package_id, return_receipt_id],
            ),
            Self::InTransit {
                source_warehouse_id,
                destination_warehouse_id,
                transfer_id,
                line_id,
            } => (
                vec![
                    source_warehouse_id,
                    destination_warehouse_id,
                    transfer_id,
                    line_id,
                ],
                vec![],
            ),
            Self::Lost {
                source_warehouse_id,
                destination_warehouse_id,
                transfer_id,
                line_id,
                loss_id,
            } => (
                vec![
                    source_warehouse_id,
                    destination_warehouse_id,
                    transfer_id,
                    line_id,
                    loss_id,
                ],
                vec![],
            ),
        };
        fields.into_iter().all(text)
            && optional.into_iter().all(|v| v.as_deref().is_none_or(text))
            && !matches!(self, Self::OnHand { location, .. } | Self::Missing { location, .. } if location == "STAGE-01")
            && !matches!(self, Self::InTransit { source_warehouse_id, destination_warehouse_id, .. } | Self::Lost { source_warehouse_id, destination_warehouse_id, .. } if source_warehouse_id == destination_warehouse_id)
    }
}

/// Useful for complete in-memory fixtures; the SQL adapter should use grouped
/// counts so an ordinary one-unit move does not load a whole SKU's unit history.
impl Context {
    pub fn from_units(tracked_skus: BTreeSet<String>, units: Vec<Unit>) -> Self {
        let mut context = Self {
            tracked_skus,
            units,
            ..Self::default()
        };
        for unit in &context.units {
            if let Some(key) = unit.place.physical(&unit.sku, &unit.lot) {
                *context.physical.entry(key).or_default() += 1;
            }
            if let Some(key) = unit.place.staged(&unit.sku, &unit.lot) {
                *context.staged.entry(key).or_default() += 1;
            }
            if let Some(key) = unit.place.packed(&unit.sku, &unit.lot) {
                *context.packed.entry(key).or_default() += 1;
            }
        }
        context
    }
}

struct Planner<'a> {
    warehouse: &'a str,
    tracked: &'a BTreeSet<String>,
    units: BTreeMap<(String, String), &'a Unit>,
    inputs: BTreeMap<Vec<String>, &'a Input>,
    changes: BTreeMap<(String, String), Change>,
}
impl<'a> Planner<'a> {
    fn new(warehouse: &'a str, context: &'a Context, inputs: &'a [Input]) -> Result<Self> {
        require(
            text(warehouse) && context.tracked_skus.iter().all(|s| text(s)),
            "Invalid serialized custody scope",
        )?;
        let mut units = BTreeMap::new();
        for unit in &context.units {
            require(
                text(&unit.sku)
                    && text(&unit.serial)
                    && text(&unit.lot)
                    && unit.place.valid()
                    && context.tracked_skus.contains(&unit.sku)
                    && units
                        .insert((unit.sku.clone(), unit.serial.clone()), unit)
                        .is_none(),
                "Serialized custody contains duplicate, untracked or malformed identities",
            )?;
        }
        require(
            inputs.len() <= 100,
            "At most one hundred exact serial targets are allowed",
        )?;
        let mut declared = BTreeMap::new();
        let mut seen = BTreeSet::new();
        let mut count = 0usize;
        for input in inputs {
            count = count
                .checked_add(input.serials.len())
                .ok_or("Too many serial observations")?;
            require(
                count <= MAX_OBSERVATIONS
                    && input.target.len() <= 3
                    && input.target.iter().all(|s| text(s))
                    && text(&input.sku)
                    && declared.insert(input.target.clone(), input).is_none(),
                "Serial observations require bounded unique original targets and SKUs",
            )?;
            for serial in &input.serials {
                require(text(serial) && seen.insert((input.sku.clone(), serial.clone())), "A serial identity cannot be blank, normalized or observed twice in one command")?;
            }
        }
        Ok(Self {
            warehouse,
            tracked: &context.tracked_skus,
            units,
            inputs: declared,
            changes: BTreeMap::new(),
        })
    }
    fn observed(&mut self, target: Vec<String>, sku: &str, quantity: i64) -> Result<Vec<String>> {
        if !self.tracked.contains(sku) {
            return Ok(vec![]);
        }
        let input = self
            .inputs
            .remove(&target)
            .ok_or("Scan exact serials for every controlled quantity target")?;
        require(
            input.sku == sku && i64::try_from(input.serials.len()).ok() == Some(quantity),
            "Observed serial count/SKU differs from its exact original each quantity",
        )?;
        Ok(input.serials.clone())
    }
    fn unit(&self, sku: &str, serial: &str, lot: &str) -> Result<Option<&Unit>> {
        let unit = self.units.get(&(sku.into(), serial.into())).copied();
        require(
            unit.is_none_or(|unit| unit.lot == lot),
            "A serial identity cannot change its original lot",
        )?;
        Ok(unit)
    }
    fn change(
        &mut self,
        sku: &str,
        serial: &str,
        lot: &str,
        target: Vec<String>,
        after: Place,
    ) -> Result<()> {
        let before = self.unit(sku, serial, lot)?.map(|u| u.place.clone());
        if before.as_ref() == Some(&after) {
            return Ok(());
        }
        require(after.valid(), "Invalid serialized custody destination")?;
        let key = (sku.into(), serial.into());
        require(!self.changes.contains_key(&key) && self.changes.len() < MAX_OBSERVATIONS, "One command cannot reinterpret a serial twice or exceed its identity limit. No changes were saved; large full counts require a reviewed count session")?;
        self.changes.insert(
            key,
            Change {
                sku: sku.into(),
                serial: serial.into(),
                lot: lot.into(),
                target,
                before,
                after,
            },
        );
        Ok(())
    }
    fn selected(&self, predicate: impl Fn(&Unit) -> bool) -> Vec<Unit> {
        self.units
            .values()
            .filter(|u| predicate(u))
            .map(|u| (*u).clone())
            .collect()
    }
    fn at_bin(&self, unit: Option<&Unit>, location: &str) -> bool {
        unit.is_some_and(|u| matches!(&u.place, Place::OnHand { warehouse_id, location: bin } if warehouse_id == self.warehouse && bin == location))
    }
}

fn order_sku<'a>(state: &'a State, order: &str, line: &str) -> Result<&'a str> {
    state
        .orders
        .iter()
        .find(|o| o.id == order)
        .and_then(|o| o.lines.iter().find(|l| l.id == line))
        .map(|l| l.sku.as_str())
        .ok_or_else(|| "Original serial order line is unavailable".into())
}

/// Produce an all-or-nothing serial change set for an already valid canonical
/// transition. It must still be committed with the ordinary event, receipt,
/// stock, ownership and outbox, under database serial/source constraints.
pub fn plan(
    before: &State,
    after: &State,
    command: &Command,
    inputs: &[Input],
    context: &Context,
    entity_id: &str,
    event_id: &str,
) -> Result<Vec<Change>> {
    require(
        before.warehouse.id == after.warehouse.id && !entity_id.is_empty() && text(event_id),
        "Serial transition belongs to a different warehouse or original event",
    )?;
    let w = before.warehouse.id.as_str();
    let mut p = Planner::new(w, context, inputs)?;
    match command {
        Command::CountStock {
            sku,
            lot,
            location,
            counted,
            ..
        } => {
            if context.tracked_skus.contains(sku) {
                let observed = p.observed(vec![], sku, *counted)?;
                let present: BTreeSet<_> = observed.iter().collect();
                let old =
                    p.selected(|u| u.sku == *sku && u.lot == *lot && p.at_bin(Some(u), location));
                for unit in old {
                    if !present.contains(&unit.serial) {
                        p.change(
                            sku,
                            &unit.serial,
                            lot,
                            vec![],
                            Place::Missing {
                                warehouse_id: w.into(),
                                location: location.clone(),
                            },
                        )?;
                    }
                }
                for serial in observed {
                    let old = p.unit(sku, &serial, lot)?;
                    require(old.is_none() || p.at_bin(old, location)
                        || old.is_some_and(|u| matches!(&u.place, Place::Missing { warehouse_id, .. } if warehouse_id == w)), "Count cannot teleport, return, settle transit or release another unit's custody")?;
                    p.change(
                        sku,
                        &serial,
                        lot,
                        vec![],
                        Place::OnHand {
                            warehouse_id: w.into(),
                            location: location.clone(),
                        },
                    )?;
                }
            }
        }
        Command::Receive {
            receipt_id,
            line_id,
            quantity,
            location,
        } => {
            let receipt = before
                .receipts
                .iter()
                .find(|r| r.id == *receipt_id)
                .ok_or("Original serial receipt unavailable")?;
            let line = receipt
                .lines
                .iter()
                .find(|l| l.id == *line_id)
                .ok_or("Original serial receipt line unavailable")?;
            for serial in p.observed(vec![], &line.sku, *quantity)? {
                let old = p.unit(&line.sku, &serial, &line.lot)?;
                if let Some(source) = &receipt.return_source {
                    let original_line = source
                        .line_ids
                        .get(line_id)
                        .ok_or("Original returned serial line unavailable")?;
                    require(old.is_some_and(|u| matches!(&u.place, Place::Shipped { warehouse_id, order_id, line_id, return_receipt_id: Some(rma), .. } if warehouse_id == w && order_id == &source.order_id && line_id == original_line && rma == receipt_id)), "Receive only the exact serial authorized from this original handoff and return")?;
                } else {
                    require(
                        old.is_none(),
                        "Supplier receiving cannot register an existing serial identity again",
                    )?;
                }
                p.change(
                    &line.sku,
                    &serial,
                    &line.lot,
                    vec![],
                    Place::OnHand {
                        warehouse_id: w.into(),
                        location: location.clone(),
                    },
                )?;
            }
        }
        Command::MoveStock {
            sku,
            lot,
            from,
            to,
            quantity,
        } => {
            for serial in p.observed(vec![], sku, *quantity)? {
                require(
                    p.at_bin(p.unit(sku, &serial, lot)?, from),
                    "Move only an observed serial in the exact source bin/lot",
                )?;
                p.change(
                    sku,
                    &serial,
                    lot,
                    vec![],
                    Place::OnHand {
                        warehouse_id: w.into(),
                        location: to.clone(),
                    },
                )?;
            }
        }
        Command::Pick {
            order_id,
            line_id,
            location,
            lot,
            quantity,
        }
        | Command::Unpick {
            order_id,
            line_id,
            location,
            lot,
            quantity,
            ..
        } => {
            let sku = order_sku(before, order_id, line_id)?;
            for serial in p.observed(vec![], sku, *quantity)? {
                let old = p.unit(sku, &serial, lot)?;
                let next = if matches!(command, Command::Pick { .. }) {
                    require(
                        p.at_bin(old, location),
                        "Pick the exact observed serial from its original bin/lot",
                    )?;
                    Place::Staged {
                        warehouse_id: w.into(),
                        order_id: order_id.clone(),
                        line_id: line_id.clone(),
                        allocation_location: location.clone(),
                        package_id: None,
                    }
                } else {
                    require(old.is_some_and(|u| matches!(&u.place, Place::Staged { warehouse_id, order_id: owner, line_id: line, allocation_location, package_id: None } if warehouse_id == w && owner == order_id && line == line_id && allocation_location == location)), "Unpick only this original line's unpacked staged serials")?;
                    Place::OnHand {
                        warehouse_id: w.into(),
                        location: location.clone(),
                    }
                };
                p.change(sku, &serial, lot, vec![], next)?;
            }
        }
        Command::CreateReturn {
            order_id, lines, ..
        } => {
            for line in lines {
                let sku = order_sku(before, order_id, &line.order_line_id)?;
                let target = vec![line.id.clone()];
                for serial in p.observed(target.clone(), sku, line.quantity)? {
                    let mut next = p
                        .unit(sku, &serial, &line.lot)?
                        .ok_or("Return serial was never handed off")?
                        .place
                        .clone();
                    if let Place::Shipped {
                        warehouse_id,
                        order_id: owner,
                        line_id: original_line,
                        return_receipt_id,
                        ..
                    } = &mut next
                    {
                        require(warehouse_id == w && owner == order_id && original_line == &line.order_line_id && return_receipt_id.is_none(), "Return authorization must retain its original shipped serial/line and cannot spend it twice")?;
                        *return_receipt_id = Some(entity_id.into());
                    } else {
                        return Err(
                            "Authorize only an actually shipped serial, before any physical return"
                                .into(),
                        );
                    }
                    p.change(sku, &serial, &line.lot, target.clone(), next)?;
                }
            }
        }
        Command::CancelReceipt { receipt_id, .. } => {
            let units = p.selected(|u| matches!(&u.place, Place::Shipped { warehouse_id, return_receipt_id: Some(rma), .. } if warehouse_id == w && rma == receipt_id));
            for unit in units {
                let mut next = unit.place.clone();
                if let Place::Shipped {
                    return_receipt_id, ..
                } = &mut next
                {
                    *return_receipt_id = None;
                }
                p.change(&unit.sku, &unit.serial, &unit.lot, vec![], next)?;
            }
        }
        Command::PackShipment {
            order_id, lines, ..
        } => {
            for line in lines {
                let sku = order_sku(before, order_id, &line.line_id)?;
                let target = vec![
                    line.line_id.clone(),
                    line.location.clone(),
                    line.lot.clone(),
                ];
                for serial in p.observed(target.clone(), sku, line.quantity)? {
                    let mut next = p
                        .unit(sku, &serial, &line.lot)?
                        .ok_or("Package serial is not staged")?
                        .place
                        .clone();
                    if let Place::Staged {
                        warehouse_id,
                        order_id: owner,
                        line_id: original_line,
                        allocation_location,
                        package_id,
                    } = &mut next
                    {
                        require(
                            warehouse_id == w
                                && owner == order_id
                                && original_line == &line.line_id
                                && allocation_location == &line.location
                                && package_id.is_none(),
                            "Pack only the exact original staged serial/line/bin/lot, once",
                        )?;
                        *package_id = Some(entity_id.into());
                    } else {
                        return Err("Package serial is not staged".into());
                    }
                    p.change(sku, &serial, &line.lot, target.clone(), next)?;
                }
            }
        }
        Command::UnpackShipment { shipment_id, .. }
        | Command::HandoffShipment { shipment_id, .. } => {
            let units = p.selected(|u| matches!(&u.place, Place::Staged { warehouse_id, package_id: Some(package), .. } if warehouse_id == w && package == shipment_id));
            for unit in units {
                let next = if let Place::Staged {
                    warehouse_id,
                    order_id,
                    line_id,
                    allocation_location,
                    ..
                } = unit.place
                {
                    if matches!(command, Command::UnpackShipment { .. }) {
                        Place::Staged {
                            warehouse_id,
                            order_id,
                            line_id,
                            allocation_location,
                            package_id: None,
                        }
                    } else {
                        Place::Shipped {
                            warehouse_id,
                            order_id,
                            line_id,
                            package_id: Some(shipment_id.clone()),
                            handoff_event_id: event_id.into(),
                            return_receipt_id: None,
                        }
                    }
                } else {
                    unreachable!("selection only returns staged package units")
                };
                p.change(&unit.sku, &unit.serial, &unit.lot, vec![], next)?;
            }
        }
        Command::Ship { order_id, .. } => {
            let units = p.selected(|u| matches!(&u.place, Place::Staged { warehouse_id, order_id: owner, package_id: None, .. } if warehouse_id == w && owner == order_id));
            for unit in units {
                let next = if let Place::Staged {
                    warehouse_id,
                    order_id,
                    line_id,
                    ..
                } = unit.place
                {
                    Place::Shipped {
                        warehouse_id,
                        order_id,
                        line_id,
                        package_id: None,
                        handoff_event_id: event_id.into(),
                        return_receipt_id: None,
                    }
                } else {
                    unreachable!("selection only returns staged order units")
                };
                p.change(&unit.sku, &unit.serial, &unit.lot, vec![], next)?;
            }
        }
        Command::DispatchTransfer {
            destination_warehouse_id,
            lines,
            ..
        } => {
            for line in lines {
                let target = vec![line.id.clone()];
                for serial in p.observed(target.clone(), &line.sku, line.quantity)? {
                    require(
                        p.at_bin(p.unit(&line.sku, &serial, &line.lot)?, &line.location),
                        "Dispatch only this original line's observed serials in source custody",
                    )?;
                    p.change(
                        &line.sku,
                        &serial,
                        &line.lot,
                        target.clone(),
                        Place::InTransit {
                            source_warehouse_id: w.into(),
                            destination_warehouse_id: destination_warehouse_id.clone(),
                            transfer_id: entity_id.into(),
                            line_id: line.id.clone(),
                        },
                    )?;
                }
            }
        }
        Command::ReceiveTransfer {
            transfer_id,
            line_id,
            quantity,
            ..
        }
        | Command::RecordTransferLoss {
            transfer_id,
            line_id,
            quantity,
            ..
        } => {
            let t = before
                .transfer_context
                .as_ref()
                .filter(|t| t.ok && t.id == *transfer_id && t.destination_warehouse_id == w)
                .ok_or("Original destination serial transfer unavailable")?;
            let line = t
                .lines
                .iter()
                .find(|l| l.id == *line_id)
                .ok_or("Original serial transfer line unavailable")?;
            for serial in p.observed(vec![], &line.sku, *quantity)? {
                require(p.unit(&line.sku, &serial, &line.lot)?.is_some_and(|u| matches!(&u.place, Place::InTransit { source_warehouse_id, destination_warehouse_id, transfer_id: original, line_id: original_line } if source_warehouse_id == &t.source_warehouse_id && destination_warehouse_id == w && original == transfer_id && original_line == line_id)), "Arrival/loss must identify this exact original in-transit serial and destination")?;
                let next = if let Command::ReceiveTransfer { location, .. } = command {
                    Place::OnHand {
                        warehouse_id: w.into(),
                        location: location.clone(),
                    }
                } else {
                    Place::Lost {
                        source_warehouse_id: t.source_warehouse_id.clone(),
                        destination_warehouse_id: w.into(),
                        transfer_id: transfer_id.clone(),
                        line_id: line_id.clone(),
                        loss_id: entity_id.into(),
                    }
                };
                p.change(&line.sku, &serial, &line.lot, vec![], next)?;
            }
        }
        Command::ReverseTransferLoss {
            loss_id, quantity, ..
        } => {
            let t = before
                .transfer_context
                .as_ref()
                .filter(|t| t.ok && t.destination_warehouse_id == w)
                .ok_or("Original destination serial loss unavailable")?;
            let loss = t
                .reviewed_loss
                .as_ref()
                .filter(|l| l.id == *loss_id)
                .ok_or("Original serial loss unavailable")?;
            let line = t
                .lines
                .iter()
                .find(|l| l.id == loss.line_id)
                .ok_or("Original serial loss line unavailable")?;
            for serial in p.observed(vec![], &line.sku, *quantity)? {
                require(p.unit(&line.sku, &serial, &line.lot)?.is_some_and(|u| matches!(&u.place, Place::Lost { source_warehouse_id, destination_warehouse_id, transfer_id, line_id, loss_id: original } if source_warehouse_id == &t.source_warehouse_id && destination_warehouse_id == w && transfer_id == &t.id && line_id == &line.id && original == loss_id)), "Reverse only a serial from this exact original unreversed loss")?;
                p.change(
                    &line.sku,
                    &serial,
                    &line.lot,
                    vec![],
                    Place::InTransit {
                        source_warehouse_id: t.source_warehouse_id.clone(),
                        destination_warehouse_id: w.into(),
                        transfer_id: t.id.clone(),
                        line_id: line.id.clone(),
                    },
                )?;
            }
        }
        _ => {}
    }
    require(
        p.inputs.is_empty(),
        "Serial observations do not belong to this exact controlled command target",
    )?;
    let changes: Vec<_> = p.changes.into_values().collect();
    check_equations(before, after, context, &changes)?;
    Ok(changes)
}

mod equations;
#[cfg(test)]
mod tests;

use equations::check_equations;
