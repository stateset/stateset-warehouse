use proptest::prelude::*;
use stateset_warehouse::domain::*;

fn apply(s: &mut State, c: Command) -> String {
    let (next, result) = s
        .execute(&c, "test:operator")
        .expect("valid test transition");
    *s = next;
    result["entity_id"].as_str().expect("entity ID").into()
}
fn fixture(q: i64) -> State {
    let mut s = State::empty(Warehouse {
        id: "test".into(),
        code: "TEST".into(),
        name: "Test".into(),
        city: "Test".into(),
        timezone: "UTC".into(),
    });
    apply(
        &mut s,
        Command::CreateProduct {
            serial_controlled: false,
            sku: "SKU-A".into(),
            name: "Widget".into(),
            category: "Test".into(),
            reorder_point: 2,
        },
    );
    apply(
        &mut s,
        Command::CreateLocation {
            code: "A-01".into(),
            zone: "A".into(),
            kind: "storage".into(),
        },
    );
    apply(
        &mut s,
        Command::CreateLocation {
            code: "B-01".into(),
            zone: "B".into(),
            kind: "storage".into(),
        },
    );
    let receipt = apply(
        &mut s,
        Command::CreateReceipt {
            reference: "RCV-1".into(),
            supplier: "Supplier".into(),
            lines: vec![ReceiptInput {
                id: "r1".into(),
                sku: "SKU-A".into(),
                lot: "LOT-1".into(),
                quantity: q,
            }],
        },
    );
    apply(
        &mut s,
        Command::Receive {
            receipt_id: receipt,
            line_id: "r1".into(),
            quantity: q,
            location: "RECV-01".into(),
        },
    );
    apply(
        &mut s,
        Command::MoveStock {
            sku: "SKU-A".into(),
            lot: "LOT-1".into(),
            from: "RECV-01".into(),
            to: "A-01".into(),
            quantity: q,
        },
    );
    s
}
fn order(s: &mut State, reference: &str, quantities: &[i64]) -> String {
    apply(
        s,
        Command::CreateOrder {
            reference: reference.into(),
            source_order_id: Some(format!("oms-{reference}")),
            customer: "Customer".into(),
            destination: "Destination".into(),
            priority: "normal".into(),
            lines: quantities
                .iter()
                .enumerate()
                .map(|(i, q)| LineInput {
                    id: format!("line-{i}"),
                    sku: "SKU-A".into(),
                    quantity: *q,
                })
                .collect(),
        },
    )
}
fn stock(s: &State, location: &str) -> (i64, i64) {
    s.stocks
        .iter()
        .find(|b| b.location == location && b.sku == "SKU-A")
        .map(|b| (b.on_hand, b.reserved))
        .unwrap_or_default()
}

#[test]
fn physical_lifecycle_preserves_stock_until_carrier_handoff() {
    let mut s = fixture(10);
    let oid = order(&mut s, "SO-1", &[3]);
    apply(
        &mut s,
        Command::Allocate {
            order_id: oid.clone(),
        },
    );
    assert_eq!(stock(&s, "A-01"), (10, 3));
    apply(
        &mut s,
        Command::Pick {
            order_id: oid.clone(),
            line_id: "line-0".into(),
            location: "A-01".into(),
            lot: "LOT-1".into(),
            quantity: 1,
        },
    );
    assert_eq!(stock(&s, "A-01"), (9, 2));
    assert_eq!(stock(&s, "STAGE-01"), (1, 1));
    assert_eq!(s.stocks.iter().map(|b| b.on_hand).sum::<i64>(), 10);
    assert!(s
        .execute(
            &Command::Pack {
                order_id: oid.clone()
            },
            "op"
        )
        .is_err());
    assert!(s
        .execute(
            &Command::CancelOrder {
                order_id: oid.clone(),
                reason: "Too late".into()
            },
            "op"
        )
        .is_err());
    apply(
        &mut s,
        Command::Pick {
            order_id: oid.clone(),
            line_id: "line-0".into(),
            location: "A-01".into(),
            lot: "LOT-1".into(),
            quantity: 2,
        },
    );
    apply(
        &mut s,
        Command::Pack {
            order_id: oid.clone(),
        },
    );
    assert_eq!(s.stocks.iter().map(|b| b.on_hand).sum::<i64>(), 10);
    apply(
        &mut s,
        Command::Ship {
            order_id: oid.clone(),
            carrier: "Carrier".into(),
            handoff_reference: "PICKUP-1".into(),
        },
    );
    assert_eq!(stock(&s, "A-01"), (7, 0));
    assert_eq!(stock(&s, "STAGE-01"), (0, 0));
    assert_eq!(s.stocks.iter().map(|b| b.on_hand).sum::<i64>(), 7);
    assert!(s
        .execute(
            &Command::Ship {
                order_id: oid,
                carrier: "Carrier".into(),
                handoff_reference: "PICKUP-1".into()
            },
            "op"
        )
        .is_err());
    assert!(s.reconciliation().iter().all(|r| r["ok"] == true));
}
#[test]
fn same_sku_lines_cannot_steal_each_others_reservations() {
    let mut s = fixture(6);
    let oid = order(&mut s, "SO-LINES", &[5, 1]);
    apply(
        &mut s,
        Command::Allocate {
            order_id: oid.clone(),
        },
    );
    apply(
        &mut s,
        Command::Pick {
            order_id: oid.clone(),
            line_id: "line-1".into(),
            location: "A-01".into(),
            lot: "LOT-1".into(),
            quantity: 1,
        },
    );
    let o = s.orders.iter().find(|o| o.id == oid).expect("order");
    assert_eq!(
        o.allocations
            .iter()
            .find(|a| a.line_id == "line-0")
            .expect("line 0")
            .picked,
        0
    );
    assert_eq!(
        o.allocations
            .iter()
            .find(|a| a.line_id == "line-1")
            .expect("line 1")
            .picked,
        1
    );
    assert_eq!(stock(&s, "A-01"), (5, 5));
    assert!(s
        .execute(
            &Command::Pick {
                order_id: oid,
                line_id: "line-1".into(),
                location: "A-01".into(),
                lot: "LOT-1".into(),
                quantity: 1
            },
            "op"
        )
        .is_err());
}
#[test]
fn insufficient_second_line_rolls_back_all_reservations() {
    let mut s = fixture(5);
    let oid = order(&mut s, "SO-FAIL", &[4, 2]);
    let before = s.clone();
    assert!(s
        .execute(&Command::Allocate { order_id: oid }, "op")
        .is_err());
    assert_eq!(s, before);
    assert_eq!(stock(&s, "A-01"), (5, 0));
}
#[test]
fn cancellation_releases_only_its_own_order() {
    let mut s = fixture(10);
    let first = order(&mut s, "SO-A", &[3]);
    let second = order(&mut s, "SO-B", &[5]);
    apply(
        &mut s,
        Command::Allocate {
            order_id: first.clone(),
        },
    );
    apply(&mut s, Command::Allocate { order_id: second });
    apply(
        &mut s,
        Command::CancelOrder {
            order_id: first,
            reason: "OMS cancelled".into(),
        },
    );
    assert_eq!(stock(&s, "A-01"), (10, 5));
    s.validate().expect("reconciled");
}
#[test]
fn receiving_is_partial_bounded_and_not_allocatable_until_putaway() {
    let mut s = fixture(1);
    let r = apply(
        &mut s,
        Command::CreateReceipt {
            reference: "RCV-PARTIAL".into(),
            supplier: "Supplier".into(),
            lines: vec![ReceiptInput {
                id: "r1".into(),
                sku: "SKU-A".into(),
                lot: "LOT-1".into(),
                quantity: 10,
            }],
        },
    );
    apply(
        &mut s,
        Command::Receive {
            receipt_id: r.clone(),
            line_id: "r1".into(),
            quantity: 4,
            location: "RECV-01".into(),
        },
    );
    assert_eq!(s.receipts.last().expect("receipt").status, "partial");
    assert!(s
        .execute(
            &Command::Receive {
                receipt_id: r.clone(),
                line_id: "r1".into(),
                quantity: 7,
                location: "RECV-01".into()
            },
            "op"
        )
        .is_err());
    let oid = order(&mut s, "SO-NOT-PUTAWAY", &[2]);
    assert!(s
        .execute(
            &Command::Allocate {
                order_id: oid.clone()
            },
            "op"
        )
        .is_err());
    apply(
        &mut s,
        Command::MoveStock {
            sku: "SKU-A".into(),
            lot: "LOT-1".into(),
            from: "RECV-01".into(),
            to: "A-01".into(),
            quantity: 2,
        },
    );
    apply(&mut s, Command::Allocate { order_id: oid });
    apply(
        &mut s,
        Command::Receive {
            receipt_id: r,
            line_id: "r1".into(),
            quantity: 6,
            location: "RECV-01".into(),
        },
    );
    assert_eq!(s.receipts.last().expect("receipt").status, "received");
}
#[test]
fn counts_and_transfers_cannot_consume_reserved_stock() {
    let mut s = fixture(10);
    let oid = order(&mut s, "SO-COUNT", &[8]);
    apply(&mut s, Command::Allocate { order_id: oid });
    assert!(s
        .execute(
            &Command::CountStock {
                sku: "SKU-A".into(),
                lot: "LOT-1".into(),
                location: "A-01".into(),
                counted: 7,
                reason: "Observed count".into()
            },
            "op"
        )
        .is_err());
    assert!(s
        .execute(
            &Command::MoveStock {
                sku: "SKU-A".into(),
                lot: "LOT-1".into(),
                from: "A-01".into(),
                to: "B-01".into(),
                quantity: 3
            },
            "op"
        )
        .is_err());
    apply(
        &mut s,
        Command::CountStock {
            sku: "SKU-A".into(),
            lot: "LOT-1".into(),
            location: "A-01".into(),
            counted: 9,
            reason: "Physical recount".into(),
        },
    );
    assert_eq!(stock(&s, "A-01"), (9, 8));
    assert_eq!(s.movements.last().expect("count movement").quantity, 1);
}
#[test]
fn exceptions_block_execution_and_resolution_does_not_invent_stock() {
    let mut s = fixture(3);
    let oid = order(&mut s, "SO-EXC", &[4]);
    let eid = apply(
        &mut s,
        Command::ReportException {
            order_id: Some(oid.clone()),
            category: "Short pick".into(),
            detail: "Only three found".into(),
        },
    );
    assert!(s
        .execute(
            &Command::Allocate {
                order_id: oid.clone()
            },
            "op"
        )
        .expect_err("blocked")
        .contains("exception"));
    apply(
        &mut s,
        Command::ResolveException {
            exception_id: eid,
            resolution: "Recount confirms three; order awaits replenishment".into(),
        },
    );
    assert!(s
        .execute(&Command::Allocate { order_id: oid }, "op")
        .expect_err("still short")
        .contains("Insufficient"));
    assert_eq!(stock(&s, "A-01"), (3, 0));
}
#[test]
fn independent_reconciliation_detects_balance_and_reservation_corruption() {
    let mut s = fixture(10);
    s.stocks
        .iter_mut()
        .find(|b| b.location == "A-01")
        .expect("bin")
        .on_hand = 11;
    assert!(s.validate().is_err());
    let mut s = fixture(10);
    s.stocks
        .iter_mut()
        .find(|b| b.location == "A-01")
        .expect("bin")
        .reserved = 1;
    assert!(s.validate().is_err());
}

