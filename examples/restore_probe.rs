//! Synthetic, isolated backup/restore qualification. Never connects to a facility.
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use sqlx::{postgres::PgConnectOptions, Connection, PgConnection, Row};
use stateset_warehouse::{
    domain::{Command, LineInput, ReceiptInput},
    native::{NativeIntegrations, TransportPlan},
    native_store::{self, Claim, Freeze},
    oms_client::{payload_digest, FulfillmentLine, FulfillmentPayload, PreparedFulfillment},
    oms_fulfillment_api::{claim_fulfillment, finish_fulfillment},
    store::{Identity, Request, Store},
};
use std::{env, fs, str::FromStr};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
const TENANT: &str = "restore-rehearsal";
const WAREHOUSE: &str = "wh-main";
const IMPORT: &str = "restore-native-import";
const TRANSPORT: &str = "restore-native-transport";
const FULFILLMENT: &str = "restore-conditional-fulfillment";
const REJECTED: &str = "restore-conditional-rejected";

fn application<T>(result: stateset_warehouse::store::Result<T>) -> Result<T> {
    result.map_err(|e| format!("Warehouse operation failed ({}): {}", e.status, e.message).into())
}
fn require(ok: bool, message: &str) -> Result<()> {
    if !ok {
        return Err(message.into());
    }
    Ok(())
}
fn identity() -> Identity {
    Identity {
        tenant: TENANT.into(),
        actor: "rehearsal:admin".into(),
        role: "admin".into(),
    }
}
fn safe_database(name: &str) -> Result<()> {
    require(
        (name.starts_with("wms_restore_source_test_")
            || name.starts_with("wms_restore_target_test_"))
            && name.len() <= 63
            && name
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_'),
        "Rehearsal database name must use its unique wms_restore_{source,target}_test_ prefix",
    )
}
async fn admin(action: &str, name: &str) -> Result<()> {
    safe_database(name)?;
    let url = env::var("WMS_RESTORE_ADMIN_URL")?;
    let options = PgConnectOptions::from_str(&url)?.database("postgres");
    let mut connection = PgConnection::connect_with(&options).await?;
    let sql = match action {
        "create" => format!("CREATE DATABASE \"{name}\" TEMPLATE template0 ENCODING 'UTF8'"),
        "drop" => format!("DROP DATABASE IF EXISTS \"{name}\""),
        _ => return Err("Unsupported database action".into()),
    };
    sqlx::query(&sql).execute(&mut connection).await?;
    connection.close().await?;
    Ok(())
}
async fn connect() -> Result<Store> {
    let url = env::var("DATABASE_URL")?;
    let options = PgConnectOptions::from_str(&url)?;
    safe_database(options.get_database().ok_or("Database name is required")?)?;
    application(Store::connect(&url).await)
}
async fn command(store: &Store, key: &str, command: Command) -> Result<(Request, Value)> {
    let request = Request {
        request_id: key.into(),
        expected_version: application(store.state(TENANT, WAREHOUSE).await)?.version,
        command,
    };
    let response = application(store.execute(&identity(), WAREHOUSE, &request).await)?;
    Ok((request, response))
}
fn entity(response: &Value) -> Result<String> {
    Ok(response["result"]["entity_id"]
        .as_str()
        .ok_or("Missing command entity identity")?
        .into())
}
async fn snapshot(store: &Store) -> Result<Value> {
    let mut tx = store.pool.begin().await?;
    sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY")
        .execute(&mut *tx)
        .await?;
    let names: Vec<String> = sqlx::query_scalar(
        "SELECT tablename FROM pg_tables WHERE schemaname='public' AND tablename LIKE 'wms_%' ORDER BY tablename",
    )
    .fetch_all(&mut *tx)
    .await?;
    let mut tables = serde_json::Map::new();
    for name in names {
        require(
            name.bytes().all(|b| b.is_ascii_lowercase() || b == b'_'),
            "Unexpected WMS table identifier",
        )?;
        let sql = format!("SELECT COALESCE(jsonb_agg(to_jsonb(t) ORDER BY to_jsonb(t)::text),'[]'::jsonb) FROM public.\"{name}\" t");
        let rows: Value = sqlx::query_scalar(&sql).fetch_one(&mut *tx).await?;
        tables.insert(
            name,
            json!({"rows":rows.as_array().ok_or("Invalid evidence rows")?.len(),"sha256":format!("{:x}",Sha256::digest(serde_json::to_vec(&rows)?))}),
        );
    }
    tx.commit().await?;
    let warehouses =
        sqlx::query("SELECT tenant,warehouse_id FROM wms_warehouses ORDER BY tenant,warehouse_id")
            .fetch_all(&store.pool)
            .await?;
    let mut proofs = Vec::new();
    for row in warehouses {
        let tenant: String = row.try_get("tenant")?;
        let warehouse: String = row.try_get("warehouse_id")?;
        let proof = application(store.reconciliation(&tenant, &warehouse).await)?;
        require(proof["ok"] == true, "Full historical reconciliation failed")?;
        proofs.push(json!({"tenant":tenant,"warehouse_id":warehouse,"proof":proof}));
    }
    let version: String = sqlx::query_scalar("SHOW server_version")
        .fetch_one(&store.pool)
        .await?;
    let migrations:Value=sqlx::query_scalar("SELECT jsonb_agg(jsonb_build_object('name',name,'sha256',digest) ORDER BY name) FROM wms_schema_migrations")
        .fetch_one(&store.pool).await?;
    Ok(
        json!({"tables":tables,"warehouses":proofs,"postgres_version":version,"migrations":migrations}),
    )
}

