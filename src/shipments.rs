//! Immutable original handoff facts shared by returns and conditional writeback.
use crate::{
    domain::Order,
    store::{Error, Result},
};
use serde_json::{json, Value};
use sqlx::{PgConnection, Row};

pub(crate) async fn initialize(c: &mut PgConnection, tenant: &str, warehouse: &str) -> Result<()> {
    sqlx::query("SELECT wms_seed_shipment_evidence($1,$2)")
        .bind(tenant)
        .bind(warehouse)
        .execute(c)
        .await?;
    Ok(())
}

pub(crate) async fn source(
    c: &mut PgConnection,
    tenant: &str,
    warehouse: &str,
    order_id: &str,
) -> Result<(Order, Value)> {
    let row = sqlx::query("SELECT o.status,o.data->>'shipment_mode' AS shipment_mode,COALESCE(e.document,wms_current_order(o.tenant,o.warehouse_id,o.id)) AS document,e.source_kind,e.event_id,e.source_version,wms_shipment_consistent(o.tenant,o.warehouse_id,o.id) AS ok
        FROM wms_orders o
        LEFT JOIN wms_shipment_evidence e ON e.tenant=o.tenant AND e.warehouse_id=o.warehouse_id AND e.order_id=o.id
        WHERE o.tenant=$1 AND o.warehouse_id=$2 AND o.id=$3")
        .bind(tenant).bind(warehouse).bind(order_id).fetch_optional(&mut *c).await?
        .ok_or_else(|| Error::new(404,"Warehouse order not found"))?;
    if !["shipped", "partially_shipped", "closed_partial"]
        .contains(&row.try_get::<String, _>("status")?.as_str())
    {
        return Err(Error::new(
            422,
            "An original completed shipment source is required",
        ));
    }
    if !row.try_get::<bool, _>("ok")? {
        return Err(Error::new(
            409,
            "Original shipment evidence disagrees with warehouse facts; writes held",
        ));
    }
    let order = serde_json::from_value(row.try_get("document")?)
        .map_err(|_| Error::new(503, "Original shipment evidence requires repair"))?;
    if row
        .try_get::<Option<String>, _>("shipment_mode")?
        .as_deref()
        == Some("packages")
    {
        let provenance:Value=sqlx::query_scalar("SELECT jsonb_build_object('kind',CASE WHEN count(*)=1 AND $4::boolean THEN 'event' ELSE 'handoff_events' END,'event_id',CASE WHEN count(*)=1 AND $4::boolean THEN min(event_id) ELSE NULL END,'event_ids',jsonb_agg(event_id
            ORDER BY version),'version',max(version))
            FROM wms_package_closures
            WHERE tenant=$1 AND warehouse_id=$2 AND order_id=$3 AND kind='handed_off'")
            .bind(tenant).bind(warehouse).bind(order_id).bind(row.try_get::<String,_>("status")?=="shipped").fetch_one(c).await?;
        return Ok((order, provenance));
    }
    Ok((
        order,
        json!({"kind":row.try_get::<String,_>("source_kind")?,"event_id":row.try_get::<Option<String>,_>("event_id")?,"version":row.try_get::<i64,_>("source_version")?}),
    ))
}

pub(crate) async fn proof(
    c: &mut PgConnection,
    tenant: &str,
    warehouse: &str,
) -> Result<Vec<Value>> {
    let rows = sqlx::query("WITH owners AS (SELECT id AS order_id FROM wms_orders WHERE tenant=$1 AND warehouse_id=$2 AND status IN('shipped','partially_shipped','closed_partial') UNION SELECT order_id FROM wms_shipment_evidence WHERE tenant=$1 AND warehouse_id=$2 UNION SELECT payload#>>'{order,id}' FROM wms_events WHERE tenant=$1 AND warehouse_id=$2 AND payload->>'type'='fulfillment.shipped') SELECT jsonb_build_object('order_id',o.order_id,'source_kind',COALESCE(e.source_kind,CASE WHEN EXISTS(SELECT 1 FROM wms_split_orders b WHERE b.tenant=$1 AND b.warehouse_id=$2 AND b.order_id=o.order_id) THEN 'handoff_events' END),'event_id',e.event_id,'source_version',e.source_version,'ok',wms_shipment_consistent($1,$2,o.order_id)) AS proof FROM owners o LEFT JOIN wms_shipment_evidence e ON e.tenant=$1 AND e.warehouse_id=$2 AND e.order_id=o.order_id ORDER BY o.order_id")
        .bind(tenant).bind(warehouse).fetch_all(c).await?;
    rows.into_iter().map(|r| Ok(r.try_get("proof")?)).collect()
}
