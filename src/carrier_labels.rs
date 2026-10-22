//! Carrier labels for packed packages, supplied by the TMS or entered by an
//! operator from a carrier system. A label records the actual carrier,
//! service, tracking number, quoted rate and observed carton, and a printable
//! label (stored ZPL or an HTTPS URL). Recording a label moves no inventory
//! and does not hand off the package; that stays the reviewed
//! `handoff_shipment` command, which can use the tracking number as its
//! handoff reference.
use crate::{
    api::App,
    domain,
    store::{Error, Identity},
};
use axum::{
    extract::{Path, State as AppState},
    Extension, Json,
};
use serde::Deserialize;
use serde_json::{json, Value};
use sqlx::Row;

#[derive(Debug, Clone, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Rate {
    /// Exact decimal string, e.g. "12.34". Never a JSON number.
    pub amount: String,
    /// ISO 4217 code, e.g. "USD".
    pub currency: String,
}

#[derive(Debug, Clone, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Carton {
    pub weight_grams: Option<i64>,
    pub length_mm: Option<i64>,
    pub width_mm: Option<i64>,
    pub height_mm: Option<i64>,
}

#[derive(Debug, Clone, Deserialize, schemars::JsonSchema)]
#[serde(tag = "format", rename_all = "snake_case", deny_unknown_fields)]
pub enum Label {
    /// Printer-ready ZPL from the carrier, starting with ^XA (≤ 64 KiB).
    Zpl { data: String },
    /// An HTTPS link to the carrier's label (for example a PDF).
    Url { url: String },
}

#[derive(Debug, Clone, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RecordRequest {
    pub carrier: String,
    pub service: String,
    pub tracking_number: String,
    #[serde(default)]
    pub rate: Option<Rate>,
    #[serde(default)]
    pub carton: Option<Carton>,
    pub label: Label,
}

#[derive(Debug, Clone, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct VoidRequest {
    pub reason: String,
}

fn bounded(value: &str, what: &str, max: usize) -> Result<(), Error> {
    if value.trim().is_empty() || value.trim() != value || value.chars().count() > max {
        return Err(Error::new(
            422,
            format!("{what} must be 1–{max} characters without surrounding spaces"),
        ));
    }
    Ok(())
}

pub fn validate(input: &RecordRequest) -> Result<(), Error> {
    bounded(&input.carrier, "Carrier", 120)?;
    bounded(&input.service, "Service", 120)?;
    let t = &input.tracking_number;
    if t.is_empty() || t.len() > 64 || !t.bytes().all(|b| (0x21..=0x7e).contains(&b)) {
        return Err(Error::new(
            422,
            "Tracking number must be 1–64 printable ASCII characters without spaces",
        ));
    }
    if let Some(rate) = &input.rate {
        let ok = regex_lite_decimal(&rate.amount)
            && rate.currency.len() == 3
            && rate.currency.bytes().all(|b| b.is_ascii_uppercase());
        if !ok {
            return Err(Error::new(
                422,
                "Rate needs an exact decimal amount string (up to 8 decimals) and a three-letter currency",
            ));
        }
    }
    if let Some(c) = &input.carton {
        let ranges = [
            (c.weight_grams, 1_000_000_000, "weight_grams"),
            (c.length_mm, 100_000, "length_mm"),
            (c.width_mm, 100_000, "width_mm"),
            (c.height_mm, 100_000, "height_mm"),
        ];
        for (value, max, name) in ranges {
            if value.is_some_and(|v| !(1..=max).contains(&v)) {
                return Err(Error::new(422, format!("{name} must be 1–{max}")));
            }
        }
    }
    match &input.label {
        Label::Zpl { data } if !data.starts_with("^XA") || data.len() > 65_536 => Err(Error::new(
            422,
            "ZPL labels must start with ^XA and be at most 64 KiB",
        )),
        Label::Url { url } if !url.starts_with("https://") || url.len() > 2048 => Err(Error::new(
            422,
            "Label URLs must use HTTPS and be at most 2,048 characters",
        )),
        _ => Ok(()),
    }
}

/// `0` or a positive integer without leading zeros, with up to 8 decimals.
fn regex_lite_decimal(value: &str) -> bool {
    let (whole, fraction) = match value.split_once('.') {
        Some((w, f)) => (w, Some(f)),
        None => (value, None),
    };
    let whole_ok = !whole.is_empty()
        && whole.len() <= 12
        && whole.bytes().all(|b| b.is_ascii_digit())
        && (whole == "0" || !whole.starts_with('0'));
    let fraction_ok = fraction
        .is_none_or(|f| !f.is_empty() && f.len() <= 8 && f.bytes().all(|b| b.is_ascii_digit()));
    whole_ok && fraction_ok
}

