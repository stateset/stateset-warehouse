//! Putaway, batch picking, cycle counts, saved replenishment, ZPL labels and
//! carrier labels through real HTTP routes and PostgreSQL. Every planned task
//! is executed as the ordinary journaled command it names, proving the plans
//! produce commands the server accepts. All data is synthetic.
use axum::{
    body::Body,
    http::{Request, StatusCode},
    Router,
};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use stateset_warehouse::{
    api::{self, App},
    domain,
    integrations::Connections,
    store::{Identity, Store},
};
use std::sync::Arc;
use tower::ServiceExt;

const W: &str = "wh-main";

struct Fixture {
    app: Router,
    store: Store,
    tenant: String,
}

impl Fixture {
    async fn new() -> Self {
        let url = std::env::var("WMS_TEST_DATABASE_URL").expect("isolated PostgreSQL test URL");
        assert!(url.contains("test"));
        let store = Store::connect(&url).await.expect("current migrations");
        let tenant = format!("planning-{}", domain::id());
        store.provision(&tenant, false).await.expect("tenant");
        let credentials = ["admin", "operator", "viewer"]
            .map(|role| {
                (
                    format!("synthetic-{role}-key-0000000000000000"),
                    Identity {
                        tenant: tenant.clone(),
                        actor: format!("synthetic:{role}"),
                        role: role.into(),
                    },
                )
            })
            .to_vec();
        let app = api::router(App {
            store: store.clone(),
            credentials: Arc::new(credentials),
            demo: false,
            connections: Connections::default(),
        });
        Self { app, store, tenant }
    }

