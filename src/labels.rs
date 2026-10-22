//! Printable ZPL labels for bins, products, packages and serial units.
//! Labels are read-only renderings of current records: printing changes no
//! inventory, and every barcode encodes the exact stored identifier. Values a
//! Code 128 barcode cannot carry are rejected rather than altered.
use crate::{
    api::App,
    domain::State,
    store::{Error, Identity},
};
use axum::{
    extract::{Path, State as AppState},
    Extension, Json,
};
use serde::Deserialize;
use serde_json::{json, Value};
use sqlx::Row;

/// Most labels per request, so one print job stays bounded.
pub const MAX_LABELS: usize = 200;
/// Labels are laid out for 203 dpi printers (8 dots per millimetre).
pub const DPI: i64 = 203;

#[derive(Debug, Clone, Deserialize, schemars::JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum LabelRequest {
    /// 4×2 in bin label: location code barcode, zone and kind.
    Location { code: String },
    /// 4×2 in product label: SKU barcode and name.
    Product { sku: String },
    /// 4×6 in carton label: package barcode, order, ship-to and contents.
    Package { id: String },
    /// 4×2 in unit label: exact serial barcode, SKU and lot.
    Serial { sku: String, serial: String },
    /// The package's active carrier label, exactly as the carrier supplied it.
    CarrierLabel { package_id: String },
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PrintRequest {
    pub labels: Vec<LabelRequest>,
}

/// Escape free text for a `^FH_` field: `_`, `^` and `~` become hex escapes,
/// so stored values can never inject ZPL commands. Other UTF-8 text prints
/// through `^CI28`.
pub fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '_' => out.push_str("_5F"),
            '^' => out.push_str("_5E"),
            '~' => out.push_str("_7E"),
            c if c.is_control() => out.push(' '),
            c => out.push(c),
        }
    }
    out
}

/// Code 128 carries printable ASCII only. Reject anything else so the
/// printed barcode always scans back to the exact stored value.
pub fn barcode_value(value: &str, what: &str) -> Result<String, String> {
    if value.is_empty() || value.len() > 80 || !value.bytes().all(|b| (0x20..=0x7e).contains(&b)) {
        return Err(format!(
            "{what} cannot be printed as a Code 128 barcode: it must be 1–80 printable ASCII characters"
        ));
    }
    Ok(escape(value))
}

fn text(x: i64, y: i64, height: i64, width: i64, lines: i64, value: &str) -> String {
    format!(
        "^FO{x},{y}^A0N,{height},{height}^FB{width},{lines},0,L^FH_^FD{}^FS",
        escape(value)
    )
}

fn barcode(x: i64, y: i64, height: i64, value: &str, what: &str) -> Result<String, String> {
    Ok(format!(
        "^FO{x},{y}^BY2^BCN,{height},Y,N,N^FH_^FD{}^FS",
        barcode_value(value, what)?
    ))
}

fn small(body: String) -> String {
    format!("^XA^CI28^PW812^LL406^LH0,0{body}^XZ\n")
}

pub fn location(state: &State, code: &str) -> Result<String, String> {
    let bin = state
        .locations
        .iter()
        .find(|l| l.code == code)
        .ok_or_else(|| format!("Location {code} not found"))?;
    Ok(small(format!(
        "{}{}{}",
        text(
            30,
            24,
            34,
            752,
            1,
            &format!("{} · {}", state.warehouse.code, bin.zone)
        ),
        barcode(30, 80, 160, &bin.code, "Location code")?,
        text(30, 320, 30, 752, 1, &format!("{} bin", bin.kind)),
    )))
}

pub fn product(state: &State, sku: &str) -> Result<String, String> {
    let product = state
        .products
        .iter()
        .find(|p| p.sku == sku)
        .ok_or_else(|| format!("Product {sku} not found"))?;
    let tracking = if product.serial_controlled {
        "Serial tracked"
    } else {
        "Whole units"
    };
    Ok(small(format!(
        "{}{}{}",
        text(30, 24, 34, 752, 2, &product.name),
        barcode(30, 110, 150, &product.sku, "SKU")?,
        text(
            30,
            330,
            28,
            752,
            1,
            &format!("{} · {tracking}", product.category)
        ),
    )))
}

