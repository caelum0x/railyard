//! DNS resolution seam, validation and pinning.

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use switchyard_core::EgressError;

use crate::policy::is_blocked_ip;

#[async_trait]
pub trait Resolver: Send + Sync {
    async fn resolve(&self, host: &str) -> Result<Vec<IpAddr>, String>;
}

/// The operating system resolver.
pub struct SystemResolver;

#[async_trait]
impl Resolver for SystemResolver {
    async fn resolve(&self, host: &str) -> Result<Vec<IpAddr>, String> {
        let addrs = tokio::net::lookup_host((host, 443)).await.map_err(|e| e.to_string())?;
        Ok(addrs.map(|a| a.ip()).collect())
    }
}

/// Resolves once, refuses if any address is blocked, and pins the result so
/// the connection uses exactly the validated addresses.
pub struct PinningResolver {
    inner: Arc<dyn Resolver>,
    pins: Mutex<HashMap<String, Vec<IpAddr>>>,
    /// Test-only exceptions (e.g. 127.0.0.1 for an in-process fake upstream).
    allow_ips: Vec<IpAddr>,
}

impl PinningResolver {
    pub fn new(inner: Arc<dyn Resolver>, allow_ips: Vec<IpAddr>) -> Self {
        PinningResolver { inner, pins: Mutex::new(HashMap::new()), allow_ips }
    }

    pub fn ip_allowed(&self, ip: IpAddr) -> bool {
        self.allow_ips.contains(&ip) || !is_blocked_ip(ip)
    }

    /// Resolve and validate `host`, then pin the result.
    pub async fn resolve_and_pin(&self, host: &str) -> Result<Vec<IpAddr>, EgressError> {
        let ips = self.inner.resolve(host).await.map_err(|e| EgressError::Network(format!("dns {host}: {e}")))?;
        if ips.is_empty() {
            return Err(EgressError::Network(format!("dns {host}: no addresses")));
        }
        if let Some(bad) = ips.iter().find(|ip| !self.ip_allowed(**ip)) {
            return Err(EgressError::BlockedAddress(format!("{host} -> {bad}")));
        }
        let mut pins = self.pins.lock().expect("pin map poisoned");
        if pins.len() > 1024 {
            pins.clear();
        }
        pins.insert(host.to_ascii_lowercase(), ips.clone());
        Ok(ips)
    }

    pub fn pinned(&self, host: &str) -> Option<Vec<IpAddr>> {
        self.pins.lock().expect("pin map poisoned").get(&host.to_ascii_lowercase()).cloned()
    }
}

/// reqwest's resolver hook: serve only pinned (already validated) addresses.
/// An unpinned host is refused, so reqwest can never resolve on its own.
pub(crate) struct ReqwestPins(pub Arc<PinningResolver>);

impl reqwest::dns::Resolve for ReqwestPins {
    fn resolve(&self, name: reqwest::dns::Name) -> reqwest::dns::Resolving {
        let pins = self.0.clone();
        let host = name.as_str().to_string();
        Box::pin(async move {
            match pins.pinned(&host) {
                Some(ips) => {
                    let addrs: Vec<SocketAddr> = ips.into_iter().map(|ip| SocketAddr::new(ip, 0)).collect();
                    Ok(Box::new(addrs.into_iter()) as reqwest::dns::Addrs)
                }
                None => Err(format!("egress: host {host} was not validated").into()),
            }
        })
    }
}
