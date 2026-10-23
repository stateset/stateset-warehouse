//! The synthetic demo warehouse used by demo mode and tests.
use super::*;

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
            serial_controlled: false,
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
