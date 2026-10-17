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
}
