//! Tenant-local operational summaries. These routes never expose retained inputs,
//! remote responses, credentials, actor identities or arbitrary metric labels.
use crate::{
    api::App,
    store::{Error, Identity, Result, Store},
};
use axum::{
    extract::{Query, State},
    http::header,
    routing::get,
    Extension, Json, Router,
};
use serde::{Deserialize, Serialize};
use sqlx::Row;
use std::time::Duration;

const DESTINATIONS: [&str; 2] = ["oms", "tms"];
const OUTBOX_STATUSES: [&str; 4] = ["pending", "delivering", "delivered", "dead_letter"];
const NATIVE_KINDS: [&str; 3] = ["oms_import", "tms_transport", "oms_fulfillment"];
const NATIVE_STATUSES: [&str; 6] = [
    "frozen",
    "sending",
    "uncertain",
    "completed",
    "local_conflict",
    "rejected",
];

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Page {
    after: Option<String>,
    #[serde(default = "default_limit")]
    limit: i64,
}
fn default_limit() -> i64 {
    50
}
impl Default for Page {
    fn default() -> Self {
        Self {
            after: None,
            limit: default_limit(),
        }
    }
}
impl Page {
    fn validate(&self) -> Result<()> {
        if !(1..=100).contains(&self.limit) {
            return Err(Error::new(
                422,
                "Operations page limit must be between 1 and 100",
            ));
        }
        if self.after.as_ref().is_some_and(|value| {
            value.is_empty()
                || value.len() > 100
                || !value
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b"_-".contains(&byte))
        }) {
            return Err(Error::new(422, "Invalid warehouse page cursor"));
        }
        Ok(())
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NoQuery {}

#[derive(Serialize)]
struct OutboxGroup {
    destination: &'static str,
    status: &'static str,
    count: i64,
    due: i64,
    active_leases: i64,
    expired_leases: i64,
    oldest_age_seconds: i64,
    oldest_due_age_seconds: i64,
}
#[derive(Serialize)]
struct Receiver {
    destination: &'static str,
    configured: bool,
}
#[derive(Serialize)]
struct Outbox {
    total: i64,
    pending: i64,
    delivering: i64,
    delivered: i64,
    dead_letter: i64,
    due: i64,
    active_leases: i64,
    expired_leases: i64,
    oldest_pending_age_seconds: i64,
    receivers: Vec<Receiver>,
    groups: Vec<OutboxGroup>,
}
#[derive(Serialize)]
struct NativeGroup {
    kind: &'static str,
    status: &'static str,
    count: i64,
    active_leases: i64,
    expired_leases: i64,
    oldest_age_seconds: i64,
    oldest_updated_age_seconds: i64,
}
#[derive(Serialize)]
struct Native {
    total: i64,
    requires_review: i64,
    uncertain: i64,
    rejected: i64,
    local_conflict: i64,
    groups: Vec<NativeGroup>,
}
#[derive(Serialize)]
struct Warehouse {
    warehouse_id: String,
    version: i64,
    open_exceptions: i64,
}
#[derive(Serialize)]
struct Warehouses {
    total: i64,
    open_exceptions: i64,
    items: Vec<Warehouse>,
    next_after: Option<String>,
}
#[derive(Serialize)]
struct Snapshot {
    schema_version: u8,
    snapshot_at: String,
    outbox: Outbox,
    native: Native,
    warehouses: Warehouses,
}

fn role(identity: &Identity, admin_only: bool) -> Result<()> {
    if identity.role != "admin" && (admin_only || identity.role != "operator") {
        return Err(Error::new(
            403,
            if admin_only {
                "Operations metrics require a warehouse administrator"
            } else {
                "Operations health requires a warehouse operator or administrator"
            },
        ));
    }
    Ok(())
}

async fn snapshot(app: &App, tenant: &str, page: &Page) -> Result<Snapshot> {
    let mut result = tokio::time::timeout(
        Duration::from_secs(5),
        read_snapshot(&app.store, tenant, page),
    )
    .await
    .map_err(|_| Error::new(503, "Operations snapshot timed out; retry the read"))??;
    result.outbox.receivers = DESTINATIONS
        .into_iter()
        .map(|destination| Receiver {
            destination,
            configured: app
                .connections
                .0
                .contains_key(&(tenant.into(), destination.into())),
        })
        .collect();
    Ok(result)
}

async fn read_snapshot(store: &Store, tenant: &str, page: &Page) -> Result<Snapshot> {
    let mut tx = store.pool.begin().await?;
    sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY")
        .execute(&mut *tx)
        .await?;
    sqlx::query("SET LOCAL statement_timeout='3s'")
        .execute(&mut *tx)
        .await?;
    let snapshot_at = sqlx::query_scalar("SELECT to_char(transaction_timestamp() AT TIME ZONE 'UTC','YYYY-MM-DD\"T\"HH24:MI:SS.US\"Z\"')")
        .fetch_one(&mut *tx)
        .await?;
    let rows = sqlx::query(
        "SELECT o.destination,o.status,count(*) AS count, \
         count(*) FILTER(WHERE (o.status='pending' AND o.due_at<=now()) OR (o.status='delivering' AND o.lease_until<=now())) AS due, \
         count(*) FILTER(WHERE o.status='delivering' AND o.lease_until>now()) AS active_leases, \
         count(*) FILTER(WHERE o.status='delivering' AND o.lease_until<=now()) AS expired_leases, \
         greatest(0,floor(extract(epoch FROM now()-min(e.created_at))))::bigint AS oldest_age_seconds, \
         COALESCE(greatest(0,floor(extract(epoch FROM now()-min(CASE WHEN o.status='pending' AND o.due_at<=now() THEN o.due_at WHEN o.status='delivering' AND o.lease_until<=now() THEN o.lease_until END)))),0)::bigint AS oldest_due_age_seconds \
         FROM wms_outbox o JOIN wms_events e ON e.id=o.event_id AND e.tenant=o.tenant \
         WHERE o.tenant=$1 GROUP BY o.destination,o.status",
    )
    .bind(tenant)
    .fetch_all(&mut *tx)
    .await?;
    let mut groups = Vec::with_capacity(8);
    for destination in DESTINATIONS {
        for status in OUTBOX_STATUSES {
            let row = rows.iter().find(|row| {
                row.get::<String, _>("destination") == destination
                    && row.get::<String, _>("status") == status
            });
            groups.push(OutboxGroup {
                destination,
                status,
                count: row.map(|r| r.get("count")).unwrap_or(0),
                due: row.map(|r| r.get("due")).unwrap_or(0),
                active_leases: row.map(|r| r.get("active_leases")).unwrap_or(0),
                expired_leases: row.map(|r| r.get("expired_leases")).unwrap_or(0),
                oldest_age_seconds: row.map(|r| r.get("oldest_age_seconds")).unwrap_or(0),
                oldest_due_age_seconds: row.map(|r| r.get("oldest_due_age_seconds")).unwrap_or(0),
            });
        }
    }
    let outbox = Outbox {
        total: groups.iter().map(|g| g.count).sum(),
        pending: groups
            .iter()
            .filter(|g| g.status == "pending")
            .map(|g| g.count)
            .sum(),
        delivering: groups
            .iter()
            .filter(|g| g.status == "delivering")
            .map(|g| g.count)
            .sum(),
        delivered: groups
            .iter()
            .filter(|g| g.status == "delivered")
            .map(|g| g.count)
            .sum(),
        dead_letter: groups
            .iter()
            .filter(|g| g.status == "dead_letter")
            .map(|g| g.count)
            .sum(),
        due: groups.iter().map(|g| g.due).sum(),
        active_leases: groups.iter().map(|g| g.active_leases).sum(),
        expired_leases: groups.iter().map(|g| g.expired_leases).sum(),
        oldest_pending_age_seconds: groups
            .iter()
            .filter(|g| ["pending", "delivering"].contains(&g.status))
            .map(|g| g.oldest_age_seconds)
            .max()
            .unwrap_or(0),
        receivers: Vec::new(),
        groups,
    };
    let rows = sqlx::query(
        "SELECT kind,status,count(*) AS count, \
         count(*) FILTER(WHERE status='sending' AND lease_until>now()) AS active_leases, \
         count(*) FILTER(WHERE status='sending' AND lease_until<=now()) AS expired_leases, \
         greatest(0,floor(extract(epoch FROM now()-min(created_at))))::bigint AS oldest_age_seconds, \
         greatest(0,floor(extract(epoch FROM now()-min(updated_at))))::bigint AS oldest_updated_age_seconds \
         FROM wms_native_requests WHERE tenant=$1 GROUP BY kind,status",
    )
    .bind(tenant)
    .fetch_all(&mut *tx)
    .await?;
    let mut groups = Vec::with_capacity(18);
    for kind in NATIVE_KINDS {
        for status in NATIVE_STATUSES {
            let row = rows.iter().find(|row| {
                row.get::<String, _>("kind") == kind && row.get::<String, _>("status") == status
            });
            groups.push(NativeGroup {
                kind,
                status,
                count: row.map(|r| r.get("count")).unwrap_or(0),
                active_leases: row.map(|r| r.get("active_leases")).unwrap_or(0),
                expired_leases: row.map(|r| r.get("expired_leases")).unwrap_or(0),
                oldest_age_seconds: row.map(|r| r.get("oldest_age_seconds")).unwrap_or(0),
                oldest_updated_age_seconds: row
                    .map(|r| r.get("oldest_updated_age_seconds"))
                    .unwrap_or(0),
            });
        }
    }
    let native = Native {
        total: groups.iter().map(|g| g.count).sum(),
        requires_review: groups
            .iter()
            .filter(|g| ["uncertain", "local_conflict", "rejected"].contains(&g.status))
            .map(|g| g.count)
            .sum(),
        uncertain: groups
            .iter()
            .filter(|g| g.status == "uncertain")
            .map(|g| g.count)
            .sum(),
        rejected: groups
            .iter()
            .filter(|g| g.status == "rejected")
            .map(|g| g.count)
            .sum(),
        local_conflict: groups
            .iter()
            .filter(|g| g.status == "local_conflict")
            .map(|g| g.count)
            .sum(),
        groups,
    };
    let total: i64 = sqlx::query_scalar("SELECT count(*) FROM wms_warehouses WHERE tenant=$1")
        .bind(tenant)
        .fetch_one(&mut *tx)
        .await?;
    let open_exceptions: i64 =
        sqlx::query_scalar("SELECT count(*) FROM wms_exceptions WHERE tenant=$1 AND status='open'")
            .bind(tenant)
            .fetch_one(&mut *tx)
            .await?;
    let rows = sqlx::query(
        "SELECT w.warehouse_id,w.version,(SELECT count(*) FROM wms_exceptions e WHERE e.tenant=w.tenant AND e.warehouse_id=w.warehouse_id AND e.status='open') AS open_exceptions \
         FROM wms_warehouses w WHERE w.tenant=$1 AND ($2::text IS NULL OR w.warehouse_id>$2) ORDER BY w.warehouse_id LIMIT $3",
    )
    .bind(tenant)
    .bind(&page.after)
    .bind(page.limit + 1)
    .fetch_all(&mut *tx)
    .await?;
    let has_more = rows.len() > page.limit as usize;
    let items = rows
        .into_iter()
        .take(page.limit as usize)
        .map(|r| Warehouse {
            warehouse_id: r.get("warehouse_id"),
            version: r.get("version"),
            open_exceptions: r.get("open_exceptions"),
        })
        .collect::<Vec<_>>();
    let next_after = has_more
        .then(|| items.last().map(|w| w.warehouse_id.clone()))
        .flatten();
    tx.commit().await?;
    Ok(Snapshot {
        schema_version: 1,
        snapshot_at,
        outbox,
        native,
        warehouses: Warehouses {
            total,
            open_exceptions,
            items,
            next_after,
        },
    })
}

async fn health(
    State(app): State<App>,
    Extension(identity): Extension<Identity>,
    Query(page): Query<Page>,
) -> Result<Json<Snapshot>> {
    role(&identity, false)?;
    page.validate()?;
    Ok(Json(snapshot(&app, &identity.tenant, &page).await?))
}

fn metric_header(output: &mut String, name: &str, description: &str) {
    output.push_str(&format!(
        "# HELP {name} {description}\n# TYPE {name} gauge\n"
    ));
}
fn prometheus(snapshot: &Snapshot) -> String {
    let mut output = String::with_capacity(16_384);
    metric_header(&mut output, "wms_outbox_receiver_configured", "Whether this tenant has a generic event receiver configured; native adapters are separate.");
    for receiver in &snapshot.outbox.receivers {
        output.push_str(&format!(
            "wms_outbox_receiver_configured{{destination=\"{}\"}} {}\n",
            receiver.destination,
            i64::from(receiver.configured)
        ));
    }
    metric_header(
        &mut output,
        "wms_outbox_items",
        "Retained event deliveries in this credential's tenant.",
    );
    for group in &snapshot.outbox.groups {
        output.push_str(&format!(
            "wms_outbox_items{{destination=\"{}\",status=\"{}\"}} {}\n",
            group.destination, group.status, group.count
        ));
    }
    for (name, description) in [
        (
            "wms_outbox_due_items",
            "Deliveries ready for claim or with an expired lease.",
        ),
        (
            "wms_outbox_active_leases",
            "Deliveries with an active lease.",
        ),
        (
            "wms_outbox_expired_leases",
            "Deliveries whose lease has expired.",
        ),
        (
            "wms_outbox_oldest_pending_age_seconds",
            "Age since event commit of the oldest pending or delivering event; zero when absent.",
        ),
        (
            "wms_outbox_oldest_due_age_seconds",
            "Age since retry became due or a lease expired; zero when absent.",
        ),
    ] {
        metric_header(&mut output, name, description);
        for destination in DESTINATIONS {
            let groups = snapshot
                .outbox
                .groups
                .iter()
                .filter(|g| g.destination == destination);
            let value = match name {
                "wms_outbox_due_items" => groups.map(|g| g.due).sum(),
                "wms_outbox_active_leases" => groups.map(|g| g.active_leases).sum(),
                "wms_outbox_expired_leases" => groups.map(|g| g.expired_leases).sum(),
                "wms_outbox_oldest_pending_age_seconds" => groups
                    .filter(|g| ["pending", "delivering"].contains(&g.status))
                    .map(|g| g.oldest_age_seconds)
                    .max()
                    .unwrap_or(0),
                _ => groups.map(|g| g.oldest_due_age_seconds).max().unwrap_or(0),
            };
            output.push_str(&format!(
                "{name}{{destination=\"{destination}\"}} {value}\n"
            ));
        }
    }
    metric_header(
        &mut output,
        "wms_native_requests",
        "Retained native requests in this credential's tenant.",
    );
    for group in &snapshot.native.groups {
        output.push_str(&format!(
            "wms_native_requests{{kind=\"{}\",status=\"{}\"}} {}\n",
            group.kind, group.status, group.count
        ));
    }
    for (name, description) in [
        (
            "wms_native_oldest_age_seconds",
            "Age since original intent creation; zero when absent.",
        ),
        (
            "wms_native_oldest_updated_age_seconds",
            "Age since most recent journal state change; zero when absent.",
        ),
        (
            "wms_native_active_leases",
            "Native requests with an active delivery lease.",
        ),
        (
            "wms_native_expired_leases",
            "Native requests with an expired delivery lease.",
        ),
    ] {
        metric_header(&mut output, name, description);
        for group in &snapshot.native.groups {
            let value = match name {
                "wms_native_oldest_age_seconds" => group.oldest_age_seconds,
                "wms_native_oldest_updated_age_seconds" => group.oldest_updated_age_seconds,
                "wms_native_active_leases" => group.active_leases,
                _ => group.expired_leases,
            };
            output.push_str(&format!(
                "{name}{{kind=\"{}\",status=\"{}\"}} {value}\n",
                group.kind, group.status
            ));
        }
    }
    for (name, description, value) in [
        (
            "wms_warehouse_count",
            "Warehouses owned by this credential's tenant.",
            snapshot.warehouses.total,
        ),
        (
            "wms_open_exceptions",
            "Open exceptions across all warehouses in this credential's tenant.",
            snapshot.warehouses.open_exceptions,
        ),
    ] {
        metric_header(&mut output, name, description);
        output.push_str(&format!("{name} {value}\n"));
    }
    output
}

async fn metrics(
    State(app): State<App>,
    Extension(identity): Extension<Identity>,
    Query(_query): Query<NoQuery>,
) -> Result<([(header::HeaderName, &'static str); 1], String)> {
    role(&identity, true)?;
    let snapshot = snapshot(&app, &identity.tenant, &Page::default()).await?;
    Ok((
        [(
            header::CONTENT_TYPE,
            "text/plain; version=0.0.4; charset=utf-8",
        )],
        prometheus(&snapshot),
    ))
}

/// Merge below the main API authentication middleware.
pub fn router() -> Router<App> {
    Router::new()
        .route("/v1/operations/health", get(health))
        .route("/v1/operations/metrics", get(metrics))
}