#[test]
fn receipt_counters_and_duplicate_sku_lines_require_exact_movement_evidence() {
    let mut s = fixture(10);
    // Forging both the status and counter used to pass stock reconciliation.
    s.receipts[0].lines[0].received = 9;
    s.receipts[0].status = "partial".into();
    assert!(s
        .validate()
        .expect_err("counter forgery")
        .contains("movement evidence"));

    let mut s = fixture(1);
    let receipt = apply(
        &mut s,
        Command::CreateReceipt {
            reference: "RCV-SAME-SKU-LOT".into(),
            supplier: "Supplier".into(),
            lines: vec![
                ReceiptInput {
                    id: "first".into(),
                    sku: "SKU-A".into(),
                    lot: "SHARED-LOT".into(),
                    quantity: 5,
                },
                ReceiptInput {
                    id: "second".into(),
                    sku: "SKU-A".into(),
                    lot: "SHARED-LOT".into(),
                    quantity: 5,
                },
            ],
        },
    );
    apply(
        &mut s,
        Command::Receive {
            receipt_id: receipt.clone(),
            line_id: "first".into(),
            quantity: 2,
            location: "RECV-01".into(),
        },
    );
    apply(
        &mut s,
        Command::Receive {
            receipt_id: receipt.clone(),
            line_id: "second".into(),
            quantity: 3,
            location: "RECV-01".into(),
        },
    );
    let movements: Vec<_> = s
        .movements
        .iter()
        .filter(|m| m.receipt_id.as_deref() == Some(receipt.as_str()))
        .collect();
    assert_eq!(movements[0].receipt_line_id.as_deref(), Some("first"));
    assert_eq!(movements[1].receipt_line_id.as_deref(), Some("second"));
    s.validate()
        .expect("identical SKU/lot receipt lines are separately evidenced");
    s.movements
        .last_mut()
        .expect("second line movement")
        .receipt_line_id = Some("first".into());
    assert!(s.validate().is_err());
}

#[test]
fn receipt_evidence_rejects_wrong_reference_ownership_and_missing_identity() {
    let fixture = fixture(10);
    for alteration in 0..4 {
        let mut s = fixture.clone();
        let movement = s
            .movements
            .iter_mut()
            .find(|m| m.kind == "receipt")
            .expect("receipt movement");
        match alteration {
            0 => movement.reference = "OTHER-REFERENCE".into(),
            1 => movement.receipt_id = Some("other-receipt".into()),
            2 => movement.receipt_line_id = Some("other-line".into()),
            _ => {
                movement.receipt_id = None;
                movement.receipt_line_id = None;
            }
        }
        assert!(s.validate().is_err());
    }
}

#[test]
fn allocation_status_and_unique_ownership_are_validated_independently() {
    let mut s = fixture(10);
    let oid = order(&mut s, "SO-STATUS", &[4]);
    apply(
        &mut s,
        Command::Allocate {
            order_id: oid.clone(),
        },
    );
    let mut forged = s.clone();
    forged
        .orders
        .iter_mut()
        .find(|o| o.id == oid)
        .expect("order")
        .status = "picking".into();
    assert!(forged
        .validate()
        .expect_err("picking requires physical work")
        .contains("partial pick"));
    let mut forged = s.clone();
    let order = forged
        .orders
        .iter_mut()
        .find(|o| o.id == oid)
        .expect("order");
    order.allocations[0].quantity = 2;
    order.allocations.push(order.allocations[0].clone());
    assert!(forged
        .validate()
        .expect_err("duplicate allocations can strand picks")
        .contains("ownership identity"));
    apply(
        &mut s,
        Command::Pick {
            order_id: oid.clone(),
            line_id: "line-0".into(),
            location: "A-01".into(),
            lot: "LOT-1".into(),
            quantity: 2,
        },
    );
    let mut forged = s.clone();
    forged
        .orders
        .iter_mut()
        .find(|o| o.id == oid)
        .expect("order")
        .status = "allocated".into();
    assert!(forged
        .validate()
        .expect_err("allocated status must expose physical pick")
        .contains("picked stock"));
    let movement = s.movements.last().expect("pick movement").clone();
    s.movements.push(movement);
    assert!(s
        .validate()
        .expect_err("movement evidence cannot be replayed under same ID")
        .contains("physical movement"));
}

#[test]
fn legacy_receipt_identity_is_backfilled_only_when_unambiguous() {
    let mut s = fixture(10);
    let mut value = serde_json::to_value(&s).expect("serialize original snapshot");
    for movement in value["movements"].as_array_mut().expect("movements") {
        movement
            .as_object_mut()
            .expect("object")
            .remove("receipt_id");
        movement
            .as_object_mut()
            .expect("object")
            .remove("receipt_line_id");
    }
    s = serde_json::from_value(value).expect("legacy receipt fields default during parsing");
    s.upgrade_legacy_receipt_evidence()
        .expect("unambiguous original line");
    s.validate()
        .expect("backfilled original evidence remains valid");

    let mut ambiguous = fixture(10);
    let receipt = &mut ambiguous.receipts[0];
    receipt.lines.push(ReceiptLine {
        id: "duplicate".into(),
        sku: "SKU-A".into(),
        lot: "LOT-1".into(),
        quantity: 1,
        received: 0,
    });
    for movement in ambiguous
        .movements
        .iter_mut()
        .filter(|m| m.kind == "receipt")
    {
        movement.receipt_id = None;
        movement.receipt_line_id = None;
    }
    let before = ambiguous.clone();
    assert!(ambiguous
        .upgrade_legacy_receipt_evidence()
        .expect_err("must not guess duplicate line ownership")
        .contains("original line identity"));
    assert_eq!(ambiguous, before);
}

