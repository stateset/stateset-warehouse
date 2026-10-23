//! Reviewed master data used by scanning and putaway: product GTINs and
//! per-bin capacity. Administrators replace these as audited changes; they
//! move no stock. Scan parsing resolves a GS1 scan to its product through
//! the reviewed GTINs only, never by guessing.
use crate::{
    api::App,
    domain, gs1,
    store::{Error, Identity},
};
use axum::{
    extract::{Path, State as AppState},
    Extension, Json,
};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::BTreeMap;

/// GTIN-8, -12, -13 or -14 with a valid check digit, as its 14-digit form
/// (the GS1 standard left-zero padding).
pub fn normalize_gtin(raw: &str) -> Result<String, String> {
    let digits = raw.trim();
    if ![8, 12, 13, 14].contains(&digits.len()) || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return Err(format!("GTIN {raw} must be 8, 12, 13 or 14 digits"));
    }
    let padded = format!("{digits:0>14}");
    if !gs1::check_digit_ok(&padded) {
        return Err(format!("GTIN {raw} fails its check digit"));
    }
    Ok(padded)
}

fn reader(identity: &Identity) -> Result<(), Error> {
    if !["viewer", "operator", "admin"].contains(&identity.role.as_str()) {
        return Err(Error::new(403, "This read requires a warehouse role"));
    }
    Ok(())
}

fn admin(identity: &Identity) -> Result<(), Error> {
    if identity.role != "admin" {
        return Err(Error::new(
            403,
            "Only administrators may change master data",
        ));
    }
    Ok(())
}

async fn audit(
    tx: &mut sqlx::PgConnection,
    identity: &Identity,
    action: &str,
    reference: Value,
) -> Result<(), Error> {
    sqlx::query(
        "INSERT INTO wms_admin_audit(id,tenant,actor,action,reference) VALUES($1,$2,$3,$4,$5)",
    )
    .bind(domain::id())
    .bind(&identity.tenant)
    .bind(&identity.actor)
    .bind(action)
    .bind(reference.to_string())
    .execute(tx)
    .await?;
    Ok(())
}

async fn lock_warehouse(
    tx: &mut sqlx::PgConnection,
    tenant: &str,
    warehouse: &str,
) -> Result<(), Error> {
    sqlx::query("SELECT 1 FROM wms_warehouses WHERE tenant=$1 AND warehouse_id=$2 FOR UPDATE")
        .bind(tenant)
        .bind(warehouse)
        .fetch_optional(tx)
        .await?
        .ok_or_else(|| Error::new(404, "Warehouse not found"))?;
    Ok(())
}

// ------------------------------------------------------------------ GTINs

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GtinRequest {
    /// The complete set of GTINs for this product; an empty list clears it.
    pub gtins: Vec<String>,
}

pub async fn product_gtins(
    AppState(app): AppState<App>,
    Extension(identity): Extension<Identity>,
    Path((warehouse, sku)): Path<(String, String)>,
) -> Result<Json<Value>, Error> {
    reader(&identity)?;
    let gtins: Vec<String> = sqlx::query_scalar(
        "SELECT gtin FROM wms_product_gtins WHERE tenant=$1 AND warehouse_id=$2 AND sku=$3 ORDER BY gtin",
    )
    .bind(&identity.tenant)
    .bind(&warehouse)
    .bind(&sku)
    .fetch_all(&app.store.pool)
    .await?;
    Ok(Json(
        json!({"warehouse_id":warehouse,"sku":sku,"gtins":gtins}),
    ))
}

pub async fn replace_product_gtins(
    AppState(app): AppState<App>,
    Extension(identity): Extension<Identity>,
    Path((warehouse, sku)): Path<(String, String)>,
    Json(input): Json<GtinRequest>,
) -> Result<Json<Value>, Error> {
    admin(&identity)?;
    let gtins = save_gtins(&app.store.pool, &identity, &warehouse, &sku, &input.gtins).await?;
    Ok(Json(
        json!({"warehouse_id":warehouse,"sku":sku,"gtins":gtins}),
    ))
}

