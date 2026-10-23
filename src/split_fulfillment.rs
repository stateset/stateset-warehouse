//! Available-stock allocation and immutable per-package packing/handoffs.
//! Context is loaded from canonical SQL ownership, never client JSON.
use crate::{
    domain::{self, Allocation, Command, Order, State},
    store::{Error, Result},
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sqlx::PgConnection;
use std::collections::BTreeSet;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LineInput {
    pub line_id: String,
    pub location: String,
    pub lot: String,
    pub quantity: i64,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PackedLine {
    pub line_id: String,
    pub sku: String,
    pub location: String,
    pub lot: String,
    pub quantity: i64,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Closure {
    pub kind: String,
    pub reason: String,
    pub carrier: String,
    pub handoff_reference: String,
    pub actor: String,
    pub closed_at: String,
    pub event_id: String,
    pub version: i64,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Package {
    pub id: String,
    pub order_id: String,
    pub reference: String,
    pub actor: String,
    pub packed_at: String,
    pub source_event_id: String,
    pub source_version: i64,
    pub lines: Vec<PackedLine>,
    pub closure: Option<Closure>,
}
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Context {
    pub order_id: String,
    pub packages: Vec<Package>,
    pub ok: bool,
}
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Change {
    Pack(Package),
    Close {
        package_id: String,
        order_id: String,
        closure: Closure,
    },
}
fn text(s: &str) -> domain::Result<()> {
    if s.trim().is_empty() || s.trim() != s || s.len() > 240 || s.chars().any(char::is_control) {
        return Err("Shipment fields require 1–240 characters without surrounding whitespace or control characters".into());
    }
    Ok(())
}
fn quantity(q: i64) -> domain::Result<()> {
    if !(1..=1_000_000).contains(&q) {
        return Err("Shipment quantities must be 1–1,000,000 whole each units".into());
    }
    Ok(())
}
pub(crate) fn status(o: &Order) -> &'static str {
    if o.remainder_cancellation.is_some() {
        return "closed_partial";
    }
    let ordered: i64 = o.lines.iter().map(|l| l.quantity).sum();
    let shipped: i64 = o.allocations.iter().map(|a| a.shipped).sum();
    let picked: i64 = o.allocations.iter().map(|a| a.picked).sum();
    if shipped == ordered {
        "shipped"
    } else if shipped > 0 {
        "partially_shipped"
    } else if picked == ordered {
        "picked"
    } else if picked > 0 {
        "picking"
    } else if o.allocations.iter().any(|a| a.quantity > a.released) {
        "allocated"
    } else {
        "pending"
    }
}
pub(crate) fn require_legacy(state: &State, order: &str) -> domain::Result<()> {
    if state.orders[state.order_index(order)?]
        .shipment_mode
        .is_some()
    {
        return Err("Use shipment packages and available-stock allocation for this order".into());
    }
    Ok(())
}
fn held(context: &Context, a: &Allocation) -> i64 {
    context
        .packages
        .iter()
        .filter(|p| p.closure.is_none())
        .flat_map(|p| &p.lines)
        .filter(|l| l.line_id == a.line_id && l.location == a.location && l.lot == a.lot)
        .map(|l| l.quantity)
        .sum()
}
fn context(state: &mut State, order: &str) -> domain::Result<()> {
    let i = state.order_index(order)?;
    if state.orders[i].shipment_mode.is_none() {
        state.shipment_context = Some(Context {
            order_id: order.into(),
            packages: vec![],
            ok: true,
        });
    }
    if !state
        .shipment_context
        .as_ref()
        .is_some_and(|c| c.order_id == order && c.ok)
    {
        return Err("Verified shipment ownership is required before changing this order".into());
    }
    if ["shipped", "cancelled", "closed_partial"].contains(&state.orders[i].status.as_str()) {
        return Err("Completed fulfillment cannot change packing or allocation".into());
    }
    state.orders[i].shipment_mode = Some("packages".into());
    Ok(())
}
pub(crate) fn apply(state: &mut State, command: &Command, actor: &str) -> domain::Result<String> {
    let order_id = match command {
        Command::AllocateAvailable { order_id }
        | Command::ReleaseUnpicked { order_id, .. }
        | Command::PackShipment { order_id, .. } => order_id.clone(),
        Command::UnpackShipment { shipment_id, .. }
        | Command::HandoffShipment { shipment_id, .. } => state
            .shipment_context
            .as_ref()
            .and_then(|c| {
                c.packages
                    .iter()
                    .find(|p| p.id == *shipment_id)
                    .map(|p| p.order_id.clone())
            })
            .ok_or("Original shipment package not found")?,
        _ => return Err("Unsupported shipment action".into()),
    };
    if matches!(command, Command::ReleaseUnpicked { .. })
        && state.orders[state.order_index(&order_id)?]
            .shipment_mode
            .is_none()
    {
        return Err("Use allocation reset for whole-order fulfillment".into());
    }
    context(state, &order_id)?;
    let i = state.order_index(&order_id)?;
    if !matches!(
        command,
        Command::ReleaseUnpicked { .. } | Command::UnpackShipment { .. }
    ) {
        state.unblocked(&order_id)?;
    }
    let entity = match command {
        Command::AllocateAvailable { .. } => {
            let mut allocated = 0;
            for line in state.orders[i].lines.clone() {
                let mut remaining = line.quantity
                    - state.orders[i]
                        .allocations
                        .iter()
                        .filter(|a| a.line_id == line.id)
                        .map(|a| a.quantity - a.released)
                        .sum::<i64>();
                let mut bins = state
                    .stocks
                    .iter()
                    .enumerate()
                    .filter(|(_, b)| {
                        b.sku == line.sku
                            && b.on_hand > b.reserved
                            && crate::lot_control::eligible(state, &b.sku, &b.lot)
                            && state
                                .locations
                                .iter()
                                .any(|l| l.code == b.location && l.kind == "storage")
                    })
                    .map(|(j, b)| (j, b.location.clone(), b.lot.clone()))
                    .collect::<Vec<_>>();
                bins.sort_by_key(|a| {
                    (
                        crate::lot_control::fefo(state, &line.sku, &a.2),
                        a.1.clone(),
                        a.2.clone(),
                    )
                });
                for (j, location, lot) in bins {
                    let q = remaining.min(state.stocks[j].on_hand - state.stocks[j].reserved);
                    if q == 0 {
                        continue;
                    }
                    state.stocks[j].reserved += q;
                    remaining -= q;
                    allocated += q;
                    if let Some(a) = state.orders[i]
                        .allocations
                        .iter_mut()
                        .find(|a| a.line_id == line.id && a.location == location && a.lot == lot)
                    {
                        a.quantity = a.quantity - a.released + q;
                        a.released = 0;
                    } else {
                        state.orders[i].allocations.push(Allocation {
                            line_id: line.id.clone(),
                            sku: line.sku.clone(),
                            lot,
                            location,
                            quantity: q,
                            picked: 0,
                            shipped: 0,
                            released: 0,
                        });
                    }
                    if remaining == 0 {
                        break;
                    }
                }
            }
            if allocated == 0 {
                return Err(
                    "No unreserved storage stock is available for the remaining order lines".into(),
                );
            }
            order_id.clone()
        }
        Command::ReleaseUnpicked { reason, .. } => {
            text(reason)?;
            let mut released = 0;
            for a in state.orders[i].allocations.clone() {
                let q = a.quantity - a.picked - a.released;
                if q > 0 {
                    state.bucket(&a.sku, &a.lot, &a.location).reserved -= q;
                    released += q;
                }
            }
            if released == 0 {
                return Err("This order has no unpicked reservations to release".into());
            }
            for a in &mut state.orders[i].allocations {
                a.released = a.quantity - a.picked;
            }
            order_id.clone()
        }
        Command::PackShipment {
            reference, lines, ..
        } => {
            text(reference)?;
            if lines.is_empty() || lines.len() > 100 {
                return Err("A shipment package requires 1–100 exact allocation lines".into());
            }
            let mut keys = BTreeSet::new();
            let mut packed = vec![];
            for l in lines {
                quantity(l.quantity)?;
                for v in [&l.line_id, &l.location, &l.lot] {
                    text(v)?;
                }
                if !keys.insert((&l.line_id, &l.location, &l.lot)) {
                    return Err("A package cannot repeat the same allocation".into());
                }
                let a = state.orders[i]
                    .allocations
                    .iter()
                    .find(|a| a.line_id == l.line_id && a.location == l.location && a.lot == l.lot)
                    .ok_or("Original picked allocation not found")?;
                if l.quantity
                    > a.picked
                        - a.shipped
                        - held(
                            state
                                .shipment_context
                                .as_ref()
                                .ok_or("Shipment ownership missing")?,
                            a,
                        )
                {
                    return Err(
                        "Package exceeds unpacked staged quantity for this exact allocation".into(),
                    );
                }
                crate::lot_control::require(state, &a.sku, &a.lot)?;
                packed.push(PackedLine {
                    line_id: l.line_id.clone(),
                    sku: a.sku.clone(),
                    location: l.location.clone(),
                    lot: l.lot.clone(),
                    quantity: l.quantity,
                });
            }
            let p = Package {
                id: domain::id(),
                order_id: order_id.clone(),
                reference: reference.clone(),
                actor: actor.into(),
                packed_at: domain::now(),
                source_event_id: String::new(),
                source_version: state.version + 1,
                lines: packed,
                closure: None,
            };
            let id = p.id.clone();
            state.shipment_change = Some(Change::Pack(p.clone()));
            state
                .shipment_context
                .as_mut()
                .ok_or("Shipment ownership missing")?
                .packages
                .push(p);
            id
        }
        Command::UnpackShipment {
            shipment_id,
            reason,
        } => {
            text(reason)?;
            let c = Closure {
                kind: "unpacked".into(),
                reason: reason.clone(),
                carrier: String::new(),
                handoff_reference: String::new(),
                actor: actor.into(),
                closed_at: domain::now(),
                event_id: String::new(),
                version: state.version + 1,
            };
            close(state, shipment_id, &order_id, c)?;
            shipment_id.clone()
        }
        Command::HandoffShipment {
            shipment_id,
            carrier,
            handoff_reference,
        } => {
            text(carrier)?;
            text(handoff_reference)?;
            let p = state
                .shipment_context
                .as_ref()
                .and_then(|c| {
                    c.packages
                        .iter()
                        .find(|p| p.id == *shipment_id && p.closure.is_none())
                })
                .cloned()
                .ok_or("Only an original packed shipment can be handed off")?;
            let at = domain::now();
            for l in &p.lines {
                crate::lot_control::require(state, &l.sku, &l.lot)?;
                let a = state.orders[i]
                    .allocations
                    .iter_mut()
                    .find(|a| a.line_id == l.line_id && a.location == l.location && a.lot == l.lot)
                    .ok_or("Original shipment allocation missing")?;
                if l.quantity > a.picked - a.shipped {
                    return Err("Shipment exceeds the original staged quantity".into());
                }
                a.shipped += l.quantity;
                let b = state.bucket(&l.sku, &l.lot, "STAGE-01");
                b.on_hand -= l.quantity;
                b.reserved -= l.quantity;
                let reference = state.orders[i].reference.clone();
                state.movement(
                    "shipment",
                    &l.sku,
                    &l.lot,
                    Some("STAGE-01"),
                    None,
                    l.quantity,
                    &reference,
                    handoff_reference,
                    actor,
                );
                let m = state
                    .movements
                    .last_mut()
                    .ok_or("Physical shipment movement missing")?;
                m.package_id = Some(shipment_id.clone());
                m.order_line_id = Some(l.line_id.clone());
                m.allocation_location = Some(l.location.clone());
                m.at = at.clone();
            }
            let prior = state
                .shipment_context
                .as_ref()
                .ok_or("Shipment ownership missing")?
                .packages
                .iter()
                .filter(|p| p.closure.as_ref().is_some_and(|c| c.kind == "handed_off"))
                .count();
            state.orders[i].shipped_at = Some(at.clone());
            if prior == 0 {
                state.orders[i].carrier = carrier.clone();
                state.orders[i].handoff_reference = handoff_reference.clone();
            } else {
                state.orders[i].carrier.clear();
                state.orders[i].handoff_reference.clear();
            }
            close(
                state,
                shipment_id,
                &order_id,
                Closure {
                    kind: "handed_off".into(),
                    reason: String::new(),
                    carrier: carrier.clone(),
                    handoff_reference: handoff_reference.clone(),
                    actor: actor.into(),
                    closed_at: at,
                    event_id: String::new(),
                    version: state.version + 1,
                },
            )?;
            shipment_id.clone()
        }
        _ => return Err("Unsupported shipment action".into()),
    };
    state.orders[i].status = status(&state.orders[i]).into();
    Ok(entity)
}
fn close(state: &mut State, id: &str, order: &str, closure: Closure) -> domain::Result<()> {
    let package = state
        .shipment_context
        .as_mut()
        .and_then(|c| c.packages.iter_mut().find(|p| p.id == id))
        .ok_or("Original shipment package not found")?;
    if package.closure.is_some() {
        return Err("Shipment package already has an immutable terminal outcome".into());
    }
    package.closure = Some(closure.clone());
    state.shipment_change = Some(Change::Close {
        package_id: id.into(),
        order_id: order.into(),
        closure,
    });
    Ok(())
}
pub(crate) fn pick(state: &mut State, command: &Command, actor: &str) -> domain::Result<String> {
    let (order_id, line, location, lot, q, unpick, reason) = match command {
        Command::Pick {
            order_id,
            line_id,
            location,
            lot,
            quantity,
        } => (
            order_id,
            line_id,
            location,
            lot,
            *quantity,
            false,
            "Order line pick",
        ),
        Command::Unpick {
            order_id,
            line_id,
            location,
            lot,
            quantity,
            reason,
        } => (
            order_id,
            line_id,
            location,
            lot,
            *quantity,
            true,
            reason.as_str(),
        ),
        _ => return Err("Unsupported pick".into()),
    };
    quantity(q)?;
    if unpick {
        text(reason)?;
    } else {
        state.unblocked(order_id)?;
    }
    context(state, order_id)?;
    let i = state.order_index(order_id)?;
    if !unpick {
        let sku = state.orders[i]
            .allocations
            .iter()
            .find(|a| a.line_id == *line && a.location == *location && a.lot == *lot)
            .ok_or("Original allocation not found")?
            .sku
            .clone();
        crate::lot_control::require(state, &sku, lot)?;
    }
    let a = state.orders[i]
        .allocations
        .iter_mut()
        .find(|a| a.line_id == *line && a.location == *location && a.lot == *lot)
        .ok_or("Original allocation not found")?;
    if unpick {
        if q > a.picked
            - a.shipped
            - held(
                state
                    .shipment_context
                    .as_ref()
                    .ok_or("Shipment ownership missing")?,
                a,
            )
        {
            return Err("Unpack the original package before returning its staged units".into());
        }
        a.picked -= q;
    } else {
        if q > a.quantity - a.picked - a.released {
            return Err("Pick exceeds remaining reservation".into());
        }
        a.picked += q;
    }
    let sku = a.sku.clone();
    let sign = if unpick { -q } else { q };
    let source = state.bucket(&sku, lot, location);
    source.on_hand -= sign;
    source.reserved -= sign;
    let staged = state.bucket(&sku, lot, "STAGE-01");
    staged.on_hand += sign;
    staged.reserved += sign;
    let reference = state.orders[i].reference.clone();
    state.movement(
        if unpick { "unpick" } else { "pick" },
        &sku,
        lot,
        Some(if unpick { "STAGE-01" } else { location }),
        Some(if unpick { location } else { "STAGE-01" }),
        q,
        &reference,
        reason,
        actor,
    );
    state.orders[i].status = status(&state.orders[i]).into();
    Ok(order_id.clone())
}

pub(crate) async fn order_id(
    c: &mut PgConnection,
    t: &str,
    w: &str,
    command: &Command,
) -> Result<Option<String>> {
    match command {
        Command::UnpackShipment { shipment_id, .. }
        | Command::HandoffShipment { shipment_id, .. } => Ok(Some(
            sqlx::query_scalar(
                "SELECT order_id FROM wms_packages WHERE tenant=$1 AND warehouse_id=$2 AND id=$3",
            )
            .bind(t)
            .bind(w)
            .bind(shipment_id)
            .fetch_optional(c)
            .await?
            .ok_or_else(|| Error::new(404, "Shipment package not found"))?,
        )),
        _ => Ok(None),
    }
}
pub(crate) async fn load(c: &mut PgConnection, t: &str, w: &str, order: &Order) -> Result<Context> {
    let rows: Vec<Value> = sqlx::query_scalar(
        "SELECT wms_package_document($1,$2,id)
        FROM wms_packages
        WHERE tenant=$1 AND warehouse_id=$2 AND order_id=$3
        ORDER BY seq",
    )
    .bind(t)
    .bind(w)
    .bind(&order.id)
    .fetch_all(&mut *c)
    .await?;
    let ok = if order.shipment_mode.is_some() {
        sqlx::query_scalar("SELECT wms_split_consistent($1,$2,$3)")
            .bind(t)
            .bind(w)
            .bind(&order.id)
            .fetch_one(c)
            .await?
    } else {
        rows.is_empty()
    };
    Ok(Context {
        order_id: order.id.clone(),
        packages: rows
            .into_iter()
            .map(crate::persistence::decode)
            .collect::<Result<_>>()?,
        ok,
    })
}
pub(crate) async fn persist(
    c: &mut PgConnection,
    t: &str,
    old: &State,
    next: &State,
    eid: &str,
) -> Result<()> {
    let w = &next.warehouse.id;
    for order in &next.orders {
        if order.shipment_mode.is_some()
            && old
                .orders
                .iter()
                .any(|o| o.id == order.id && o.shipment_mode.is_none())
        {
            sqlx::query("INSERT INTO wms_split_orders(tenant,warehouse_id,order_id,source_event_id,source_version,document)
                VALUES($1,$2,$3,$4,$5,wms_split_contract($6))").bind(t).bind(w).bind(&order.id).bind(eid).bind(next.version).bind(json!(order)).execute(&mut *c).await?;
        }
    }
    match &next.shipment_change {
        Some(Change::Pack(p)) => {
            sqlx::query("INSERT INTO wms_packages(tenant,warehouse_id,id,order_id,reference,actor,packed_at,source_event_id,source_version)
                VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9)").bind(t).bind(w).bind(&p.id).bind(&p.order_id).bind(&p.reference).bind(&p.actor).bind(&p.packed_at).bind(eid).bind(next.version).execute(&mut *c).await?;
            for l in &p.lines {
                sqlx::query("INSERT INTO wms_package_lines(tenant,warehouse_id,package_id,order_id,line_id,sku,location,lot,quantity)
                    VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9)").bind(t).bind(w).bind(&p.id).bind(&p.order_id).bind(&l.line_id).bind(&l.sku).bind(&l.location).bind(&l.lot).bind(l.quantity).execute(&mut *c).await?;
            }
        }
        Some(Change::Close {
            package_id,
            order_id,
            closure: s,
        }) => {
            sqlx::query("INSERT INTO wms_package_closures(tenant,warehouse_id,package_id,order_id,kind,reason,carrier,handoff_reference,actor,closed_at,event_id,version)
                VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12)").bind(t).bind(w).bind(package_id).bind(order_id).bind(&s.kind).bind(&s.reason).bind(&s.carrier).bind(&s.handoff_reference).bind(&s.actor).bind(&s.closed_at).bind(eid).bind(next.version).execute(&mut *c).await?;
        }
        None => {}
    }
    Ok(())
}

pub async fn endpoint(
    axum::extract::State(app): axum::extract::State<crate::api::App>,
    axum::Extension(identity): axum::Extension<crate::store::Identity>,
    axum::extract::Path((warehouse, order_id)): axum::extract::Path<(String, String)>,
) -> Result<axum::Json<Value>> {
    let mut tx = app.store.pool.begin().await?;
    sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY")
        .execute(&mut *tx)
        .await?;
    let data: Option<Value> = sqlx::query_scalar("SELECT wms_current_order($1,$2,$3)")
        .bind(&identity.tenant)
        .bind(&warehouse)
        .bind(&order_id)
        .fetch_one(&mut *tx)
        .await?;
    let order: Order = crate::persistence::decode(
        data.ok_or_else(|| Error::new(404, "Warehouse order not found"))?,
    )?;
    let context = load(&mut tx, &identity.tenant, &warehouse, &order).await?;
    let version: i64 = sqlx::query_scalar(
        "SELECT version FROM wms_warehouses WHERE tenant=$1 AND warehouse_id=$2",
    )
    .bind(&identity.tenant)
    .bind(&warehouse)
    .fetch_one(&mut *tx)
    .await?;
    let progress=order.lines.iter().map(|l|{let a:Vec<_>=order.allocations.iter().filter(|a|a.line_id==l.id).collect();let allocated:i64=a.iter().map(|a|a.quantity-a.released).sum();json!({
        "line_id":l.id,
        "sku":l.sku,
        "ordered":l.quantity,
        "allocated":allocated,
        "picked":a.iter().map(|a|a.picked).sum::<i64>(),
        "shipped":a.iter().map(|a|a.shipped).sum::<i64>(),
        "cancelled":crate::order_cancellation::cancelled(&order,&l.id),
        "unallocated":l.quantity-allocated-crate::order_cancellation::cancelled(&order,&l.id)
    })}).collect::<Vec<_>>();
    tx.commit().await?;
    Ok(axum::Json(
        json!({"warehouse_id":warehouse,"order_id":order_id,"version":version,"ok":context.ok,"packages":context.packages,"progress":progress,"order":order}),
    ))
}
pub(crate) async fn proof(c: &mut PgConnection, t: &str, w: &str) -> Result<Vec<Value>> {
    sqlx::query_scalar("SELECT jsonb_build_object('order_id',b.order_id,'source_event_id',b.source_event_id,'ok',wms_split_consistent($1,$2,b.order_id),'status',o.status,'packages',(
        SELECT COALESCE(jsonb_agg(wms_package_document($1,$2,p.id)
        ORDER BY p.seq),'[]'::jsonb)
        FROM wms_packages p
        WHERE p.tenant=$1 AND p.warehouse_id=$2 AND p.order_id=b.order_id))
        FROM wms_split_orders b
        JOIN wms_orders o ON o.tenant=b.tenant AND o.warehouse_id=b.warehouse_id AND o.id=b.order_id
        WHERE b.tenant=$1 AND b.warehouse_id=$2
        ORDER BY b.order_id").bind(t).bind(w).fetch_all(c).await.map_err(Into::into)
}
