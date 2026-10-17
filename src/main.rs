use stateset_warehouse::{
    api::{self, App},
    integrations::Connections,
    store::{Identity, Store},
};
use std::{collections::BTreeMap, sync::Arc, time::Duration};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    dotenvy::dotenv().ok();
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "stateset_warehouse=info,tower_http=info".into()),
        )
        .init();
    let demo = std::env::var("WMS_DEMO").as_deref() == Ok("true");
    let bind = std::env::var("WMS_BIND").unwrap_or_else(|_| "127.0.0.1:8088".into());
    let address: std::net::SocketAddr = bind.parse()?;
    if demo && !address.ip().is_loopback() {
        return Err("Demo API must bind to a loopback address".into());
    }
    let workos_config = stateset_warehouse::auth::WorkOsConfig::from_env()?;
    if demo && workos_config.is_some() {
        return Err("Demo mode cannot enable WorkOS".into());
    }
    let workos = workos_config
        .clone()
        .map(stateset_warehouse::auth::WorkOs::new)
        .transpose()?;
    let keys: Vec<(String, Identity)> = if demo {
        vec![(
            "demo-warehouse".into(),
            Identity {
                tenant: "demo".into(),
                actor: "operator:demo".into(),
                role: "admin".into(),
            },
        )]
    } else {
        let map: BTreeMap<String, Identity> = serde_json::from_str(
            &std::env::var("WMS_API_KEYS_JSON")
                .ok()
                .filter(|raw| !raw.trim().is_empty())
                .unwrap_or_else(|| "{}".into()),
        )?;
        if (map.is_empty() && workos.is_none())
            || map.iter().any(|(k, i)| {
                k.len() < 32
                    || i.actor.starts_with("workos:")
                    || i.tenant.trim().is_empty()
                    || i.actor.trim().is_empty()
                    || !["viewer", "operator", "admin", "oms"].contains(&i.role.as_str())
            })
        {
            return Err("Configure WorkOS or API keys of at least 32 characters with valid tenant/actor/role; workos: actors are reserved".into());
        }
        if map.values().any(|i| i.tenant == "demo") {
            return Err("The demo tenant is reserved for synthetic fixtures".into());
        }
        map.into_iter().collect()
    };
    let mut tenants: std::collections::BTreeSet<String> =
        keys.iter().map(|(_, i)| i.tenant.clone()).collect();
    if let Some(config) = &workos_config {
        tenants.extend(config.organizations.values().map(|m| m.tenant.clone()));
    }
    let native = stateset_warehouse::native::NativeIntegrations::from_env()
        .map_err(|e| format!("Native integration configuration: {e}"))?;
    if demo && !native.tenants.is_empty() {
        return Err("Demo mode cannot connect to native OMS or TMS services".into());
    }
    if native
        .tenants
        .keys()
        .any(|tenant| !tenants.contains(tenant))
    {
        return Err("Every native integration tenant must have a configured identity".into());
    }
    let store = Store::connect(
        &std::env::var("DATABASE_URL")
            .map_err(|_| "DATABASE_URL is required; there is no in-memory fallback")?,
    )
    .await
    .map_err(|e| e.message)?;
    for tenant in &tenants {
        store.provision(tenant, demo).await.map_err(|e| e.message)?;
    }
    let mut connections = BTreeMap::new();
    for destination in ["oms", "tms"] {
        let prefix = format!("WMS_{}_WEBHOOK", destination.to_uppercase());
        match (
            std::env::var(format!("{prefix}_URL")),
            std::env::var(format!("{prefix}_TOKEN")),
        ) {
            (Ok(url), Ok(token)) => {
                if demo {
                    return Err("Demo mode cannot deliver events to live receivers".into());
                }
                let url = reqwest::Url::parse(&url)?;
                if url.scheme() != "https"
                    || !url.username().is_empty()
                    || url.password().is_some()
                    || token.len() < 16
                {
                    return Err("Receivers require HTTPS without URL credentials and a token of at least 16 characters".into());
                }
                let tenant = std::env::var("WMS_WEBHOOK_TENANT")
                    .map_err(|_| "WMS_WEBHOOK_TENANT is required to scope event delivery")?;
                if !tenants.contains(&tenant) {
                    return Err("Webhook tenant must have a configured identity".into());
                }
                connections.insert((tenant, destination.into()), (url.to_string(), token));
            }
            (Err(_), Err(_)) => {}
            _ => return Err("Receiver URL and token must be configured together".into()),
        }
    }
    let connections = Connections(Arc::new(connections));
    let worker_store = store.clone();
    let worker_connections = connections.clone();
    let (shutdown_tx, mut shutdown_rx) = tokio::sync::watch::channel(false);
    let worker = tokio::spawn(async move {
        loop {
            tokio::select! { _=shutdown_rx.changed()=>break,_=tokio::time::sleep(Duration::from_secs(2))=>{} }
            for _ in 0..20 {
                if *shutdown_rx.borrow() {
                    return;
                }
                match worker_connections.dispatch_one(&worker_store).await {
                    Ok(true) => {}
                    Ok(false) => break,
                    Err(e) => {
                        tracing::warn!(error=%e.message,"outbox delivery deferred");
                        break;
                    }
                }
            }
        }
    });
    let listener = tokio::net::TcpListener::bind(address).await?;
    tracing::info!(%address,demo,"StateSet WMS listening");
    let app = App {
        store,
        credentials: Arc::new(keys),
        demo,
        connections,
    };
    axum::serve(listener, api::router_with_auth(app, workos))
        .with_graceful_shutdown(async move {
            #[cfg(unix)]
            {
                let mut terminate =
                    tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                        .expect("install SIGTERM handler");
                tokio::select! {_=tokio::signal::ctrl_c()=>{},_=terminate.recv()=>{}}
            }
            #[cfg(not(unix))]
            {
                tokio::signal::ctrl_c().await.ok();
            }
            let _ = shutdown_tx.send(true);
        })
        .await?;
    let _ = tokio::time::timeout(Duration::from_secs(15), worker).await;
    Ok(())
}
