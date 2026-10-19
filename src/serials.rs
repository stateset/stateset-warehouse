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

fn add<K: Ord>(map: &mut BTreeMap<K, i64>, key: K, quantity: i64) -> Result<()> {
    let value = map.entry(key).or_default();
    *value = value
        .checked_add(quantity)
        .ok_or("Serial custody count exceeds the exact range")?;
    Ok(())
}
fn stock(state: &State, tracked: &BTreeSet<String>) -> BTreeMap<StockKey, i64> {
    state
        .stocks
        .iter()
        .filter(|s| tracked.contains(&s.sku))
        .map(|s| {
            (
                (
                    state.warehouse.id.clone(),
                    s.sku.clone(),
                    s.lot.clone(),
                    s.location.clone(),
                ),
                s.on_hand,
            )
        })
        .collect()
}
fn staged(state: &State, tracked: &BTreeSet<String>) -> Result<BTreeMap<AllocationKey, i64>> {
    let mut counts = BTreeMap::new();
    for order in &state.orders {
        for a in &order.allocations {
            if tracked.contains(&a.sku) {
                add(
                    &mut counts,
                    (
                        state.warehouse.id.clone(),
                        order.id.clone(),
                        a.line_id.clone(),
                        a.location.clone(),
                        a.sku.clone(),
                        a.lot.clone(),
                    ),
                    a.picked - a.shipped,
                )?;
            }
        }
    }
    Ok(counts)
}
fn packed(state: &State, tracked: &BTreeSet<String>) -> Result<BTreeMap<PackageKey, i64>> {
    // The canonical transition already updates its loaded package context.
    // shipment_change is the persistence delta, not another business effect.
    let packages = state
        .shipment_context
        .as_ref()
        .map(|c| c.packages.as_slice())
        .unwrap_or_default();
    let mut counts = BTreeMap::new();
    for package in packages.iter().filter(|p| p.closure.is_none()) {
        for line in &package.lines {
            if tracked.contains(&line.sku) {
                add(
                    &mut counts,
                    (
                        state.warehouse.id.clone(),
                        package.id.clone(),
                        line.line_id.clone(),
                        line.location.clone(),
                        line.sku.clone(),
                        line.lot.clone(),
                    ),
                    line.quantity,
                )?;
            }
        }
    }
    Ok(counts)
}
fn equal_counts<K: Ord>(left: &BTreeMap<K, i64>, right: &BTreeMap<K, i64>) -> bool {
    left.iter()
        .all(|(key, value)| *value >= 0 && right.get(key).copied().unwrap_or(0) == *value)
        && right
            .iter()
            .all(|(key, value)| *value >= 0 && left.get(key).copied().unwrap_or(0) == *value)
}
fn check_equations(
    before: &State,
    after: &State,
    context: &Context,
    changes: &[Change],
) -> Result<()> {
    let w = before.warehouse.id.as_str();
    let orders: BTreeSet<_> = before
        .orders
        .iter()
        .chain(&after.orders)
        .map(|o| o.id.as_str())
        .collect();
    let package_scope: BTreeSet<_> = before
        .shipment_context
        .iter()
        .flat_map(|c| c.packages.iter())
        .chain(
            after
                .shipment_context
                .iter()
                .flat_map(|c| c.packages.iter()),
        )
        .map(|p| p.id.as_str())
        .collect();
    let mut physical: BTreeMap<_, _> = context
        .physical
        .iter()
        .filter(|(k, _)| k.0 == w && context.tracked_skus.contains(&k.1))
        .map(|(k, v)| (k.clone(), *v))
        .collect();
    let mut ownership: BTreeMap<_, _> = context
        .staged
        .iter()
        .filter(|(k, _)| k.0 == w && orders.contains(k.1.as_str()))
        .map(|(k, v)| (k.clone(), *v))
        .collect();
    let mut packing: BTreeMap<_, _> = context
        .packed
        .iter()
        .filter(|(k, _)| k.0 == w && package_scope.contains(k.1.as_str()))
        .map(|(k, v)| (k.clone(), *v))
        .collect();
    require(
        equal_counts(&physical, &stock(before, &context.tracked_skus)),
        "Current serial custody disagrees with original physical stock",
    )?;
    require(
        equal_counts(&ownership, &staged(before, &context.tracked_skus)?),
        "Current staged serials disagree with exact original allocation owners",
    )?;
    require(
        equal_counts(&packing, &packed(before, &context.tracked_skus)?),
        "Current serial package claims disagree with original active packing",
    )?;
    for change in changes {
        for (place, delta) in [(change.before.as_ref(), -1), (Some(&change.after), 1)] {
            let Some(place) = place else { continue };
            if let Some(key) = place
                .physical(&change.sku, &change.lot)
                .filter(|k| k.0 == w)
            {
                add(&mut physical, key, delta)?;
            }
            if let Some(key) = place.staged(&change.sku, &change.lot).filter(|k| k.0 == w) {
                add(&mut ownership, key, delta)?;
            }
            if let Some(key) = place.packed(&change.sku, &change.lot).filter(|k| k.0 == w) {
                add(&mut packing, key, delta)?;
            }
        }
    }
    require(
        equal_counts(&physical, &stock(after, &context.tracked_skus)),
        "Observed serial changes do not equal the canonical physical stock transition",
    )?;
    require(
        equal_counts(&ownership, &staged(after, &context.tracked_skus)?),
        "Observed serial changes do not equal exact staged line/bin ownership",
    )?;
    require(
        equal_counts(&packing, &packed(after, &context.tracked_skus)?),
        "Observed serial changes do not equal original package ownership",
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        domain::{LineInput, ReceiptInput, Warehouse},
        split_fulfillment::{self, Package},
    };
    use proptest::prelude::*;

    struct Trial {
        state: State,
        units: Vec<Unit>,
        packages: Vec<Package>,
        events: usize,
    }
    impl Trial {
        fn new() -> Self {
            let mut t = Self {
                state: State::empty(Warehouse {
                    id: "main".into(),
                    code: "MAIN".into(),
                    name: "Synthetic".into(),
                    city: "Synthetic".into(),
                    timezone: "UTC".into(),
                }),
                units: vec![],
                packages: vec![],
                events: 0,
            };
            t.apply(
                Command::CreateProduct {
                    serial_controlled: false,
                    sku: "SKU".into(),
                    name: "Synthetic serialized item".into(),
                    category: "Test".into(),
                    reorder_point: 0,
                },
                vec![],
            )
            .expect("valid synthetic serial fixture");
            for code in ["A", "B"] {
                t.apply(
                    Command::CreateLocation {
                        code: code.into(),
                        zone: "Test".into(),
                        kind: "storage".into(),
                    },
                    vec![],
                )
                .expect("valid synthetic serial fixture");
            }
            t
        }
        fn context(&self) -> Context {
            Context::from_units(BTreeSet::from(["SKU".into()]), self.units.clone())
        }
        fn prepare(&mut self, c: &Command) {
            let order_id = match c {
                Command::PackShipment { order_id, .. }
                | Command::AllocateAvailable { order_id }
                | Command::Pick { order_id, .. }
                | Command::Unpick { order_id, .. }
                | Command::ReleaseUnpicked { order_id, .. }
                | Command::CancelRemainder { order_id, .. } => Some(order_id.clone()),
                Command::UnpackShipment { shipment_id, .. }
                | Command::HandoffShipment { shipment_id, .. } => self
                    .packages
                    .iter()
                    .find(|p| p.id == *shipment_id)
                    .map(|p| p.order_id.clone()),
                _ => None,
            };
            self.state.shipment_context = order_id.map(|id| split_fulfillment::Context {
                order_id: id.clone(),
                packages: self
                    .packages
                    .iter()
                    .filter(|p| p.order_id == id)
                    .cloned()
                    .collect(),
                ok: true,
            });
            self.state.shipment_change = None;
        }
        fn apply(&mut self, c: Command, inputs: Vec<Input>) -> Result<String> {
            self.prepare(&c);
            let context = self.context();
            let (next, result) = self.state.execute(&c, "synthetic:operator")?;
            let id = result["entity_id"]
                .as_str()
                .expect("valid synthetic serial fixture")
                .to_string();
            let changes = plan(
                &self.state,
                &next,
                &c,
                &inputs,
                &context,
                &id,
                &format!("event-{}", self.events),
            )?;
            for change in changes {
                if let Some(unit) = self
                    .units
                    .iter_mut()
                    .find(|u| u.sku == change.sku && u.serial == change.serial)
                {
                    unit.place = change.after;
                } else {
                    self.units.push(Unit {
                        sku: change.sku,
                        serial: change.serial,
                        lot: change.lot,
                        place: change.after,
                    });
                }
            }
            match &next.shipment_change {
                Some(split_fulfillment::Change::Pack(p)) => self.packages.push(p.clone()),
                Some(split_fulfillment::Change::Close {
                    package_id,
                    closure,
                    ..
                }) => {
                    self.packages
                        .iter_mut()
                        .find(|p| p.id == *package_id)
                        .expect("valid synthetic serial fixture")
                        .closure = Some(closure.clone())
                }
                None => {}
            }
            self.state = next;
            self.events += 1;
            Ok(id)
        }
        fn count(&mut self, serials: &[&str]) {
            self.apply(
                Command::CountStock {
                    sku: "SKU".into(),
                    lot: "LOT".into(),
                    location: "A".into(),
                    counted: serials.len() as i64,
                    reason: "Independently scanned synthetic opening count".into(),
                },
                obs(&[], serials),
            )
            .expect("valid synthetic serial fixture");
        }
        fn order(&mut self, quantities: &[i64], partial: bool) -> String {
            let order = self
                .apply(
                    Command::CreateOrder {
                        reference: format!("ORDER-{}", self.events),
                        source_order_id: None,
                        customer: "Synthetic".into(),
                        destination: "Synthetic".into(),
                        priority: "normal".into(),
                        lines: quantities
                            .iter()
                            .enumerate()
                            .map(|(n, quantity)| LineInput {
                                id: format!("line-{n}"),
                                sku: "SKU".into(),
                                quantity: *quantity,
                            })
                            .collect(),
                    },
                    vec![],
                )
                .expect("valid synthetic serial fixture");
            self.apply(
                if partial {
                    Command::AllocateAvailable {
                        order_id: order.clone(),
                    }
                } else {
                    Command::Allocate {
                        order_id: order.clone(),
                    }
                },
                vec![],
            )
            .expect("valid synthetic serial fixture");
            order
        }
        fn pick(&mut self, order: &str, line: &str, serials: &[&str]) {
            self.apply(
                Command::Pick {
                    order_id: order.into(),
                    line_id: line.into(),
                    location: "A".into(),
                    lot: "LOT".into(),
                    quantity: serials.len() as i64,
                },
                obs(&[], serials),
            )
            .expect("valid synthetic serial fixture");
        }
    }
    fn obs(target: &[&str], serials: &[&str]) -> Vec<Input> {
        vec![Input {
            target: target.iter().map(|s| (*s).into()).collect(),
            sku: "SKU".into(),
            serials: serials.iter().map(|s| (*s).into()).collect(),
        }]
    }

    #[test]
    fn serial_count_move_and_same_sku_picks_keep_exact_original_line_owners() {
        let mut t = Trial::new();
        t.count(&["SN-1", "SN-2", "SN-3"]);
        t.apply(
            Command::MoveStock {
                sku: "SKU".into(),
                lot: "LOT".into(),
                from: "A".into(),
                to: "B".into(),
                quantity: 1,
            },
            obs(&[], &["SN-3"]),
        )
        .expect("valid synthetic serial fixture");
        let order = t.order(&[1, 1], false);
        t.pick(&order, "line-0", &["SN-1"]);
        t.pick(&order, "line-1", &["SN-2"]);
        let unpick = Command::Unpick {
            order_id: order.clone(),
            line_id: "line-0".into(),
            location: "A".into(),
            lot: "LOT".into(),
            quantity: 1,
            reason: "Wrong unit must not borrow an equal-SKU owner".into(),
        };
        let old_units = t.units.clone();
        let old_state = t.state.clone();
        assert!(t.apply(unpick.clone(), obs(&[], &["SN-2"])).is_err());
        assert_eq!(t.units, old_units);
        assert_eq!(t.state.stocks, old_state.stocks);
        t.apply(unpick, obs(&[], &["SN-1"]))
            .expect("valid synthetic serial fixture");
        assert!(
            matches!(&t.units.iter().find(|u| u.serial == "SN-1").expect("valid synthetic serial fixture").place, Place::OnHand { location, .. } if location == "A")
        );
        assert!(
            matches!(&t.units.iter().find(|u| u.serial == "SN-2").expect("valid synthetic serial fixture").place, Place::Staged { line_id, .. } if line_id == "line-1")
        );
    }

    #[test]
    fn serial_receiving_rejects_reused_cross_facility_and_wrong_lot_identity() {
        let mut t = Trial::new();
        let receipt = t
            .apply(
                Command::CreateReceipt {
                    reference: "ASN".into(),
                    supplier: "Synthetic".into(),
                    lines: vec![ReceiptInput {
                        id: "r".into(),
                        sku: "SKU".into(),
                        lot: "LOT".into(),
                        quantity: 4,
                    }],
                },
                vec![],
            )
            .expect("valid synthetic serial fixture");
        let receive = Command::Receive {
            receipt_id: receipt,
            line_id: "r".into(),
            quantity: 1,
            location: "RECV-01".into(),
        };
        t.apply(receive.clone(), obs(&[], &["New-Serial"]))
            .expect("valid synthetic serial fixture");
        assert!(t.apply(receive.clone(), obs(&[], &["New-Serial"])).is_err());
        t.units.push(Unit {
            sku: "SKU".into(),
            serial: "Elsewhere".into(),
            lot: "LOT".into(),
            place: Place::OnHand {
                warehouse_id: "other".into(),
                location: "A".into(),
            },
        });
        assert!(t.apply(receive.clone(), obs(&[], &["Elsewhere"])).is_err());
        t.units.push(Unit {
            sku: "SKU".into(),
            serial: "Other-Lot".into(),
            lot: "ORIGINAL".into(),
            place: Place::Missing {
                warehouse_id: "main".into(),
                location: "A".into(),
            },
        });
        assert!(t.apply(receive, obs(&[], &["Other-Lot"])).is_err());
        assert_eq!(t.state.receipts[0].lines[0].received, 1);
    }

    #[test]
    fn serial_classic_handoff_return_authorization_and_partial_rma_cancel_preserve_each_identity() {
        let mut t = Trial::new();
        t.count(&["SN-A", "SN-B"]);
        let order = t.order(&[2], false);
        t.pick(&order, "line-0", &["SN-A", "SN-B"]);
        t.apply(
            Command::Pack {
                order_id: order.clone(),
            },
            vec![],
        )
        .expect("valid synthetic serial fixture");
        t.apply(
            Command::Ship {
                order_id: order.clone(),
                carrier: "Synthetic".into(),
                handoff_reference: "Actual test handoff".into(),
            },
            vec![],
        )
        .expect("valid synthetic serial fixture");
        assert!(t
            .units
            .iter()
            .all(|u| matches!(u.place, Place::Shipped { .. })));
        let authorization = |reference: &str, quantity| Command::CreateReturn {
            reference: reference.into(),
            order_id: order.clone(),
            reason: "Synthetic original-line return".into(),
            lines: vec![crate::returns::ReturnLineInput {
                id: "r".into(),
                order_line_id: "line-0".into(),
                lot: "LOT".into(),
                quantity,
            }],
        };
        let rma = t
            .apply(authorization("RMA-1", 2), obs(&["r"], &["SN-A", "SN-B"]))
            .expect("valid synthetic serial fixture");
        assert!(t.state.stocks.iter().all(|s| s.on_hand == 0));
        t.apply(
            Command::Receive {
                receipt_id: rma.clone(),
                line_id: "r".into(),
                quantity: 1,
                location: "RECV-01".into(),
            },
            obs(&[], &["SN-A"]),
        )
        .expect("valid synthetic serial fixture");
        t.apply(
            Command::CancelReceipt {
                receipt_id: rma,
                reason: "Release only the still-unreceived identity".into(),
            },
            vec![],
        )
        .expect("valid synthetic serial fixture");
        assert!(
            matches!(&t.units.iter().find(|u| u.serial == "SN-A").expect("valid synthetic serial fixture").place, Place::OnHand { location, .. } if location == "RECV-01")
        );
        assert!(matches!(
            &t.units
                .iter()
                .find(|u| u.serial == "SN-B")
                .expect("valid synthetic serial fixture")
                .place,
            Place::Shipped {
                return_receipt_id: None,
                ..
            }
        ));
        assert!(t
            .apply(authorization("RMA-Wrong", 1), obs(&["r"], &["SN-A"]))
            .is_err());
        t.apply(authorization("RMA-2", 1), obs(&["r"], &["SN-B"]))
            .expect("valid synthetic serial fixture");
        assert!(matches!(
            &t.units
                .iter()
                .find(|u| u.serial == "SN-B")
                .expect("valid synthetic serial fixture")
                .place,
            Place::Shipped {
                return_receipt_id: Some(_),
                ..
            }
        ));
    }

    #[test]
    fn serial_partial_packages_hold_exact_tuples_then_unpack_unpick_and_cancel_remainder() {
        let mut t = Trial::new();
        t.count(&["ONE", "TWO", "THREE"]);
        let order = t.order(&[1, 2], true);
        t.pick(&order, "line-0", &["ONE"]);
        t.pick(&order, "line-1", &["TWO", "THREE"]);
        let pack = |line: &str, reference: &str| Command::PackShipment {
            order_id: order.clone(),
            reference: reference.into(),
            lines: vec![split_fulfillment::LineInput {
                line_id: line.into(),
                location: "A".into(),
                lot: "LOT".into(),
                quantity: 1,
            }],
        };
        assert!(t
            .apply(
                pack("line-0", "Wrong"),
                obs(&["line-0", "A", "LOT"], &["TWO"])
            )
            .is_err());
        let first = t
            .apply(
                pack("line-0", "First"),
                obs(&["line-0", "A", "LOT"], &["ONE"]),
            )
            .expect("valid synthetic serial fixture");
        let second = t
            .apply(
                pack("line-1", "Second"),
                obs(&["line-1", "A", "LOT"], &["TWO"]),
            )
            .expect("valid synthetic serial fixture");
        let unpick = |serials: &[&str]| {
            (
                Command::Unpick {
                    order_id: order.clone(),
                    line_id: "line-1".into(),
                    location: "A".into(),
                    lot: "LOT".into(),
                    quantity: 1,
                    reason: "Original physical return".into(),
                },
                obs(&[], serials),
            )
        };
        let (command, input) = unpick(&["TWO"]);
        assert!(
            t.apply(command, input).is_err(),
            "One unpacked sibling does not free a packed serial"
        );
        t.apply(
            Command::HandoffShipment {
                shipment_id: first,
                carrier: "Synthetic".into(),
                handoff_reference: "FIRST".into(),
            },
            vec![],
        )
        .expect("valid synthetic serial fixture");
        t.apply(
            Command::UnpackShipment {
                shipment_id: second,
                reason: "Recover remaining work".into(),
            },
            vec![],
        )
        .expect("valid synthetic serial fixture");
        t.apply(
            Command::Unpick {
                order_id: order.clone(),
                line_id: "line-1".into(),
                location: "A".into(),
                lot: "LOT".into(),
                quantity: 2,
                reason: "Observed physical return to bin".into(),
            },
            obs(&[], &["TWO", "THREE"]),
        )
        .expect("valid synthetic serial fixture");
        t.apply(
            Command::ReleaseUnpicked {
                order_id: order.clone(),
                reason: "Release recovered remainder".into(),
            },
            vec![],
        )
        .expect("valid synthetic serial fixture");
        t.apply(
            Command::CancelRemainder {
                order_id: order,
                reason: "Cancel only unshipped demand".into(),
            },
            vec![],
        )
        .expect("valid synthetic serial fixture");
        assert_eq!(
            t.units
                .iter()
                .filter(|u| matches!(u.place, Place::Shipped { .. }))
                .count(),
            1
        );
        assert_eq!(
            t.units
                .iter()
                .filter(|u| matches!(u.place, Place::OnHand { .. }))
                .count(),
            2
        );
        assert_eq!(t.state.stocks.iter().map(|s| s.on_hand).sum::<i64>(), 2);
    }

    fn transfer_context(destination: &str) -> crate::transfers::Transfer {
        crate::transfers::Transfer {
            id: "transfer-original".into(),
            seq: 1,
            source_warehouse_id: "source".into(),
            destination_warehouse_id: destination.into(),
            reference: "Synthetic".into(),
            carrier: "Synthetic".into(),
            handoff_reference: "HANDOFF".into(),
            actor: "synthetic:operator".into(),
            dispatched_at: domain::now(),
            source_version: 10,
            source_event_id: "dispatch-event".into(),
            lines: vec![crate::transfers::Line {
                id: "t-line".into(),
                sku: "SKU".into(),
                lot: "LOT".into(),
                location: "A".into(),
                quantity: 2,
                movement_id: "dispatch-move".into(),
                received: 0,
                lost: 0,
                loss_record_count: 0,
            }],
            reviewed_loss: None,
            ok: true,
        }
    }
    fn transit(serial: &str) -> Unit {
        Unit {
            sku: "SKU".into(),
            serial: serial.into(),
            lot: "LOT".into(),
            place: Place::InTransit {
                source_warehouse_id: "source".into(),
                destination_warehouse_id: "main".into(),
                transfer_id: "transfer-original".into(),
                line_id: "t-line".into(),
            },
        }
    }

    #[test]
    fn serial_destination_loss_reversal_and_separate_arrival_never_credit_stock_early() {
        let mut t = Trial::new();
        t.units = vec![transit("LOST-ONE"), transit("LOST-TWO")];
        t.state.transfer_context = Some(transfer_context("main"));
        let loss = t
            .apply(
                Command::RecordTransferLoss {
                    transfer_id: "transfer-original".into(),
                    line_id: "t-line".into(),
                    quantity: 2,
                    incident_reference: "INCIDENT".into(),
                    reason: "Independently confirmed missing goods".into(),
                },
                obs(&[], &["LOST-ONE", "LOST-TWO"]),
            )
            .expect("valid synthetic serial fixture");
        assert!(t.state.stocks.is_empty());
        let source = t
            .state
            .transfer_context
            .as_mut()
            .expect("valid synthetic serial fixture");
        source.lines[0].lost = 2;
        source.reviewed_loss = Some(crate::transfers::Loss {
            id: loss.clone(),
            quantity: 2,
            reversed: 0,
            incident_reference: "INCIDENT".into(),
            reason: "Original incident".into(),
            actor: "synthetic:operator".into(),
            recorded_at: domain::now(),
            event_id: "loss-event".into(),
            version: t.state.version,
            line_id: "t-line".into(),
        });
        let reversal = Command::ReverseTransferLoss {
            loss_id: loss,
            quantity: 1,
            reason: "Located exact original identity".into(),
        };
        t.apply(reversal.clone(), obs(&[], &["LOST-ONE"]))
            .expect("valid synthetic serial fixture");
        assert!(t.state.stocks.is_empty());
        let source = t
            .state
            .transfer_context
            .as_mut()
            .expect("valid synthetic serial fixture");
        source.lines[0].lost = 1;
        source
            .reviewed_loss
            .as_mut()
            .expect("valid synthetic serial fixture")
            .reversed = 1;
        assert!(
            t.apply(reversal, obs(&[], &["LOST-ONE"])).is_err(),
            "An available aggregate allowance cannot reverse the same recovered identity again"
        );
        let arrival = Command::ReceiveTransfer {
            transfer_id: "transfer-original".into(),
            line_id: "t-line".into(),
            quantity: 1,
            location: "RECV-01".into(),
            arrival_reference: "Actually observed receiving".into(),
        };
        assert!(t.apply(arrival.clone(), obs(&[], &["LOST-TWO"])).is_err());
        t.apply(arrival, obs(&[], &["LOST-ONE"]))
            .expect("valid synthetic serial fixture");
        assert_eq!(t.state.stocks.iter().map(|s| s.on_hand).sum::<i64>(), 1);
        assert!(matches!(
            &t.units
                .iter()
                .find(|u| u.serial == "LOST-TWO")
                .expect("valid synthetic serial fixture")
                .place,
            Place::Lost { .. }
        ));
    }

    #[test]
    fn serial_dispatch_and_partial_arrival_cannot_exchange_equal_sku_original_lines() {
        let mut source = Trial::new();
        source.count(&["LINE-ONE", "LINE-TWO", "STAYS"]);
        let lines = vec![
            crate::transfers::LineInput {
                id: "one".into(),
                sku: "SKU".into(),
                lot: "LOT".into(),
                location: "A".into(),
                quantity: 1,
            },
            crate::transfers::LineInput {
                id: "two".into(),
                sku: "SKU".into(),
                lot: "LOT".into(),
                location: "A".into(),
                quantity: 1,
            },
        ];
        let dispatched = source
            .apply(
                Command::DispatchTransfer {
                    reference: "Two original owners".into(),
                    destination_warehouse_id: "other".into(),
                    carrier: "Synthetic".into(),
                    handoff_reference: "Physical test dispatch".into(),
                    lines: lines.clone(),
                },
                vec![
                    obs(&["one"], &["LINE-ONE"]).remove(0),
                    obs(&["two"], &["LINE-TWO"]).remove(0),
                ],
            )
            .expect("valid synthetic serial fixture");
        assert_eq!(
            source.state.stocks.iter().map(|s| s.on_hand).sum::<i64>(),
            1
        );
        assert_eq!(
            source
                .units
                .iter()
                .filter(|u| matches!(u.place, Place::InTransit { .. }))
                .count(),
            2
        );
        let mut destination = Trial::new();
        destination.state.warehouse.id = "other".into();
        destination.units = source.units.clone();
        let mut transfer = transfer_context("other");
        transfer.id = dispatched.clone();
        transfer.source_warehouse_id = "main".into();
        transfer.lines = lines
            .iter()
            .map(|l| crate::transfers::Line {
                id: l.id.clone(),
                sku: l.sku.clone(),
                lot: l.lot.clone(),
                location: l.location.clone(),
                quantity: l.quantity,
                movement_id: format!("move-{}", l.id),
                received: 0,
                lost: 0,
                loss_record_count: 0,
            })
            .collect();
        destination.state.transfer_context = Some(transfer);
        let receive = |line: &str| Command::ReceiveTransfer {
            transfer_id: dispatched.clone(),
            line_id: line.into(),
            quantity: 1,
            location: "RECV-01".into(),
            arrival_reference: "Actual exact-line receiving".into(),
        };
        assert!(destination
            .apply(receive("one"), obs(&[], &["LINE-TWO"]))
            .is_err());
        destination
            .apply(receive("one"), obs(&[], &["LINE-ONE"]))
            .expect("valid synthetic serial fixture");
        destination
            .state
            .transfer_context
            .as_mut()
            .expect("valid synthetic serial fixture")
            .lines[0]
            .received = 1;
        assert!(destination
            .apply(receive("two"), obs(&[], &["LINE-ONE"]))
            .is_err());
        assert_eq!(
            destination
                .state
                .stocks
                .iter()
                .map(|s| s.on_hand)
                .sum::<i64>(),
            1
        );
        destination
            .apply(receive("two"), obs(&[], &["LINE-TWO"]))
            .expect("valid synthetic serial fixture");
        assert_eq!(
            destination
                .state
                .stocks
                .iter()
                .map(|s| s.on_hand)
                .sum::<i64>(),
            2
        );
        assert!(
            matches!(&destination.units.iter().find(|u| u.serial == "STAYS").expect("valid synthetic serial fixture").place, Place::OnHand { warehouse_id, location } if warehouse_id == "main" && location == "A")
        );
    }

    #[test]
    fn serial_counts_mark_missing_and_found_without_teleporting_transit_or_shipped_units() {
        let mut t = Trial::new();
        t.count(&["Seen", "Missing"]);
        t.count(&["Seen"]);
        assert!(matches!(
            t.units
                .iter()
                .find(|u| u.serial == "Missing")
                .expect("valid synthetic serial fixture")
                .place,
            Place::Missing { .. }
        ));
        t.apply(
            Command::CountStock {
                sku: "SKU".into(),
                lot: "LOT".into(),
                location: "B".into(),
                counted: 1,
                reason: "Actually found in another bin".into(),
            },
            obs(&[], &["Missing"]),
        )
        .expect("valid synthetic serial fixture");
        let command = Command::CountStock {
            sku: "SKU".into(),
            lot: "LOT".into(),
            location: "A".into(),
            counted: 2,
            reason: "May not teleport an existing identity".into(),
        };
        assert!(t
            .apply(command.clone(), obs(&[], &["Seen", "Missing"]))
            .is_err());
        t.units.push(transit("Transit"));
        assert!(t.apply(command, obs(&[], &["Seen", "Transit"])).is_err());
        t.count(&[]);
        assert_eq!(
            t.state
                .stocks
                .iter()
                .find(|s| s.location == "A")
                .expect("valid synthetic serial fixture")
                .on_hand,
            0
        );
    }

    #[test]
    fn serial_observations_reject_duplicates_wrong_targets_unknown_units_and_control_text() {
        let mut t = Trial::new();
        t.count(&["Exact", "exact"]);
        let command = Command::MoveStock {
            sku: "SKU".into(),
            lot: "LOT".into(),
            from: "A".into(),
            to: "B".into(),
            quantity: 1,
        };
        for input in [
            vec![],
            obs(&[], &[" Exact"]),
            obs(&[], &["Exact\u{85}hidden"]),
            obs(&["unrelated"], &["Exact"]),
            obs(&[], &["Unknown"]),
            obs(&[], &["Exact", "Exact"]),
        ] {
            assert!(t.apply(command.clone(), input).is_err());
        }
        t.apply(command, obs(&[], &["exact"]))
            .expect("valid synthetic serial fixture");
        assert!(
            matches!(&t.units.iter().find(|u| u.serial == "Exact").expect("valid synthetic serial fixture").place, Place::OnHand { location, .. } if location == "A")
        );
        let count = Command::CountStock {
            sku: "SKU".into(),
            lot: "LOT".into(),
            location: "A".into(),
            counted: 1,
            reason: "Count".into(),
        };
        let input = obs(&[], &["Exact"])[0].clone();
        assert!(t.apply(count, vec![input.clone(), input]).is_err());
    }

    #[test]
    fn serial_equations_reject_missing_baselines_and_stock_balanced_owner_forgery() {
        let mut t = Trial::new();
        t.count(&["A1", "A2"]);
        let order = t.order(&[1, 1], false);
        t.pick(&order, "line-0", &["A1"]);
        t.pick(&order, "line-1", &["A2"]);
        let c = Command::Unpick {
            order_id: order,
            line_id: "line-0".into(),
            location: "A".into(),
            lot: "LOT".into(),
            quantity: 1,
            reason: "Original line".into(),
        };
        t.prepare(&c);
        let (after, result) = t
            .state
            .execute(&c, "synthetic:operator")
            .expect("valid synthetic serial fixture");
        let context = t.context();
        let input = obs(&[], &["A1"]);
        let mut missing = context.clone();
        missing.physical.clear();
        assert!(plan(
            &t.state,
            &after,
            &c,
            &input,
            &missing,
            result["entity_id"]
                .as_str()
                .expect("valid synthetic serial fixture"),
            "new-event"
        )
        .is_err());
        let mut forged = context.clone();
        for unit in &mut forged.units {
            if let Place::Staged { line_id, .. } = &mut unit.place {
                *line_id = if *line_id == "line-0" {
                    "line-1".into()
                } else {
                    "line-0".into()
                };
            }
        }
        // Counts and total stock remain identical; exact selected ownership is wrong.
        assert!(plan(
            &t.state,
            &after,
            &c,
            &input,
            &forged,
            result["entity_id"]
                .as_str()
                .expect("valid synthetic serial fixture"),
            "new-event"
        )
        .is_err());
    }

    #[test]
    fn serial_derived_unpack_rejects_partial_lookup_even_when_physical_stock_stays_balanced() {
        let mut t = Trial::new();
        t.count(&["PACKED"]);
        let order = t.order(&[1], true);
        t.pick(&order, "line-0", &["PACKED"]);
        let package = t
            .apply(
                Command::PackShipment {
                    order_id: order,
                    reference: "Original".into(),
                    lines: vec![split_fulfillment::LineInput {
                        line_id: "line-0".into(),
                        location: "A".into(),
                        lot: "LOT".into(),
                        quantity: 1,
                    }],
                },
                obs(&["line-0", "A", "LOT"], &["PACKED"]),
            )
            .expect("valid synthetic serial fixture");
        let command = Command::UnpackShipment {
            shipment_id: package,
            reason: "No physical movement".into(),
        };
        t.prepare(&command);
        let (after, result) = t
            .state
            .execute(&command, "synthetic:operator")
            .expect("valid synthetic serial fixture");
        assert_eq!(t.state.stocks, after.stocks);
        let mut context = t.context();
        context.units.clear();
        assert!(plan(
            &t.state,
            &after,
            &command,
            &[],
            &context,
            result["entity_id"]
                .as_str()
                .expect("valid synthetic serial fixture"),
            "event-original-unpack"
        )
        .is_err());
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(24))]
        #[test]
        fn serial_repeated_moves_preserve_every_identity_and_exact_bin_quantities(indices in prop::collection::vec(0usize..12, 1..80)) {
            let mut t = Trial::new();
            let names: Vec<_> = (0..12).map(|n| format!("UNIT-{n}")).collect();
            t.count(&names.iter().map(String::as_str).collect::<Vec<_>>());
            for index in indices {
                let name = &names[index];
                let Place::OnHand { location, .. } = &t.units.iter().find(|u| u.serial == *name).expect("valid synthetic serial fixture").place else { unreachable!() };
                let destination = if location == "A" { "B" } else { "A" };
                t.apply(Command::MoveStock { sku: "SKU".into(), lot: "LOT".into(), from: location.clone(), to: destination.into(), quantity: 1 }, obs(&[], &[name])).expect("valid synthetic serial fixture");
                prop_assert_eq!(t.units.len(), 12);
                prop_assert_eq!(t.units.iter().map(|u| u.serial.as_str()).collect::<BTreeSet<_>>().len(), 12);
                for bin in ["A", "B"] {
                    let observed = t.units.iter().filter(|u| matches!(&u.place, Place::OnHand { location, .. } if location == bin)).count() as i64;
                    let stock = t.state.stocks.iter().filter(|s| s.location == bin).map(|s| s.on_hand).sum::<i64>();
                    prop_assert_eq!(observed, stock);
                }
                prop_assert!(t.state.validate().is_ok());
            }
        }
    }
}