fn row_json(r: &sqlx::postgres::PgRow) -> Value {
    json!({
        "id": r.get::<String,_>("id"),
        "package_id": r.get::<String,_>("package_id"),
        "carrier": r.get::<String,_>("carrier"),
        "service": r.get::<String,_>("service"),
        "tracking_number": r.get::<String,_>("tracking_number"),
        "rate": r.get::<Option<String>,_>("rate_amount").map(|amount| json!({
            "amount": amount, "currency": r.get::<Option<String>,_>("rate_currency")})),
        "carton": {
            "weight_grams": r.get::<Option<i64>,_>("weight_grams"),
            "length_mm": r.get::<Option<i64>,_>("length_mm"),
            "width_mm": r.get::<Option<i64>,_>("width_mm"),
            "height_mm": r.get::<Option<i64>,_>("height_mm"),
        },
        "label_format": r.get::<String,_>("label_format"),
        "label_url": r.get::<Option<String>,_>("label_url"),
        "recorded_by": r.get::<String,_>("recorded_by"),
        "recorded_at": r.get::<String,_>("recorded_at"),
        "voided": r.get::<Option<String>,_>("voided_at").map(|at| json!({
            "at": at,
            "by": r.get::<Option<String>,_>("voided_by"),
            "reason": r.get::<Option<String>,_>("void_reason")})),
    })
}

const COLUMNS: &str = "id,package_id,carrier,service,tracking_number,rate_amount,rate_currency,weight_grams,length_mm,width_mm,height_mm,label_format,label_url,recorded_by,to_char(recorded_at AT TIME ZONE 'UTC','YYYY-MM-DD\"T\"HH24:MI:SS.US\"Z\"') AS recorded_at,voided_by,to_char(voided_at AT TIME ZONE 'UTC','YYYY-MM-DD\"T\"HH24:MI:SS.US\"Z\"') AS voided_at,void_reason";

fn worker(identity: &Identity) -> Result<(), Error> {
    if !["operator", "admin"].contains(&identity.role.as_str()) {
        return Err(Error::new(403, "Carrier labels require an operator"));
    }
    Ok(())
}

fn conflict(error: sqlx::Error, message: &str) -> Error {
    match error.as_database_error().and_then(|e| e.code()).as_deref() {
        Some("23505") | Some("23514") => Error::new(409, message),
        _ => error.into(),
    }
}

/// Record a label on an open package. Shared by the endpoint and fixtures.
pub async fn insert(
    pool: &sqlx::PgPool,
    tenant: &str,
    warehouse: &str,
    actor: &str,
    package: &str,
    input: &RecordRequest,
) -> Result<Value, Error> {
    validate(input)?;
    let mut tx = pool.begin().await?;
    // Serialize with handoff and unpack, which take the same warehouse lock.
    sqlx::query("SELECT 1 FROM wms_warehouses WHERE tenant=$1 AND warehouse_id=$2 FOR UPDATE")
        .bind(tenant)
        .bind(warehouse)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(|| Error::new(404, "Warehouse not found"))?;
    let exists: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM wms_packages WHERE tenant=$1 AND warehouse_id=$2 AND id=$3)",
    )
    .bind(tenant)
    .bind(warehouse)
    .bind(package)
    .fetch_one(&mut *tx)
    .await?;
    if !exists {
        return Err(Error::new(404, "Package not found"));
    }
    let (zpl, url) = match &input.label {
        Label::Zpl { data } => (Some(data.as_str()), None),
        Label::Url { url } => (None, Some(url.as_str())),
    };
    let carton = input.carton.clone().unwrap_or(Carton {
        weight_grams: None,
        length_mm: None,
        width_mm: None,
        height_mm: None,
    });
    let row = sqlx::query(&format!(
        "INSERT INTO wms_carrier_labels(tenant,warehouse_id,id,package_id,carrier,service,tracking_number,rate_amount,rate_currency,weight_grams,length_mm,width_mm,height_mm,label_format,label_zpl,label_url,recorded_by)
         VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17) RETURNING {COLUMNS}"
    ))
    .bind(tenant)
    .bind(warehouse)
    .bind(domain::id())
    .bind(package)
    .bind(&input.carrier)
    .bind(&input.service)
    .bind(&input.tracking_number)
    .bind(input.rate.as_ref().map(|r| r.amount.clone()))
    .bind(input.rate.as_ref().map(|r| r.currency.clone()))
    .bind(carton.weight_grams)
    .bind(carton.length_mm)
    .bind(carton.width_mm)
    .bind(carton.height_mm)
    .bind(if zpl.is_some() { "zpl" } else { "url" })
    .bind(zpl)
    .bind(url)
    .bind(actor)
    .fetch_one(&mut *tx)
    .await
    .map_err(|e| {
        conflict(
            e,
            "Package already has an active label, or it is already handed off or unpacked; void the label first",
        )
    })?;
    tx.commit().await?;
    Ok(row_json(&row))
}