/// Replace a product's reviewed GTINs and audit the change in one
/// transaction. Returns the normalized 14-digit GTINs.
pub async fn save_gtins(
    pool: &sqlx::PgPool,
    identity: &Identity,
    warehouse: &str,
    sku: &str,
    raw: &[String],
) -> Result<Vec<String>, Error> {
    if raw.len() > 20 {
        return Err(Error::new(422, "A product may have at most 20 GTINs"));
    }
    let mut gtins = vec![];
    for value in raw {
        let gtin = normalize_gtin(value).map_err(|e| Error::new(422, e))?;
        if gtins.contains(&gtin) {
            return Err(Error::new(422, format!("GTIN {value} is listed twice")));
        }
        gtins.push(gtin);
    }
    let mut tx = pool.begin().await?;
    lock_warehouse(&mut tx, &identity.tenant, warehouse).await?;
    let exists: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM wms_products WHERE tenant=$1 AND warehouse_id=$2 AND sku=$3)",
    )
    .bind(&identity.tenant)
    .bind(warehouse)
    .bind(sku)
    .fetch_one(&mut *tx)
    .await?;
    if !exists {
        return Err(Error::new(404, "Product not found"));
    }
    sqlx::query("DELETE FROM wms_product_gtins WHERE tenant=$1 AND warehouse_id=$2 AND sku=$3")
        .bind(&identity.tenant)
        .bind(warehouse)
        .bind(sku)
        .execute(&mut *tx)
        .await?;
    for gtin in &gtins {
        let inserted = sqlx::query(
            "INSERT INTO wms_product_gtins(tenant,warehouse_id,gtin,sku,updated_by)
             VALUES($1,$2,$3,$4,$5) ON CONFLICT DO NOTHING",
        )
        .bind(&identity.tenant)
        .bind(warehouse)
        .bind(gtin)
        .bind(sku)
        .bind(&identity.actor)
        .execute(&mut *tx)
        .await?
        .rows_affected();
        if inserted == 0 {
            return Err(Error::new(
                409,
                format!("GTIN {gtin} already identifies another product in this warehouse"),
            ));
        }
    }
    audit(
        &mut tx,
        identity,
        "product.gtins",
        json!({"warehouse_id":warehouse,"sku":sku,"gtins":gtins}),
    )
    .await?;
    tx.commit().await?;
    Ok(gtins)
}

// ------------------------------------------------------------ scan parsing

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ScanRequest {
    /// The raw scanner data, including any ]C1/]d2 prefix and FNC1 (GS)
    /// separators, or the bracketed (01)…(10)… form.
    pub data: String,
}

pub async fn parse_scan(
    AppState(app): AppState<App>,
    Extension(identity): Extension<Identity>,
    Path(warehouse): Path<String>,
    Json(input): Json<ScanRequest>,
) -> Result<Json<Value>, Error> {
    reader(&identity)?;
    let year = chrono::Datelike::year(&chrono::Utc::now());
    let parsed = gs1::parse(&input.data, year).map_err(|e| Error::new(422, e))?;
    let product =
        match parsed.fields.get("gtin") {
            Some(gtin) => sqlx::query_scalar::<_, String>(
                "SELECT sku FROM wms_product_gtins WHERE tenant=$1 AND warehouse_id=$2 AND gtin=$3",
            )
            .bind(&identity.tenant)
            .bind(&warehouse)
            .bind(gtin)
            .fetch_optional(&app.store.pool)
            .await?,
            None => None,
        };
    let expiry_date = parsed
        .elements
        .iter()
        .find(|e| e.name == "expiry")
        .and_then(|e| e.date.clone());
    Ok(Json(json!({
        "warehouse_id": warehouse,
        "gs1": parsed,
        "sku": product,
        "lot": parsed.fields.get("lot"),
        "serial": parsed.fields.get("serial"),
        "expiry_date": expiry_date,
        "notes": [
            "sku is set only when the GTIN is reviewed for a product in this warehouse",
            "expiry_date is the label's calendar date; recording lot expiry needs an exact time with a UTC offset under your facility policy",
        ],
    })))
}