async fn conditional_fixture(
    store: &Store,
) -> Result<(PreparedFulfillment, String, native_store::NativeRecord)> {
    let source = "00000000-0000-4000-8000-000000000322";
    let line_id = format!(
        "oms-line-{:x}",
        Sha256::digest(b"source:restore-conditional-line")
    );
    let (_, created) = command(
        store,
        "restore-conditional-order",
        Command::CreateOrder {
            reference: "RESTORE-CONDITIONAL-ORDER".into(),
            source_order_id: Some(format!("oms:restore-oms:{source}")),
            customer: "Synthetic restore customer".into(),
            destination: "Synthetic destination".into(),
            priority: "normal".into(),
            lines: vec![LineInput {
                id: line_id.clone(),
                sku: "RESTORE-EACH".into(),
                quantity: 1,
            }],
        },
    )
    .await?;
    let order_id = entity(&created)?;
    command(
        store,
        "restore-conditional-allocation",
        Command::Allocate {
            order_id: order_id.clone(),
        },
    )
    .await?;
    let state = application(store.state(TENANT, WAREHOUSE).await)?;
    let order = state
        .orders
        .iter()
        .find(|o| o.id == order_id)
        .ok_or("Missing conditional order")?;
    for (index, a) in order.allocations.iter().enumerate() {
        command(
            store,
            &format!("restore-conditional-pick-{index}"),
            Command::Pick {
                order_id: order_id.clone(),
                line_id: a.line_id.clone(),
                location: a.location.clone(),
                lot: a.lot.clone(),
                quantity: a.quantity,
            },
        )
        .await?;
    }
    command(
        store,
        "restore-conditional-pack",
        Command::Pack {
            order_id: order_id.clone(),
        },
    )
    .await?;
    command(
        store,
        "restore-conditional-ship",
        Command::Ship {
            order_id: order_id.clone(),
            carrier: "Synthetic restore carrier".into(),
            handoff_reference: "RESTORE-PHYSICAL-HANDOFF".into(),
        },
    )
    .await?;
    let order: stateset_warehouse::domain::Order = serde_json::from_value(application(
        store.detail(TENANT, WAREHOUSE, "orders", &order_id).await,
    )?)?;
    let event_id:String=sqlx::query_scalar("SELECT id FROM wms_events WHERE tenant=$1 AND warehouse_id=$2 AND payload->>'type'='fulfillment.shipped' AND payload->'order'->>'id'=$3")
        .bind(TENANT).bind(WAREHOUSE).bind(&order_id).fetch_one(&store.pool).await?;
    let payload = FulfillmentPayload {
        request_id: stateset_warehouse::domain::id(),
        order_id: source.into(),
        event_id: event_id.clone(),
        expected_updated_at: "2026-10-17T00:00:00Z".into(),
        warehouse_id: WAREHOUSE.into(),
        warehouse_order_id: order_id.clone(),
        carrier: order.carrier,
        handoff_reference: order.handoff_reference,
        handed_off_at: order.shipped_at.ok_or("Physical handoff time missing")?,
        lines: vec![FulfillmentLine {
            line_id,
            sku: "RESTORE-EACH".into(),
            quantity_each: 1,
        }],
    };
    let mut prepared = PreparedFulfillment {
        tenant: TENANT.into(),
        warehouse_id: WAREHOUSE.into(),
        order_id: order_id.clone(),
        event_id: event_id.clone(),
        recipient_base_url: "http://127.0.0.1:1".into(),
        recipient_tenant_id: "restore-oms".into(),
        idempotency_key: payload.request_id.clone(),
        digest: payload_digest(&payload)?,
        payload,
    };
    let version = application(store.state(TENANT, WAREHOUSE).await)?.version;
    let rejected_input = json!({"request_id":REJECTED,"expected_version":version});
    let rejected_digest = application(native_store::digest(
        "oms_fulfillment",
        WAREHOUSE,
        Some(&order_id),
        &rejected_input,
    ))?;
    application(native_store::freeze(store,&identity(),WAREHOUSE,Freeze {
        kind:"oms_fulfillment",request_id:REJECTED,digest:&rejected_digest,input:&rejected_input,expected_version:version,order_id:Some(&order_id),
        source_snapshot:Some(&json!({"data":{"id":source,"updatedAt":"2026-10-17T00:00:00Z","fixture":"synthetic reviewed rejection"}})),frozen:&json!(prepared),
    }).await)?;
    let rejected=match application(claim_fulfillment(store,&identity(),WAREHOUSE,REJECTED,&rejected_digest).await)? {
        Claim::Ready{lease,..}=>application(finish_fulfillment(store,&identity(),WAREHOUSE,REJECTED,&lease,Ok(json!({"data":{"contract":stateset_warehouse::oms_client::CONTRACT,"tenant_id":"restore-oms","request_id":prepared.payload.request_id,"event_id":event_id,"order_id":source,"request_digest":prepared.digest,"status":"rejected","reason":"source_revision_changed","observed_updated_at":"2026-10-17T00:01:00Z","replayed":false,"fixture":"Synthetic terminal journal receipt; no external HTTP was attempted"}}))).await)?,
        Claim::Existing(_)=>return Err("Expected fresh synthetic rejected intent".into()),
    };
    // Explicitly reviewed second intent retains the same physical event and
    // receives a new remote request identity; the old rejection stays immutable.
    prepared.payload.request_id = stateset_warehouse::domain::id();
    prepared.idempotency_key = prepared.payload.request_id.clone();
    prepared.payload.expected_updated_at = "2026-10-17T00:01:00Z".into();
    prepared.digest = payload_digest(&prepared.payload)?;
    let input = json!({"request_id":FULFILLMENT,"expected_version":version});
    let digest = application(native_store::digest(
        "oms_fulfillment",
        WAREHOUSE,
        Some(&order_id),
        &input,
    ))?;
    application(native_store::freeze(store,&identity(),WAREHOUSE,Freeze {
        kind:"oms_fulfillment",request_id:FULFILLMENT,digest:&digest,input:&input,expected_version:version,order_id:Some(&order_id),
        source_snapshot:Some(&json!({"data":{"id":source,"updatedAt":"2026-10-17T00:01:00Z","fixture":"synthetic conditional restore"}})),frozen:&json!(prepared),
    }).await)?;
    match application(claim_fulfillment(store, &identity(), WAREHOUSE, FULFILLMENT, &digest).await)?
    {
        Claim::Ready { lease, .. } => {
            application(
                finish_fulfillment(
                    store,
                    &identity(),
                    WAREHOUSE,
                    FULFILLMENT,
                    &lease,
                    Err(
                        "Synthetic conditional journal uncertainty; no external HTTP was attempted"
                            .into(),
                    ),
                )
                .await,
            )?;
        }
        Claim::Existing(_) => return Err("Expected fresh conditional fulfillment claim".into()),
    }
    Ok((prepared, digest, rejected))
}

