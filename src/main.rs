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
                .map_err(|_| "WMS_API_KEYS_JSON is required in real mode")?,
        )?;
        if map.is_empty()
            || map.iter().any(|(k, i)| {
                k.len() < 32
                    || i.tenant.trim().is_empty()
                    || i.actor.trim().is_empty()
                    || !["viewer", "operator", "admin", "oms"].contains(&i.role.as_str())
            })
        {
            return Err("Configure nonempty tenant/actor, valid role and API keys of at least 32 characters".into());
        }
        if map.values().any(|i| i.tenant == "demo") {
            return Err("The demo tenant is reserved for synthetic fixtures".into());
        }
        map.into_iter().collect()
    };
    let store = Store::connect(
        &std::env::var("DATABASE_URL")
            .map_err(|_| "DATABASE_URL is required; there is no in-memory fallback")?,
    )
    .await
    .map_err(|e| e.message)?;
    for (_, identity) in &keys {
        store
            .provision(&identity.tenant, demo)
            .await
            .map_err(|e| e.message)?;
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
                if !keys.iter().any(|(_, i)| i.tenant == tenant) {
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
    axum::serve(listener, api::router(app))
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
