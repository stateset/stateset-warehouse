//! Reproducible local capacity/correctness qualification, never a facility SLO.
//! See docs/performance.md. All inventory changes use the public Store contract.
use serde::Serialize;
use serde_json::{json, Value};
use sqlx::Row;
use stateset_warehouse::{
    domain::{id, Command, LineInput, ReceiptInput, Warehouse},
    store::{Identity, Request, Store},
};
use std::{collections::BTreeSet, sync::Arc, time::Instant};
use tokio::sync::Barrier;

type Result<T> = std::result::Result<T, String>;

#[derive(Clone, Serialize)]
struct Config {
    products: usize,
    storage_bins: usize,
    lots_per_product: usize,
    open_orders: usize,
    clients: usize,
    actions_per_client: usize,
    max_action_p95_ms: Option<f64>,
    min_actions_per_second: Option<f64>,
}

fn setting(name: &str, default: usize, min: usize, max: usize) -> Result<usize> {
    let value = std::env::var(name)
        .map_or(Ok(default), |v| v.parse::<usize>())
        .map_err(|_| format!("{name} must be an integer"))?;
    if !(min..=max).contains(&value) {
        return Err(format!("{name} must be between {min} and {max}"));
    }
    Ok(value)
}

fn threshold(name: &str) -> Result<Option<f64>> {
    std::env::var(name)
        .ok()
        .map(|v| {
            v.parse::<f64>()
                .ok()
                .filter(|n| n.is_finite() && *n > 0.0)
                .ok_or_else(|| format!("{name} must be positive and finite"))
        })
        .transpose()
}

impl Config {
    fn read() -> Result<Self> {
        let clients = setting("WMS_WORKLOAD_CLIENTS", 4, 1, 32)?;
        Ok(Self {
            products: setting("WMS_WORKLOAD_PRODUCTS", 100, clients, 20_000)?,
            storage_bins: setting("WMS_WORKLOAD_BINS", 20, 2, 10_000)?,
            lots_per_product: setting("WMS_WORKLOAD_LOTS", 2, 1, 20)?,
            open_orders: setting("WMS_WORKLOAD_OPEN_ORDERS", 100, 0, 20_000)?,
            clients,
            actions_per_client: setting("WMS_WORKLOAD_ACTIONS", 10, 1, 10_000)?,
            max_action_p95_ms: threshold("WMS_WORKLOAD_MAX_P95_MS")?,
            min_actions_per_second: threshold("WMS_WORKLOAD_MIN_ACTIONS_PER_SECOND")?,
        })
    }
}

fn sku(n: usize) -> String {
    format!("LOAD-{n:05}")
}
fn bin(n: usize) -> String {
    format!("BIN-{n:05}")
}
fn lot(n: usize) -> String {
    format!("LOT-{n:03}")
}
fn describe(e: stateset_warehouse::store::Error) -> String {
    format!("{}: {}", e.status, e.message)
}
fn check(ok: bool, message: &str) -> Result<()> {
    if ok {
        Ok(())
    } else {
        Err(message.into())
    }
}

#[derive(Default)]
struct Latencies(Vec<f64>);
impl Latencies {
    fn record(&mut self, start: Instant) {
        self.0.push(start.elapsed().as_secs_f64() * 1000.0);
    }
    fn summary(&self) -> Value {
        let mut v = self.0.clone();
        v.sort_by(f64::total_cmp);
        let percentile = |p: f64| -> f64 {
            if v.is_empty() {
                0.0
            } else {
                v[((v.len() as f64 * p).ceil() as usize).saturating_sub(1)]
            }
        };
        json!({"samples":v.len(),"p50_ms":percentile(0.50),"p95_ms":percentile(0.95),"p99_ms":percentile(0.99),"max_ms":v.last().copied().unwrap_or(0.0)})
    }
}