async fn seed(path: &str) -> Result<()> {
    let store = connect().await?;
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM wms_warehouses")
        .fetch_one(&store.pool)
        .await?;
    require(count == 0, "Source rehearsal database must be empty")?;
    application(store.provision(TENANT, false).await)?;
    // A second tenant proves that the archive contains every warehouse scope.
    application(store.provision("restore-other-tenant", false).await)?;
    command(
        &store,
        "restore-product",
        Command::CreateProduct {
            sku: "RESTORE-EACH".into(),
            name: "Synthetic restore item".into(),
            category: "Rehearsal".into(),
            reorder_point: 0,
        },
    )
    .await?;
    command(
        &store,
        "restore-bin",
        Command::CreateLocation {
            code: "A-01".into(),
            zone: "Synthetic storage".into(),
            kind: "storage".into(),
        },
    )
    .await?;
    let (_, receipt) = command(
        &store,
        "restore-create-receipt",
        Command::CreateReceipt {
            reference: "RESTORE-ASN".into(),
            supplier: "Synthetic supplier".into(),
            lines: vec![ReceiptInput {
                id: "receipt-line".into(),
                sku: "RESTORE-EACH".into(),
                lot: "RESTORE-LOT".into(),
                quantity: 12,
            }],
        },
    )
    .await?;
    let receipt_id = entity(&receipt)?;
    let (receive, receive_response) = command(
        &store,
        "restore-receive-original",
        Command::Receive {
            receipt_id: receipt_id.clone(),
            line_id: "receipt-line".into(),
            quantity: 12,
            location: "RECV-01".into(),
        },
    )
    .await?;
    command(
        &store,
        "restore-putaway",
        Command::MoveStock {
            sku: "RESTORE-EACH".into(),
            lot: "RESTORE-LOT".into(),
            from: "RECV-01".into(),
            to: "A-01".into(),
            quantity: 12,
        },
    )
    .await?;
    let import_input = json!({"request_id":IMPORT,"expected_version":application(store.state(TENANT,WAREHOUSE).await)?.version,"oms_order_id":"00000000-0000-4000-8000-000000000321","source_updated_at":"2026-10-17T00:00:00Z","details":{"customer":"Synthetic restore customer","destination":"Synthetic receiving facility","priority":"normal"}});
    let import_request = Request {
        request_id: native_store::import_command_id(TENANT, WAREHOUSE, IMPORT),
        expected_version: import_input["expected_version"]
            .as_i64()
            .ok_or("Missing version")?,
        command: Command::CreateOrder {
            reference: "RESTORE-NATIVE-ORDER".into(),
            source_order_id: Some("oms:restore-oms:00000000-0000-4000-8000-000000000321".into()),
            customer: "Synthetic restore customer".into(),
            destination: "Synthetic receiving facility".into(),
            priority: "normal".into(),
            lines: vec![
                LineInput {
                    id: "source-line-a".into(),
                    sku: "RESTORE-EACH".into(),
                    quantity: 2,
                },
                LineInput {
                    id: "source-line-b".into(),
                    sku: "RESTORE-EACH".into(),
                    quantity: 3,
                },
            ],
        },
    };
    let import_digest = native_store::digest("oms_import", WAREHOUSE, None, &import_input)
        .map_err(|e| format!("Import digest: {}", e.message))?;
    let source_snapshot = json!({"meta":{"fixture":"restore-rehearsal"},"data":{"id":"00000000-0000-4000-8000-000000000321","status":"processing","updatedAt":"2026-10-17T00:00:00Z","lineItems":[{"shopifyLineItemId":"source-line-a","sku":"RESTORE-EACH","quantity":2},{"shopifyLineItemId":"source-line-b","sku":"RESTORE-EACH","quantity":3}],"unknownSourceField":{"retain":[1,null,"raw"]}}});
    let frozen = json!({"command_request":import_request});
    application(
        native_store::freeze(
            &store,
            &identity(),
            WAREHOUSE,
            Freeze {
                kind: "oms_import",
                request_id: IMPORT,
                digest: &import_digest,
                input: &import_input,
                expected_version: import_request.expected_version,
                order_id: None,
                source_snapshot: Some(&source_snapshot),
                frozen: &frozen,
            },
        )
        .await,
    )?;
    let import_response =
        application(store.execute(&identity(), WAREHOUSE, &import_request).await)?;
    application(
        native_store::complete_import(&store, &identity(), WAREHOUSE, IMPORT, &import_response)
            .await,
    )?;
    let order_id = entity(&import_response)?;
    command(
        &store,
        "restore-allocate",
        Command::Allocate {
            order_id: order_id.clone(),
        },
    )
    .await?;
    let allocated = application(store.state(TENANT, WAREHOUSE).await)?;
    let order = allocated
        .orders
        .iter()
        .find(|order| order.id == order_id)
        .ok_or("Missing native order")?;
    for (index, allocation) in order.allocations.iter().enumerate() {
        command(
            &store,
            &format!("restore-pick-{index}"),
            Command::Pick {
                order_id: order_id.clone(),
                line_id: allocation.line_id.clone(),
                location: allocation.location.clone(),
                lot: allocation.lot.clone(),
                quantity: allocation.quantity,
            },
        )
        .await?;
    }
    command(
        &store,
        "restore-pack",
        Command::Pack {
            order_id: order_id.clone(),
        },
    )
    .await?;
    let plan: TransportPlan = serde_json::from_value(json!({
        "reference":"RESTORE-PLAN","customer_id":"synthetic-restore-customer","mode":"LTL","equipment":"Dry van","commodity":"Synthetic rehearsal goods","weight_lbs":100,"miles":10,"buy_cents":10000,"sell_cents":12000,
        "origin":{"facility":"Synthetic origin","city":"Portland","state":"OR","date":"2026-10-18","address_lines":["1 Synthetic Way"],"postal_code":"97201","country":"US","contact":"Rehearsal","phone":"","instructions":"Synthetic test only"},
        "destination":{"facility":"Synthetic destination","city":"Portland","state":"OR","date":"2026-10-19","address_lines":["2 Synthetic Way"],"postal_code":"97202","country":"US","contact":"Rehearsal","phone":"","instructions":"Synthetic test only"}
    }))?;
    // Preparation performs no HTTP. No remote service is used by this rehearsal.
    let native = NativeIntegrations::from_json(&json!({TENANT:{
        "oms":{"base_url":"http://127.0.0.1:1","tenant_id":"restore-oms","api_key":"synthetic-unused-credential"},
        "tms":{"base_url":"http://127.0.0.1:1","tenant_id":"restore-freight","bearer_token":"synthetic-unused-credential"}
    }}).to_string())?;
    let state = application(store.state(TENANT, WAREHOUSE).await)?;
    let prepared = native.prepare_transport(TENANT, &state, &order_id, &plan)?;
    let transport_input =
        json!({"request_id":TRANSPORT,"expected_version":state.version,"plan":plan});
    let transport_digest = native_store::digest(
        "tms_transport",
        WAREHOUSE,
        Some(&order_id),
        &transport_input,
    )
    .map_err(|e| format!("Transport digest: {}", e.message))?;
    application(
        native_store::freeze(
            &store,
            &identity(),
            WAREHOUSE,
            Freeze {
                kind: "tms_transport",
                request_id: TRANSPORT,
                digest: &transport_digest,
                input: &transport_input,
                expected_version: state.version,
                order_id: Some(&order_id),
                source_snapshot: Some(&source_snapshot),
                frozen: &json!(prepared),
            },
        )
        .await,
    )?;
    match application(
        native_store::claim_transport(&store, &identity(), WAREHOUSE, TRANSPORT, &transport_digest)
            .await,
    )? {
        Claim::Ready { lease, .. } => {
            application(
                native_store::finish_transport(
                    &store,
                    &identity(),
                    WAREHOUSE,
                    TRANSPORT,
                    &lease,
                    Err(
                        "Synthetic uncertain journal outcome; no external HTTP was attempted"
                            .into(),
                    ),
                )
                .await,
            )?;
        }
        Claim::Existing(_) => return Err("Expected a fresh transport claim".into()),
    }
    // Reserve additional source-bin stock while the first order owns staging.
    let (_, waiting) = command(
        &store,
        "restore-waiting-order",
        Command::CreateOrder {
            reference: "RESTORE-WAITING".into(),
            source_order_id: None,
            customer: "Synthetic customer".into(),
            destination: "Synthetic destination".into(),
            priority: "normal".into(),
            lines: vec![LineInput {
                id: "waiting-line".into(),
                sku: "RESTORE-EACH".into(),
                quantity: 2,
            }],
        },
    )
    .await?;
    command(
        &store,
        "restore-waiting-allocation",
        Command::Allocate {
            order_id: entity(&waiting)?,
        },
    )
    .await?;
    let (conditional, conditional_digest, conditional_rejected) =
        conditional_fixture(&store).await?;
    let evidence = snapshot(&store).await?;
    let manifest = json!({"schema_version":1,"synthetic":true,"receive_request":receive,"receive_response":receive_response,"receipt_id":receipt_id,"order_id":order_id,"import_input":import_input,"import_digest":import_digest,"import_request":import_request,"import_response":import_response,"import_frozen":frozen,"source_snapshot":source_snapshot,"transport_digest":transport_digest,"transport_frozen":prepared,"conditional_frozen":conditional,"conditional_digest":conditional_digest,"conditional_rejected":conditional_rejected,"evidence":evidence});
    fs::write(path, serde_json::to_vec_pretty(&manifest)?)?;
    store.pool.close().await;
    println!("Seeded physical movements, receiving evidence, source/staging reservations, native receipts, uncertain attempt, and pending outbox in two tenant scopes.");
    Ok(())
}

