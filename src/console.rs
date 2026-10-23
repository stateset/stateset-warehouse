//! Read-only, tenant-bound visual console data. Terminal escape sequences and
//! rendering stay in the client; configured assets never grant stock authority.
use crate::{
    api::App,
    domain::State as WarehouseState,
    store::{Error, Identity},
};
use axum::{
    extract::{Path, Query, State},
    http::{header, HeaderMap},
    response::{IntoResponse, Response},
    routing::get,
    Extension, Json, Router,
};
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    io::Read,
    path::{Component, Path as FsPath, PathBuf},
    sync::Arc,
};
use tokio::io::AsyncReadExt;

const IMAGE_LIMIT: usize = 4 * 1024 * 1024;
const CONFIG_LIMIT: u64 = 1024 * 1024;
const FLOW: [&str; 6] = [
    "pending",
    "allocated",
    "picking",
    "picked",
    "packed",
    "partially_shipped",
];

#[derive(Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub schema_version: u8,
    #[serde(default)]
    pub facilities: Vec<Facility>,
}
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Facility {
    pub tenant: String,
    pub warehouse_id: String,
    pub layout: Option<Layout>,
    #[serde(default)]
    pub product_images: BTreeMap<String, String>,
}
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Layout {
    pub name: String,
    pub unit: String,
    pub width: u32,
    pub height: u32,
    pub bins: Vec<Position>,
}
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Position {
    pub code: String,
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}
#[derive(Clone)]
pub struct Assets {
    config: Config,
    root: PathBuf,
    revision: String,
    valid: bool,
}
fn identity(value: &str) -> bool {
    !value.is_empty() && value.len() <= 240 && !value.chars().any(char::is_control)
}
fn relative_image(value: &str) -> bool {
    identity(value)
        && value.to_ascii_lowercase().ends_with(".png")
        && FsPath::new(value)
            .components()
            .all(|c| matches!(c, Component::Normal(_)))
        && !FsPath::new(value).is_absolute()
}
impl Assets {
    pub fn parse(bytes: &[u8], root: PathBuf) -> Result<Self, String> {
        if bytes.len() > CONFIG_LIMIT as usize {
            return Err("Console configuration exceeds its limit".into());
        }
        let config: Config =
            serde_json::from_slice(bytes).map_err(|_| "Invalid console configuration")?;
        if config.schema_version != 1 || config.facilities.len() > 1000 {
            return Err("Invalid console configuration version or facility count".into());
        }
        let mut facilities = BTreeSet::new();
        for facility in &config.facilities {
            if !identity(&facility.tenant)
                || !identity(&facility.warehouse_id)
                || !facilities.insert((&facility.tenant, &facility.warehouse_id))
                || facility.product_images.len() > 10000
                || facility
                    .product_images
                    .iter()
                    .any(|(sku, file)| !identity(sku) || !relative_image(file))
            {
                return Err("Invalid or duplicate console facility/assets".into());
            }
            if let Some(layout) = &facility.layout {
                let mut codes = BTreeSet::new();
                if !identity(&layout.name)
                    || !["mm", "cm", "m", "grid"].contains(&layout.unit.as_str())
                    || !(1..=1_000_000).contains(&layout.width)
                    || !(1..=1_000_000).contains(&layout.height)
                    || layout.bins.len() > 1024
                {
                    return Err("Invalid console floor plan".into());
                }
                for bin in &layout.bins {
                    if !identity(&bin.code)
                        || !codes.insert(&bin.code)
                        || bin.width == 0
                        || bin.height == 0
                        || bin
                            .x
                            .checked_add(bin.width)
                            .is_none_or(|n| n > layout.width)
                        || bin
                            .y
                            .checked_add(bin.height)
                            .is_none_or(|n| n > layout.height)
                    {
                        return Err("Invalid floor-plan bin bounds or identity".into());
                    }
                }
                for (index, bin) in layout.bins.iter().enumerate() {
                    if layout.bins[..index].iter().any(|other| {
                        bin.x < other.x + other.width
                            && other.x < bin.x + bin.width
                            && bin.y < other.y + other.height
                            && other.y < bin.y + bin.height
                    }) {
                        return Err("Floor-plan bins overlap".into());
                    }
                }
            }
        }
        let root = root
            .canonicalize()
            .map_err(|_| "Unavailable console asset directory")?;
        Ok(Self {
            config,
            root,
            revision: format!("{:x}", Sha256::digest(bytes)),
            valid: true,
        })
    }
    pub fn empty() -> Self {
        Self {
            config: Config {
                schema_version: 1,
                facilities: vec![],
            },
            root: PathBuf::new(),
            revision: String::new(),
            valid: true,
        }
    }
    pub(crate) fn from_env() -> Self {
        let Some(path) = std::env::var_os("WMS_CONSOLE_ASSETS_FILE") else {
            return Self::empty();
        };
        let loaded = (|| {
            let path = FsPath::new(&path)
                .canonicalize()
                .map_err(|_| "Unavailable console assets")?;
            let mut bytes = Vec::new();
            std::fs::File::open(&path)
                .map_err(|_| "Unavailable console assets")?
                .take(CONFIG_LIMIT + 1)
                .read_to_end(&mut bytes)
                .map_err(|_| "Unavailable console assets")?;
            Self::parse(
                &bytes,
                path.parent()
                    .ok_or("Missing asset directory")?
                    .to_path_buf(),
            )
        })();
        loaded.unwrap_or_else(|_: String| Self {
            valid: false,
            ..Self::empty()
        })
    }
    fn check(&self) -> Result<(), Error> {
        if self.valid {
            Ok(())
        } else {
            Err(Error::new(
                503,
                "Visual console asset configuration requires review",
            ))
        }
    }
    fn facility(&self, tenant: &str, warehouse: &str) -> Option<&Facility> {
        self.config
            .facilities
            .iter()
            .find(|row| row.tenant == tenant && row.warehouse_id == warehouse)
    }
    async fn image(
        &self,
        tenant: &str,
        warehouse: &str,
        sku: &str,
    ) -> Result<Option<Vec<u8>>, Error> {
        let Some(file) = self
            .facility(tenant, warehouse)
            .and_then(|row| row.product_images.get(sku))
        else {
            return Ok(None);
        };
        let path = tokio::fs::canonicalize(self.root.join(file))
            .await
            .map_err(|_| Error::new(404, "Product image unavailable"))?;
        if !path.starts_with(&self.root) {
            return Err(Error::new(404, "Product image unavailable"));
        }
        let file = tokio::fs::File::open(path)
            .await
            .map_err(|_| Error::new(404, "Product image unavailable"))?;
        if !file
            .metadata()
            .await
            .map_err(|_| Error::new(404, "Product image unavailable"))?
            .is_file()
        {
            return Err(Error::new(404, "Product image unavailable"));
        }
        let mut bytes = Vec::new();
        file.take((IMAGE_LIMIT + 1) as u64)
            .read_to_end(&mut bytes)
            .await
            .map_err(|_| Error::new(404, "Product image unavailable"))?;
        png_dimensions(&bytes)?;
        Ok(Some(bytes))
    }
}

