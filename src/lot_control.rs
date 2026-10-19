//! Lot availability is separate from physical custody and reservation ownership.
//! Expiry is an exact instant; a hold never silently moves or releases stock.
use crate::domain::{self, Command, State};
use chrono::{DateTime, Datelike, Utc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Hold {
    pub id: String,
    pub reason: String,
    pub actor: String,
    pub quarantined_at: String,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Lot {
    pub sku: String,
    pub lot: String,
    pub expiry_recorded: bool,
    pub expires_at: Option<String>,
    pub holds: Vec<Hold>,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub(crate) struct Arrival {
    pub transfer_id: String,
    pub line_id: String,
    pub source_warehouse_id: String,
    pub source_version: i64,
    pub source: Lot,
    pub source_required: bool,
    pub dispatched: Option<Lot>,
    pub expiry_recorded: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub(crate) struct Change {
    pub kind: String,
    pub sku: String,
    pub lot: Option<String>,
    pub required: Option<bool>,
    pub expires_at: Option<String>,
    pub hold_id: Option<String>,
    pub reason: String,
    pub actor: String,
    pub recorded_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arrival: Option<Arrival>,
}
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Availability {
    Available,
    Quarantined,
    Expired,
    ExpiryMissing,
    Invalid,
}

pub(crate) fn text(value: &str) -> bool {
    !value.trim().is_empty()
        && value.trim() == value
        && value.len() <= 240
        && !value.chars().any(char::is_control)
}
fn expiry(value: &str) -> domain::Result<DateTime<Utc>> {
    let time = DateTime::parse_from_rfc3339(value)
        .map_err(|_| "Expiry must be an exact RFC3339 time including its UTC offset")?
        .with_timezone(&Utc);
    if !(1..=9999).contains(&time.year()) {
        return Err("Expiry year must be between 0001 and 9999".into());
    }
    if time.timestamp_subsec_nanos() >= 1_000_000_000 || time.timestamp_subsec_nanos() % 1000 != 0 {
        return Err("Expiry supports microsecond precision and ordinary seconds; supply an exact non-leap-second time".into());
    }
    Ok(time)
}

pub fn availability_at(state: &State, sku: &str, lot: &str, at: DateTime<Utc>) -> Availability {
    let Some(product) = state.products.iter().find(|p| p.sku == sku) else {
        return Availability::Invalid;
    };
    let control = state
        .lot_controls
        .iter()
        .find(|c| c.sku == sku && c.lot == lot);
    if control.is_some_and(|c| !c.holds.is_empty()) {
        return Availability::Quarantined;
    }
    if let Some(value) = control.and_then(|c| c.expires_at.as_deref()) {
        return match expiry(value) {
            Ok(value) if at >= value => Availability::Expired,
            Ok(_) => Availability::Available,
            Err(_) => Availability::Invalid,
        };
    }
    if product.expiry_required {
        Availability::ExpiryMissing
    } else {
        Availability::Available
    }
}
pub(crate) fn eligible(state: &State, sku: &str, lot: &str) -> bool {
    availability_at(state, sku, lot, state.quality_time()) == Availability::Available
}
pub(crate) fn require(state: &State, sku: &str, lot: &str) -> domain::Result<()> {
    let status = availability_at(state, sku, lot, state.quality_time());
    if status != Availability::Available {
        return Err(format!(
            "Lot {sku} / {lot} is unavailable for new fulfillment: {status:?}"
        ));
    }
    Ok(())
}
pub(crate) fn fefo(state: &State, sku: &str, lot: &str) -> (bool, Option<DateTime<Utc>>) {
    let value = state
        .lot_controls
        .iter()
        .find(|c| c.sku == sku && c.lot == lot)
        .and_then(|c| c.expires_at.as_deref())
        .and_then(|v| expiry(v).ok());
    (value.is_none(), value)
}
pub fn validate(state: &State) -> domain::Result<()> {
    let products: BTreeSet<_> = state.products.iter().map(|p| p.sku.as_str()).collect();
    let mut lots = BTreeSet::new();
    let mut holds = BTreeSet::new();
    for control in &state.lot_controls {
        if !text(&control.sku)
            || !text(&control.lot)
            || !products.contains(control.sku.as_str())
            || !lots.insert((&control.sku, &control.lot))
            || (!control.expiry_recorded && control.expires_at.is_some())
        {
            return Err("Invalid or duplicate lot-control identity".into());
        }
        if let Some(value) = &control.expires_at {
            expiry(value)?;
        }
        for hold in &control.holds {
            if !text(&hold.id)
                || !text(&hold.actor)
                || !text(&hold.reason)
                || !holds.insert(&hold.id)
                || DateTime::parse_from_rfc3339(&hold.quarantined_at).is_err()
            {
                return Err("Invalid original lot quarantine evidence".into());
            }
        }
    }
    Ok(())
}
fn lot_index(state: &mut State, sku: &str, lot: &str) -> domain::Result<usize> {
    if !text(sku) || !text(lot) || !state.products.iter().any(|p| p.sku == sku) {
        return Err("Lot controls require an existing SKU and exact lot identity".into());
    }
    if let Some(index) = state
        .lot_controls
        .iter()
        .position(|c| c.sku == sku && c.lot == lot)
    {
        return Ok(index);
    }
    state.lot_controls.push(Lot {
        sku: sku.into(),
        lot: lot.into(),
        expiry_recorded: false,
        expires_at: None,
        holds: vec![],
    });
    Ok(state.lot_controls.len() - 1)
}
pub(crate) fn apply(state: &mut State, command: &Command, actor: &str) -> domain::Result<String> {
    let at = state.quality_time().to_rfc3339();
    let reason = match command {
        Command::SetProductExpiryPolicy { reason, .. }
        | Command::RecordLotExpiry { reason, .. }
        | Command::CorrectLotExpiry { reason, .. }
        | Command::QuarantineLot { reason, .. }
        | Command::ReleaseLotHold { reason, .. } => reason,
        _ => return Err("Unsupported lot-control command".into()),
    };
    if !text(reason) || !text(actor) {
        return Err(
            "Lot controls require an attributable actor and a 1–240 character reason".into(),
        );
    }
    let mut change = Change {
        kind: command.kind().into(),
        sku: String::new(),
        lot: None,
        required: None,
        expires_at: None,
        hold_id: None,
        reason: reason.clone(),
        actor: actor.into(),
        recorded_at: at.clone(),
        arrival: None,
    };
    let result = match command {
        Command::SetProductExpiryPolicy { sku, required, .. } => {
            let product = state
                .products
                .iter_mut()
                .find(|p| p.sku == *sku)
                .ok_or("Product not found")?;
            if product.expiry_required == *required {
                return Err("Product expiry requirement is already set to this value".into());
            }
            product.expiry_required = *required;
            change.sku = sku.clone();
            change.required = Some(*required);
            sku.clone()
        }
        Command::RecordLotExpiry {
            sku,
            lot,
            expires_at,
            ..
        } => {
            let expires = expiry(expires_at)?.to_rfc3339();
            let index = lot_index(state, sku, lot)?;
            if state.lot_controls[index].expiry_recorded {
                return Err("The original lot expiry is already recorded; a supervisor must correct it explicitly".into());
            }
            state.lot_controls[index].expiry_recorded = true;
            state.lot_controls[index].expires_at = Some(expires.clone());
            change.sku = sku.clone();
            change.lot = Some(lot.clone());
            change.expires_at = Some(expires);
            lot.clone()
        }
        Command::CorrectLotExpiry {
            sku,
            lot,
            expires_at,
            ..
        } => {
            let expires = expires_at
                .as_deref()
                .map(expiry)
                .transpose()?
                .map(|v| v.to_rfc3339());
            let index = lot_index(state, sku, lot)?;
            if !state.lot_controls[index].expiry_recorded {
                return Err("Record the original expiry before correcting it".into());
            }
            if state.lot_controls[index].expires_at == expires {
                return Err("Corrected expiry must differ from the current recorded value".into());
            }
            state.lot_controls[index].expires_at = expires.clone();
            change.sku = sku.clone();
            change.lot = Some(lot.clone());
            change.expires_at = expires;
            lot.clone()
        }
        Command::QuarantineLot { sku, lot, .. } => {
            let index = lot_index(state, sku, lot)?;
            let id = domain::id();
            state.lot_controls[index].holds.push(Hold {
                id: id.clone(),
                reason: reason.clone(),
                actor: actor.into(),
                quarantined_at: at,
            });
            change.sku = sku.clone();
            change.lot = Some(lot.clone());
            change.hold_id = Some(id.clone());
            id
        }
        Command::ReleaseLotHold { hold_id, .. } => {
            let index = state
                .lot_controls
                .iter()
                .position(|c| c.holds.iter().any(|h| h.id == *hold_id))
                .ok_or("Original active lot hold not found")?;
            let control = &mut state.lot_controls[index];
            control.holds.retain(|h| h.id != *hold_id);
            change.sku = control.sku.clone();
            change.lot = Some(control.lot.clone());
            change.hold_id = Some(hold_id.clone());
            hold_id.clone()
        }
        _ => return Err("Unsupported lot-control command".into()),
    };
    state.lot_changes.push(change);
    Ok(result)
}

/// The arrival records custody even when its policy is unavailable. Most
/// restrictive known expiry wins; conflicting labels create a separate hold.
pub(crate) fn merge_arrival(
    state: &mut State,
    mut arrival: Arrival,
    actor: &str,
) -> domain::Result<()> {
    let source = &arrival.source;
    let index = lot_index(state, &source.sku, &source.lot)?;
    let destination = &state.lot_controls[index];
    let mut dates = BTreeSet::new();
    for lot in [Some(destination), Some(source), arrival.dispatched.as_ref()]
        .into_iter()
        .flatten()
    {
        if let Some(value) = &lot.expires_at {
            dates.insert(expiry(value)?);
        }
    }
    let expires_at = dates.first().map(|v| v.to_rfc3339());
    let hold_needed = dates.len() > 1
        || !source.holds.is_empty()
        || (arrival.source_required && source.expires_at.is_none());
    arrival.expiry_recorded = destination.expiry_recorded
        || source.expiry_recorded
        || arrival
            .dispatched
            .as_ref()
            .is_some_and(|l| l.expiry_recorded);
    let at = state.quality_time().to_rfc3339();
    let reason = "Transfer arrival retains original and current lot restrictions".to_string();
    let hold_id = hold_needed.then(domain::id);
    let target = &mut state.lot_controls[index];
    target.expiry_recorded = arrival.expiry_recorded;
    target.expires_at = expires_at.clone();
    if let Some(id) = &hold_id {
        target.holds.push(Hold {
            id: id.clone(),
            reason: reason.clone(),
            actor: actor.into(),
            quarantined_at: at.clone(),
        });
    }
    state.lot_changes.push(Change {
        kind: "lot.arrival_restricted".into(),
        sku: source.sku.clone(),
        lot: Some(source.lot.clone()),
        required: None,
        expires_at,
        hold_id,
        reason,
        actor: actor.into(),
        recorded_at: at,
        arrival: Some(arrival),
    });
    validate(state)
}