#[test]
fn short_pick_can_be_returned_recounted_replenished_and_reallocated() {
    let mut s = fixture(10);
    let oid = order(&mut s, "SO-SHORT", &[8]);
    apply(
        &mut s,
        Command::Allocate {
            order_id: oid.clone(),
        },
    );
    apply(
        &mut s,
        Command::Pick {
            order_id: oid.clone(),
            line_id: "line-0".into(),
            location: "A-01".into(),
            lot: "LOT-1".into(),
            quantity: 3,
        },
    );
    let exception = apply(
        &mut s,
        Command::ReportException {
            order_id: Some(oid.clone()),
            category: "Short pick".into(),
            detail: "Only three physical units found; source bin is empty".into(),
        },
    );
    assert!(s
        .execute(
            &Command::ResetAllocation {
                order_id: oid.clone(),
                reason: "Recount required".into(),
            },
            "op"
        )
        .expect_err("staged ownership must be preserved")
        .contains("staged"));

    // Recovery remains available while the order is blocked. The returned three
    // units are physically observed; no fictional completion of the pick occurs.
    apply(
        &mut s,
        Command::Unpick {
            order_id: oid.clone(),
            line_id: "line-0".into(),
            location: "A-01".into(),
            lot: "LOT-1".into(),
            quantity: 3,
            reason: "Return verified units for recount".into(),
        },
    );
    assert_eq!(stock(&s, "A-01"), (10, 8));
    assert_eq!(stock(&s, "STAGE-01"), (0, 0));
    apply(
        &mut s,
        Command::ResetAllocation {
            order_id: oid.clone(),
            reason: "Release reservations for physical recount".into(),
        },
    );
    assert_eq!(stock(&s, "A-01"), (10, 0));
    assert_eq!(
        s.orders.iter().find(|o| o.id == oid).expect("order").status,
        "pending"
    );
    assert!(s
        .activity
        .last()
        .expect("audit")
        .detail
        .contains("physical recount"));
    apply(
        &mut s,
        Command::CountStock {
            sku: "SKU-A".into(),
            lot: "LOT-1".into(),
            location: "A-01".into(),
            counted: 3,
            reason: "Physical recount: three returned units only".into(),
        },
    );
    apply(
        &mut s,
        Command::ResolveException {
            exception_id: exception,
            resolution: "Recount confirms three; five units replenishing".into(),
        },
    );
    assert!(s
        .execute(
            &Command::Allocate {
                order_id: oid.clone()
            },
            "op"
        )
        .is_err());
    assert_eq!(stock(&s, "A-01"), (3, 0));
    let receipt = apply(
        &mut s,
        Command::CreateReceipt {
            reference: "RCV-REPLENISH".into(),
            supplier: "Supplier".into(),
            lines: vec![ReceiptInput {
                id: "replenish".into(),
                sku: "SKU-A".into(),
                lot: "LOT-1".into(),
                quantity: 5,
            }],
        },
    );
    apply(
        &mut s,
        Command::Receive {
            receipt_id: receipt,
            line_id: "replenish".into(),
            quantity: 5,
            location: "RECV-01".into(),
        },
    );
    apply(
        &mut s,
        Command::MoveStock {
            sku: "SKU-A".into(),
            lot: "LOT-1".into(),
            from: "RECV-01".into(),
            to: "A-01".into(),
            quantity: 5,
        },
    );
    apply(
        &mut s,
        Command::Allocate {
            order_id: oid.clone(),
        },
    );
    apply(
        &mut s,
        Command::Pick {
            order_id: oid.clone(),
            line_id: "line-0".into(),
            location: "A-01".into(),
            lot: "LOT-1".into(),
            quantity: 8,
        },
    );
    apply(
        &mut s,
        Command::Pack {
            order_id: oid.clone(),
        },
    );
    apply(
        &mut s,
        Command::Ship {
            order_id: oid,
            carrier: "Carrier".into(),
            handoff_reference: "SHORT-RECOVERED".into(),
        },
    );
    assert_eq!(s.stocks.iter().map(|b| b.on_hand).sum::<i64>(), 0);
    assert!(s.reconciliation().iter().all(|r| r["ok"] == true));
    assert_eq!(
        s.movements
            .iter()
            .filter(|m| m.kind == "count")
            .map(|m| m.quantity)
            .sum::<i64>(),
        7
    );
}

#[test]
fn recovery_preserves_exact_line_and_order_ownership_and_invalidates_packing() {
    let mut s = fixture(12);
    let first = order(&mut s, "SO-RETURN-A", &[4, 2]);
    let second = order(&mut s, "SO-RETURN-B", &[3]);
    apply(
        &mut s,
        Command::Allocate {
            order_id: first.clone(),
        },
    );
    apply(
        &mut s,
        Command::Allocate {
            order_id: second.clone(),
        },
    );
    for (line, q) in [("line-0", 4), ("line-1", 2)] {
        apply(
            &mut s,
            Command::Pick {
                order_id: first.clone(),
                line_id: line.into(),
                location: "A-01".into(),
                lot: "LOT-1".into(),
                quantity: q,
            },
        );
    }
    apply(
        &mut s,
        Command::Pick {
            order_id: second.clone(),
            line_id: "line-0".into(),
            location: "A-01".into(),
            lot: "LOT-1".into(),
            quantity: 3,
        },
    );
    apply(
        &mut s,
        Command::Pack {
            order_id: first.clone(),
        },
    );
    let before = s.clone();
    assert!(s
        .execute(
            &Command::Unpick {
                order_id: first.clone(),
                line_id: "line-1".into(),
                location: "A-01".into(),
                lot: "LOT-1".into(),
                quantity: 3,
                reason: "Must not consume the other line's staging stock".into(),
            },
            "op"
        )
        .is_err());
    assert_eq!(s, before);
    apply(
        &mut s,
        Command::Unpick {
            order_id: first.clone(),
            line_id: "line-1".into(),
            location: "A-01".into(),
            lot: "LOT-1".into(),
            quantity: 2,
            reason: "Verify staging label".into(),
        },
    );
    let first_order = s
        .orders
        .iter()
        .find(|o| o.id == first)
        .expect("first order");
    assert_eq!(first_order.status, "picking");
    assert_eq!(
        first_order
            .allocations
            .iter()
            .find(|a| a.line_id == "line-0")
            .expect("line 0")
            .picked,
        4
    );
    assert_eq!(
        s.orders
            .iter()
            .find(|o| o.id == second)
            .expect("second order")
            .allocations[0]
            .picked,
        3
    );
    assert_eq!(stock(&s, "STAGE-01"), (7, 7));
    assert!(s
        .execute(
            &Command::Ship {
                order_id: first,
                carrier: "Carrier".into(),
                handoff_reference: "Invalidated pack".into(),
            },
            "op"
        )
        .is_err());
    s.validate().expect("all stock retains exact ownership");
}

#[test]
fn recovery_rejects_completed_work_and_unauthorized_roles() {
    for command in [
        Command::Unpick {
            order_id: "id".into(),
            line_id: "line".into(),
            location: "A-01".into(),
            lot: "LOT-1".into(),
            quantity: 1,
            reason: "Reason".into(),
        },
        Command::ResetAllocation {
            order_id: "id".into(),
            reason: "Reason".into(),
        },
    ] {
        assert!(command.allowed("admin"));
        assert!(command.allowed("operator"));
        assert!(!command.allowed("oms"));
        assert!(!command.allowed("viewer"));
    }
    let mut s = fixture(3);
    let oid = order(&mut s, "SO-COMPLETE", &[3]);
    apply(
        &mut s,
        Command::Allocate {
            order_id: oid.clone(),
        },
    );
    apply(
        &mut s,
        Command::Pick {
            order_id: oid.clone(),
            line_id: "line-0".into(),
            location: "A-01".into(),
            lot: "LOT-1".into(),
            quantity: 3,
        },
    );
    apply(
        &mut s,
        Command::Pack {
            order_id: oid.clone(),
        },
    );
    apply(
        &mut s,
        Command::Ship {
            order_id: oid.clone(),
            carrier: "Carrier".into(),
            handoff_reference: "SHIPPED".into(),
        },
    );
    let before = s.clone();
    assert!(s
        .execute(
            &Command::Unpick {
                order_id: oid.clone(),
                line_id: "line-0".into(),
                location: "A-01".into(),
                lot: "LOT-1".into(),
                quantity: 1,
                reason: "Too late".into()
            },
            "op"
        )
        .is_err());
    assert!(s
        .execute(
            &Command::ResetAllocation {
                order_id: oid,
                reason: "Too late".into()
            },
            "op"
        )
        .is_err());
    assert_eq!(s, before);
}

