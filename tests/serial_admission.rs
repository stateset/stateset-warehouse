//! Real command transactions. All tenants, inventory and faults are synthetic
//! and live only in the explicitly isolated PostgreSQL test database.
use serde_json::{json, Value};
use stateset_warehouse::{
    concurrency,
    domain::{self, Command, State},
    serials::Input,
    store::{Identity, Request, Store},
};

struct Fixture {
    store: Store,
    who: Identity,
}
impl Fixture {
    async fn new() -> Self {
        let url = std::env::var("WMS_TEST_DATABASE_URL").expect("isolated PostgreSQL test URL");
        assert!(reqwest::Url::parse(&url)
            .expect("test URL")
            .path()
            .contains("test"));
        let store = Store::connect(&url).await.expect("current migrations");
        let who = Identity {
            tenant: format!("serial-admission-{}", domain::id()),
            actor: "synthetic:serial-operator".into(),
            role: "admin".into(),
        };
        store
            .provision(&who.tenant, false)
            .await
            .expect("empty isolated tenant");
        Self { store, who }
    }
    async fn state(&self) -> State {
        self.store
            .state(&self.who.tenant, "wh-main")
            .await
            .expect("current canonical state")
    }
    async fn request(&self, command: Value, inputs: Vec<Input>) -> Request {
        Request {
            request_id: domain::id(),
            expected_version: self.state().await.version,
            resource_versions: None,
            unit_inputs: vec![],
            serial_inputs: inputs,
            command: serde_json::from_value(command).expect("typed canonical command"),
        }
    }
    async fn execute(&self, command: Value, inputs: Vec<Input>) -> (Request, Value) {
        let request = self.request(command, inputs).await;
        plan::verify(self, "wh-main", &request).await;
        let result = self
            .store
            .execute(&self.who, "wh-main", &request)
            .await
            .expect("real atomic command");
        (request, result)
    }
    async fn plain(&self, command: Value) -> Value {
        self.execute(command, vec![]).await.1["result"]["entity_id"].clone()
    }
    async fn catalog(&self) {
        self.plain(json!({"type":"create_product","sku":"SERIAL","name":"Synthetic serialized product","category":"Synthetic","reorder_point":0,"serial_controlled":true})).await;
        for bin in ["A", "B"] {
            self.plain(
                json!({"type":"create_location","code":bin,"zone":"Synthetic","kind":"storage"}),
            )
            .await;
        }
    }
    async fn facts(&self) -> Value {
        sqlx::query_scalar("SELECT jsonb_build_object('commands',(SELECT count(*) FROM wms_commands WHERE tenant=$1),'events',(SELECT count(*) FROM wms_events WHERE tenant=$1),'movements',(SELECT count(*) FROM wms_movements WHERE tenant=$1),'outbox',(SELECT count(*) FROM wms_outbox WHERE tenant=$1),'batches',(SELECT count(*) FROM wms_serial_batches WHERE tenant=$1),'units',(SELECT count(*) FROM wms_serial_units WHERE tenant=$1),'steps',(SELECT count(*) FROM wms_serial_steps WHERE tenant=$1),'observations',(SELECT count(*) FROM wms_serial_observations WHERE tenant=$1),'heads',(SELECT jsonb_object_agg(sku,version) FROM wms_serial_heads WHERE tenant=$1))")
            .bind(&self.who.tenant).fetch_one(&self.store.pool).await.expect("independent exact tenant fact counts")
    }
    async fn unit(&self, serial: &str) -> Value {
        sqlx::query_scalar(
            "SELECT place FROM wms_serial_units WHERE tenant=$1 AND sku='SERIAL' AND serial=$2",
        )
        .bind(&self.who.tenant)
        .bind(serial)
        .fetch_one(&self.store.pool)
        .await
        .expect("actual current literal UID")
    }
    async fn proof(&self) {
        let report = self
            .store
            .reconciliation(&self.who.tenant, "wh-main")
            .await
            .expect("complete original source reconciliation");
        assert_eq!(report["ok"], true, "{report}");
        assert_eq!(report["serials"]["source_ok"], true);
        let ok: bool=sqlx::query_scalar("SELECT wms_serial_scope_current_valid($1,'wh-main') AND wms_serial_scope_transit_valid($1,'wh-main') AND NOT EXISTS(SELECT 1 FROM wms_serial_heads h WHERE tenant=$1 AND NOT wms_serial_head_bound(h)) AND NOT EXISTS(SELECT 1 FROM wms_serial_units u WHERE tenant=$1 AND NOT wms_serial_unit_bound(u)) AND NOT EXISTS(SELECT 1 FROM wms_events e WHERE tenant=$1 AND NOT wms_serial_event_valid(e.tenant,e.warehouse_id,e.id))")
            .bind(&self.who.tenant).fetch_one(&self.store.pool).await.expect("independent source, identity, history and physical equations");
        assert!(ok);
    }
}
fn observed(target: &[&str], serials: &[&str]) -> Vec<Input> {
    vec![Input {
        target: target.iter().map(|s| (*s).into()).collect(),
        sku: "SERIAL".into(),
        serials: serials.iter().map(|s| (*s).into()).collect(),
    }]
}

