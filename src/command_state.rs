//! Selective physical-command snapshots. The warehouse lock is already held.
//! Only the command's SKU and owner context is loaded; other owners' holds are
//! independently aggregated from normalized allocations, never reservation caches.
use crate::{
    concurrency,
    domain::{Command, State, Stock},
    persistence::{decode, Store},
    store::Result,
};
use sqlx::{PgConnection, Row};
use std::collections::BTreeSet;

pub(crate) async fn load(
    c: &mut PgConnection,
    tenant: &str,
    warehouse: &str,
    command: &Command,
) -> Result<Option<State>> {
    let mut skus = BTreeSet::new();
    let mut locations = BTreeSet::from(["STAGE-01".to_string()]);
    let (order_id, receipt_id) = match command {
        Command::MoveStock { sku, from, to, .. } => {
            skus.insert(sku.clone());
            locations.extend([from.clone(), to.clone()]);
            (None, None)
        }
        Command::CountStock { sku, location, .. } => {
            skus.insert(sku.clone());
            locations.insert(location.clone());
            (None, None)
        }
        Command::Receive {
            receipt_id,
            location,
            ..
        } => {
            locations.insert(location.clone());
            (None, Some(receipt_id.as_str()))
        }
        Command::Allocate { order_id }
        | Command::Pack { order_id }
        | Command::Ship { order_id, .. }
        | Command::CancelOrder { order_id, .. }
        | Command::ResetAllocation { order_id, .. }
        | Command::Pick { order_id, .. }
        | Command::Unpick { order_id, .. } => (Some(order_id.as_str()), None),
        _ => return Ok(None),
    };
    let head = sqlx::query(
        "SELECT metadata,version FROM wms_warehouses WHERE tenant=$1 AND warehouse_id=$2",
    )
    .bind(tenant)
    .bind(warehouse)
    .fetch_one(&mut *c)
    .await?;
    let mut state = State::empty(decode(head.try_get("metadata")?)?);
    state.version = head.try_get("version")?;
    if let Some(order_id) = order_id {
        let rows = sqlx::query("SELECT data || '{\"lines\":[],\"allocations\":[]}'::jsonb AS data FROM wms_orders WHERE tenant=$1 AND warehouse_id=$2 AND id=$3")
            .bind(tenant).bind(warehouse).bind(order_id).fetch_all(&mut *c).await?;
        state.orders = rows
            .into_iter()
            .map(|r| decode(r.get("data")))
            .collect::<Result<_>>()?;
        Store::order_children(c, tenant, warehouse, &mut state.orders).await?;
        for order in &state.orders {
            skus.extend(order.lines.iter().map(|line| line.sku.clone()));
            locations.extend(order.allocations.iter().map(|a| a.location.clone()));
        }
        state.exceptions = sqlx::query("SELECT data FROM wms_exceptions WHERE tenant=$1 AND warehouse_id=$2 AND order_id=$3 AND status='open' ORDER BY seq")
            .bind(tenant).bind(warehouse).bind(order_id).fetch_all(&mut *c).await?
            .into_iter().map(|r| decode(r.get("data"))).collect::<Result<_>>()?;
    }
    if let Some(receipt_id) = receipt_id {
        let rows = sqlx::query("SELECT data || '{\"lines\":[],\"return_source\":null,\"cancellation\":null}'::jsonb AS data FROM wms_receipts WHERE tenant=$1 AND warehouse_id=$2 AND id=$3")
            .bind(tenant).bind(warehouse).bind(receipt_id).fetch_all(&mut *c).await?;
        state.receipts = rows
            .into_iter()
            .map(|r| decode(r.get("data")))
            .collect::<Result<_>>()?;
        Store::receipt_children(c, tenant, warehouse, &mut state.receipts).await?;
        for receipt in &state.receipts {
            skus.extend(receipt.lines.iter().map(|line| line.sku.clone()));
            for line in &receipt.lines {
                state
                    .receipt_baseline
                    .entry(receipt.id.clone())
                    .or_default()
                    .insert(line.id.clone(), 0);
            }
        }
        let rows = sqlx::query("SELECT receipt_id,line_id,quantity FROM wms_receipt_evidence WHERE tenant=$1 AND warehouse_id=$2 AND receipt_id=$3")
            .bind(tenant).bind(warehouse).bind(receipt_id).fetch_all(&mut *c).await?;
        for row in rows {
            state
                .receipt_baseline
                .entry(row.try_get("receipt_id")?)
                .or_default()
                .insert(row.try_get("line_id")?, row.try_get("quantity")?);
        }
    }
    let skus: Vec<_> = skus.into_iter().collect();
    state.products = sqlx::query("SELECT data FROM wms_products WHERE tenant=$1 AND warehouse_id=$2 AND sku=ANY($3) ORDER BY sku")
        .bind(tenant).bind(warehouse).bind(&skus).fetch_all(&mut *c).await?
        .into_iter().map(|r| decode(r.get("data"))).collect::<Result<_>>()?;
    crate::lot_store::load(c, tenant, warehouse, &mut state).await?;
    crate::units::load(c, tenant, warehouse, &mut state).await?;
    state.stocks = sqlx::query("SELECT sku,lot,location,on_hand,reserved FROM wms_stock WHERE tenant=$1 AND warehouse_id=$2 AND sku=ANY($3) ORDER BY sku,lot,location")
        .bind(tenant).bind(warehouse).bind(&skus).fetch_all(&mut *c).await?
        .into_iter().map(|r| Ok(Stock { sku:r.try_get("sku")?,lot:r.try_get("lot")?,location:r.try_get("location")?,on_hand:r.try_get("on_hand")?,reserved:r.try_get("reserved")? })).collect::<Result<_>>()?;
    state.inventory_baseline = sqlx::query("SELECT sku,lot,location,quantity FROM wms_stock_ledger WHERE tenant=$1 AND warehouse_id=$2 AND sku=ANY($3) ORDER BY sku,lot,location")
        .bind(tenant).bind(warehouse).bind(&skus).fetch_all(&mut *c).await?
        .into_iter().map(|r| Ok(Stock { sku:r.try_get("sku")?,lot:r.try_get("lot")?,location:r.try_get("location")?,on_hand:r.try_get("quantity")?,reserved:0 })).collect::<Result<_>>()?;
    let rows = sqlx::query(
        "WITH holds AS (
        SELECT sku,lot,location,quantity-picked-released AS held
        FROM wms_allocations
        WHERE tenant=$1 AND warehouse_id=$2 AND sku=ANY($3) AND ($4::text IS NULL OR order_id<>$4)
        UNION ALL
        SELECT sku,lot,'STAGE-01',picked-shipped
        FROM wms_allocations
        WHERE tenant=$1 AND warehouse_id=$2 AND sku=ANY($3) AND ($4::text IS NULL OR order_id<>$4))
        SELECT sku,lot,location,sum(held)::bigint AS held
        FROM holds
        GROUP BY sku,lot,location
        HAVING sum(held)<>0
        ORDER BY sku,lot,location",
    )
    .bind(tenant)
    .bind(warehouse)
    .bind(&skus)
    .bind(order_id)
    .fetch_all(&mut *c)
    .await?;
    state.reservation_baseline = rows
        .into_iter()
        .map(|r| {
            Ok(Stock {
                sku: r.try_get("sku")?,
                lot: r.try_get("lot")?,
                location: r.try_get("location")?,
                on_hand: 0,
                reserved: r.try_get("held")?,
            })
        })
        .collect::<Result<_>>()?;
    for stock in state
        .stocks
        .iter()
        .chain(&state.inventory_baseline)
        .chain(&state.reservation_baseline)
    {
        locations.insert(stock.location.clone());
    }
    if let Command::Pick { location, .. } | Command::Unpick { location, .. } = command {
        locations.insert(location.clone());
    }
    let locations: Vec<_> = locations.into_iter().collect();
    state.locations = sqlx::query("SELECT data FROM wms_locations WHERE tenant=$1 AND warehouse_id=$2 AND code=ANY($3) ORDER BY code")
        .bind(tenant).bind(warehouse).bind(&locations).fetch_all(&mut *c).await?
        .into_iter().map(|r| decode(r.get("data"))).collect::<Result<_>>()?;
    // Required keys include prospective destination buckets that do not exist yet.
    // Invalid work is left to the normal transition/input guards below.
    if let Ok(Some(mut versions)) = concurrency::reviewed_versions(&state, command) {
        let keys: Vec<_> = versions.keys().cloned().collect();
        let rows = sqlx::query("SELECT resource,revision FROM wms_resource_versions WHERE tenant=$1 AND warehouse_id=$2 AND resource=ANY($3)")
            .bind(tenant).bind(warehouse).bind(&keys).fetch_all(&mut *c).await?;
        for row in rows {
            versions.insert(row.try_get("resource")?, row.try_get("revision")?);
        }
        state.resource_versions = versions;
    }
    Ok(Some(state))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        domain::{self, LineInput},
        store::{Identity, Request},
    };

    #[tokio::test]
    #[ignore = "requires isolated PostgreSQL; scripts/local-postgres.sh"]
    async fn selective_snapshot_keeps_foreign_holds_without_loading_unrelated_catalog_or_orders() {
        let url = std::env::var("WMS_TEST_DATABASE_URL").expect("isolated test database");
        assert!(reqwest::Url::parse(&url)
            .expect("URL")
            .path()
            .contains("test"));
        let store = Store::connect(&url).await.expect("database");
        let identity = Identity {
            tenant: format!("selective-{}", domain::id()),
            actor: "selective:admin".into(),
            role: "admin".into(),
        };
        store
            .provision(&identity.tenant, false)
            .await
            .expect("warehouse");
        async fn apply(store: &Store, identity: &Identity, command: Command) -> String {
            let state = store
                .state(&identity.tenant, "wh-main")
                .await
                .expect("state");
            let response = store
                .execute(
                    identity,
                    "wh-main",
                    &Request {
                        serial_inputs: Vec::new(),
                        unit_inputs: Vec::new(),
                        request_id: domain::id(),
                        expected_version: state.version,
                        resource_versions: None,
                        command,
                    },
                )
                .await
                .expect("command");
            response["result"]["entity_id"].as_str().expect("id").into()
        }
        for sku in ["SELECTED", "UNRELATED"] {
            apply(
                &store,
                &identity,
                Command::CreateProduct {
                    serial_controlled: false,
                    sku: sku.into(),
                    name: sku.into(),
                    category: "Test".into(),
                    reorder_point: 0,
                },
            )
            .await;
        }
        apply(
            &store,
            &identity,
            Command::CreateLocation {
                code: "A".into(),
                zone: "Storage".into(),
                kind: "storage".into(),
            },
        )
        .await;
        for sku in ["SELECTED", "UNRELATED"] {
            apply(
                &store,
                &identity,
                Command::CountStock {
                    sku: sku.into(),
                    lot: "LOT".into(),
                    location: "A".into(),
                    counted: 20,
                    reason: "Synthetic count".into(),
                },
            )
            .await;
        }
        let mut ids = vec![];
        for reference in ["OTHER", "TARGET"] {
            let id = apply(
                &store,
                &identity,
                Command::CreateOrder {
                    reference: reference.into(),
                    source_order_id: None,
                    customer: "Customer".into(),
                    destination: "Destination".into(),
                    priority: "normal".into(),
                    lines: vec![LineInput {
                        id: "line".into(),
                        sku: "SELECTED".into(),
                        quantity: 3,
                    }],
                },
            )
            .await;
            apply(
                &store,
                &identity,
                Command::Allocate {
                    order_id: id.clone(),
                },
            )
            .await;
            ids.push(id);
        }
        apply(
            &store,
            &identity,
            Command::Pick {
                order_id: ids[0].clone(),
                line_id: "line".into(),
                location: "A".into(),
                lot: "LOT".into(),
                quantity: 1,
            },
        )
        .await;
        // A malformed unrelated display header must not be read into this
        // command's execution context. Its normalized stock remains untouched.
        sqlx::query("UPDATE wms_products SET data=jsonb_set(data,'{name}','false'::jsonb) WHERE tenant=$1 AND sku='UNRELATED'")
            .bind(&identity.tenant).execute(&store.pool).await.expect("unrelated header fixture");
        let command = Command::Pick {
            order_id: ids[1].clone(),
            line_id: "line".into(),
            location: "A".into(),
            lot: "LOT".into(),
            quantity: 1,
        };
        let mut tx = store.pool.begin().await.expect("transaction");
        sqlx::query("SELECT version FROM wms_warehouses WHERE tenant=$1 AND warehouse_id='wh-main' FOR UPDATE")
            .bind(&identity.tenant).fetch_one(&mut *tx).await.expect("writer lock");
        let snapshot = load(&mut tx, &identity.tenant, "wh-main", &command)
            .await
            .expect("snapshot")
            .expect("selective");
        assert_eq!(snapshot.products.len(), 1);
        assert_eq!(snapshot.orders.len(), 1);
        assert!(snapshot.receipts.is_empty());
        assert!(snapshot.stocks.iter().all(|s| s.sku == "SELECTED"));
        assert_eq!(
            snapshot
                .reservation_baseline
                .iter()
                .find(|s| s.location == "A")
                .expect("foreign source hold")
                .reserved,
            2
        );
        assert_eq!(
            snapshot
                .reservation_baseline
                .iter()
                .find(|s| s.location == "STAGE-01")
                .expect("foreign staged hold")
                .reserved,
            1
        );
        assert!(snapshot.validate().is_ok());
        let mut missing = snapshot.clone();
        missing.reservation_baseline.clear();
        assert!(
            missing.validate().is_err(),
            "foreign holds are required for reconciliation"
        );
        let payload = serde_json::to_value(&snapshot).expect("JSON");
        assert!(payload.get("reservation_baseline").is_none());
        let mut forged = payload;
        forged["reservation_baseline"] = serde_json::json!(snapshot.reservation_baseline);
        let decoded: State = serde_json::from_value(forged).expect("snapshot decode");
        assert!(
            decoded.reservation_baseline.is_empty(),
            "reservation authority is never accepted from JSON"
        );
        tx.rollback().await.expect("release lock");
    }
}