#[test]
fn opening_physical_balances_reconcile_new_movement_deltas_but_not_reservations() {
    let mut s = fixture(10);
    s.inventory_baseline = s.stocks.clone();
    s.receipt_baseline = s
        .receipts
        .iter()
        .map(|receipt| {
            (
                receipt.id.clone(),
                receipt
                    .lines
                    .iter()
                    .map(|line| (line.id.clone(), line.received))
                    .collect(),
            )
        })
        .collect();
    s.movements.clear();
    let oid = order(&mut s, "SO-BASELINE", &[3]);
    apply(
        &mut s,
        Command::Allocate {
            order_id: oid.clone(),
        },
    );
    apply(
        &mut s,
        Command::Pick {
            order_id: oid,
            line_id: "line-0".into(),
            location: "A-01".into(),
            lot: "LOT-1".into(),
            quantity: 2,
        },
    );
    assert_eq!(stock(&s, "A-01"), (8, 1));
    assert_eq!(stock(&s, "STAGE-01"), (2, 2));
    s.validate().expect("baseline plus deltas reconciles");
    s.stocks
        .iter_mut()
        .find(|b| b.location == "A-01")
        .expect("bin")
        .on_hand += 1;
    assert!(s.validate().is_err());
}
#[test]
fn rejects_fractional_quantities_unknown_fields_and_role_escalation() {
    assert!(serde_json::from_value::<Command>(serde_json::json!({"type":"pick","order_id":"1","line_id":"1","location":"A","lot":"L","quantity":1.5})).is_err());
    assert!(serde_json::from_value::<Command>(
        serde_json::json!({"type":"allocate","order_id":"1","tenant":"other"})
    )
    .is_err());
    assert!(!Command::CountStock {
        sku: "A".into(),
        lot: "L".into(),
        location: "A".into(),
        counted: 1,
        reason: "test".into()
    }
    .allowed("operator"));
    assert!(!Command::Allocate {
        order_id: "1".into()
    }
    .allowed("oms"));
    assert!(!Command::Allocate {
        order_id: "1".into()
    }
    .allowed("viewer"));
}
#[test]
fn synthetic_fixture_is_a_real_reconciled_workflow() {
    let s = demo_state();
    s.validate().expect("valid demo");
    assert_eq!(s.stocks.iter().map(|b| b.on_hand).sum::<i64>(), 1577);
    assert_eq!(s.orders.len(), 6);
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(128))]
    #[test]
    fn arbitrary_moves_and_counts_preserve_ledger(actions in prop::collection::vec((0u8..3,1i64..30),1..40)) {
        let mut s=fixture(50);
        for (kind,q) in actions {
            let c=match kind {0=>Command::MoveStock{sku:"SKU-A".into(),lot:"LOT-1".into(),from:"A-01".into(),to:"B-01".into(),quantity:q},1=>Command::MoveStock{sku:"SKU-A".into(),lot:"LOT-1".into(),from:"B-01".into(),to:"A-01".into(),quantity:q},_=>Command::CountStock{sku:"SKU-A".into(),lot:"LOT-1".into(),location:"A-01".into(),counted:q,reason:"Observed physical count".into()}};
            if let Ok((next,_))=s.execute(&c,"property:operator") {s=next;}
            prop_assert!(s.validate().is_ok());
            prop_assert!(s.stocks.iter().all(|b|b.on_hand>=b.reserved && b.reserved>=0));
            let received:i64=s.movements.iter().filter(|m|m.from.is_none()).map(|m|m.quantity).sum();
            let removed:i64=s.movements.iter().filter(|m|m.to.is_none()).map(|m|m.quantity).sum();
            prop_assert_eq!(s.stocks.iter().map(|b|b.on_hand).sum::<i64>(),received-removed);
        }
    }
    #[test]
    fn partial_picks_never_change_physical_warehouse_total(total in 2i64..50,split in 1i64..50) {
        let mut s=fixture(100);let oid=order(&mut s,"SO-PROP",&[total]);apply(&mut s,Command::Allocate{order_id:oid.clone()});
        let first=split.min(total-1);
        apply(&mut s,Command::Pick{order_id:oid.clone(),line_id:"line-0".into(),location:"A-01".into(),lot:"LOT-1".into(),quantity:first});
        prop_assert_eq!(s.stocks.iter().map(|b|b.on_hand).sum::<i64>(),100);
        prop_assert_eq!(s.stocks.iter().map(|b|b.reserved).sum::<i64>(),total);
        apply(&mut s,Command::Pick{order_id:oid,line_id:"line-0".into(),location:"A-01".into(),lot:"LOT-1".into(),quantity:total-first});
        prop_assert_eq!(stock(&s,"A-01"),(100-total,0));prop_assert_eq!(stock(&s,"STAGE-01"),(total,total));
    }
    #[test]
    fn arbitrary_pick_returns_preserve_stock_and_line_reservations(actions in prop::collection::vec((any::<bool>(),1i64..20),1..60)) {
        let mut s=fixture(50);
        let oid=order(&mut s,"SO-RECOVERY-PROP",&[20,10]);
        apply(&mut s,Command::Allocate{order_id:oid.clone()});
        for (returning,q) in actions {
            let command=if returning {
                Command::Unpick{order_id:oid.clone(),line_id:"line-0".into(),location:"A-01".into(),lot:"LOT-1".into(),quantity:q,reason:"Verified physical return".into()}
            } else {
                Command::Pick{order_id:oid.clone(),line_id:"line-0".into(),location:"A-01".into(),lot:"LOT-1".into(),quantity:q}
            };
            let before=s.clone();
            match s.execute(&command,"property:operator") {
                Ok((next,_))=>s=next,
                Err(_)=>prop_assert_eq!(&s,&before),
            }
            prop_assert!(s.validate().is_ok());
            prop_assert_eq!(s.stocks.iter().map(|b|b.on_hand).sum::<i64>(),50);
            prop_assert_eq!(s.stocks.iter().map(|b|b.reserved).sum::<i64>(),30);
            let order=s.orders.iter().find(|order|order.id==oid).expect("order");
            prop_assert_eq!(order.allocations.iter().find(|a|a.line_id=="line-1").expect("untouched line").picked,0);
            prop_assert!(s.reconciliation().iter().all(|row|row["ok"]==true));
        }
    }
}

#[test]
fn returns_preserve_exact_shipment_lines_and_enter_receiving_before_resale() {
    use stateset_warehouse::returns::ReturnLineInput;
    let mut s = fixture(10);
    let oid = order(&mut s, "RETURN-SOURCE", &[3, 2]);
    apply(
        &mut s,
        Command::Allocate {
            order_id: oid.clone(),
        },
    );
    for (line, q) in [("line-0", 3), ("line-1", 2)] {
        apply(
            &mut s,
            Command::Pick {
                order_id: oid.clone(),
                line_id: line.into(),
                location: "A-01".into(),
                lot: "LOT-1".into(),
                quantity: q,
            },
        );
    }
    apply(
        &mut s,
        Command::Pack {
            order_id: oid.clone(),
        },
    );
    apply(
        &mut s,
        Command::Ship {
            order_id: oid.clone(),
            carrier: "Carrier".into(),
            handoff_reference: "HANDOFF".into(),
        },
    );
    let shipped = s
        .orders
        .iter()
        .find(|o| o.id == oid)
        .expect("original")
        .clone();
    let receipt = apply(
        &mut s,
        Command::CreateReturn {
            reference: "RMA-1".into(),
            order_id: oid.clone(),
            reason: "Customer return; inspect before restocking".into(),
            lines: vec![
                ReturnLineInput {
                    id: "return-a".into(),
                    order_line_id: "line-0".into(),
                    lot: "LOT-1".into(),
                    quantity: 2,
                },
                ReturnLineInput {
                    id: "return-b".into(),
                    order_line_id: "line-1".into(),
                    lot: "LOT-1".into(),
                    quantity: 1,
                },
            ],
        },
    );
    assert_eq!(
        s.stocks.iter().map(|b| b.on_hand).sum::<i64>(),
        5,
        "authorization is not physical receipt"
    );
    for (line, q) in [("return-a", 2), ("return-b", 1)] {
        apply(
            &mut s,
            Command::Receive {
                receipt_id: receipt.clone(),
                line_id: line.into(),
                quantity: q,
                location: "RECV-01".into(),
            },
        );
    }
    assert_eq!(stock(&s, "RECV-01"), (3, 0));
    assert_eq!(
        s.orders.iter().find(|o| o.id == oid).expect("original"),
        &shipped
    );
    let before = s.clone();
    assert!(s
        .execute(
            &Command::CreateReturn {
                reference: "RMA-OVER".into(),
                order_id: oid.clone(),
                reason: "Over-return".into(),
                lines: vec![ReturnLineInput {
                    id: "bad".into(),
                    order_line_id: "line-0".into(),
                    lot: "LOT-1".into(),
                    quantity: 2
                },]
            },
            "operator"
        )
        .is_err());
    assert_eq!(s, before);
    apply(
        &mut s,
        Command::MoveStock {
            sku: "SKU-A".into(),
            lot: "LOT-1".into(),
            from: "RECV-01".into(),
            to: "A-01".into(),
            quantity: 3,
        },
    );
    assert_eq!(stock(&s, "A-01"), (8, 0));
    assert!(s.reconciliation().iter().all(|r| r["ok"] == true));
}