#[derive(Default)]
struct ClientResult {
    actions: usize,
    stale_versions: usize,
    transient_attempts: usize,
    frozen_replays: usize,
    action_latency: Latencies,
    refresh_latency: Latencies,
    attempt_latency: Latencies,
    replay_latency: Latencies,
}
impl ClientResult {
    fn merge(&mut self, other: Self) {
        self.actions += other.actions;
        self.stale_versions += other.stale_versions;
        self.transient_attempts += other.transient_attempts;
        self.frozen_replays += other.frozen_replays;
        self.action_latency.0.extend(other.action_latency.0);
        self.refresh_latency.0.extend(other.refresh_latency.0);
        self.attempt_latency.0.extend(other.attempt_latency.0);
        self.replay_latency.0.extend(other.replay_latency.0);
    }
}

async fn submit(
    store: &Store,
    identity: &Identity,
    warehouse: &str,
    version: &mut i64,
    command: Command,
) -> Result<Value> {
    let response = store
        .execute(
            identity,
            warehouse,
            &Request {
                request_id: id(),
                expected_version: *version,
                command,
            },
        )
        .await
        .map_err(describe)?;
    *version = response["result"]["version"]
        .as_i64()
        .ok_or("command omitted version")?;
    Ok(response["result"].clone())
}

async fn seed(store: &Store, identity: &Identity, c: &Config) -> Result<i64> {
    let mut version = 0;
    for n in 0..=c.products {
        submit(
            store,
            identity,
            "wh-main",
            &mut version,
            Command::CreateProduct {
                sku: sku(n),
                name: format!("Capacity qualification product {n}"),
                category: "Qualification".into(),
                reorder_point: 1,
            },
        )
        .await?;
    }
    for n in 0..c.storage_bins {
        submit(
            store,
            identity,
            "wh-main",
            &mut version,
            Command::CreateLocation {
                code: bin(n),
                zone: "Qualification".into(),
                kind: "storage".into(),
            },
        )
        .await?;
    }
    for n in 0..c.products {
        for l in 0..c.lots_per_product {
            submit(
                store,
                identity,
                "wh-main",
                &mut version,
                Command::CountStock {
                    sku: sku(n),
                    lot: lot(l),
                    location: bin(n % c.storage_bins),
                    counted: 100_000,
                    reason: "Synthetic opening physical count".into(),
                },
            )
            .await?;
        }
    }
    submit(
        store,
        identity,
        "wh-main",
        &mut version,
        Command::CountStock {
            sku: sku(c.products),
            lot: "LAST-UNIT".into(),
            location: bin(0),
            counted: 1,
            reason: "Synthetic last-unit race fixture".into(),
        },
    )
    .await?;
    for n in 0..c.open_orders {
        let order = submit(
            store,
            identity,
            "wh-main",
            &mut version,
            Command::CreateOrder {
                reference: format!("LOAD-SO-{n:05}"),
                source_order_id: None,
                customer: "Synthetic capacity customer".into(),
                destination: "Qualification facility".into(),
                priority: "normal".into(),
                lines: vec![LineInput {
                    id: "line-1".into(),
                    sku: sku(n % c.products),
                    quantity: 1,
                }],
            },
        )
        .await?;
        if n % 2 == 0 {
            submit(
                store,
                identity,
                "wh-main",
                &mut version,
                Command::Allocate {
                    order_id: order["entity_id"]
                        .as_str()
                        .ok_or("missing order ID")?
                        .into(),
                },
            )
            .await?;
        }
    }
    // Keep exact receipt-line evidence in the operational workload, including a lost response.
    let receipt = submit(
        store,
        identity,
        "wh-main",
        &mut version,
        Command::CreateReceipt {
            reference: "LOAD-ASN-1".into(),
            supplier: "Synthetic capacity supplier".into(),
            lines: vec![ReceiptInput {
                id: "line-1".into(),
                sku: sku(0),
                lot: lot(0),
                quantity: 7,
            }],
        },
    )
    .await?;
    let receive = Request {
        request_id: id(),
        expected_version: version,
        command: Command::Receive {
            receipt_id: receipt["entity_id"]
                .as_str()
                .ok_or("missing receipt ID")?
                .into(),
            line_id: "line-1".into(),
            quantity: 7,
            location: "RECV-01".into(),
        },
    };
    store
        .execute(identity, "wh-main", &receive)
        .await
        .map_err(describe)?;
    version += 1;
    let replay = store
        .execute(identity, "wh-main", &receive)
        .await
        .map_err(describe)?;
    check(
        replay["replayed"] == true,
        "receipt replay was not retained",
    )?;
    submit(
        store,
        identity,
        "wh-main",
        &mut version,
        Command::MoveStock {
            sku: sku(0),
            lot: lot(0),
            from: "RECV-01".into(),
            to: bin(0),
            quantity: 7,
        },
    )
    .await?;
    Ok(version)
}

