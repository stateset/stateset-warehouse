//! Fast, database-free tests for the planners and label renderers. Every
//! planned task is executed through the real domain reducer, so a plan that
//! names a command the server would reject fails here in milliseconds.
use proptest::prelude::*;
use serde_json::{json, Value};
use stateset_warehouse::{batch_pick, cycle_counts, domain::*, labels, putaway, replenishment};
use std::collections::BTreeMap;

fn apply(s: &mut State, c: Command) -> String {
    let (next, result) = s
        .execute(&c, "test:planner")
        .expect("valid test transition");
    *s = next;
    result["entity_id"].as_str().expect("entity ID").into()
}

fn product(s: &mut State, sku: &str) {
    apply(
        s,
        Command::CreateProduct {
            serial_controlled: false,
            sku: sku.into(),
            name: format!("Synthetic {sku}"),
            category: "Test".into(),
            reorder_point: 0,
        },
    );
}

fn bin(s: &mut State, code: &str, zone: &str) {
    apply(
        s,
        Command::CreateLocation {
            code: code.into(),
            zone: zone.into(),
            kind: "storage".into(),
        },
    );
}

fn count(s: &mut State, sku: &str, lot: &str, location: &str, counted: i64) {
    apply(
        s,
        Command::CountStock {
            sku: sku.into(),
            lot: lot.into(),
            location: location.into(),
            counted,
            reason: "Synthetic count".into(),
        },
    );
}

fn warehouse() -> State {
    let mut s = State::empty(Warehouse {
        id: "test".into(),
        code: "TEST".into(),
        name: "Test".into(),
        city: "Test".into(),
        timezone: "UTC".into(),
    });
    for sku in ["SKU-A", "SKU-B", "SKU-C"] {
        product(&mut s, sku);
    }
    for (code, zone) in [("A-1", "Z1"), ("A-2", "Z1"), ("A-10", "Z1"), ("B-1", "Z2")] {
        bin(&mut s, code, zone);
    }
    s
}

/// Run each `{command}` task in order; return the final state.
fn execute_all(mut s: State, tasks: &[Value]) -> State {
    for task in tasks {
        let command: Command =
            serde_json::from_value(task["command"].clone()).expect("plan names a typed command");
        apply(&mut s, command);
    }
    s
}

fn order(s: &mut State, reference: &str, priority: &str, quantity: i64) -> String {
    let id = apply(
        s,
        Command::CreateOrder {
            reference: reference.into(),
            source_order_id: None,
            customer: "Synthetic".into(),
            destination: "Synthetic".into(),
            priority: priority.into(),
            lines: vec![LineInput {
                id: "line".into(),
                sku: "SKU-A".into(),
                quantity,
            }],
        },
    );
    apply(
        s,
        Command::Allocate {
            order_id: id.clone(),
        },
    );
    id
}

// ---------------------------------------------------------------- putaway

#[test]
fn putaway_prefers_same_lot_then_same_sku_then_empty_bins() {
    let mut s = warehouse();
    count(&mut s, "SKU-A", "L1", "A-2", 3);
    count(&mut s, "SKU-B", "L5", "A-10", 1);
    count(&mut s, "SKU-A", "L1", "RECV-01", 4);
    count(&mut s, "SKU-B", "L6", "RECV-01", 2);
    count(&mut s, "SKU-C", "L7", "RECV-01", 1);
    let plan = putaway::plan(&s, &putaway::PlanRequest::default(), &BTreeMap::new()).expect("plan");
    let tasks = plan["tasks"].as_array().expect("tasks");
    let to = |sku: &str| {
        let t = tasks
            .iter()
            .find(|t| t["command"]["sku"] == sku)
            .expect("task");
        (t["command"]["to"].clone(), t["reason"].clone())
    };
    assert_eq!(to("SKU-A"), (json!("A-2"), json!("consolidate_same_lot")));
    assert_eq!(to("SKU-B"), (json!("A-10"), json!("consolidate_same_sku")));
    assert_eq!(
        to("SKU-C"),
        (json!("A-1"), json!("empty_bin")),
        "lowest empty bin"
    );
    let after = execute_all(s, tasks);
    assert!(after.validate().is_ok());
    assert!(
        putaway::plan(&after, &putaway::PlanRequest::default(), &BTreeMap::new()).expect("replan")
            ["tasks"]
            .as_array()
            .expect("tasks")
            .is_empty()
    );
}

#[test]
fn putaway_never_reuses_one_empty_bin_for_two_skus_in_one_plan() {
    let mut s = warehouse();
    count(&mut s, "SKU-A", "L1", "RECV-01", 1);
    count(&mut s, "SKU-B", "L1", "RECV-01", 1);
    let plan = putaway::plan(&s, &putaway::PlanRequest::default(), &BTreeMap::new()).expect("plan");
    let bins: Vec<_> = plan["tasks"]
        .as_array()
        .expect("tasks")
        .iter()
        .map(|t| t["command"]["to"].clone())
        .collect();
    assert_eq!(bins, vec![json!("A-1"), json!("A-2")]);
}

