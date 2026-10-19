//! Compare shared event proofs against the exact previous complete query and
//! standalone row predicates, including committed, owned forensic damage.
use super::*;
use sqlx::{PgConnection, Row};
async fn compare(c: &mut PgConnection, tenant: &str, w: &str, expected: bool) {
    let previous: Value =
        sqlx::query_scalar(include_str!("../fixtures/serial_reconciliation_v23.sql"))
            .bind(tenant)
            .bind(w)
            .fetch_one(&mut *c)
            .await
            .expect("original complete source query");
    let current: Value = sqlx::query_scalar(include_str!("../../src/serial_reconciliation.sql"))
        .bind(tenant)
        .bind(w)
        .fetch_one(c)
        .await
        .expect("shared source query");
    assert_eq!(current, previous, "complete proof changed for scope {w}");
    assert_eq!(
        current["ok"], expected,
        "expected independent original outcome"
    );
}
async fn fixture() -> Fixture {
    let f = Fixture::new().await;
    f.catalog().await;
    extended::other_warehouse(&f).await;
    f.execute(json!({"type":"count_stock","sku":"SERIAL","lot":"LOT","location":"A","counted":2,"reason":"Synthetic complete originals"}),observed(&[],&["One","Other"])).await;
    for (from, to) in [("A", "B"), ("B", "A")] {
        f.execute(json!({"type":"move_stock","sku":"SERIAL","lot":"LOT","from":from,"to":to,"quantity":1}),observed(&[],&["One"])).await;
    }
    f
}
#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn shared_full_proof_matches_original_for_projection_orphan_and_original_source_damage() {
    let f = fixture().await;
    let mut tx = f
        .store
        .pool
        .begin()
        .await
        .expect("owned proof differential");
    for w in ["wh-main", "wh-other"] {
        compare(&mut tx, &f.who.tenant, w, true).await;
    }
    sqlx::raw_sql("ALTER TABLE wms_serial_units DISABLE TRIGGER wms_serial_unit_identity; ALTER TABLE wms_serial_steps DISABLE TRIGGER wms_serial_step_immutable; ALTER TABLE wms_serial_observations DISABLE TRIGGER wms_serial_observation_immutable; ALTER TABLE wms_events DISABLE TRIGGER wms_events_immutable").execute(&mut *tx).await.expect("named rollback-only forensic bypasses");
    for sql in [
        "UPDATE wms_serial_units SET lot='forged' WHERE tenant=$1 AND serial='One'",
        "UPDATE wms_serial_units u SET revision=1,last_event_id=s.event_id,place=s.after_place FROM wms_serial_steps s WHERE u.tenant=$1 AND u.serial='One' AND s.tenant=u.tenant AND s.sku=u.sku AND s.serial=u.serial AND s.revision=1",
        "UPDATE wms_serial_units SET last_event_id=(SELECT event_id FROM wms_serial_steps WHERE tenant=$1 AND serial='Other') WHERE tenant=$1 AND serial='One'",
        "UPDATE wms_serial_steps SET actor='forged' WHERE tenant=$1 AND serial='Other'",
        "UPDATE wms_serial_steps SET warehouse_id='wh-other' WHERE tenant=$1 AND serial='Other'",
        "UPDATE wms_serial_observations SET ordinal=99 WHERE tenant=$1 AND sku_version=1",
        "UPDATE wms_serial_observations SET warehouse_id='wh-other' WHERE tenant=$1 AND sku_version=1",
        "DELETE FROM wms_serial_observations WHERE tenant=$1 AND sku_version=1",
        "UPDATE wms_events SET payload=jsonb_set(payload,'{serial_inputs,0,serials}','[\"forged\"]') WHERE tenant=$1 AND payload#>>'{command,type}'='count_stock'",
    ] {
        sqlx::query("SAVEPOINT shared_source_damage").execute(&mut *tx).await.expect("exact forensic boundary");sqlx::query(sql).bind(&f.who.tenant).execute(&mut *tx).await.expect("owned damaged original facts");
        for w in ["wh-main","wh-other"]{compare(&mut tx,&f.who.tenant,w,false).await;}
        sqlx::query("ROLLBACK TO SAVEPOINT shared_source_damage").execute(&mut *tx).await.expect("restore original data");
    }
    tx.rollback().await.expect("remove forensic bypasses");
    f.proof().await;
}
fn app(f: &Fixture) -> axum::Router {
    stateset_warehouse::api::router(stateset_warehouse::api::App {
        store: f.store.clone(),
        demo: false,
        connections: stateset_warehouse::integrations::Connections::default(),
        credentials: std::sync::Arc::new(vec![(
            "reader".into(),
            Identity {
                role: "viewer".into(),
                ..f.who.clone()
            },
        )]),
    })
}
async fn flags(f: &Fixture, app: &axum::Router, expected_bad: bool) {
    let rows=sqlx::query("SELECT u.serial,wms_serial_unit_bound(u) AND wms_serial_place_references_valid(u.tenant,u.sku,u.lot,u.place) AND wms_serial_event_valid(e.tenant,e.warehouse_id,e.id) AS ok FROM wms_serial_units u JOIN wms_events e ON e.id=u.last_event_id WHERE u.tenant=$1 ORDER BY serial").bind(&f.who.tenant).fetch_all(&f.store.pool).await.expect("original selected current source oracle");
    let (status, page) = extended::http(
        app,
        "GET",
        "/v1/serials?sku=SERIAL&limit=50",
        "reader",
        None,
    )
    .await;
    assert_eq!(status, 200, "{page}");
    assert_eq!(page["page_source_ok"], !expected_bad);
    assert_eq!(
        page["items"].as_array().expect("current records").len(),
        rows.len()
    );
    for row in rows {
        let serial: String = row.get("serial");
        let record = page["items"]
            .as_array()
            .expect("records")
            .iter()
            .find(|r| r["serial"] == serial)
            .expect("same original unit");
        assert_eq!(record["source_ok"], row.get::<bool, _>("ok"));
    }
    for (path,sql) in [("/v1/serial-history?sku=SERIAL&serial=One&limit=50","SELECT s.revision::text AS key,wms_serial_step_bound(s) AND wms_serial_event_valid(e.tenant,e.warehouse_id,e.id) AS ok FROM wms_serial_steps s JOIN wms_events e ON e.id=s.event_id WHERE s.tenant=$1 AND s.serial='One' ORDER BY s.revision"),("/v1/serial-observations?sku=SERIAL&serial=One&limit=50","SELECT o.sku_version::text AS key,wms_serial_observation_bound(o) AND wms_serial_event_valid(e.tenant,e.warehouse_id,e.id) AS ok FROM wms_serial_observations o JOIN wms_events e ON e.id=o.event_id WHERE o.tenant=$1 AND o.serials @> '[\"One\"]' ORDER BY o.sku_version")]{
        let rows=sqlx::query(sql).bind(&f.who.tenant).fetch_all(&f.store.pool).await.expect("original selected history/observation oracle");let(status,page)=extended::http(app,"GET",path,"reader",None).await;assert_eq!(status,200,"{page}");assert_eq!(page["page_source_ok"],!expected_bad);
        for row in rows {let key:String=row.get("key");let record=page["items"].as_array().expect("records").iter().find(|r|r[if path.contains("serial-history"){"revision"}else{"sku_version"}].as_i64()==key.parse::<i64>().ok()).expect("same selected original source");assert_eq!(record["source_ok"],row.get::<bool,_>("ok"));}
        if path.contains("serial-history"){assert_eq!(page["current_source_ok"],true,"standalone current source remains independent of a different unit's old damage");}
    }
}
#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn shared_selected_sources_keep_scope_false_flags_and_refresh_after_original_repair() {
    let f = fixture().await;
    let app = app(&f);
    flags(&f, &app, false).await;
    let original: String =
        sqlx::query_scalar("SELECT actor FROM wms_serial_steps WHERE tenant=$1 AND serial='Other'")
            .bind(&f.who.tenant)
            .fetch_one(&f.store.pool)
            .await
            .expect("literal retained original actor");
    let head: i64 =
        sqlx::query_scalar("SELECT version FROM wms_serial_heads WHERE tenant=$1 AND sku='SERIAL'")
            .bind(&f.who.tenant)
            .fetch_one(&f.store.pool)
            .await
            .expect("original global review");
    for (actor, bad) in [("forged", true), (original.as_str(), false)] {
        let mut tx = f
            .store
            .pool
            .begin()
            .await
            .expect("owned committed forensic fixture");
        sqlx::query("ALTER TABLE wms_serial_steps DISABLE TRIGGER wms_serial_step_immutable")
            .execute(&mut *tx)
            .await
            .expect("named immutable guard bypass");
        sqlx::query("UPDATE wms_serial_steps SET actor=$2 WHERE tenant=$1 AND serial='Other'")
            .bind(&f.who.tenant)
            .bind(actor)
            .execute(&mut *tx)
            .await
            .expect("damage or restore exact original actor");
        sqlx::query("ALTER TABLE wms_serial_steps ENABLE TRIGGER wms_serial_step_immutable")
            .execute(&mut *tx)
            .await
            .expect("restore immutable protection");
        tx.commit()
            .await
            .expect("publish only owned forensic fixture");
        flags(&f, &app, bad).await;
        let same: i64 = sqlx::query_scalar(
            "SELECT version FROM wms_serial_heads WHERE tenant=$1 AND sku='SERIAL'",
        )
        .bind(&f.who.tenant)
        .fetch_one(&f.store.pool)
        .await
        .expect("unchanged global review");
        assert_eq!(same, head);
    }
    f.proof().await;
}