async fn client(
    store: Store,
    mut identity: Identity,
    c: Config,
    number: usize,
    barrier: Arc<Barrier>,
) -> Result<ClientResult> {
    identity.actor = format!("capacity:operator:{number}");
    identity.role = "operator".into();
    let mut stats = ClientResult::default();
    barrier.wait().await;
    for action in 0..c.actions_per_client {
        let start = Instant::now();
        let (a, b) = (
            bin(number % c.storage_bins),
            bin((number + 1) % c.storage_bins),
        );
        let command = Command::MoveStock {
            sku: sku(number),
            lot: lot(0),
            from: if action % 2 == 0 {
                a.clone()
            } else {
                b.clone()
            },
            to: if action % 2 == 0 { b } else { a },
            quantity: 1,
        };
        let mut committed = false;
        for _ in 0..1_000 {
            // This is the operator's refresh, included in end-to-end action latency.
            let refresh_start = Instant::now();
            let version = store
                .state(&identity.tenant, "wh-main")
                .await
                .map_err(describe)?
                .version;
            stats.refresh_latency.record(refresh_start);
            let request = Request {
                request_id: id(),
                expected_version: version,
                command: command.clone(),
            };
            let mut transient_retries = 0;
            loop {
                let attempt = Instant::now();
                let response = store.execute(&identity, "wh-main", &request).await;
                stats.attempt_latency.record(attempt);
                match response {
                    Ok(result) => {
                        check(
                            result["replayed"] == false || transient_retries > 0,
                            "fresh request unexpectedly replayed",
                        )?;
                        stats.actions += 1;
                        stats.action_latency.record(start);
                        let replay_start = Instant::now();
                        let replay = store
                            .execute(&identity, "wh-main", &request)
                            .await
                            .map_err(describe)?;
                        stats.replay_latency.record(replay_start);
                        check(
                            replay["replayed"] == true && replay["result"] == result["result"],
                            "lost-response replay changed its result",
                        )?;
                        stats.frozen_replays += 1;
                        committed = true;
                        break;
                    }
                    Err(e) if e.status == 409 && e.message.starts_with("Warehouse changed") => {
                        // Definite rejection: no physical action/receipt happened. Refresh,
                        // review this lane's unchanged move, then use a new request identity.
                        stats.stale_versions += 1;
                        break;
                    }
                    Err(e) if e.status == 503 && transient_retries < 3 => {
                        // Unknown completion: preserve this exact request, including version.
                        stats.transient_attempts += 1;
                        transient_retries += 1;
                    }
                    Err(e) => return Err(describe(e)),
                }
            }
            if committed {
                break;
            }
            tokio::task::yield_now().await;
        }
        check(
            committed,
            "operation exceeded 1,000 stale-version refreshes",
        )?;
    }
    Ok(stats)
}