async fn verify(manifest_path: &str, report_path: &str) -> Result<()> {
    let manifest: Value = serde_json::from_slice(&fs::read(manifest_path)?)?;
    require(
        manifest["schema_version"] == 1 && manifest["synthetic"] == true,
        "Unsupported rehearsal manifest",
    )?;
    let store = connect().await?;
    let restored = snapshot(&store).await?;
    require(restored == manifest["evidence"],"Restored table hashes, warehouse versions, or full ledger/receipt/allocation proofs differ from the recovery point")?;
    let receive: Request = serde_json::from_value(manifest["receive_request"].clone())?;
    let replay = application(store.execute(&identity(), WAREHOUSE, &receive).await)?;
    require(
        replay["replayed"] == true && replay["result"] == manifest["receive_response"]["result"],
        "Original receiving request did not replay its exact receipt",
    )?;
    let import_request: Request = serde_json::from_value(manifest["import_request"].clone())?;
    let (native, replayed) = application(
        native_store::freeze(
            &store,
            &identity(),
            WAREHOUSE,
            Freeze {
                kind: "oms_import",
                request_id: IMPORT,
                digest: manifest["import_digest"]
                    .as_str()
                    .ok_or("Missing import digest")?,
                input: &manifest["import_input"],
                expected_version: import_request.expected_version,
                order_id: None,
                source_snapshot: Some(&manifest["source_snapshot"]),
                frozen: &manifest["import_frozen"],
            },
        )
        .await,
    )?;
    require(
        replayed
            && native.status == "completed"
            && native.response == Some(manifest["import_response"].clone()),
        "Original native import did not replay its exact receipt",
    )?;
    let import_replay = application(store.execute(&identity(), WAREHOUSE, &import_request).await)?;
    require(
        import_replay["replayed"] == true
            && import_replay["result"] == manifest["import_response"]["result"],
        "Native command receipt was not preserved",
    )?;
    let receipt = application(
        store
            .detail(
                TENANT,
                WAREHOUSE,
                "receipts",
                manifest["receipt_id"]
                    .as_str()
                    .ok_or("Missing receipt ID")?,
            )
            .await,
    )?;
    require(
        receipt["status"] == "received",
        "Completed receiving evidence was not restored",
    )?;
    let order = application(
        store
            .detail(
                TENANT,
                WAREHOUSE,
                "orders",
                manifest["order_id"].as_str().ok_or("Missing order ID")?,
            )
            .await,
    )?;
    require(order["status"] == "packed", "Packed order was not restored")?;
    require(
        snapshot(&store).await? == restored,
        "Replaying original request IDs changed restored evidence",
    )?;
    let rejected =
        application(native_store::lookup(&store, &identity(), WAREHOUSE, REJECTED).await)?
            .ok_or("Missing durable rejected OMS receipt")?;
    require(
        json!(rejected) == manifest["conditional_rejected"],
        "Durable rejected OMS receipt changed on restore",
    )?;
    match application(
        claim_fulfillment(&store, &identity(), WAREHOUSE, REJECTED, &rejected.digest).await,
    )? {
        Claim::Existing(record) => require(
            record.status == "rejected" && record.attempts == 1,
            "Restored rejected OMS receipt must replay without another attempt",
        )?,
        Claim::Ready { .. } => {
            return Err("A restored definitive rejection was incorrectly resubmitted".into())
        }
    }
    require(
        snapshot(&store).await? == restored,
        "Replaying restored definitive rejection changed evidence",
    )?;
    let mut other_actor = identity();
    other_actor.actor = "rehearsal:other-operator".into();
    require(
        application(
            store
                .receipt(&other_actor, WAREHOUSE, &receive.request_id)
                .await,
        )?["found"]
            == false,
        "Restored command receipt leaked to another actor",
    )?;
    let record =
        application(native_store::lookup(&store, &identity(), WAREHOUSE, TRANSPORT).await)?
            .ok_or("Missing uncertain transport journal")?;
    require(
        record.status == "uncertain"
            && record.attempts == 1
            && record.frozen == manifest["transport_frozen"],
        "Uncertain native intent or attempt evidence changed on restore",
    )?;
    let native_digest = manifest["transport_digest"]
        .as_str()
        .ok_or("Missing transport digest")?;
    match application(
        native_store::claim_transport(&store, &identity(), WAREHOUSE, TRANSPORT, native_digest)
            .await,
    )? {
        Claim::Ready { record, lease } => {
            require(
                record.frozen == manifest["transport_frozen"] && record.attempts == 2,
                "Restored native journal changed its frozen retry or attempt counter",
            )?;
            application(native_store::finish_transport(&store,&identity(),WAREHOUSE,TRANSPORT,&lease,Err("Synthetic restore retry validated journal only; no external HTTP was attempted".into())).await)?;
        }
        Claim::Existing(_) => {
            return Err("Restored uncertain native intent could not be reclaimed".into())
        }
    }
    let conditional =
        application(native_store::lookup(&store, &identity(), WAREHOUSE, FULFILLMENT).await)?
            .ok_or("Missing conditional OMS journal")?;
    require(
        conditional.status == "uncertain"
            && conditional.attempts == 1
            && conditional.frozen == manifest["conditional_frozen"],
        "Conditional OMS intent or attempt evidence changed on restore",
    )?;
    match application(
        claim_fulfillment(
            &store,
            &identity(),
            WAREHOUSE,
            FULFILLMENT,
            manifest["conditional_digest"]
                .as_str()
                .ok_or("Missing conditional digest")?,
        )
        .await,
    )? {
        Claim::Ready { record, lease } => {
            require(
                record.frozen == manifest["conditional_frozen"] && record.attempts == 2,
                "Restored conditional OMS retry changed frozen handoff evidence",
            )?;
            application(finish_fulfillment(&store,&identity(),WAREHOUSE,FULFILLMENT,&lease,Err("Synthetic restore validates conditional journal recovery only; no external HTTP was attempted".into())).await)?;
        }
        Claim::Existing(_) => {
            return Err("Restored conditional OMS journal could not be reclaimed".into())
        }
    }
    let state = application(store.state(TENANT, WAREHOUSE).await)?;
    let stock = state
        .stocks
        .iter()
        .find(|s| s.sku == "RESTORE-EACH" && s.location == "A-01")
        .ok_or("Missing source-bin stock")?;
    let (new_request, _) = command(
        &store,
        "restore-new-count",
        Command::CountStock {
            sku: stock.sku.clone(),
            lot: stock.lot.clone(),
            location: stock.location.clone(),
            counted: stock.on_hand + 1,
            reason: "Synthetic count verifies post-restore command and sequence continuity".into(),
        },
    )
    .await?;
    let final_proof = application(store.reconciliation(TENANT, WAREHOUSE).await)?;
    require(
        final_proof["ok"] == true && final_proof["version"] == new_request.expected_version + 1,
        "Post-restore execution failed full independent reconciliation",
    )?;
    let report = json!({"ok":true,"synthetic":true,"archive_round_trip":"pg_dump custom / pg_restore single transaction","all_table_hashes_identical_before_retry":true,"all_warehouse_reconciliation":restored["warehouses"],"table_evidence":restored["tables"],"migrations":restored["migrations"],"receive_original_request_replayed":true,"native_import_original_request_replayed":true,"replays_changed_no_evidence":true,"rejected_conditional_oms_receipt_replayed_unchanged":true,"uncertain_native_frozen_retry_reclaimed":true,"uncertain_conditional_oms_retry_reclaimed":true,"native_external_http_attempted":false,"new_post_restore_command_reconciled":true,"post_restore_reconciliation":final_proof});
    fs::write(report_path, serde_json::to_vec_pretty(&report)?)?;
    store.pool.close().await;
    println!("Verified every WMS table, every warehouse proof, original command/native receipts, unchanged idempotent replay, uncertain journal recovery, and a new reconciled command.");
    Ok(())
}
#[tokio::main]
async fn main() -> Result<()> {
    let arguments: Vec<String> = env::args().skip(1).collect();
    match arguments.as_slice() {
        [action, name] if action == "create" || action == "drop" => admin(action, name).await,
        [action, path] if action == "seed" => seed(path).await,
        [action, manifest, report] if action == "verify" => verify(manifest, report).await,
        _ => Err(
            "Usage: restore_probe create|drop DATABASE; seed MANIFEST; verify MANIFEST REPORT"
                .into(),
        ),
    }
}
