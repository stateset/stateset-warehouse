//! Scale reproduction for the 2026-10-21 qualification failure: final serial
//! API reconciliation exceeded the unchanged 10-second statement deadline at
//! 16,000 seeded labels. This test births a same-order corpus through real
//! count commands and requires complete reconciliation to finish (the pool
//! enforces statement_timeout='10s', so completion proves the deadline holds).
use serde_json::{json, Value};
use stateset_warehouse::{
    domain::{self, State},
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
            tenant: format!("serial-scale-{}", domain::id()),
            actor: "synthetic:serial-scale".into(),
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
    async fn execute(&self, command: Value, inputs: Vec<Input>) -> Value {
        let request = Request {
            request_id: domain::id(),
            expected_version: self.state().await.version,
            resource_versions: None,
            unit_inputs: vec![],
            serial_inputs: inputs,
            command: serde_json::from_value(command).expect("typed canonical command"),
        };
        self.store
            .execute(&self.who, "wh-main", &request)
            .await
            .expect("real atomic command")
    }
    async fn plain(&self, command: Value) {
        self.execute(command, vec![]).await;
    }
}

/// Birth `skus * rounds * per_round` labels. Each round is one count at bin A
/// observing the complete accumulated set for that SKU, so later counts carry
/// growing closure sets exactly like incremental opening counts in the field.
#[tokio::test]
#[ignore = "requires WMS_TEST_DATABASE_URL"]
async fn reconciliation_completes_at_sixteen_thousand_labels() {
    let f = Fixture::new().await;
    let skus = 8;
    let rounds = 4;
    let per_round = 500;
    for s in 0..skus {
        f.plain(json!({"type":"create_product","sku":format!("SCALE-{s}"),"name":"Synthetic scale product","category":"Synthetic","reorder_point":0,"serial_controlled":true}))
            .await;
    }
    f.plain(json!({"type":"create_location","code":"A","zone":"Synthetic","kind":"storage"}))
        .await;
    f.plain(json!({"type":"create_location","code":"B","zone":"Synthetic","kind":"storage"}))
        .await;

    let started = std::time::Instant::now();
    let mut total = 0usize;
    for s in 0..skus {
        let sku = format!("SCALE-{s}");
        let mut observed: Vec<String> = Vec::new();
        for round in 0..rounds {
            for i in 0..per_round {
                observed.push(format!("{sku}-R{round}-{i}"));
            }
            let input = Input {
                target: vec![],
                sku: sku.clone(),
                serials: observed.clone(),
            };
            f.execute(
                json!({"type":"count_stock","sku":sku,"lot":"LOT","location":"A","counted":observed.len(),"reason":"Synthetic scale opening count"}),
                vec![input],
            )
            .await;
            total = observed.len();
        }
        assert_eq!(total, rounds * per_round);
    }
    let seeded = skus * rounds * per_round;
    eprintln!("seeded {seeded} labels in {:?}", started.elapsed());

    // Add revision depth: move a slice of one SKU to bin B so historical
    // point-in-time resolution has multi-revision chains to walk.
    let moved: Vec<String> = (0..250).map(|i| format!("SCALE-0-R0-{i}")).collect();
    f.execute(
        json!({"type":"move_stock","sku":"SCALE-0","lot":"LOT","from":"A","to":"B","quantity":moved.len()}),
        vec![Input {
            target: vec![],
            sku: "SCALE-0".into(),
            serials: moved,
        }],
    )
    .await;

    let started = std::time::Instant::now();
    let report = f
        .store
        .reconciliation(&f.who.tenant, "wh-main")
        .await
        .expect("complete reconciliation within the statement deadline");
    eprintln!("reconciliation completed in {:?}", started.elapsed());
    assert_eq!(report["ok"], true, "{report}");
    assert_eq!(report["serials"]["source_ok"], true);
    assert_eq!(report["serials"]["physical_ok"], true);
    assert_eq!(report["serials"]["registered_units"], seeded);
}

