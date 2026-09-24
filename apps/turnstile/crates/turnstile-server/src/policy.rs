//! Guardrails for tools edited over the admin API.
//!
//! A tool file on disk is operator-trusted. A tool written through an HTTP API
//! with a bearer token is not: a leaked token must not turn Turnstile into an
//! SSRF proxy into the private network, nor into a way to ship the server's own
//! secrets (admin token, Mint API key, database URL) to an attacker's upstream.
//!
//! - `TURNSTILE_UPSTREAM_ALLOW_HOSTS` — optional comma-separated host allowlist
//!   (`api.example.com`, `*.example.com`). When set, every NEW or CHANGED
//!   `upstream_url` must match it.
//! - Private, loopback, link-local, CGNAT, multicast and unspecified addresses
//!   (literal or resolved) are refused unless `TURNSTILE_UPSTREAM_ALLOW_PRIVATE=1`.
//! - `upstream_auth_env` may only name `TURNSTILE_UPSTREAM_*` variables or ones
//!   listed in `TURNSTILE_UPSTREAM_AUTH_ENVS`, and never the server's own
//!   secrets. Values are never returned; only names and set/unset status.
//!
//! Unchanged values of an existing tool (e.g. one seeded from the file that
//! proxies to a sidecar on localhost) are not re-checked, so editing its price
//! does not require loosening the policy.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::time::Duration;

use serde::Serialize;

/// Env vars an admin-API tool may never forward upstream.
const RESERVED_ENVS: [&str; 7] = [
    "TURNSTILE_ADMIN_TOKEN",
    "TURNSTILE_API_KEY",
    "TURNSTILE_DATABASE_URL",
    "DATABASE_URL",
    "MINT_SERVER_URL",
    "TURNSTILE_PAYEE",
    "AWS_SECRET_ACCESS_KEY",
];

/// Prefix of env vars that are credential-eligible without extra config.
pub const UPSTREAM_ENV_PREFIX: &str = "TURNSTILE_UPSTREAM_";

/// How long a write may wait on DNS while vetting an upstream host.
const RESOLVE_TIMEOUT: Duration = Duration::from_secs(3);

/// One credential env var the console may offer.
#[derive(Debug, Clone, Serialize)]
pub struct AuthEnv {
    pub name: String,
    /// Whether the variable is set (non-empty) in this process.
    pub set: bool,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct UpstreamPolicy {
    /// Host allowlist; empty = any public host.
    pub allow_hosts: Vec<String>,
    /// Permit private / loopback / link-local upstreams.
    pub allow_private: bool,
    /// Extra credential env vars allowed besides the `TURNSTILE_UPSTREAM_` prefix.
    pub auth_env_allowlist: Vec<String>,
}

fn csv(var: &str) -> Vec<String> {
    std::env::var(var)
        .unwrap_or_default()
        .split(',')
        .map(|item| item.trim().to_string())
        .filter(|item| !item.is_empty())
        .collect()
}

fn env_is_set(name: &str) -> bool {
    std::env::var(name).is_ok_and(|value| !value.trim().is_empty())
}

impl UpstreamPolicy {
    pub fn from_env() -> Self {
        Self {
            allow_hosts: csv("TURNSTILE_UPSTREAM_ALLOW_HOSTS")
                .into_iter()
                .map(|host| host.to_ascii_lowercase())
                .collect(),
            allow_private: matches!(
                std::env::var("TURNSTILE_UPSTREAM_ALLOW_PRIVATE")
                    .map(|v| v.trim().to_ascii_lowercase())
                    .as_deref(),
                Ok("1") | Ok("true") | Ok("yes")
            ),
            auth_env_allowlist: csv("TURNSTILE_UPSTREAM_AUTH_ENVS"),
        }
    }

    fn host_allowed(&self, host: &str) -> bool {
        if self.allow_hosts.is_empty() {
            return true;
        }
        self.allow_hosts.iter().any(|pattern| match pattern.strip_prefix("*.") {
            Some(suffix) => host.len() > suffix.len() + 1 && host.ends_with(&format!(".{suffix}")),
            None => host == pattern,
        })
    }

    /// Vet a new or changed upstream URL.
    pub async fn check_url(&self, raw: &str) -> Result<(), String> {
        let url = reqwest::Url::parse(raw).map_err(|e| format!("upstream_url is not a valid URL: {e}"))?;
        if !matches!(url.scheme(), "http" | "https") {
            return Err("upstream_url must be http(s)".into());
        }
        if !url.username().is_empty() || url.password().is_some() {
            return Err("upstream_url must not embed credentials; use upstream_auth_env".into());
        }
        let host = url
            .host_str()
            .ok_or("upstream_url has no host")?
            .trim_end_matches('.')
            .to_ascii_lowercase();
        if !self.host_allowed(&host) {
            return Err(format!(
                "upstream host {host} is not in TURNSTILE_UPSTREAM_ALLOW_HOSTS"
            ));
        }
        if self.allow_private {
            return Ok(());
        }
        let deny = |ip: IpAddr| {
            Err(format!(
                "upstream host {host} resolves to non-public address {ip}; set TURNSTILE_UPSTREAM_ALLOW_PRIVATE=1 to permit internal upstreams"
            ))
        };
        let literal = host.trim_start_matches('[').trim_end_matches(']');
        if let Ok(ip) = literal.parse::<IpAddr>() {
            return if is_public(ip) { Ok(()) } else { deny(ip) };
        }
        if host == "localhost" || host.ends_with(".localhost") || host.ends_with(".internal") {
            return Err(format!(
                "upstream host {host} is internal; set TURNSTILE_UPSTREAM_ALLOW_PRIVATE=1 to permit it"
            ));
        }
        let port = url.port_or_known_default().unwrap_or(443);
        let resolved = tokio::time::timeout(
            RESOLVE_TIMEOUT,
            tokio::net::lookup_host((host.as_str(), port)),
        )
        .await
        .map_err(|_| format!("resolving upstream host {host} timed out"))?
        .map_err(|e| format!("cannot resolve upstream host {host}: {e}"))?;
        for addr in resolved {
            if !is_public(addr.ip()) {
                return deny(addr.ip());
            }
        }
        Ok(())
    }

