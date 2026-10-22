//! Serial admission runs after the warehouse/policy locks and before identity
//! reads. It never owns a transaction or commits outside the command journal.
use crate::{
    domain::{Command, State},
    persistence::decode,
    serials::{self, Context, Unit, MAX_OBSERVATIONS},
    store::{Error, Identity, Request, Result},
};
use serde_json::{json, Value};
use sqlx::{PgConnection, Row};

// Each bounded selection shares its complete original event proof within this
// statement's snapshot. Retain the exact latest projection/step join separately;
// missing events fail closed. Callers supply only static SQL, never user text.
fn selected_sources_sql(selection: &str) -> String {
    format!(
        r#"WITH selected AS MATERIALIZED ({selection}),
    events AS MATERIALIZED (
      SELECT e.id,e.warehouse_id,wms_serial_event_valid_read_source(e.tenant,e.warehouse_id,e.id) AS ok
      FROM wms_events e JOIN (SELECT DISTINCT last_event_id FROM selected) k ON k.last_event_id=e.id
      WHERE e.tenant=$1
    ) SELECT u.sku,u.serial,u.lot,u.place,u.revision,
      COALESCE(e.ok,false) AND EXISTS(
        SELECT 1 FROM wms_serial_steps s WHERE s.tenant=u.tenant AND s.sku=u.sku AND s.serial=u.serial
        AND s.lot=u.lot AND s.revision=u.revision AND s.event_id=u.last_event_id
        AND s.after_place=u.place AND s.warehouse_id=e.warehouse_id
      ) AND NOT EXISTS(
        SELECT 1 FROM wms_serial_steps later WHERE later.tenant=u.tenant AND later.sku=u.sku
        AND later.serial=u.serial AND later.revision>u.revision
      ) AS source_ok
    FROM selected u LEFT JOIN events e ON e.id=u.last_event_id ORDER BY u.sku,u.serial"#
    )
}
use std::collections::BTreeMap;

#[derive(Default)]
pub(crate) struct Prepared {
    pub context: Context,
    heads: BTreeMap<String, i64>,
    revisions: BTreeMap<(String, String), i64>,
}

/// Existing mode rows are touched even on agreement, fencing concurrent catalog
/// creation. A second facility retains the first source and current SKU head.
pub(crate) async fn create_mode(
    c: &mut PgConnection,
    t: &str,
    w: &str,
    command: &Command,
    eid: &str,
) -> Result<()> {
    let Command::CreateProduct {
        sku,
        serial_controlled,
        ..
    } = command
    else {
        return Ok(());
    };
    let actual: bool = sqlx::query_scalar("INSERT INTO wms_serial_modes(tenant,sku,controlled,source_warehouse_id,source_event_id) VALUES($1,$2,$3,CASE WHEN $3 THEN $4 END,CASE WHEN $3 THEN $5 END) ON CONFLICT(tenant,sku) DO UPDATE SET sku=EXCLUDED.sku RETURNING controlled")
        .bind(t).bind(sku).bind(serial_controlled).bind(w).bind(eid).fetch_one(&mut *c).await?;
    if actual != *serial_controlled {
        return Err(Error::new(422, "Serial mode must match this tenant/SKU's original catalog choice; existing inventory cannot be relabelled"));
    }
    if actual {
        sqlx::query(
            "INSERT INTO wms_serial_heads VALUES($1,$2,0,NULL) ON CONFLICT(tenant,sku) DO NOTHING",
        )
        .bind(t)
        .bind(sku)
        .execute(c)
        .await?;
    }
    Ok(())
}

