//! `switchyard-server`: serve (default), `migrate`, `gen-kek`, `rotate-kek`.
//!
//! Environment (see README): SWITCHYARD_DATABASE_URL (or SWITCHYARD_STORE=memory
//! for a throwaway dev run), SWITCHYARD_KEK, SWITCHYARD_PUBLIC_URL,
//! RELAY_DATABASE_URL, MINT_SERVER_URL/MINT_API_KEY/SWITCHYARD_MINT_REVENUE_ACCOUNT,
//! RAGO_URL/RAGO_API_KEY, SWITCHYARD_SCRIBASE_API_URL, SWITCHYARD_JWT_SEED,
//! SWITCHYARD_ADMIN_TOKEN, product URLs (SWITCHYARD_RALLY_URL, ...).

use std::sync::Arc;
use std::time::Duration;

use anyhow::{bail, Context};
use switchyard_auth::{IdentityProvider, KeyVerifier, MemRelayKeys, PgRelayKeys, ScribaseIdentity, StaticIdentity, TokenSigner};
use switchyard_catalog::Catalog;
use switchyard_core::HttpTransport;
use switchyard_egress::{EgressConfig, SafeClient};
use switchyard_meter::{MintClient, MintTransport, OutboxWorker, RagoClient, RagoTransport};
use switchyard_server::{egress_allowlist, router, AppState, Deps, Settings};
use switchyard_store::{MemStore, PgStore, Store};
use switchyard_vault::{Vault, WrappedDek};

fn env(k: &str) -> Option<String> {
    std::env::var(k).ok().filter(|v| !v.trim().is_empty())
}

fn vault() -> anyhow::Result<Vault> {
    match env("SWITCHYARD_KEK") {
        Some(k) => Vault::from_env_values(&k, env("SWITCHYARD_KEK_VERSION").as_deref(), env("SWITCHYARD_KEK_PREVIOUS").as_deref()).map_err(|e| anyhow::anyhow!("SWITCHYARD_KEK: {e}")),
        None if env("SWITCHYARD_STORE").as_deref() == Some("memory") => {
            tracing::warn!("SWITCHYARD_KEK not set: using an ephemeral key (memory dev mode only)");
            Ok(Vault::new(1, &Vault::generate_kek_b64())?)
        }
        None => bail!("SWITCHYARD_KEK is required (32 bytes, base64); generate one with `switchyard-server gen-kek`"),
    }
}