    /// Vet a new or changed credential env var name.
    pub fn check_auth_env(&self, var: &str) -> Result<(), String> {
        if RESERVED_ENVS.contains(&var) {
            return Err(format!("{var} is a Turnstile server secret and cannot be sent upstream"));
        }
        let eligible = var.starts_with(UPSTREAM_ENV_PREFIX)
            || self.auth_env_allowlist.iter().any(|allowed| allowed == var);
        if !eligible {
            return Err(format!(
                "upstream_auth_env must start with {UPSTREAM_ENV_PREFIX} or be listed in TURNSTILE_UPSTREAM_AUTH_ENVS (got {var})"
            ));
        }
        Ok(())
    }

    /// Credential env vars the console can offer: allowlisted names, every set
    /// `TURNSTILE_UPSTREAM_*` variable, and any name an existing tool already
    /// references. Names and set/unset only — never values.
    pub fn available_auth_envs(&self, referenced: impl IntoIterator<Item = String>) -> Vec<AuthEnv> {
        let mut names: std::collections::BTreeSet<String> =
            self.auth_env_allowlist.iter().cloned().collect();
        names.extend(
            std::env::vars_os()
                .filter_map(|(key, _)| key.into_string().ok())
                .filter(|key| key.starts_with(UPSTREAM_ENV_PREFIX)),
        );
        names.extend(referenced);
        names
            .into_iter()
            .filter(|name| !RESERVED_ENVS.contains(&name.as_str()))
            .map(|name| AuthEnv {
                set: env_is_set(&name),
                name,
            })
            .collect()
    }
}

/// Whether `ip` is a globally routable unicast address.
fn is_public(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_public_v4(v4),
        IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
            Some(v4) => is_public_v4(v4),
            None => is_public_v6(v6),
        },
    }
}

fn is_public_v4(ip: Ipv4Addr) -> bool {
    let [a, b, ..] = ip.octets();
    !(ip.is_private()
        || ip.is_loopback()
        || ip.is_link_local()
        || ip.is_unspecified()
        || ip.is_broadcast()
        || ip.is_multicast()
        || ip.is_documentation()
        || a == 0
        || (a == 100 && (64..=127).contains(&b)) // CGNAT 100.64/10
        || (a == 198 && (18..=19).contains(&b)) // benchmarking 198.18/15
        || a >= 240)
}

fn is_public_v6(ip: Ipv6Addr) -> bool {
    let first = ip.segments()[0];
    !(ip.is_loopback()
        || ip.is_unspecified()
        || ip.is_multicast()
        || (first & 0xfe00) == 0xfc00 // unique local fc00::/7
        || (first & 0xffc0) == 0xfe80 // link-local fe80::/10
        || first == 0x2001 && ip.segments()[1] == 0x0db8) // documentation
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn refuses_private_and_metadata_upstreams() {
        let policy = UpstreamPolicy::default();
        for url in [
            "http://169.254.169.254/latest/meta-data",
            "http://127.0.0.1:8080/",
            "http://10.0.0.5/",
            "http://[::1]/",
            "http://[::ffff:192.168.1.1]/",
            "http://localhost:9000/",
            "http://user:pw@example.com/",
        ] {
            assert!(policy.check_url(url).await.is_err(), "{url} should be refused");
        }
        let open = UpstreamPolicy { allow_private: true, ..UpstreamPolicy::default() };
        assert!(open.check_url("http://127.0.0.1:8080/").await.is_ok());
    }

    #[test]
    fn host_allowlist_and_env_rules() {
        let policy = UpstreamPolicy {
            allow_hosts: vec!["api.example.com".into(), "*.tools.dev".into()],
            allow_private: false,
            auth_env_allowlist: vec!["OPENAI_API_KEY".into()],
        };
        assert!(policy.host_allowed("api.example.com"));
        assert!(policy.host_allowed("a.tools.dev"));
        assert!(!policy.host_allowed("tools.dev"));
        assert!(!policy.host_allowed("evil.com"));
        assert!(policy.check_auth_env("TURNSTILE_UPSTREAM_SEARCH").is_ok());
        assert!(policy.check_auth_env("OPENAI_API_KEY").is_ok());
        assert!(policy.check_auth_env("TURNSTILE_ADMIN_TOKEN").is_err());
        assert!(policy.check_auth_env("HOME").is_err());
    }
}
