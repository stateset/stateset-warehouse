//! Writing only the changed rows of a warehouse state, with its activity.
use super::*;

impl Store {
    pub(super) async fn save(
        c: &mut PgConnection,
        t: &str,
        old: Option<&State>,
        s: &State,
    ) -> Result<()> {
        let w = &s.warehouse.id;
        // Build identity indexes once. Typed equality avoids serializing every
        // unchanged operational row just to discover that nothing changed.
        let previous_products: HashMap<_, _> = old
            .into_iter()
            .flat_map(|s| s.products.iter())
            .map(|p| (p.sku.as_str(), p))
            .collect();
        let previous_locations: HashMap<_, _> = old
            .into_iter()
            .flat_map(|s| s.locations.iter())
            .map(|l| (l.code.as_str(), l))
            .collect();
        let previous_stocks: HashMap<_, _> = old
            .into_iter()
            .flat_map(|s| s.stocks.iter())
            .map(|b| (stock_key(b), b))
            .collect();
        let previous_receipts: HashMap<_, _> = old
            .into_iter()
            .flat_map(|s| s.receipts.iter())
            .map(|r| (r.id.as_str(), r))
            .collect();
        let previous_orders: HashMap<_, _> = old
            .into_iter()
            .flat_map(|s| s.orders.iter())
            .map(|o| (o.id.as_str(), o))
            .collect();
        let previous_exceptions: HashMap<_, _> = old
            .into_iter()
            .flat_map(|s| s.exceptions.iter())
            .map(|e| (e.id.as_str(), e))
            .collect();
        let previous_movements: HashSet<_> = old
            .into_iter()
            .flat_map(|s| s.movements.iter())
            .map(|m| m.id.as_str())
            .collect();
        let previous_activity: HashSet<_> = old
            .into_iter()
            .flat_map(|s| s.activity.iter())
            .map(|a| a.id.as_str())
            .collect();
        sqlx::query(
            "INSERT INTO wms_warehouses(tenant,warehouse_id,version,metadata)
            VALUES($1,$2,$3,$4)
            ON CONFLICT(tenant,warehouse_id) DO
            UPDATE
            SET version=EXCLUDED.version,metadata=EXCLUDED.metadata",
        )
        .bind(t)
        .bind(w)
        .bind(s.version)
        .bind(json!(s.warehouse))
        .execute(&mut *c)
        .await?;
        for p in &s.products {
            if changed(p, previous_products.get(p.sku.as_str()).copied()) {
                sqlx::query(
                    "INSERT INTO wms_products
                    VALUES($1,$2,$3,$4)
                    ON CONFLICT(tenant,warehouse_id,sku) DO
                    UPDATE
                    SET data=EXCLUDED.data",
                )
                .bind(t)
                .bind(w)
                .bind(&p.sku)
                .bind(json!(p))
                .execute(&mut *c)
                .await?;
            }
        }
        for l in &s.locations {
            if changed(l, previous_locations.get(l.code.as_str()).copied()) {
                sqlx::query(
                    "INSERT INTO wms_locations
                    VALUES($1,$2,$3,$4,$5)
                    ON CONFLICT(tenant,warehouse_id,code) DO
                    UPDATE
                    SET kind=EXCLUDED.kind,data=EXCLUDED.data",
                )
                .bind(t)
                .bind(w)
                .bind(&l.code)
                .bind(&l.kind)
                .bind(json!(l))
                .execute(&mut *c)
                .await?;
            }
        }
        for b in &s.stocks {
            if changed(b, previous_stocks.get(&stock_key(b)).copied()) {
                sqlx::query(
                    "INSERT INTO wms_stock
                    VALUES($1,$2,$3,$4,$5,$6,$7)
                    ON CONFLICT(tenant,warehouse_id,sku,lot,location) DO
                    UPDATE
                    SET on_hand=EXCLUDED.on_hand,reserved=EXCLUDED.reserved",
                )
                .bind(t)
                .bind(w)
                .bind(&b.sku)
                .bind(&b.lot)
                .bind(&b.location)
                .bind(b.on_hand)
                .bind(b.reserved)
                .execute(&mut *c)
                .await?;
            }
        }
        for r in &s.receipts {
            if changed(r, previous_receipts.get(r.id.as_str()).copied()) {
                sqlx::query(
                    "INSERT INTO wms_receipts(tenant,warehouse_id,id,reference,status,data)
                    VALUES($1,$2,$3,$4,$5,$6)
                    ON CONFLICT(tenant,warehouse_id,id) DO
                    UPDATE
                    SET status=EXCLUDED.status,data=EXCLUDED.data",
                )
                .bind(t)
                .bind(w)
                .bind(&r.id)
                .bind(&r.reference)
                .bind(&r.status)
                .bind(json!(r))
                .execute(&mut *c)
                .await?;
                for l in &r.lines {
                    sqlx::query(
                        "INSERT INTO wms_receipt_lines
                        VALUES($1,$2,$3,$4,$5,$6,$7,$8)
                        ON CONFLICT(tenant,warehouse_id,receipt_id,id) DO
                        UPDATE
                        SET received=EXCLUDED.received",
                    )
                    .bind(t)
                    .bind(w)
                    .bind(&r.id)
                    .bind(&l.id)
                    .bind(&l.sku)
                    .bind(&l.lot)
                    .bind(l.quantity)
                    .bind(l.received)
                    .execute(&mut *c)
                    .await?;
                    if let Some(source) = &r.return_source {
                        if !previous_receipts.contains_key(r.id.as_str()) {
                            sqlx::query("INSERT INTO wms_return_lines(tenant,warehouse_id,receipt_id,line_id,order_id,order_line_id,sku,lot,quantity,reason) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10)")
                                .bind(t).bind(w).bind(&r.id).bind(&l.id).bind(&source.order_id)
                                .bind(source.line_ids.get(&l.id).ok_or_else(|| Error::new(422, "Missing original return line"))?)
                                .bind(&l.sku).bind(&l.lot).bind(l.quantity).bind(&source.reason).execute(&mut *c).await?;
                        }
                    }
                }
            }
        }
        for r in &s.receipts {
            if r.cancellation.is_some()
                && previous_receipts
                    .get(r.id.as_str())
                    .is_none_or(|old| old.cancellation.is_none())
            {
                crate::receipts::save(c, t, w, r).await?;
            }
        }
        for o in &s.orders {
            if changed(o, previous_orders.get(o.id.as_str()).copied()) {
                sqlx::query("INSERT INTO wms_orders(tenant,warehouse_id,id,reference,source_order_id,status,data)
                    VALUES($1,$2,$3,$4,$5,$6,$7)
                    ON CONFLICT(tenant,warehouse_id,id) DO
                    UPDATE
                    SET status=EXCLUDED.status,data=EXCLUDED.data").bind(t).bind(w).bind(&o.id).bind(&o.reference).bind(&o.source_order_id).bind(&o.status).bind(json!(o)).execute(&mut *c).await?;
                for l in &o.lines {
                    sqlx::query(
                        "INSERT INTO wms_order_lines
                        VALUES($1,$2,$3,$4,$5,$6)
                        ON CONFLICT DO NOTHING",
                    )
                    .bind(t)
                    .bind(w)
                    .bind(&o.id)
                    .bind(&l.id)
                    .bind(&l.sku)
                    .bind(l.quantity)
                    .execute(&mut *c)
                    .await?;
                }
                if let Some(previous) = previous_orders.get(o.id.as_str()) {
                    for a in &previous.allocations {
                        if !o.allocations.iter().any(|n| {
                            n.line_id == a.line_id && n.location == a.location && n.lot == a.lot
                        }) {
                            sqlx::query("DELETE FROM wms_allocations
                                WHERE tenant=$1 AND warehouse_id=$2 AND order_id=$3 AND line_id=$4 AND location=$5 AND lot=$6").bind(t).bind(w).bind(&o.id).bind(&a.line_id).bind(&a.location).bind(&a.lot).execute(&mut *c).await?;
                        }
                    }
                }
                for a in &o.allocations {
                    sqlx::query(
                        "INSERT INTO wms_allocations VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11) ON CONFLICT(tenant,warehouse_id,order_id,line_id,location,lot) DO UPDATE SET quantity=EXCLUDED.quantity,picked=EXCLUDED.picked,shipped=EXCLUDED.shipped,released=EXCLUDED.released",
                    )
                    .bind(t)
                    .bind(w)
                    .bind(&o.id)
                    .bind(&a.line_id)
                    .bind(&a.sku)
                    .bind(&a.lot)
                    .bind(&a.location)
                    .bind(a.quantity)
                    .bind(a.picked)
                    .bind(a.shipped)
                    .bind(a.released)
                    .execute(&mut *c)
                    .await?;
                }
            }
        }
        for e in &s.exceptions {
            if changed(e, previous_exceptions.get(e.id.as_str()).copied()) {
                sqlx::query(
                    "INSERT INTO wms_exceptions(tenant,warehouse_id,id,status,order_id,data)
                    VALUES($1,$2,$3,$4,$5,$6)
                    ON CONFLICT(tenant,warehouse_id,id) DO
                    UPDATE
                    SET status=EXCLUDED.status,data=EXCLUDED.data",
                )
                .bind(t)
                .bind(w)
                .bind(&e.id)
                .bind(&e.status)
                .bind(&e.order_id)
                .bind(json!(e))
                .execute(&mut *c)
                .await?;
            }
        }
        for m in &s.movements {
            if !previous_movements.contains(m.id.as_str()) {
                sqlx::query("INSERT INTO wms_movements(id,tenant,warehouse_id,sku,lot,from_location,to_location,quantity,kind,reference,receipt_id,receipt_line_id,data,transfer_id,transfer_line_id,transfer_event_id,package_id,order_line_id,allocation_location,shipment_event_id)
                    VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17,$18,$19,$20)").bind(&m.id).bind(t).bind(w).bind(&m.sku).bind(&m.lot).bind(&m.from).bind(&m.to).bind(m.quantity).bind(&m.kind).bind(&m.reference).bind(&m.receipt_id).bind(&m.receipt_line_id).bind(json!(m)).bind(&m.transfer_id).bind(&m.transfer_line_id).bind(&m.transfer_event_id).bind(&m.package_id).bind(&m.order_line_id).bind(&m.allocation_location).bind(&m.shipment_event_id).execute(&mut *c).await?;
            }
        }
        for a in &s.activity {
            if !previous_activity.contains(a.id.as_str()) {
                sqlx::query("INSERT INTO wms_activity(id,tenant,warehouse_id,kind,data) VALUES($1,$2,$3,$4,$5)").bind(&a.id).bind(t).bind(w).bind(&a.kind).bind(json!(a)).execute(&mut *c).await?;
            }
        }
        Ok(())
    }
}
