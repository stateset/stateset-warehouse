//! Typed iCommerce interoperability over the WMS's existing inventory authority.
//! Numeric IDs are caller-supplied mappings, never fabricated database keys.
use crate::{
    api::App,
    domain,
    store::{Error, Identity, Result},
};
use axum::{
    extract::{Path, State},
    Extension, Json,
};
use rust_decimal::{prelude::ToPrimitive, Decimal};
use serde::{
    de::{self, DeserializeOwned, MapAccess, Visitor},
    Deserialize, Deserializer, Serialize,
};
use serde_json::{json, Value};
use stateset_core::{
    validation::{Validate, ValidationBuilder},
    CreateInventoryItem, LocationStock, MoveInventory, StockLevel,
};
use stateset_primitives::Sku;
use std::collections::{BTreeMap, BTreeSet};
use uuid::Uuid;

pub const UPSTREAM_REVISION: &str = "b8a5c6db885998defa9fd74e2b12f0d1add21e1c";

#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Mappings {
    #[serde(deserialize_with = "location_map")]
    pub locations: BTreeMap<i32, String>,
    #[serde(default, deserialize_with = "lot_map")]
    pub lots: BTreeMap<Uuid, String>,
}
// JSON object keys arrive as strings, including through serde's internally
// tagged enum buffering. Parse explicitly and reject aliases/duplicates.
fn exact_map<'de, D, K>(deserializer: D) -> std::result::Result<BTreeMap<K, String>, D::Error>
where
    D: Deserializer<'de>,
    K: std::str::FromStr + Ord + ToString,
{
    struct Exact<K>(std::marker::PhantomData<K>);
    impl<'de, K: std::str::FromStr + Ord + ToString> Visitor<'de> for Exact<K> {
        type Value = BTreeMap<K, String>;
        fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
            f.write_str("an object of canonical unique ID mappings")
        }
        fn visit_map<A: MapAccess<'de>>(
            self,
            mut map: A,
        ) -> std::result::Result<Self::Value, A::Error> {
            let mut values = BTreeMap::new();
            while let Some((raw, value)) = map.next_entry::<String, String>()? {
                let key = raw
                    .parse::<K>()
                    .map_err(|_| de::Error::custom("Invalid mapping ID"))?;
                if key.to_string() != raw || values.insert(key, value).is_some() {
                    return Err(de::Error::custom("Noncanonical or duplicate mapping ID"));
                }
                if values.len() > 1000 {
                    return Err(de::Error::custom("Map at most 1,000 IDs"));
                }
            }
            Ok(values)
        }
    }
    deserializer.deserialize_map(Exact(std::marker::PhantomData))
}
fn location_map<'de, D: Deserializer<'de>>(
    d: D,
) -> std::result::Result<BTreeMap<i32, String>, D::Error> {
    exact_map(d)
}
fn lot_map<'de, D: Deserializer<'de>>(
    d: D,
) -> std::result::Result<BTreeMap<Uuid, String>, D::Error> {
    exact_map(d)
}

