//! Authenticated serial review on an owned synthetic database. Fault fixtures
//! deliberately retain the original observations while bypassing named guards.
use super::extended::{at, http, other_warehouse};
use super::*;
use axum::Router;
use std::sync::Arc;

fn app(f: &Fixture) -> Router {
    stateset_warehouse::api::router(stateset_warehouse::api::App {
        store: f.store.clone(),
        demo: false,
        connections: stateset_warehouse::integrations::Connections::default(),
        credentials: Arc::new(vec![
            (
                "reader".into(),
                Identity {
                    role: "viewer".into(),
                    ..f.who.clone()
                },
            ),
            (
                "worker".into(),
                Identity {
                    role: "operator".into(),
                    ..f.who.clone()
                },
            ),
            (
                "other".into(),
                Identity {
                    tenant: format!("outsider-{}", domain::id()),
                    ..f.who.clone()
                },
            ),
        ]),
    })
}
fn url(path: &str, pairs: &[(&str, String)]) -> String {
    let mut u = reqwest::Url::parse(&format!("http://localhost{path}")).expect("local test URL");
    u.query_pairs_mut()
        .extend_pairs(pairs.iter().map(|(k, v)| (*k, v.as_str())));
    format!("{}?{}", u.path(), u.query().unwrap_or(""))
}
async fn get(app: &Router, path: &str, pairs: &[(&str, String)]) -> (u16, Value) {
    http(app, "GET", &url(path, pairs), "reader", None).await
}
async fn count(f: &Fixture, labels: &[&str], reason: &str) {
    f.execute(json!({"type":"count_stock","sku":"SERIAL","lot":"LOT","location":"A","counted":labels.len(),"reason":reason}),observed(&[],labels)).await;
}
fn sku() -> Vec<(&'static str, String)> {
    vec![("sku", "SERIAL".into())]
}