/// Void a package's active label. Shared by the endpoint and fixtures.
pub async fn void_label(
    pool: &sqlx::PgPool,
    tenant: &str,
    warehouse: &str,
    actor: &str,
    package: &str,
    label: &str,
    reason: &str,
) -> Result<Value, Error> {
    bounded(reason, "Void reason", 240)?;
    let row = sqlx::query(&format!(
        "UPDATE wms_carrier_labels SET voided_by=$5,voided_at=now(),void_reason=$6
         WHERE tenant=$1 AND warehouse_id=$2 AND package_id=$3 AND id=$4 AND voided_at IS NULL RETURNING {COLUMNS}"
    ))
    .bind(tenant)
    .bind(warehouse)
    .bind(package)
    .bind(label)
    .bind(actor)
    .bind(reason)
    .fetch_optional(pool)
    .await?
    .ok_or_else(|| Error::new(409, "No active label with this ID on the package"))?;
    Ok(row_json(&row))
}

pub async fn record(
    AppState(app): AppState<App>,
    Extension(identity): Extension<Identity>,
    Path((warehouse, package)): Path<(String, String)>,
    Json(input): Json<RecordRequest>,
) -> Result<Json<Value>, Error> {
    worker(&identity)?;
    Ok(Json(
        insert(
            &app.store.pool,
            &identity.tenant,
            &warehouse,
            &identity.actor,
            &package,
            &input,
        )
        .await?,
    ))
}

pub async fn list(
    AppState(app): AppState<App>,
    Extension(identity): Extension<Identity>,
    Path((warehouse, package)): Path<(String, String)>,
) -> Result<Json<Value>, Error> {
    if !["viewer", "operator", "admin"].contains(&identity.role.as_str()) {
        return Err(Error::new(
            403,
            "Reading carrier labels requires a warehouse role",
        ));
    }
    let rows = sqlx::query(&format!(
        "SELECT {COLUMNS} FROM wms_carrier_labels WHERE tenant=$1 AND warehouse_id=$2 AND package_id=$3 ORDER BY seq"
    ))
    .bind(&identity.tenant)
    .bind(&warehouse)
    .bind(&package)
    .fetch_all(&app.store.pool)
    .await?;
    Ok(Json(
        json!({"package_id":package,"labels":rows.iter().map(row_json).collect::<Vec<_>>()}),
    ))
}

pub async fn void(
    AppState(app): AppState<App>,
    Extension(identity): Extension<Identity>,
    Path((warehouse, package, label)): Path<(String, String, String)>,
    Json(input): Json<VoidRequest>,
) -> Result<Json<Value>, Error> {
    worker(&identity)?;
    Ok(Json(
        void_label(
            &app.store.pool,
            &identity.tenant,
            &warehouse,
            &identity.actor,
            &package,
            &label,
            &input.reason,
        )
        .await?,
    ))
}

/// The active label's ZPL for printing, if the carrier supplied ZPL.
pub async fn active_zpl(
    app: &App,
    tenant: &str,
    warehouse: &str,
    package: &str,
) -> Result<String, Error> {
    let row = sqlx::query(
        "SELECT label_format,label_zpl FROM wms_carrier_labels WHERE tenant=$1 AND warehouse_id=$2 AND package_id=$3 AND voided_at IS NULL",
    )
    .bind(tenant)
    .bind(warehouse)
    .bind(package)
    .fetch_optional(&app.store.pool)
    .await?
    .ok_or_else(|| Error::new(404, format!("Package {package} has no active carrier label")))?;
    row.get::<Option<String>, _>("label_zpl").ok_or_else(|| {
        Error::new(
            422,
            "The active carrier label is a URL, not ZPL; print it from the carrier link",
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(label: Label) -> RecordRequest {
        RecordRequest {
            carrier: "Synthetic Parcel".into(),
            service: "Ground".into(),
            tracking_number: "1Z999AA10123456784".into(),
            rate: Some(Rate {
                amount: "12.34".into(),
                currency: "USD".into(),
            }),
            carton: None,
            label,
        }
    }

    #[test]
    fn decimal_rates_are_exact_strings() {
        for ok in ["0", "12", "12.3", "12.34567890", "999999999999"] {
            assert!(regex_lite_decimal(ok), "{ok}");
        }
        for bad in ["", "012", "1.", ".5", "1.123456789", "-1", "1e3", "1,5"] {
            assert!(!regex_lite_decimal(bad), "{bad}");
        }
    }

    #[test]
    fn labels_must_be_zpl_or_https() {
        assert!(validate(&request(Label::Zpl {
            data: "^XA^FDx^FS^XZ".into()
        }))
        .is_ok());
        assert!(validate(&request(Label::Zpl {
            data: "garbage".into()
        }))
        .is_err());
        assert!(validate(&request(Label::Url {
            url: "https://carrier.example/l.pdf".into()
        }))
        .is_ok());
        assert!(validate(&request(Label::Url {
            url: "http://carrier.example/l.pdf".into()
        }))
        .is_err());
        let mut spaced = request(Label::Zpl {
            data: "^XA^XZ".into(),
        });
        spaced.tracking_number = "1Z 999".into();
        assert!(validate(&spaced).is_err());
    }
}