fn strict_model<'de, D: Deserializer<'de>, T: DeserializeOwned>(
    d: D,
    fields: &'static [&'static str],
) -> std::result::Result<T, D::Error> {
    struct Model(&'static [&'static str]);
    impl<'de> Visitor<'de> for Model {
        type Value = serde_json::Map<String, Value>;
        fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
            f.write_str("an upstream model with supported unique fields")
        }
        fn visit_map<A: MapAccess<'de>>(
            self,
            mut map: A,
        ) -> std::result::Result<Self::Value, A::Error> {
            let mut values = serde_json::Map::new();
            while let Some((key, value)) = map.next_entry::<String, Value>()? {
                if !self.0.contains(&key.as_str()) {
                    return Err(de::Error::custom(format!(
                        "Unsupported upstream field: {key}"
                    )));
                }
                if values.insert(key, value).is_some() {
                    return Err(de::Error::custom("Duplicate upstream field"));
                }
            }
            Ok(values)
        }
    }
    let value = d.deserialize_map(Model(fields))?;
    // Upstream Decimal::from_str rounds beyond its representable scale. Check
    // the original string before upstream deserialization can lose a fraction.
    // Numeric JSON may already have passed through a floating-point client.
    for field in [
        "quantity",
        "initial_quantity",
        "reorder_point",
        "safety_stock",
    ] {
        if let Some(raw) = value.get(field).filter(|value| !value.is_null()) {
            let text = raw.as_str().ok_or_else(|| {
                de::Error::custom(format!("{field} must be an exact decimal string"))
            })?;
            if text.len() > 64 {
                return Err(de::Error::custom(format!(
                    "{field} exceeds supported exact decimal precision"
                )));
            }
            Decimal::from_str_exact(text).map_err(|_| {
                de::Error::custom(format!(
                    "{field} exceeds supported exact decimal precision or has invalid syntax"
                ))
            })?;
        }
    }
    serde_json::from_value(Value::Object(value)).map_err(de::Error::custom)
}
fn move_model<'de, D: Deserializer<'de>>(d: D) -> std::result::Result<MoveInventory, D::Error> {
    strict_model(
        d,
        &[
            "from_location_id",
            "to_location_id",
            "sku",
            "lot_id",
            "quantity",
            "reason",
            "performed_by",
        ],
    )
}
fn product_model<'de, D: Deserializer<'de>>(
    d: D,
) -> std::result::Result<CreateInventoryItem, D::Error> {
    strict_model(
        d,
        &[
            "sku",
            "name",
            "description",
            "unit_of_measure",
            "initial_quantity",
            "location_id",
            "reorder_point",
            "safety_stock",
        ],
    )
}
impl Mappings {
    fn validate(&self) -> Result<()> {
        if self.locations.len() > 1000 || self.lots.len() > 1000 {
            return Err(Error::new(422, "Map at most 1,000 locations and lots"));
        }
        let exact = |value: &str| {
            !value.is_empty()
                && value.len() <= 240
                && value.trim() == value
                && !value.chars().any(char::is_control)
        };
        let unique_locations: BTreeSet<_> = self.locations.values().collect();
        let unique_lots: BTreeSet<_> = self.lots.values().collect();
        if self
            .locations
            .iter()
            .any(|(id, code)| *id <= 0 || !exact(code))
            || self.lots.values().any(|lot| !exact(lot))
            || unique_locations.len() != self.locations.len()
            || unique_lots.len() != self.lots.len()
        {
            return Err(Error::new(
                422,
                "Mappings require positive unique IDs and exact unique WMS labels",
            ));
        }
        Ok(())
    }
    fn location(&self, id: i32) -> Result<String> {
        self.locations
            .get(&id)
            .cloned()
            .ok_or_else(|| Error::new(422, "iCommerce location ID has no explicit WMS mapping"))
    }
}