pub struct PackageLabel {
    pub id: String,
    pub reference: String,
    pub order_reference: String,
    pub customer: String,
    pub destination: String,
    pub lines: Vec<(String, String, i64)>,
    pub closure: Option<String>,
}

pub fn package(label: &PackageLabel) -> Result<String, String> {
    let mut body = format!(
        "^XA^CI28^PW812^LL1218^LH0,0{}{}{}{}",
        text(30, 30, 40, 752, 1, "SHIP TO"),
        text(30, 80, 46, 752, 2, &label.customer),
        text(30, 190, 34, 752, 3, &label.destination),
        "^FO30,330^GB752,3,3^FS",
    );
    body.push_str(&text(
        30,
        350,
        30,
        752,
        1,
        &format!(
            "Order {} · Package {}",
            label.order_reference, label.reference
        ),
    ));
    body.push_str(&barcode(30, 400, 180, &label.id, "Package ID")?);
    body.push_str("^FO30,640^GB752,3,3^FS");
    let mut y = 660;
    let shown = label.lines.len().min(12);
    for (sku, lot, quantity) in &label.lines[..shown] {
        body.push_str(&text(
            30,
            y,
            28,
            752,
            1,
            &format!("{quantity} × {sku} · lot {lot}"),
        ));
        y += 38;
    }
    if label.lines.len() > shown {
        body.push_str(&text(
            30,
            y,
            28,
            752,
            1,
            &format!(
                "+ {} more lines (see packing list)",
                label.lines.len() - shown
            ),
        ));
    }
    if let Some(closure) = &label.closure {
        body.push_str(&text(30, 1150, 30, 752, 1, &format!("Package {closure}")));
    }
    body.push_str("^XZ\n");
    Ok(body)
}

pub fn serial(sku: &str, serial: &str, lot: &str, status: &str) -> Result<String, String> {
    Ok(small(format!(
        "{}{}{}",
        text(30, 24, 30, 752, 1, &format!("SKU {sku}")),
        barcode(30, 70, 170, serial, "Serial")?,
        text(30, 330, 28, 752, 1, &format!("Lot {lot} · {status}")),
    )))
}

async fn package_label(
    app: &App,
    state: &State,
    tenant: &str,
    warehouse: &str,
    id: &str,
) -> Result<PackageLabel, Error> {
    let header = sqlx::query(
        "SELECT p.order_id,p.reference,c.kind FROM wms_packages p LEFT JOIN wms_package_closures c
         ON c.tenant=p.tenant AND c.warehouse_id=p.warehouse_id AND c.package_id=p.id
         WHERE p.tenant=$1 AND p.warehouse_id=$2 AND p.id=$3",
    )
    .bind(tenant)
    .bind(warehouse)
    .bind(id)
    .fetch_optional(&app.store.pool)
    .await?
    .ok_or_else(|| Error::new(404, format!("Package {id} not found")))?;
    let order_id: String = header.get("order_id");
    let lines = sqlx::query(
        "SELECT sku,lot,sum(quantity)::bigint AS quantity FROM wms_package_lines
         WHERE tenant=$1 AND warehouse_id=$2 AND package_id=$3 GROUP BY sku,lot ORDER BY sku,lot",
    )
    .bind(tenant)
    .bind(warehouse)
    .bind(id)
    .fetch_all(&app.store.pool)
    .await?
    .into_iter()
    .map(|r| (r.get("sku"), r.get("lot"), r.get("quantity")))
    .collect();
    let order = state.orders.iter().find(|o| o.id == order_id);
    Ok(PackageLabel {
        id: id.to_string(),
        reference: header.get("reference"),
        order_reference: order.map_or(order_id.clone(), |o| o.reference.clone()),
        customer: order.map_or_else(String::new, |o| o.customer.clone()),
        destination: order.map_or_else(String::new, |o| o.destination.clone()),
        lines,
        closure: header
            .get::<Option<String>, _>("kind")
            .map(|k| k.replace('_', " ")),
    })
}

