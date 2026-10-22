//! Owned synthetic serial cost diagnostic; not sustained capacity or a facility SLO.
//! Writes use the actual Store contract and reads the authenticated API router.
use axum::{body::Body, http::Request as HttpRequest, Router};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use stateset_warehouse::{
    api::{self, App},
    domain::{id, Command},
    integrations::Connections,
    serials::Input,
    store::{Identity, Request, Store},
};
use std::{env, fs, path::PathBuf, sync::Arc, time::Instant};
use tower::ServiceExt;
type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
fn require(ok: bool, message: &str) -> Result<()> {
    if ok {
        Ok(())
    } else {
        Err(message.into())
    }
}
fn application<T>(r: stateset_warehouse::store::Result<T>) -> Result<T> {
    r.map_err(|e| format!("{}: {}", e.status, e.message).into())
}
struct Fixture {
    store: Store,
    who: Identity,
    router: Router,
    labels: Vec<String>,
    version: i64,
    originals: Vec<(Request, Value)>,
}
impl Fixture {
    async fn new(store: &Store, units: usize) -> Result<Self> {
        let who = Identity {
            tenant: format!("serial-profile-{}", id()),
            actor: "synthetic:serial-profile".into(),
            role: "admin".into(),
        };
        application(store.provision(&who.tenant, false).await)?;
        let router = api::router(App {
            store: store.clone(),
            demo: false,
            connections: Connections::default(),
            credentials: Arc::new(vec![(
                "synthetic-serial-profile-reader".into(),
                Identity {
                    role: "viewer".into(),
                    ..who.clone()
                },
            )]),
        });
        Ok(Self {
            store: store.clone(),
            who,
            router,
            labels: (0..units).map(|n| format!("Unit-{n:06}")).collect(),
            version: 0,
            originals: vec![],
        })
    }
    async fn execute(&mut self, command: Value, serials: Vec<String>) -> Result<Value> {
        let r = Request {
            request_id: id(),
            expected_version: self.version,
            resource_versions: None,
            command: serde_json::from_value::<Command>(command)?,
            unit_inputs: vec![],
            serial_inputs: if serials.is_empty() {
                vec![]
            } else {
                vec![Input {
                    target: vec![],
                    sku: "PROFILE".into(),
                    serials,
                }]
            },
        };
        let response = application(self.store.execute(&self.who, "wh-main", &r).await)?;
        self.version = response["result"]["version"]
            .as_i64()
            .ok_or("Missing result version")?;
        self.originals.push((r, response.clone()));
        Ok(response)
    }
    async fn seed(&mut self) -> Result<Value> {
        for command in [
            json!({"type":"create_product","sku":"PROFILE","name":"Synthetic serial cost diagnostic","category":"Synthetic","reorder_point":0,"serial_controlled":true}),
            json!({"type":"create_location","code":"A","zone":"Synthetic","kind":"storage"}),
            json!({"type":"create_location","code":"B","zone":"Synthetic","kind":"storage"}),
        ] {
            self.execute(command, vec![]).await?;
        }
        let start = Instant::now();
        self.execute(json!({"type":"count_stock","sku":"PROFILE","lot":"LOT","location":"A","counted":self.labels.len(),"reason":"Synthetic complete observed set"}),self.labels.clone()).await?;
        Ok(
            json!({"complete_count_ms":start.elapsed().as_secs_f64()*1000.0,"registered_units":self.labels.len()}),
        )
    }
    async fn read(&self, path: &str) -> Result<Value> {
        let r = self
            .router
            .clone()
            .oneshot(
                HttpRequest::builder()
                    .uri(path)
                    .header("authorization", "Bearer synthetic-serial-profile-reader")
                    .body(Body::empty())?,
            )
            .await?;
        let status = r.status();
        let bytes = r.into_body().collect().await?.to_bytes();
        let body: Value = serde_json::from_slice(&bytes)?;
        require(
            status.is_success(),
            &format!("Read {path} failed{status}: {body}"),
        )?;
        Ok(body)
    }
    async fn reads(&self, samples: usize) -> Result<Value> {
        let paths = [
            ("current_page", "/v1/serials?sku=PROFILE&limit=50"),
            (
                "unit_history",
                "/v1/serial-history?sku=PROFILE&serial=Unit-000000&limit=50",
            ),
            (
                "observation_page",
                "/v1/serial-observations?sku=PROFILE&limit=50",
            ),
        ];
        let before = self.evidence().await?;
        let mut result = serde_json::Map::new();
        for (name, path) in paths {
            let mut elapsed = vec![];
            for _ in 0..samples {
                let now = Instant::now();
                let body = self.read(path).await?;
                require(
                    body["page_source_ok"] == true,
                    "Selected source proof failed",
                )?;
                elapsed.push(now.elapsed().as_secs_f64() * 1000.0);
            }
            result.insert(name.into(), json!({"milliseconds":elapsed}));
        }
        let mut elapsed = vec![];
        for _ in 0..samples {
            let now = Instant::now();
            let proof = application(self.store.reconciliation(&self.who.tenant, "wh-main").await)?;
            require(
                proof["ok"] == true,
                "Independent full reconciliation failed",
            )?;
            elapsed.push(now.elapsed().as_secs_f64() * 1000.0);
        }
        result.insert(
            "full_reconciliation".into(),
            json!({"milliseconds":elapsed}),
        );
        require(
            self.evidence().await? == before,
            "Diagnostic reads changed database evidence",
        )?;
        Ok(Value::Object(result))
    }
    async fn evidence(&self) -> Result<Value> {
        Ok(sqlx::query_scalar("SELECT jsonb_build_object('warehouse',(SELECT version FROM wms_warehouses WHERE tenant=$1 AND warehouse_id='wh-main'),'commands',(SELECT count(*) FROM wms_commands WHERE tenant=$1),'events',(SELECT count(*) FROM wms_events WHERE tenant=$1),'outbox',(SELECT count(*) FROM wms_outbox WHERE tenant=$1),'units',(SELECT count(*) FROM wms_serial_units WHERE tenant=$1),'steps',(SELECT count(*) FROM wms_serial_steps WHERE tenant=$1),'observations',(SELECT count(*) FROM wms_serial_observations WHERE tenant=$1),'batches',(SELECT count(*) FROM wms_serial_batches WHERE tenant=$1),'heads',(SELECT jsonb_agg(to_jsonb(h) ORDER BY sku) FROM wms_serial_heads h WHERE tenant=$1))").bind(&self.who.tenant).fetch_one(&self.store.pool).await?)
    }
    async fn moves(&mut self, count: usize) -> Result<Value> {
        let mut elapsed = vec![];
        for n in 0..count {
            let (from, to) = if n % 2 == 0 { ("A", "B") } else { ("B", "A") };
            let now = Instant::now();
            self.execute(json!({"type":"move_stock","sku":"PROFILE","lot":"LOT","from":from,"to":to,"quantity":1}),vec![self.labels[0].clone()]).await?;
            elapsed.push(now.elapsed().as_secs_f64() * 1000.0);
        }
        let before = self.evidence().await?;
        for (r, original) in &self.originals {
            let v = application(self.store.execute(&self.who, "wh-main", r).await)?;
            require(
                v["replayed"] == true && v["result"] == original["result"],
                "Original diagnostic command failed exact replay",
            )?;
        }
        require(
            self.evidence().await? == before,
            "Original replays changed evidence",
        )?;
        Ok(
            json!({"single_unit_move_milliseconds":elapsed,"all_original_commands_replayed":self.originals.len(),"evidence":before}),
        )
    }
    async fn plans(&self) -> Result<Value> {
        let mut plans = serde_json::Map::new();
        // Compare complete scope predicates on identical committed rows. The
        // immutable predecessor is installed only in this rolled-back session.
        let mut tx = self.store.pool.begin().await?;
        let previous = include_str!("../migrations/029_serial_scope_source_sets.sql");
        let start = previous
            .find("CREATE OR REPLACE FUNCTION wms_serial_scope_current_valid")
            .ok_or("Missing original scope declaration")?;
        let end = start + previous[start..].find("$$;").ok_or("Missing scope body")? + 3;
        let oracle = previous[start..end].replacen(
            "wms_serial_scope_current_valid(",
            "wms_serial_profile_scope_v29(",
            1,
        );
        sqlx::raw_sql(&oracle).execute(&mut *tx).await?;
        let mut comparisons = vec![];
        for _ in 0..7 {
            let mut sample = serde_json::Map::new();
            for (phase, function) in [
                ("original_before_ms", "wms_serial_profile_scope_v29"),
                ("candidate_ms", "wms_serial_scope_current_valid"),
                ("original_after_ms", "wms_serial_profile_scope_v29"),
            ] {
                let start = Instant::now();
                let valid: bool = sqlx::query_scalar(&format!("SELECT {function}($1,'wh-main')"))
                    .bind(&self.who.tenant)
                    .fetch_one(&mut *tx)
                    .await?;
                require(valid, "Complete scope comparison rejected valid custody")?;
                sample.insert(phase.into(), json!(start.elapsed().as_secs_f64() * 1000.0));
            }
            comparisons.push(sample);
        }
        tx.rollback().await?;
        plans.insert("scope_comparison".into(), json!({
            "samples": comparisons,
            "scope": "Interleaved complete v29/current/v29 predicates on identical rows; shared host, read cost only, not write throughput"
        }));
        for (name,sql) in [
            ("birth_event_read_source","SELECT wms_serial_event_valid_read_source(e.tenant,e.warehouse_id,e.id) FROM wms_events e WHERE tenant=$1 AND payload->'command'->>'type'='count_stock'"),
            ("selected_current_sources","SELECT wms_serial_unit_bound(u) FROM wms_serial_units u WHERE tenant=$1 AND sku='PROFILE' ORDER BY serial LIMIT 50"),
            ("physical_scope","SELECT wms_serial_scope_current_valid($1,'wh-main')"),
        ]{let plan:Value=sqlx::query_scalar(&format!("EXPLAIN (ANALYZE,BUFFERS,FORMAT JSON) {sql}")).bind(&self.who.tenant).fetch_one(&self.store.pool).await?;plans.insert(name.into(),plan);}
        Ok(Value::Object(plans))
    }
}
fn setting(name: &str, default: usize, min: usize, max: usize) -> Result<usize> {
    let v = env::var(name).map_or(Ok(default), |s| s.parse::<usize>())?;
    require(
        (min..=max).contains(&v),
        &format!("{name} outside{min}..{max}"),
    )?;
    Ok(v)
}
#[tokio::main]
async fn main() -> Result<()> {
    let url = env::var("WMS_TEST_DATABASE_URL")?;
    require(
        reqwest::Url::parse(&url)?.path().contains("test"),
        "Diagnostic requires explicit owned test database",
    )?;
    let output = PathBuf::from(env::var("WMS_SERIAL_PROFILE_OUTPUT")?);
    require(!output.exists(), "Never overwrite profile evidence")?;
    let sizes = env::var("WMS_SERIAL_PROFILE_UNITS")
        .unwrap_or_else(|_| "16,64,128".into())
        .split(',')
        .map(str::parse::<usize>)
        .collect::<std::result::Result<Vec<_>, _>>()?;
    require(
        !sizes.is_empty() && sizes.len() <= 16 && sizes.iter().all(|n| (1..=10_000).contains(n)),
        "Use1..16sizes of1..10000wholeeach identities",
    )?;
    let samples = setting("WMS_SERIAL_PROFILE_SAMPLES", 3, 1, 100)?;
    let moves = setting("WMS_SERIAL_PROFILE_MOVES", 10, 0, 1000)?;
    let store = application(Store::connect(&url).await)?;
    let postgres: String = sqlx::query_scalar("SHOW server_version")
        .fetch_one(&store.pool)
        .await?;
    let mut report = json!({"schema_version":1,"synthetic":true,"ok":false,"production_qualified":false,"postgres_version":postgres,"sizes":sizes,"samples_per_read":samples,"single_unit_moves_per_fixture":moves,"scope":"Actual Store transactions and authenticated in-process API router costs, excluding TCP/browser/device and sustained capacity. Uncontrolled shared host; no comparative causal claim.","cases":[]});
    fs::write(&output, serde_json::to_vec_pretty(&report)?)?;
    for units in sizes {
        let mut f = Fixture::new(&store, units).await?;
        let mut case = json!({"units":units,"tenant":f.who.tenant,"ok":false,"stage":"seed"});
        report["cases"]
            .as_array_mut()
            .ok_or("Invalid report cases")?
            .push(case.clone());
        let index = report["cases"]
            .as_array()
            .ok_or("Invalid report cases")?
            .len()
            - 1;
        let result: Result<()> = async {
            case["seed"] = f.seed().await?;
            case["stage"] = json!("reads_at_birth");
            report["cases"][index] = case.clone();
            fs::write(&output, serde_json::to_vec_pretty(&report)?)?;
            case["reads_at_birth"] = f.reads(samples).await?;
            case["stage"] = json!("single_unit_moves");
            report["cases"][index] = case.clone();
            fs::write(&output, serde_json::to_vec_pretty(&report)?)?;
            case["moves"] = f.moves(moves).await?;
            case["stage"] = json!("reads_with_history");
            report["cases"][index] = case.clone();
            fs::write(&output, serde_json::to_vec_pretty(&report)?)?;
            case["reads_with_history"] = f.reads(samples).await?;
            case["stage"] = json!("query_plans");
            report["cases"][index] = case.clone();
            fs::write(&output, serde_json::to_vec_pretty(&report)?)?;
            case["query_plans"] = f.plans().await?;
            case["stage"] = json!("complete");
            case["ok"] = json!(true);
            Ok(())
        }
        .await;
        if let Err(e) = &result {
            case["error"] = json!(e.to_string());
        }
        report["cases"][index] = case;
        fs::write(&output, serde_json::to_vec_pretty(&report)?)?;
        result?;
        eprintln!("Serial diagnostic completed {units} literal identities");
    }
    report["ok"] = json!(true);
    fs::write(&output, serde_json::to_vec_pretty(&report)?)?;
    store.pool.close().await;
    println!("PASS: owned serial source/read/history cost diagnostic");
    Ok(())
}