    async fn call(
        &self,
        role: &str,
        method: &str,
        uri: &str,
        body: Option<Value>,
    ) -> (StatusCode, Value) {
        let request = Request::builder()
            .method(method)
            .uri(uri)
            .header(
                "authorization",
                format!("Bearer synthetic-{role}-key-0000000000000000"),
            )
            .header("content-type", "application/json")
            .body(body.map_or_else(Body::empty, |b| Body::from(b.to_string())))
            .expect("request");
        let response = self.app.clone().oneshot(request).await.expect("response");
        let status = response.status();
        let bytes = response
            .into_body()
            .collect()
            .await
            .expect("body")
            .to_bytes();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        )
    }

    async fn version(&self) -> i64 {
        self.store
            .state(&self.tenant, W)
            .await
            .expect("state")
            .version
    }

    /// Execute a canonical command, with the plan's reviewed resource versions
    /// when given, otherwise the current warehouse version.
    async fn command(&self, command: Value, resource_versions: Option<Value>) -> Value {
        let mut request = json!({
            "request_id": domain::id(),
            "expected_version": self.version().await,
            "command": command,
        });
        if let Some(versions) = resource_versions.filter(|v| !v.is_null()) {
            request["resource_versions"] = versions;
        }
        let (status, body) = self
            .call(
                "admin",
                "POST",
                &format!("/v1/warehouses/{W}/commands"),
                Some(request),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        body
    }

    async fn setup(&self) {
        for sku in ["SKU-A", "SKU-B"] {
            self.command(json!({"type":"create_product","sku":sku,"name":format!("Synthetic {sku}"),"category":"Synthetic","reorder_point":0}), None).await;
        }
        for (code, zone) in [("S-1", "Z1"), ("S-2", "Z1"), ("S-10", "Z1"), ("S-20", "Z2")] {
            self.command(
                json!({"type":"create_location","code":code,"zone":zone,"kind":"storage"}),
                None,
            )
            .await;
        }
        self.command(json!({"type":"count_stock","sku":"SKU-A","lot":"L1","location":"S-2","counted":4,"reason":"Synthetic opening"}), None).await;
        self.command(json!({"type":"count_stock","sku":"SKU-A","lot":"L1","location":"RECV-01","counted":5,"reason":"Synthetic arrival"}), None).await;
        self.command(json!({"type":"count_stock","sku":"SKU-B","lot":"L9","location":"RECV-01","counted":3,"reason":"Synthetic arrival"}), None).await;
    }

    async fn proof(&self) {
        let (status, report) = self
            .call(
                "admin",
                "GET",
                &format!("/v1/warehouses/{W}/reconciliation"),
                None,
            )
            .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(report["ok"], true, "{report}");
    }
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn putaway_plan_consolidates_by_lot_then_zone_and_its_moves_commit() {
    let f = Fixture::new().await;
    f.setup().await;
    let (status, plan) = f
        .call(
            "operator",
            "POST",
            &format!("/v1/warehouses/{W}/putaway/plan"),
            Some(json!({"zone":"Z2"})),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{plan}");
    let tasks = plan["tasks"].as_array().expect("tasks");
    assert_eq!(tasks.len(), 2, "{plan}");
    let a = tasks
        .iter()
        .find(|t| t["command"]["sku"] == "SKU-A")
        .expect("SKU-A task");
    assert_eq!(a["command"]["to"], "S-2", "same SKU and lot already there");
    assert_eq!(a["reason"], "consolidate_same_lot");
    assert_eq!(a["command"]["quantity"], 5);
    let b = tasks
        .iter()
        .find(|t| t["command"]["sku"] == "SKU-B")
        .expect("SKU-B task");
    assert_eq!(
        b["command"]["to"], "S-20",
        "requested zone wins for a new SKU"
    );
    assert_eq!(b["reason"], "empty_bin_in_requested_zone");
    // With every empty bin excluded, SKU-B either shares the least mixed
    // bin or, when mixing is disallowed, stays unplaced.
    let only_mixed = json!({"skus":["SKU-B"],"exclude":["S-1","S-10","S-20"]});
    let (_, mixed) = f
        .call(
            "operator",
            "POST",
            &format!("/v1/warehouses/{W}/putaway/plan"),
            Some(only_mixed.clone()),
        )
        .await;
    assert_eq!(mixed["tasks"][0]["command"]["to"], "S-2", "{mixed}");
    assert_eq!(mixed["tasks"][0]["reason"], "mixed_sku_bin");
    let mut strict = only_mixed;
    strict["allow_mixed"] = json!(false);
    let (_, strict) = f
        .call(
            "operator",
            "POST",
            &format!("/v1/warehouses/{W}/putaway/plan"),
            Some(strict),
        )
        .await;
    assert_eq!(strict["tasks"], json!([]), "{strict}");
    assert_eq!(strict["unplaced"][0]["sku"], "SKU-B");
    // Viewers cannot plan physical work.
    let (status, _) = f
        .call(
            "viewer",
            "POST",
            &format!("/v1/warehouses/{W}/putaway/plan"),
            Some(json!({})),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    for task in tasks {
        f.command(
            task["command"].clone(),
            Some(task["resource_versions"].clone()),
        )
        .await;
    }
    let (_, after) = f
        .call(
            "operator",
            "POST",
            &format!("/v1/warehouses/{W}/putaway/plan"),
            Some(json!({})),
        )
        .await;
    assert_eq!(after["tasks"], json!([]), "receiving is empty: {after}");
    f.proof().await;
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn batch_pick_plan_groups_orders_by_bin_and_every_pick_commits() {
    let f = Fixture::new().await;
    f.setup().await;
    let mut orders = vec![];
    for (i, priority) in ["normal", "urgent"].iter().enumerate() {
        let created = f
            .command(
                json!({"type":"create_order","reference":format!("SO-{i}"),"source_order_id":null,
            "customer":"Synthetic customer","destination":"1 Synthetic Way","priority":priority,
            "lines":[{"id":"line-a","sku":"SKU-A","quantity":2}]}),
                None,
            )
            .await;
        let id = created["result"]["entity_id"]
            .as_str()
            .expect("order")
            .to_string();
        f.command(json!({"type":"allocate","order_id":id}), None)
            .await;
        orders.push(id);
    }
    // Default selection ranks urgent first.
    let (status, plan) = f
        .call(
            "operator",
            "POST",
            &format!("/v1/warehouses/{W}/picking/batch-plan"),
            Some(json!({})),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{plan}");
    assert_eq!(
        plan["totes"][0]["order_id"], orders[1],
        "urgent order gets tote 1"
    );
    assert_eq!(plan["pick_count"], 2);
    // Both orders draw from the same bin, so they share one stop.
    let stops = plan["stops"].as_array().expect("stops");
    assert_eq!(stops.len(), 1, "{plan}");
    assert_eq!(stops[0]["total"], 4);
    for pick in stops[0]["picks"].as_array().expect("picks") {
        // Picks at one stop advance the same resource version, so each
        // later pick is sent against the refreshed warehouse version.
        f.command(pick["command"].clone(), None).await;
    }
    let (_, after) = f
        .call(
            "operator",
            "POST",
            &format!("/v1/warehouses/{W}/picking/batch-plan"),
            Some(json!({"order_ids":orders})),
        )
        .await;
    assert_eq!(after["pick_count"], 0);
    assert_eq!(
        after["held_orders"].as_array().map(Vec::len),
        Some(2),
        "{after}"
    );
    f.proof().await;
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn cycle_count_plan_is_blind_and_lists_uncounted_bins_first() {
    let f = Fixture::new().await;
    f.setup().await;
    f.command(
        json!({"type":"move_stock","sku":"SKU-A","lot":"L1","from":"S-2","to":"S-1","quantity":1}),
        None,
    )
    .await;
    let (status, plan) = f
        .call(
            "operator",
            "POST",
            &format!("/v1/warehouses/{W}/counts/plan"),
            Some(json!({})),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{plan}");
    let tasks = plan["tasks"].as_array().expect("tasks");
    // S-1 was never counted; the other bins were counted moments ago.
    assert_eq!(tasks.len(), 1, "{plan}");
    assert_eq!(tasks[0]["location"], "S-1");
    assert!(tasks[0]["last_counted_at"].is_null());
    assert!(
        tasks[0].get("expected_on_hand").is_none(),
        "blind by default"
    );
    let (_, open) = f
        .call(
            "operator",
            "POST",
            &format!("/v1/warehouses/{W}/counts/plan"),
            Some(json!({"blind":false})),
        )
        .await;
    assert_eq!(open["tasks"][0]["expected_on_hand"], 1);
    let (status, _) = f
        .call(
            "operator",
            "POST",
            &format!("/v1/warehouses/{W}/counts/plan"),
            Some(json!({"interval_days":[30,7,90]})),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    let mut command = tasks[0]["command"].clone();
    command["counted"] = json!(1);
    f.command(command, None).await;
    let (_, after) = f
        .call(
            "operator",
            "POST",
            &format!("/v1/warehouses/{W}/counts/plan"),
            Some(json!({})),
        )
        .await;
    assert_eq!(after["tasks"], json!([]), "{after}");
    f.proof().await;
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn saved_replenishment_rules_are_admin_only_audited_and_drive_due_work() {
    let f = Fixture::new().await;
    f.setup().await;
    let uri = format!("/v1/warehouses/{W}/replenishment/rules");
    let rules = json!({"rules":[{"sku":"SKU-A","destination":"S-1","minimum":2,"target":3,"source_zone":null}]});
    let (status, _) = f.call("operator", "PUT", &uri, Some(rules.clone())).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, body) = f.call("admin", "PUT", &uri, Some(rules)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (_, saved) = f.call("viewer", "GET", &uri, None).await;
    assert_eq!(saved["rules"][0]["target"], 3);
    let audits: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM wms_admin_audit WHERE tenant=$1 AND action='replenishment.rules'",
    )
    .bind(&f.tenant)
    .fetch_one(&f.store.pool)
    .await
    .expect("audit");
    assert_eq!(audits, 1);
    let (status, due) = f
        .call(
            "operator",
            "GET",
            &format!("/v1/warehouses/{W}/replenishment/due"),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{due}");
    let task = &due["tasks"][0];
    assert_eq!(task["command"]["to"], "S-1");
    assert_eq!(task["command"]["quantity"], 3);
    f.command(
        task["command"].clone(),
        Some(task["resource_versions"].clone()),
    )
    .await;
    let (_, after) = f
        .call(
            "operator",
            "GET",
            &format!("/v1/warehouses/{W}/replenishment/due"),
            None,
        )
        .await;
    assert_eq!(after["tasks"], json!([]), "{after}");
    let (status, _) = f
        .call("admin", "PUT", &uri, Some(json!({"rules":[{"sku":"NOPE","destination":"S-1","minimum":1,"target":1,"source_zone":null}]})))
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    let (_, cleared) = f
        .call("admin", "PUT", &uri, Some(json!({"rules":[]})))
        .await;
    assert_eq!(cleared["rules"], 0);
    f.proof().await;
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn labels_and_carrier_labels_cover_bins_packages_and_their_lifecycle() {
    let f = Fixture::new().await;
    f.setup().await;
    let created = f
        .command(
            json!({"type":"create_order","reference":"SO-LABEL","source_order_id":null,
        "customer":"Synthetic customer","destination":"1 Synthetic Way","priority":"normal",
        "lines":[{"id":"line-a","sku":"SKU-A","quantity":1}]}),
            None,
        )
        .await;
    let order = created["result"]["entity_id"]
        .as_str()
        .expect("order")
        .to_string();
    f.command(json!({"type":"allocate_available","order_id":order}), None)
        .await;
    let state = f.store.state(&f.tenant, W).await.expect("state");
    let allocation = state
        .orders
        .iter()
        .find(|o| o.id == order)
        .expect("order")
        .allocations[0]
        .clone();
    f.command(json!({"type":"pick","order_id":order,"line_id":"line-a","location":allocation.location,"lot":allocation.lot,"quantity":1}), None).await;
    let packed = f.command(json!({"type":"pack_shipment","order_id":order,"reference":"CARTON-1",
        "lines":[{"line_id":"line-a","location":allocation.location,"lot":allocation.lot,"quantity":1}]}), None).await;
    let package = packed["result"]["entity_id"]
        .as_str()
        .expect("package")
        .to_string();

    let labels = format!("/v1/warehouses/{W}/labels");
    let (status, printed) = f
        .call("viewer", "POST", &labels, Some(json!({"labels":[{"kind":"location","code":"S-1"},{"kind":"product","sku":"SKU-A"},{"kind":"package","id":package}]})))
        .await;
    assert_eq!(status, StatusCode::OK, "{printed}");
    let zpl = printed["zpl"].as_str().expect("zpl");
    assert_eq!(zpl.matches("^XA").count(), 3);
    assert!(
        zpl.contains("^FDS-1^FS")
            && zpl.contains("^FDSKU-A^FS")
            && zpl.contains(&format!("^FD{package}^FS"))
    );
    assert!(zpl.contains("Synthetic customer"));
    let (status, _) = f
        .call(
            "viewer",
            "POST",
            &labels,
            Some(json!({"labels":[{"kind":"location","code":"NOPE"}]})),
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = f
        .call("viewer", "POST", &labels, Some(json!({"labels":[]})))
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);

    let carrier = format!("/v1/warehouses/{W}/packages/{package}/carrier-labels");
    let label = json!({"carrier":"Synthetic Parcel","service":"Ground","tracking_number":"SYN123456789",
        "rate":{"amount":"12.34","currency":"USD"},"carton":{"weight_grams":850,"length_mm":300,"width_mm":200,"height_mm":100},
        "label":{"format":"zpl","data":"^XA^FO50,50^FDSYN123456789^FS^XZ"}});
    let (status, _) = f
        .call("viewer", "POST", &carrier, Some(label.clone()))
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, recorded) = f
        .call("operator", "POST", &carrier, Some(label.clone()))
        .await;
    assert_eq!(status, StatusCode::OK, "{recorded}");
    assert_eq!(recorded["rate"]["amount"], "12.34");
    let (status, _) = f
        .call("operator", "POST", &carrier, Some(label.clone()))
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "one active label per package");
    let (_, printed) = f
        .call(
            "viewer",
            "POST",
            &labels,
            Some(json!({"labels":[{"kind":"carrier_label","package_id":package}]})),
        )
        .await;
    assert!(printed["zpl"]
        .as_str()
        .expect("zpl")
        .starts_with("^XA^FO50,50^FDSYN123456789"));
    let id = recorded["id"].as_str().expect("label id");
    let (status, voided) = f
        .call(
            "operator",
            "POST",
            &format!("{carrier}/{id}/void"),
            Some(json!({"reason":"Synthetic reprint"})),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{voided}");
    assert_eq!(voided["voided"]["reason"], "Synthetic reprint");
    let (status, _) = f
        .call(
            "operator",
            "POST",
            &format!("{carrier}/{id}/void"),
            Some(json!({"reason":"Again"})),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT);
    let (status, _) = f
        .call("operator", "POST", &carrier, Some(label.clone()))
        .await;
    assert_eq!(status, StatusCode::OK, "replacement after void");
    // Labels are immutable apart from one void, even for raw SQL.
    let update =
        sqlx::query("UPDATE wms_carrier_labels SET tracking_number='FORGED' WHERE tenant=$1")
            .bind(&f.tenant)
            .execute(&f.store.pool)
            .await;
    assert!(update.is_err());
    // After handoff no new label can be attached.
    f.command(json!({"type":"handoff_shipment","shipment_id":package,"carrier":"Synthetic Parcel","handoff_reference":"SYN123456789"}), None).await;
    let (_, listed) = f.call("viewer", "GET", &carrier, None).await;
    assert_eq!(listed["labels"].as_array().map(Vec::len), Some(2));
    let active = listed["labels"]
        .as_array()
        .expect("labels")
        .iter()
        .find(|l| l["voided"].is_null())
        .expect("active")
        .clone();
    let (status, _) = f
        .call(
            "operator",
            "POST",
            &format!("{carrier}/{}/void", active["id"].as_str().expect("id")),
            Some(json!({"reason":"After handoff"})),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "voiding stays possible for records");
    let (status, _) = f.call("operator", "POST", &carrier, Some(label)).await;
    assert_eq!(status, StatusCode::CONFLICT, "package already handed off");
    f.proof().await;
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn gtins_scans_and_bin_capacity_are_reviewed_and_drive_putaway() {
    let f = Fixture::new().await;
    f.setup().await;
    let gtins = format!("/v1/warehouses/{W}/products/SKU-A/gtins");
    let (status, _) = f
        .call(
            "operator",
            "PUT",
            &gtins,
            Some(json!({"gtins":["9506000134352"]})),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, saved) = f
        .call(
            "admin",
            "PUT",
            &gtins,
            Some(json!({"gtins":["9506000134352"]})),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{saved}");
    assert_eq!(
        saved["gtins"],
        json!(["09506000134352"]),
        "normalized to GTIN-14"
    );
    let (status, _) = f
        .call(
            "admin",
            "PUT",
            &format!("/v1/warehouses/{W}/products/SKU-B/gtins"),
            Some(json!({"gtins":["09506000134352"]})),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "one product per GTIN");
    let (status, _) = f
        .call(
            "admin",
            "PUT",
            &gtins,
            Some(json!({"gtins":["9506000134353"]})),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "bad check digit");

    let parse = format!("/v1/warehouses/{W}/scans/parse");
    let scan = "]C1010950600013435217261231\u{1d}10L1";
    let (status, parsed) = f
        .call("viewer", "POST", &parse, Some(json!({"data": scan})))
        .await;
    assert_eq!(status, StatusCode::OK, "{parsed}");
    assert_eq!(parsed["sku"], "SKU-A");
    assert_eq!(parsed["lot"], "L1");
    assert_eq!(parsed["expiry_date"], "2026-12-31");
    let (_, unknown) = f
        .call(
            "viewer",
            "POST",
            &parse,
            Some(json!({"data":"(01)00012345678905(10)X"})),
        )
        .await;
    assert!(
        unknown["sku"].is_null(),
        "an unreviewed GTIN identifies nothing"
    );
    let (status, _) = f
        .call(
            "viewer",
            "POST",
            &parse,
            Some(json!({"data":"(01)00012345678906"})),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);

    // S-2 holds 4 units of SKU-A L1; a 6-unit limit leaves room for 2 of
    // the 5 waiting in receiving, so the rest goes to an empty bin.
    let (status, _) = f
        .call(
            "operator",
            "PUT",
            &format!("/v1/warehouses/{W}/locations/S-2/capacity"),
            Some(json!({"max_units":6})),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _) = f
        .call(
            "admin",
            "PUT",
            &format!("/v1/warehouses/{W}/locations/S-2/capacity"),
            Some(json!({"max_units":6})),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let (_, bins) = f
        .call(
            "viewer",
            "GET",
            &format!("/v1/warehouses/{W}/capacity"),
            None,
        )
        .await;
    assert_eq!(bins["bins"][0]["max_units"], 6);
    let (_, plan) = f
        .call(
            "operator",
            "POST",
            &format!("/v1/warehouses/{W}/putaway/plan"),
            Some(json!({"skus":["SKU-A"]})),
        )
        .await;
    let tasks = plan["tasks"].as_array().expect("tasks");
    assert_eq!(tasks[0]["command"]["to"], "S-2", "{plan}");
    assert_eq!(tasks[0]["command"]["quantity"], 2);
    assert_eq!(tasks[1]["command"]["quantity"], 3);
    for task in tasks {
        f.command(task["command"].clone(), None).await;
    }
    let audits: i64 = sqlx::query_scalar("SELECT count(*) FROM wms_admin_audit WHERE tenant=$1 AND action IN('product.gtins','location.capacity')")
        .bind(&f.tenant).fetch_one(&f.store.pool).await.expect("audit");
    assert_eq!(audits, 2);
    f.proof().await;
}