async fn last_unit(store: &Store, identity: &Identity, c: &Config) -> Result<Value> {
    let version = store
        .state(&identity.tenant, "wh-main")
        .await
        .map_err(describe)?
        .version;
    let request = |to| Request {
        request_id: id(),
        expected_version: version,
        command: Command::MoveStock {
            sku: sku(c.products),
            lot: "LAST-UNIT".into(),
            from: bin(0),
            to,
            quantity: 1,
        },
    };
    let requests = [request(bin(1)), request("RACE-DESTINATION".into())];
    let (a, b) = tokio::join!(
        store.execute(identity, "wh-main", &requests[0]),
        store.execute(identity, "wh-main", &requests[1])
    );
    let results = [a, b];
    let winners: Vec<_> = results
        .iter()
        .enumerate()
        .filter_map(|(n, result)| result.as_ref().ok().map(|_| n))
        .collect();
    check(
        winners.len() == 1,
        "last physical unit had other than one winner",
    )?;
    let winner = winners[0];
    let loser = 1 - winner;
    let stale = results[loser].as_ref().err().ok_or("missing loser")?;
    check(
        stale.status == 409,
        "last-unit loser did not receive a version conflict",
    )?;
    let mut retry = requests[loser].clone();
    retry.request_id = id();
    retry.expected_version += 1;
    let rejected = store.execute(identity, "wh-main", &retry).await;
    check(
        rejected
            .as_ref()
            .is_err_and(|e| e.status == 422 && e.message.contains("Insufficient")),
        "reviewed last-unit loser did not reject insufficient stock",
    )?;
    let replay = store
        .execute(identity, "wh-main", &requests[winner])
        .await
        .map_err(describe)?;
    check(
        replay["replayed"] == true,
        "last-unit winner did not replay",
    )?;
    Ok(
        json!({"physical_actions":1,"stale_version_rejections":1,"business_rejections":1,"frozen_replays":1}),
    )
}

async fn isolation(store: &Store, identity: &Identity) -> Result<Value> {
    let outside = Identity {
        tenant: format!("capacity-outside-{}", id()),
        actor: "capacity:outside".into(),
        role: "admin".into(),
    };
    store
        .provision(&outside.tenant, false)
        .await
        .map_err(describe)?;
    // Provisioning currently exposes only wh-main. This fixture creates only an
    // empty second warehouse header; all catalog/stock changes still use Store.
    let metadata = Warehouse {
        id: "wh-isolation".into(),
        code: "ISOLATION".into(),
        name: "Empty isolation qualification fixture".into(),
        city: "Synthetic".into(),
        timezone: "UTC".into(),
    };
    sqlx::query(
        "INSERT INTO wms_warehouses(tenant,warehouse_id,version,metadata) VALUES($1,$2,0,$3)",
    )
    .bind(&identity.tenant)
    .bind(&metadata.id)
    .bind(json!(metadata))
    .execute(&store.pool)
    .await
    .map_err(|e| e.to_string())?;
    // An empty warehouse also requires system locations; add them as provisioning
    // fixtures, matching State::empty exactly, without balances or execution rows.
    for (code, kind) in [("RECV-01", "receiving"), ("STAGE-01", "staging")] {
        sqlx::query("INSERT INTO wms_locations VALUES($1,$2,$3,$4,$5)")
            .bind(&identity.tenant)
            .bind(&metadata.id)
            .bind(code)
            .bind(kind)
            .bind(json!({"code":code,"zone":if kind=="staging" {"Outbound"} else {"Receiving"},"kind":kind}))
            .execute(&store.pool)
            .await
            .map_err(|e| e.to_string())?;
    }
    let before = store
        .state(&identity.tenant, "wh-main")
        .await
        .map_err(describe)?;
    let mut version = 0;
    submit(
        store,
        identity,
        &metadata.id,
        &mut version,
        Command::CreateProduct {
            sku: "ISOLATED-PRODUCT".into(),
            name: "Isolated product".into(),
            category: "Qualification".into(),
            reorder_point: 0,
        },
    )
    .await?;
    let after = store
        .state(&identity.tenant, "wh-main")
        .await
        .map_err(describe)?;
    check(
        before == after,
        "second warehouse changed primary warehouse",
    )?;
    let other = store
        .state(&outside.tenant, "wh-main")
        .await
        .map_err(describe)?;
    check(
        other.version == 0 && other.products.is_empty() && other.stocks.is_empty(),
        "second tenant changed",
    )?;
    let order_id = before
        .orders
        .first()
        .map(|o| o.id.as_str())
        .unwrap_or("unknown");
    for (tenant, warehouse) in [
        (&outside.tenant, "wh-main"),
        (&identity.tenant, metadata.id.as_str()),
    ] {
        check(
            store
                .detail(tenant, warehouse, "orders", order_id)
                .await
                .as_ref()
                .is_err_and(|e| e.status == 404),
            "order escaped its warehouse/tenant",
        )?;
        let page = store
            .history(tenant, warehouse, "movements", None, 100)
            .await
            .map_err(describe)?;
        check(
            page["items"].as_array().is_some_and(Vec::is_empty),
            "movement history escaped its warehouse/tenant",
        )?;
        let foreign_identity = Identity {
            tenant: tenant.clone(),
            actor: identity.actor.clone(),
            role: "admin".into(),
        };
        let request = Request {
            request_id: id(),
            expected_version: if warehouse == "wh-main" { 0 } else { version },
            command: Command::CountStock {
                sku: sku(0),
                lot: lot(0),
                location: "RECV-01".into(),
                counted: 1,
                reason: "Must not discover a foreign product".into(),
            },
        };
        check(
            store
                .execute(&foreign_identity, warehouse, &request)
                .await
                .as_ref()
                .is_err_and(|e| e.status == 422),
            "foreign warehouse accepted a stock command for a primary-warehouse SKU",
        )?;
        let original_request: String = sqlx::query_scalar(
            "SELECT request_id FROM wms_commands WHERE tenant=$1 AND warehouse_id='wh-main' LIMIT 1",
        )
        .bind(&identity.tenant)
        .fetch_one(&store.pool)
        .await
        .map_err(|e| e.to_string())?;
        check(
            store
                .receipt(&foreign_identity, warehouse, &original_request)
                .await
                .map_err(describe)?["found"]
                == false,
            "command receipt escaped its warehouse/tenant",
        )?;
    }
    Ok(
        json!({"tenant_isolation":true,"warehouse_isolation":true,"second_warehouse_provisioning_fixture":"empty header and standard locations only"}),
    )
}

