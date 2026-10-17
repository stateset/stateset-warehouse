//! Optional WorkOS AuthKit authentication. Trust is configured by the operator,
//! never discovered from a token's issuer, jku, organization, or role headers.
use crate::store::{Error, Identity};
use jsonwebtoken::{decode, decode_header, Algorithm, DecodingKey, Validation};
use serde::Deserialize;
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    sync::Arc,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tokio::sync::Mutex;

const KEY_TTL: Duration = Duration::from_secs(300);
const REFRESH_COOLDOWN: Duration = Duration::from_secs(30);
const MAX_JWKS_BYTES: usize = 64 * 1024;

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Organization {
    pub tenant: String,
    pub roles: BTreeMap<String, String>,
}
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkOsConfig {
    pub client_id: String,
    pub api_origin: String,
    pub issuer: String,
    pub organizations: BTreeMap<String, Organization>,
    /// AuthKit session tokens normally omit aud. If a deployment emits aud,
    /// require its exact configured value, including presence on every token.
    pub audience: Option<String>,
}
fn identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 160
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
}
impl WorkOsConfig {
    pub fn parse(raw: &str) -> Result<Self, String> {
        let config: Self =
            serde_json::from_str(raw).map_err(|_| "Invalid WMS_WORKOS_JSON schema")?;
        let origin = reqwest::Url::parse(&config.api_origin)
            .map_err(|_| "WorkOS API origin must be HTTPS")?;
        if origin.scheme() != "https"
            || origin.host_str().is_none()
            || origin.port().is_some()
            || !origin.username().is_empty()
            || origin.password().is_some()
            || origin.path() != "/"
            || origin.query().is_some()
            || origin.fragment().is_some()
            || config.api_origin != origin.origin().ascii_serialization()
            || config.issuer != format!("{}/", config.api_origin)
            || !config.client_id.starts_with("client_")
            || !identifier(&config.client_id)
            || config
                .audience
                .as_ref()
                .is_some_and(|a| a.trim().is_empty() || a.len() > 240)
        {
            return Err("WorkOS requires a client ID, canonical HTTPS API origin, and matching issuer with trailing slash".into());
        }
        let mut tenants = std::collections::BTreeSet::new();
        if config.organizations.is_empty()
            || config.organizations.iter().any(|(org, mapping)| {
                !org.starts_with("org_")
                    || !identifier(org)
                    || !identifier(&mapping.tenant)
                    || mapping.tenant == "demo"
                    || !tenants.insert(mapping.tenant.clone())
                    || mapping.roles.is_empty()
                    || mapping.roles.iter().any(|(role, target)| {
                        !identifier(role)
                            || !["viewer", "operator", "admin"].contains(&target.as_str())
                    })
            })
        {
            return Err("WorkOS requires distinct non-demo tenants, explicit organization IDs and viewer/operator/admin role mappings".into());
        }
        Ok(config)
    }
    pub fn from_env() -> Result<Option<Self>, String> {
        match std::env::var("WMS_WORKOS_JSON") {
            Ok(raw) => Self::parse(&raw).map(Some),
            Err(std::env::VarError::NotPresent) => Ok(None),
            Err(_) => Err("WMS_WORKOS_JSON must be UTF-8".into()),
        }
    }
    pub fn public(&self) -> Value {
        json!({"provider":"workos", "clientId":self.client_id, "apiHostname":self.api_origin.trim_start_matches("https://")})
    }
}
#[derive(Default)]
struct KeyCache {
    keys: BTreeMap<String, DecodingKey>,
    fetched: Option<Instant>,
    attempted: Option<Instant>,
}
impl KeyCache {
    fn fresh(&self) -> bool {
        self.fetched.is_some_and(|at| at.elapsed() < KEY_TTL)
    }
    fn fresh_key(&self, kid: &str) -> Option<DecodingKey> {
        self.fresh().then(|| self.keys.get(kid).cloned()).flatten()
    }
}
#[derive(Clone)]
pub struct WorkOs {
    pub config: WorkOsConfig,
    jwks_url: String,
    client: reqwest::Client,
    cache: Arc<Mutex<KeyCache>>,
    refresh: Arc<Mutex<()>>,
}
#[derive(Deserialize)]
struct Claims {
    sub: String,
    sid: String,
    org_id: String,
    role: String,
    iat: u64,
    exp: u64,
    impersonator: Option<Value>,
}
#[derive(Deserialize)]
struct Jwks {
    keys: Vec<Jwk>,
}
#[derive(Deserialize)]
struct Jwk {
    kid: String,
    kty: String,
    n: Option<String>,
    e: Option<String>,
    alg: Option<String>,
    #[serde(rename = "use")]
    usage: Option<String>,
    key_ops: Option<Vec<String>>,
}
fn invalid() -> Error {
    Error::new(401, "Warehouse sign-in is required or has expired")
}
fn unavailable() -> Error {
    Error::new(
        503,
        "Sign-in verification is temporarily unavailable; retry without repeating physical work",
    )
}
impl WorkOs {
    pub fn new(config: WorkOsConfig) -> Result<Self, reqwest::Error> {
        Ok(Self {
            jwks_url: format!("{}/sso/jwks/{}", config.api_origin, config.client_id),
            config,
            client: reqwest::Client::builder()
                .timeout(Duration::from_secs(3))
                .redirect(reqwest::redirect::Policy::none())
                .build()?,
            cache: Arc::new(Mutex::new(KeyCache::default())),
            refresh: Arc::new(Mutex::new(())),
        })
    }
    async fn fetch_keys(&self) -> Result<BTreeMap<String, DecodingKey>, Error> {
        let mut response = self
            .client
            .get(&self.jwks_url)
            .send()
            .await
            .map_err(|_| unavailable())?;
        if response.status() != reqwest::StatusCode::OK
            || response
                .content_length()
                .is_some_and(|n| n > MAX_JWKS_BYTES as u64)
        {
            return Err(unavailable());
        }
        let mut body = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|_| unavailable())? {
            if body.len() + chunk.len() > MAX_JWKS_BYTES {
                return Err(unavailable());
            }
            body.extend_from_slice(&chunk);
        }
        let set: Jwks = serde_json::from_slice(&body).map_err(|_| unavailable())?;
        if set.keys.is_empty() || set.keys.len() > 32 {
            return Err(unavailable());
        }
        let mut keys = BTreeMap::new();
        for key in set.keys {
            if key.kty != "RSA"
                || key.alg.as_deref().is_some_and(|a| a != "RS256")
                || key.usage.as_deref().is_some_and(|u| u != "sig")
                || key
                    .key_ops
                    .as_ref()
                    .is_some_and(|ops| !ops.iter().any(|op| op == "verify"))
            {
                continue;
            }
            if key.kid.is_empty() || key.kid.len() > 200 {
                return Err(unavailable());
            }
            let decoding = DecodingKey::from_rsa_components(
                key.n.as_deref().ok_or_else(unavailable)?,
                key.e.as_deref().ok_or_else(unavailable)?,
            )
            .map_err(|_| unavailable())?;
            if keys.insert(key.kid, decoding).is_some() {
                return Err(unavailable());
            }
        }
        if keys.is_empty() {
            return Err(unavailable());
        }
        Ok(keys)
    }
    async fn key(&self, kid: &str) -> Result<DecodingKey, Error> {
        // Reading a fresh key must never wait for provider I/O triggered by an
        // unknown kid. Only requests that need new keys join the shared refresh.
        if let Some(key) = self.cache.lock().await.fresh_key(kid) {
            return Ok(key);
        }
        let _refresh = self.refresh.lock().await;
        // Recheck after joining: another request may have refreshed the keys,
        // failed, or been cancelled. Its attempt still consumes the cooldown.
        let mut cache = self.cache.lock().await;
        if let Some(key) = cache.fresh_key(kid) {
            return Ok(key);
        }
        if cache
            .attempted
            .is_some_and(|at| at.elapsed() < REFRESH_COOLDOWN)
        {
            return Err(if cache.fresh() {
                invalid()
            } else {
                unavailable()
            });
        }
        cache.attempted = Some(Instant::now());
        drop(cache);
        let keys = self.fetch_keys().await?;
        let mut cache = self.cache.lock().await;
        cache.keys = keys;
        cache.fetched = Some(Instant::now());
        cache.keys.get(kid).cloned().ok_or_else(invalid)
    }
    pub async fn authenticate(&self, token: &str) -> Result<Identity, Error> {
        if token.len() > 16384 {
            return Err(invalid());
        }
        let header = decode_header(token).map_err(|_| invalid())?;
        if header.alg != Algorithm::RS256 {
            return Err(invalid());
        }
        let kid = header
            .kid
            .filter(|k| !k.is_empty() && k.len() <= 200)
            .ok_or_else(invalid)?;
        let key = self.key(&kid).await?;
        let mut validation = Validation::new(Algorithm::RS256);
        validation.set_issuer(&[&self.config.issuer]);
        validation.set_required_spec_claims(&["exp", "iss", "sub", "iat"]);
        validation.leeway = 0;
        validation.validate_nbf = true;
        if let Some(audience) = &self.config.audience {
            validation.set_audience(&[audience]);
            validation.required_spec_claims.insert("aud".into());
        }
        // With no configured audience, the library rejects tokens containing aud.
        let claims = decode::<Claims>(token, &key, &validation)
            .map_err(|_| invalid())?
            .claims;
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| invalid())?
            .as_secs();
        if !claims.sub.starts_with("user_")
            || !identifier(&claims.sub)
            || !identifier(&claims.sid)
            || claims.impersonator.is_some()
            || claims.iat > now.saturating_add(30)
            || claims.exp > now.saturating_add(300)
            || claims.exp <= now
            || claims.exp <= claims.iat
            || claims.exp - claims.iat > 300
        {
            return Err(invalid());
        }
        let mapping = self
            .config
            .organizations
            .get(&claims.org_id)
            .ok_or_else(|| Error::new(403, "This organization has no warehouse access"))?;
        let role = mapping
            .roles
            .get(&claims.role)
            .ok_or_else(|| Error::new(403, "This membership has no warehouse role"))?;
        Ok(Identity {
            tenant: mapping.tenant.clone(),
            actor: format!("workos:{}", claims.sub),
            role: role.clone(),
        })
    }
}

#[cfg(test)]
#[path = "../tests/support/workos.rs"]
mod tests;
