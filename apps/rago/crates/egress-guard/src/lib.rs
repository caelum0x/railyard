//! Outbound requests to user-chosen URLs (webhooks) reach public addresses
//! only, shared by rago (outbound webhooks) and rithub (repository webhooks).
//!
//! - [`check_url`]: the URL's shape (scheme, no credentials, a host). The
//!   WHATWG parser normalises `http://2130706433/`, `http://0177.0.0.1/`,
//!   `http://0x7f.1/` and `http://[::ffff:127.0.0.1]/` to their canonical
//!   address first, so they are judged as addresses.
//! - [`resolve_pinned`]: the host is resolved **once**, every answer must be
//!   public ([`ip::is_blocked`]), and the validated socket addresses are
//!   returned so the connection goes to exactly those (no second lookup, so
//!   a DNS answer that changes between the check and the connect, DNS
//!   rebinding, cannot reach an internal address).
//! - `pinned_client` (feature `reqwest`): a client that connects only to the
//!   pinned addresses, never follows a redirect and ignores proxy settings.
//!
//! `allow_private` lifts the address rule for self-hosted or local receivers
//! (the connection is still pinned).
//!
//! A port of `apps/product-analytics/src/exports/net-guard.ts`.

pub mod ip;

use std::fmt;
use std::future::Future;
use std::io;
use std::net::{IpAddr, SocketAddr};
use std::pin::Pin;

pub use ip::is_blocked;
pub use url::Url;

/// What a destination may be.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Policy {
    /// `http://` as well as `https://`.
    pub allow_http: bool,
    /// Lift the address rule (local development, self-hosted receivers).
    pub allow_private: bool,
}

/// Why a destination was refused (safe to show to the person who set it).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Refused(pub String);

impl fmt::Display for Refused {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Refused {}

fn refuse(msg: impl Into<String>) -> Refused {
    Refused(msg.into())
}

/// Parses and checks a destination's shape. When the host is an address
/// literal (in any of its spellings) it is judged here too, without DNS.
///
/// # Errors
/// [`Refused`] with a plain reason.
pub fn check_url(raw: &str, policy: Policy) -> Result<Url, Refused> {
    let url = Url::parse(raw.trim()).map_err(|_| refuse("not a valid URL"))?;
    match url.scheme() {
        "https" => {}
        "http" if policy.allow_http => {}
        _ if policy.allow_http => return Err(refuse("only http and https URLs are allowed")),
        _ => return Err(refuse("only https URLs are allowed")),
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(refuse("URLs with embedded credentials are not allowed"));
    }
    match url.host() {
        None => return Err(refuse("URL has no host")),
        Some(url::Host::Domain("")) => return Err(refuse("URL has no host")),
        Some(url::Host::Ipv4(v4)) if !policy.allow_private && ip::v4_blocked(v4) => {
            return Err(refuse(format!("{v4} is not a public address")));
        }
        Some(url::Host::Ipv6(v6)) if !policy.allow_private && ip::v6_blocked(v6) => {
            return Err(refuse(format!("[{v6}] is not a public address")));
        }
        Some(_) => {}
    }
    if url.port_or_known_default().is_none() {
        return Err(refuse("URL has no port"));
    }
    Ok(url)
}

/// Resolves a host name to socket addresses.
pub trait Resolver: Send + Sync {
    fn resolve<'a>(&'a self, host: &'a str, port: u16) -> Pin<Box<dyn Future<Output = io::Result<Vec<SocketAddr>>> + Send + 'a>>;
}

/// The system resolver (`getaddrinfo` through tokio).
#[derive(Clone, Copy, Debug, Default)]
pub struct SystemResolver;

impl Resolver for SystemResolver {
    fn resolve<'a>(&'a self, host: &'a str, port: u16) -> Pin<Box<dyn Future<Output = io::Result<Vec<SocketAddr>>> + Send + 'a>> {
        Box::pin(async move { Ok(tokio::net::lookup_host((host, port)).await?.collect()) })
    }
}

/// A destination resolved once and validated: connect only to `addrs`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pinned {
    pub url: Url,
    /// The host name to pin (`None` when the URL holds an address literal,
    /// which needs no lookup).
    pub domain: Option<String>,
    /// The validated addresses, in the resolver's order.
    pub addrs: Vec<SocketAddr>,
}

/// Checks `raw`, resolves its host once and refuses any internal answer.
///
/// # Errors
/// [`Refused`] for a bad shape, a host that does not resolve, or any answer
/// that is not a public address (a mixed answer could be used to pick an
/// internal one, so one is enough to refuse).
pub async fn resolve_pinned(raw: &str, policy: Policy, resolver: &dyn Resolver) -> Result<Pinned, Refused> {
    let url = check_url(raw, policy)?;
    let port = url.port_or_known_default().ok_or_else(|| refuse("URL has no port"))?;
    let (domain, addrs) = match url.host() {
        Some(url::Host::Ipv4(v4)) => (None, vec![SocketAddr::new(IpAddr::V4(v4), port)]),
        Some(url::Host::Ipv6(v6)) => (None, vec![SocketAddr::new(IpAddr::V6(v6), port)]),
        Some(url::Host::Domain(d)) => {
            let found = resolver.resolve(d, port).await.map_err(|_| refuse(format!("cannot resolve host '{d}'")))?;
            (Some(d.to_owned()), found)
        }
        None => return Err(refuse("URL has no host")),
    };
    if addrs.is_empty() {
        return Err(refuse(format!("cannot resolve host '{}'", url.host_str().unwrap_or_default())));
    }
    if !policy.allow_private && addrs.iter().any(|a| is_blocked(a.ip())) {
        return Err(refuse(format!("host '{}' is not a public address", url.host_str().unwrap_or_default())));
    }
    // The port the URL names is the one connected to.
    let addrs = addrs.into_iter().map(|a| SocketAddr::new(a.ip(), port)).collect();
    Ok(Pinned { url, domain, addrs })
}

/// A client bound to one [`Pinned`] destination: it connects only to the
/// validated addresses, follows no redirect (a `3xx` is returned as is) and
/// ignores proxy settings (a proxy would resolve the name again).
///
/// # Errors
/// The client could not be built (TLS backend unavailable).
#[cfg(feature = "reqwest")]
pub fn pinned_client(pinned: &Pinned, timeout: std::time::Duration) -> Result<reqwest::Client, reqwest::Error> {
    let builder = reqwest::Client::builder().redirect(reqwest::redirect::Policy::none()).no_proxy().timeout(timeout);
    let builder = match &pinned.domain {
        Some(domain) => builder.resolve_to_addrs(domain, &pinned.addrs),
        None => builder,
    };
    builder.build()
}
