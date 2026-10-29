//! Loading canonical warehouse state and its child records inside a transaction.
use super::*;

impl Store {
    pub(crate) async fn order_children(
        c: &mut PgConnection,
        t: &str,
        w: &str,
        orders: &mut [domain::Order],
    ) -> Result<()> {
        crate::order_cancellation::load(c, t, w, orders).await?;
        let ids: Vec<_> = orders.iter().map(|o| o.id.clone()).collect();
        let lines = sqlx::query(
            "SELECT order_id,id,sku,quantity
            FROM wms_order_lines
            WHERE tenant=$1 AND warehouse_id=$2 AND order_id=ANY($3)
            ORDER BY order_id,id",
        )
        .bind(t)
        .bind(w)
        .bind(&ids)
        .fetch_all(&mut *c)
        .await?;
        let allocations = sqlx::query(
            "SELECT order_id,line_id,sku,lot,location,quantity,picked,shipped,released
            FROM wms_allocations
            WHERE tenant=$1 AND warehouse_id=$2 AND order_id=ANY($3)
            ORDER BY order_id,line_id,location,lot",
        )
        .bind(t)
        .bind(w)
        .bind(&ids)
        .fetch_all(&mut *c)
        .await?;
        let mut lines_by_order: HashMap<String, Vec<OrderLine>> = HashMap::new();
        for r in lines {
            lines_by_order
                .entry(r.try_get("order_id")?)
                .or_default()
                .push(OrderLine {
                    id: r.try_get("id")?,
                    sku: r.try_get("sku")?,
                    quantity: r.try_get("quantity")?,
                });
        }
        let mut allocations_by_order: HashMap<String, Vec<Allocation>> = HashMap::new();
        for r in allocations {
            allocations_by_order
                .entry(r.try_get("order_id")?)
                .or_default()
                .push(Allocation {
                    line_id: r.try_get("line_id")?,
                    sku: r.try_get("sku")?,
                    lot: r.try_get("lot")?,
                    location: r.try_get("location")?,
                    quantity: r.try_get("quantity")?,
                    picked: r.try_get("picked")?,
                    shipped: r.try_get("shipped")?,
                    released: r.try_get("released")?,
                });
        }
        for o in orders {
            // Preserve canonical child ordering from SQL while decoding/joining
            // each row once, independent of the number of active orders.
            o.lines = lines_by_order.remove(&o.id).unwrap_or_default();
            o.allocations = allocations_by_order.remove(&o.id).unwrap_or_default();
        }
        Ok(())
    }
    pub(crate) async fn receipt_children(
        c: &mut PgConnection,
        t: &str,
        w: &str,
        receipts: &mut [domain::Receipt],
    ) -> Result<()> {
        let ids: Vec<_> = receipts.iter().map(|r| r.id.clone()).collect();
        let lines = sqlx::query(
            "SELECT receipt_id,id,sku,lot,quantity,received
            FROM wms_receipt_lines
            WHERE tenant=$1 AND warehouse_id=$2 AND receipt_id=ANY($3)
            ORDER BY receipt_id,id",
        )
        .bind(t)
        .bind(w)
        .bind(&ids)
        .fetch_all(&mut *c)
        .await?;
        let mut lines_by_receipt: HashMap<String, Vec<ReceiptLine>> = HashMap::new();
        for l in lines {
            lines_by_receipt
                .entry(l.try_get("receipt_id")?)
                .or_default()
                .push(ReceiptLine {
                    id: l.try_get("id")?,
                    sku: l.try_get("sku")?,
                    lot: l.try_get("lot")?,
                    quantity: l.try_get("quantity")?,
                    received: l.try_get("received")?,
                });
        }
        let returns = sqlx::query(
            "SELECT receipt_id,line_id,order_id,order_line_id,reason
            FROM wms_return_lines
            WHERE tenant=$1 AND warehouse_id=$2 AND receipt_id=ANY($3)
            ORDER BY receipt_id,line_id",
        )
        .bind(t)
        .bind(w)
        .bind(&ids)
        .fetch_all(&mut *c)
        .await?;
        let mut sources: HashMap<String, crate::returns::ReturnSource> = HashMap::new();
        for row in returns {
            let receipt_id: String = row.try_get("receipt_id")?;
            let source = sources
                .entry(receipt_id)
                .or_insert(crate::returns::ReturnSource {
                    order_id: row.try_get("order_id")?,
                    reason: row.try_get("reason")?,
                    line_ids: Default::default(),
                });
            source
                .line_ids
                .insert(row.try_get("line_id")?, row.try_get("order_line_id")?);
        }
        crate::receipts::load(c, t, w, receipts).await?;
        for r in receipts {
            r.lines = lines_by_receipt.remove(&r.id).unwrap_or_default();
            r.return_source = sources.remove(&r.id);
        }
        Ok(())
    }
    pub(super) async fn canonical_records(
        c: &mut PgConnection,
        t: &str,
        w: &str,
        kind: &str,
        records: Vec<Value>,
    ) -> Result<Vec<Value>> {
        if records.is_empty() {
            return Ok(records);
        }
        let headers = records
            .into_iter()
            .map(|mut value| {
                let object = value
                    .as_object_mut()
                    .ok_or_else(|| Error::new(503, "Stored warehouse header requires repair"))?;
                object.insert("lines".into(), json!([]));
                if kind == "orders" {
                    object.insert("allocations".into(), json!([]));
                } else if kind == "receipts" {
                    object.remove("return_source");
                    object.remove("cancellation");
                }
                Ok(value)
            })
            .collect::<Result<Vec<_>>>()?;
        match kind {
            "orders" => {
                let mut orders = headers
                    .into_iter()
                    .map(decode)
                    .collect::<Result<Vec<domain::Order>>>()?;
                Self::order_children(c, t, w, &mut orders).await?;
                Ok(orders.into_iter().map(|order| json!(order)).collect())
            }
            "receipts" => {
                let mut receipts = headers
                    .into_iter()
                    .map(decode)
                    .collect::<Result<Vec<domain::Receipt>>>()?;
                Self::receipt_children(c, t, w, &mut receipts).await?;
                Ok(receipts.into_iter().map(|receipt| json!(receipt)).collect())
            }
            _ => Err(Error::new(404, "Unknown warehouse record")),
        }
    }
    pub(crate) async fn load(
        c: &mut PgConnection,
        t: &str,
        w: &str,
        command: Option<&Command>,
        display: bool,
    ) -> Result<State> {
        let h = sqlx::query(
            "SELECT metadata,version FROM wms_warehouses WHERE tenant=$1 AND warehouse_id=$2",
        )
        .bind(t)
        .bind(w)
        .fetch_optional(&mut *c)
        .await?
        .ok_or_else(|| Error::new(404, "Warehouse not found"))?;
        let mut s = State::empty(decode(h.try_get("metadata")?)?);
        s.version = h.try_get("version")?;
        let (oid, rid, eid) = target(command);
        s.products = sqlx::query(
            "SELECT data FROM wms_products WHERE tenant=$1 AND warehouse_id=$2 ORDER BY sku",
        )
        .bind(t)
        .bind(w)
        .fetch_all(&mut *c)
        .await?
        .into_iter()
        .map(|r| decode(r.get("data")))
        .collect::<Result<_>>()?;
        crate::lot_store::load(c, t, w, &mut s).await?;
        crate::units::load(c, t, w, &mut s).await?;
        s.locations = sqlx::query(
            "SELECT data FROM wms_locations WHERE tenant=$1 AND warehouse_id=$2 ORDER BY code",
        )
        .bind(t)
        .bind(w)
        .fetch_all(&mut *c)
        .await?
        .into_iter()
        .map(|r| decode(r.get("data")))
        .collect::<Result<_>>()?;
        s.stocks = sqlx::query(
            "SELECT sku,lot,location,on_hand,reserved
            FROM wms_stock
            WHERE tenant=$1 AND warehouse_id=$2
            ORDER BY sku,lot,location",
        )
        .bind(t)
        .bind(w)
        .fetch_all(&mut *c)
        .await?
        .iter()
        .map(|r| {
            Ok(Stock {
                sku: r.try_get("sku")?,
                lot: r.try_get("lot")?,
                location: r.try_get("location")?,
                on_hand: r.try_get("on_hand")?,
                reserved: r.try_get("reserved")?,
            })
        })
        .collect::<Result<_>>()?;
        s.inventory_baseline = sqlx::query(
            "SELECT sku,lot,location,quantity
            FROM wms_stock_ledger
            WHERE tenant=$1 AND warehouse_id=$2
            ORDER BY sku,lot,location",
        )
        .bind(t)
        .bind(w)
        .fetch_all(&mut *c)
        .await?
        .iter()
        .map(|r| {
            Ok(Stock {
                sku: r.try_get("sku")?,
                lot: r.try_get("lot")?,
                location: r.try_get("location")?,
                on_hand: r.try_get("quantity")?,
                reserved: 0,
            })
        })
        .collect::<Result<_>>()?;
        if display {
            s.movements = sqlx::query(
                "SELECT data
                FROM wms_movements
                WHERE tenant=$1 AND warehouse_id=$2
                ORDER BY seq DESC
                LIMIT 100",
            )
            .bind(t)
            .bind(w)
            .fetch_all(&mut *c)
            .await?
            .into_iter()
            .map(|r| decode(r.get("data")))
            .collect::<Result<_>>()?;
            s.movements.reverse();
            s.activity = sqlx::query(
                "SELECT data
                FROM wms_activity
                WHERE tenant=$1 AND warehouse_id=$2
                ORDER BY seq DESC
                LIMIT 100",
            )
            .bind(t)
            .bind(w)
            .fetch_all(&mut *c)
            .await?
            .into_iter()
            .map(|r| decode(r.get("data")))
            .collect::<Result<_>>()?;
            s.activity.reverse();
        }
        s.orders=sqlx::query("SELECT data || '{\"lines\":[],\"allocations\":[]}'::jsonb AS data FROM wms_orders WHERE tenant=$1 AND warehouse_id=$2 AND (status NOT IN('shipped','cancelled','closed_partial') OR id=$3 OR ($4 AND seq IN(SELECT seq FROM wms_orders WHERE tenant=$1 AND warehouse_id=$2 AND status IN('shipped','cancelled','closed_partial') ORDER BY seq DESC LIMIT 20))) ORDER BY seq").bind(t).bind(w).bind(oid).bind(display).fetch_all(&mut *c).await?.into_iter().map(|r|decode(r.get("data"))).collect::<Result<_>>()?;
        Self::order_children(c, t, w, &mut s.orders).await?;
        let movement_receipts: Vec<_> = s
            .movements
            .iter()
            .filter_map(|m| m.receipt_id.clone())
            .collect();
        s.receipts=sqlx::query("SELECT data || '{\"lines\":[],\"return_source\":null,\"cancellation\":null}'::jsonb AS data FROM wms_receipts WHERE tenant=$1 AND warehouse_id=$2 AND (status IN('expected','partial') OR id=$3 OR id=ANY($5) OR ($4 AND seq IN(SELECT seq FROM wms_receipts WHERE tenant=$1 AND warehouse_id=$2 AND status IN('received','cancelled') ORDER BY seq DESC LIMIT 20))) ORDER BY seq").bind(t).bind(w).bind(rid).bind(display).bind(movement_receipts).fetch_all(&mut *c).await?.into_iter().map(|r|decode(r.get("data"))).collect::<Result<_>>()?;
        Self::receipt_children(c, t, w, &mut s.receipts).await?;
        let ids: Vec<_> = s.receipts.iter().map(|r| r.id.clone()).collect();
        for r in &s.receipts {
            for l in &r.lines {
                s.receipt_baseline
                    .entry(r.id.clone())
                    .or_default()
                    .insert(l.id.clone(), 0);
            }
        }
        let proofs = sqlx::query(
            "SELECT receipt_id,line_id,quantity
            FROM wms_receipt_evidence
            WHERE tenant=$1 AND warehouse_id=$2 AND receipt_id=ANY($3)",
        )
        .bind(t)
        .bind(w)
        .bind(&ids)
        .fetch_all(&mut *c)
        .await?;
        for p in proofs {
            s.receipt_baseline
                .entry(p.get("receipt_id"))
                .or_default()
                .insert(p.get("line_id"), p.get("quantity"));
        }
        s.exceptions = sqlx::query(
            "SELECT data
            FROM wms_exceptions
            WHERE tenant=$1 AND warehouse_id=$2 AND (status='open' OR id=$3 OR ($4 AND seq IN(
            SELECT seq
            FROM wms_exceptions
            WHERE tenant=$1 AND warehouse_id=$2 AND status='resolved'
            ORDER BY seq DESC
            LIMIT 20)))
            ORDER BY seq",
        )
        .bind(t)
        .bind(w)
        .bind(eid)
        .bind(display)
        .fetch_all(&mut *c)
        .await?
        .into_iter()
        .map(|r| decode(r.get("data")))
        .collect::<Result<_>>()?;
        for m in &s.movements {
            for (location, delta) in [
                (m.from.as_deref(), m.quantity),
                (m.to.as_deref(), -m.quantity),
            ] {
                if let Some(location) = location {
                    if let Some(b) = s
                        .inventory_baseline
                        .iter_mut()
                        .find(|b| b.sku == m.sku && b.lot == m.lot && b.location == location)
                    {
                        b.on_hand += delta;
                    } else {
                        s.inventory_baseline.push(Stock {
                            sku: m.sku.clone(),
                            lot: m.lot.clone(),
                            location: location.into(),
                            on_hand: delta,
                            reserved: 0,
                        });
                    }
                }
            }
            if let (Some(rid), Some(lid)) = (&m.receipt_id, &m.receipt_line_id) {
                *s.receipt_baseline
                    .entry(rid.clone())
                    .or_default()
                    .entry(lid.clone())
                    .or_default() -= m.quantity;
            }
        }
        let mut keys = crate::concurrency::visible_keys(&s);
        if let Some(command) = command {
            if let Ok(Some(versions)) = crate::concurrency::reviewed_versions(&s, command) {
                keys.extend(versions.into_keys());
            }
        }
        s.resource_versions = keys.iter().map(|key| (key.clone(), 0)).collect();
        let versions = sqlx::query("SELECT resource,revision FROM wms_resource_versions WHERE tenant=$1 AND warehouse_id=$2 AND resource=ANY($3)")
            .bind(t).bind(w).bind(&keys).fetch_all(&mut *c).await?;
        for row in versions {
            s.resource_versions
                .insert(row.try_get("resource")?, row.try_get("revision")?);
        }
        Ok(s)
    }
}
