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
