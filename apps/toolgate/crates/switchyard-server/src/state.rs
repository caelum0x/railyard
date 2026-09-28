//! Shared application state and flow control (per-provider token buckets,
//! circuit breakers, latency windows).

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use switchyard_adapters::AdapterMap;
use switchyard_auth::oauth::AuthServer;
use switchyard_auth::{IdentityProvider, KeyVerifier, TokenSigner};
use switchyard_cache::{ResponseCache, SingleFlight};
use switchyard_catalog::Catalog;
use switchyard_core::HttpTransport;
use switchyard_meter::{Caps, MintTransport, RagoTransport};
use switchyard_store::Store;
use switchyard_vault::Vault;

use crate::config::Settings;
use crate::pipeline::Shared;

#[derive(Clone)]
pub struct AppState(pub Arc<Inner>);

impl std::ops::Deref for AppState {
    type Target = Inner;
    fn deref(&self) -> &Inner {
        &self.0
    }
}

pub struct Inner {
    pub settings: Settings,
    pub catalog: Arc<Catalog>,
    pub adapters: AdapterMap,
    pub store: Arc<dyn Store>,
    pub keys: Arc<dyn KeyVerifier>,
    pub signer: Arc<TokenSigner>,
    pub oauth: Arc<AuthServer>,
    pub identity: Arc<dyn IdentityProvider>,
    pub vault: Arc<Vault>,
    pub http: Arc<dyn HttpTransport>,
    pub mint: Option<Arc<dyn MintTransport>>,
    pub rago: Option<Arc<dyn RagoTransport>>,
    pub caps: Arc<Caps>,
    pub cache: ResponseCache,
    pub flights: SingleFlight<Shared>,
    pub flow: FlowControl,
    pub adapter_config: Arc<BTreeMap<String, String>>,
    pub started: Instant,
}

/// Everything needed to build [`AppState`].
pub struct Deps {
    pub settings: Settings,
    pub catalog: Catalog,
    pub store: Arc<dyn Store>,
    pub keys: Arc<dyn KeyVerifier>,
    pub identity: Arc<dyn IdentityProvider>,
    pub vault: Vault,
    pub http: Arc<dyn HttpTransport>,
    pub mint: Option<Arc<dyn MintTransport>>,
    pub rago: Option<Arc<dyn RagoTransport>>,
    pub signer: TokenSigner,
}

impl AppState {
    pub fn new(d: Deps) -> anyhow::Result<Self> {
        let adapters = switchyard_adapters::build(&d.catalog).map_err(anyhow::Error::msg)?;
        let signer = Arc::new(d.signer);
        let oauth = Arc::new(AuthServer {
            store: d.store.clone(),
            signer: signer.clone(),
            cfg: switchyard_auth::oauth::OAuthConfig::for_issuer(&d.settings.public_url),
            http: Some(d.http.clone()),
        });
        let adapter_config = Arc::new(d.settings.adapter_config.clone());
        let flow = FlowControl::new(&d.catalog);
        Ok(AppState(Arc::new(Inner {
            cache: ResponseCache::new(d.settings.cache_bytes),
            settings: d.settings,
            catalog: Arc::new(d.catalog),
            adapters,
            store: d.store,
            keys: d.keys,
            signer,
            oauth,
            identity: d.identity,
            vault: Arc::new(d.vault),
            http: d.http,
            mint: d.mint,
            rago: d.rago,
            caps: Caps::new(),
            flights: SingleFlight::default(),
            flow,
            adapter_config,
            started: Instant::now(),
        })))
    }
}

struct Bucket {
    tokens: f64,
    last: Instant,
    rate: f64,
    burst: f64,
}

struct Breaker {
    consecutive_failures: u32,
    open_until: Option<Instant>,
}

pub struct FlowControl {
    buckets: Mutex<HashMap<String, Bucket>>,
    breakers: Mutex<HashMap<String, Breaker>>,
    latencies: Mutex<HashMap<String, VecDeque<u32>>>,
    pub breaker_threshold: u32,
    pub breaker_cooldown: Duration,
}

impl FlowControl {
    pub fn new(catalog: &Catalog) -> Self {
        let mut buckets = HashMap::new();
        for p in catalog.providers() {
            if let Some(rate) = p.rate_per_s {
                let burst = p.burst.unwrap_or(1).max(1) as f64;
                buckets.insert(p.id.clone(), Bucket { tokens: burst, last: Instant::now(), rate, burst });
            }
        }
        FlowControl {
            buckets: Mutex::new(buckets),
            breakers: Mutex::new(HashMap::new()),
            latencies: Mutex::new(HashMap::new()),
            breaker_threshold: 3,
            breaker_cooldown: Duration::from_secs(60),
        }
    }

    /// Take one token for `provider`, waiting at most `max_wait`. Providers
    /// without a declared rate are unlimited.
    pub async fn acquire(&self, provider: &str, max_wait: Duration) -> bool {
        let deadline = Instant::now() + max_wait;
        loop {
            let wait = {
                let mut b = self.buckets.lock().expect("buckets poisoned");
                let Some(bucket) = b.get_mut(provider) else { return true };
                let now = Instant::now();
                bucket.tokens = (bucket.tokens + now.duration_since(bucket.last).as_secs_f64() * bucket.rate).min(bucket.burst);
                bucket.last = now;
                if bucket.tokens >= 1.0 {
                    bucket.tokens -= 1.0;
                    return true;
                }
                Duration::from_secs_f64((1.0 - bucket.tokens) / bucket.rate)
            };
            if Instant::now() + wait > deadline {
                return false;
            }
            tokio::time::sleep(wait).await;
        }
    }

    pub fn is_open(&self, provider: &str) -> bool {
        let b = self.breakers.lock().expect("breakers poisoned");
        b.get(provider).and_then(|x| x.open_until).map(|t| t > Instant::now()).unwrap_or(false)
    }

    pub fn record(&self, provider: &str, tool: &str, success: bool, latency_ms: u32) {
        {
            let mut b = self.breakers.lock().expect("breakers poisoned");
            let e = b.entry(provider.to_string()).or_insert(Breaker { consecutive_failures: 0, open_until: None });
            if success {
                e.consecutive_failures = 0;
                e.open_until = None;
            } else {
                e.consecutive_failures += 1;
                if e.consecutive_failures >= self.breaker_threshold {
                    e.open_until = Some(Instant::now() + self.breaker_cooldown);
                    tracing::warn!(provider, "circuit open for {:?}", self.breaker_cooldown);
                }
            }
        }
        let mut l = self.latencies.lock().expect("latency poisoned");
        let w = l.entry(tool.to_string()).or_default();
        if w.len() >= 200 {
            w.pop_front();
        }
        w.push_back(latency_ms);
    }

    /// `(p50, p95)` over the last 200 calls of a tool.
    pub fn percentiles(&self, tool: &str) -> Option<(u32, u32)> {
        let l = self.latencies.lock().expect("latency poisoned");
        let w = l.get(tool)?;
        if w.is_empty() {
            return None;
        }
        let mut v: Vec<u32> = w.iter().copied().collect();
        v.sort_unstable();
        let at = |q: f64| v[((v.len() as f64 - 1.0) * q).round() as usize];
        Some((at(0.5), at(0.95)))
    }
}