async fn pg_store(catalog: &Catalog) -> anyhow::Result<PgStore> {
    let url = env("SWITCHYARD_DATABASE_URL").context("SWITCHYARD_DATABASE_URL is required (or SWITCHYARD_STORE=memory for dev)")?;
    let pg = PgStore::connect(&url).await.context("connect/migrate Postgres")?;
    let providers: Vec<_> = catalog.providers().cloned().collect();
    let caps: Vec<_> = catalog.capabilities().cloned().collect();
    let tools: Vec<_> = catalog.tools().cloned().collect();
    pg.sync_catalog(&providers, &caps, &tools).await.context("sync catalog")?;
    Ok(pg)
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt().with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into())).init();
    let cmd = std::env::args().nth(1).unwrap_or_else(|| "serve".into());
    let catalog_dir = env("SWITCHYARD_CATALOG_DIR").unwrap_or_else(|| "catalog".into());
    let catalog = Catalog::load_dir(&catalog_dir).with_context(|| format!("load catalog from {catalog_dir}"))?;

    match cmd.as_str() {
        "gen-kek" => {
            println!("{}", Vault::generate_kek_b64());
            return Ok(());
        }
        "migrate" => {
            pg_store(&catalog).await?;
            println!("migrations applied and catalog synced ({} tools)", catalog.tools().count());
            return Ok(());
        }
        "rotate-kek" => {
            let pg = pg_store(&catalog).await?;
            let v = vault()?;
            let mut n = 0;
            for k in pg.list_org_keys().await? {
                let w = WrappedDek { wrapped: k.dek_wrapped.clone(), kek_version: k.kek_version as u32 };
                let r = v.rewrap(&w, &k.org_id).map_err(|e| anyhow::anyhow!("org {}: {e}", k.org_id))?;
                if r.kek_version != w.kek_version {
                    pg.update_org_key(switchyard_store::OrgKey { dek_wrapped: r.wrapped, kek_version: r.kek_version as i32, ..k }).await?;
                    n += 1;
                }
            }
            println!("re-wrapped {n} org data keys under KEK v{}", v.current_version());
            return Ok(());
        }
        "serve" => {}
        other => bail!("unknown command {other}; use serve | migrate | gen-kek | rotate-kek"),
    }

    let settings = Settings::from_env();
    let memory = env("SWITCHYARD_STORE").as_deref() == Some("memory");
    let (store, keys): (Arc<dyn Store>, Arc<dyn KeyVerifier>) = if memory {
        tracing::warn!("SWITCHYARD_STORE=memory: data is lost on restart");
        (Arc::new(MemStore::new()), Arc::new(MemRelayKeys::new()))
    } else {
        let pg = pg_store(&catalog).await?;
        let relay_pool = match env("RELAY_DATABASE_URL") {
            Some(u) => sqlx::postgres::PgPoolOptions::new().max_connections(3).connect(&u).await.context("connect RELAY_DATABASE_URL")?,
            None => pg.pool().clone(),
        };
        (Arc::new(pg), Arc::new(PgRelayKeys::new(relay_pool)))
    };
    let identity: Arc<dyn IdentityProvider> = match env("SWITCHYARD_SCRIBASE_API_URL") {
        Some(u) => Arc::new(ScribaseIdentity::new(&u, env("SWITCHYARD_SCRIBASE_LOGIN_URL").as_deref())),
        None => Arc::new(StaticIdentity::parse(&env("SWITCHYARD_DEV_SESSIONS").unwrap_or_default())),
    };
    let contact = settings.adapter_config.get("contact_email").cloned().unwrap_or_else(|| "ops@railyard.dev".into());
    let egress = SafeClient::new(EgressConfig {
        allow_hosts: egress_allowlist(&catalog, &settings.adapter_config),
        user_agent: format!("Switchyard/{} (+{}/llms.txt; {contact})", env!("CARGO_PKG_VERSION"), settings.public_url),
        ..Default::default()
    })?;
    let http: Arc<dyn HttpTransport> = Arc::new(egress);
    let mint: Option<Arc<dyn MintTransport>> = match (env("MINT_SERVER_URL"), env("MINT_API_KEY")) {
        (Some(u), Some(k)) => {
            if settings.mint_revenue_account.is_none() {
                bail!("SWITCHYARD_MINT_REVENUE_ACCOUNT is required when Mint is configured");
            }
            Some(Arc::new(MintClient::new(&u, &k)))
        }
        _ => {
            tracing::warn!("Mint not configured: paid tools answer 402, free tools work");
            None
        }
    };
    let rago: Option<Arc<dyn RagoTransport>> = match (env("RAGO_URL"), env("RAGO_API_KEY")) {
        (Some(u), Some(k)) => Some(Arc::new(RagoClient::new(&u, &k))),
        _ => None,
    };
    let signer = TokenSigner::new(env("SWITCHYARD_JWT_SEED").as_deref()).map_err(anyhow::Error::msg)?;
    let st = AppState::new(Deps { settings, catalog, store: store.clone(), keys, identity, vault: vault()?, http, mint: mint.clone(), rago: rago.clone(), signer })?;

    let worker = Arc::new(OutboxWorker { store, rago, mint });
    tokio::spawn(worker.run(Duration::from_secs(5)));

    let bind = env("SWITCHYARD_BIND").unwrap_or_else(|| "0.0.0.0:8130".into());
    let listener = tokio::net::TcpListener::bind(&bind).await.with_context(|| format!("bind {bind}"))?;
    tracing::info!(%bind, tools = st.catalog.tools().count(), "switchyard listening");
    axum::serve(listener, router(st)).with_graceful_shutdown(async {
        let _ = tokio::signal::ctrl_c().await;
    })
    .await?;
    Ok(())
}