// ---------------------------------------------------------- bin capacity

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Capacity {
    pub max_units: i64,
    pub max_skus: Option<i64>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CapacityRequest {
    /// Most whole units the bin may hold; null removes the limit.
    pub max_units: Option<i64>,
    /// Most distinct SKUs the bin may hold (optional).
    #[serde(default)]
    pub max_skus: Option<i64>,
}

pub async fn load_capacity(
    pool: &sqlx::PgPool,
    tenant: &str,
    warehouse: &str,
) -> Result<BTreeMap<String, Capacity>, Error> {
    Ok(sqlx::query_as::<_, (String, i64, Option<i32>)>(
        "SELECT code,max_units,max_skus FROM wms_location_capacity WHERE tenant=$1 AND warehouse_id=$2",
    )
    .bind(tenant)
    .bind(warehouse)
    .fetch_all(pool)
    .await?
    .into_iter()
    .map(|(code, max_units, max_skus)| {
        (code, Capacity { max_units, max_skus: max_skus.map(i64::from) })
    })
    .collect())
}

pub async fn capacities(
    AppState(app): AppState<App>,
    Extension(identity): Extension<Identity>,
    Path(warehouse): Path<String>,
) -> Result<Json<Value>, Error> {
    reader(&identity)?;
    let rows = load_capacity(&app.store.pool, &identity.tenant, &warehouse).await?;
    Ok(Json(
        json!({"warehouse_id":warehouse,"bins":rows.iter().map(|(code,c)| json!({
        "code":code,"max_units":c.max_units,"max_skus":c.max_skus
    })).collect::<Vec<_>>()}),
    ))
}

pub async fn set_capacity(
    AppState(app): AppState<App>,
    Extension(identity): Extension<Identity>,
    Path((warehouse, code)): Path<(String, String)>,
    Json(input): Json<CapacityRequest>,
) -> Result<Json<Value>, Error> {
    admin(&identity)?;
    save_capacity(&app.store.pool, &identity, &warehouse, &code, &input).await?;
    Ok(Json(
        json!({"warehouse_id":warehouse,"code":code,"max_units":input.max_units,"max_skus":input.max_skus}),
    ))
}

/// Set or clear one storage bin's capacity and audit the change.
pub async fn save_capacity(
    pool: &sqlx::PgPool,
    identity: &Identity,
    warehouse: &str,
    code: &str,
    input: &CapacityRequest,
) -> Result<(), Error> {
    if input
        .max_units
        .is_some_and(|n| !(1..=1_000_000_000).contains(&n))
        || input.max_skus.is_some_and(|n| !(1..=1000).contains(&n))
        || (input.max_units.is_none() && input.max_skus.is_some())
    {
        return Err(Error::new(
            422,
            "max_units must be 1–1,000,000,000 and max_skus 1–1,000; max_skus needs max_units",
        ));
    }
    let mut tx = pool.begin().await?;
    lock_warehouse(&mut tx, &identity.tenant, warehouse).await?;
    let storage: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM wms_locations WHERE tenant=$1 AND warehouse_id=$2 AND code=$3 AND kind='storage')",
    )
    .bind(&identity.tenant)
    .bind(warehouse)
    .bind(code)
    .fetch_one(&mut *tx)
    .await?;
    if !storage {
        return Err(Error::new(404, "Storage bin not found"));
    }
    match input.max_units {
        None => {
            sqlx::query(
                "DELETE FROM wms_location_capacity WHERE tenant=$1 AND warehouse_id=$2 AND code=$3",
            )
            .bind(&identity.tenant)
            .bind(warehouse)
            .bind(code)
            .execute(&mut *tx)
            .await?;
        }
        Some(max_units) => {
            sqlx::query(
                "INSERT INTO wms_location_capacity(tenant,warehouse_id,code,max_units,max_skus,updated_by)
                 VALUES($1,$2,$3,$4,$5,$6)
                 ON CONFLICT(tenant,warehouse_id,code) DO UPDATE
                 SET max_units=EXCLUDED.max_units,max_skus=EXCLUDED.max_skus,
                     updated_by=EXCLUDED.updated_by,updated_at=now()",
            )
            .bind(&identity.tenant)
            .bind(warehouse)
            .bind(code)
            .bind(max_units)
            .bind(input.max_skus.map(|n| n as i32))
            .bind(&identity.actor)
            .execute(&mut *tx)
            .await?;
        }
    }
    audit(
        &mut tx,
        identity,
        "location.capacity",
        json!({"warehouse_id":warehouse,"code":code,"max_units":input.max_units,"max_skus":input.max_skus}),
    )
    .await?;
    tx.commit().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gtins_normalize_to_fourteen_digits_with_valid_check_digits() {
        assert_eq!(
            normalize_gtin("9506000134352").expect("GTIN-13"),
            "09506000134352"
        );
        assert_eq!(
            normalize_gtin("09506000134352").expect("GTIN-14"),
            "09506000134352"
        );
        assert_eq!(
            normalize_gtin("012345678905").expect("UPC-A"),
            "00012345678905"
        );
        assert!(normalize_gtin("9506000134353").is_err(), "bad check digit");
        assert!(normalize_gtin("95060001343").is_err(), "11 digits");
        assert!(normalize_gtin("ABCDEFGHIJKLM").is_err());
    }
}