pub(crate) async fn admit(
    c: &mut PgConnection,
    t: &str,
    w: &str,
    request: &Request,
) -> Result<Prepared> {
    serials::validate_inputs(&request.serial_inputs).map_err(|e| Error::new(422, e))?;
    let command = json!(&request.command);
    let skus: Vec<String> =
        sqlx::query_scalar("SELECT sku FROM wms_serial_event_skus($1,$2,$3) ORDER BY sku")
            .bind(t)
            .bind(w)
            .bind(&command)
            .fetch_all(&mut *c)
            .await?;
    if skus.is_empty() {
        if !request.serial_inputs.is_empty() {
            return Err(Error::new(422,"Serial observations belong only to controlled custody quantities, not untracked stock or metadata plans"));
        }
        return Ok(Prepared::default());
    }
    let valid: bool = sqlx::query_scalar("SELECT wms_serial_inputs_match_command($1,$2,$3,$4)")
        .bind(t)
        .bind(w)
        .bind(&command)
        .bind(json!(&request.serial_inputs))
        .fetch_one(&mut *c)
        .await?;
    if !valid {
        return Err(Error::new(422,"Scan the complete exact serial set for every controlled command target; targets and literal IDs must be unique"));
    }
    let rows=sqlx::query("SELECT h.sku,h.version FROM wms_serial_heads h WHERE h.tenant=$1 AND h.sku=ANY($2) ORDER BY h.sku FOR UPDATE")
        .bind(t).bind(&skus).fetch_all(&mut *c).await?;
    // READ COMMITTED must validate in a fresh statement after any lock wait;
    // a function in the locking SELECT could see an earlier batch snapshot.
    let valid: bool=sqlx::query_scalar("SELECT NOT EXISTS(SELECT 1 FROM wms_serial_heads h WHERE h.tenant=$1 AND h.sku=ANY($2) AND NOT wms_serial_head_bound(h))")
        .bind(t).bind(&skus).fetch_one(&mut *c).await?;
    if rows.len() != skus.len() || !valid {
        return Err(Error::new(
            503,
            "Original global serial review disagrees; identity writes held",
        ));
    }
    let mut prepared = Prepared::default();
    for row in rows {
        prepared
            .heads
            .insert(row.try_get("sku")?, row.try_get("version")?);
    }
    prepared.context.tracked_skus = skus.iter().cloned().collect();
    // A PK join keeps ordinary requested-unit reads bounded. Derived closures
    // and full counts use a separate indexed predicate and reject excess rows;
    // neither a partial lookup nor an empty input can prove a zero balance.
    let keys: Vec<_> = request
        .serial_inputs
        .iter()
        .flat_map(|i| {
            i.serials
                .iter()
                .map(|uid| json!({"sku":i.sku,"serial":uid}))
        })
        .collect();
    let mut selected = BTreeMap::new();
    if !keys.is_empty() {
        // A missing projection is not a new identity when retained original
        // observations/history already name it. Keep these bounded point
        // checks under the global SKU locks; do not manufacture a new birth.
        let missing: bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM jsonb_to_recordset($2) AS k(sku TEXT,serial TEXT) WHERE NOT EXISTS(SELECT 1 FROM wms_serial_units u WHERE u.tenant=$1 AND u.sku=k.sku AND u.serial=k.serial) AND (EXISTS(SELECT 1 FROM wms_serial_steps s WHERE s.tenant=$1 AND s.sku=k.sku AND s.serial=k.serial) OR EXISTS(SELECT 1 FROM wms_serial_observations o WHERE o.tenant=$1 AND o.sku=k.sku AND o.serials @> jsonb_build_array(k.serial))))")
            .bind(t).bind(json!(keys)).fetch_one(&mut *c).await?;
        if missing {
            return Err(Error::new(
                503,
                "Original serial evidence lacks its current projection; identity writes held",
            ));
        }
        let sql = selected_sources_sql("SELECT u.* FROM jsonb_to_recordset($2) AS k(sku TEXT,serial TEXT) JOIN wms_serial_units u ON u.tenant=$1 AND u.sku=k.sku AND u.serial=k.serial");
        let rows = sqlx::query(&sql)
            .bind(t)
            .bind(json!(keys))
            .fetch_all(&mut *c)
            .await?;
        for row in rows {
            selected.insert(
                (row.get::<String, _>("sku"), row.get::<String, _>("serial")),
                row,
            );
        }
    }
    let predicate=match &request.command {
        Command::CountStock { .. } => Some("place->>'status'='on_hand' AND sku=$4->>'sku' AND lot=$4->>'lot' AND place->>'location'=$4->>'location'"),
        Command::Ship { .. } => Some("place->>'status'='staged' AND place->>'order_id'=$4->>'order_id' AND place->'package_id'='null'::jsonb"),
        Command::UnpackShipment { .. } | Command::HandoffShipment { .. } => Some("place->>'status'='staged' AND place->>'package_id'=$4->>'shipment_id'"),
        Command::CancelReceipt { .. } => Some("place->>'status'='shipped' AND place->>'return_receipt_id'=$4->>'receipt_id'"),
        _ => None,
    };
    if let Some(predicate) = predicate {
        let sql = selected_sources_sql(&format!("SELECT * FROM wms_serial_units WHERE tenant=$1 AND sku=ANY($2) AND place->>'warehouse_id'=$3 AND ({predicate}) ORDER BY sku,serial LIMIT 10001"));
        let rows = sqlx::query(&sql)
            .bind(t)
            .bind(&skus)
            .bind(w)
            .bind(&command)
            .fetch_all(&mut *c)
            .await?;
        for row in rows {
            selected.insert(
                (row.get::<String, _>("sku"), row.get::<String, _>("serial")),
                row,
            );
        }
    }
    if selected.len() > MAX_OBSERVATIONS {
        return Err(Error::new(422,"Complete serial scope exceeds one command. Smaller original packages are supported; large full counts require reviewed count sessions, which are not available yet. No identities were truncated"));
    }
    for ((sku, serial), row) in selected {
        if !row.try_get::<bool, _>("source_ok")? {
            return Err(Error::new(
                503,
                "Original serial projection disagrees with its immutable source; writes held",
            ));
        }
        prepared
            .revisions
            .insert((sku.clone(), serial.clone()), row.try_get("revision")?);
        prepared.context.units.push(Unit {
            sku,
            serial,
            lot: row.try_get("lot")?,
            place: decode(row.try_get("place")?)?,
        });
    }
    let groups=sqlx::query("SELECT sku,lot,place->>'status' AS status,place->>'location' AS location,place->>'order_id' AS order_id,place->>'line_id' AS line_id,place->>'allocation_location' AS allocation_location,place->>'package_id' AS package_id,count(*)::bigint AS quantity FROM wms_serial_units WHERE tenant=$1 AND sku=ANY($2) AND place->>'warehouse_id'=$3 AND place->>'status' IN('on_hand','staged') GROUP BY sku,lot,place->>'status',place->>'location',place->>'order_id',place->>'line_id',place->>'allocation_location',place->>'package_id'")
        .bind(t).bind(&skus).bind(w).fetch_all(c).await?;
    for row in groups {
        let sku: String = row.try_get("sku")?;
        let lot: String = row.try_get("lot")?;
        let n: i64 = row.try_get("quantity")?;
        let staged = row.try_get::<String, _>("status")? == "staged";
        let bin = if staged {
            "STAGE-01".to_owned()
        } else {
            row.try_get("location")?
        };
        add(
            &mut prepared.context.physical,
            (w.into(), sku.clone(), lot.clone(), bin),
            n,
        )?;
        if staged {
            let order: String = row.try_get("order_id")?;
            let line: String = row.try_get("line_id")?;
            let location: String = row.try_get("allocation_location")?;
            add(
                &mut prepared.context.staged,
                (
                    w.into(),
                    order,
                    line.clone(),
                    location.clone(),
                    sku.clone(),
                    lot.clone(),
                ),
                n,
            )?;
            if let Some(package) = row.try_get::<Option<String>, _>("package_id")? {
                add(
                    &mut prepared.context.packed,
                    (w.into(), package, line, location, sku, lot),
                    n,
                )?;
            }
        }
    }
    Ok(prepared)
}

