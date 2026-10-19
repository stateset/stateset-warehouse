use super::{domain, observed, Fixture, Identity, Request};
use axum::{
    body::{to_bytes, Body},
    http::Request as HttpRequest,
    Router,
};
use serde_json::{json, Value};
use std::{sync::Arc, time::Duration};
use tower::ServiceExt;

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn long_canonical_count_result_descriptions_do_not_restrict_valid_literal_custody_keys() {
    let f = Fixture::new().await;
    let sku = "S".repeat(240);
    let bin = "B".repeat(240);
    f.plain(json!({"type":"create_product","sku":sku,"name":"Synthetic long-key product","category":"Synthetic","reorder_point":0,"serial_controlled":true})).await;
    f.plain(json!({"type":"create_location","code":bin,"zone":"Synthetic","kind":"storage"}))
        .await;
    let input = stateset_warehouse::serials::Input {
        target: vec![],
        sku: sku.clone(),
        serials: vec!["LITERAL-UNIT".into()],
    };
    let (_,result)=f.execute(json!({"type":"count_stock","sku":sku,"lot":"LOT","location":bin,"counted":1,"reason":"Actual bounded SKU/bin keys with a longer descriptive result"}),vec![input]).await;
    assert!(
        result["result"]["entity_id"]
            .as_str()
            .expect("canonical descriptive result")
            .len()
            > 240
    );
    let actual: String=sqlx::query_scalar("SELECT place->>'location' FROM wms_serial_units WHERE tenant=$1 AND sku=$2 AND serial='LITERAL-UNIT'")
        .bind(&f.who.tenant).bind(sku).fetch_one(&f.store.pool).await.expect("original valid literal custody key");
    assert_eq!(actual, bin);
    f.proof().await;
}

pub(super) async fn other_warehouse(f: &Fixture) {
    let create=Request {request_id:domain::id(),expected_version:0,resource_versions:None,unit_inputs:vec![],serial_inputs:vec![],command:serde_json::from_value(json!({"type":"create_warehouse","code":"OTHER","name":"Synthetic other facility","city":"Synthetic","timezone":"UTC"})).expect("new facility command")};
    f.store
        .execute(&f.who, "wh-other", &create)
        .await
        .expect("second isolated facility");
    let request=Request {request_id:domain::id(),expected_version:1,resource_versions:None,unit_inputs:vec![],serial_inputs:vec![],command:serde_json::from_value(json!({"type":"create_product","sku":"SERIAL","name":"Same synthetic product","category":"Synthetic","reorder_point":0,"serial_controlled":true})).expect("matching catalog mode")};
    f.store
        .execute(&f.who, "wh-other", &request)
        .await
        .expect("original tenant/SKU mode at a second facility");
}