#[test]
fn replenishment_protects_reservations_pick_faces_and_shared_source_capacity() {
    use stateset_warehouse::replenishment::{plan, Rule};
    let mut s = fixture(10);
    apply(
        &mut s,
        Command::CreateLocation {
            code: "C-01".into(),
            zone: "Pick".into(),
            kind: "storage".into(),
        },
    );
    let oid = order(&mut s, "RESERVED", &[4]);
    apply(&mut s, Command::Allocate { order_id: oid });
    let before = s.clone();
    let rules = vec![
        Rule {
            sku: "SKU-A".into(),
            destination: "B-01".into(),
            minimum: 2,
            target: 4,
            source_zone: Some("A".into()),
        },
        Rule {
            sku: "SKU-A".into(),
            destination: "C-01".into(),
            minimum: 2,
            target: 4,
            source_zone: Some("A".into()),
        },
    ];
    let result = plan(&s, &rules).expect("plan");
    assert_eq!(
        result["tasks"]
            .as_array()
            .expect("tasks")
            .iter()
            .map(|t| t["quantity"].as_i64().expect("quantity"))
            .sum::<i64>(),
        6
    );
    assert_eq!(result["shortages"][1]["unfilled"], 2);
    assert_eq!(s, before, "planning cannot reserve or move stock");
    for task in result["tasks"].as_array().expect("tasks") {
        assert_eq!(task["command"]["from"], "A-01");
    }
    let mut invalid = rules.clone();
    invalid[0].destination = "RECV-01".into();
    assert!(plan(&s, &invalid).is_err());
    invalid = rules.clone();
    invalid[1] = invalid[0].clone();
    assert!(plan(&s, &invalid).is_err());
    invalid = rules;
    invalid[0].target = 1;
    assert!(plan(&s, &invalid).is_err());
}

#[test]
fn replenishment_skips_full_destination_lots_without_hiding_unfilled_target() {
    use stateset_warehouse::replenishment::{plan, Rule};
    let mut s = fixture(10);
    apply(
        &mut s,
        Command::CountStock {
            sku: "SKU-A".into(),
            lot: "LOT-2".into(),
            location: "A-01".into(),
            counted: 4,
            reason: "Synthetic count".into(),
        },
    );
    // A valid selective snapshot can contain another owner's fully held bin.
    let full = Stock {
        sku: "SKU-A".into(),
        lot: "LOT-1".into(),
        location: "B-01".into(),
        on_hand: 1_000_000_000,
        reserved: 1_000_000_000,
    };
    s.stocks.push(full.clone());
    s.inventory_baseline.push(full.clone());
    s.reservation_baseline.push(Stock { on_hand: 0, ..full });
    s.validate().expect("valid held inventory");
    let before = s.clone();
    let result = plan(
        &s,
        &[Rule {
            sku: "SKU-A".into(),
            destination: "B-01".into(),
            minimum: 1,
            target: 5,
            source_zone: Some("A".into()),
        }],
    )
    .expect("plan");
    assert_eq!(s, before, "planning cannot mutate stock");
    assert_eq!(result["tasks"].as_array().expect("planned tasks").len(), 1);
    assert_eq!(result["tasks"][0]["command"]["lot"], "LOT-2");
    assert_eq!(result["tasks"][0]["quantity"], 4);
    assert_eq!(result["shortages"][0]["unfilled"], 1);
    let command: Command =
        serde_json::from_value(result["tasks"][0]["command"].clone()).expect("typed move command");
    s.execute(&command, "test:operator")
        .expect("planned move respects bin/lot limit");
}