pub(super) async fn verify_large_original_pages(f: &Fixture, first: &str, units: i64) {
    let app = app(f);
    let facts = f.facts().await;
    for (path, query, expected_total) in [
        (
            "/v1/serials",
            vec![("sku", "SERIAL".into()), ("limit", "50".into())],
            units,
        ),
        (
            "/v1/serial-history",
            vec![
                ("sku", "SERIAL".into()),
                ("serial", first.into()),
                ("limit", "50".into()),
            ],
            1,
        ),
        (
            "/v1/serial-observations",
            vec![("sku", "SERIAL".into()), ("limit", "50".into())],
            units,
        ),
    ] {
        let (status, page) = get(&app, path, &query).await;
        assert_eq!(status, 200, "{path}: {page}");
        assert_eq!(page["page_source_ok"], true, "{path}: {page}");
        assert_eq!(page["total_records"], expected_total);
        assert_eq!(
            page["items"]
                .as_array()
                .expect("bounded original records")
                .len(),
            expected_total.min(50) as usize
        );
    }
    assert_eq!(
        f.facts().await,
        facts,
        "large source reads changed evidence"
    );
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn bounded_pages_validate_2048_identity_count_sources_at_normal_sql_timeouts() {
    let f = Fixture::new().await;
    f.catalog().await;
    let labels: Vec<String> = (0..2048).map(|n| format!("Large-{n:04}")).collect();
    let refs: Vec<&str> = labels.iter().map(String::as_str).collect();
    count(&f, &refs, "Complete synthetic 2048-identity source").await;
    verify_large_original_pages(&f, refs[0], 2048).await;
    let report = f
        .store
        .reconciliation(&f.who.tenant, "wh-main")
        .await
        .expect("complete production reconciliation at the normal SQL deadline");
    assert_eq!(report["ok"], true, "{report}");
    assert_eq!(report["serials"]["source_ok"], true);
    assert_eq!(report["serials"]["physical_ok"], true);

    // Preparing an unchanged complete count must validate the shared original
    // event once, while retaining every selected label and the normal timeout.
    count(&f, &refs, "Unchanged complete 2048-identity recount").await;
    let observations: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM wms_serial_observations WHERE tenant=$1 AND jsonb_array_length(serials)=2048",
    )
    .bind(&f.who.tenant)
    .fetch_one(&f.store.pool)
    .await
    .expect("both complete original observations retained");
    assert_eq!(observations, 2);

    // Independently check every projection with the original standalone unit
    // predicate. A single all-label SELECT repeats the entire original count
    // source for each unit and exceeds the SQL deadline; bounded statements
    // retain every label and the normal timeout in one read-only snapshot.
    // The 512-unit and damaged-source tests retain the complete legacy event
    // oracle; production reconciliation above also checks complete event sets.
    let mut tx = f.store.pool.begin().await.expect("independent audit");
    sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY")
        .execute(&mut *tx)
        .await
        .expect("one stable read-only audit snapshot");
    for batch in labels.chunks(16) {
        let (found, valid): (i64, bool) = sqlx::query_as(
            "SELECT count(*),COALESCE(bool_and(wms_serial_unit_bound(u) AND wms_serial_place_references_valid(u.tenant,u.sku,u.lot,u.place)),false) FROM wms_serial_units u WHERE tenant=$1 AND sku='SERIAL' AND serial=ANY($2)",
        )
        .bind(&f.who.tenant)
        .bind(batch)
        .fetch_one(&mut *tx)
        .await
        .expect("bounded original unit predicates at the unchanged deadline");
        assert_eq!(found, batch.len() as i64);
        assert!(valid, "original source failed for {batch:?}");
    }
    tx.commit().await.expect("read-only audit complete");
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn current_and_lineage_reads_bind_literal_cursors_tenant_roles_and_mcp() {
    let f = Fixture::new().await;
    f.catalog().await;
    let app = app(&f);
    let (status, empty) = get(&app, "/v1/serials", &sku()).await;
    assert_eq!(status, 200, "{empty}");
    assert_eq!(empty["review"], 0);
    assert_eq!(empty["total_records"], 0);
    count(&f, &["One", "one"], "Original case-sensitive labels").await;
    f.execute(
        json!({"type":"move_stock","sku":"SERIAL","lot":"LOT","from":"A","to":"B","quantity":1}),
        observed(&[], &["One"]),
    )
    .await;
    f.plain(json!({"type":"create_product","sku":"UNTRACKED","name":"Synthetic legacy product","category":"Synthetic","reorder_point":0})).await;
    assert_eq!(
        get(&app, "/v1/serials", &[("sku", "UNTRACKED".into())])
            .await
            .0,
        422
    );
    let (_, unknown) = get(
        &app,
        "/v1/serials",
        &[("sku", "SERIAL".into()), ("serial", "ONE".into())],
    )
    .await;
    assert_eq!(unknown["total_records"], 0);
    assert_eq!(
        http(&app, "GET", "/v1/serials?sku=SERIAL", "worker", None)
            .await
            .0,
        200
    );
    let before = f.facts().await;
    let mut q = sku();
    q.push(("limit", "1".into()));
    let (status, first) = get(&app, "/v1/serials", &q).await;
    assert_eq!(status, 200, "{first}");
    assert_eq!(first["total_records"], 2);
    assert_eq!(first["items"][0]["serial"], "One");
    assert_eq!(first["items"][0]["place"]["location"], "B");
    assert_eq!(first["page_source_ok"], true);
    assert!(first["items"][0]["last_source"]["recorded_at"].is_string());
    let cursor = first["next_cursor"]
        .as_str()
        .expect("real cursor")
        .to_string();
    q.push(("after", cursor.clone()));
    q.push(("review", first["review"].to_string()));
    let (status, second) = get(&app, "/v1/serials", &q).await;
    assert_eq!(status, 200, "{second}");
    assert_eq!(second["items"][0]["serial"], "one");
    assert!(second["next_cursor"].is_null());
    let (_, admin_catalog) = http(
        &app,
        "POST",
        "/mcp",
        "other",
        Some(json!({"jsonrpc":"2.0","id":3,"method":"tools/list","params":{}})),
    )
    .await;
    assert_eq!(
        admin_catalog["result"]["tools"]
            .as_array()
            .expect("administrator catalog")
            .len(),
        87
    );
    assert_eq!(get(&app, "/v1/serial-observations", &q).await.0, 422);
    let mut changed = q.clone();
    changed.push(("status", "on_hand".into()));
    assert_eq!(get(&app, "/v1/serials", &changed).await.0, 422);
    let mut forged: Value =
        serde_json::from_str(&cursor).expect("owned synthetic serial test evidence");
    forged["key"] = json!(["Absent"]);
    let forged = vec![("sku", "SERIAL".into()), ("after", forged.to_string())];
    assert_eq!(get(&app, "/v1/serials", &forged).await.0, 422);
    let mut h = sku();
    h.extend([("serial", "One".into()), ("limit", "1".into())]);
    let (status, history) = get(&app, "/v1/serial-history", &h).await;
    assert_eq!(status, 200, "{history}");
    assert_eq!(history["total_records"], 2);
    assert_eq!(history["current_source_ok"], true);
    assert!(history["items"][0]["before_place"].is_null());
    assert_eq!(history["items"][0]["revision"], 1);
    h.push((
        "after",
        history["next_cursor"]
            .as_str()
            .expect("owned synthetic serial test evidence")
            .into(),
    ));
    let (status, history) = get(&app, "/v1/serial-history", &h).await;
    assert_eq!(status, 200, "{history}");
    assert_eq!(history["items"][0]["before_place"]["location"], "A");
    assert_eq!(history["items"][0]["after_place"]["location"], "B");
    assert_eq!(history["page_source_ok"], true);
    for (path, pairs, expected) in [
        ("/v1/serial-history", sku(), 422),
        (
            "/v1/serials",
            vec![("sku", "SERIAL".into()), ("limit", "0".into())],
            422,
        ),
        (
            "/v1/serials",
            vec![("sku", "SERIAL".into()), ("unknown", "x".into())],
            422,
        ),
        (
            "/v1/serials",
            vec![("sku", "SERIAL".into()), ("warehouse_id", "absent".into())],
            404,
        ),
        (
            "/v1/serial-history",
            vec![("sku", "SERIAL".into()), ("serial", "ONE".into())],
            404,
        ),
    ] {
        assert_eq!(get(&app, path, &pairs).await.0, expected, "{path}");
    }
    assert_eq!(
        http(&app, "GET", "/v1/serials?sku=SERIAL", "other", None)
            .await
            .0,
        404
    );
    assert_eq!(
        http(&app, "GET", "/v1/serials?sku=SERIAL", "", None)
            .await
            .0,
        401
    );
    let (_, catalog) = http(
        &app,
        "POST",
        "/mcp",
        "reader",
        Some(json!({"jsonrpc":"2.0","id":1,"method":"tools/list","params":{}})),
    )
    .await;
    for (name, required) in [
        ("wms_serials", json!(["sku"])),
        ("wms_serial_history", json!(["sku", "serial"])),
        ("wms_serial_observations", json!(["sku"])),
    ] {
        let tool = catalog["result"]["tools"]
            .as_array()
            .expect("MCP catalog")
            .iter()
            .find(|t| t["name"] == name)
            .expect("new read tool");
        assert_eq!(tool["inputSchema"]["required"], required);
        assert_eq!(tool["annotations"]["readOnlyHint"], true);
        let (_,response)=http(&app,"POST","/mcp","reader",Some(json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":name,"arguments":{"sku":"SERIAL","serial":"One","limit":1}}}))).await;
        assert_eq!(
            response["result"]["structuredContent"]["http_status"], 200,
            "{response}"
        );
        assert_eq!(
            response["result"]["structuredContent"]["body"]["page_source_ok"], true,
            "{response}"
        );
    }
    assert_eq!(f.facts().await, before);
    f.proof().await;
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn observations_page_inside_original_groups_and_retain_noop_and_empty_counts() {
    let f = Fixture::new().await;
    f.catalog().await;
    count(&f, &["UPPER", "lower"], "First complete count").await;
    count(&f, &["UPPER", "lower"], "Unchanged complete observation").await;
    count(&f, &[], "Actual empty count").await;
    let app = app(&f);
    let before = f.facts().await;
    let mut q = sku();
    q.push(("limit", "1".into()));
    let mut rows = vec![];
    let mut cursors = std::collections::BTreeSet::new();
    loop {
        let (status, page) = get(&app, "/v1/serial-observations", &q).await;
        assert_eq!(status, 200, "{page}");
        assert_eq!(page["total_records"], 5);
        assert_eq!(page["page_source_ok"], true, "{page}");
        rows.push(page["items"][0].clone());
        if let Some(c) = page["next_cursor"].as_str() {
            assert!(cursors.insert(c.to_string()));
            q.retain(|(k, _)| *k != "after");
            q.push(("after", c.into()));
        } else {
            break;
        }
    }
    assert_eq!(rows.len(), 5);
    assert_eq!(rows[0]["observed_serial"], "UPPER");
    assert_eq!(rows[1]["observed_serial"], "lower");
    assert_eq!(rows[0]["event_id"], rows[1]["event_id"]);
    assert_eq!(rows[0]["serial_ordinal"], 1);
    assert_eq!(rows[1]["serial_ordinal"], 2);
    assert_ne!(rows[0]["event_id"], rows[2]["event_id"]);
    assert_eq!(rows[2]["sku_version"], 2);
    assert_eq!(rows[4]["observed_serial_count"], 0);
    assert_eq!(rows[4]["serial_ordinal"], 0);
    assert!(rows[4]["observed_serial"].is_null());
    assert!(rows.iter().all(|r| r.get("serials").is_none()
        && r["actor"].is_string()
        && r["recorded_at"].is_string()));
    let mut exact = sku();
    exact.push(("serial", "UPPER".into()));
    let (status, page) = get(&app, "/v1/serial-observations", &exact).await;
    assert_eq!(status, 200, "{page}");
    assert_eq!(page["total_records"], 2);
    assert_eq!(
        page["items"]
            .as_array()
            .expect("owned synthetic serial test evidence")
            .len(),
        2
    );
    let (_, current) = get(&app, "/v1/serials", &sku()).await;
    assert_eq!(current["items"][0]["place"]["status"], "missing");
    assert_eq!(current["page_source_ok"], true);
    assert_eq!(f.facts().await, before);
    f.proof().await;
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn destination_changes_expire_global_pages_without_advancing_source_warehouse() {
    let f = Fixture::new().await;
    f.catalog().await;
    other_warehouse(&f).await;
    count(&f, &["FIRST", "SECOND"], "Original transfer labels").await;
    let (_,dispatched)=f.execute(json!({"type":"dispatch_transfer","reference":"Read boundary transfer","destination_warehouse_id":"wh-other","carrier":"Synthetic","handoff_reference":"SYNTHETIC","lines":[{"id":"LINE","sku":"SERIAL","lot":"LOT","location":"A","quantity":2}]}),observed(&["LINE"],&["FIRST","SECOND"])).await;
    let app = app(&f);
    let mut q = sku();
    q.extend([
        ("warehouse_id", "wh-main".into()),
        ("limit", "1".into()),
        ("status", "in_transit".into()),
    ]);
    let (status, first) = get(&app, "/v1/serials", &q).await;
    assert_eq!(status, 200, "{first}");
    assert_eq!(first["total_records"], 2);
    let mut dest = q.clone();
    dest[1].1 = "wh-other".into();
    let (status, page) = get(&app, "/v1/serials", &dest).await;
    assert_eq!(status, 200, "{page}");
    assert_eq!(page["total_records"], 2);
    let source = f.state().await.version;
    at(&f,"wh-other",json!({"type":"receive_transfer","transfer_id":dispatched["result"]["entity_id"],"line_id":"LINE","location":"RECV-01","quantity":1,"arrival_reference":"SYNTHETIC-ARRIVAL"}),observed(&[],&["FIRST"])).await;
    assert_eq!(f.state().await.version, source);
    q.push((
        "after",
        first["next_cursor"]
            .as_str()
            .expect("owned synthetic serial test evidence")
            .into(),
    ));
    assert_eq!(get(&app, "/v1/serials", &q).await.0, 409);
    let mut h = sku();
    h.extend([
        ("serial", "FIRST".into()),
        ("warehouse_id", "wh-other".into()),
    ]);
    let (status, page) = get(&app, "/v1/serial-history", &h).await;
    assert_eq!(status, 200, "{page}");
    assert_eq!(page["total_records"], 1);
    assert_eq!(page["items"][0]["revision"], 3);
    assert_eq!(page["page_source_ok"], true);
    let (status, page) = get(&app, "/v1/serials", &sku()).await;
    assert_eq!(status, 200, "{page}");
    assert_eq!(page["total_records"], 2);
    assert_eq!(page["page_source_ok"], true);
    f.proof().await;
}

async fn remove_projection_and_steps(f: &Fixture) {
    // Only this isolated synthetic tenant is altered; normal application guards
    // forbid deleting either record. Keep immutable event/observation evidence.
    let mut tx = f
        .store
        .pool
        .begin()
        .await
        .expect("owned synthetic serial test evidence");
    sqlx::raw_sql("ALTER TABLE wms_serial_units DISABLE TRIGGER wms_serial_unit_identity; ALTER TABLE wms_serial_steps DISABLE TRIGGER wms_serial_step_immutable").execute(&mut *tx).await.expect("owned synthetic serial test evidence");
    sqlx::query("DELETE FROM wms_serial_units WHERE tenant=$1")
        .bind(&f.who.tenant)
        .execute(&mut *tx)
        .await
        .expect("owned synthetic serial test evidence");
    sqlx::query("DELETE FROM wms_serial_steps WHERE tenant=$1")
        .bind(&f.who.tenant)
        .execute(&mut *tx)
        .await
        .expect("owned synthetic serial test evidence");
    sqlx::raw_sql("SET CONSTRAINTS ALL IMMEDIATE; ALTER TABLE wms_serial_units ENABLE TRIGGER wms_serial_unit_identity; ALTER TABLE wms_serial_steps ENABLE TRIGGER wms_serial_step_immutable").execute(&mut *tx).await.expect("owned synthetic serial test evidence");
    tx.commit()
        .await
        .expect("owned synthetic serial test evidence");
}
#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn retained_observation_blocks_rebirth_when_current_and_steps_are_missing() {
    let f = Fixture::new().await;
    f.catalog().await;
    other_warehouse(&f).await;
    count(&f, &["RETAINED"], "Original source evidence").await;
    remove_projection_and_steps(&f).await;
    let request=Request{request_id:domain::id(),expected_version:2,resource_versions:None,unit_inputs:vec![],serial_inputs:observed(&[],&["RETAINED"]),command:serde_json::from_value(json!({"type":"count_stock","sku":"SERIAL","lot":"LOT","location":"RECV-01","counted":1,"reason":"Must not re-register a retained original label"})).expect("owned synthetic serial test evidence")};
    let before = f.facts().await;
    assert_eq!(
        f.store
            .execute(&f.who, "wh-other", &request)
            .await
            .expect_err("retained observation forbids rebirth")
            .status,
        503
    );
    assert_eq!(f.facts().await, before);
}
#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn retained_observation_holds_exact_read_when_current_and_steps_are_missing() {
    let f = Fixture::new().await;
    f.catalog().await;
    count(&f, &["RETAINED"], "Original source evidence").await;
    remove_projection_and_steps(&f).await;
    let app = app(&f);
    let mut q = sku();
    q.push(("serial", "RETAINED".into()));
    for path in ["/v1/serials", "/v1/serial-history"] {
        let (status, body) = get(&app, path, &q).await;
        assert_eq!(status, 503, "{path}: {body}");
    }
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn current_source_damage_stays_visible_and_taints_lineage_without_rewriting_facts() {
    let f = Fixture::new().await;
    f.catalog().await;
    count(&f, &["ORIGINAL"], "Original source").await;
    count(&f, &["ORIGINAL"], "Unchanged observation").await;
    let mut tx = f
        .store
        .pool
        .begin()
        .await
        .expect("owned synthetic serial test evidence");
    sqlx::raw_sql("ALTER TABLE wms_serial_units DISABLE TRIGGER wms_serial_unit_identity; ALTER TABLE wms_serial_units DISABLE TRIGGER wms_serial_unit_bound").execute(&mut *tx).await.expect("owned synthetic serial test evidence");
    sqlx::query("UPDATE wms_serial_units SET last_event_id=(SELECT last_event_id FROM wms_serial_heads WHERE tenant=$1 AND sku='SERIAL') WHERE tenant=$1 AND serial='ORIGINAL'").bind(&f.who.tenant).execute(&mut *tx).await.expect("owned synthetic serial test evidence");
    sqlx::raw_sql("SET CONSTRAINTS ALL IMMEDIATE; ALTER TABLE wms_serial_units ENABLE TRIGGER wms_serial_unit_identity; ALTER TABLE wms_serial_units ENABLE TRIGGER wms_serial_unit_bound").execute(&mut *tx).await.expect("owned synthetic serial test evidence");
    tx.commit()
        .await
        .expect("owned synthetic serial test evidence");
    let before = f.facts().await;
    let app = app(&f);
    let mut q = sku();
    q.push(("serial", "ORIGINAL".into()));
    let (status, page) = get(&app, "/v1/serials", &q).await;
    assert_eq!(status, 200, "{page}");
    assert_eq!(
        page["items"]
            .as_array()
            .expect("owned synthetic serial test evidence")
            .len(),
        1
    );
    assert_eq!(page["page_source_ok"], false);
    assert!(page["items"][0]["last_source"].is_null());
    let (status, page) = get(&app, "/v1/serial-history", &q).await;
    assert_eq!(status, 200, "{page}");
    assert_eq!(page["total_records"], 1);
    assert_eq!(page["items"][0]["source_ok"], true);
    assert_eq!(page["current_source_ok"], false);
    assert_eq!(page["page_source_ok"], false);
    assert_eq!(f.facts().await, before);
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn encoded_utf8_budget_continues_before_a_complete_record_without_losing_identities() {
    let mut f = Fixture::new().await;
    f.who.actor = "a".repeat(240);
    let sku = "界".repeat(80);
    let lot = "批".repeat(80);
    let bin = "位".repeat(80);
    f.plain(json!({"type":"create_product","sku":sku,"name":"Synthetic bounded long identity","category":"Synthetic","reorder_point":0,"serial_controlled":true})).await;
    f.plain(json!({"type":"create_location","code":bin,"zone":"Synthetic","kind":"storage"}))
        .await;
    let labels: Vec<String> = (0..100)
        .map(|n| format!("{n:04}{}", "号".repeat(78)))
        .collect();
    f.execute(json!({"type":"count_stock","sku":sku,"lot":lot,"location":bin,"counted":100,"reason":"Synthetic UTF-8 byte-budget source"}),vec![Input{target:vec![],sku:sku.clone(),serials:labels.clone()}]).await;
    let before = f.facts().await;
    let app = app(&f);
    let mut q = vec![("sku", sku), ("limit", "100".into())];
    let mut seen = vec![];
    let mut pages = 0;
    loop {
        let (status, page) = get(&app, "/v1/serials", &q).await;
        assert_eq!(status, 200, "{page}");
        assert_eq!(page["page_source_ok"], true);
        assert_eq!(page["total_records"], 100);
        let items = page["items"]
            .as_array()
            .expect("owned synthetic serial test evidence");
        assert!(!items.is_empty());
        assert!(
            serde_json::to_vec(items)
                .expect("owned synthetic serial test evidence")
                .len()
                <= 128 * 1024
        );
        assert!(
            serde_json::to_vec(&page)
                .expect("owned synthetic serial test evidence")
                .len()
                < 256 * 1024
        );
        seen.extend(items.iter().map(|i| {
            i["serial"]
                .as_str()
                .expect("owned synthetic serial test evidence")
                .to_string()
        }));
        pages += 1;
        assert!(pages <= 4);
        if let Some(c) = page["next_cursor"].as_str() {
            q.retain(|(k, _)| *k != "after");
            q.push(("after", c.into()));
        } else {
            break;
        }
    }
    assert!(
        pages > 1,
        "byte budget, not only item limit, must force continuation"
    );
    assert_eq!(seen, labels);
    assert_eq!(f.facts().await, before);
    f.proof().await;
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn forged_observation_remains_visible_but_fails_selected_original_event_checks() {
    let f = Fixture::new().await;
    f.catalog().await;
    count(&f, &["ORIGINAL"], "Original observed label").await;
    let mut tx = f
        .store
        .pool
        .begin()
        .await
        .expect("owned forensic observation fixture");
    sqlx::raw_sql(
        "ALTER TABLE wms_serial_observations DISABLE TRIGGER wms_serial_observation_immutable",
    )
    .execute(&mut *tx)
    .await
    .expect("bypass only owned fixture observation immutability");
    sqlx::query("UPDATE wms_serial_observations SET serials='[\"FORGED\"]'::jsonb WHERE tenant=$1")
        .bind(&f.who.tenant)
        .execute(&mut *tx)
        .await
        .expect("retain original event while forging normalized label");
    sqlx::raw_sql("SET CONSTRAINTS ALL IMMEDIATE; ALTER TABLE wms_serial_observations ENABLE TRIGGER wms_serial_observation_immutable").execute(&mut *tx).await.expect("restore named guard before retaining explicit bad evidence");
    tx.commit()
        .await
        .expect("retain synthetic original-source disagreement");
    let before = f.facts().await;
    let app = app(&f);
    let (status, page) = get(&app, "/v1/serial-observations", &sku()).await;
    assert_eq!(status, 200, "{page}");
    assert_eq!(page["items"][0]["observed_serial"], "FORGED");
    assert_eq!(page["items"][0]["source_ok"], false);
    assert_eq!(page["page_source_ok"], false);
    let (status, page) = get(&app, "/v1/serials", &sku()).await;
    assert_eq!(status, 200, "{page}");
    assert_eq!(page["items"][0]["serial"], "ORIGINAL");
    assert_eq!(page["page_source_ok"], false);
    assert_eq!(f.facts().await, before);
}