pub fn png_dimensions(bytes: &[u8]) -> Result<(u32, u32), Error> {
    let invalid = || Error::new(422, "Product image must be a bounded static PNG");
    if bytes.len() > IMAGE_LIMIT
        || bytes.get(..8) != Some(b"\x89PNG\r\n\x1a\n")
        || bytes.get(8..16) != Some(b"\0\0\0\rIHDR")
    {
        return Err(invalid());
    }
    let width = u32::from_be_bytes(
        bytes
            .get(16..20)
            .ok_or_else(invalid)?
            .try_into()
            .map_err(|_| invalid())?,
    );
    let height = u32::from_be_bytes(
        bytes
            .get(20..24)
            .ok_or_else(invalid)?
            .try_into()
            .map_err(|_| invalid())?,
    );
    if width == 0
        || height == 0
        || width > 2048
        || height > 2048
        || !bytes.get(24).is_some_and(|n| [1, 2, 4, 8].contains(n))
        || !bytes.get(25).is_some_and(|n| [0, 2, 3, 4, 6].contains(n))
        || bytes.get(26) != Some(&0)
        || bytes.get(27) != Some(&0)
        || bytes.get(28).is_none_or(|n| *n > 1)
    {
        return Err(invalid());
    }
    let mut offset = 8usize;
    let mut data = false;
    let mut headers = 0;
    while offset + 12 <= bytes.len() {
        let length = u32::from_be_bytes(
            bytes[offset..offset + 4]
                .try_into()
                .map_err(|_| invalid())?,
        ) as usize;
        let end = offset
            .checked_add(12)
            .and_then(|n| n.checked_add(length))
            .filter(|n| *n <= bytes.len())
            .ok_or_else(invalid)?;
        let kind = &bytes[offset + 4..offset + 8];
        if crc32(&bytes[offset + 4..end - 4])
            != u32::from_be_bytes(bytes[end - 4..end].try_into().map_err(|_| invalid())?)
        {
            return Err(invalid());
        }
        if kind == b"IHDR" {
            headers += 1;
            if headers != 1 {
                return Err(invalid());
            }
        }
        if kind == b"acTL" || kind == b"fcTL" || kind == b"fdAT" {
            return Err(invalid());
        }
        if kind == b"IDAT" {
            data = true;
        }
        if kind == b"IEND" {
            return if length == 0 && end == bytes.len() && data {
                Ok((width, height))
            } else {
                Err(invalid())
            };
        }
        offset = end;
    }
    Err(invalid())
}
fn crc32(bytes: &[u8]) -> u32 {
    static TABLE: std::sync::LazyLock<[u32; 256]> = std::sync::LazyLock::new(|| {
        std::array::from_fn(|n| {
            let mut value = n as u32;
            for _ in 0..8 {
                value = if value & 1 != 0 {
                    0xedb88320 ^ (value >> 1)
                } else {
                    value >> 1
                };
            }
            value
        })
    });
    !bytes.iter().fold(0xffffffff, |crc, byte| {
        TABLE[((crc ^ u32::from(*byte)) & 255) as usize] ^ (crc >> 8)
    })
}

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Page {
    pub after: Option<String>,
    pub review: Option<String>,
    pub limit: Option<usize>,
}
impl Page {
    fn validate(&self) -> Result<(), Error> {
        if !(1..=64).contains(&self.limit.unwrap_or(32))
            || self.after.as_ref().is_some_and(|s| !identity(s))
            || self.after.is_some() && self.review.is_none()
            || self.review.as_ref().is_some_and(|s| {
                s.len() > 19
                    || s.is_empty()
                    || !s.bytes().all(|b| b.is_ascii_digit())
                    || s.len() > 1 && s.starts_with('0')
            })
        {
            return Err(Error::new(
                422,
                "Visual bin pages require a bounded limit and original review",
            ));
        }
        Ok(())
    }
}
fn check_review(state: &WarehouseState, review: Option<&str>) -> Result<(), Error> {
    if review.is_some_and(|version| version != state.version.to_string()) {
        return Err(Error::new(
            409,
            "Warehouse changed; refresh the visual review",
        ));
    }
    Ok(())
}
pub fn project(
    state: &WarehouseState,
    proof_ok: bool,
    tenant: &str,
    assets: &Assets,
    page: &Page,
) -> Result<Value, Error> {
    assets.check()?;
    page.validate()?;
    if !proof_ok {
        return Err(Error::new(
            409,
            "Inventory reconciliation is held; refresh and review warehouse evidence",
        ));
    }
    let limit = page.limit.unwrap_or(32);
    check_review(state, page.review.as_deref())?;
    let mut bins: BTreeMap<&str, (&crate::domain::Location, i128, i128, usize)> = state
        .locations
        .iter()
        .map(|bin| (bin.code.as_str(), (bin, 0, 0, 0)))
        .collect();
    let mut physical = 0i128;
    let mut reserved = 0i128;
    for stock in &state.stocks {
        let bin = bins
            .get_mut(stock.location.as_str())
            .ok_or_else(|| Error::new(409, "Stock bin evidence is held"))?;
        bin.1 += i128::from(stock.on_hand);
        bin.2 += i128::from(stock.reserved);
        bin.3 += 1;
        physical += i128::from(stock.on_hand);
        reserved += i128::from(stock.reserved);
    }
    let all = bins
        .iter()
        .filter(|(code, _)| page.after.as_deref().is_none_or(|after| **code > after))
        .collect::<Vec<_>>();
    let shown = all
        .iter()
        .take(limit)
        .map(|(code, (bin, on_hand, reserved, positions))| {
            json!({
                "code": code,
                "zone": bin.zone,
                "kind": bin.kind,
                "on_hand_each": on_hand.to_string(),
                "reserved_each": reserved.to_string(),
                "lot_positions": positions
            })
        })
        .collect::<Vec<_>>();
    let next_after = if all.len() > limit {
        all.get(limit - 1).map(|(code, _)| **code)
    } else {
        None
    };
    let layout = assets
        .facility(tenant, &state.warehouse.id)
        .and_then(|row| row.layout.as_ref());
    let plan = match layout {
        Some(layout)
            if layout
                .bins
                .iter()
                .all(|bin| bins.contains_key(bin.code.as_str())) =>
        {
            json!({
                "available": true,
                "revision": assets.revision,
                "name": layout.name,
                "unit": layout.unit,
                "width": layout.width,
                "height": layout.height,
                "mapped_bins": layout.bins.len(),
                "total_bins": bins.len(),
                "bins": layout.bins.iter().map(|p| {let bin = bins[p.code.as_str()]; json!({"code": p.code, "x": p.x, "y": p.y, "width": p.width, "height": p.height, "on_hand_each": bin.1.to_string(), "reserved_each": bin.2.to_string()})}).collect::<Vec<_>>()
            })
        }
        Some(_) => json!({"available": false, "reason": "bin_sources_changed"}),
        None => json!({"available": false, "reason": "not_configured"}),
    };
    Ok(json!({
        "schema_version": 1,
        "tenant": tenant,
        "warehouse_id": state.warehouse.id,
        "warehouse_name": state.warehouse.name,
        "version": state.version.to_string(),
        "captured_at": crate::domain::now(),
        "source_ok": true,
        "unit": "each",
        "flow": FLOW.iter().map(|status| json!({"status": status, "orders": state.orders.iter().filter(|o| o.status == *status).count().to_string()})).collect::<Vec<_>>(),
        "open_exceptions": state.exceptions.iter().filter(|e| e.status == "open").count().to_string(),
        "stock": {"on_hand_each": physical.to_string(), "reserved_each": reserved.to_string()},
        "bins": {"items": shown, "total": bins.len(), "next_after": next_after},
        "floor_plan": plan
    }))
}