/// Qualification-shape corpus: few labels per event but thousands of events.
/// The failed gate ran 300s of rotating single-label moves after seeding, so
/// final reconciliation re-validated thousands of small envelopes.
#[tokio::test]
#[ignore = "requires WMS_TEST_DATABASE_URL"]
async fn reconciliation_completes_with_thousands_of_move_events() {
    let f = Fixture::new().await;
    f.plain(json!({"type":"create_product","sku":"SCALE-M","name":"Synthetic scale product","category":"Synthetic","reorder_point":0,"serial_controlled":true}))
        .await;
    f.plain(json!({"type":"create_location","code":"A","zone":"Synthetic","kind":"storage"}))
        .await;
    f.plain(json!({"type":"create_location","code":"B","zone":"Synthetic","kind":"storage"}))
        .await;

    let labels: Vec<String> = (0..2000).map(|i| format!("MOVE-{i}")).collect();
    for chunk in labels.chunks(500) {
        // Counts at an empty-then-growing bin must observe the full set, so
        // seed each chunk into its own lot to keep four independent counts.
        let lot = format!("LOT-{}", chunk[0]);
        f.execute(
            json!({"type":"count_stock","sku":"SCALE-M","lot":lot,"location":"A","counted":chunk.len(),"reason":"Synthetic scale opening count"}),
            vec![Input {
                target: vec![],
                sku: "SCALE-M".into(),
                serials: chunk.to_vec(),
            }],
        )
        .await;
    }

    // 2,000 rotating single-label moves: each label alternates bins once.
    let started = std::time::Instant::now();
    for (i, label) in labels.iter().enumerate() {
        let lot = format!("LOT-MOVE-{}", i / 500 * 500);
        f.execute(
            json!({"type":"move_stock","sku":"SCALE-M","lot":lot,"from":"A","to":"B","quantity":1}),
            vec![Input {
                target: vec![],
                sku: "SCALE-M".into(),
                serials: vec![label.clone()],
            }],
        )
        .await;
    }
    eprintln!("seeded 2000 move events in {:?}", started.elapsed());

    let started = std::time::Instant::now();
    let report = f
        .store
        .reconciliation(&f.who.tenant, "wh-main")
        .await
        .expect("complete reconciliation within the statement deadline");
    eprintln!(
        "many-events reconciliation completed in {:?}",
        started.elapsed()
    );
    assert_eq!(report["ok"], true, "{report}");
    assert_eq!(report["serials"]["source_ok"], true);
    assert_eq!(report["serials"]["registered_units"], 2000);
}

/// Growth curve for the 2026-10-27 gate timeout: the gate's 16,000 labels plus
/// thousands of single-label moves. Prints the full request time and its two
/// largest statements after each batch; correctness is asserted, timing is not.
#[tokio::test]
#[ignore = "requires WMS_TEST_DATABASE_URL; prints a timing curve"]
async fn reconciliation_time_with_serial_move_history() {
    let f = Fixture::new().await;
    let skus = 8;
    let per_sku = 2000;
    for s in 0..skus {
        f.plain(json!({"type":"create_product","sku":format!("CURVE-{s}"),"name":"Synthetic scale product","category":"Synthetic","reorder_point":0,"serial_controlled":true}))
            .await;
    }
    f.plain(json!({"type":"create_location","code":"A","zone":"Synthetic","kind":"storage"}))
        .await;
    f.plain(json!({"type":"create_location","code":"B","zone":"Synthetic","kind":"storage"}))
        .await;
    for s in 0..skus {
        let sku = format!("CURVE-{s}");
        let labels: Vec<String> = (0..per_sku).map(|i| format!("{sku}-{i}")).collect();
        f.execute(
            json!({"type":"count_stock","sku":sku,"lot":"LOT","location":"A","counted":labels.len(),"reason":"Synthetic scale opening count"}),
            vec![Input { target: vec![], sku: sku.clone(), serials: labels }],
        )
        .await;
    }

    let url = std::env::var("WMS_TEST_DATABASE_URL").expect("isolated PostgreSQL test URL");
    let pool = sqlx::PgPool::connect(&url)
        .await
        .expect("timing connection");
    let timed = |sql: &'static str| {
        let pool = pool.clone();
        let tenant = f.who.tenant.clone();
        async move {
            let started = std::time::Instant::now();
            sqlx::query(sql)
                .bind(&tenant)
                .bind("wh-main")
                .fetch_all(&pool)
                .await
                .expect("proof statement");
            started.elapsed().as_millis()
        }
    };

    let mut version = f.state().await.version;
    let mut moves = 0usize;
    for target in [0usize, 1000, 2000, 4000, 6000] {
        while moves < target {
            // Rotate one label per move: out to B on the first pass, back to A on the next.
            let sku = format!("CURVE-{}", moves % skus);
            let index = moves / skus % per_sku;
            let (from, to) = if (moves / (skus * per_sku)).is_multiple_of(2) {
                ("A", "B")
            } else {
                ("B", "A")
            };
            let request = Request {
                request_id: domain::id(),
                expected_version: version,
                resource_versions: None,
                unit_inputs: vec![],
                serial_inputs: vec![Input { target: vec![], sku: sku.clone(), serials: vec![format!("{sku}-{index}")] }],
                command: serde_json::from_value(json!({"type":"move_stock","sku":sku,"lot":"LOT","from":from,"to":to,"quantity":1}))
                    .expect("typed canonical command"),
            };
            f.store
                .execute(&f.who, "wh-main", &request)
                .await
                .expect("real atomic move");
            version += 1;
            moves += 1;
        }
        let started = std::time::Instant::now();
        let report = f
            .store
            .reconciliation(&f.who.tenant, "wh-main")
            .await
            .expect("complete reconciliation within the statement deadline");
        let full = started.elapsed().as_millis();
        assert_eq!(report["ok"], true, "{report}");
        assert_eq!(report["version"], version);
        let started = std::time::Instant::now();
        f.state().await;
        let state = started.elapsed().as_millis();
        let ledger = timed(include_str!("../src/reconciliation.sql")).await;
        let serial = timed(include_str!("../src/serial_reconciliation.sql")).await;
        eprintln!("curve moves={moves} commands={version} full_ms={full} state_ms={state} ledger_sql_ms={ledger} serial_sql_ms={serial}");
    }
}
