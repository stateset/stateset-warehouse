//! Differential source-bound validation against the immutable schema21 predicate.
use super::*;
use sqlx::{PgConnection, Row};
async fn original(c: &mut PgConnection) {
    sqlx::query("CREATE TEMP TABLE serial_target_oracle_marker(n INTEGER) ON COMMIT DROP")
        .execute(&mut *c)
        .await
        .expect("owned temporary namespace");
    let source = include_str!("../../migrations/021_serial_tracking.sql");
    let text_start = source
        .find("CREATE FUNCTION wms_serial_text(")
        .expect("original text predicate");
    let text_end = text_start + source[text_start..].find("$$;").expect("text body") + 3;
    sqlx::raw_sql(&source[text_start..text_end].replacen(
        "wms_serial_text(",
        "pg_temp.wms_serial_text_original(",
        1,
    ))
    .execute(&mut *c)
    .await
    .expect("unchanged original Unicode and byte rules");
    let targets_start = source
        .find("CREATE FUNCTION wms_serial_targets(")
        .expect("original target resolver");
    let targets_end = targets_start + source[targets_start..].find("$$;").expect("target body") + 3;
    sqlx::raw_sql(&source[targets_start..targets_end].replacen(
        "wms_serial_targets(",
        "pg_temp.wms_serial_targets_original(",
        1,
    ))
    .execute(&mut *c)
    .await
    .expect("immutable complete target resolver for independent input oracle");
    let start = source
        .find("CREATE FUNCTION wms_serial_inputs_match_command(")
        .expect("original predicate start");
    let tail = &source[start..];
    let end = tail
        .find("\nCREATE FUNCTION wms_serial_event_inputs_bound")
        .expect("original predicate end");
    let sql = tail[..end]
        .replacen(
            "CREATE FUNCTION wms_serial_inputs_match_command(",
            "CREATE FUNCTION pg_temp.wms_serial_inputs_original(",
            1,
        )
        .replace("wms_serial_text(", "pg_temp.wms_serial_text_original(")
        .replace(
            "wms_serial_targets(",
            "pg_temp.wms_serial_targets_original(",
        );
    sqlx::raw_sql(&sql)
        .execute(c)
        .await
        .expect("independent unmodified original predicate");
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn direct_serial_targets_preserve_original_rows_duplicates_nulls_and_conversion_errors() {
    let f = Fixture::new().await;
    f.catalog().await;
    let mut tx = f
        .store
        .pool
        .begin()
        .await
        .expect("owned target differential");
    original(&mut tx).await;
    sqlx::raw_sql(
        r#"
        CREATE FUNCTION pg_temp.serial_target_outcome(t TEXT,w TEXT,c JSONB,candidate BOOLEAN)
        RETURNS JSONB LANGUAGE plpgsql STABLE AS $$
        DECLARE rows JSONB;
        BEGIN
          IF candidate THEN
            SELECT COALESCE(jsonb_agg(to_jsonb(q) ORDER BY to_jsonb(q)::text),'[]'::jsonb)
              INTO rows FROM wms_serial_targets(t,w,c) q;
          ELSE
            SELECT COALESCE(jsonb_agg(to_jsonb(q) ORDER BY to_jsonb(q)::text),'[]'::jsonb)
              INTO rows FROM pg_temp.wms_serial_targets_original(t,w,c) q;
          END IF;
          RETURN jsonb_build_object('rows',rows);
        EXCEPTION WHEN OTHERS THEN
          RETURN jsonb_build_object('sqlstate',SQLSTATE);
        END $$;
    "#,
    )
    .execute(&mut *tx)
    .await
    .expect("independent rows and SQLSTATE capture");
    let quantities = vec![
        json!(null),
        json!(false),
        json!({}),
        json!([]),
        json!(""),
        json!("bad"),
        json!(0),
        json!(1),
        json!(-1),
        json!(1.0),
        json!(1.5),
        json!("01"),
        json!(" 1 "),
        json!("1e2"),
        json!(i64::MIN),
        json!(i64::MAX),
        json!("9223372036854775808"),
        json!("-9223372036854775809"),
    ];
    let mut commands = vec![
        json!(null),
        json!([]),
        json!({}),
        json!("move_stock"),
        json!(12),
        json!({"type":12}),
        json!({"type":null}),
    ];
    for kind in [
        "move_stock",
        "count_stock",
        "receive",
        "pick",
        "unpick",
        "create_return",
        "pack_shipment",
        "dispatch_transfer",
        "receive_transfer",
        "record_transfer_loss",
        "reverse_transfer_loss",
        "create_order",
        "create_receipt",
        "ship",
        "unpack_shipment",
        "handoff_shipment",
        "cancel_receipt",
        "unknown",
        "",
    ] {
        commands.push(json!({"type":kind}));
        for quantity in &quantities {
            let base = json!({"type":kind,"sku":"SERIAL","quantity":quantity,"counted":quantity,
                "receipt_id":"missing","order_id":"missing","line_id":"missing","transfer_id":"missing","loss_id":"missing"});
            commands.push(base.clone());
            if matches!(kind, "move_stock" | "count_stock") {
                let mut ordinary = base.clone();
                ordinary
                    .as_object_mut()
                    .expect("command object")
                    .remove(if kind == "move_stock" {
                        "counted"
                    } else {
                        "quantity"
                    });
                commands.push(ordinary);
            }
            for lines in [
                json!(null),
                json!({}),
                json!([
                    {"id":"duplicate","sku":"SERIAL","quantity":quantity,"line_id":"missing","order_line_id":"missing","location":"A","lot":"LOT"},
                    {"id":"duplicate","sku":"SERIAL","quantity":quantity,"line_id":"missing","order_line_id":"missing","location":"A","lot":"LOT"}
                ]),
            ] {
                let mut command = base.clone();
                command["lines"] = lines;
                commands.push(command);
            }
        }
    }
    for command in [
        json!({"type":"move_stock","sku":null,"quantity":1,"counted":"irrelevant"}),
        json!({"type":"count_stock","sku":12,"counted":0,"quantity":"irrelevant"}),
        json!({"type":"move_stock","sku":"Case-sensitive \\ 中","quantity":"+2"}),
    ] {
        commands.push(command);
    }
    // The original UNION resolver's inactive-arm casts raise 22P02 under a
    // custom plan but not under a generic plan, and PL/pgSQL switches plans
    // after repeated calls. Compare both resolvers under each fixed mode so
    // unequal plan-cache histories cannot produce a false difference.
    for mode in ["force_custom_plan", "force_generic_plan"] {
        sqlx::query(&format!("SET LOCAL plan_cache_mode={mode}"))
            .execute(&mut *tx)
            .await
            .expect("fixed plan mode");
        for batch in commands.chunks(32) {
            let differences: Value = sqlx::query_scalar(
                r#"
            SELECT COALESCE(jsonb_agg(jsonb_build_object('command',c,'old',old,'new',new)),'[]')
            FROM (SELECT c,pg_temp.serial_target_outcome($1,'wh-main',c,false) AS old,
                pg_temp.serial_target_outcome($1,'wh-main',c,true) AS new
                FROM jsonb_array_elements($2) c) compared WHERE old IS DISTINCT FROM new
        "#,
            )
            .bind(&f.who.tenant)
            .bind(json!(batch))
            .fetch_one(&mut *tx)
            .await
            .expect("all raw target vectors");
            assert_eq!(
                differences,
                json!([]),
                "target rows or conversion errors changed under {mode}"
            );
        }
    }
    let same:bool=sqlx::query_scalar("SELECT pg_temp.serial_target_outcome($1,'wh-main',NULL,false) IS NOT DISTINCT FROM pg_temp.serial_target_outcome($1,'wh-main',NULL,true)")
        .bind(&f.who.tenant).fetch_one(&mut *tx).await.expect("SQL NULL target envelope");
    assert!(same);
    tx.rollback()
        .await
        .expect("remove independent temporary oracle");
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn native_ascii_subset_preserves_unicode_bytes_controls_and_malformed_label_shapes() {
    let f = Fixture::new().await;
    f.catalog().await;
    let mut tx = f
        .store
        .pool
        .begin()
        .await
        .expect("literal differential audit");
    original(&mut tx).await;
    let differences: i64 = sqlx::query_scalar(r#"
        WITH samples AS (
          SELECT chr(a)||chr(b) AS label FROM generate_series(1,127) a CROSS JOIN generate_series(1,127) b
          UNION ALL SELECT chr(n) FROM generate_series(1,1024) n
          UNION ALL SELECT chr(n)||'Unit'||chr(n) FROM unnest(ARRAY[133,160,5760,8192,8193,8194,8195,8196,8197,8198,8199,8200,8201,8202,8232,8233,8239,8287,12288]) n
          UNION ALL SELECT v FROM (VALUES(NULL::text),(''),(repeat('a',240)),(repeat('a',241)),(repeat('中',80)),(repeat('中',81)),('😀')) x(v)
        ) SELECT count(*) FROM samples WHERE wms_serial_text(label) IS DISTINCT FROM pg_temp.wms_serial_text_original(label)
    "#).fetch_one(&mut *tx).await.expect("complete literal predicate comparison");
    assert_eq!(differences, 0);
    let command = json!({"type":"count_stock","sku":"SERIAL","lot":"LOT","location":"A","counted":1,"reason":"Literal source check"});
    for (label, valid) in [
        (json!("Valid"), true),
        (json!("two words"), true),
        (json!("a\"\\b"), true),
        (json!("中😀"), true),
        (json!("a".repeat(240)), true),
        (json!("a".repeat(241)), false),
        (json!("中".repeat(80)), true),
        (json!("中".repeat(81)), false),
        (json!(""), false),
        (json!(" a"), false),
        (json!("a "), false),
        (json!("a\n"), false),
        (json!("a\u{85}b"), false),
        (json!("\u{a0}a"), false),
        (json!("a\u{a0}b"), true),
        (json!(null), false),
        (json!(1), false),
        (json!(true), false),
        (json!([]), false),
        (json!(["Valid"]), false),
        (json!([["Valid"]]), false),
        (json!({"label":"Valid"}), false),
    ] {
        compare(
            &mut tx,
            &f.who.tenant,
            &command,
            &json!([{"target":[],"sku":"SERIAL","serials":[label]}]),
            valid,
        )
        .await;
    }
    let labels: Vec<String> = (0..10000).map(|n| format!("Unit-{n}")).collect();
    let mut large = command.clone();
    large["counted"] = json!(10000);
    compare(
        &mut tx,
        &f.who.tenant,
        &large,
        &json!([{"target":[],"sku":"SERIAL","serials":labels}]),
        true,
    )
    .await;
    let mut duplicate = labels.clone();
    duplicate[9999] = duplicate[0].clone();
    compare(
        &mut tx,
        &f.who.tenant,
        &large,
        &json!([{"target":[],"sku":"SERIAL","serials":duplicate}]),
        false,
    )
    .await;
    tx.rollback()
        .await
        .expect("remove private independent oracle");
}
async fn compare(
    c: &mut PgConnection,
    tenant: &str,
    command: &Value,
    inputs: &Value,
    expected: bool,
) {
    let (current,old):(bool,bool)=sqlx::query_as("SELECT wms_serial_inputs_match_command($1,'wh-main',$2,$3),pg_temp.wms_serial_inputs_original($1,'wh-main',$2,$3)").bind(tenant).bind(command).bind(inputs).fetch_one(c).await.expect("compare complete original target semantics");
    assert_eq!(
        current, old,
        "original predicate disagreement command{command}, inputs{inputs}"
    );
    assert_eq!(current, expected, "expected target validation outcome");
}
#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn resolved_target_set_matches_original_root_group_shape_and_exact_count_rules() {
    let f = Fixture::new().await;
    f.catalog().await;
    let mut tx = f
        .store
        .pool
        .begin()
        .await
        .expect("owned differential transaction");
    original(&mut tx).await;
    let command = json!({"type":"count_stock","sku":"SERIAL","lot":"LOT","location":"A","counted":2,"reason":"Synthetic complete original set"});
    let good = json!([{"target":[],"sku":"SERIAL","serials":["Case","case"]}]);
    compare(&mut tx, &f.who.tenant, &command, &good, true).await;
    for bad in [
        json!(null),
        json!({}),
        json!([]),
        json!([{"target":[],"sku":"SERIAL","serials":["Case"]}]),
        json!([{"target":[],"sku":"OTHER","serials":["Case","case"]}]),
        json!([{"target":["wrong"],"sku":"SERIAL","serials":["Case","case"]}]),
        json!([{"target":[],"sku":"SERIAL","serials":["Case","Case"]}]),
        json!([{"target":[],"sku":"SERIAL","serials":[" leading","case"]}]),
        json!([{"target":[],"sku":"SERIAL","serials":["Case","case"],"extra":true}]),
        json!([{"target":[],"sku":"SERIAL","serials":null}]),
        json!([{"target":[],"sku":"SERIAL","serials":["Case",1]}]),
    ] {
        compare(&mut tx, &f.who.tenant, &command, &bad, false).await;
    }
    let mut zero = command.clone();
    zero["counted"] = json!(0);
    compare(
        &mut tx,
        &f.who.tenant,
        &zero,
        &json!([{"target":[],"sku":"SERIAL","serials":[]}]),
        true,
    )
    .await;
    let move_zero =
        json!({"type":"move_stock","sku":"SERIAL","lot":"LOT","from":"A","to":"B","quantity":0});
    compare(
        &mut tx,
        &f.who.tenant,
        &move_zero,
        &json!([{"target":[],"sku":"SERIAL","serials":[]}]),
        false,
    )
    .await;
    let unrelated =
        json!({"type":"create_location","code":"NEW","zone":"Synthetic","kind":"storage"});
    compare(&mut tx, &f.who.tenant, &unrelated, &json!([]), true).await;
    let stored =
        sqlx::query("SELECT digest FROM wms_schema_migrations WHERE name='021_serial_tracking'")
            .fetch_one(&mut *tx)
            .await
            .expect("unchanged original migration digest");
    assert!(!stored.get::<String, _>("digest").is_empty());
    tx.rollback().await.expect("remove temporary oracle");
}
#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn hundred_original_line_targets_match_original_predicate_and_actual_dispatch() {
    let f = Fixture::new().await;
    f.catalog().await;
    extended::other_warehouse(&f).await;
    let labels: Vec<String> = (0..100).map(|n| format!("Original-{n:03}")).collect();
    f.execute(json!({"type":"count_stock","sku":"SERIAL","lot":"LOT","location":"A","counted":100,"reason":"Synthetic100original units"}),vec![Input{target:vec![],sku:"SERIAL".into(),serials:labels.clone()}]).await;
    let lines:Vec<Value>=(0..100).map(|n|json!({"id":format!("line-{n:03}"),"sku":"SERIAL","lot":"LOT","location":"A","quantity":1})).collect();
    let command = json!({"type":"dispatch_transfer","reference":"HUNDRED-ORIGINAL-LINES","destination_warehouse_id":"wh-other","carrier":"Synthetic","handoff_reference":"ORIGINAL-HANDOFF","lines":lines});
    let inputs: Vec<Input> = (0..100)
        .map(|n| Input {
            target: vec![format!("line-{n:03}")],
            sku: "SERIAL".into(),
            serials: vec![labels[n].clone()],
        })
        .collect();
    let good = json!(&inputs);
    let mut tx = f
        .store
        .pool
        .begin()
        .await
        .expect("owned100target comparison");
    original(&mut tx).await;
    compare(&mut tx, &f.who.tenant, &command, &good, true).await;
    let mut missing = good.clone();
    missing.as_array_mut().expect("groups").pop();
    compare(&mut tx, &f.who.tenant, &command, &missing, false).await;
    let mut duplicate = good.clone();
    duplicate[99] = duplicate[0].clone();
    compare(&mut tx, &f.who.tenant, &command, &duplicate, false).await;
    let mut reused = good.clone();
    reused[99]["serials"] = reused[0]["serials"].clone();
    compare(&mut tx, &f.who.tenant, &command, &reused, false).await;
    let mut wrong = good.clone();
    wrong[99]["target"] = json!(["absent-line"]);
    compare(&mut tx, &f.who.tenant, &command, &wrong, false).await;
    let mut duplicated_command = command.clone();
    duplicated_command["lines"][99] = duplicated_command["lines"][0].clone();
    compare(&mut tx, &f.who.tenant, &duplicated_command, &good, false).await;
    tx.rollback().await.expect("remove comparison oracle");
    f.execute(command, inputs).await;
    f.proof().await;
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn event_observation_set_matches_original_on_zero_missing_changed_and_extra_rows() {
    let f = Fixture::new().await;
    f.catalog().await;
    let (count,_)=f.execute(json!({"type":"count_stock","sku":"SERIAL","lot":"LOT","location":"A","counted":1,"reason":"Original nonempty observation"}),observed(&[],&["Original"])).await;
    let (zero,_)=f.execute(json!({"type":"count_stock","sku":"SERIAL","lot":"LOT","location":"B","counted":0,"reason":"Original explicit empty group"}),observed(&[],&[])).await;
    let mut tx = f
        .store
        .pool
        .begin()
        .await
        .expect("owned original observation oracle");
    original(&mut tx).await;
    let source = include_str!("../../migrations/021_serial_tracking.sql");
    let start = source
        .find("CREATE FUNCTION wms_serial_event_inputs_bound(")
        .expect("original event inputs");
    let tail = &source[start..];
    let end = tail
        .find("\nCREATE OR REPLACE FUNCTION wms_serial_observation_bound")
        .expect("original metadata function end");
    let sql = tail[..end].replacen(
        "CREATE FUNCTION wms_serial_event_inputs_bound(",
        "CREATE FUNCTION pg_temp.wms_serial_event_inputs_original(",
        1,
    );
    sqlx::raw_sql(&sql)
        .execute(&mut *tx)
        .await
        .expect("original complete group/ordinal/head comparison");
    let events: Vec<String> = sqlx::query_scalar(
        "SELECT id FROM wms_events WHERE tenant=$1 AND payload->>'request_id'=ANY($2)",
    )
    .bind(&f.who.tenant)
    .bind(vec![count.request_id, zero.request_id])
    .fetch_all(&mut *tx)
    .await
    .expect("two original observation events");
    assert_eq!(events.len(), 2);
    for event in &events {
        let pair:(bool,bool)=sqlx::query_as("SELECT wms_serial_event_inputs_bound($1,'wh-main',$2),pg_temp.wms_serial_event_inputs_original($1,'wh-main',$2)").bind(&f.who.tenant).bind(event).fetch_one(&mut *tx).await.expect("original nonempty/zero source");
        assert_eq!(pair, (true, true));
    }
    sqlx::raw_sql("ALTER TABLE wms_serial_observations DISABLE TRIGGER wms_serial_observation_immutable; ALTER TABLE wms_events DISABLE TRIGGER wms_events_immutable").execute(&mut *tx).await.expect("named corruption guards in rollback-only fixture");
    for sql in [
        "DELETE FROM wms_serial_observations WHERE tenant=$1 AND event_id=$2",
        "UPDATE wms_serial_observations SET ordinal=99 WHERE tenant=$1 AND event_id=$2",
        "UPDATE wms_serial_observations SET target='[\"foreign\"]' WHERE tenant=$1 AND event_id=$2",
        "UPDATE wms_serial_observations SET serials='[\"forged\"]' WHERE tenant=$1 AND event_id=$2",
        "INSERT INTO wms_serial_observations(tenant,warehouse_id,event_id,ordinal,sku,sku_version,target,serials) SELECT tenant,warehouse_id,event_id,99,sku,sku_version,jsonb_build_array('extra'),serials FROM wms_serial_observations WHERE tenant=$1 AND event_id=$2",
        "UPDATE wms_events SET payload=jsonb_set(payload,'{serial_heads,SERIAL}','999') WHERE tenant=$1 AND id=$2",
    ] {
        for event in &events {sqlx::query("SAVEPOINT original_observation_damage").execute(&mut *tx).await.expect("damage boundary");sqlx::query(sql).bind(&f.who.tenant).bind(event).execute(&mut *tx).await.expect("retained source damage");let pair:(bool,bool)=sqlx::query_as("SELECT wms_serial_event_inputs_bound($1,'wh-main',$2),pg_temp.wms_serial_event_inputs_original($1,'wh-main',$2)").bind(&f.who.tenant).bind(event).fetch_one(&mut *tx).await.expect("differential damaged source result");assert_eq!(pair,(false,false),"{sql}");sqlx::query("ROLLBACK TO SAVEPOINT original_observation_damage").execute(&mut *tx).await.expect("restore exact original source");}
    }
    tx.rollback()
        .await
        .expect("remove oracle and named corruption fixtures");
    f.proof().await;
}
