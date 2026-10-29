//! Connecting, migrating and refusing a newer schema; provisioning warehouses.
use super::*;

impl Store {
    pub async fn connect(url: &str) -> Result<Self> {
        let pool = PgPoolOptions::new()
            .max_connections(8)
            .acquire_timeout(Duration::from_secs(5))
            .after_connect(|c, _| {
                Box::pin(async move {
                    sqlx::query("SET statement_timeout='10s'")
                        .execute(&mut *c)
                        .await?;
                    sqlx::query("SET lock_timeout='5s'")
                        .execute(&mut *c)
                        .await?;
                    Ok(())
                })
            })
            .connect(url)
            .await?;
        let mut tx = pool.begin().await?;
        sqlx::query("SELECT pg_advisory_xact_lock(84721821)")
            .execute(&mut *tx)
            .await?;
        sqlx::query("CREATE TABLE IF NOT EXISTS wms_schema_migrations(name TEXT PRIMARY KEY,digest TEXT NOT NULL,applied_at TIMESTAMPTZ NOT NULL DEFAULT now())").execute(&mut *tx).await?;
        let migrations = [
            (
                "001_initial",
                include_str!("../../migrations/001_initial.sql"),
            ),
            (
                "002_relational_execution",
                include_str!("../../migrations/002_relational_execution.sql"),
            ),
            (
                "003_native_operations",
                include_str!("../../migrations/003_native_operations.sql"),
            ),
            (
                "004_native_source_ownership",
                include_str!("../../migrations/004_native_source_ownership.sql"),
            ),
            (
                "005_conditional_oms_fulfillment",
                include_str!("../../migrations/005_conditional_oms_fulfillment.sql"),
            ),
            (
                "006_oms_rejections",
                include_str!("../../migrations/006_oms_rejections.sql"),
            ),
            (
                "007_operations_indexes",
                include_str!("../../migrations/007_operations_indexes.sql"),
            ),
            (
                "008_delivery_recipient",
                include_str!("../../migrations/008_delivery_recipient.sql"),
            ),
            (
                "009_resource_versions",
                include_str!("../../migrations/009_resource_versions.sql"),
            ),
            (
                "010_customer_returns",
                include_str!("../../migrations/010_customer_returns.sql"),
            ),
            (
                "011_immutable_command_receipts",
                include_str!("../../migrations/011_immutable_command_receipts.sql"),
            ),
            (
                "012_receipt_cancellation",
                include_str!("../../migrations/012_receipt_cancellation.sql"),
            ),
            (
                "013_shipment_evidence",
                include_str!("../../migrations/013_shipment_evidence.sql"),
            ),
            (
                "014_facility_transfers",
                include_str!("../../migrations/014_facility_transfers.sql"),
            ),
            (
                "015_split_fulfillment",
                include_str!("../../migrations/015_split_fulfillment.sql"),
            ),
            (
                "016_order_remainder_cancellation",
                include_str!("../../migrations/016_order_remainder_cancellation.sql"),
            ),
            (
                "017_lot_controls",
                include_str!("../../migrations/017_lot_controls.sql"),
            ),
            (
                "018_lot_trace",
                include_str!("../../migrations/018_lot_trace.sql"),
            ),
            (
                "019_transfer_loss",
                include_str!("../../migrations/019_transfer_loss.sql"),
            ),
            (
                "020_pack_units",
                include_str!("../../migrations/020_pack_units.sql"),
            ),
            (
                "021_serial_tracking",
                include_str!("../../migrations/021_serial_tracking.sql"),
            ),
            (
                "022_serial_restore_search_path",
                include_str!("../../migrations/022_serial_restore_search_path.sql"),
            ),
            (
                "023_serial_target_resolution",
                include_str!("../../migrations/023_serial_target_resolution.sql"),
            ),
            (
                "024_serial_event_payload_reuse",
                include_str!("../../migrations/024_serial_event_payload_reuse.sql"),
            ),
            (
                "025_serial_resolved_read_sources",
                include_str!("../../migrations/025_serial_resolved_read_sources.sql"),
            ),
            (
                "026_serial_event_batch_index",
                include_str!("../../migrations/026_serial_event_batch_index.sql"),
            ),
            (
                "027_serial_reference_classes",
                include_str!("../../migrations/027_serial_reference_classes.sql"),
            ),
            (
                "028_serial_read_source",
                include_str!("../../migrations/028_serial_read_source.sql"),
            ),
            (
                "029_serial_scope_source_sets",
                include_str!("../../migrations/029_serial_scope_source_sets.sql"),
            ),
            (
                "030_serial_scope_custody_counts",
                include_str!("../../migrations/030_serial_scope_custody_counts.sql"),
            ),
            (
                "031_serial_ascii_validation",
                include_str!("../../migrations/031_serial_ascii_validation.sql"),
            ),
            (
                "032_serial_deferred_check_sets",
                include_str!("../../migrations/032_serial_deferred_check_sets.sql"),
            ),
            (
                "033_serial_target_dispatch",
                include_str!("../../migrations/033_serial_target_dispatch.sql"),
            ),
            (
                "034_serial_command_target_sets",
                include_str!("../../migrations/034_serial_command_target_sets.sql"),
            ),
            (
                "035_serial_sku_scope_checks",
                include_str!("../../migrations/035_serial_sku_scope_checks.sql"),
            ),
            (
                "036_replenishment_rules",
                include_str!("../../migrations/036_replenishment_rules.sql"),
            ),
            (
                "037_carrier_labels",
                include_str!("../../migrations/037_carrier_labels.sql"),
            ),
            (
                "038_gtins_and_bin_capacity",
                include_str!("../../migrations/038_gtins_and_bin_capacity.sql"),
            ),
            (
                "039_reconciliation_runs",
                include_str!("../../migrations/039_reconciliation_runs.sql"),
            ),
            (
                "040_putaway_work",
                include_str!("../../migrations/040_putaway_work.sql"),
            ),
        ];
        // A database migrated by a newer release has ledger rows this binary
        // does not know. Refuse to start before applying anything: an older
        // release would otherwise run without the newer release's rules.
        let known: Vec<&str> = migrations.iter().map(|(name, _)| *name).collect();
        let unknown: Vec<String> = sqlx::query_scalar(
            "SELECT name FROM wms_schema_migrations WHERE NOT (name = ANY($1)) ORDER BY name",
        )
        .bind(&known)
        .fetch_all(&mut *tx)
        .await?;
        if !unknown.is_empty() {
            return Err(Error::new(
                503,
                format!(
                    "Database was migrated by a newer release ({}); deploy that release or restore a pre-upgrade backup",
                    unknown.join(", ")
                ),
            ));
        }
        for (name, sql) in migrations {
            let digest = format!("{:x}", Sha256::digest(sql.as_bytes()));
            let old: Option<String> =
                sqlx::query_scalar("SELECT digest FROM wms_schema_migrations WHERE name=$1")
                    .bind(name)
                    .fetch_optional(&mut *tx)
                    .await?;
            if let Some(old) = old {
                if old != digest {
                    return Err(Error::new(
                        503,
                        format!("Applied migration {name} changed; add a new migration instead"),
                    ));
                }
            } else {
                sqlx::raw_sql(sql).execute(&mut *tx).await?;
                sqlx::query("INSERT INTO wms_schema_migrations(name,digest) VALUES($1,$2)")
                    .bind(name)
                    .bind(digest)
                    .execute(&mut *tx)
                    .await?;
            }
        }
        let legacy = sqlx::query(
            "SELECT tenant,state
            FROM wms_workspaces s
            WHERE NOT EXISTS(
            SELECT 1
            FROM wms_warehouses w
            WHERE w.tenant=s.tenant AND w.warehouse_id=s.warehouse_id) FOR
            UPDATE",
        )
        .fetch_all(&mut *tx)
        .await?;
        for row in legacy {
            let tenant: String = row.try_get("tenant")?;
            let mut s: State = decode(row.try_get("state")?)?;
            s.upgrade_legacy_receipt_evidence()
                .map_err(|e| Error::new(503, format!("Legacy evidence migration held: {e}")))?;
            s.validate()
                .map_err(|e| Error::new(503, format!("Legacy reconciliation failed: {e}")))?;
            Self::save(&mut tx, &tenant, None, &s).await?;
            crate::shipments::initialize(&mut tx, &tenant, &s.warehouse.id).await?;
        }
        tx.commit().await?;
        Ok(Self { pool })
    }
    pub async fn provision(&self, tenant: &str, demo: bool) -> Result<()> {
        let s = if demo {
            domain::demo_state()
        } else {
            State::empty(Warehouse {
                id: "wh-main".into(),
                code: "MAIN".into(),
                name: "Main Warehouse".into(),
                city: "Configure facility before live operations".into(),
                timezone: "UTC".into(),
            })
        };
        let mut tx = self.pool.begin().await?;
        let n = sqlx::query(
            "INSERT INTO wms_warehouses(tenant,warehouse_id,version,metadata)
            VALUES($1,$2,$3,$4)
            ON CONFLICT DO NOTHING",
        )
        .bind(tenant)
        .bind(&s.warehouse.id)
        .bind(s.version)
        .bind(json!(s.warehouse))
        .execute(&mut *tx)
        .await?
        .rows_affected();
        if n == 1 {
            Self::save(&mut tx, tenant, None, &s).await?;
            crate::shipments::initialize(&mut tx, tenant, &s.warehouse.id).await?;
        }
        tx.commit().await?;
        Ok(())
    }
}