fn peak_memory_kib() -> Option<u64> {
    std::fs::read_to_string("/proc/self/status")
        .ok()?
        .lines()
        .find(|line| line.starts_with("VmHWM:"))?
        .split_whitespace()
        .nth(1)?
        .parse()
        .ok()
}

async fn run() -> Result<()> {
    let c = Config::read()?;
    let url = std::env::var("WMS_TEST_DATABASE_URL")
        .map_err(|_| "set WMS_TEST_DATABASE_URL to an isolated PostgreSQL test database")?;
    check(
        reqwest::Url::parse(&url)
            .map_err(|e| e.to_string())?
            .path()
            .contains("test"),
        "workload database name must contain test",
    )?;
    let store = Store::connect(&url).await.map_err(describe)?;
    let identity = Identity {
        tenant: format!("capacity-{}", id()),
        actor: "capacity:admin".into(),
        role: "admin".into(),
    };
    store
        .provision(&identity.tenant, false)
        .await
        .map_err(describe)?;
    eprintln!(
        "Seeding {} products, {} bin-lots and {} open orders in {}",
        c.products,
        c.products * c.lots_per_product,
        c.open_orders,
        identity.tenant
    );
    let seed_start = Instant::now();
    let mut seed_version = seed(&store, &identity, &c).await?;
    submit(
        &store,
        &identity,
        "wh-main",
        &mut seed_version,
        Command::CreateLocation {
            code: "RACE-DESTINATION".into(),
            zone: "Qualification".into(),
            kind: "storage".into(),
        },
    )
    .await?;
    let seed_seconds = seed_start.elapsed().as_secs_f64();
    let memory_before = peak_memory_kib();
    let isolation = isolation(&store, &identity).await?;
    let race = last_unit(&store, &identity, &c).await?;
    let barrier = Arc::new(Barrier::new(c.clients));
    let mut handles = Vec::new();
    eprintln!(
        "Running {} concurrent clients, {} physical moves each",
        c.clients, c.actions_per_client
    );
    let start = Instant::now();
    for n in 0..c.clients {
        handles.push(tokio::spawn(client(
            store.clone(),
            identity.clone(),
            c.clone(),
            n,
            barrier.clone(),
        )));
    }
    let mut result = ClientResult::default();
    for handle in handles {
        result.merge(handle.await.map_err(|e| e.to_string())??);
    }
    let seconds = start.elapsed().as_secs_f64();
    let state = store
        .state(&identity.tenant, "wh-main")
        .await
        .map_err(describe)?;
    state.validate()?;
    let reconciliation = store
        .reconciliation(&identity.tenant, "wh-main")
        .await
        .map_err(describe)?;
    check(
        reconciliation["ok"] == true,
        "independent final reconciliation failed",
    )?;
    let physical_total: i64 = state.stocks.iter().map(|stock| stock.on_hand).sum();
    let reserved_total: i64 = state.stocks.iter().map(|stock| stock.reserved).sum();
    check(
        physical_total == (c.products * c.lots_per_product) as i64 * 100_000 + 8,
        "stock moves changed total physical custody",
    )?;
    check(
        reserved_total == c.open_orders.div_ceil(2) as i64,
        "disjoint stock moves changed allocated ownership",
    )?;
    check(
        state.orders.len() == c.open_orders,
        "current operational order scale was truncated",
    )?;
    let row=sqlx::query("SELECT (SELECT count(*) FROM wms_events WHERE tenant=$1 AND warehouse_id='wh-main') AS events,(SELECT count(*) FROM wms_commands WHERE tenant=$1 AND warehouse_id='wh-main') AS commands,(SELECT count(*) FROM wms_movements WHERE tenant=$1 AND warehouse_id='wh-main') AS movements,(SELECT count(*) FROM wms_activity WHERE tenant=$1 AND warehouse_id='wh-main') AS activity,(SELECT sum(received) FROM wms_receipt_lines WHERE tenant=$1 AND warehouse_id='wh-main')::bigint AS received,(SELECT sum(quantity) FROM wms_movements WHERE tenant=$1 AND warehouse_id='wh-main' AND kind='receipt')::bigint AS receipt_evidence,(SELECT count(*) FROM wms_movements WHERE tenant=$1 AND warehouse_id='wh-main' AND sku=$2 AND kind='putaway') AS last_unit_moves")
        .bind(&identity.tenant).bind(sku(c.products)).fetch_one(&store.pool).await.map_err(|e|e.to_string())?;
    let events: i64 = row.get("events");
    let commands: i64 = row.get("commands");
    let movements: i64 = row.get("movements");
    let activity: i64 = row.get("activity");
    let expected_commands = seed_version + 1 + result.actions as i64;
    let expected_movements = (c.products * c.lots_per_product + 1 + 2 + 1 + result.actions) as i64;
    check(
        events == expected_commands
            && commands == expected_commands
            && activity == expected_commands
            && state.version == expected_commands,
        "command/event/activity/version exactly-once totals differ",
    )?;
    check(
        movements == expected_movements,
        "physical movement exactly-once count differs",
    )?;
    check(
        row.get::<i64, _>("received") == 7
            && row.get::<i64, _>("receipt_evidence") == 7
            && row.get::<i64, _>("last_unit_moves") == 1,
        "receipt or last-unit evidence duplicated",
    )?;
    let first = store
        .history(&identity.tenant, "wh-main", "activity", None, 25)
        .await
        .map_err(describe)?;
    let cursor = first["next_cursor"].as_i64();
    let second = if let Some(cursor) = cursor {
        store
            .history(&identity.tenant, "wh-main", "activity", Some(cursor), 25)
            .await
            .map_err(describe)?
    } else {
        json!({"items":[]})
    };
    let a: BTreeSet<_> = first["items"]
        .as_array()
        .ok_or("history items absent")?
        .iter()
        .filter_map(|i| i["cursor"].as_i64())
        .collect();
    let b: BTreeSet<_> = second["items"]
        .as_array()
        .ok_or("history items absent")?
        .iter()
        .filter_map(|i| i["cursor"].as_i64())
        .collect();
    check(
        a.len() <= 25
            && b.len() <= 25
            && a.is_disjoint(&b)
            && state.activity.len() <= 100
            && state.movements.len() <= 100,
        "history windows are unbounded or overlapping",
    )?;
    let action_latency = result.action_latency.summary();
    let throughput = result.actions as f64 / seconds;
    let p95 = action_latency["p95_ms"].as_f64().ok_or("missing p95")?;
    let threshold_ok = c.max_action_p95_ms.is_none_or(|max| p95 <= max)
        && c.min_actions_per_second.is_none_or(|min| throughput >= min);
    let report = json!({
        "schema_version":1,"kind":"local_synthetic_store_workload","recorded_at":stateset_warehouse::domain::now(),"tenant":identity.tenant,"config":c,
        "environment":{"available_parallelism":std::thread::available_parallelism().ok().map(usize::from),"postgres_version":sqlx::query_scalar::<_,String>("SHOW server_version").fetch_one(&store.pool).await.map_err(|e|e.to_string())?,"build":"use cargo run --release for capacity comparison","store_pool_connections":8},
        "seed":{"seconds":seed_seconds,"commands":seed_version,"products":state.products.len(),"storage_bins":c.storage_bins+1,"bin_lot_balances":state.stocks.len(),"open_orders":state.orders.len(),"allocated_orders":state.orders.iter().filter(|o|o.status=="allocated").count()},
        "concurrent_work":{"seconds":seconds,"physical_actions":result.actions,"actions_per_second":throughput,"stale_version_rejections":result.stale_versions,"transient_503_attempts":result.transient_attempts,"successful_frozen_replays":result.frozen_replays,"unexpected_errors":0,"action_including_refresh_and_retries":action_latency,"state_refresh":result.refresh_latency.summary(),"command_attempt":result.attempt_latency.summary(),"frozen_replay":result.replay_latency.summary()},
        "proof":{"last_unit":race,"isolation":isolation,"independent_reconciliation":true,"commands":commands,"events":events,"activity":activity,"physical_movements":movements,"physical_custody_total":physical_total,"reserved_total":reserved_total,"received":7,"receipt_movement_quantity":7,"exactly_once_totals":true,"display_activity":state.activity.len(),"display_movements":state.movements.len(),"history_page_sizes":[a.len(),b.len()],"history_pages_disjoint":true},
        "memory":{"process_peak_before_clients_kib":memory_before,"process_peak_after_clients_kib":peak_memory_kib(),"scope":"Rust workload process high-water mark; excludes PostgreSQL and is not a constant-memory proof"},
        "configured_thresholds_passed":threshold_ok,
        "limits":["local synthetic Store test, not deployed HTTP/scanner or facility acceptance","one warehouse serializes writes and clients explicitly re-review stale versions","all current catalog/bin-lots/open orders load for each command; bounded history does not bound live-work memory","synthetic opening counts are fixtures, not measured facility inventory","PostgreSQL and harness may share CPU; no production throughput or HA claim"]
    });
    let report = serde_json::to_string_pretty(&report).map_err(|e| e.to_string())?;
    if let Ok(path) = std::env::var("WMS_WORKLOAD_REPORT") {
        std::fs::write(path, &report).map_err(|e| e.to_string())?;
    }
    println!("{report}");
    check(
        threshold_ok,
        "configured capacity thresholds failed; report retained",
    )
}

#[tokio::main]
async fn main() {
    if let Err(error) = run().await {
        eprintln!("WMS workload failed: {error}");
        std::process::exit(1);
    }
}