pub async fn endpoint(
    AppState(app): AppState<App>,
    Extension(identity): Extension<Identity>,
    Path(warehouse): Path<String>,
    Json(input): Json<PrintRequest>,
) -> Result<Json<Value>, Error> {
    if !["viewer", "operator", "admin"].contains(&identity.role.as_str()) {
        return Err(Error::new(403, "Printing labels requires a warehouse role"));
    }
    if input.labels.is_empty() || input.labels.len() > MAX_LABELS {
        return Err(Error::new(422, format!("Request 1–{MAX_LABELS} labels")));
    }
    let state = app.store.state(&identity.tenant, &warehouse).await?;
    let mut zpl = String::new();
    for request in &input.labels {
        let label = match request {
            LabelRequest::Location { code } => location(&state, code),
            LabelRequest::Product { sku } => product(&state, sku),
            LabelRequest::Package { id } => {
                let label = package_label(&app, &state, &identity.tenant, &warehouse, id).await?;
                package(&label)
            }
            LabelRequest::CarrierLabel { package_id } => {
                let label = crate::carrier_labels::active_zpl(&app, &identity.tenant, &warehouse, package_id).await?;
                zpl.push_str(&label);
                if !label.ends_with('\n') {
                    zpl.push('\n');
                }
                continue;
            }
            LabelRequest::Serial { sku, serial: label } => {
                let unit = sqlx::query(
                    "SELECT lot,place->>'status' AS status FROM wms_serial_units WHERE tenant=$1 AND sku=$2 AND serial=$3",
                )
                .bind(&identity.tenant)
                .bind(sku)
                .bind(label)
                .fetch_optional(&app.store.pool)
                .await?
                .ok_or_else(|| Error::new(404, format!("Serial {label} of {sku} not found")))?;
                serial(sku, label, &unit.get::<String, _>("lot"), &unit.get::<String, _>("status"))
            }
        }
        .map_err(|e| {
            if e.ends_with("not found") {
                Error::new(404, e)
            } else {
                Error::new(422, e)
            }
        })?;
        zpl.push_str(&label);
    }
    Ok(Json(json!({
        "format": "zpl",
        "dpi": DPI,
        "warehouse_id": state.warehouse.id,
        "version": state.version,
        "count": input.labels.len(),
        "zpl": zpl,
    })))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escapes_zpl_control_characters_in_free_text() {
        assert_eq!(escape("A^XZ~JA_b"), "A_5EXZ_7EJA_5Fb");
        assert_eq!(escape("Bin\nA"), "Bin A");
        assert_eq!(escape("Größe 中"), "Größe 中");
    }

    #[test]
    fn barcodes_accept_only_exact_printable_ascii() {
        assert_eq!(barcode_value("A-01", "Bin").expect("ASCII bin"), "A-01");
        assert_eq!(
            barcode_value("Unit^1", "Serial").expect("escaped serial"),
            "Unit_5E1"
        );
        assert!(barcode_value("", "Bin").is_err());
        assert!(barcode_value("Größe", "SKU").is_err());
        assert!(barcode_value("tab\there", "Serial").is_err());
        assert!(barcode_value(&"x".repeat(81), "Serial").is_err());
    }

    #[test]
    fn package_labels_cap_listed_lines_and_say_how_many_more() {
        let label = PackageLabel {
            id: "PKG-1".into(),
            reference: "Carton 1".into(),
            order_reference: "SO-1".into(),
            customer: "Synthetic customer".into(),
            destination: "1 Synthetic Way".into(),
            lines: (0..15)
                .map(|i| (format!("SKU-{i}"), "LOT".into(), 1))
                .collect(),
            closure: Some("handed off".into()),
        };
        let zpl = package(&label).expect("package label");
        assert!(zpl.starts_with("^XA^CI28^PW812^LL1218"));
        assert!(zpl.ends_with("^XZ\n"));
        assert!(zpl.contains("^FDPKG-1^FS"));
        assert!(zpl.contains("+ 3 more lines"));
        assert!(zpl.contains("Package handed off"));
    }
}