fn add<K: Ord>(map: &mut BTreeMap<K, i64>, key: K, n: i64) -> Result<()> {
    let value = map.entry(key).or_default();
    *value = value
        .checked_add(n)
        .ok_or_else(|| Error::new(503, "Serial custody count exceeds exact range"))?;
    Ok(())
}

impl Prepared {
    pub(crate) fn plan(
        &self,
        before: &State,
        after: &State,
        request: &Request,
        entity: &str,
        eid: &str,
    ) -> Result<Vec<serials::Change>> {
        if self.heads.is_empty() {
            return Ok(vec![]);
        }
        serials::validate_context(before, &self.context).map_err(|e| Error::new(503, e))?;
        serials::plan(
            before,
            after,
            &request.command,
            &request.serial_inputs,
            &self.context,
            entity,
            eid,
        )
        .map_err(|e| Error::new(422, e))
    }

    /// Bulk writes retain original observation order and database row shapes in
    /// the immutable event. Every write remains in the caller's transaction.
    pub(crate) async fn persist(
        &self,
        c: &mut PgConnection,
        identity: &Identity,
        after: &State,
        request: &Request,
        changes: &[serials::Change],
        eid: &str,
    ) -> Result<Value> {
        if self.heads.is_empty() {
            return Ok(json!({}));
        }
        let t = &identity.tenant;
        let actor = &identity.actor;
        let w = &after.warehouse.id;
        let heads: Vec<_> = self
            .heads
            .iter()
            .map(|(sku, version)| json!({"sku":sku,"version":version}))
            .collect();
        let updated=sqlx::query("UPDATE wms_serial_heads h SET version=h.version+1,last_event_id=$3 FROM jsonb_to_recordset($2) AS k(sku TEXT,version BIGINT) WHERE h.tenant=$1 AND h.sku=k.sku AND h.version=k.version")
            .bind(t).bind(json!(&heads)).bind(eid).execute(&mut *c).await?.rows_affected();
        if updated != self.heads.len() as u64 {
            return Err(Error::new(
                503,
                "Global serial review changed during admission; retry the original request",
            ));
        }
        sqlx::query("INSERT INTO wms_serial_batches SELECT $1,k.sku,k.version+1,$3,$4,$5 FROM jsonb_to_recordset($2) AS k(sku TEXT,version BIGINT) ORDER BY k.sku")
            .bind(t).bind(json!(&heads)).bind(w).bind(after.version).bind(eid).execute(&mut *c).await?;
        let mut steps = Vec::new();
        for (index, change) in changes.iter().enumerate() {
            let old = self
                .revisions
                .get(&(change.sku.clone(), change.serial.clone()))
                .copied();
            if old.is_some() != change.before.is_some() {
                return Err(Error::new(
                    503,
                    "Original serial revision unavailable; writes held",
                ));
            }
            let version = self
                .heads
                .get(&change.sku)
                .ok_or_else(|| Error::new(503, "Serial change exceeds its admitted SKU scope"))?;
            steps.push(json!({"sku":change.sku,"serial":change.serial,"lot":change.lot,"revision":old.unwrap_or(0)+1,"previous_revision":old,"sku_version":version+1,"warehouse_id":w,"warehouse_version":after.version,"event_id":eid,"ordinal":index+1,"target":change.target,"before_place":change.before,"after_place":change.after,"actor":actor,"recorded_at":after.quality_time().to_rfc3339()}));
        }
        if !steps.is_empty() {
            let n=sqlx::query("INSERT INTO wms_serial_units AS u(tenant,sku,serial,lot,revision,place,last_event_id) SELECT $1,k.sku,k.serial,k.lot,k.revision,k.after_place,$3 FROM jsonb_to_recordset($2) AS k(sku TEXT,serial TEXT,lot TEXT,revision BIGINT,after_place JSONB) ORDER BY k.sku,k.serial ON CONFLICT(tenant,sku,serial) DO UPDATE SET revision=EXCLUDED.revision,place=EXCLUDED.place,last_event_id=EXCLUDED.last_event_id WHERE u.revision=EXCLUDED.revision-1 AND u.lot=EXCLUDED.lot")
                .bind(t).bind(json!(&steps)).bind(eid).execute(&mut *c).await?.rows_affected();
            if n != steps.len() as u64 {
                return Err(Error::new(503,"Original serial revision changed during persistence; retry the original request"));
            }
            sqlx::query("INSERT INTO wms_serial_steps SELECT $1,k.* FROM jsonb_to_recordset($2) AS k(sku TEXT,serial TEXT,lot TEXT,revision BIGINT,previous_revision BIGINT,sku_version BIGINT,warehouse_id TEXT,warehouse_version BIGINT,event_id TEXT,ordinal INTEGER,target JSONB,before_place JSONB,after_place JSONB,actor TEXT,recorded_at TEXT) ORDER BY k.ordinal")
                .bind(t).bind(json!(&steps)).execute(&mut *c).await?;
        }
        let inputs: Vec<_>=request.serial_inputs.iter().enumerate().map(|(i,input)|json!({"ordinal":i+1,"sku":input.sku,"target":input.target,"serials":input.serials,"sku_version":self.heads[&input.sku]+1})).collect();
        if !inputs.is_empty() {
            sqlx::query("INSERT INTO wms_serial_observations SELECT $1,$2,$3,k.ordinal,k.sku,k.sku_version,k.target,k.serials FROM jsonb_to_recordset($4) AS k(ordinal INTEGER,sku TEXT,sku_version BIGINT,target JSONB,serials JSONB) ORDER BY k.ordinal")
                .bind(t).bind(w).bind(eid).bind(json!(inputs)).execute(c).await?;
        }
        let heads: BTreeMap<_, _> = self
            .heads
            .iter()
            .map(|(sku, version)| (sku, version + 1))
            .collect();
        Ok(json!({"serial_heads":heads,"serial_steps":steps,"serial_inputs":request.serial_inputs}))
    }
}
