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
#[path = "restore_probe/histories.rs"]
mod history_restore;
#[path = "restore_probe/serials.rs"]
mod serial_restore;

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
    if action == "create" {
        // Exercise restoration into a different database display timezone. The
        // snapshot hashes normalize timestamps; original UTC authority survives.
        let timezone = if name.starts_with("wms_restore_source_test_") {
            "Pacific/Auckland"
        } else {
            "UTC"
        };
        sqlx::query(&format!(
            r#"ALTER DATABASE "{name}" SET timezone TO '{timezone}'"#
        ))
        .execute(&mut connection)
        .await?;
    }
    connection.close().await?;
    Ok(())
}
async fn connect() -> Result<Store> {
    let url = env::var("DATABASE_URL")?;
    let options = PgConnectOptions::from_str(&url)?;
    safe_database(options.get_database().ok_or("Database name is required")?)?;
    let migrated = application(Store::connect(&url).await)?;
    migrated.pool.close().await;
    // SQLx deliberately starts sessions in UTC, overriding database defaults.
    // Use an explicit test-only session setting to exercise cross-timezone
    // recovery rather than merely asserting ALTER DATABASE took effect.
    let timezone = if options
        .get_database()
        .is_some_and(|name| name.starts_with("wms_restore_source_test_"))
    {
        "Pacific/Auckland"
    } else {
        "UTC"
    };
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(8)
        .after_connect(move |c, _| {
            Box::pin(async move {
                sqlx::query("SELECT set_config('TimeZone',$1,false)")
                    .bind(timezone)
                    .execute(&mut *c)
                    .await?;
                sqlx::query("SET statement_timeout='10s'")
                    .execute(&mut *c)
                    .await?;
                sqlx::query("SET lock_timeout='5s'")
                    .execute(&mut *c)
                    .await?;
                Ok(())
            })
        })
        .connect(&url)
        .await?;
    Ok(Store { pool })
}
async fn command(store: &Store, key: &str, command: Command) -> Result<(Request, Value)> {
    let request = Request {
        serial_inputs: Vec::new(),
        unit_inputs: Vec::new(),
        request_id: key.into(),
        expected_version: application(store.state(TENANT, WAREHOUSE).await)?.version,
        resource_versions: None,
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
async fn table_evidence(store: &Store) -> Result<Value> {
    let mut tx = store.pool.begin().await?;
    sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY")
        .execute(&mut *tx)
        .await?;
    sqlx::query("SET LOCAL TIME ZONE 'UTC'")
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
        if name == "wms_serial_pending_checks" {
            require(
                rows == json!([]),
                "Committed serial validation keys must be empty",
            )?;
        }
        tables.insert(
            name,
            json!({"rows":rows.as_array().ok_or("Invalid evidence rows")?.len(),"sha256":format!("{:x}",Sha256::digest(serde_json::to_vec(&rows)?))}),
        );
    }
    tx.commit().await?;
    Ok(Value::Object(tables))
}

async fn snapshot(store: &Store) -> Result<Value> {
    let tables = table_evidence(store).await?;
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

// Keep a non-terminal fulfillment recovery point, including both closure kinds.
async fn split_fixture(store: &Store) -> Result<Value> {
    let mut originals = Vec::new();
    for (key, command_value) in [
        (
            "restore-split-product",
            Command::CreateProduct {
                serial_controlled: false,
                sku: "RESTORE-SPLIT".into(),
                name: "Synthetic split restore".into(),
                category: "General".into(),
                reorder_point: 0,
            },
        ),
        (
            "restore-split-bin",
            Command::CreateLocation {
                code: "SPLIT-A".into(),
                zone: "Storage".into(),
                kind: "storage".into(),
            },
        ),
        (
            "restore-split-stock",
            Command::CountStock {
                sku: "RESTORE-SPLIT".into(),
                lot: "SPLIT-LOT".into(),
                location: "SPLIT-A".into(),
                counted: 7,
                reason: "Synthetic recovery point".into(),
            },
        ),
    ] {
        command(store, key, command_value).await?;
    }
    let (_, created) = command(
        store,
        "restore-split-order",
        Command::CreateOrder {
            reference: "RESTORE-SPLIT-ORDER".into(),
            source_order_id: None,
            customer: "Synthetic split customer".into(),
            destination: "Synthetic destination".into(),
            priority: "normal".into(),
            lines: vec![
                LineInput {
                    id: "a".into(),
                    sku: "RESTORE-SPLIT".into(),
                    quantity: 3,
                },
                LineInput {
                    id: "b".into(),
                    sku: "RESTORE-SPLIT".into(),
                    quantity: 5,
                },
            ],
        },
    )
    .await?;
    let oid = entity(&created)?;
    let (request, response) = command(
        store,
        "restore-split-available",
        Command::AllocateAvailable {
            order_id: oid.clone(),
        },
    )
    .await?;
    originals.push(json!({"request":request,"response":response}));
    for (line, quantity) in [("a", 3), ("b", 2)] {
        command(
            store,
            &format!("restore-split-pick-{line}"),
            Command::Pick {
                order_id: oid.clone(),
                line_id: line.into(),
                location: "SPLIT-A".into(),
                lot: "SPLIT-LOT".into(),
                quantity,
            },
        )
        .await?;
    }
    for (reference, line, quantity, outcome) in [
        ("RESTORE-SPLIT-SENT", "a", 3, "handoff"),
        ("RESTORE-SPLIT-ACTIVE", "b", 1, "active"),
        ("RESTORE-SPLIT-UNPACKED", "b", 1, "unpack"),
    ] {
        let (request, response) = command(
            store,
            reference,
            Command::PackShipment {
                order_id: oid.clone(),
                reference: reference.into(),
                lines: vec![stateset_warehouse::split_fulfillment::LineInput {
                    line_id: line.into(),
                    location: "SPLIT-A".into(),
                    lot: "SPLIT-LOT".into(),
                    quantity,
                }],
            },
        )
        .await?;
        let package = entity(&response)?;
        originals.push(json!({"request":request,"response":response}));
        // Carrier labels: one recorded before the handoff, and on the still
        // open package one voided label plus its active replacement.
        if outcome != "unpack" {
            let label = |tracking: &str| stateset_warehouse::carrier_labels::RecordRequest {
                carrier: "Synthetic restore parcel".into(),
                service: "Ground".into(),
                tracking_number: tracking.into(),
                rate: Some(stateset_warehouse::carrier_labels::Rate {
                    amount: "9.75".into(),
                    currency: "USD".into(),
                }),
                carton: None,
                label: stateset_warehouse::carrier_labels::Label::Zpl {
                    data: format!("^XA^FO40,40^FD{tracking}^FS^XZ"),
                },
            };
            let first = application(
                stateset_warehouse::carrier_labels::insert(
                    &store.pool,
                    TENANT,
                    WAREHOUSE,
                    "synthetic:restore",
                    &package,
                    &label(&format!("{reference}-LABEL")),
                )
                .await,
            )?;
            if outcome == "active" {
                application(
                    stateset_warehouse::carrier_labels::void_label(
                        &store.pool,
                        TENANT,
                        WAREHOUSE,
                        "synthetic:restore",
                        &package,
                        first["id"].as_str().ok_or("label id")?,
                        "Synthetic reprint before recovery point",
                    )
                    .await,
                )?;
                application(
                    stateset_warehouse::carrier_labels::insert(
                        &store.pool,
                        TENANT,
                        WAREHOUSE,
                        "synthetic:restore",
                        &package,
                        &label(&format!("{reference}-RELABEL")),
                    )
                    .await,
                )?;
            }
        }
        let closure = match outcome {
            "handoff" => Some(Command::HandoffShipment {
                shipment_id: package,
                carrier: "Synthetic split carrier".into(),
                handoff_reference: "RESTORE-SPLIT-HANDOFF".into(),
            }),
            "unpack" => Some(Command::UnpackShipment {
                shipment_id: package,
                reason: "Physical repacking before recovery point".into(),
            }),
            _ => None,
        };
        if let Some(closure) = closure {
            let (request, response) =
                command(store, &format!("{reference}-closed"), closure).await?;
            originals.push(json!({"request":request,"response":response}));
        }
    }
    let (request, response) = command(
        store,
        "restore-split-early-rma",
        Command::CreateReturn {
            reference: "RESTORE-SPLIT-EARLY-RMA".into(),
            order_id: oid.clone(),
            reason: "Return before remaining handoff".into(),
            lines: vec![stateset_warehouse::returns::ReturnLineInput {
                id: "return-a".into(),
                order_line_id: "a".into(),
                lot: "SPLIT-LOT".into(),
                quantity: 1,
            }],
        },
    )
    .await?;
    originals.push(json!({"request":request,"response":response}));
    let order = application(store.detail(TENANT, WAREHOUSE, "orders", &oid).await)?;
    require(
        order["status"] == "partially_shipped",
        "Split recovery point must remain partly shipped",
    )?;
    let allocations = order["allocations"]
        .as_array()
        .ok_or("Missing split allocations")?;
    require(
        allocations
            .iter()
            .map(|a| a["quantity"].as_i64().unwrap_or(0))
            .sum::<i64>()
            == 7,
        "Split recovery point lost original backorder",
    )?;
    let proof = application(store.reconciliation(TENANT, WAREHOUSE).await)?["packages"].clone();
    Ok(json!({"order_id":oid,"order":order,"proof":proof,"originals":originals}))
}

async fn remainder_fixture(store: &Store) -> Result<Value> {
    let mut originals = Vec::new();
    command(
        store,
        "restore-remainder-stock",
        Command::CountStock {
            sku: "RESTORE-SPLIT".into(),
            lot: "CLOSED-LOT".into(),
            location: "SPLIT-A".into(),
            counted: 7,
            reason: "Synthetic remainder recovery fixture".into(),
        },
    )
    .await?;
    let (_, created) = command(
        store,
        "restore-remainder-order",
        Command::CreateOrder {
            reference: "RESTORE-CLOSED-PARTIAL".into(),
            source_order_id: None,
            customer: "Synthetic cancellation customer".into(),
            destination: "Synthetic destination".into(),
            priority: "normal".into(),
            lines: vec![
                LineInput {
                    id: "a".into(),
                    sku: "RESTORE-SPLIT".into(),
                    quantity: 3,
                },
                LineInput {
                    id: "b".into(),
                    sku: "RESTORE-SPLIT".into(),
                    quantity: 5,
                },
            ],
        },
    )
    .await?;
    let oid = entity(&created)?;
    for (key, value) in [
        (
            "restore-remainder-allocate",
            Command::AllocateAvailable {
                order_id: oid.clone(),
            },
        ),
        (
            "restore-remainder-pick",
            Command::Pick {
                order_id: oid.clone(),
                line_id: "a".into(),
                location: "SPLIT-A".into(),
                lot: "CLOSED-LOT".into(),
                quantity: 3,
            },
        ),
        (
            "restore-remainder-pack",
            Command::PackShipment {
                order_id: oid.clone(),
                reference: "RESTORE-CLOSED-PACKAGE".into(),
                lines: vec![stateset_warehouse::split_fulfillment::LineInput {
                    line_id: "a".into(),
                    location: "SPLIT-A".into(),
                    lot: "CLOSED-LOT".into(),
                    quantity: 3,
                }],
            },
        ),
    ] {
        let (request, response) = command(store, key, value).await?;
        originals.push(json!({"request":request,"response":response}));
    }
    let package = entity(&originals.last().ok_or("Missing remainder package")?["response"])?;
    for (key, value) in [
        (
            "restore-remainder-handoff",
            Command::HandoffShipment {
                shipment_id: package,
                carrier: "Actual synthetic carrier".into(),
                handoff_reference: "RESTORE-CLOSED-HANDOFF".into(),
            },
        ),
        (
            "restore-remainder-cancel",
            Command::CancelRemainder {
                order_id: oid.clone(),
                reason: "Customer cancelled all unshipped demand".into(),
            },
        ),
        (
            "restore-remainder-return",
            Command::CreateReturn {
                reference: "RESTORE-CLOSED-RMA".into(),
                order_id: oid.clone(),
                reason: "Returned after commercial closure".into(),
                lines: vec![stateset_warehouse::returns::ReturnLineInput {
                    id: "closed-rma-line".into(),
                    order_line_id: "a".into(),
                    lot: "CLOSED-LOT".into(),
                    quantity: 1,
                }],
            },
        ),
    ] {
        let (request, response) = command(store, key, value).await?;
        originals.push(json!({"request":request,"response":response}));
    }
    let order = application(store.detail(TENANT, WAREHOUSE, "orders", &oid).await)?;
    require(
        order["status"] == "closed_partial"
            && order["remainder_cancellation"]["lines"]["b"]["cancelled"] == 5,
        "Remainder fixture must retain original shipped and cancelled demand",
    )?;
    let eligible = application(
        stateset_warehouse::returns::eligible(store, &identity(), WAREHOUSE, &oid).await,
    )?;
    require(
        eligible["lines"][0]["shipped"] == 3 && eligible["lines"][0]["remaining"] == 2,
        "Closed order must retain actual handoff return allowance",
    )?;
    Ok(json!({"order_id":oid,"order":order,"originals":originals,"eligibility":eligible}))
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

const QUALITY_SKU: &str = "RESTORE-QUALITY";
const QUALITY_LOT: &str = "QUALITY-LOT";
const QUALITY_DESTINATION: &str = "wh-restore-secondary";
async fn quality_command(
    store: &Store,
    originals: &mut Vec<Value>,
    warehouse: &str,
    key: &str,
    command: Command,
) -> Result<Value> {
    let request = Request {
        serial_inputs: Vec::new(),
        unit_inputs: Vec::new(),
        request_id: key.into(),
        expected_version: if matches!(command, Command::CreateWarehouse { .. }) {
            0
        } else {
            application(store.state(TENANT, warehouse).await)?.version
        },
        resource_versions: None,
        command,
    };
    let response = application(store.execute(&identity(), warehouse, &request).await)?;
    originals.push(json!({"warehouse_id":warehouse,"request":request,"response":response}));
    Ok(response)
}
async fn quality_packages(store: &Store, order_id: &str) -> Result<Value> {
    Ok(sqlx::query_scalar("SELECT COALESCE(jsonb_agg(wms_package_document(tenant,warehouse_id,id) ORDER BY id),'[]'::jsonb) FROM wms_packages WHERE tenant=$1 AND warehouse_id=$2 AND order_id=$3")
        .bind(TENANT).bind(WAREHOUSE).bind(order_id).fetch_one(&store.pool).await?)
}
async fn quality_transfer(store: &Store, transfer_id: &str) -> Result<Value> {
    let (document,valid):(Value,bool)=sqlx::query_as("SELECT wms_transfer_document($1,id),wms_transfer_consistent($1,id) FROM wms_transfers WHERE tenant=$1 AND id=$2")
        .bind(TENANT).bind(transfer_id).fetch_one(&store.pool).await?;
    require(
        valid,
        "Quality transfer must retain its immutable original ownership",
    )?;
    Ok(document)
}
const LOSS_SOURCE: &str = "wh-loss-source";
const LOSS_DESTINATION: &str = "wh-loss-destination";
async fn retained_loss_journal(store: &Store, transfer_id: &str) -> Result<Value> {
    use axum::{
        extract::{Path, Query, State as AppState},
        Extension,
    };
    use stateset_warehouse::{api::App, integrations::Connections, transfers};
    let app = App {
        store: store.clone(),
        credentials: std::sync::Arc::new(Vec::new()),
        demo: false,
        connections: Connections::default(),
    };
    let mut after = None;
    let mut review = None;
    let mut records = Vec::new();
    loop {
        let page = application(
            transfers::losses(
                AppState(app.clone()),
                Extension(identity()),
                Path((LOSS_DESTINATION.into(), transfer_id.into())),
                Ok(Query(transfers::LossPageQuery {
                    after,
                    review,
                    limit: Some(1),
                })),
            )
            .await,
        )?
        .0;
        require(
            page["page_source_ok"] == true,
            "Restored loss journal requires original source proof",
        )?;
        let current = page["review"]
            .as_i64()
            .ok_or("Missing original destination review")?;
        require(
            review.is_none_or(|r| r == current),
            "Loss journal changed its original review",
        )?;
        review = Some(current);
        for record in page["items"]
            .as_array()
            .ok_or("Missing loss journal records")?
        {
            require(
                record["source_ok"] == true
                    && !records.iter().any(|r: &Value| r["id"] == record["id"]),
                "Loss journal source failed or repeated a record",
            )?;
            records.push(record.clone());
        }
        match page["next_cursor"].as_i64() {
            Some(next) => {
                require(
                    after.is_none_or(|old| next > old),
                    "Loss journal cursor did not advance",
                )?;
                after = Some(next);
            }
            None => break,
        }
    }
    Ok(
        json!({"transfer_id":transfer_id,"destination_warehouse_id":LOSS_DESTINATION,"review":review,"records":records}),
    )
}
async fn transfer_loss_fixture(store: &Store) -> Result<Value> {
    let mut originals = Vec::new();
    for (w, code) in [(LOSS_SOURCE, "LOSS-SRC"), (LOSS_DESTINATION, "LOSS-DST")] {
        quality_command(
            store,
            &mut originals,
            w,
            &format!("restore-loss-create-{code}"),
            Command::CreateWarehouse {
                code: code.into(),
                name: "Synthetic restored loss facility".into(),
                city: "Vancouver".into(),
                timezone: "America/Vancouver".into(),
            },
        )
        .await?;
        quality_command(
            store,
            &mut originals,
            w,
            &format!("restore-loss-product-{code}"),
            Command::CreateProduct {
                serial_controlled: false,
                sku: "RESTORE-LOSS".into(),
                name: "Synthetic loss each".into(),
                category: "Restore".into(),
                reorder_point: 0,
            },
        )
        .await?;
    }
    quality_command(
        store,
        &mut originals,
        LOSS_SOURCE,
        "restore-loss-location",
        Command::CreateLocation {
            code: "LOSS-A".into(),
            zone: "Storage".into(),
            kind: "storage".into(),
        },
    )
    .await?;
    quality_command(
        store,
        &mut originals,
        LOSS_SOURCE,
        "restore-loss-opening",
        Command::CountStock {
            sku: "RESTORE-LOSS".into(),
            lot: "ORIGINAL-LOSS-LOT".into(),
            location: "LOSS-A".into(),
            counted: 10,
            reason: "Synthetic original source custody".into(),
        },
    )
    .await?;
    let dispatched = quality_command(
        store,
        &mut originals,
        LOSS_SOURCE,
        "restore-loss-dispatch",
        Command::DispatchTransfer {
            reference: "RESTORE-LOSS-TRANSFER".into(),
            destination_warehouse_id: LOSS_DESTINATION.into(),
            carrier: "Synthetic loss carrier".into(),
            handoff_reference: "ORIGINAL-LOSS-HANDOFF".into(),
            lines: vec![stateset_warehouse::transfers::LineInput {
                id: "original-loss-line".into(),
                sku: "RESTORE-LOSS".into(),
                lot: "ORIGINAL-LOSS-LOT".into(),
                location: "LOSS-A".into(),
                quantity: 5,
            }],
        },
    )
    .await?;
    let tid = entity(&dispatched)?;
    quality_command(
        store,
        &mut originals,
        LOSS_DESTINATION,
        "restore-loss-arrival",
        Command::ReceiveTransfer {
            transfer_id: tid.clone(),
            line_id: "original-loss-line".into(),
            quantity: 1,
            location: "RECV-01".into(),
            arrival_reference: "One original each physically arrived".into(),
        },
    )
    .await?;
    let loss = quality_command(
        store,
        &mut originals,
        LOSS_DESTINATION,
        "restore-loss-record",
        Command::RecordTransferLoss {
            transfer_id: tid.clone(),
            line_id: "original-loss-line".into(),
            quantity: 2,
            incident_reference: "RESTORE-CARRIER-LOSS-CASE".into(),
            reason: "Carrier and controller confirmed original missing units".into(),
        },
    )
    .await?;
    let lid = entity(&loss)?;
    quality_command(
        store,
        &mut originals,
        LOSS_DESTINATION,
        "restore-loss-reversal",
        Command::ReverseTransferLoss {
            loss_id: lid.clone(),
            quantity: 1,
            reason: "One original unit located; actual arrival remains separate".into(),
        },
    )
    .await?;
    let transfer = quality_transfer(store, &tid).await?;
    require(
        transfer["lines"][0]["quantity"] == 5
            && transfer["lines"][0]["received"] == 1
            && transfer["lines"][0]["lost"] == 1
            && transfer["lines"][0]["loss_record_count"] == 2,
        "Recovery point must retain original partial arrival, active loss and capped reversal",
    )?;
    let journal = retained_loss_journal(store, &tid).await?;
    require(
        journal["records"].as_array().is_some_and(|r| r.len() == 2)
            && journal["records"][0]["id"] == lid
            && journal["records"][0]["reversed"] == 1,
        "Nonempty original loss journal required",
    )?;
    Ok(
        json!({"source_warehouse_id":LOSS_SOURCE,"destination_warehouse_id":LOSS_DESTINATION,"transfer_id":tid,"loss_id":lid,"transfer":transfer,"journal":journal,"originals":originals}),
    )
}
async fn recover_transfer_loss(store: &Store, fixture: &Value) -> Result<Value> {
    let tid = fixture["transfer_id"]
        .as_str()
        .ok_or("Missing original loss transfer")?;
    let lid = fixture["loss_id"]
        .as_str()
        .ok_or("Missing original loss identity")?;
    let before = snapshot(store).await?;
    for command in [
        Command::ReverseTransferLoss {
            loss_id: lid.into(),
            quantity: 2,
            reason: "Must reject excess remaining original reversal".into(),
        },
        Command::ReceiveTransfer {
            transfer_id: tid.into(),
            line_id: "original-loss-line".into(),
            quantity: 4,
            location: "RECV-01".into(),
            arrival_reference: "Must reject arrival beyond remaining transit".into(),
        },
    ] {
        let request = Request {
            serial_inputs: Vec::new(),
            unit_inputs: Vec::new(),
            request_id: stateset_warehouse::domain::id(),
            expected_version: application(store.state(TENANT, LOSS_DESTINATION).await)?.version,
            resource_versions: None,
            command,
        };
        require(
            store
                .execute(&identity(), LOSS_DESTINATION, &request)
                .await
                .err()
                .is_some_and(|e| e.status == 422),
            "Restored original loss/arrival allowance was exceeded",
        )?;
    }
    require(
        snapshot(store).await? == before,
        "Rejected restored loss work changed evidence",
    )?;
    let physical = application(store.state(TENANT, LOSS_DESTINATION).await)?;
    let mut originals = Vec::new();
    quality_command(
        store,
        &mut originals,
        LOSS_DESTINATION,
        "restore-loss-new-reversal",
        Command::ReverseTransferLoss {
            loss_id: lid.into(),
            quantity: 1,
            reason: "Remaining original missing unit located after restore".into(),
        },
    )
    .await?;
    let reopened = application(store.state(TENANT, LOSS_DESTINATION).await)?;
    require(
        physical.stocks == reopened.stocks && physical.movements == reopened.movements,
        "A restored loss reversal must not credit warehouse custody",
    )?;
    quality_command(
        store,
        &mut originals,
        LOSS_DESTINATION,
        "restore-loss-new-arrival",
        Command::ReceiveTransfer {
            transfer_id: tid.into(),
            line_id: "original-loss-line".into(),
            quantity: 1,
            location: "RECV-01".into(),
            arrival_reference: "Actual recovered original each observed after restore".into(),
        },
    )
    .await?;
    let proof = application(store.reconciliation(TENANT, LOSS_DESTINATION).await)?;
    require(
        proof["ok"] == true
            && proof["transfers"][0]["lines"][0]["received"] == 2
            && proof["transfers"][0]["lines"][0]["lost"] == 0
            && proof["transfers"][0]["lines"][0]["in_transit"] == 3,
        "Actual recovered arrival must retain original custody and unresolved transit",
    )?;
    Ok(
        json!({"excess_arrival_and_reversal_rejected_without_changes":true,"reversal_credited_no_stock":true,"actual_recovered_arrival_succeeded":true,"proof":proof,"originals":originals}),
    )
}
async fn retained_lot_trace(store: &Store) -> Result<Value> {
    use axum::{
        extract::{Query, State as AppState},
        Extension,
    };
    use stateset_warehouse::{
        api::App,
        integrations::Connections,
        lot_trace::{self, PageQuery, Section},
    };
    let app = App {
        store: store.clone(),
        credentials: std::sync::Arc::new(Vec::new()),
        demo: false,
        connections: Connections::default(),
    };
    let mut review: Option<String> = None;
    let mut summary = Value::Null;
    let mut sections = serde_json::Map::new();
    for section in [
        Section::Facilities,
        Section::Positions,
        Section::Allocations,
        Section::Receipts,
        Section::Shipments,
        Section::Transfers,
        Section::Movements,
        Section::Policies,
        Section::Decisions,
    ] {
        let name = serde_json::to_value(section)?
            .as_str()
            .ok_or("Trace section missing")?
            .to_string();
        let mut after = None;
        let mut items = Vec::new();
        let mut keys = std::collections::BTreeSet::new();
        loop {
            let page = application(
                lot_trace::page(
                    AppState(app.clone()),
                    Extension(identity()),
                    Ok(Query(PageQuery {
                        sku: QUALITY_SKU.into(),
                        lot: QUALITY_LOT.into(),
                        section: Some(section),
                        limit: Some(2),
                        review: review.clone(),
                        after: after.clone(),
                    })),
                )
                .await,
            )?
            .0;
            require(
                page["page_source_ok"] == true
                    && page["summary"]["physical_reconciliation_ok"] == true,
                "Restored lot trace page has an invalid original source or custody proof",
            )?;
            if let Some(expected) = &review {
                require(
                    page["review"] == *expected && page["summary"] == summary,
                    "Lot trace source changed while collecting the isolated recovery point",
                )?;
            } else {
                review = Some(
                    page["review"]
                        .as_str()
                        .ok_or("Trace review missing")?
                        .to_string(),
                );
                summary = page["summary"].clone();
            }
            for item in page["items"]
                .as_array()
                .ok_or("Trace page records missing")?
            {
                require(
                    keys.insert(json!([item["warehouse_id"], item["key"]]).to_string()),
                    "Trace page repeated an original source record",
                )?;
                items.push(item.clone());
            }
            after = page["next_cursor"].as_str().map(str::to_owned);
            if after.is_none() {
                break;
            }
            require(
                items.len() < 200,
                "Synthetic trace fixture pagination did not finish",
            )?;
        }
        require(
            summary["sections"][name.as_str()] == items.len() as u64,
            "Trace section lost original records across pages",
        )?;
        sections.insert(name, json!(items));
    }
    require(summary["physical"]==8 && summary["reserved"]==2 && summary["usable_storage"]==0 && summary["staged"]==1 && summary["receiving"]==1,"Trace fixture must retain seven/one source/destination custody, protected staging and no usable held stock")?;
    let transfer = sections
        .get("transfers")
        .and_then(Value::as_array)
        .ok_or("Trace transfer records missing")?;
    require(
        transfer.len() == 1
            && transfer[0]["document"]["dispatched"] == 2
            && transfer[0]["document"]["received"] == 1,
        "Trace must retain the exact original partial transfer and one in transit",
    )?;
    let shipments = sections
        .get("shipments")
        .and_then(Value::as_array)
        .ok_or("Trace package records missing")?;
    require(
        shipments.len() == 2
            && shipments
                .iter()
                .any(|r| r["document"]["state"] == "packed" && r["document"]["quantity"] == 1)
            && shipments
                .iter()
                .any(|r| r["document"]["state"] == "handed_off" && r["document"]["quantity"] == 1),
        "Trace must keep actual prior handoff and separate protected packing",
    )?;
    let receipts = sections
        .get("receipts")
        .and_then(Value::as_array)
        .ok_or("Trace return receipt missing")?;
    require(receipts.len()==1 && receipts[0]["document"]["status"]=="cancelled" && receipts[0]["document"]["expected"]==1 && receipts[0]["document"]["received"]==0 && receipts[0]["document"]["cancelled"]==1,"Trace must retain the original capped RMA and its unreceived cancellation without inventing receiving")?;
    Ok(
        json!({"sku":QUALITY_SKU,"lot":QUALITY_LOT,"review":review,"summary":summary,"sections":sections,"scope":"all_nine_sections_of_the_nonempty_synthetic_recovery_point","observation_clock_excluded_from_immutable_fact_comparison":true}),
    )
}

async fn quality_fixture(store: &Store) -> Result<Value> {
    let mut originals = Vec::new();
    for (warehouse, key) in [
        (WAREHOUSE, "restore-quality-source-product"),
        (QUALITY_DESTINATION, "restore-quality-destination-product"),
    ] {
        quality_command(
            store,
            &mut originals,
            warehouse,
            key,
            Command::CreateProduct {
                serial_controlled: false,
                sku: QUALITY_SKU.into(),
                name: "Synthetic expiring restore item".into(),
                category: "Rehearsal".into(),
                reorder_point: 1,
            },
        )
        .await?;
    }
    quality_command(
        store,
        &mut originals,
        WAREHOUSE,
        "restore-quality-count",
        Command::CountStock {
            sku: QUALITY_SKU.into(),
            lot: QUALITY_LOT.into(),
            location: "A-01".into(),
            counted: 10,
            reason: "Witnessed synthetic quality opening count".into(),
        },
    )
    .await?;
    for (warehouse, key) in [
        (WAREHOUSE, "restore-quality-source-policy"),
        (QUALITY_DESTINATION, "restore-quality-destination-policy"),
    ] {
        quality_command(
            store,
            &mut originals,
            warehouse,
            key,
            Command::SetProductExpiryPolicy {
                sku: QUALITY_SKU.into(),
                required: true,
                reason: "Original expiry labels are required for this SKU".into(),
            },
        )
        .await?;
    }
    let missing = application(store.state(TENANT, WAREHOUSE).await)?;
    require(
        stateset_warehouse::lot_control::availability_at(
            &missing,
            QUALITY_SKU,
            QUALITY_LOT,
            chrono::Utc::now(),
        ) == stateset_warehouse::lot_control::Availability::ExpiryMissing,
        "Required missing-label stock must retain custody without becoming available",
    )?;
    quality_command(
        store,
        &mut originals,
        WAREHOUSE,
        "restore-quality-original-expiry",
        Command::RecordLotExpiry {
            sku: QUALITY_SKU.into(),
            lot: QUALITY_LOT.into(),
            expires_at: "2099-10-21T05:00:00-07:00".into(),
            reason: "Read the exact original synthetic manufacturer label".into(),
        },
    )
    .await?;
    let dispatch = quality_command(
        store,
        &mut originals,
        WAREHOUSE,
        "restore-quality-dispatch",
        Command::DispatchTransfer {
            reference: "RESTORE-QUALITY-TRANSFER".into(),
            destination_warehouse_id: QUALITY_DESTINATION.into(),
            carrier: "Synthetic carrier".into(),
            handoff_reference: "QUALITY-ORIGINAL-DEPARTURE".into(),
            lines: vec![stateset_warehouse::transfers::LineInput {
                id: "quality-transfer-line".into(),
                sku: QUALITY_SKU.into(),
                lot: QUALITY_LOT.into(),
                location: "A-01".into(),
                quantity: 2,
            }],
        },
    )
    .await?;
    let transfer_id = entity(&dispatch)?;
    let created = quality_command(
        store,
        &mut originals,
        WAREHOUSE,
        "restore-quality-order",
        Command::CreateOrder {
            reference: "RESTORE-QUALITY-ORDER".into(),
            source_order_id: None,
            customer: "Synthetic expiring customer".into(),
            destination: "Synthetic customer dock".into(),
            priority: "normal".into(),
            lines: vec![LineInput {
                id: "quality-line".into(),
                sku: QUALITY_SKU.into(),
                quantity: 3,
            }],
        },
    )
    .await?;
    let order_id = entity(&created)?;
    quality_command(
        store,
        &mut originals,
        WAREHOUSE,
        "restore-quality-allocate",
        Command::AllocateAvailable {
            order_id: order_id.clone(),
        },
    )
    .await?;
    quality_command(
        store,
        &mut originals,
        WAREHOUSE,
        "restore-quality-pick",
        Command::Pick {
            order_id: order_id.clone(),
            line_id: "quality-line".into(),
            location: "A-01".into(),
            lot: QUALITY_LOT.into(),
            quantity: 2,
        },
    )
    .await?;
    let handed = quality_command(
        store,
        &mut originals,
        WAREHOUSE,
        "restore-quality-packed-first",
        Command::PackShipment {
            order_id: order_id.clone(),
            reference: "RESTORE-QUALITY-HANDED".into(),
            lines: vec![stateset_warehouse::split_fulfillment::LineInput {
                line_id: "quality-line".into(),
                location: "A-01".into(),
                lot: QUALITY_LOT.into(),
                quantity: 1,
            }],
        },
    )
    .await?;
    quality_command(
        store,
        &mut originals,
        WAREHOUSE,
        "restore-quality-handoff",
        Command::HandoffShipment {
            shipment_id: entity(&handed)?,
            carrier: "Synthetic customer carrier".into(),
            handoff_reference: "QUALITY-WITNESSED-CUSTOMER-HANDOFF".into(),
        },
    )
    .await?;
    let packed = quality_command(
        store,
        &mut originals,
        WAREHOUSE,
        "restore-quality-packed-active",
        Command::PackShipment {
            order_id: order_id.clone(),
            reference: "RESTORE-QUALITY-ACTIVE".into(),
            lines: vec![stateset_warehouse::split_fulfillment::LineInput {
                line_id: "quality-line".into(),
                location: "A-01".into(),
                lot: QUALITY_LOT.into(),
                quantity: 1,
            }],
        },
    )
    .await?;
    let active_package_id = entity(&packed)?;
    let return_receipt = quality_command(
        store,
        &mut originals,
        WAREHOUSE,
        "restore-quality-return-authorization",
        Command::CreateReturn {
            reference: "RESTORE-QUALITY-RMA".into(),
            order_id: order_id.clone(),
            reason: "Only the actual earlier handed-off unit may be returned".into(),
            lines: vec![stateset_warehouse::returns::ReturnLineInput {
                id: "quality-return-line".into(),
                order_line_id: "quality-line".into(),
                lot: QUALITY_LOT.into(),
                quantity: 1,
            }],
        },
    )
    .await?;
    quality_command(
        store,
        &mut originals,
        WAREHOUSE,
        "restore-quality-return-cancellation",
        Command::CancelReceipt {
            receipt_id: entity(&return_receipt)?,
            reason: "Customer retained the shipped unit; no physical return occurred".into(),
        },
    )
    .await?;
    quality_command(
        store,
        &mut originals,
        WAREHOUSE,
        "restore-quality-correct-expiry",
        Command::CorrectLotExpiry {
            sku: QUALITY_SKU.into(),
            lot: QUALITY_LOT.into(),
            expires_at: Some("2000-01-01T00:00:00Z".into()),
            reason: "Supervisor corrected a label transcription after original observed work"
                .into(),
        },
    )
    .await?;
    let first = quality_command(
        store,
        &mut originals,
        WAREHOUSE,
        "restore-quality-first-hold",
        Command::QuarantineLot {
            sku: QUALITY_SKU.into(),
            lot: QUALITY_LOT.into(),
            reason: "Independent label investigation".into(),
        },
    )
    .await?;
    quality_command(
        store,
        &mut originals,
        WAREHOUSE,
        "restore-quality-second-hold",
        Command::QuarantineLot {
            sku: QUALITY_SKU.into(),
            lot: QUALITY_LOT.into(),
            reason: "Independent temperature investigation remains open".into(),
        },
    )
    .await?;
    quality_command(
        store,
        &mut originals,
        WAREHOUSE,
        "restore-quality-release-first",
        Command::ReleaseLotHold {
            hold_id: entity(&first)?,
            reason: "Supervisor closed only the exact label investigation".into(),
        },
    )
    .await?;
    quality_command(
        store,
        &mut originals,
        QUALITY_DESTINATION,
        "restore-quality-arrival",
        Command::ReceiveTransfer {
            transfer_id: transfer_id.clone(),
            line_id: "quality-transfer-line".into(),
            quantity: 1,
            location: "RECV-01".into(),
            arrival_reference: "QUALITY-WITNESSED-ONE-ARRIVED".into(),
        },
    )
    .await?;
    let source = application(store.state(TENANT, WAREHOUSE).await)?;
    let destination = application(store.state(TENANT, QUALITY_DESTINATION).await)?;
    let source_control = source
        .lot_controls
        .iter()
        .find(|c| c.sku == QUALITY_SKU && c.lot == QUALITY_LOT)
        .ok_or("Missing source quality control")?;
    let destination_control = destination
        .lot_controls
        .iter()
        .find(|c| c.sku == QUALITY_SKU && c.lot == QUALITY_LOT)
        .ok_or("Missing propagated destination control")?;
    require(source_control.expires_at.as_deref()==Some("2000-01-01T00:00:00+00:00") && source_control.holds.len()==1 && destination_control.expires_at==source_control.expires_at && destination_control.holds.len()==1,"Corrected expiry, independent active/released source holds and conservative arrival restriction must be nonempty")?;
    require(
        source
            .products
            .iter()
            .any(|p| p.sku == QUALITY_SKU && p.expiry_required)
            && destination
                .products
                .iter()
                .any(|p| p.sku == QUALITY_SKU && p.expiry_required),
        "Both actual product policies must require labels",
    )?;
    let source_physical: i64 = source
        .stocks
        .iter()
        .filter(|s| s.sku == QUALITY_SKU)
        .map(|s| s.on_hand)
        .sum();
    let source_reserved: i64 = source
        .stocks
        .iter()
        .filter(|s| s.sku == QUALITY_SKU)
        .map(|s| s.reserved)
        .sum();
    let destination_physical: i64 = destination
        .stocks
        .iter()
        .filter(|s| s.sku == QUALITY_SKU)
        .map(|s| s.on_hand)
        .sum();
    require(source_physical==7 && source_reserved==2 && destination_physical==1,"Quality fixture custody must retain7 source,1 arrived,1 in transit and1 actually handed off")?;
    let order = application(store.detail(TENANT, WAREHOUSE, "orders", &order_id).await)?;
    let packages = quality_packages(store, &order_id).await?;
    let transfer = quality_transfer(store, &transfer_id).await?;
    let eligibility = application(
        stateset_warehouse::returns::eligible(store, &identity(), WAREHOUSE, &order_id).await,
    )?;
    require(
        eligibility["lines"]
            .as_array()
            .ok_or("Missing quality return lines")?
            .iter()
            .any(|l| l["sku"] == QUALITY_SKU && l["shipped"] == 1 && l["remaining"] == 1),
        "Current quarantine/expiry must not rewrite the one original handed-off unit",
    )?;
    let decisions: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM wms_lot_decisions WHERE tenant=$1 AND warehouse_id=$2 AND sku=$3",
    )
    .bind(TENANT)
    .bind(WAREHOUSE)
    .bind(QUALITY_SKU)
    .fetch_one(&store.pool)
    .await?;
    require(decisions==6,"Quality restore needs original dispatch/allocation/pick/two packs/handoff eligibility decisions")?;
    require(
        application(store.reconciliation(TENANT, WAREHOUSE).await)?["ok"] == true
            && application(store.reconciliation(TENANT, QUALITY_DESTINATION).await)?["ok"] == true,
        "Both original quality warehouse proofs must be healthy",
    )?;
    Ok(
        json!({"sku":QUALITY_SKU,"lot":QUALITY_LOT,"order_id":order_id,"order":order,"active_package_id":active_package_id,"packages":packages,"transfer_id":transfer_id,"transfer":transfer,"source_control":source_control,"destination_control":destination_control,"eligibility":eligibility,"source_eligibility_decisions":decisions,"originals":originals}),
    )
}
async fn verify_quality_fixture(store: &Store, q: &Value) -> Result<()> {
    let order_id = q["order_id"]
        .as_str()
        .ok_or("Missing quality restore order")?;
    require(
        application(store.detail(TENANT, WAREHOUSE, "orders", order_id).await)? == q["order"],
        "Quality order, actual shipped allowance and staged ownership changed on restore",
    )?;
    require(
        quality_packages(store, order_id).await? == q["packages"],
        "Original held active package and historical handoff documents changed on restore",
    )?;
    require(
        quality_transfer(
            store,
            q["transfer_id"]
                .as_str()
                .ok_or("Missing quality transfer")?,
        )
        .await?
            == q["transfer"],
        "Quality dispatch, conservative arrival and goods in transit changed on restore",
    )?;
    require(
        application(
            stateset_warehouse::returns::eligible(store, &identity(), WAREHOUSE, order_id).await,
        )? == q["eligibility"],
        "Current quality cannot replace original shipped return allowance",
    )?;
    for (warehouse, field) in [
        (WAREHOUSE, "source_control"),
        (QUALITY_DESTINATION, "destination_control"),
    ] {
        let state = application(store.state(TENANT, warehouse).await)?;
        require(
            state
                .products
                .iter()
                .any(|p| p.sku == QUALITY_SKU && p.expiry_required),
            "Required-label product policy was not restored",
        )?;
        require(
            json!(state
                .lot_controls
                .iter()
                .find(|c| c.sku == QUALITY_SKU && c.lot == QUALITY_LOT))
                == q[field],
            "Original corrected expiry and exact open holds were not restored",
        )?;
    }
    for original in q["originals"]
        .as_array()
        .ok_or("Missing original quality workflow requests")?
    {
        let request: Request = serde_json::from_value(original["request"].clone())?;
        let warehouse = original["warehouse_id"]
            .as_str()
            .ok_or("Missing original quality warehouse")?;
        let replay = application(store.execute(&identity(), warehouse, &request).await)?;
        require(replay["replayed"]==true&&replay["result"]==original["response"]["result"],"Original quality/physical request did not replay exactly after corrected expiry or across restoration")?;
    }
    Ok(())
}
async fn recover_quality_fixture(store: &Store, q: &Value) -> Result<Value> {
    let order_id = q["order_id"]
        .as_str()
        .ok_or("Missing recovered quality order")?
        .to_string();
    let package_id = q["active_package_id"]
        .as_str()
        .ok_or("Missing recovered active package")?
        .to_string();
    let before = snapshot(store).await?;
    let denied = Request {
        serial_inputs: Vec::new(),
        unit_inputs: Vec::new(),
        request_id: "restore-quality-denied-handoff".into(),
        expected_version: application(store.state(TENANT, WAREHOUSE).await)?.version,
        resource_versions: None,
        command: Command::HandoffShipment {
            shipment_id: package_id.clone(),
            carrier: "Must remain staged".into(),
            handoff_reference: "Do not infer new handoff".into(),
        },
    };
    require(
        store
            .execute(&identity(), WAREHOUSE, &denied)
            .await
            .is_err_and(|e| e.status == 422),
        "Restored active held/expired package must reject a new handoff",
    )?;
    require(
        snapshot(store).await? == before,
        "Rejected held handoff changed restored physical or audit evidence",
    )?;
    let mut recovery = Vec::new();
    quality_command(
        store,
        &mut recovery,
        WAREHOUSE,
        "restore-quality-post-unpack",
        Command::UnpackShipment {
            shipment_id: package_id,
            reason: "Actually unpacked held restored package for recovery".into(),
        },
    )
    .await?;
    quality_command(
        store,
        &mut recovery,
        WAREHOUSE,
        "restore-quality-post-unpick",
        Command::Unpick {
            order_id: order_id.clone(),
            line_id: "quality-line".into(),
            location: "A-01".into(),
            lot: QUALITY_LOT.into(),
            quantity: 1,
            reason: "Actually returned the one unshipped staged unit".into(),
        },
    )
    .await?;
    quality_command(
        store,
        &mut recovery,
        WAREHOUSE,
        "restore-quality-post-release",
        Command::ReleaseUnpicked {
            order_id: order_id.clone(),
            reason: "Release only remaining unpicked held reservations".into(),
        },
    )
    .await?;
    let state = application(store.state(TENANT, WAREHOUSE).await)?;
    require(
        state
            .stocks
            .iter()
            .filter(|s| s.sku == QUALITY_SKU)
            .map(|s| s.on_hand)
            .sum::<i64>()
            == 7
            && state
                .stocks
                .iter()
                .filter(|s| s.sku == QUALITY_SKU)
                .map(|s| s.reserved)
                .sum::<i64>()
                == 0,
        "Post-restore quality recovery changed custody or retained unpicked reservations",
    )?;
    let allowance = application(
        stateset_warehouse::returns::eligible(store, &identity(), WAREHOUSE, &order_id).await,
    )?;
    require(allowance["lines"].as_array().ok_or("Missing original quality handoff allowance")?.iter().any(|l|l["sku"]==QUALITY_SKU&&l["shipped"]==1&&l["remaining"]==1),"Unpacking/unpicking must not erase the actual original handoff or enlarge return allowance")?;
    let proof = application(store.reconciliation(TENANT, WAREHOUSE).await)?;
    require(
        proof["ok"] == true,
        "New recovery work must retain independent original quality and package proofs",
    )?;
    Ok(
        json!({"unavailable_handoff_rejected_without_changes":true,"actual_unpack_unpick_release_succeeded":true,"source_physical":7,"source_reserved":0,"original_shipped_return_allowance":1,"recovery_commands":recovery,"proof":proof}),
    )
}

const UNIT_WAREHOUSE: &str = "wh-unit-packs";
const UNIT_SKU: &str = "RESTORE-PACK";
async fn unit_command(
    store: &Store,
    originals: &mut Vec<Value>,
    key: &str,
    command: Command,
    inputs: Vec<stateset_warehouse::units::Input>,
) -> Result<Value> {
    let request = Request {
        request_id: key.into(),
        expected_version: application(store.state(TENANT, UNIT_WAREHOUSE).await)?.version,
        resource_versions: None,
        serial_inputs: Vec::new(),
        unit_inputs: inputs,
        command,
    };
    let response = application(store.execute(&identity(), UNIT_WAREHOUSE, &request).await)?;
    originals.push(json!({"warehouse_id":UNIT_WAREHOUSE,"request":request,"response":response}));
    Ok(response)
}
fn unit_definition(code: &str, factor: i64, enabled: bool) -> Command {
    Command::SetUnitPack {
        sku: UNIT_SKU.into(),
        code: code.into(),
        label: format!("Reviewed {code}"),
        each_per_pack: factor,
        enabled,
        reason: "Witnessed synthetic packaging label".into(),
    }
}
fn unit_input(
    definition: &str,
    count: i64,
    target: &[&str],
) -> Vec<stateset_warehouse::units::Input> {
    vec![stateset_warehouse::units::Input {
        target: target.iter().map(|s| (*s).into()).collect(),
        sku: UNIT_SKU.into(),
        pack_code: "CASE".into(),
        definition_id: definition.into(),
        pack_quantity: count,
        each_per_pack: 12,
    }]
}
async fn retained_units(store: &Store) -> Result<Value> {
    let definitions:Value=sqlx::query_scalar("SELECT COALESCE(jsonb_agg(to_jsonb(p)-'tenant' ORDER BY version),'[]') FROM wms_unit_pack_changes p WHERE tenant=$1 AND warehouse_id=$2").bind(TENANT).bind(UNIT_WAREHOUSE).fetch_one(&store.pool).await?;
    let observations:Value=sqlx::query_scalar("SELECT COALESCE(jsonb_agg(to_jsonb(r)-'tenant' ORDER BY version,ordinal),'[]') FROM wms_unit_observations r WHERE tenant=$1 AND warehouse_id=$2").bind(TENANT).bind(UNIT_WAREHOUSE).fetch_one(&store.pool).await?;
    let state = application(store.state(TENANT, UNIT_WAREHOUSE).await)?;
    Ok(
        json!({"definitions":definitions,"observations":observations,"stock":state.stocks,"active_packs":state.unit_packs}),
    )
}
async fn unit_fixture(store: &Store) -> Result<Value> {
    let mut originals = Vec::new();
    quality_command(
        store,
        &mut originals,
        UNIT_WAREHOUSE,
        "restore-unit-warehouse",
        Command::CreateWarehouse {
            code: "UNIT".into(),
            name: "Synthetic packaging facility".into(),
            city: "Vancouver".into(),
            timezone: "America/Vancouver".into(),
        },
    )
    .await?;
    quality_command(
        store,
        &mut originals,
        UNIT_WAREHOUSE,
        "restore-unit-product",
        Command::CreateProduct {
            serial_controlled: false,
            sku: UNIT_SKU.into(),
            name: "Synthetic packaging product".into(),
            category: "Test".into(),
            reorder_point: 0,
        },
    )
    .await?;
    quality_command(
        store,
        &mut originals,
        UNIT_WAREHOUSE,
        "restore-unit-bin",
        Command::CreateLocation {
            code: "PACK-01".into(),
            zone: "Synthetic".into(),
            kind: "storage".into(),
        },
    )
    .await?;
    let defined = unit_command(
        store,
        &mut originals,
        "restore-unit-original-case",
        unit_definition("CASE", 12, true),
        vec![],
    )
    .await?;
    let did = entity(&defined)?;
    let receipt = unit_command(
        store,
        &mut originals,
        "restore-unit-receipt",
        Command::CreateReceipt {
            reference: "RESTORE-CASES".into(),
            supplier: "Synthetic".into(),
            lines: vec![ReceiptInput {
                id: "a".into(),
                sku: UNIT_SKU.into(),
                lot: "ORIGINAL-PACK-LOT".into(),
                quantity: 36,
            }],
        },
        unit_input(&did, 3, &["a"]),
    )
    .await?;
    unit_command(
        store,
        &mut originals,
        "restore-unit-receive",
        Command::Receive {
            receipt_id: entity(&receipt)?,
            line_id: "a".into(),
            location: "RECV-01".into(),
            quantity: 24,
        },
        unit_input(&did, 2, &[]),
    )
    .await?;
    unit_command(
        store,
        &mut originals,
        "restore-unit-move",
        Command::MoveStock {
            sku: UNIT_SKU.into(),
            lot: "ORIGINAL-PACK-LOT".into(),
            from: "RECV-01".into(),
            to: "PACK-01".into(),
            quantity: 12,
        },
        unit_input(&did, 1, &[]),
    )
    .await?;
    let order = unit_command(
        store,
        &mut originals,
        "restore-unit-order",
        Command::CreateOrder {
            reference: "RESTORE-CASE-ORDER".into(),
            source_order_id: None,
            customer: "Synthetic".into(),
            destination: "Test".into(),
            priority: "normal".into(),
            lines: vec![LineInput {
                id: "a".into(),
                sku: UNIT_SKU.into(),
                quantity: 12,
            }],
        },
        unit_input(&did, 1, &["a"]),
    )
    .await?;
    let oid = entity(&order)?;
    unit_command(
        store,
        &mut originals,
        "restore-unit-allocate",
        Command::Allocate {
            order_id: oid.clone(),
        },
        vec![],
    )
    .await?;
    unit_command(
        store,
        &mut originals,
        "restore-unit-pick",
        Command::Pick {
            order_id: oid.clone(),
            line_id: "a".into(),
            location: "PACK-01".into(),
            lot: "ORIGINAL-PACK-LOT".into(),
            quantity: 12,
        },
        unit_input(&did, 1, &[]),
    )
    .await?;
    let package = unit_command(
        store,
        &mut originals,
        "restore-unit-pack",
        Command::PackShipment {
            order_id: oid.clone(),
            reference: "RESTORE-CASE-PACKAGE".into(),
            lines: vec![stateset_warehouse::split_fulfillment::LineInput {
                line_id: "a".into(),
                location: "PACK-01".into(),
                lot: "ORIGINAL-PACK-LOT".into(),
                quantity: 12,
            }],
        },
        unit_input(&did, 1, &["a", "PACK-01", "ORIGINAL-PACK-LOT"]),
    )
    .await?;
    unit_command(
        store,
        &mut originals,
        "restore-unit-handoff",
        Command::HandoffShipment {
            shipment_id: entity(&package)?,
            carrier: "Synthetic".into(),
            handoff_reference: "RESTORE-CASE-HANDOFF".into(),
        },
        vec![],
    )
    .await?;
    unit_command(
        store,
        &mut originals,
        "restore-unit-changed-case",
        unit_definition("CASE", 6, true),
        vec![],
    )
    .await?;
    unit_command(
        store,
        &mut originals,
        "restore-unit-inner",
        unit_definition("INNER", 3, true),
        vec![],
    )
    .await?;
    unit_command(
        store,
        &mut originals,
        "restore-unit-disable-inner",
        unit_definition("INNER", 3, false),
        vec![],
    )
    .await?;
    let evidence = retained_units(store).await?;
    require(evidence["definitions"].as_array().is_some_and(|a|a.len()==4) && evidence["observations"].as_array().is_some_and(|a|a.len()==6),"Unit restore fixture must include original changed/disabled definitions and nonempty observations")?;
    require(
        application(store.state(TENANT, UNIT_WAREHOUSE).await)?
            .stocks
            .iter()
            .map(|s| s.on_hand)
            .sum::<i64>()
            == 12,
        "Original case handoff must leave twelve each in custody",
    )?;
    Ok(
        json!({"warehouse_id":UNIT_WAREHOUSE,"original_definition_id":did,"originals":originals,"evidence":evidence}),
    )
}
async fn replay_unit_fixture(store: &Store, q: &Value) -> Result<()> {
    require(
        retained_units(store).await? == q["evidence"],
        "Restored original pack definitions, quantities or disabled codes changed",
    )?;
    for original in q["originals"]
        .as_array()
        .ok_or("Missing original pack requests")?
    {
        let r: Request = serde_json::from_value(original["request"].clone())?;
        let replay = application(store.execute(&identity(), UNIT_WAREHOUSE, &r).await)?;
        require(
            replay["replayed"] == true && replay["result"] == original["response"]["result"],
            "Restored pack request must replay its original conversion",
        )?;
    }
    require(
        retained_units(store).await? == q["evidence"],
        "Replaying original case observations changed their evidence",
    )?;
    Ok(())
}
async fn recover_unit_fixture(store: &Store, q: &Value) -> Result<Value> {
    let state = application(store.state(TENANT, UNIT_WAREHOUSE).await)?;
    let original = q["original_definition_id"]
        .as_str()
        .ok_or("Missing original pack ID")?;
    let mut r = Request {
        request_id: "restore-unit-stale-definition".into(),
        expected_version: state.version,
        resource_versions: None,
        serial_inputs: Vec::new(),
        unit_inputs: unit_input(original, 1, &[]),
        command: Command::CountStock {
            sku: UNIT_SKU.into(),
            lot: "ORIGINAL-PACK-LOT".into(),
            location: "RECV-01".into(),
            counted: 12,
            reason: "Observed original pack label requires review".into(),
        },
    };
    let before = snapshot(store).await?;
    require(
        store
            .execute(&identity(), UNIT_WAREHOUSE, &r)
            .await
            .is_err_and(|e| e.status == 409),
        "Fresh old-definition observation must require review after restore",
    )?;
    require(
        snapshot(store).await? == before,
        "Rejected old definition changed restored tables",
    )?;
    let current = state
        .unit_packs
        .iter()
        .find(|p| p.code == "CASE")
        .ok_or("Missing restored current case")?;
    r.request_id = "restore-unit-fresh-definition".into();
    r.unit_inputs[0].definition_id = current.id.clone();
    r.unit_inputs[0].each_per_pack = current.each_per_pack;
    r.unit_inputs[0].pack_quantity = 2;
    let accepted = application(store.execute(&identity(), UNIT_WAREHOUSE, &r).await)?;
    let proof = application(store.reconciliation(TENANT, UNIT_WAREHOUSE).await)?;
    require(
        proof["ok"] == true && proof["units"]["quantity_observations"] == 7,
        "Post-restore fresh definition must preserve twelve each and original history",
    )?;
    Ok(
        json!({"old_definition_rejected_without_table_changes":true,"fresh_original_request":r,"response":accepted,"proof":proof}),
    )
}

async fn seed(path: &str) -> Result<()> {
    let legacy = history_restore::seed_legacy().await?;
    let store = connect().await?;
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM wms_warehouses")
        .fetch_one(&store.pool)
        .await?;
    require(
        count == 1,
        "Source must contain only the migrated owned legacy fixture",
    )?;
    application(store.provision(TENANT, false).await)?;
    // A second tenant proves that the archive contains every warehouse scope.
    application(store.provision("restore-other-tenant", false).await)?;
    let facility_create = Request {
        serial_inputs: Vec::new(),
        unit_inputs: Vec::new(),
        request_id: "restore-create-facility".into(),
        expected_version: 0,
        resource_versions: None,
        command: Command::CreateWarehouse {
            code: "SECOND".into(),
            name: "Synthetic second facility".into(),
            city: "Restore fixture".into(),
            timezone: "UTC".into(),
        },
    };
    let facility_response = application(
        store
            .execute(&identity(), "wh-restore-secondary", &facility_create)
            .await,
    )?;
    command(
        &store,
        "restore-product",
        Command::CreateProduct {
            serial_controlled: false,
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
        serial_inputs: Vec::new(),
        unit_inputs: Vec::new(),
        request_id: native_store::import_command_id(TENANT, WAREHOUSE, IMPORT),
        expected_version: import_input["expected_version"]
            .as_i64()
            .ok_or("Missing version")?,
        resource_versions: None,
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
    let (_, returned) = command(
        &store,
        "restore-authorize-return",
        Command::CreateReturn {
            reference: "RESTORE-RMA".into(),
            order_id: conditional.order_id.clone(),
            reason: "Synthetic customer return; inspection evidence retained".into(),
            lines: vec![stateset_warehouse::returns::ReturnLineInput {
                id: "return-line".into(),
                order_line_id: conditional.payload.lines[0].line_id.clone(),
                lot: "RESTORE-LOT".into(),
                quantity: 1,
            }],
        },
    )
    .await?;
    let return_receipt_id = entity(&returned)?;
    let reviewed = application(store.state(TENANT, WAREHOUSE).await)?;
    let return_command = Command::Receive {
        receipt_id: return_receipt_id.clone(),
        line_id: "return-line".into(),
        quantity: 1,
        location: "RECV-01".into(),
    };
    let return_receive = Request {
        serial_inputs: Vec::new(),
        unit_inputs: Vec::new(),
        request_id: "restore-scoped-return-receive".into(),
        expected_version: reviewed.version,
        resource_versions: stateset_warehouse::concurrency::reviewed_versions(
            &reviewed,
            &return_command,
        )?,
        command: return_command,
    };
    let return_receive_response =
        application(store.execute(&identity(), WAREHOUSE, &return_receive).await)?;
    let (_, cancelled_receipt) = command(
        &store,
        "restore-create-cancelled-receipt",
        Command::CreateReceipt {
            reference: "RESTORE-CANCELLED-INBOUND".into(),
            supplier: "Synthetic supplier".into(),
            lines: vec![ReceiptInput {
                id: "closed-line".into(),
                sku: "RESTORE-EACH".into(),
                lot: "CLOSED-LOT".into(),
                quantity: 3,
            }],
        },
    )
    .await?;
    let cancelled_receipt_id = entity(&cancelled_receipt)?;
    command(
        &store,
        "restore-before-cancellation-receive",
        Command::Receive {
            receipt_id: cancelled_receipt_id.clone(),
            line_id: "closed-line".into(),
            quantity: 1,
            location: "RECV-01".into(),
        },
    )
    .await?;
    let (cancellation_request, cancellation_response) = command(
        &store,
        "restore-cancel-original",
        Command::CancelReceipt {
            receipt_id: cancelled_receipt_id.clone(),
            reason: "Supplier will not deliver the final two units".into(),
        },
    )
    .await?;
    let source_review = application(
        stateset_warehouse::returns::eligible(
            &store,
            &identity(),
            WAREHOUSE,
            &conditional.order_id,
        )
        .await,
    )?;
    let shipment_source = source_review["shipment_source"].clone();
    require(
        shipment_source["kind"] == "event" && shipment_source["event_id"] == conditional.event_id,
        "Seeded original physical shipment provenance does not match its event",
    )?;
    let destination = "wh-restore-secondary";
    let destination_product = Request {
        serial_inputs: Vec::new(),
        unit_inputs: Vec::new(),
        request_id: "restore-transfer-catalog".into(),
        expected_version: application(store.state(TENANT, destination).await)?.version,
        resource_versions: None,
        command: Command::CreateProduct {
            serial_controlled: false,
            sku: "RESTORE-EACH".into(),
            name: "Synthetic restore SKU".into(),
            category: "General".into(),
            reorder_point: 0,
        },
    };
    application(
        store
            .execute(&identity(), destination, &destination_product)
            .await,
    )?;
    let (transfer_dispatch, transfer_dispatched) = command(
        &store,
        "restore-transfer-dispatch",
        Command::DispatchTransfer {
            reference: "RESTORE-TRANSFER".into(),
            destination_warehouse_id: destination.into(),
            carrier: "Synthetic carrier".into(),
            handoff_reference: "RESTORE-TRANSFER-HANDOFF".into(),
            lines: vec![stateset_warehouse::transfers::LineInput {
                id: "original-line".into(),
                sku: "RESTORE-EACH".into(),
                lot: "RESTORE-LOT".into(),
                location: "A-01".into(),
                quantity: 3,
            }],
        },
    )
    .await?;
    let transfer_id = entity(&transfer_dispatched)?;
    let transfer_arrival = Request {
        serial_inputs: Vec::new(),
        unit_inputs: Vec::new(),
        request_id: "restore-transfer-arrival".into(),
        expected_version: application(store.state(TENANT, destination).await)?.version,
        resource_versions: None,
        command: Command::ReceiveTransfer {
            transfer_id: transfer_id.clone(),
            line_id: "original-line".into(),
            quantity: 1,
            location: "RECV-01".into(),
            arrival_reference: "RESTORE-ONE-ARRIVED".into(),
        },
    };
    let transfer_arrived = application(
        store
            .execute(&identity(), destination, &transfer_arrival)
            .await,
    )?;
    let transfer_proof =
        application(store.reconciliation(TENANT, WAREHOUSE).await)?["transfers"].clone();
    require(
        transfer_proof[0]["lines"][0]["in_transit"] == 2,
        "Transfer fixture must retain goods in transit",
    )?;
    let mut split_fulfillment = split_fixture(&store).await?;
    application(
        stateset_warehouse::replenishment::save_rules(
            &store.pool,
            TENANT,
            WAREHOUSE,
            "synthetic:restore",
            &[stateset_warehouse::replenishment::Rule {
                sku: "RESTORE-SPLIT".into(),
                destination: "SPLIT-A".into(),
                minimum: 1,
                target: 2,
                source_zone: None,
            }],
        )
        .await,
    )?;
    let mut remainder_cancellation = remainder_fixture(&store).await?;
    let lot_controls = quality_fixture(&store).await?;
    let transfer_loss = transfer_loss_fixture(&store).await?;
    let units = unit_fixture(&store).await?;
    let serials = serial_restore::seed(&store).await?;
    let histories = history_restore::seed(&store, legacy, path).await?;
    eprintln!(
        "Serial recovery point: six nonempty identity tables and all six custody states retained"
    );
    let updated_closed = application(
        stateset_warehouse::returns::eligible(
            &store,
            &identity(),
            WAREHOUSE,
            remainder_cancellation["order_id"]
                .as_str()
                .ok_or("Missing prior closed order")?,
        )
        .await,
    )?;
    let mut previous_allowance = remainder_cancellation["eligibility"].clone();
    previous_allowance["version"] = updated_closed["version"].clone();
    require(
        previous_allowance == updated_closed,
        "New quality SKU must not alter another closed order's original return allowance",
    )?;
    remainder_cancellation["eligibility"] = updated_closed;
    let transfer_proof =
        application(store.reconciliation(TENANT, WAREHOUSE).await)?["transfers"].clone();
    split_fulfillment["proof"] =
        application(store.reconciliation(TENANT, WAREHOUSE).await)?["packages"].clone();
    let evidence = snapshot(&store).await?;
    let lot_trace = retained_lot_trace(&store).await?;
    require(
        snapshot(&store).await? == evidence,
        "Read-only lot trace changed the recovery point evidence",
    )?;
    let source_session_timezone: String = sqlx::query_scalar("SHOW timezone")
        .fetch_one(&store.pool)
        .await?;
    let mut manifest = json!({"transfer_loss":transfer_loss,"lot_trace":lot_trace,"source_session_timezone":source_session_timezone,"lot_controls":lot_controls,"remainder_cancellation":remainder_cancellation,"split_fulfillment":split_fulfillment,"schema_version":1,"synthetic":true,"transfer_dispatch":transfer_dispatch,"transfer_dispatched":transfer_dispatched,"transfer_arrival":transfer_arrival,"transfer_arrived":transfer_arrived,"transfer_proof":transfer_proof,"shipment_source":shipment_source,"cancelled_receipt_id":cancelled_receipt_id,"cancellation_request":cancellation_request,"cancellation_response":cancellation_response,"facility_create":facility_create,"facility_response":facility_response,"receive_request":receive,"receive_response":receive_response,"receipt_id":receipt_id,"order_id":order_id,"import_input":import_input,"import_digest":import_digest,"import_request":import_request,"import_response":import_response,"import_frozen":frozen,"source_snapshot":source_snapshot,"transport_digest":transport_digest,"transport_frozen":prepared,"conditional_frozen":conditional,"conditional_digest":conditional_digest,"conditional_rejected":conditional_rejected,"return_receipt_id":return_receipt_id,"return_receive":return_receive,"return_receive_response":return_receive_response,"evidence":evidence});
    manifest["units"] = units;
    manifest["serials"] = serials;
    manifest["operational_histories"] = histories;
    fs::write(path, serde_json::to_vec_pretty(&manifest)?)?;
    store.pool.close().await;
    println!("Seeded physical movements, receiving evidence, source/staging reservations, native receipts, uncertain attempt, and pending outbox in multiple tenant scopes.");
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
    require(restored == manifest["evidence"], "Restored table hashes, warehouse versions, migration digests or independent proofs differ before replay")?;
    eprintln!(
        "Restored {} WMS table hashes; checking original identity histories and retries",
        restored["tables"]
            .as_object()
            .ok_or("Missing restored table evidence")?
            .len()
    );
    history_restore::verify_and_replay(&store, &manifest["operational_histories"]).await?;
    serial_restore::verify_and_replay(&store, &manifest["serials"]).await?;
    replay_unit_fixture(&store, &manifest["units"]).await?;
    require(
        snapshot(&store).await? == restored,
        "Original pack retries changed restored table hashes",
    )?;
    let loss = &manifest["transfer_loss"];
    let loss_transfer = loss["transfer_id"]
        .as_str()
        .ok_or("Missing retained transfer loss")?;
    require(
        quality_transfer(&store, loss_transfer).await? == loss["transfer"]
            && retained_loss_journal(&store, loss_transfer).await? == loss["journal"],
        "Restored loss settlement, exact original journal, reversal or review changed",
    )?;
    for original in loss["originals"]
        .as_array()
        .ok_or("Missing retained original loss workflow")?
    {
        let w = original["warehouse_id"]
            .as_str()
            .ok_or("Missing original loss facility")?;
        let request: Request = serde_json::from_value(original["request"].clone())?;
        let replay = application(store.execute(&identity(), w, &request).await)?;
        require(
            replay["replayed"] == true && replay["result"] == original["response"]["result"],
            "Original loss/reversal/dispatch/arrival did not replay exactly",
        )?;
    }
    require(
        retained_loss_journal(&store, loss_transfer).await? == loss["journal"],
        "Original retries changed the loss journal",
    )?;

    verify_quality_fixture(&store, &manifest["lot_controls"]).await?;
    require(retained_lot_trace(&store).await?==manifest["lot_trace"],"Restored lot trace changed original policy, custody, staged packing, actual handoff, transit or source review")?;
    require(restored == manifest["evidence"],"Restored table hashes, warehouse versions, or full ledger/receipt/allocation proofs differ from the recovery point")?;
    let closed = &manifest["remainder_cancellation"];
    let closed_id = closed["order_id"]
        .as_str()
        .ok_or("Missing cancelled remainder order")?;
    require(
        application(store.detail(TENANT, WAREHOUSE, "orders", closed_id).await)? == closed["order"],
        "Restored partial closure changed original demand or cancellation ownership",
    )?;
    require(
        application(
            stateset_warehouse::returns::eligible(&store, &identity(), WAREHOUSE, closed_id).await,
        )? == closed["eligibility"],
        "Restored closure changed actual shipped return allowance or its captured version",
    )?;
    for original in closed["originals"]
        .as_array()
        .ok_or("Missing remainder original requests")?
    {
        let request: Request = serde_json::from_value(original["request"].clone())?;
        let replay = application(store.execute(&identity(), WAREHOUSE, &request).await)?;
        require(
            replay["replayed"] == true && replay["result"] == original["response"]["result"],
            "Restored commercial closure workflow failed exact original recovery",
        )?;
    }
    let split = &manifest["split_fulfillment"];
    let oid = split["order_id"]
        .as_str()
        .ok_or("Missing partial order identity")?;
    require(
        application(store.detail(TENANT, WAREHOUSE, "orders", oid).await)? == split["order"],
        "Original split allocations, shipped units and backorder were not restored exactly",
    )?;
    require(
        application(store.reconciliation(TENANT, WAREHOUSE).await)?["packages"] == split["proof"],
        "Original packing and closure ownership was not restored exactly",
    )?;
    for original in split["originals"]
        .as_array()
        .ok_or("Missing original split requests")?
    {
        let request: Request = serde_json::from_value(original["request"].clone())?;
        let replay = application(store.execute(&identity(), WAREHOUSE, &request).await)?;
        require(replay["replayed"] == true && replay["result"] == original["response"]["result"], "Original allocation, packing, handoff, unpacking or early return did not replay exactly")?;
    }
    let receive: Request = serde_json::from_value(manifest["receive_request"].clone())?;
    let facility: Request = serde_json::from_value(manifest["facility_create"].clone())?;
    let facility_replay = application(
        store
            .execute(&identity(), "wh-restore-secondary", &facility)
            .await,
    )?;
    require(
        facility_replay["replayed"] == true
            && facility_replay["result"] == manifest["facility_response"]["result"],
        "Restored facility creation did not replay its original receipt",
    )?;
    require(
        application(store.reconciliation(TENANT, WAREHOUSE).await)?["transfers"]
            == manifest["transfer_proof"],
        "Original transfer ownership, arrival and in-transit balance changed during restore",
    )?;
    for (warehouse, request_key, response_key) in [
        (WAREHOUSE, "transfer_dispatch", "transfer_dispatched"),
        (
            "wh-restore-secondary",
            "transfer_arrival",
            "transfer_arrived",
        ),
    ] {
        let original: Request = serde_json::from_value(manifest[request_key].clone())?;
        let replay = application(store.execute(&identity(), warehouse, &original).await)?;
        require(
            replay["replayed"] == true && replay["result"] == manifest[response_key]["result"],
            "Original transfer dispatch/arrival did not replay exactly",
        )?;
    }
    let replay = application(store.execute(&identity(), WAREHOUSE, &receive).await)?;
    require(
        replay["replayed"] == true && replay["result"] == manifest["receive_response"]["result"],
        "Original receiving request did not replay its exact receipt",
    )?;
    let cancellation_request: Request =
        serde_json::from_value(manifest["cancellation_request"].clone())?;
    let replay = application(
        store
            .execute(&identity(), WAREHOUSE, &cancellation_request)
            .await,
    )?;
    require(
        replay["replayed"] == true
            && replay["result"] == manifest["cancellation_response"]["result"],
        "Restored cancellation did not replay its exact receipt",
    )?;
    let cancelled = application(
        store
            .detail(
                TENANT,
                WAREHOUSE,
                "receipts",
                manifest["cancelled_receipt_id"]
                    .as_str()
                    .ok_or("Missing cancelled receipt")?,
            )
            .await,
    )?;
    require(
        cancelled["status"] == "cancelled"
            && cancelled["lines"][0]["received"] == 1
            && cancelled["cancellation"]["lines"]["closed-line"]["cancelled"] == 2,
        "Restored cancellation lost frozen received/remainder evidence",
    )?;
    let source_review = application(
        stateset_warehouse::returns::eligible(
            &store,
            &identity(),
            WAREHOUSE,
            manifest["conditional_frozen"]["order_id"]
                .as_str()
                .ok_or("Missing physical source order")?,
        )
        .await,
    )?;
    require(
        source_review["shipment_source"] == manifest["shipment_source"],
        "Original immutable shipment source was not restored exactly",
    )?;
    let return_receive: Request = serde_json::from_value(manifest["return_receive"].clone())?;
    let replay = application(store.execute(&identity(), WAREHOUSE, &return_receive).await)?;
    require(
        replay["replayed"] == true
            && replay["result"] == manifest["return_receive_response"]["result"],
        "Scoped customer-return receiving did not replay its exact committed receipt",
    )?;
    let returned = application(
        store
            .detail(
                TENANT,
                WAREHOUSE,
                "receipts",
                manifest["return_receipt_id"]
                    .as_str()
                    .ok_or("Missing return receipt")?,
            )
            .await,
    )?;
    require(
        returned["status"] == "received"
            && returned["lines"][0]["received"] == 1
            && returned["return_source"]["order_id"] == manifest["conditional_frozen"]["order_id"],
        "Physical customer-return source/quantity evidence was not restored",
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
    require(
        retained_lot_trace(&store).await? == manifest["lot_trace"],
        "Original request replays changed the nine retained lot trace sections or review",
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
    let history_recovery =
        history_restore::recover(&store, &manifest["operational_histories"], manifest_path).await?;
    let serial_recovery = serial_restore::recover(&store, &manifest["serials"]).await?;
    let unit_recovery = recover_unit_fixture(&store, &manifest["units"]).await?;
    let lot_recovery = recover_quality_fixture(&store, &manifest["lot_controls"]).await?;
    let loss_recovery = recover_transfer_loss(&store, &manifest["transfer_loss"]).await?;
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
    let target_session_timezone: String = sqlx::query_scalar("SHOW timezone")
        .fetch_one(&store.pool)
        .await?;
    require(manifest["source_session_timezone"] == "Pacific/Auckland" && target_session_timezone == "UTC", "Restore must preserve authority across the deliberately different source and target display timezones")?;
    let mut report = json!({"lot_trace_original_records_and_review_restored":true,"lot_trace_all_nine_sections_unchanged_after_original_replays":true,"lot_trace":manifest["lot_trace"],"source_session_timezone":manifest["source_session_timezone"],"target_session_timezone":target_session_timezone,"original_utc_authority_preserved_across_timezones":true,"lot_controls_required_expiry_holds_and_original_decisions_restored":true,"lot_original_requests_replayed_without_table_changes":true,"most_restrictive_transfer_arrival_policy_restored":true,"lot_post_restore_recovery":lot_recovery,"partial_order_remainder_cancellation_restored":true,"cancelled_demand_original_request_replayed":true,"actual_handoff_return_allowance_preserved_after_close":true,"partial_shipment_original_requests_restored":true,"active_packing_and_backorder_ownership_preserved":true,"early_partial_return_restored":true,"ok":true,"synthetic":true,"archive_round_trip":"pg_dump custom / pg_restore single transaction","all_table_hashes_identical_before_retry":true,"all_warehouse_reconciliation":restored["warehouses"],"table_evidence":restored["tables"],"migrations":restored["migrations"],"immutable_original_shipment_source_restored":true,"original_transfer_dispatch_and_partial_arrival_restored":true,"transfer_in_transit_custody_preserved":true,"receipt_cancellation_original_request_replayed":true,"cancelled_partial_receipt_restored":true,"facility_creation_original_request_replayed":true,"receive_original_request_replayed":true,"customer_return_receipt_restored":true,"scoped_return_receive_replayed":true,"native_import_original_request_replayed":true,"replays_changed_no_evidence":true,"rejected_conditional_oms_receipt_replayed_unchanged":true,"uncertain_native_frozen_retry_reclaimed":true,"uncertain_conditional_oms_retry_reclaimed":true,"native_external_http_attempted":false,"new_post_restore_command_reconciled":true,"post_restore_reconciliation":final_proof});
    let table_rows = restored["tables"]
        .as_object()
        .ok_or("Missing restored table counts")?;
    let empty_tables: Vec<_> = table_rows
        .iter()
        .filter_map(|(name, e)| (e["rows"].as_u64() == Some(0)).then_some(name.clone()))
        .collect();
    require(
        empty_tables == ["wms_serial_pending_checks"],
        "Every durable WMS table must be populated; only transaction-local serial validation keys must be empty",
    )?;
    report["operational_history_post_restore_recovery"] = history_recovery;
    report["recovery_point_table_count"] = json!(table_rows.len());
    report["recovery_point_nonempty_table_count"] = json!(table_rows.len() - empty_tables.len());
    report["recovery_point_empty_tables"] = json!(empty_tables);
    let pending: i64 = sqlx::query_scalar("SELECT count(*) FROM wms_serial_pending_checks")
        .fetch_one(&store.pool)
        .await?;
    require(
        pending == 0,
        "Post-restore commands left serial validation keys",
    )?;
    report["serial_pending_checks_empty_before_and_after_restore_and_continuation"] = json!(true);
    report["nonempty_serial_tables_restored"] = json!([
        "wms_serial_modes",
        "wms_serial_heads",
        "wms_serial_batches",
        "wms_serial_units",
        "wms_serial_steps",
        "wms_serial_observations"
    ]
    .iter()
    .all(|name| table_rows[*name]["rows"].as_u64().is_some_and(|n| n > 0)));
    report["serial_original_identity_history_observations_and_global_heads_restored"] = json!(true);
    report["serial_original_replays_changed_no_tables"] = json!(true);
    report["serials"] = manifest["serials"].clone();
    report["serial_post_restore_recovery"] = serial_recovery;
    report["unit_original_definitions_and_observations_restored"] = json!(true);
    report["unit_original_replays_changed_no_tables"] = json!(true);
    report["units"] = manifest["units"].clone();
    report["unit_post_restore_recovery"] = unit_recovery;
    report["transfer_loss_original_records_and_capped_reversal_restored"] = json!(true);
    report["transfer_loss_original_replays_changed_no_evidence"] = json!(true);
    report["transfer_loss"] = manifest["transfer_loss"].clone();
    report["transfer_loss_post_restore_recovery"] = loss_recovery;
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
