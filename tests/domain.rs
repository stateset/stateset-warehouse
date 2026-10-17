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
