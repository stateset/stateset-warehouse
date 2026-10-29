//! Executing one journaled command: admission, replay, apply, save and outbox.
use super::*;

impl Store {
    pub async fn execute(&self, i: &Identity, w: &str, r: &Request) -> Result<Value> {
        if !r.command.allowed(&i.role) {
            return Err(Error::new(403, "Your role does not permit this action"));
        }
        if !(8..=100).contains(&r.request_id.len())
            || r.request_id
                .chars()
                .any(|c| !c.is_ascii_alphanumeric() && c != '-' && c != '_')
        {
            return Err(Error::new(
                422,
                "Request ID must contain 8–100 letters, digits, hyphens or underscores",
            ));
        }
        let digest = format!(
            "{:x}",
            Sha256::digest(serde_json::to_vec(r).map_err(|_| Error::new(422, "Invalid command"))?)
        );
        // Immutable committed receipts need no warehouse write lock. Recheck
        // under the lock below when absent, so racing first submissions still
        // have one physical effect. Authorization always precedes both lookups.
        if let Some(receipt) = sqlx::query(
            "SELECT c.actor,c.digest,c.result
            FROM wms_commands c
            JOIN wms_warehouses w ON w.tenant=c.tenant AND w.warehouse_id=c.warehouse_id
            WHERE c.tenant=$1 AND c.warehouse_id=$2 AND c.request_id=$3",
        )
        .bind(&i.tenant)
        .bind(w)
        .bind(&r.request_id)
        .fetch_optional(&self.pool)
        .await?
        {
            if receipt.get::<String, _>("digest") != digest
                || receipt.get::<String, _>("actor") != i.actor
            {
                return Err(Error::new(
                    409,
                    "Request ID already belongs to a different payload or operator",
                ));
            }
            return Ok(json!({"replayed":true,"result":receipt.get::<Value,_>("result")}));
        }
        let mut tx = self.pool.begin().await?;
        crate::transfers::lock_destination(&mut tx, &i.tenant, w, &r.command).await?;
        let mut created = None;
        if let Command::CreateWarehouse {
            code,
            name,
            city,
            timezone,
        } = &r.command
        {
            if w.is_empty()
                || w.len() > 80
                || !w.as_bytes()[0].is_ascii_alphanumeric()
                || w.bytes()
                    .any(|c| !c.is_ascii_alphanumeric() && c != b'-' && c != b'_')
                || r.expected_version != 0
                || r.resource_versions.is_some()
            {
                return Err(Error::new(422, "New warehouses require a 1–80 character letter/digit/hyphen/underscore ID, version zero and no resource map"));
            }
            let state = State::empty(Warehouse {
                id: w.into(),
                code: code.clone(),
                name: name.clone(),
                city: city.clone(),
                timezone: timezone.clone(),
            });
            // Check metadata before admission. Invalid work leaves no header,
            // default bins, command receipt or delivery intent behind.
            state
                .execute(&r.command, &i.actor)
                .map_err(|e| Error::new(422, e))?;
            let inserted = sqlx::query("INSERT INTO wms_warehouses(tenant,warehouse_id,version,metadata) VALUES($1,$2,0,$3) ON CONFLICT DO NOTHING")
                .bind(&i.tenant).bind(w).bind(json!(state.warehouse)).execute(&mut *tx).await?.rows_affected();
            if inserted == 1 {
                created = Some(state);
            }
        }
        let h = sqlx::query(
            "SELECT version FROM wms_warehouses WHERE tenant=$1 AND warehouse_id=$2 FOR UPDATE",
        )
        .bind(&i.tenant)
        .bind(w)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(|| Error::new(404, "Warehouse not found"))?;
        if let Some(receipt) = sqlx::query(
            "SELECT actor,digest,result
            FROM wms_commands
            WHERE tenant=$1 AND warehouse_id=$2 AND request_id=$3",
        )
        .bind(&i.tenant)
        .bind(w)
        .bind(&r.request_id)
        .fetch_optional(&mut *tx)
        .await?
        {
            if receipt.get::<String, _>("digest") != digest
                || receipt.get::<String, _>("actor") != i.actor
            {
                return Err(Error::new(
                    409,
                    "Request ID already belongs to a different payload or operator",
                ));
            }
            return Ok(json!({
                "replayed":true,
                "result":receipt.get::<Value,
                _>("result")
            }));
        }
        let version: i64 = h.try_get("version")?;
        if r.resource_versions.is_none() && r.expected_version < version {
            // The lock also fences a delayed copy of this exact request: the
            // warehouse version only increases, so it can never commit later.
            return Err(Error::stale_command(i, w, r, version));
        }
        if r.expected_version < 0
            || r.expected_version > version
            || (r.resource_versions.is_none() && r.expected_version != version)
        {
            return Err(Error::new(409,format!("Warehouse changed (version {version}). Refresh and review before submitting a new request.")));
        }
        if matches!(r.command, Command::CreateWarehouse { .. }) && created.is_none() {
            return Err(Error::new(
                409,
                "Warehouse ID already exists; choose a new facility identity",
            ));
        }
        if let Some(expected) = &r.resource_versions {
            if expected.is_empty() || expected.len() > 2000 || expected.values().any(|v| *v < 0) {
                return Err(Error::new(422, "Invalid resource review"));
            }
            let keys: Vec<_> = expected.keys().cloned().collect();
            let mut actual: std::collections::BTreeMap<_, _> =
                keys.iter().map(|k| (k.clone(), 0_i64)).collect();
            let rows = sqlx::query("SELECT resource,revision FROM wms_resource_versions WHERE tenant=$1 AND warehouse_id=$2 AND resource=ANY($3)")
                .bind(&i.tenant).bind(w).bind(&keys).fetch_all(&mut *tx).await?;
            for row in rows {
                actual.insert(
                    row.try_get::<String, _>("resource")?,
                    row.try_get::<i64, _>("revision")?,
                );
            }
            if *expected != actual {
                let mut error = Error::new(
                    409,
                    "Reviewed stock or work changed. Refresh and review before saving.",
                );
                if expected.iter().all(|(key, value)| actual[key] >= *value) {
                    error.command_outcome = Some(Box::new(CommandOutcome::NotCommitted {
                        reason: NotCommittedReason::ChangedResources,
                        tenant: i.tenant.clone(),
                        actor: i.actor.clone(),
                        warehouse_id: w.into(),
                        request_id: r.request_id.clone(),
                        expected_version: r.expected_version,
                        current_version: version,
                        expected_resource_versions: Some(expected.clone()),
                        current_resource_versions: Some(actual),
                    }));
                }
                return Err(error);
            }
        }
        let package_order =
            crate::split_fulfillment::order_id(&mut tx, &i.tenant, w, &r.command).await?;
        let load_command = package_order.as_ref().map(|order_id| Command::Pack {
            order_id: order_id.clone(),
        });
        let selective = if r.resource_versions.is_some() {
            crate::command_state::load(&mut tx, &i.tenant, w, &r.command).await?
        } else {
            None
        };
        let is_new = created.is_some();
        let mut state = match (created, selective) {
            (Some(state), _) => state,
            (_, Some(state)) => state,
            (_, None) => {
                Self::load(
                    &mut tx,
                    &i.tenant,
                    w,
                    Some(load_command.as_ref().unwrap_or(&r.command)),
                    false,
                )
                .await?
            }
        };
        crate::lot_store::lock(&mut tx, &i.tenant, &state, &r.command).await?;
        // Re-read after cross-facility policy locks; an arrival must observe the
        // source policy that won admission, including an initially absent row.
        crate::lot_store::load(&mut tx, &i.tenant, w, &mut state).await?;
        let shipment_order_id = package_order.or_else(|| match &r.command {
            Command::AllocateAvailable { order_id }
            | Command::ReleaseUnpicked { order_id, .. }
            | Command::CancelRemainder { order_id, .. }
            | Command::PackShipment { order_id, .. }
            | Command::Pick { order_id, .. }
            | Command::Unpick { order_id, .. }
            | Command::CancelOrder { order_id, .. }
            | Command::Allocate { order_id }
            | Command::Pack { order_id }
            | Command::Ship { order_id, .. }
            | Command::ResetAllocation { order_id, .. } => Some(order_id.clone()),
            _ => None,
        });
        if let Some(order_id) = &shipment_order_id {
            if let Some(order) = state.orders.iter().find(|o| o.id == *order_id) {
                let context = crate::split_fulfillment::load(&mut tx, &i.tenant, w, order).await?;
                if !context.ok {
                    return Err(Error::new(
                        409,
                        "Shipment packing or handoff ownership disagrees; writes held",
                    ));
                }
                state.shipment_context = Some(context);
            }
        }
        if let Some(expected) = &r.resource_versions {
            let mut required = crate::concurrency::reviewed_versions(&state, &r.command)
                .map_err(|message| Error::new(422, message))?
                .ok_or_else(|| Error::new(422, "This command requires a warehouse version"))?;
            crate::work_tasks::admit(&mut tx, i, w, r, &mut required).await?;
            if expected != &required {
                return Err(Error::new(
                    422,
                    "Review must include exactly this command's resources",
                ));
            }
        }
        if r.resource_versions.is_none() {
            crate::work_tasks::admit(&mut tx, i, w, r, &mut Default::default()).await?;
        }
        match &r.command {
            Command::CreateOrder {
                reference,
                source_order_id,
                ..
            } => {
                let duplicate:bool=sqlx::query_scalar("SELECT EXISTS(
                    SELECT 1
                    FROM wms_orders
                    WHERE tenant=$1 AND warehouse_id=$2 AND (reference=$3 OR ($4::text IS NOT NULL AND source_order_id=$4)))").bind(&i.tenant).bind(w).bind(reference).bind(source_order_id).fetch_one(&mut *tx).await?;
                if duplicate {
                    return Err(Error::new(
                        409,
                        "Order reference or OMS source already exists, including completed work",
                    ));
                }
            }
            Command::CreateReceipt { reference, .. } | Command::CreateReturn { reference, .. } => {
                let duplicate: bool = sqlx::query_scalar(
                    "SELECT EXISTS(
                    SELECT 1
                    FROM wms_receipts
                    WHERE tenant=$1 AND warehouse_id=$2 AND reference=$3)",
                )
                .bind(&i.tenant)
                .bind(w)
                .bind(reference)
                .fetch_one(&mut *tx)
                .await?;
                if duplicate {
                    return Err(Error::new(
                        409,
                        "Receipt reference already exists, including completed receipts",
                    ));
                }
            }
            _ => {}
        }
        if let Command::CreateReturn {
            order_id, lines, ..
        } = &r.command
        {
            let available = crate::returns::availability(&mut tx, &i.tenant, w, order_id).await?;
            let mut requested = std::collections::BTreeMap::<(&str, &str), i64>::new();
            for line in lines {
                if !(1..=1_000_000).contains(&line.quantity) {
                    return Err(Error::new(422, "Invalid return quantity"));
                }
                *requested
                    .entry((&line.order_line_id, &line.lot))
                    .or_default() += line.quantity;
            }
            for ((line, lot), quantity) in requested {
                let remaining = available
                    .iter()
                    .find(|a| a["order_line_id"] == line && a["lot"] == lot)
                    .and_then(|a| a["remaining"].as_i64())
                    .unwrap_or(0);
                if quantity > remaining {
                    return Err(Error::new(
                        422,
                        "Return exceeds the original line/lot's remaining shipped quantity",
                    ));
                }
            }
        }
        state.transfer_context =
            crate::transfers::reviewed_transfer(&mut tx, &i.tenant, w, &r.command).await?;
        crate::units::admit(&mut tx, &i.tenant, w, r).await?;
        let serials = crate::serial_store::admit(&mut tx, &i.tenant, w, r).await?;
        if i.role == "oms" {
            if let Command::CancelOrder { order_id, .. }
            | Command::CancelRemainder { order_id, .. } = &r.command
            {
                if !state
                    .orders
                    .iter()
                    .any(|o| o.id == *order_id && o.source_order_id.is_some())
                {
                    return Err(Error::new(403, "OMS can cancel only source-linked orders"));
                }
            }
        }
        crate::lot_store::guard_order(
            &mut tx,
            &i.tenant,
            w,
            shipment_order_id.as_deref(),
            &r.command,
        )
        .await?;
        state.validate().map_err(|e| {
            Error::new(
                503,
                format!("Warehouse reconciliation failed; writes held: {e}"),
            )
        })?;
        let admitted_at: String = sqlx::query_scalar(r#"SELECT to_char(clock_timestamp() AT TIME ZONE 'UTC','YYYY-MM-DD"T"HH24:MI:SS.US"Z"')"#)
            .fetch_one(&mut *tx).await?;
        let admitted_at = chrono::DateTime::parse_from_rfc3339(&admitted_at)
            .map_err(|_| Error::new(503, "Database admission clock unavailable"))?
            .with_timezone(&chrono::Utc);
        let (mut next, result) = state
            .execute_at(&r.command, &i.actor, admitted_at)
            .map_err(|e| Error::new(422, e))?;
        crate::lot_store::prepare_arrival(&mut tx, &i.tenant, &mut next, &r.command, &i.actor)
            .await?;
        let eid = domain::id();
        let serial_changes = serials.plan(
            &state,
            &next,
            r,
            result["entity_id"].as_str().unwrap_or_default(),
            &eid,
        )?;
        crate::serial_store::create_mode(&mut tx, &i.tenant, w, &r.command, &eid).await?;
        for movement in &mut next.movements {
            if movement.package_id.is_some() && movement.shipment_event_id.is_none() {
                movement.shipment_event_id = Some(eid.clone());
            }
            if movement.transfer_id.is_some() && movement.transfer_event_id.is_none() {
                movement.transfer_event_id = Some(eid.clone());
            }
        }
        // Forward SQL guards record the upcoming immutable event version, even
        // when package facts are inserted before the operational projection.
        sqlx::query("UPDATE wms_warehouses SET version=$3 WHERE tenant=$1 AND warehouse_id=$2")
            .bind(&i.tenant)
            .bind(w)
            .bind(next.version)
            .execute(&mut *tx)
            .await?;
        crate::lot_store::persist(&mut tx, &i.tenant, &next, &eid).await?;
        crate::split_fulfillment::persist(&mut tx, &i.tenant, &state, &next, &eid).await?;
        crate::transfers::persist(
            &mut tx,
            i,
            w,
            &r.command,
            &next,
            result["entity_id"].as_str().unwrap_or_default(),
            &eid,
        )
        .await?;
        crate::order_cancellation::persist(&mut tx, &i.tenant, w, &state, &next, &eid).await?;
        Self::save(
            &mut tx,
            &i.tenant,
            if is_new { None } else { Some(&state) },
            &next,
        )
        .await?;
        crate::units::persist(&mut tx, &i.tenant, w, r, &next, &eid).await?;
        let serial_evidence = serials
            .persist(&mut tx, i, &next, r, &serial_changes, &eid)
            .await?;
        let order = next.orders.iter().find(|o| {
            shipment_order_id.as_deref() == Some(o.id.as_str()) || result["entity_id"] == o.id
        });
        let previous_stocks: HashMap<_, _> =
            state.stocks.iter().map(|b| (stock_key(b), b)).collect();
        let inventory: Vec<_> = next
            .stocks
            .iter()
            .filter(|b| changed(*b, previous_stocks.get(&stock_key(b)).copied()))
            .map(|b| json!({"stock":b,"version":next.version}))
            .collect();
        let mut event = json!({
            "schema_version":2,
            "id":eid,
            "tenant":i.tenant,
            "warehouse_id":w,
            "version":next.version,
            "type":r.command.kind(),
            "actor":i.actor,
            "occurred_at":next.quality_time().to_rfc3339(),
            "request_id":r.request_id,
            "result":result,
            "order":order,
            "inventory_changes":inventory,
            "command":r.command
        });
        event["previous_order"] =
            json!(order.and_then(|o| state.orders.iter().find(|old| old.id == o.id)));
        event["lot_changes"] = json!(next.lot_changes);
        if let Some(fields) = serial_evidence.as_object() {
            for (key, value) in fields {
                event[key] = value.clone();
            }
        }
        if !r.unit_inputs.is_empty() {
            event["unit_inputs"] = json!(r.unit_inputs);
        }
        if next.unit_pack_change.is_some() {
            event["unit_pack_change"]=sqlx::query_scalar("SELECT to_jsonb(p)-'tenant' FROM wms_unit_pack_changes p WHERE tenant=$1 AND warehouse_id=$2 AND source_event_id=$3")
                .bind(&i.tenant).bind(w).bind(&eid).fetch_one(&mut *tx).await?;
        }
        if matches!(
            r.command,
            Command::RecordTransferLoss { .. } | Command::ReverseTransferLoss { .. }
        ) {
            event["transfer_loss_record"]=sqlx::query_scalar("SELECT to_jsonb(r)-'tenant' FROM wms_transfer_loss_records r WHERE tenant=$1 AND warehouse_id=$2 AND id=$3")
                .bind(&i.tenant).bind(w).bind(result["entity_id"].as_str().unwrap_or_default()).fetch_one(&mut *tx).await?;
        }
        event["lot_decisions"] =
            crate::lot_store::decisions(&mut tx, &i.tenant, w, next.version).await?;
        if matches!(
            r.command,
            Command::DispatchTransfer { .. } | Command::ReceiveTransfer { .. }
        ) {
            event["transfer"] = sqlx::query_scalar("SELECT wms_transfer_document($1,$2)")
                .bind(&i.tenant)
                .bind(result["entity_id"].as_str().unwrap_or_default())
                .fetch_one(&mut *tx)
                .await?;
        }
        if matches!(
            r.command,
            Command::PackShipment { .. }
                | Command::UnpackShipment { .. }
                | Command::HandoffShipment { .. }
        ) {
            event["shipment"] = sqlx::query_scalar("SELECT wms_package_document($1,$2,$3)")
                .bind(&i.tenant)
                .bind(w)
                .bind(result["entity_id"].as_str().unwrap_or_default())
                .fetch_one(&mut *tx)
                .await?;
        }
        if matches!(
            r.command,
            Command::CancelReceipt { .. } | Command::CreateReturn { .. }
        ) {
            event["receipt"] = json!(next
                .receipts
                .iter()
                .find(|receipt| result["entity_id"] == receipt.id));
        }
        sqlx::query(
            "INSERT INTO wms_events(id,tenant,warehouse_id,version,payload) VALUES($1,$2,$3,$4,$5)",
        )
        .bind(&eid)
        .bind(&i.tenant)
        .bind(w)
        .bind(next.version)
        .bind(event)
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "INSERT INTO wms_commands(tenant,warehouse_id,request_id,actor,digest,result)
            VALUES($1,$2,$3,$4,$5,$6)",
        )
        .bind(&i.tenant)
        .bind(w)
        .bind(&r.request_id)
        .bind(&i.actor)
        .bind(digest)
        .bind(&result)
        .execute(&mut *tx)
        .await?;
        let destinations = if matches!(
            r.command,
            Command::Pack { .. } | Command::Ship { .. } | Command::HandoffShipment { .. }
        ) {
            vec!["oms", "tms"]
        } else {
            vec!["oms"]
        };
        for d in destinations {
            sqlx::query(
                "INSERT INTO wms_outbox(id,event_id,tenant,destination) VALUES($1,$2,$3,$4)",
            )
            .bind(domain::id())
            .bind(&eid)
            .bind(&i.tenant)
            .bind(d)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(json!({"replayed":false,"result":result}))
    }
}