#[test]
fn putaway_rejects_unknown_bins_skus_and_zones() {
    let s = warehouse();
    for request in [
        json!({"from":["A-1"]}),
        json!({"skus":["NOPE"]}),
        json!({"zone":"NOPE"}),
        json!({"surprise":true}),
    ] {
        let parsed: std::result::Result<putaway::PlanRequest, _> =
            serde_json::from_value(request.clone());
        let ok = parsed.is_ok_and(|r| putaway::plan(&s, &r, &BTreeMap::new()).is_ok());
        assert!(!ok, "{request} should be rejected");
    }
}

// ----------------------------------------------------------- batch picking

#[test]
fn batch_plan_ranks_urgent_first_and_groups_shared_bins() {
    let mut s = warehouse();
    count(&mut s, "SKU-A", "L1", "A-2", 2);
    count(&mut s, "SKU-A", "L1", "A-10", 2);
    let normal = order(&mut s, "SO-N", "normal", 2);
    let urgent = order(&mut s, "SO-U", "urgent", 2);
    let plan = batch_pick::plan(&s, &batch_pick::PlanRequest::default()).expect("plan");
    assert_eq!(plan["totes"][0]["order_id"], urgent.as_str());
    assert_eq!(plan["totes"][1]["order_id"], normal.as_str());
    let locations: Vec<_> = plan["stops"]
        .as_array()
        .expect("stops")
        .iter()
        .map(|s| s["location"].clone())
        .collect();
    assert_eq!(
        locations,
        vec![json!("A-2"), json!("A-10")],
        "natural bin order"
    );
}

#[test]
fn batch_plan_holds_blocked_orders_and_rejects_bad_requests() {
    let mut s = warehouse();
    count(&mut s, "SKU-A", "L1", "A-1", 2);
    let id = order(&mut s, "SO-B", "normal", 1);
    apply(
        &mut s,
        Command::ReportException {
            order_id: Some(id.clone()),
            category: "damage".into(),
            detail: "Synthetic blocking exception".into(),
        },
    );
    let plan = batch_pick::plan(
        &s,
        &batch_pick::PlanRequest {
            order_ids: vec![id.clone()],
            ..Default::default()
        },
    )
    .expect("plan");
    assert_eq!(plan["held_orders"][0]["reason"], "blocking_exception");
    assert_eq!(plan["pick_count"], 0);
    for request in [
        batch_pick::PlanRequest {
            order_ids: vec![id.clone(), id.clone()],
            ..Default::default()
        },
        batch_pick::PlanRequest {
            max_orders: Some(0),
            ..Default::default()
        },
        batch_pick::PlanRequest {
            max_orders: Some(51),
            ..Default::default()
        },
        batch_pick::PlanRequest {
            order_ids: vec!["missing".into()],
            ..Default::default()
        },
    ] {
        assert!(batch_pick::plan(&s, &request).is_err());
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(48))]
    /// Whatever the order sizes and priorities, every planned pick executes,
    /// and picking the whole plan leaves nothing to pick.
    #[test]
    fn every_planned_pick_executes_and_the_batch_completes(
        orders in prop::collection::vec((1i64..=4, 0usize..3), 1..6)
    ) {
        let mut s = warehouse();
        count(&mut s, "SKU-A", "L1", "A-1", 10);
        count(&mut s, "SKU-A", "L2", "A-2", 10);
        for (i, (quantity, p)) in orders.iter().enumerate() {
            order(&mut s, &format!("SO-{i}"), ["normal", "high", "urgent"][*p], *quantity);
        }
        let plan = batch_pick::plan(&s, &batch_pick::PlanRequest::default()).expect("plan");
        let picks: Vec<Value> = plan["stops"].as_array().expect("stops").iter()
            .flat_map(|stop| stop["picks"].as_array().expect("picks").clone()).collect();
        let planned: i64 = picks.iter().map(|p| p["quantity"].as_i64().expect("quantity")).sum();
        prop_assert_eq!(planned, orders.iter().map(|o| o.0).sum::<i64>());
        let after = execute_all(s, &picks);
        prop_assert!(after.validate().is_ok());
        let again = batch_pick::plan(&after, &batch_pick::PlanRequest::default()).expect("replan");
        prop_assert_eq!(again["pick_count"].as_i64(), Some(0));
    }
}

// ------------------------------------------------------------ cycle counts

#[test]
fn cycle_counts_are_blind_and_honor_class_intervals() {
    let mut s = warehouse();
    count(&mut s, "SKU-A", "L1", "A-1", 5);
    count(&mut s, "SKU-B", "L1", "A-2", 5);
    let now = chrono::Utc::now();
    let inputs = cycle_counts::Inputs {
        picked: BTreeMap::from([("SKU-A".to_string(), 100)]),
        last_counted: BTreeMap::from([
            (
                ("SKU-A".into(), "L1".into(), "A-1".into()),
                now - chrono::Duration::days(8),
            ),
            (
                ("SKU-B".into(), "L1".into(), "A-2".into()),
                now - chrono::Duration::days(8),
            ),
        ]),
        now,
    };
    let plan =
        cycle_counts::plan(&s, &cycle_counts::PlanRequest::default(), &inputs).expect("plan");
    // SKU-A is class A (7-day interval): due. SKU-B is never picked, class C: not due.
    let tasks = plan["tasks"].as_array().expect("tasks");
    assert_eq!(tasks.len(), 1, "{plan}");
    assert_eq!(tasks[0]["sku"], "SKU-A");
    assert_eq!(tasks[0]["class"], "A");
    assert!(tasks[0].get("expected_on_hand").is_none());
    let mut command = tasks[0]["command"].clone();
    command["counted"] = json!(5);
    let command: Command = serde_json::from_value(command).expect("count command");
    apply(&mut s, command);
}