// Frozen pre-optimization proof algorithm: independent maps and key union.
// Keep this oracle separate from the optimized single-map accumulation.
fn reference_reconciliation(s: &State) -> Vec<serde_json::Value> {
    use std::collections::{BTreeMap, BTreeSet};
    type Key = (String, String, String);
    let mut physical: BTreeMap<Key, i64> = BTreeMap::new();
    let mut reserved: BTreeMap<Key, i64> = BTreeMap::new();
    for balance in &s.reservation_baseline {
        *reserved
            .entry((
                balance.sku.clone(),
                balance.lot.clone(),
                balance.location.clone(),
            ))
            .or_default() += balance.reserved;
    }
    for balance in &s.inventory_baseline {
        *physical
            .entry((
                balance.sku.clone(),
                balance.lot.clone(),
                balance.location.clone(),
            ))
            .or_default() += balance.on_hand;
    }
    for m in &s.movements {
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
    for o in &s.orders {
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
    for s in &s.stocks {
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
            serde_json::json!({"sku":key.0,"lot":key.1,"location":key.2,"on_hand":on_hand,"ledger":ledger,"reserved":held,"allocation_reserved":expected,"difference":on_hand-ledger,"reserved_difference":held-expected,"ok":on_hand==ledger && held==expected})
        }).collect()
}

fn assert_snapshot_contract(s: State, valid: bool) {
    let rows = reference_reconciliation(&s);
    assert_eq!(s.reconciliation(), rows);
    assert_eq!(s.validate().is_ok(), valid);
    let mut expected = serde_json::json!(s);
    expected["reconciliation"] = serde_json::json!({"version":s.version,"ok":valid,"rows":rows});
    let encoded = serde_json::to_vec(&s.into_snapshot()).expect("serialized snapshot");
    let snapshot: serde_json::Value = serde_json::from_slice(&encoded).expect("wire JSON");
    assert_eq!(snapshot, expected);
    assert!(snapshot.get("reservation_baseline").is_none());
}

#[test]
fn typed_snapshot_preserves_wire_contract_and_trusted_owner_holds() {
    let mut s = fixture(20);
    let oid = order(&mut s, "SO-SNAPSHOT", &[3]);
    apply(
        &mut s,
        Command::Allocate {
            order_id: oid.clone(),
        },
    );
    apply(
        &mut s,
        Command::Pick {
            order_id: oid.clone(),
            line_id: "line-0".into(),
            location: "A-01".into(),
            lot: "LOT-1".into(),
            quantity: 1,
        },
    );
    let balance = s
        .stocks
        .iter_mut()
        .find(|b| b.location == "A-01")
        .expect("source");
    balance.reserved += 2;
    s.reservation_baseline.push(Stock {
        on_hand: 0,
        reserved: 2,
        ..balance.clone()
    });
    assert_snapshot_contract(s, true);
    assert_snapshot_contract(demo_state(), true);
}

#[test]
fn typed_snapshot_keeps_invalid_state_diagnostics() {
    let mut s = fixture(20);
    let oid = order(&mut s, "SO-SNAPSHOT", &[3]);
    apply(&mut s, Command::Allocate { order_id: oid });
    let mut cases = Vec::new();
    let mut bad = s.clone();
    bad.products.clear();
    cases.push(bad);
    let mut bad = s.clone();
    bad.products.push(bad.products[0].clone());
    cases.push(bad);
    let mut bad = s.clone();
    bad.locations.push(bad.locations[0].clone());
    cases.push(bad);
    let mut bad = s.clone();
    bad.locations.retain(|l| l.code != "STAGE-01");
    cases.push(bad);
    let mut bad = s.clone();
    bad.receipts[0].lines[0].received -= 1;
    cases.push(bad);
    let mut bad = s.clone();
    bad.movements[0].receipt_line_id = Some("wrong-line".into());
    cases.push(bad);
    let mut bad = s.clone();
    bad.orders[0].allocations[0].line_id = "other-owner".into();
    cases.push(bad);
    let mut bad = s.clone();
    bad.stocks[0].on_hand += 1;
    cases.push(bad);
    let mut bad = s.clone();
    bad.stocks[0].reserved += 1;
    cases.push(bad);
    let mut bad = s.clone();
    bad.stocks.push(bad.stocks[0].clone());
    cases.push(bad);
    let mut bad = s.clone();
    bad.inventory_baseline.push(Stock {
        sku: "SKU-A".into(),
        lot: "missing".into(),
        location: "A-01".into(),
        on_hand: 2,
        reserved: 0,
    });
    cases.push(bad);
    for bad in cases {
        assert_snapshot_contract(bad, false);
    }
}

proptest! {
    #[test]
    fn optimized_proof_matches_prior_independent_maps(
        physical in -100i64..100, held in -100i64..100,
        opening in 0i64..100, foreign_hold in 0i64..100,
        unknown_lot in any::<bool>(), duplicate in any::<bool>(),
    ) {
        let mut s = fixture(20);
        s.stocks[0].on_hand = physical;
        s.stocks[0].reserved = held;
        if duplicate { s.stocks.push(s.stocks[0].clone()); }
        let lot = if unknown_lot { "missing" } else { "LOT-1" };
        s.inventory_baseline.push(Stock { sku: "SKU-A".into(), lot: lot.into(), location: "B-01".into(), on_hand: opening, reserved: 0 });
        s.reservation_baseline.push(Stock { sku: "SKU-A".into(), lot: lot.into(), location: "B-01".into(), on_hand: 0, reserved: foreign_hold });
        let expected = reference_reconciliation(&s);
        prop_assert_eq!(s.reconciliation(), expected);
    }
}

#[test]
fn receipt_cancellation_closes_only_unreceived_units_and_freezes_evidence() {
    let mut s = fixture(20);
    let rid = apply(
        &mut s,
        Command::CreateReceipt {
            reference: "PARTIAL-CANCEL".into(),
            supplier: "Supplier".into(),
            lines: vec![
                ReceiptInput {
                    id: "first".into(),
                    sku: "SKU-A".into(),
                    lot: "LOT-1".into(),
                    quantity: 5,
                },
                ReceiptInput {
                    id: "second".into(),
                    sku: "SKU-A".into(),
                    lot: "LOT-2".into(),
                    quantity: 3,
                },
            ],
        },
    );
    apply(
        &mut s,
        Command::Receive {
            receipt_id: rid.clone(),
            line_id: "first".into(),
            quantity: 2,
            location: "RECV-01".into(),
        },
    );
    let before = s.clone();
    apply(
        &mut s,
        Command::CancelReceipt {
            receipt_id: rid.clone(),
            reason: "Supplier will not ship the remainder".into(),
        },
    );
    assert_eq!(s.stocks, before.stocks);
    assert_eq!(s.movements, before.movements);
    let r = s.receipts.iter().find(|r| r.id == rid).expect("receipt");
    assert_eq!(r.status, "cancelled");
    assert_eq!(r.lines, before.receipts.last().expect("before").lines);
    let frozen = r.cancellation.as_ref().expect("cancellation");
    assert_eq!(frozen.lines["first"].cancelled, 3);
    assert_eq!(frozen.lines["second"].cancelled, 3);
    assert_eq!(frozen.actor, "test:operator");
    for command in [
        Command::Receive {
            receipt_id: rid.clone(),
            line_id: "first".into(),
            quantity: 1,
            location: "RECV-01".into(),
        },
        Command::CancelReceipt {
            receipt_id: rid.clone(),
            reason: "Again".into(),
        },
    ] {
        assert!(s.execute(&command, "test:operator").is_err());
    }
    for mutate in 0..5 {
        let mut bad = s.clone();
        let r = bad
            .receipts
            .iter_mut()
            .find(|r| r.id == rid)
            .expect("receipt");
        match mutate {
            0 => r.status = "partial".into(),
            1 => r.cancellation = None,
            2 => {
                r.cancellation
                    .as_mut()
                    .expect("proof")
                    .lines
                    .get_mut("first")
                    .expect("line")
                    .cancelled += 1;
            }
            3 => {
                r.cancellation
                    .as_mut()
                    .expect("proof")
                    .lines
                    .remove("second");
            }
            _ => r.cancellation.as_mut().expect("proof").cancelled_at = "invalid".into(),
        }
        assert!(bad.validate().is_err(), "corruption {mutate}");
    }
    assert_snapshot_contract(s, true);
    assert!(before
        .execute(
            &Command::CancelReceipt {
                receipt_id: rid,
                reason: " ".into()
            },
            "test:operator"
        )
        .is_err());
    assert!(before
        .execute(
            &Command::CancelReceipt {
                receipt_id: before.receipts[0].id.clone(),
                reason: "Already received".into()
            },
            "test:operator"
        )
        .is_err());
}

#[test]
fn partial_return_cancellation_releases_only_its_exact_unreceived_line_lot() {
    use stateset_warehouse::returns::ReturnLineInput;
    let mut s = fixture(20);
    let oid = order(&mut s, "RETURN-CANCEL-SOURCE", &[3, 2]);
    apply(
        &mut s,
        Command::Allocate {
            order_id: oid.clone(),
        },
    );
    for (line, q) in [("line-0", 3), ("line-1", 2)] {
        apply(
            &mut s,
            Command::Pick {
                order_id: oid.clone(),
                line_id: line.into(),
                location: "A-01".into(),
                lot: "LOT-1".into(),
                quantity: q,
            },
        );
    }
    apply(
        &mut s,
        Command::Pack {
            order_id: oid.clone(),
        },
    );
    apply(
        &mut s,
        Command::Ship {
            order_id: oid.clone(),
            carrier: "Carrier".into(),
            handoff_reference: "HANDOFF-CANCEL".into(),
        },
    );
    let shipment = s.orders[0].clone();
    let rid = apply(
        &mut s,
        Command::CreateReturn {
            reference: "RMA-CANCEL".into(),
            order_id: oid.clone(),
            reason: "Customer return".into(),
            lines: vec![
                ReturnLineInput {
                    id: "r0".into(),
                    order_line_id: "line-0".into(),
                    lot: "LOT-1".into(),
                    quantity: 3,
                },
                ReturnLineInput {
                    id: "r1".into(),
                    order_line_id: "line-1".into(),
                    lot: "LOT-1".into(),
                    quantity: 2,
                },
            ],
        },
    );
    for (line, q) in [("r0", 1), ("r1", 2)] {
        apply(
            &mut s,
            Command::Receive {
                receipt_id: rid.clone(),
                line_id: line.into(),
                quantity: q,
                location: "RECV-01".into(),
            },
        );
    }
    let physical = s.stocks.clone();
    apply(
        &mut s,
        Command::CancelReceipt {
            receipt_id: rid.clone(),
            reason: "Customer retained the last two units".into(),
        },
    );
    let again = |line: &str, q| Command::CreateReturn {
        reference: "RMA-REPLACE".into(),
        order_id: oid.clone(),
        reason: "New return authorization".into(),
        lines: vec![ReturnLineInput {
            id: "new".into(),
            order_line_id: line.into(),
            lot: "LOT-1".into(),
            quantity: q,
        }],
    };
    assert!(s.execute(&again("line-0", 3), "operator").is_err());
    assert!(s.execute(&again("line-1", 1), "operator").is_err());
    apply(&mut s, again("line-0", 2));
    assert_eq!(s.stocks, physical);
    assert_eq!(s.orders[0], shipment);
    let original = s
        .receipts
        .iter()
        .find(|r| r.id == rid)
        .expect("original RMA");
    assert_eq!(original.lines[0].quantity, 3);
    assert_eq!(original.lines[0].received, 1);
    assert_eq!(
        original.cancellation.as_ref().expect("closure").lines["r0"].cancelled,
        2
    );
    assert_snapshot_contract(s, true);
}

fn partial_cancellation_source() -> (State, String, String) {
    let mut state = fixture(7);
    let order = apply(
        &mut state,
        Command::CreateOrder {
            reference: "PARTIAL-CLOSE".into(),
            source_order_id: None,
            customer: "Synthetic customer".into(),
            destination: "Verified destination".into(),
            priority: "normal".into(),
            lines: vec![
                LineInput {
                    id: "first".into(),
                    sku: "SKU-A".into(),
                    quantity: 3,
                },
                LineInput {
                    id: "backorder".into(),
                    sku: "SKU-A".into(),
                    quantity: 5,
                },
            ],
        },
    );
    apply(
        &mut state,
        Command::AllocateAvailable {
            order_id: order.clone(),
        },
    );
    apply(
        &mut state,
        Command::Pick {
            order_id: order.clone(),
            line_id: "first".into(),
            location: "A-01".into(),
            lot: "LOT-1".into(),
            quantity: 3,
        },
    );
    let package = apply(
        &mut state,
        Command::PackShipment {
            order_id: order.clone(),
            reference: "PARTIAL-PACK".into(),
            lines: vec![stateset_warehouse::split_fulfillment::LineInput {
                line_id: "first".into(),
                location: "A-01".into(),
                lot: "LOT-1".into(),
                quantity: 3,
            }],
        },
    );
    apply(
        &mut state,
        Command::HandoffShipment {
            shipment_id: package.clone(),
            carrier: "Observed carrier".into(),
            handoff_reference: "ACTUAL-PICKUP".into(),
        },
    );
    (state, order, package)
}
#[test]
fn partial_cancellation_releases_storage_reservations_preserves_handoff_and_retains_returns() {
    let (mut state, order, _) = partial_cancellation_source();
    let original = state
        .orders
        .iter()
        .find(|o| o.id == order)
        .expect("order")
        .clone();
    let moves = state.movements.clone();
    apply(
        &mut state,
        Command::CancelRemainder {
            order_id: order.clone(),
            reason: "Customer cancelled all remaining demand".into(),
        },
    );
    let closed = state
        .orders
        .iter()
        .find(|o| o.id == order)
        .expect("closed order");
    assert_eq!(closed.status, "closed_partial");
    assert_eq!(closed.lines, original.lines);
    assert_eq!(
        (
            closed.shipped_at.as_ref(),
            &closed.carrier,
            &closed.handoff_reference
        ),
        (
            original.shipped_at.as_ref(),
            &original.carrier,
            &original.handoff_reference
        )
    );
    assert_eq!(
        state.movements, moves,
        "commercial cancellation invents no physical movement"
    );
    assert_eq!(state.stocks.iter().map(|s| s.on_hand).sum::<i64>(), 4);
    assert_eq!(state.stocks.iter().map(|s| s.reserved).sum::<i64>(), 0);
    let cancellation = closed
        .remainder_cancellation
        .as_ref()
        .expect("immutable closure");
    assert_eq!(cancellation.lines["first"].shipped, 3);
    assert_eq!(cancellation.lines["first"].cancelled, 0);
    assert_eq!(cancellation.lines["backorder"].ordered, 5);
    assert_eq!(cancellation.lines["backorder"].cancelled, 5);
    let return_command = Command::CreateReturn {
        reference: "PARTIAL-CLOSED-RMA".into(),
        order_id: order.clone(),
        reason: "Actual returned unit".into(),
        lines: vec![stateset_warehouse::returns::ReturnLineInput {
            id: "return-first".into(),
            order_line_id: "first".into(),
            lot: "LOT-1".into(),
            quantity: 1,
        }],
    };
    apply(&mut state, return_command);
    assert!(state
        .execute(
            &Command::CreateReturn {
                reference: "CANCELLED-DEMAND-RMA".into(),
                order_id: order,
                reason: "Invalid unshipped return".into(),
                lines: vec![stateset_warehouse::returns::ReturnLineInput {
                    id: "return-backorder".into(),
                    order_line_id: "backorder".into(),
                    lot: "LOT-1".into(),
                    quantity: 1
                }]
            },
            "test:operator"
        )
        .is_err());
}
#[test]
fn partial_cancellation_requires_unpacking_and_actual_unpick_before_commercial_close() {
    let (mut state, order, _) = partial_cancellation_source();
    apply(
        &mut state,
        Command::Pick {
            order_id: order.clone(),
            line_id: "backorder".into(),
            location: "A-01".into(),
            lot: "LOT-1".into(),
            quantity: 1,
        },
    );
    let package = apply(
        &mut state,
        Command::PackShipment {
            order_id: order.clone(),
            reference: "STILL-STAGED".into(),
            lines: vec![stateset_warehouse::split_fulfillment::LineInput {
                line_id: "backorder".into(),
                location: "A-01".into(),
                lot: "LOT-1".into(),
                quantity: 1,
            }],
        },
    );
    let close = Command::CancelRemainder {
        order_id: order.clone(),
        reason: "Customer cancellation".into(),
    };
    let before = state.clone();
    assert!(state.execute(&close, "test:operator").is_err());
    assert_eq!(state, before);
    apply(
        &mut state,
        Command::UnpackShipment {
            shipment_id: package,
            reason: "Cancelled remaining shipment".into(),
        },
    );
    assert!(
        state.execute(&close, "test:operator").is_err(),
        "unpacking alone does not return physical stock"
    );
    apply(
        &mut state,
        Command::Unpick {
            order_id: order,
            line_id: "backorder".into(),
            location: "A-01".into(),
            lot: "LOT-1".into(),
            quantity: 1,
            reason: "Physically returned from staging".into(),
        },
    );
    apply(&mut state, close);
    assert_eq!(state.stocks.iter().map(|s| s.reserved).sum::<i64>(), 0);
}
#[test]
fn partial_cancellation_is_terminal_but_never_relabels_unshipped_demand_as_a_completed_handoff() {
    let (mut state, order, package) = partial_cancellation_source();
    apply(
        &mut state,
        Command::CancelRemainder {
            order_id: order.clone(),
            reason: "Customer cancellation".into(),
        },
    );
    for command in [
        Command::AllocateAvailable {
            order_id: order.clone(),
        },
        Command::ReleaseUnpicked {
            order_id: order.clone(),
            reason: "Attempt to reopen".into(),
        },
        Command::CancelRemainder {
            order_id: order.clone(),
            reason: "Duplicate close".into(),
        },
        Command::HandoffShipment {
            shipment_id: package,
            carrier: "Other carrier".into(),
            handoff_reference: "Invalid second pickup".into(),
        },
        Command::ReportException {
            order_id: Some(order),
            category: "block".into(),
            detail: "Cannot block completed commercial work".into(),
        },
    ] {
        assert!(
            state.execute(&command, "test:operator").is_err(),
            "{command:?}"
        );
    }
    let mut forged = state.clone();
    forged.orders[0]
        .remainder_cancellation
        .as_mut()
        .expect("closure")
        .lines
        .get_mut("backorder")
        .expect("line")
        .cancelled = 4;
    assert!(forged.validate().is_err());
    forged = state.clone();
    forged.orders[0].status = "shipped".into();
    assert!(forged.validate().is_err());
    forged = state;
    forged.orders[0].remainder_cancellation = None;
    assert!(forged.validate().is_err());
}

#[test]
fn cancelled_demand_read_model_keeps_fully_cancelled_package_orders_closed_without_inventing_handoffs(
) {
    let mut state = fixture(7);
    let oid = apply(
        &mut state,
        Command::CreateOrder {
            reference: "CANCEL-ALL-PACKAGE-MODE".into(),
            source_order_id: None,
            customer: "Synthetic".into(),
            destination: "Test".into(),
            priority: "normal".into(),
            lines: vec![LineInput {
                id: "a".into(),
                sku: "SKU-A".into(),
                quantity: 8,
            }],
        },
    );
    apply(
        &mut state,
        Command::AllocateAvailable {
            order_id: oid.clone(),
        },
    );
    apply(
        &mut state,
        Command::CancelOrder {
            order_id: oid.clone(),
            reason: "Customer cancelled before physical picking".into(),
        },
    );
    let order = state
        .orders
        .iter()
        .find(|o| o.id == oid)
        .expect("cancelled order");
    assert_eq!(order.status, "cancelled");
    assert_eq!(
        stateset_warehouse::order_cancellation::cancelled(order, "a"),
        8
    );
    assert!(order.remainder_cancellation.is_none());
    assert!(order.shipped_at.is_none());
    assert_eq!(state.stocks.iter().map(|s| s.reserved).sum::<i64>(), 0);
}

fn lot_time(value: &str) -> chrono::DateTime<chrono::Utc> {
    chrono::DateTime::parse_from_rfc3339(value)
        .expect("fixed clock")
        .with_timezone(&chrono::Utc)
}
fn lot_apply(state: &mut State, command: Command, at: &str) -> String {
    let (next, result) = state
        .execute_at(&command, "test:quality", lot_time(at))
        .expect("valid lot transition");
    *state = next;
    result["entity_id"].as_str().expect("entity").into()
}
fn lot_order(state: &mut State, quantity: i64) -> String {
    apply(
        state,
        Command::CreateOrder {
            reference: format!("LOT-ORDER-{}", id()),
            source_order_id: None,
            customer: "Synthetic".into(),
            destination: "Test".into(),
            priority: "normal".into(),
            lines: vec![LineInput {
                id: "line".into(),
                sku: "SKU-A".into(),
                quantity,
            }],
        },
    )
}
#[test]
fn lot_expiry_excludes_unlabelled_required_stock_and_enforces_the_exact_offset_boundary() {
    use stateset_warehouse::lot_control::{availability_at, Availability};
    let mut state = fixture(7);
    let physical = state.stocks.clone();
    let ledger = state.movements.clone();
    lot_apply(
        &mut state,
        Command::SetProductExpiryPolicy {
            sku: "SKU-A".into(),
            required: true,
            reason: "This SKU requires verified expiry labels".into(),
        },
        "2026-10-20T11:00:00Z",
    );
    assert_eq!(
        availability_at(&state, "SKU-A", "LOT-1", lot_time("2026-10-20T11:00:00Z")),
        Availability::ExpiryMissing
    );
    let order = lot_order(&mut state, 1);
    assert!(state
        .execute_at(
            &Command::Allocate {
                order_id: order.clone()
            },
            "test:operator",
            lot_time("2026-10-20T11:00:00Z")
        )
        .is_err());
    lot_apply(
        &mut state,
        Command::RecordLotExpiry {
            sku: "SKU-A".into(),
            lot: "LOT-1".into(),
            expires_at: "2026-10-20T05:00:00-07:00".into(),
            reason: "Witnessed exact label cutoff and offset".into(),
        },
        "2026-10-20T11:00:00Z",
    );
    assert_eq!(state.stocks, physical);
    assert_eq!(state.movements, ledger);
    assert_eq!(
        availability_at(
            &state,
            "SKU-A",
            "LOT-1",
            lot_time("2026-10-20T11:59:59.999999Z")
        ),
        Availability::Available
    );
    assert_eq!(
        availability_at(&state, "SKU-A", "LOT-1", lot_time("2026-10-20T12:00:00Z")),
        Availability::Expired
    );
    assert!(state
        .execute_at(
            &Command::Allocate { order_id: order },
            "test:operator",
            lot_time("2026-10-20T12:00:00Z")
        )
        .is_err());
    assert!(state
        .execute_at(
            &Command::RecordLotExpiry {
                sku: "SKU-A".into(),
                lot: "NEW".into(),
                expires_at: "2026-10-20T12:00:00".into(),
                reason: "Missing offset".into()
            },
            "test:operator",
            lot_time("2026-10-20T11:00:00Z")
        )
        .is_err());
}
#[test]
fn lot_allocation_uses_earliest_expiry_before_bin_order_and_leaves_unlabelled_nonperishables_last()
{
    let mut state = fixture(7);
    apply(
        &mut state,
        Command::CountStock {
            sku: "SKU-A".into(),
            lot: "EARLY".into(),
            location: "B-01".into(),
            counted: 3,
            reason: "Synthetic observed stock".into(),
        },
    );
    lot_apply(
        &mut state,
        Command::RecordLotExpiry {
            sku: "SKU-A".into(),
            lot: "LOT-1".into(),
            expires_at: "2026-10-22T12:00:00Z".into(),
            reason: "Later label".into(),
        },
        "2026-10-20T10:00:00Z",
    );
    lot_apply(
        &mut state,
        Command::RecordLotExpiry {
            sku: "SKU-A".into(),
            lot: "EARLY".into(),
            expires_at: "2026-10-21T12:00:00Z".into(),
            reason: "Earlier label".into(),
        },
        "2026-10-20T10:00:00Z",
    );
    apply(
        &mut state,
        Command::CountStock {
            sku: "SKU-A".into(),
            lot: "UNLABELLED".into(),
            location: "A-01".into(),
            counted: 10,
            reason: "Nonperishable stock does not require an expiry".into(),
        },
    );
    for partial in [false, true] {
        let mut candidate = state.clone();
        let order = lot_order(&mut candidate, 5);
        lot_apply(
            &mut candidate,
            if partial {
                Command::AllocateAvailable {
                    order_id: order.clone(),
                }
            } else {
                Command::Allocate {
                    order_id: order.clone(),
                }
            },
            "2026-10-20T11:00:00Z",
        );
        let order = candidate
            .orders
            .iter()
            .find(|o| o.id == order)
            .expect("allocated");
        assert_eq!(
            order
                .allocations
                .iter()
                .map(|a| (a.lot.as_str(), a.quantity))
                .collect::<Vec<_>>(),
            vec![("EARLY", 3), ("LOT-1", 2)]
        );
    }
}
#[test]
fn lot_quarantine_holds_are_independent_and_release_never_overrides_expiry() {
    use stateset_warehouse::lot_control::{availability_at, Availability};
    let mut state = fixture(7);
    let stock = state.stocks.clone();
    lot_apply(
        &mut state,
        Command::RecordLotExpiry {
            sku: "SKU-A".into(),
            lot: "LOT-1".into(),
            expires_at: "2026-10-20T12:00:00Z".into(),
            reason: "Confirmed label".into(),
        },
        "2026-10-20T10:00:00Z",
    );
    let first = lot_apply(
        &mut state,
        Command::QuarantineLot {
            sku: "SKU-A".into(),
            lot: "LOT-1".into(),
            reason: "Damaged packaging".into(),
        },
        "2026-10-20T10:00:00Z",
    );
    let second = lot_apply(
        &mut state,
        Command::QuarantineLot {
            sku: "SKU-A".into(),
            lot: "LOT-1".into(),
            reason: "Independent inspection pending".into(),
        },
        "2026-10-20T10:00:00Z",
    );
    lot_apply(
        &mut state,
        Command::ReleaseLotHold {
            hold_id: first.clone(),
            reason: "First inspection resolved".into(),
        },
        "2026-10-20T11:00:00Z",
    );
    assert_eq!(
        availability_at(&state, "SKU-A", "LOT-1", lot_time("2026-10-20T11:00:00Z")),
        Availability::Quarantined
    );
    assert!(state
        .execute_at(
            &Command::ReleaseLotHold {
                hold_id: first,
                reason: "Cannot release another issue by repeating this hold".into()
            },
            "test:admin",
            lot_time("2026-10-20T11:00:00Z")
        )
        .is_err());
    lot_apply(
        &mut state,
        Command::ReleaseLotHold {
            hold_id: second,
            reason: "Second inspection resolved".into(),
        },
        "2026-10-20T12:00:00Z",
    );
    assert_eq!(
        availability_at(&state, "SKU-A", "LOT-1", lot_time("2026-10-20T12:00:00Z")),
        Availability::Expired
    );
    assert_eq!(state.stocks, stock);
    assert!(!Command::ReleaseLotHold {
        hold_id: "hold".into(),
        reason: "Reason".into()
    }
    .allowed("operator"));
    assert!(Command::QuarantineLot {
        sku: "SKU-A".into(),
        lot: "LOT-1".into(),
        reason: "Urgent hold".into()
    }
    .allowed("operator"));
}
#[test]
fn lot_quarantine_blocks_reserved_picks_but_preserves_reservations_and_physical_recovery() {
    let mut state = fixture(7);
    let order = lot_order(&mut state, 3);
    lot_apply(
        &mut state,
        Command::Allocate {
            order_id: order.clone(),
        },
        "2026-10-20T10:00:00Z",
    );
    let held = lot_apply(
        &mut state,
        Command::QuarantineLot {
            sku: "SKU-A".into(),
            lot: "LOT-1".into(),
            reason: "Inspection before picking".into(),
        },
        "2026-10-20T10:00:00Z",
    );
    let pick = Command::Pick {
        order_id: order.clone(),
        line_id: "line".into(),
        location: "A-01".into(),
        lot: "LOT-1".into(),
        quantity: 1,
    };
    assert!(state
        .execute_at(&pick, "test:operator", lot_time("2026-10-20T10:00:00Z"))
        .is_err());
    assert_eq!(state.stocks.iter().map(|s| s.reserved).sum::<i64>(), 3);
    lot_apply(
        &mut state,
        Command::ReleaseLotHold {
            hold_id: held,
            reason: "Inspection cleared".into(),
        },
        "2026-10-20T10:00:00Z",
    );
    lot_apply(&mut state, pick, "2026-10-20T10:00:00Z");
    lot_apply(
        &mut state,
        Command::QuarantineLot {
            sku: "SKU-A".into(),
            lot: "LOT-1".into(),
            reason: "New issue found in staging".into(),
        },
        "2026-10-20T10:00:00Z",
    );
    lot_apply(
        &mut state,
        Command::Unpick {
            order_id: order.clone(),
            line_id: "line".into(),
            location: "A-01".into(),
            lot: "LOT-1".into(),
            quantity: 1,
            reason: "Physically recovered held staging".into(),
        },
        "2026-10-20T10:00:00Z",
    );
    lot_apply(
        &mut state,
        Command::ResetAllocation {
            order_id: order,
            reason: "Release held reservation after recovery".into(),
        },
        "2026-10-20T10:00:00Z",
    );
    assert_eq!(state.stocks.iter().map(|s| s.on_hand).sum::<i64>(), 7);
    assert_eq!(state.stocks.iter().map(|s| s.reserved).sum::<i64>(), 0);
}

#[test]
fn empty_pack_observations_preserve_legacy_request_bytes_and_default_on_read() {
    use serde::Serialize;
    use stateset_warehouse::store::Request;
    use std::collections::BTreeMap;
    #[derive(Serialize)]
    struct Legacy<'a> {
        request_id: &'a str,
        expected_version: i64,
        #[serde(skip_serializing_if = "Option::is_none")]
        resource_versions: Option<BTreeMap<String, i64>>,
        command: &'a Command,
    }
    let command = Command::MoveStock {
        sku: "SKU-A".into(),
        lot: "LOT".into(),
        from: "A".into(),
        to: "B".into(),
        quantity: 12,
    };
    for resource_versions in [
        None,
        Some(BTreeMap::from([("[\"product\",\"SKU-A\"]".into(), 4)])),
    ] {
        let legacy = serde_json::to_vec(&Legacy {
            request_id: "original",
            expected_version: 7,
            resource_versions: resource_versions.clone(),
            command: &command,
        })
        .expect("isolated pack-unit fixture and original source evidence");
        let current = Request {
            request_id: "original".into(),
            expected_version: 7,
            resource_versions,
            serial_inputs: Vec::new(),
            unit_inputs: vec![],
            command: command.clone(),
        };
        assert_eq!(
            legacy,
            serde_json::to_vec(&current)
                .expect("isolated pack-unit fixture and original source evidence")
        );
        let decoded: Request = serde_json::from_slice(&legacy)
            .expect("isolated pack-unit fixture and original source evidence");
        assert!(decoded.unit_inputs.is_empty());
        assert_eq!(
            legacy,
            serde_json::to_vec(&decoded)
                .expect("isolated pack-unit fixture and original source evidence")
        );
    }
}
