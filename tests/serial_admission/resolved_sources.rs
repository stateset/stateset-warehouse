//! Shared command resolution must preserve every complete original source verdict.
use super::*;
use sqlx::PgConnection;

fn event_map(sql: &str) -> String {
    let boundary = sql
        .find("), original AS")
        .expect("complete source boundary");
    format!("{}) SELECT COALESCE(jsonb_object_agg(id,jsonb_build_object('warehouse_id',warehouse_id,'ok',ok)),'{{}}'::jsonb) FROM event_sources;",&sql[..boundary])
}

async fn compare(c: &mut PgConnection, tenant: &str, warehouse: &str, healthy: bool) {
    let original: Value =
        sqlx::query_scalar(include_str!("../fixtures/serial_reconciliation_v24.sql"))
            .bind(tenant)
            .bind(warehouse)
            .fetch_one(&mut *c)
            .await
            .expect("immutable complete source24 oracle");
    let resolved: Value = sqlx::query_scalar(include_str!("../../src/serial_reconciliation.sql"))
        .bind(tenant)
        .bind(warehouse)
        .fetch_one(&mut *c)
        .await
        .expect("snapshot-local resolved source query");
    assert_eq!(
        resolved, original,
        "complete original proof changed at {warehouse}"
    );
    assert_eq!(resolved["ok"], healthy);
    let original_events: Value = sqlx::query_scalar(&event_map(include_str!(
        "../fixtures/serial_reconciliation_v24.sql"
    )))
    .bind(tenant)
    .fetch_one(&mut *c)
    .await
    .expect("every original event verdict");
    let resolved_events: Value = sqlx::query_scalar(&event_map(include_str!(
        "../../src/serial_reconciliation.sql"
    )))
    .bind(tenant)
    .fetch_one(&mut *c)
    .await
    .expect("every resolved event verdict");
    assert_eq!(
        resolved_events, original_events,
        "a damaged class must not mask another event's changed source verdict"
    );
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn resolved_sources_preserve_empty_untracked_and_zero_identity_head_outcomes() {
    let f = Fixture::new().await;
    let mut tx = f.store.pool.begin().await.expect("empty original proof");
    compare(&mut tx, &f.who.tenant, "wh-main", true).await;
    tx.rollback().await.expect("read-only empty oracle");
    f.plain(json!({"type":"create_product","sku":"UNTRACKED","name":"Synthetic untracked","category":"Synthetic","reorder_point":0})).await;
    f.plain(json!({"type":"count_stock","sku":"UNTRACKED","lot":"LOT","location":"RECV-01","counted":3,"reason":"Synthetic untracked source"})).await;
    // Distinct metadata commands keep the class maps large even though the
    // controlled identity population is zero. Their exact event flags matter.
    for n in 0..200 {
        f.plain(json!({"type":"create_location","code":format!("META-{n:04}"),"zone":"Synthetic","kind":"storage"})).await;
    }
    f.catalog().await;
    f.execute(json!({"type":"count_stock","sku":"SERIAL","lot":"LOT","location":"A","counted":0,"reason":"Complete original empty set"}),observed(&[],&[])).await;
    let mut tx = f.store.pool.begin().await.expect("zero identity oracle");
    compare(&mut tx, &f.who.tenant, "wh-main", true).await;
    tx.rollback().await.expect("original zero head retained");
    f.proof().await;
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn shared_command_classes_keep_each_original_event_group_and_facility_binding_after_damage() {
    let f = Fixture::new().await;
    f.catalog().await;
    extended::other_warehouse(&f).await;
    f.execute(json!({"type":"count_stock","sku":"SERIAL","lot":"LOT","location":"A","counted":2,"reason":"Original complete labels"}),observed(&[],&["One","Other"])).await;
    for (from, to, label) in [
        ("A", "B", "One"),
        ("A", "B", "Other"),
        ("B", "A", "One"),
        ("B", "A", "Other"),
        ("A", "B", "One"),
        ("B", "A", "One"),
    ] {
        f.execute(json!({"type":"move_stock","sku":"SERIAL","lot":"LOT","from":from,"to":to,"quantity":1}),observed(&[],&[label])).await;
    }
    f.execute(json!({"type":"count_stock","sku":"SERIAL","lot":"LOT","location":"A","counted":2,"reason":"Unchanged complete set in different original order"}),observed(&[],&["Other","One"])).await;
    f.execute(json!({"type":"count_stock","sku":"SERIAL","lot":"LOT","location":"B","counted":0,"reason":"Original empty historical bucket"}),observed(&[],&[])).await;
    for bin in ["A", "B"] {
        extended::at(
            &f,
            "wh-other",
            json!({"type":"create_location","code":bin,"zone":"Synthetic","kind":"storage"}),
            vec![],
        )
        .await;
    }
    extended::at(&f,"wh-other",json!({"type":"count_stock","sku":"SERIAL","lot":"LOT","location":"A","counted":1,"reason":"Another original facility"}),observed(&[],&["Remote"])).await;
    extended::at(
        &f,
        "wh-other",
        json!({"type":"move_stock","sku":"SERIAL","lot":"LOT","from":"A","to":"B","quantity":1}),
        observed(&[], &["Remote"]),
    )
    .await;
    let mut tx = f
        .store
        .pool
        .begin()
        .await
        .expect("shared resolution forensic oracle");
    for w in ["wh-main", "wh-other"] {
        compare(&mut tx, &f.who.tenant, w, true).await;
    }
    sqlx::raw_sql("ALTER TABLE wms_events DISABLE TRIGGER wms_events_immutable; ALTER TABLE wms_commands DISABLE TRIGGER wms_commands_immutable; ALTER TABLE wms_serial_observations DISABLE TRIGGER wms_serial_observation_immutable; ALTER TABLE wms_serial_steps DISABLE TRIGGER wms_serial_step_immutable; ALTER TABLE wms_serial_batches DISABLE TRIGGER wms_serial_batch_immutable; ALTER TABLE wms_products DISABLE TRIGGER wms_serial_product_mode").execute(&mut *tx).await.expect("named rollback-only forensic bypasses");
    let event="(SELECT id FROM wms_events WHERE tenant=$1 AND warehouse_id='wh-main' AND payload#>>'{command,type}'='move_stock' ORDER BY version LIMIT 1)";
    for damage in [
        "UPDATE wms_events SET payload='null' WHERE id=<EVENT>",
        "UPDATE wms_events SET payload='123' WHERE id=<EVENT>",
        "UPDATE wms_events SET payload='[]' WHERE id=<EVENT>",
        "UPDATE wms_events SET payload=jsonb_set(payload,'{serial_steps}','{}') WHERE id=<EVENT>",
        "UPDATE wms_events SET payload=jsonb_set(payload,'{serial_steps}',(SELECT jsonb_agg('{}'::jsonb) FROM generate_series(1,10001))) WHERE id=<EVENT>",
        "UPDATE wms_events SET payload=jsonb_set(payload,'{serial_heads}','[]') WHERE id=<EVENT>",
        "UPDATE wms_events SET payload=payload-'serial_heads' WHERE id=<EVENT>",
        "UPDATE wms_events SET payload=jsonb_set(payload,'{serial_heads,SERIAL}','\"2\"') WHERE id=<EVENT>",
        "UPDATE wms_events SET payload=jsonb_set(payload,'{command}','123') WHERE id=<EVENT>",
        "UPDATE wms_events SET payload=jsonb_set(payload,'{command,type}','\"count_stock\"') WHERE id=<EVENT>",
        "UPDATE wms_events SET payload=jsonb_set(payload,'{command,type}','\"create_location\"') WHERE id=<EVENT>",
        "UPDATE wms_events SET payload=jsonb_set(payload,'{command,quantity}','\"not-a-number\"') WHERE id=<EVENT>",
        "UPDATE wms_events SET payload=jsonb_set(payload,'{serial_inputs}','{}') WHERE id=<EVENT>",
        "UPDATE wms_events SET payload=jsonb_set(payload,'{serial_inputs,0,serials}','{}') WHERE id=<EVENT>",
        "UPDATE wms_events SET payload=jsonb_set(payload,'{serial_inputs,0,serials}','[\"Other\"]') WHERE id=<EVENT>",
        "UPDATE wms_events SET payload=jsonb_set(payload,'{serial_inputs}',(payload->'serial_inputs')||(payload->'serial_inputs')) WHERE id=<EVENT>",
        "UPDATE wms_events SET warehouse_id='wh-other' WHERE id=(SELECT id FROM wms_events WHERE tenant=$1 AND warehouse_id='wh-main' AND payload#>>'{command,type}'='move_stock' ORDER BY version DESC LIMIT 1)",
        "UPDATE wms_serial_observations SET ordinal=99 WHERE event_id=<EVENT>",
        "UPDATE wms_serial_observations SET sku_version=sku_version+1 WHERE event_id=<EVENT>",
        "DELETE FROM wms_serial_observations WHERE event_id=<EVENT>",
        "INSERT INTO wms_serial_observations SELECT tenant,warehouse_id,event_id,99,sku,sku_version,'[\"extra\"]','[]' FROM wms_serial_observations WHERE event_id=<EVENT>",
        "UPDATE wms_serial_steps SET actor='forged' WHERE event_id=<EVENT>",
        "UPDATE wms_serial_steps SET before_place=jsonb_set(before_place,'{location}','\"RECV-01\"') WHERE event_id=<EVENT>",
        "UPDATE wms_serial_steps SET warehouse_id='wh-other' WHERE event_id=<EVENT>",
        "UPDATE wms_serial_batches SET warehouse_version=warehouse_version+1 WHERE event_id=<EVENT>",
        "UPDATE wms_commands SET actor='forged' WHERE tenant=$1 AND request_id=(SELECT payload->>'request_id' FROM wms_events WHERE id=<EVENT>)",
        "UPDATE wms_products SET data=jsonb_set(data,'{serial_controlled}','false') WHERE tenant=$1 AND warehouse_id='wh-main' AND sku='SERIAL'",
    ] {
        sqlx::query("SAVEPOINT shared_resolution_damage").execute(&mut *tx).await.expect("forensic source boundary");
        sqlx::query(&damage.replace("<EVENT>",event)).bind(&f.who.tenant).execute(&mut *tx).await.expect("owned original-fact damage");
        for w in ["wh-main","wh-other"] {compare(&mut tx,&f.who.tenant,w,false).await;}
        sqlx::query("ROLLBACK TO SAVEPOINT shared_resolution_damage").execute(&mut *tx).await.expect("restore exact original facts");
        for w in ["wh-main","wh-other"] {compare(&mut tx,&f.who.tenant,w,true).await;}
    }
    tx.rollback().await.expect("remove all forensic bypasses");
    f.proof().await;
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn count_read_sources_keep_complete_historical_sets_and_original_damaged_event_verdicts() {
    let f = Fixture::new().await;
    f.catalog().await;
    let mut labels: Vec<String> = (0..60)
        .map(|n| format!("Literal-{n:04}-{}", "z".repeat(32)))
        .collect();
    labels.extend(["Unit-A", "unit-a", "é", "e\u{301}"].map(str::to_owned));
    let literal_refs: Vec<&str> = labels.iter().map(String::as_str).collect();
    let (birth, _) = f.execute(json!({"type":"count_stock","sku":"SERIAL","lot":"LOT","location":"A","counted":64,"reason":"Complete literal birth with a large original payload"}), observed(&[], &literal_refs)).await;
    let (noop, _) = f.execute(json!({"type":"count_stock","sku":"SERIAL","lot":"LOT","location":"A","counted":64,"reason":"Complete unchanged historical set"}), observed(&[], &literal_refs)).await;
    // Later absence and relocation must not change the historical no-op proof.
    f.execute(json!({"type":"count_stock","sku":"SERIAL","lot":"LOT","location":"A","counted":63,"reason":"One observed missing unit"}), observed(&[], &literal_refs[..63])).await;
    f.execute(
        json!({"type":"move_stock","sku":"SERIAL","lot":"LOT","from":"A","to":"B","quantity":1}),
        observed(&[], &[literal_refs[0]]),
    )
    .await;
    let mut tx = f
        .store
        .pool
        .begin()
        .await
        .expect("count source forensic oracle");
    compare(&mut tx, &f.who.tenant, "wh-main", true).await;
    sqlx::raw_sql("ALTER TABLE wms_events DISABLE TRIGGER wms_events_immutable; ALTER TABLE wms_commands DISABLE TRIGGER wms_commands_immutable; ALTER TABLE wms_serial_observations DISABLE TRIGGER wms_serial_observation_immutable; ALTER TABLE wms_serial_steps DISABLE TRIGGER wms_serial_step_immutable; ALTER TABLE wms_serial_batches DISABLE TRIGGER wms_serial_batch_immutable")
        .execute(&mut *tx).await.expect("rollback-only owned forensic bypasses");
    let event = "(SELECT id FROM wms_events WHERE tenant=$1 AND payload->>'request_id'=$2)";
    for (request, damage, aligned_input_damage) in [
        (&birth, "UPDATE wms_events SET payload='null' WHERE id=<EVENT>", false),
        (&birth, "UPDATE wms_events SET payload='[]' WHERE id=<EVENT>", false),
        (&birth, "UPDATE wms_events SET payload='1.0' WHERE id=<EVENT>", false),
        (&birth, "UPDATE wms_events SET payload=jsonb_set(payload,'{serial_steps}','{}') WHERE id=<EVENT>", false),
        (&birth, "UPDATE wms_events SET payload=jsonb_set(payload,'{serial_heads,SERIAL}','\"1\"') WHERE id=<EVENT>", false),
        (&birth, "UPDATE wms_events SET payload=jsonb_set(payload,'{serial_heads,SERIAL}','1.0') WHERE id=<EVENT>", false),
        (&birth, "UPDATE wms_events SET payload=jsonb_set(payload,'{serial_heads,SERIAL}','1.5') WHERE id=<EVENT>", false),
        (&birth, "UPDATE wms_events SET payload=jsonb_set(payload,'{serial_heads,SERIAL}','-1') WHERE id=<EVENT>", false),
        (&birth, "UPDATE wms_events SET payload=jsonb_set(payload,'{serial_heads,SERIAL}','9223372036854775808') WHERE id=<EVENT>", false),
        (&birth, "DELETE FROM wms_serial_steps WHERE event_id=<EVENT> AND ordinal=1", false),
        (&birth, "UPDATE wms_events SET payload=jsonb_set(payload,'{actor}','\"forged\"') WHERE id=<EVENT>", false),
        (&birth, "UPDATE wms_serial_steps SET actor='forged' WHERE event_id=<EVENT>", false),
        (&birth, "UPDATE wms_serial_batches SET warehouse_version=warehouse_version+1 WHERE event_id=<EVENT>", false),
        (&birth, "UPDATE wms_commands SET actor='forged' WHERE tenant=$1 AND request_id=$2", false),
        (&noop, "DELETE FROM wms_commands WHERE tenant=$1 AND request_id=$2", false),
        // Keep the command, submitted labels and normalized observation aligned.
        // Omission requires the complete historical-set check; an unknown literal
        // also fails original membership. Both keep the input-binding facts valid.
        (&noop, "WITH changed AS (UPDATE wms_events SET payload=jsonb_set(jsonb_set(payload,'{serial_inputs,0,serials}',(payload#>'{serial_inputs,0,serials}')-0),'{command,counted}',to_jsonb((payload#>>'{command,counted}')::bigint-1)) WHERE id=<EVENT> RETURNING id) UPDATE wms_serial_observations SET serials=serials-0 WHERE event_id IN(SELECT id FROM changed)", true),
        (&noop, "WITH changed AS (UPDATE wms_events SET payload=jsonb_set(jsonb_set(payload,'{serial_inputs,0,serials}',(payload#>'{serial_inputs,0,serials}')||'\"Unknown-literal\"'::jsonb),'{command,counted}',to_jsonb((payload#>>'{command,counted}')::bigint+1)) WHERE id=<EVENT> RETURNING id) UPDATE wms_serial_observations SET serials=serials||'\"Unknown-literal\"'::jsonb WHERE event_id IN(SELECT id FROM changed)", true),
    ] {
        sqlx::query("SAVEPOINT count_source_damage").execute(&mut *tx).await.expect("original count boundary");
        sqlx::query(&damage.replace("<EVENT>", event)).bind(&f.who.tenant).bind(&request.request_id).execute(&mut *tx).await.expect("owned count source damage");
        if aligned_input_damage {
            let bound: bool = sqlx::query_scalar(&format!("SELECT wms_serial_event_inputs_bound($1,'wh-main',{event})"))
                .bind(&f.who.tenant).bind(&request.request_id).fetch_one(&mut *tx).await.expect("aligned submitted count facts");
            assert!(bound, "aligned input facts must remain bound before historical validation");
            let verdicts: (bool, bool) = sqlx::query_as(&format!("SELECT wms_serial_event_valid($1,'wh-main',{event}),wms_serial_event_valid_read_source($1,'wh-main',{event})"))
                .bind(&f.who.tenant).bind(&request.request_id).fetch_one(&mut *tx).await.expect("original and read-only count verdicts");
            assert_eq!(verdicts, (false, false));
        }
        compare(&mut tx, &f.who.tenant, "wh-main", false).await;
        sqlx::query("ROLLBACK TO SAVEPOINT count_source_damage").execute(&mut *tx).await.expect("restore every original count fact");
        compare(&mut tx, &f.who.tenant, "wh-main", true).await;
    }
    tx.rollback().await.expect("remove every forensic bypass");
    f.proof().await;
}

/// Every input class verdict from the set-form predicates, beside the original
/// per-class matcher evaluated in the same snapshot.
fn class_map(sql: &str) -> String {
    let boundary = sql
        .find("), command_sources AS")
        .expect("input class boundary");
    format!("{}) SELECT jsonb_build_object('classes',count(*),'mismatches',COALESCE(jsonb_agg(jsonb_build_object('command',command,'inputs',inputs,'ok',ok,'oracle',oracle)) FILTER(WHERE ok IS DISTINCT FROM oracle),'[]'::jsonb)) FROM (SELECT *,CASE WHEN inputs='[]'::jsonb AND COALESCE(command->>'type','') NOT IN('count_stock','move_stock','receive','pick','unpick','create_return','pack_shipment','dispatch_transfer','receive_transfer','record_transfer_loss','reverse_transfer_loss') THEN true ELSE wms_serial_inputs_match_command($1,warehouse_id,command,inputs) END AS oracle FROM input_sources) verdicts;",&sql[..boundary])
}

async fn compare_classes(c: &mut PgConnection, tenant: &str, case: &str) {
    let report: Value = sqlx::query_scalar(&class_map(include_str!(
        "../../src/serial_reconciliation.sql"
    )))
    .bind(tenant)
    .fetch_one(&mut *c)
    .await
    .expect("set-form and original input class verdicts");
    assert!(
        report["classes"].as_i64().unwrap_or(0) > 0,
        "{case}: {report}"
    );
    assert_eq!(report["mismatches"], json!([]), "{case}: {report}");
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn set_form_input_classes_match_the_original_matcher_on_every_malformed_shape() {
    let f = Fixture::new().await;
    f.catalog().await;
    f.execute(json!({"type":"count_stock","sku":"SERIAL","lot":"LOT","location":"A","counted":2,"reason":"Original complete labels"}),observed(&[],&["One","Other"])).await;
    f.execute(
        json!({"type":"move_stock","sku":"SERIAL","lot":"LOT","from":"A","to":"B","quantity":1}),
        observed(&[], &["One"]),
    )
    .await;
    f.execute(
        json!({"type":"move_stock","sku":"SERIAL","lot":"LOT","from":"A","to":"B","quantity":1}),
        observed(&[], &["Other"]),
    )
    .await;
    f.plain(json!({"type":"create_location","code":"META","zone":"Synthetic","kind":"storage"}))
        .await;
    let mut tx = f.store.pool.begin().await.expect("input class oracle");
    compare_classes(&mut tx, &f.who.tenant, "healthy").await;
    sqlx::raw_sql("ALTER TABLE wms_events DISABLE TRIGGER wms_events_immutable; ALTER TABLE wms_products DISABLE TRIGGER wms_serial_product_mode")
        .execute(&mut *tx).await.expect("rollback-only forensic bypasses");
    let moved = "(SELECT id FROM wms_events WHERE tenant=$1 AND payload#>>'{command,type}'='move_stock' ORDER BY version LIMIT 1)";
    let counted = "(SELECT id FROM wms_events WHERE tenant=$1 AND payload#>>'{command,type}'='count_stock' ORDER BY version LIMIT 1)";
    let metadata = "(SELECT id FROM wms_events WHERE tenant=$1 AND payload#>>'{command,type}'='create_location' AND payload#>>'{command,code}'='META')";
    let entry = |inputs: &str| {
        format!("UPDATE wms_events SET payload=jsonb_set(payload,'{{serial_inputs}}','{inputs}') WHERE id={moved}")
    };
    let command = |path: &str, value: &str| {
        format!("UPDATE wms_events SET payload=jsonb_set(payload,'{{command,{path}}}','{value}') WHERE id={moved}")
    };
    let many = format!(
        "[{}]",
        (0..101)
            .map(|n| format!("{{\"target\":[\"T{n}\"],\"sku\":\"SERIAL\",\"serials\":[]}}"))
            .collect::<Vec<_>>()
            .join(",")
    );
    let wide = format!(
        "[{{\"target\":[],\"sku\":\"SERIAL\",\"serials\":[{}]}}]",
        (0..10001)
            .map(|n| format!("\"U{n}\""))
            .collect::<Vec<_>>()
            .join(",")
    );
    for (case, damage) in [
        ("non-array inputs", entry("{}")),
        ("JSON null inputs", entry("null")),
        ("101 groups", entry(&many)),
        ("10001 labels", entry(&wide)),
        ("scalar group", entry("[1]")),
        ("extra key", entry(r#"[{"target":[],"sku":"SERIAL","serials":["One"],"x":1}]"#)),
        ("missing key", entry(r#"[{"target":[],"sku":"SERIAL"}]"#)),
        ("scalar target", entry(r#"[{"target":"A","sku":"SERIAL","serials":["One"]}]"#)),
        ("four target members", entry(r#"[{"target":["a","b","c","d"],"sku":"SERIAL","serials":["One"]}]"#)),
        ("numeric target member", entry(r#"[{"target":[1],"sku":"SERIAL","serials":["One"]}]"#)),
        ("padded target member", entry(r#"[{"target":[" a"],"sku":"SERIAL","serials":["One"]}]"#)),
        ("numeric sku", entry(r#"[{"target":[],"sku":1,"serials":["One"]}]"#)),
        ("empty sku", entry(r#"[{"target":[],"sku":"","serials":["One"]}]"#)),
        ("padded sku", entry(r#"[{"target":[],"sku":" SERIAL","serials":["One"]}]"#)),
        ("scalar serials", entry(r#"[{"target":[],"sku":"SERIAL","serials":"One"}]"#)),
        ("numeric label", entry(r#"[{"target":[],"sku":"SERIAL","serials":[1]}]"#)),
        ("nested label", entry(r#"[{"target":[],"sku":"SERIAL","serials":[["One"]]}]"#)),
        ("control label", entry(r#"[{"target":[],"sku":"SERIAL","serials":["On\u0001e"]}]"#)),
        ("unicode label", entry(r#"[{"target":[],"sku":"SERIAL","serials":["é"]}]"#)),
        ("duplicate label", entry(r#"[{"target":[],"sku":"SERIAL","serials":["One","One"]}]"#)),
        ("duplicate group", entry(r#"[{"target":[],"sku":"SERIAL","serials":["One"]},{"target":[],"sku":"SERIAL","serials":["Other"]}]"#)),
        ("wrong quantity", entry(r#"[{"target":[],"sku":"SERIAL","serials":["One","Other"]}]"#)),
        ("undeclared target", entry(r#"[{"target":["line"],"sku":"SERIAL","serials":["One"]}]"#)),
        ("other sku", entry(r#"[{"target":[],"sku":"OTHER","serials":["One"]}]"#)),
        ("missing observation", entry("[]")),
        ("text quantity", command("quantity", "\"not-a-number\"")),
        ("fractional quantity", command("quantity", "1.5")),
        ("overflow quantity", command("quantity", "9223372036854775808")),
        ("zero quantity", command("quantity", "0")),
        ("negative quantity", command("quantity", "-1")),
        ("line-bearing move", command("lines", "[]")),
        ("unknown command", command("type", "\"teleport\"")),
        ("absent command", format!("UPDATE wms_events SET payload=payload-'command' WHERE id={moved}")),
        ("scalar command", format!("UPDATE wms_events SET payload=jsonb_set(payload,'{{command}}','123') WHERE id={moved}")),
        ("count labels as move", format!("UPDATE wms_events SET payload=jsonb_set(payload,'{{serial_inputs}}',(SELECT payload->'serial_inputs' FROM wms_events WHERE id={counted})) WHERE id={moved}")),
        ("count wrong total", format!("UPDATE wms_events SET payload=jsonb_set(payload,'{{command,counted}}','3') WHERE id={counted}")),
        ("untargeted nonempty", format!("UPDATE wms_events SET payload=jsonb_set(payload,'{{serial_inputs}}','[{{\"target\":[],\"sku\":\"SERIAL\",\"serials\":[\"One\"]}}]') WHERE id={metadata}")),
        ("untargeted malformed", format!("UPDATE wms_events SET payload=jsonb_set(payload,'{{serial_inputs}}','{{}}') WHERE id={metadata}")),
        ("uncontrolled product", "UPDATE wms_products SET data=jsonb_set(data,'{serial_controlled}','false') WHERE tenant=$1 AND warehouse_id='wh-main' AND sku='SERIAL'".to_owned()),
    ] {
        sqlx::query("SAVEPOINT input_class_damage").execute(&mut *tx).await.expect("input class boundary");
        sqlx::query(&damage).bind(&f.who.tenant).execute(&mut *tx).await.expect("owned input class damage");
        compare_classes(&mut tx, &f.who.tenant, case).await;
        sqlx::query("ROLLBACK TO SAVEPOINT input_class_damage").execute(&mut *tx).await.expect("restore original input facts");
    }
    compare_classes(&mut tx, &f.who.tenant, "restored").await;
    tx.rollback().await.expect("remove every forensic bypass");
    f.proof().await;
}