#[test]
fn default_serial_fields_preserve_legacy_canonical_request_and_catalog_bytes() {
    let original = r#"{"request_id":"legacy-request-0001","expected_version":7,"command":{"type":"create_product","sku":"LEGACY","name":"Legacy","category":"General","reorder_point":0}}"#;
    let request: Request = serde_json::from_str(original).expect("legacy typed request");
    assert!(request.serial_inputs.is_empty());
    assert_eq!(
        serde_json::to_string(&request).expect("canonical legacy request"),
        original
    );
    let explicit: Request=serde_json::from_value(json!({"request_id":"legacy-request-0001","expected_version":7,"serial_inputs":[],"command":{"type":"create_product","sku":"LEGACY","name":"Legacy","category":"General","reorder_point":0,"serial_controlled":false}})).expect("explicit default fields");
    assert_eq!(
        serde_json::to_string(&explicit).expect("canonical explicit defaults"),
        original
    );
    let product = r#"{"sku":"LEGACY","name":"Legacy","category":"General","reorder_point":0}"#;
    let decoded: domain::Product = serde_json::from_str(product).expect("legacy catalog");
    assert!(!decoded.serial_controlled);
    assert_eq!(
        serde_json::to_string(&decoded).expect("unchanged catalog shape"),
        product
    );
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn serial_supplier_pick_package_handoff_and_return_commit_original_case_sensitive_identity() {
    let f = Fixture::new().await;
    f.catalog().await;
    let receipt=f.plain(json!({"type":"create_receipt","reference":"Synthetic inbound","supplier":"Synthetic supplier","lines":[{"id":"INBOUND","sku":"SERIAL","lot":"LOT","quantity":2}]})).await;
    let (original_request,original_response)=f.execute(json!({"type":"receive","receipt_id":receipt,"line_id":"INBOUND","location":"RECV-01","quantity":2}),observed(&[],&["One","one"])).await;
    f.execute(json!({"type":"move_stock","sku":"SERIAL","lot":"LOT","from":"RECV-01","to":"A","quantity":2}),observed(&[],&["One","one"])).await;
    let order=f.plain(json!({"type":"create_order","reference":"Synthetic outbound","source_order_id":null,"customer":"Synthetic customer","destination":"Synthetic destination","priority":"normal","lines":[{"id":"LINE-1","sku":"SERIAL","quantity":1},{"id":"LINE-2","sku":"SERIAL","quantity":1}]})).await;
    f.plain(json!({"type":"allocate_available","order_id":order}))
        .await;
    for (line, uid) in [("LINE-1", "One"), ("LINE-2", "one")] {
        f.execute(json!({"type":"pick","order_id":order,"line_id":line,"location":"A","lot":"LOT","quantity":1}),observed(&[],&[uid])).await;
    }
    let wrong=f.request(json!({"type":"unpick","order_id":order,"line_id":"LINE-1","location":"A","lot":"LOT","quantity":1,"reason":"Synthetic wrong owner"}),observed(&[],&["one"])).await;
    let before = f.facts().await;
    let error = f
        .store
        .execute(&f.who, "wh-main", &wrong)
        .await
        .expect_err("same-SKU units retain exact line ownership");
    assert_eq!(error.status, 422);
    assert_eq!(f.facts().await, before);
    let (_,package)=f.execute(json!({"type":"pack_shipment","order_id":order,"reference":"Synthetic package","lines":[{"line_id":"LINE-1","location":"A","lot":"LOT","quantity":1}]}),observed(&["LINE-1","A","LOT"],&["One"])).await;
    let package = &package["result"]["entity_id"];
    f.plain(json!({"type":"handoff_shipment","shipment_id":package,"carrier":"Synthetic carrier","handoff_reference":"SYNTHETIC-HANDOFF"})).await;
    assert_eq!(f.unit("One").await["status"], "shipped");
    assert_eq!(f.unit("one").await["line_id"], "LINE-2");
    let (_,returned)=f.execute(json!({"type":"create_return","reference":"Synthetic RMA","order_id":order,"reason":"Observed original unit","lines":[{"id":"RETURN-LINE","order_line_id":"LINE-1","lot":"LOT","quantity":1}]}),observed(&["RETURN-LINE"],&["One"])).await;
    f.execute(json!({"type":"receive","receipt_id":returned["result"]["entity_id"],"line_id":"RETURN-LINE","location":"RECV-01","quantity":1}),observed(&[],&["One"])).await;
    assert_eq!(
        f.unit("One").await,
        json!({"status":"on_hand","warehouse_id":"wh-main","location":"RECV-01"})
    );
    let facts = f.facts().await;
    let replay = f
        .store
        .execute(&f.who, "wh-main", &original_request)
        .await
        .expect("exact original supplier request remains replayable after handoff and return");
    assert_eq!(replay["replayed"], true);
    assert_eq!(replay["result"], original_response["result"]);
    assert_eq!(f.facts().await, facts);
    f.proof().await;
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn serial_final_outbox_failure_rolls_back_every_identity_fact_and_allows_exact_original_retry(
) {
    let f = Fixture::new().await;
    f.catalog().await;
    let request=f.request(json!({"type":"count_stock","sku":"SERIAL","lot":"LOT","location":"A","counted":2,"reason":"Synthetic observed identities"}),observed(&[],&["First","Second"])).await;
    let state = f.state().await;
    let before = f.facts().await;
    let name = format!("serial_outbox_fault_{}", domain::id().replace('-', ""));
    // The only interpolated identifiers/tenant are generated UUID strings.
    let ddl=format!("CREATE FUNCTION {name}() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'Synthetic final outbox failure'; END $$; CREATE TRIGGER {name} BEFORE INSERT ON wms_outbox FOR EACH ROW WHEN(NEW.tenant='{}') EXECUTE FUNCTION {name}()",f.who.tenant);
    sqlx::raw_sql(&ddl)
        .execute(&f.store.pool)
        .await
        .expect("owned tenant-specific final-write fault");
    let error = f
        .store
        .execute(&f.who, "wh-main", &request)
        .await
        .expect_err("final delivery intent must be atomic with serial custody");
    assert_eq!(error.status, 503);
    assert_eq!(f.state().await, state);
    assert_eq!(f.facts().await, before);
    assert_eq!(
        f.store
            .receipt(&f.who, "wh-main", &request.request_id)
            .await
            .expect("original request recovery")["found"],
        false
    );
    sqlx::raw_sql(&format!(
        "DROP TRIGGER {name} ON wms_outbox; DROP FUNCTION {name}()"
    ))
    .execute(&f.store.pool)
    .await
    .expect("remove only this owned synthetic fault");
    let saved = f
        .store
        .execute(&f.who, "wh-main", &request)
        .await
        .expect("exact original request succeeds after final-write recovery");
    assert_eq!(saved["replayed"], false);
    let committed = f.facts().await;
    let replay = f
        .store
        .execute(&f.who, "wh-main", &request)
        .await
        .expect("exact retry has no second physical/serial effect");
    assert_eq!(replay["replayed"], true);
    assert_eq!(replay["result"], saved["result"]);
    assert_eq!(f.facts().await, committed);
    f.proof().await;
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn equal_quantity_serial_replacement_fences_scoped_reviews_and_preserves_full_observed_sets()
{
    let f = Fixture::new().await;
    f.catalog().await;
    f.execute(json!({"type":"count_stock","sku":"SERIAL","lot":"LOT","location":"A","counted":2,"reason":"Opening observed set"}),observed(&[],&["A","B"])).await;
    let before = f.state().await;
    let command: Command = serde_json::from_value(
        json!({"type":"move_stock","sku":"SERIAL","lot":"LOT","from":"A","to":"B","quantity":1}),
    )
    .expect("scoped move");
    let stale = Request {
        request_id: domain::id(),
        expected_version: before.version,
        resource_versions: concurrency::reviewed_versions(&before, &command).expect("exact review"),
        unit_inputs: vec![],
        serial_inputs: observed(&[], &["A"]),
        command,
    };
    f.execute(json!({"type":"count_stock","sku":"SERIAL","lot":"LOT","location":"A","counted":2,"reason":"One old unit absent, one newly observed"}),observed(&[],&["A","C"])).await;
    assert_eq!(f.unit("B").await["status"], "missing");
    assert_eq!(f.unit("C").await["status"], "on_hand");
    let facts = f.facts().await;
    let error = f
        .store
        .execute(&f.who, "wh-main", &stale)
        .await
        .expect_err("unchanged quantity cannot preserve a stale physical review");
    assert_eq!(error.status, 409);
    assert!(error.command_outcome.is_some());
    assert_eq!(f.facts().await, facts);
    f.proof().await;
}

#[path = "serial_admission/extended.rs"]
mod extended;

#[path = "serial_admission/reads.rs"]
mod reads;

#[path = "serial_admission/plan.rs"]
mod plan;

#[path = "serial_admission/restore.rs"]
mod restore;

#[path = "serial_admission/target_resolution.rs"]
mod target_resolution;

#[path = "serial_admission/resolved_sources.rs"]
mod resolved_sources;
#[path = "serial_admission/scoped_references.rs"]
mod scoped_references;
#[path = "serial_admission/shared_sources.rs"]
mod shared_sources;

#[path = "serial_admission/payload_reuse.rs"]
mod payload_reuse;