async fn overview(
    State(app): State<App>,
    Extension(identity): Extension<Identity>,
    Extension(assets): Extension<Arc<Assets>>,
    Path(warehouse): Path<String>,
    query: Result<Query<Page>, axum::extract::rejection::QueryRejection>,
) -> Result<Json<Value>, Error> {
    let Query(page) = query.map_err(|_| Error::new(422, "Invalid visual console query"))?;
    page.validate()?;
    let snapshot = app.store.snapshot(&identity.tenant, &warehouse).await?;
    let (state, ok) = snapshot.console_source();
    let mut value = project(state, ok, &identity.tenant, &assets, &page)?;
    value["demo"] = json!(app.demo);
    Ok(Json(value))
}
#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProductQuery {
    review: Option<String>,
    sha256: Option<String>,
    scope: Option<String>,
}
fn image_scope(identity: &Identity, warehouse: &str) -> String {
    let bytes = serde_json::to_vec(&(&identity.tenant, &identity.actor, &identity.role, warehouse))
        .expect("identity strings serialize");
    format!("{:x}", Sha256::digest(bytes))
}
async fn product(
    State(app): State<App>,
    Extension(identity): Extension<Identity>,
    Extension(assets): Extension<Arc<Assets>>,
    Path((warehouse, sku)): Path<(String, String)>,
    query: Result<Query<ProductQuery>, axum::extract::rejection::QueryRejection>,
) -> Result<Json<Value>, Error> {
    let Query(query) = query.map_err(|_| Error::new(422, "Invalid product preview query"))?;
    if query.sha256.is_some() || query.scope.is_some() {
        return Err(Error::new(
            422,
            "Image digests belong to the image endpoint",
        ));
    }
    Page {
        review: query.review.clone(),
        ..Page::default()
    }
    .validate()?;
    assets.check()?;
    let snapshot = app.store.snapshot(&identity.tenant, &warehouse).await?;
    let (state, ok) = snapshot.console_source();
    if !ok {
        return Err(Error::new(409, "Inventory reconciliation is held"));
    }
    check_review(state, query.review.as_deref())?;
    let product = state
        .products
        .iter()
        .find(|p| p.sku == sku)
        .ok_or_else(|| Error::new(404, "Product not found in this warehouse"))?;
    let (image, image_status) = match assets.image(&identity.tenant, &warehouse, &sku).await {
        Ok(Some(bytes)) => {
            let (width, height) = png_dimensions(&bytes)?;
            (
                json!({"sha256": format!("{:x}", Sha256::digest(&bytes)), "width": width, "height": height, "bytes": bytes.len()}),
                "ready",
            )
        }
        Ok(None) => (Value::Null, "not_configured"),
        Err(_) => {
            tracing::warn!("Configured console product image is unavailable; catalog evidence remains readable");
            (Value::Null, "unavailable")
        }
    };
    Ok(Json(json!({
        "schema_version": 1,
        "demo": app.demo,
        "tenant": identity.tenant,
        "image_scope": image_scope(&identity, &warehouse),
        "warehouse_id": warehouse,
        "version": state.version.to_string(),
        "captured_at": crate::domain::now(),
        "source_ok": true,
        "sku": product.sku,
        "name": product.name,
        "category": product.category,
        "serial_controlled": product.serial_controlled,
        "expiry_required": product.expiry_required,
        "reorder_point_each": product.reorder_point.to_string(),
        "label": {"value": product.sku, "unit": "each", "symbology": if product.sku.bytes().all(|b| (32..=126).contains(&b)) && product.sku.len() <= 80 {Some("code128")} else {None}},
        "image": image,
        "image_status": image_status
    })))
}
async fn image(
    State(app): State<App>,
    Extension(identity): Extension<Identity>,
    Extension(assets): Extension<Arc<Assets>>,
    Path((warehouse, sku)): Path<(String, String)>,
    query: Result<Query<ProductQuery>, axum::extract::rejection::QueryRejection>,
) -> Result<Response, Error> {
    let Query(query) = query.map_err(|_| Error::new(422, "Invalid product image query"))?;
    Page {
        review: query.review.clone(),
        ..Page::default()
    }
    .validate()?;
    assets.check()?;
    if query.review.is_none()
        || !query
            .sha256
            .as_ref()
            .is_some_and(|v| v.len() == 64 && v.bytes().all(|b| b.is_ascii_hexdigit()))
    {
        return Err(Error::new(
            422,
            "Product images require the original review and image digest",
        ));
    }
    if query.scope.as_deref() != Some(image_scope(&identity, &warehouse).as_str()) {
        return Err(Error::new(
            409,
            "Image review identity changed; reconnect and refresh",
        ));
    }
    let snapshot = app.store.snapshot(&identity.tenant, &warehouse).await?;
    let (state, ok) = snapshot.console_source();
    if !ok {
        return Err(Error::new(409, "Inventory reconciliation is held"));
    }
    check_review(state, query.review.as_deref())?;
    if !state.products.iter().any(|p| p.sku == sku) {
        return Err(Error::new(404, "Product not found in this warehouse"));
    }
    let bytes = assets
        .image(&identity.tenant, &warehouse, &sku)
        .await?
        .ok_or_else(|| Error::new(404, "Product image unavailable"))?;
    let sha = format!("{:x}", Sha256::digest(&bytes));
    if query.sha256.as_deref() != Some(&sha) {
        return Err(Error::new(
            409,
            "Product image changed; refresh the preview",
        ));
    }
    let mut headers = HeaderMap::new();
    headers.insert(
        header::CONTENT_TYPE,
        "image/png".parse().expect("static MIME"),
    );
    headers.insert(
        header::CACHE_CONTROL,
        "no-store".parse().expect("static cache policy"),
    );
    Ok((headers, bytes).into_response())
}
pub fn router_with_assets(assets: Assets) -> Router<App> {
    Router::new()
        .route("/v1/warehouses/{warehouse}/console", get(overview))
        .route(
            "/v1/warehouses/{warehouse}/console/products/{sku}",
            get(product),
        )
        .route(
            "/v1/warehouses/{warehouse}/console/products/{sku}/image",
            get(image),
        )
        .layer(Extension(Arc::new(assets)))
}
pub fn router() -> Router<App> {
    router_with_assets(Assets::from_env())
}