pub(super) async fn at(
    f: &Fixture,
    warehouse: &str,
    command: Value,
    inputs: Vec<stateset_warehouse::serials::Input>,
) -> (Request, Value) {
    let request = Request {
        request_id: domain::id(),
        expected_version: f
            .store
            .state(&f.who.tenant, warehouse)
            .await
            .expect("current facility")
            .version,
        resource_versions: None,
        unit_inputs: vec![],
        serial_inputs: inputs,
        command: serde_json::from_value(command).expect("typed facility command"),
    };
    super::plan::verify(f, warehouse, &request).await;
    let result = f
        .store
        .execute(&f.who, warehouse, &request)
        .await
        .expect("atomic original facility command");
    (request, result)
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn modes_and_literal_identity_are_global_per_tenant_and_keep_original_catalog_source() {
    let f = Fixture::new().await;
    f.catalog().await;
    f.execute(json!({"type":"count_stock","sku":"SERIAL","lot":"LOT","location":"A","counted":1,"reason":"Original observed unit"}),observed(&[],&["GLOBAL"])).await;
    let original: Value = sqlx::query_scalar(
        "SELECT to_jsonb(m) FROM wms_serial_modes m WHERE tenant=$1 AND sku='SERIAL'",
    )
    .bind(&f.who.tenant)
    .fetch_one(&f.store.pool)
    .await
    .expect("immutable original catalog source");
    other_warehouse(&f).await;
    let after: Value = sqlx::query_scalar(
        "SELECT to_jsonb(m) FROM wms_serial_modes m WHERE tenant=$1 AND sku='SERIAL'",
    )
    .bind(&f.who.tenant)
    .fetch_one(&f.store.pool)
    .await
    .expect("same source after second facility setup");
    assert_eq!(after, original);
    let other = f
        .store
        .state(&f.who.tenant, "wh-other")
        .await
        .expect("other catalog");
    assert!(other.products[0].serial_controlled);
    let duplicate=Request {request_id:domain::id(),expected_version:other.version,resource_versions:None,unit_inputs:vec![],serial_inputs:observed(&[],&["GLOBAL"]),command:serde_json::from_value(json!({"type":"count_stock","sku":"SERIAL","lot":"LOT","location":"RECV-01","counted":1,"reason":"Cannot invent second custody of this unit"})).expect("typed observed count")};
    let before = f.facts().await;
    assert_eq!(
        f.store
            .execute(&f.who, "wh-other", &duplicate)
            .await
            .expect_err("one literal UID cannot exist in two facilities")
            .status,
        422
    );
    assert_eq!(f.facts().await, before);
    f.plain(json!({"type":"create_product","sku":"LEGACY","name":"Untracked existing product","category":"Synthetic","reorder_point":0})).await;
    f.plain(json!({"type":"count_stock","sku":"LEGACY","lot":"LOT","location":"A","counted":2,"reason":"Untracked original stock remains each-only"})).await;
    let changed=Request {request_id:domain::id(),expected_version:other.version,resource_versions:None,unit_inputs:vec![],serial_inputs:vec![],command:serde_json::from_value(json!({"type":"create_product","sku":"LEGACY","name":"Wrong new serial mode","category":"Synthetic","reorder_point":0,"serial_controlled":true})).expect("mode conflict command")};
    assert_eq!(
        f.store
            .execute(&f.who, "wh-other", &changed)
            .await
            .expect_err("another facility cannot relabel existing untracked inventory")
            .status,
        422
    );
    let independent = Fixture::new().await;
    independent.catalog().await;
    independent.execute(json!({"type":"count_stock","sku":"SERIAL","lot":"LOT","location":"A","counted":1,"reason":"Independent tenant's literal label"}),observed(&[],&["GLOBAL"])).await;
    f.proof().await;
    independent.proof().await;
}

pub(super) async fn http(
    app: &Router,
    method: &str,
    path: &str,
    token: &str,
    body: Option<Value>,
) -> (u16, Value) {
    let request = HttpRequest::builder()
        .method(method)
        .uri(path)
        .header("Authorization", format!("Bearer {token}"))
        .header("Content-Type", "application/json")
        .header("Host", "localhost:8088")
        .header("Accept", "application/json, text/event-stream")
        .header("MCP-Protocol-Version", "2025-11-25")
        .body(body.map_or_else(Body::empty, |v| Body::from(v.to_string())))
        .expect("synthetic HTTP request");
    let response = app
        .clone()
        .oneshot(request)
        .await
        .expect("actual router response");
    let status = response.status().as_u16();
    let bytes = to_bytes(response.into_body(), 4 * 1024 * 1024)
        .await
        .expect("bounded HTTP response");
    (
        status,
        serde_json::from_slice(&bytes).expect("JSON response"),
    )
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn http_and_mcp_share_serial_roles_actor_bound_recovery_and_exact_replay() {
    let f = Fixture::new().await;
    f.catalog().await;
    let operator = Identity {
        actor: "synthetic:http-operator".into(),
        role: "operator".into(),
        ..f.who.clone()
    };
    let app = stateset_warehouse::api::router(stateset_warehouse::api::App {
        store: f.store.clone(),
        demo: false,
        connections: stateset_warehouse::integrations::Connections::default(),
        credentials: Arc::new(vec![
            ("serial-admin".into(), f.who.clone()),
            ("serial-operator".into(), operator.clone()),
            (
                "serial-viewer".into(),
                Identity {
                    role: "viewer".into(),
                    ..operator.clone()
                },
            ),
            (
                "serial-outsider".into(),
                Identity {
                    tenant: format!("outsider-{}", domain::id()),
                    ..f.who.clone()
                },
            ),
        ]),
    });
    let receipt=f.plain(json!({"type":"create_receipt","reference":"Synthetic HTTP inbound","supplier":"Synthetic supplier","lines":[{"id":"IN","sku":"SERIAL","lot":"LOT","quantity":1}]})).await;
    let request=f.request(json!({"type":"receive","receipt_id":receipt,"line_id":"IN","location":"RECV-01","quantity":1}),observed(&[],&["HTTP-UID"])).await;
    for token in ["serial-viewer", "serial-outsider"] {
        let (status, _) = http(
            &app,
            "POST",
            "/v1/warehouses/wh-main/commands",
            token,
            Some(json!(&request)),
        )
        .await;
        assert_eq!(status, if token == "serial-viewer" { 403 } else { 404 });
    }
    let mut omitted = json!(&request);
    omitted
        .as_object_mut()
        .expect("request object")
        .remove("serial_inputs");
    assert_eq!(
        http(
            &app,
            "POST",
            "/v1/warehouses/wh-main/commands",
            "serial-operator",
            Some(omitted)
        )
        .await
        .0,
        422
    );
    let (status, saved) = http(
        &app,
        "POST",
        "/v1/warehouses/wh-main/commands",
        "serial-operator",
        Some(json!(&request)),
    )
    .await;
    assert_eq!(status, 200, "{saved}");
    assert_eq!(saved["replayed"], false);
    let (_, receipt) = http(
        &app,
        "GET",
        &format!("/v1/warehouses/wh-main/commands/{}", request.request_id),
        "serial-operator",
        None,
    )
    .await;
    assert_eq!(receipt["found"], true);
    let (_, receipt) = http(
        &app,
        "GET",
        &format!("/v1/warehouses/wh-main/commands/{}", request.request_id),
        "serial-admin",
        None,
    )
    .await;
    assert_eq!(receipt["found"], false);
    assert_eq!(
        http(
            &app,
            "POST",
            "/v1/warehouses/wh-main/commands",
            "serial-admin",
            Some(json!(&request))
        )
        .await
        .0,
        409
    );
    let move_request=f.request(json!({"type":"move_stock","sku":"SERIAL","lot":"LOT","from":"RECV-01","to":"A","quantity":1}),observed(&[],&["HTTP-UID"])).await;
    let call = json!({"jsonrpc":"2.0","id":7,"method":"tools/call","params":{"name":"wms_move_stock","arguments":{"warehouse_id":"wh-main","request":move_request}}});
    let (status, moved) = http(&app, "POST", "/mcp", "serial-operator", Some(call)).await;
    assert_eq!(status, 200, "{moved}");
    assert_eq!(
        moved["result"]["structuredContent"]["http_status"], 200,
        "{moved}"
    );
    assert_eq!(
        f.unit("HTTP-UID").await,
        json!({"status":"on_hand","warehouse_id":"wh-main","location":"A"})
    );
    let facts = f.facts().await;
    let (_, replay) = http(
        &app,
        "POST",
        "/v1/warehouses/wh-main/commands",
        "serial-operator",
        Some(json!(&request)),
    )
    .await;
    assert_eq!(replay["replayed"], true);
    assert_eq!(replay["result"], saved["result"]);
    assert_eq!(f.facts().await, facts);
    f.proof().await;
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn simultaneous_facility_births_wait_for_one_global_head_and_cannot_duplicate_a_uid() {
    let f = Fixture::new().await;
    f.catalog().await;
    other_warehouse(&f).await;
    let main=f.request(json!({"type":"count_stock","sku":"SERIAL","lot":"LOT","location":"A","counted":1,"reason":"Synthetic competing source observation"}),observed(&[],&["ONE-OWNER"])).await;
    let other=Request {request_id:domain::id(),expected_version:2,resource_versions:None,unit_inputs:vec![],serial_inputs:observed(&[],&["ONE-OWNER"]),command:serde_json::from_value(json!({"type":"count_stock","sku":"SERIAL","lot":"LOT","location":"RECV-01","counted":1,"reason":"Synthetic competing destination observation"})).expect("competing facility count")};
    let mut blocker = f
        .store
        .pool
        .begin()
        .await
        .expect("owned global-head test barrier");
    sqlx::query("SELECT version FROM wms_serial_heads WHERE tenant=$1 AND sku='SERIAL' FOR UPDATE")
        .bind(&f.who.tenant)
        .execute(&mut *blocker)
        .await
        .expect("hold exact shared serial head");
    let a_store = f.store.clone();
    let a_who = f.who.clone();
    let a_request = main.clone();
    let a = tokio::spawn(async move { a_store.execute(&a_who, "wh-main", &a_request).await });
    let b_store = f.store.clone();
    let b_who = f.who.clone();
    let b_request = other.clone();
    let b = tokio::spawn(async move { b_store.execute(&b_who, "wh-other", &b_request).await });
    tokio::time::timeout(Duration::from_secs(5),async {
        loop {
            let waiting: i64=sqlx::query_scalar("SELECT count(*) FROM pg_stat_activity WHERE datname=current_database() AND wait_event_type='Lock' AND query LIKE '%SELECT h.sku,h.version FROM wms_serial_heads h%FOR UPDATE%'")
                .fetch_one(&f.store.pool).await.expect("observe both actual commands waiting at the same head");
            if waiting>=2 {break;}
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }).await.expect("both commands must demonstrably overlap at admission");
    blocker
        .commit()
        .await
        .expect("release only the owned test barrier");
    let results = [
        a.await.expect("source task"),
        b.await.expect("destination task"),
    ];
    assert_eq!(
        results.iter().filter(|r| r.is_ok()).count(),
        1,
        "{results:?}"
    );
    assert_eq!(
        results
            .iter()
            .filter_map(|r| r.as_ref().err())
            .map(|e| e.status)
            .collect::<Vec<_>>(),
        vec![422]
    );
    let actual: (i64,i64,i64)=sqlx::query_as("SELECT (SELECT count(*) FROM wms_serial_units WHERE tenant=$1),(SELECT COALESCE(sum(on_hand),0)::bigint FROM wms_stock WHERE tenant=$1 AND sku='SERIAL'),version FROM wms_serial_heads WHERE tenant=$1 AND sku='SERIAL'")
        .bind(&f.who.tenant).fetch_one(&f.store.pool).await.expect("one identity, one physical unit and one global review");
    assert_eq!(actual, (1, 1, 1));
    let (warehouse, winner) = if results[0].is_ok() {
        ("wh-main", main)
    } else {
        ("wh-other", other)
    };
    let facts = f.facts().await;
    assert_eq!(
        f.store
            .execute(&f.who, warehouse, &winner)
            .await
            .expect("winner's original replay")["replayed"],
        true
    );
    assert_eq!(f.facts().await, facts);
    f.proof().await;
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn balanced_projection_corruption_fails_reconciliation_and_is_not_healed_by_a_move() {
    let f = Fixture::new().await;
    f.catalog().await;
    for reason in [
        "Original observed identity",
        "Unchanged full observed identity",
    ] {
        f.execute(json!({"type":"count_stock","sku":"SERIAL","lot":"LOT","location":"A","counted":1,"reason":reason}),observed(&[],&["ORIGINAL"])).await;
    }
    // Deliberate corruption in the owned synthetic DB. Stock, UID counts,
    // revisions and immutable history remain unchanged. Normal guards are
    // restored before committing this explicit bad-evidence fixture.
    let mut tx = f
        .store
        .pool
        .begin()
        .await
        .expect("owned forensic corruption");
    sqlx::raw_sql("ALTER TABLE wms_serial_units DISABLE TRIGGER wms_serial_unit_identity; ALTER TABLE wms_serial_units DISABLE TRIGGER wms_serial_unit_bound")
        .execute(&mut *tx).await.expect("bypass only the named guards in isolated evidence fixture");
    sqlx::query("UPDATE wms_serial_units SET last_event_id=(SELECT last_event_id FROM wms_serial_heads WHERE tenant=$1 AND sku='SERIAL') WHERE tenant=$1 AND sku='SERIAL' AND serial='ORIGINAL'")
        .bind(&f.who.tenant).execute(&mut *tx).await.expect("wrong source pointer, unchanged actual custody");
    sqlx::raw_sql("SET CONSTRAINTS ALL IMMEDIATE; ALTER TABLE wms_serial_units ENABLE TRIGGER wms_serial_unit_identity; ALTER TABLE wms_serial_units ENABLE TRIGGER wms_serial_unit_bound")
        .execute(&mut *tx).await.expect("restore exact source and identity guards");
    tx.commit()
        .await
        .expect("retain synthetic bad projection for independent detection");
    let report = f
        .store
        .reconciliation(&f.who.tenant, "wh-main")
        .await
        .expect("independent full reconciliation");
    assert_eq!(report["serials"]["physical_ok"], true);
    assert_eq!(report["serials"]["source_ok"], false);
    assert_eq!(report["ok"], false);
    let request=f.request(json!({"type":"move_stock","sku":"SERIAL","lot":"LOT","from":"A","to":"B","quantity":1}),observed(&[],&["ORIGINAL"])).await;
    let before = f.facts().await;
    assert_eq!(
        f.store
            .execute(&f.who, "wh-main", &request)
            .await
            .expect_err("a move must not silently replace an invalid current source")
            .status,
        503
    );
    assert_eq!(f.facts().await, before);
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn two_transfer_lines_preserve_exact_serial_loss_reversal_and_arrival_ownership() {
    let f = Fixture::new().await;
    f.catalog().await;
    other_warehouse(&f).await;
    f.execute(json!({"type":"count_stock","sku":"SERIAL","lot":"LOT","location":"A","counted":2,"reason":"Two original observed transfer units"}),observed(&[],&["FIRST","SECOND"])).await;
    let mut inputs = observed(&["LINE-1"], &["FIRST"]);
    inputs.extend(observed(&["LINE-2"], &["SECOND"]));
    let (original,dispatched)=f.execute(json!({"type":"dispatch_transfer","reference":"Synthetic two-line transfer","destination_warehouse_id":"wh-other","carrier":"Synthetic carrier","handoff_reference":"SYNTHETIC-DISPATCH","lines":[{"id":"LINE-1","sku":"SERIAL","lot":"LOT","location":"A","quantity":1},{"id":"LINE-2","sku":"SERIAL","lot":"LOT","location":"A","quantity":1}]}),inputs).await;
    let transfer = &dispatched["result"]["entity_id"];
    let source_version = f.state().await.version;
    let wrong=Request {request_id:domain::id(),expected_version:2,resource_versions:None,unit_inputs:vec![],serial_inputs:observed(&[],&["FIRST"]),command:serde_json::from_value(json!({"type":"record_transfer_loss","transfer_id":transfer,"line_id":"LINE-2","quantity":1,"incident_reference":"SYNTHETIC-WRONG-LINE","reason":"Cannot lend another line's original identity"})).expect("wrong original line")};
    let before = f.facts().await;
    assert_eq!(
        f.store
            .execute(&f.who, "wh-other", &wrong)
            .await
            .expect_err("same-SKU original lines remain distinct")
            .status,
        422
    );
    assert_eq!(f.facts().await, before);
    for (line, uid) in [("LINE-1", "FIRST"), ("LINE-2", "SECOND")] {
        let (_,lost)=at(&f,"wh-other",json!({"type":"record_transfer_loss","transfer_id":transfer,"line_id":line,"quantity":1,"incident_reference":format!("SYNTHETIC-{line}"),"reason":"Actual original unit confirmed missing"}),observed(&[],&[uid])).await;
        let before_physical: i64=sqlx::query_scalar("SELECT COALESCE(sum(on_hand),0)::bigint FROM wms_stock WHERE tenant=$1 AND sku='SERIAL'").bind(&f.who.tenant).fetch_one(&f.store.pool).await.expect("actual custody before recovery");
        at(&f,"wh-other",json!({"type":"reverse_transfer_loss","loss_id":lost["result"]["entity_id"],"quantity":1,"reason":"Exact original unit recovered; still awaiting arrival"}),observed(&[],&[uid])).await;
        let after_physical: i64=sqlx::query_scalar("SELECT COALESCE(sum(on_hand),0)::bigint FROM wms_stock WHERE tenant=$1 AND sku='SERIAL'").bind(&f.who.tenant).fetch_one(&f.store.pool).await.expect("reversal is not physical arrival");
        assert_eq!(after_physical, before_physical);
        at(&f,"wh-other",json!({"type":"receive_transfer","transfer_id":transfer,"line_id":line,"location":"RECV-01","quantity":1,"arrival_reference":format!("SYNTHETIC-ARRIVAL-{line}")}),observed(&[],&[uid])).await;
        assert_eq!(
            f.unit(uid).await,
            json!({"status":"on_hand","warehouse_id":"wh-other","location":"RECV-01"})
        );
    }
    assert_eq!(f.state().await.version, source_version);
    let facts = f.facts().await;
    assert_eq!(
        f.store
            .execute(&f.who, "wh-main", &original)
            .await
            .expect("original dispatch replay after destination recoveries")["replayed"],
        true
    );
    assert_eq!(f.facts().await, facts);
    f.proof().await;
    assert_eq!(
        f.store
            .reconciliation(&f.who.tenant, "wh-other")
            .await
            .expect("full destination reconciliation")["ok"],
        true
    );
}