// ------------------------------------------------------------ replenishment

#[test]
fn replenishment_tasks_refill_the_face_and_execute() {
    let mut s = warehouse();
    count(&mut s, "SKU-A", "L1", "A-10", 9);
    let rules = vec![replenishment::Rule {
        sku: "SKU-A".into(),
        destination: "A-1".into(),
        minimum: 2,
        target: 5,
        source_zone: None,
    }];
    let plan = replenishment::plan(&s, &rules).expect("plan");
    let tasks = plan["tasks"].as_array().expect("tasks");
    assert_eq!(tasks.len(), 1);
    assert_eq!(tasks[0]["command"]["quantity"], 5);
    let after = execute_all(s, tasks);
    assert!(
        replenishment::plan(&after, &rules).expect("replan")["tasks"]
            .as_array()
            .expect("tasks")
            .is_empty()
    );
}

// ------------------------------------------------------------------ labels

#[test]
fn labels_encode_exact_values_and_refuse_unknown_records() {
    let s = warehouse();
    let bin = labels::location(&s, "A-10").expect("bin label");
    assert!(bin.starts_with("^XA^CI28") && bin.ends_with("^XZ\n"));
    assert!(bin.contains("^FDA-10^FS"));
    let product = labels::product(&s, "SKU-A").expect("product label");
    assert!(product.contains("^FDSKU-A^FS") && product.contains("Synthetic SKU-A"));
    assert!(labels::location(&s, "NOPE").is_err());
    assert!(labels::product(&s, "NOPE").is_err());
    let serial = labels::serial("SKU-A", "Unit~A", "L1", "on_hand").expect("serial label");
    assert!(
        serial.contains("^FDUnit_7EA^FS"),
        "tilde escaped, not dropped"
    );
}

#[test]
fn putaway_splits_across_bins_when_capacity_is_reached() {
    use stateset_warehouse::master_data::Capacity;
    let mut s = warehouse();
    count(&mut s, "SKU-A", "L1", "A-2", 4);
    count(&mut s, "SKU-A", "L1", "RECV-01", 5);
    let capacity = BTreeMap::from([(
        "A-2".to_string(),
        Capacity {
            max_units: 6,
            max_skus: None,
        },
    )]);
    let plan = putaway::plan(&s, &putaway::PlanRequest::default(), &capacity).expect("plan");
    let tasks = plan["tasks"].as_array().expect("tasks");
    let moves: Vec<_> = tasks
        .iter()
        .map(|t| (t["command"]["to"].clone(), t["command"]["quantity"].clone()))
        .collect();
    assert_eq!(
        moves,
        vec![(json!("A-2"), json!(2)), (json!("A-1"), json!(3))],
        "{plan}"
    );
    assert_eq!(tasks[0]["capacity_limited"], true);
    let after = execute_all(s, tasks);
    assert!(after.validate().is_ok());
}

#[test]
fn putaway_respects_distinct_sku_limits_and_full_bins() {
    use stateset_warehouse::master_data::Capacity;
    let mut s = warehouse();
    count(&mut s, "SKU-B", "L1", "A-1", 1);
    count(&mut s, "SKU-A", "L1", "RECV-01", 2);
    // Every other bin is full; A-1 allows only one SKU, so mixing is refused.
    let capacity = BTreeMap::from([
        (
            "A-1".to_string(),
            Capacity {
                max_units: 100,
                max_skus: Some(1),
            },
        ),
        (
            "A-2".to_string(),
            Capacity {
                max_units: 1,
                max_skus: None,
            },
        ),
        (
            "A-10".to_string(),
            Capacity {
                max_units: 1,
                max_skus: None,
            },
        ),
        (
            "B-1".to_string(),
            Capacity {
                max_units: 1,
                max_skus: None,
            },
        ),
    ]);
    count(&mut s, "SKU-C", "L1", "A-2", 1);
    count(&mut s, "SKU-C", "L1", "A-10", 1);
    count(&mut s, "SKU-C", "L1", "B-1", 1);
    let plan = putaway::plan(&s, &putaway::PlanRequest::default(), &capacity).expect("plan");
    let a = plan["tasks"]
        .as_array()
        .expect("tasks")
        .iter()
        .any(|t| t["command"]["sku"] == "SKU-A");
    assert!(!a, "{plan}");
    assert_eq!(plan["unplaced"][0]["sku"], "SKU-A");
    assert_eq!(plan["unplaced"][0]["quantity"], 2);
}