fn whole(value: Decimal, minimum: i64) -> Result<i64> {
    ValidationBuilder::new()
        .non_negative("quantity", value)
        .build()
        .map_err(|error| Error::new(422, error.to_string()))?;
    if !value.fract().is_zero()
        || value < Decimal::from(minimum)
        || value > Decimal::from(1_000_000)
    {
        return Err(Error::new(
            422,
            format!("WMS quantities must be whole each units between {minimum} and 1,000,000"),
        ));
    }
    value
        .to_i64()
        .ok_or_else(|| Error::new(422, "Quantity cannot be represented as whole each units"))
}
fn exact_sku(value: &str) -> Result<()> {
    let sku = Sku::new(value).map_err(|error| Error::new(422, error.to_string()))?;
    if sku.as_str() != value {
        return Err(Error::new(
            422,
            "SKU normalization would change its original identity",
        ));
    }
    stateset_core::validate_sku(value).map_err(|error| Error::new(422, error.to_string()))
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Preview {
    MoveInventory {
        #[serde(deserialize_with = "move_model")]
        input: MoveInventory,
        mappings: Mappings,
    },
    CreateInventoryItem {
        #[serde(deserialize_with = "product_model")]
        input: CreateInventoryItem,
        category: String,
    },
}
impl Preview {
    pub fn command(&self) -> Result<domain::Command> {
        match self {
            Self::MoveInventory { input, mappings } => {
                mappings.validate()?;
                exact_sku(&input.sku)?;
                let from = mappings.location(input.from_location_id)?;
                let to = mappings.location(input.to_location_id)?;
                if from == to {
                    return Err(Error::new(422, "Move source and destination must differ"));
                }
                let lot = match input.lot_id {
                    Some(id) => mappings.lots.get(&id).cloned().ok_or_else(|| {
                        Error::new(422, "iCommerce lot UUID has no explicit WMS mapping")
                    })?,
                    None => "UNTRACKED".to_string(),
                };
                // An upstream performed_by value cannot replace authenticated Identity.
                // These fields have no canonical WMS move representation; require callers
                // to preserve them externally rather than silently discard them.
                if input.reason.is_some() || input.performed_by.is_some() {
                    return Err(Error::new(422, "Move reason/performed_by are unsupported; the authenticated WMS actor owns execution"));
                }
                Ok(domain::Command::MoveStock {
                    sku: input.sku.clone(),
                    lot,
                    from,
                    to,
                    quantity: whole(input.quantity, 1)?,
                })
            }
            Self::CreateInventoryItem { input, category } => {
                exact_sku(&input.sku)?;
                input
                    .validate()
                    .map_err(|error| Error::new(422, error.to_string()))?;
                if input.initial_quantity.is_some_and(|q| !q.is_zero())
                    || input.location_id.is_some()
                    || input.safety_stock.is_some_and(|q| !q.is_zero())
                    || input.description.is_some()
                    || input
                        .unit_of_measure
                        .as_deref()
                        .is_some_and(|unit| !matches!(unit, "EA" | "each"))
                {
                    return Err(Error::new(422, "Initial stock requires receiving or a physical count; location, safety stock, description and other units have no product mapping"));
                }
                Ok(domain::Command::CreateProduct {
                    serial_controlled: false,
                    sku: input.sku.clone(),
                    name: input.name.clone(),
                    category: category.clone(),
                    reorder_point: whole(input.reorder_point.unwrap_or(Decimal::ZERO), 0)?,
                })
            }
        }
    }
}

pub async fn preview(
    State(app): State<App>,
    Extension(identity): Extension<Identity>,
    Path(warehouse): Path<String>,
    input: std::result::Result<Json<Preview>, axum::extract::rejection::JsonRejection>,
) -> Result<Json<Value>> {
    let Json(input) = input.map_err(|error| {
        Error::new(
            error.status().as_u16(),
            format!("Invalid iCommerce preview: {}", error.body_text()),
        )
    })?;
    let command = input.command()?;
    if !command.allowed(&identity.role) {
        return Err(Error::new(403, "Your role does not permit this action"));
    }
    let state = app.store.state(&identity.tenant, &warehouse).await?;
    state
        .execute(&command, &identity.actor)
        .map_err(|error| Error::new(422, error))?;
    let serial = match &command {
        domain::Command::MoveStock { sku, .. } => state
            .products
            .iter()
            .any(|product| product.sku == *sku && product.serial_controlled),
        _ => false,
    };
    Ok(Json(
        json!({"tenant":identity.tenant,"actor":identity.actor,"warehouse_id":warehouse,
        "reviewed_version":state.version,"command":command,"serial_observation_required":serial,"upstream_input":input,
        "committed":false,"execution":"Review and preserve a complete original WMS request, then submit it to the canonical commands endpoint",
        "upstream_revision":UPSTREAM_REVISION}),
    ))
}

/// Aggregate the original balances into actual iCommerce domain types. Lots and
/// their eligibility remain separately identified in original WMS positions.
pub fn inventory(state: &domain::State, mappings: &Mappings) -> Result<Vec<StockLevel>> {
    mappings.validate()?;
    state.validate().map_err(|error| Error::new(422, error))?;
    if mappings.locations.values().any(|code| {
        !state
            .locations
            .iter()
            .any(|location| location.code == *code)
    }) {
        return Err(Error::new(
            422,
            "A mapped location is absent from this WMS warehouse",
        ));
    }
    let reverse: BTreeMap<_, _> = mappings
        .locations
        .iter()
        .map(|(id, code)| (code.as_str(), *id))
        .collect();
    let mut grouped: BTreeMap<String, BTreeMap<i32, (Decimal, Decimal)>> = BTreeMap::new();
    for stock in &state.stocks {
        let id = *reverse.get(stock.location.as_str()).ok_or_else(|| {
            Error::new(
                422,
                "Every original stock location requires an explicit iCommerce ID mapping",
            )
        })?;
        let values = grouped
            .entry(stock.sku.clone())
            .or_default()
            .entry(id)
            .or_default();
        values.0 += Decimal::from(stock.on_hand);
        values.1 += Decimal::from(stock.reserved);
    }
    Ok(state
        .products
        .iter()
        .map(|product| {
            let locations: Vec<_> = grouped
                .remove(&product.sku)
                .unwrap_or_default()
                .into_iter()
                .map(|(id, (on_hand, allocated))| LocationStock {
                    location_id: id,
                    location_name: mappings.locations.get(&id).cloned(),
                    on_hand,
                    allocated,
                    available: on_hand - allocated,
                })
                .collect();
            StockLevel {
                sku: product.sku.clone(),
                name: product.name.clone(),
                total_on_hand: locations.iter().map(|row| row.on_hand).sum(),
                total_allocated: locations.iter().map(|row| row.allocated).sum(),
                total_available: locations.iter().map(|row| row.available).sum(),
                locations,
            }
        })
        .collect())
}
pub async fn inventory_endpoint(
    State(app): State<App>,
    Extension(identity): Extension<Identity>,
    Path(warehouse): Path<String>,
    mappings: std::result::Result<Json<Mappings>, axum::extract::rejection::JsonRejection>,
) -> Result<Json<Value>> {
    let Json(mappings) = mappings.map_err(|error| {
        Error::new(
            error.status().as_u16(),
            format!("Invalid iCommerce mappings: {}", error.body_text()),
        )
    })?;
    let state = app.store.state(&identity.tenant, &warehouse).await?;
    let rows = inventory(&state, &mappings)?;
    Ok(Json(
        json!({"tenant":identity.tenant,"warehouse_id":warehouse,"version":state.version,"inventory":rows,
        "source_positions":state.stocks,"lot_controls":state.lot_controls,
        "availability_basis":"physical_minus_reserved; expiry and quarantine eligibility remain in original WMS lot controls",
        "read_only":true,"upstream_revision":UPSTREAM_REVISION}),
    ))
}

/// Narrow, explicit MCP inputs. Decimal quantities are strings to preserve all
/// digits through JavaScript/LLM clients. REST validates original decimal
/// strings exactly before invoking the upstream decoder.
pub fn mappings_schema() -> Value {
    json!({"type":"object","additionalProperties":false,"required":["locations"],"properties":{
        "locations":{"type":"object","maxProperties":1000,"propertyNames":{"pattern":"^[1-9][0-9]*$"},"additionalProperties":{"type":"string","minLength":1,"maxLength":240}},
        "lots":{"type":"object","maxProperties":1000,"propertyNames":{"pattern":"^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$"},"additionalProperties":{"type":"string","minLength":1,"maxLength":240}}
    }})
}
pub fn preview_schema() -> Value {
    let text = json!({"type":"string","minLength":1,"maxLength":240});
    let decimal = json!({"type":"string","pattern":"^[0-9]+(\\.[0-9]+)?$","maxLength":64});
    json!({"oneOf":[
        {"type":"object","additionalProperties":false,"required":["kind","input","mappings"],"properties":{
            "kind":{"const":"move_inventory"},"mappings":mappings_schema(),"input":{
                "type":"object","additionalProperties":false,"required":["from_location_id","to_location_id","sku","quantity"],"properties":{
                    "from_location_id":{"type":"integer","minimum":1,"maximum":2147483647},"to_location_id":{"type":"integer","minimum":1,"maximum":2147483647},
                    "sku":text,"quantity":decimal,"lot_id":{"type":["string","null"],"format":"uuid"},"reason":{"type":"null"},"performed_by":{"type":"null"}
                }
            }
        }},
        {"type":"object","additionalProperties":false,"required":["kind","input","category"],"properties":{
            "kind":{"const":"create_inventory_item"},"category":text,"input":{
                "type":"object","additionalProperties":false,"required":["sku","name"],"properties":{
                    "sku":text,"name":text,"reorder_point":decimal,"initial_quantity":{"enum":["0","0.0",null]},
                    "unit_of_measure":{"enum":["EA","each",null]},"description":{"type":"null"},"location_id":{"type":"null"},"safety_stock":{"enum":["0","0.0",null]}
                }
            }
        }}
    ],"description":"Pinned iCommerce models; canonical WMS validation and authenticated roles remain authoritative"})
}
