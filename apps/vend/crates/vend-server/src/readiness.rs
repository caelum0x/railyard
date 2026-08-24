//! Dependency-aware readiness for Postgres, Mint, and Relay.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use futures::future::try_join3;
use reqwest::header::{HeaderValue, CONTENT_TYPE};
use vend_core::{ReadinessRepo, VendError};

/// Combines Vend's Postgres probe with an authenticated Mint account read and
/// Relay `/health`. Mint's public `/readyz` cannot be used here because its
/// current store abstraction converts a failed journal count to zero.
pub struct DependencyReadiness {
    postgres: Arc<dyn ReadinessRepo>,
    http: reqwest::Client,
    mint_ready_url: reqwest::Url,
    mint_api_key: HeaderValue,
    relay_health_url: reqwest::Url,
}

impl DependencyReadiness {
    pub fn new(
        postgres: Arc<dyn ReadinessRepo>,
        mint_base: &str,
        mint_api_key: &str,
        mint_clearing_account: &str,
        relay_base: &str,
        relay_health_url: Option<&str>,
        timeout: Duration,
    ) -> Result<Self, VendError> {
        if timeout.is_zero() || timeout > Duration::from_secs(30) {
            return Err(VendError::Storage(
                "readiness timeout must be between 1ns and 30s".into(),
            ));
        }
        let mint_ready_url = account_url(mint_base, mint_clearing_account)?;
        let mut mint_api_key = HeaderValue::from_str(mint_api_key)
            .map_err(|_| VendError::Storage("Mint API key must be a valid HTTP header".into()))?;
        if mint_api_key.is_empty() {
            return Err(VendError::Storage("Mint API key must not be empty".into()));
        }
        mint_api_key.set_sensitive(true);
        // RELAY_BASE_PATH selects the inference API surface and is unrelated to
        // the gateway deployment prefix. Relay health therefore defaults to
        // RELAY_BASE + /health, with an exact override for prefixed deployments.
        let relay_health_url = match relay_health_url {
            Some(url) => exact_service_url(url, "Relay health")?,
            None => service_url(relay_base, "/health", "Relay")?,
        };
        let http = reqwest::Client::builder()
            .connect_timeout(timeout)
            .timeout(timeout)
            .read_timeout(timeout)
            .pool_max_idle_per_host(2)
            .redirect(reqwest::redirect::Policy::none())
            .user_agent("vend-readiness/0.1")
            .build()
            .map_err(|error| VendError::Storage(format!("build readiness client: {error}")))?;
        Ok(Self {
            postgres,
            http,
            mint_ready_url,
            mint_api_key,
            relay_health_url,
        })
    }

    async fn mint_ready(
        http: &reqwest::Client,
        url: reqwest::Url,
        api_key: HeaderValue,
    ) -> Result<(), VendError> {
        let response = http
            .get(url)
            .header("X-API-Key", api_key)
            .send()
            .await
            .map_err(|error| {
                VendError::Transport(format!("Mint readiness request failed: {error}"))
            })?;
        if !response.status().is_success() {
            return Err(VendError::Transport(format!(
                "Mint readiness returned HTTP {}",
                response.status().as_u16()
            )));
        }
        Ok(())
    }

    async fn relay_ready(http: &reqwest::Client, url: reqwest::Url) -> Result<(), VendError> {
        let response = http.get(url).send().await.map_err(|error| {
            VendError::Transport(format!("Relay readiness request failed: {error}"))
        })?;
        if !response.status().is_success() {
            return Err(VendError::Transport(format!(
                "Relay readiness returned HTTP {}",
                response.status().as_u16()
            )));
        }
        // Reject an accidental HTML/login endpoint even if a proxy returns 200.
        if response.headers().get(CONTENT_TYPE).is_some_and(|value| {
            value
                .to_str()
                .map(|value| !value.starts_with("application/json"))
                .unwrap_or(true)
        }) {
            return Err(VendError::Transport(
                "Relay readiness did not return JSON".into(),
            ));
        }
        Ok(())
    }
}

#[async_trait]
impl ReadinessRepo for DependencyReadiness {
    async fn ready(&self) -> Result<(), VendError> {
        try_join3(
            self.postgres.ready(),
            Self::mint_ready(
                &self.http,
                self.mint_ready_url.clone(),
                self.mint_api_key.clone(),
            ),
            Self::relay_ready(&self.http, self.relay_health_url.clone()),
        )
        .await?;
        Ok(())
    }
}

fn account_url(base: &str, account_id: &str) -> Result<reqwest::Url, VendError> {
    if account_id.is_empty() {
        return Err(VendError::Storage(
            "Mint clearing account must not be empty".into(),
        ));
    }
    let mut url = service_url(base, "/v1/accounts", "Mint")?;
    url.path_segments_mut()
        .map_err(|_| VendError::Storage("Mint base URL cannot be a base".into()))?
        .push(account_id);
    Ok(url)
}

fn service_url(base: &str, path: &str, name: &str) -> Result<reqwest::Url, VendError> {
    let mut url = reqwest::Url::parse(base)
        .map_err(|_| VendError::Storage(format!("{name} base URL must be absolute")))?;
    if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
        return Err(VendError::Storage(format!(
            "{name} base URL must use http or https and include a host"
        )));
    }
    if !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(VendError::Storage(format!(
            "{name} base URL must not contain credentials, query, or fragment"
        )));
    }
    let full_path = format!(
        "{}/{}",
        url.path().trim_end_matches('/'),
        path.trim_start_matches('/')
    );
    url.set_path(&full_path);
    Ok(url)
}

fn exact_service_url(url: &str, name: &str) -> Result<reqwest::Url, VendError> {
    let mut parsed = service_url(url, "", name)?;
    let path = parsed.path().trim_end_matches('/').to_owned();
    if path.is_empty() {
        return Err(VendError::Storage(format!(
            "{name} URL must include a health endpoint path"
        )));
    }
    parsed.set_path(&path);
    Ok(parsed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_exact_dependency_health_urls() {
        assert_eq!(
            account_url("https://mint.internal", "acct_live_01")
                .unwrap()
                .as_str(),
            "https://mint.internal/v1/accounts/acct_live_01"
        );
        assert_eq!(
            account_url("https://mint.internal", "bad/account")
                .unwrap()
                .as_str(),
            "https://mint.internal/v1/accounts/bad%2Faccount"
        );
        assert_eq!(
            service_url("https://relay.internal/gateway", "/health", "Relay")
                .unwrap()
                .as_str(),
            "https://relay.internal/gateway/health"
        );
        assert_eq!(
            exact_service_url(
                "https://relay.internal/custom-prefix/health",
                "Relay health"
            )
            .unwrap()
            .as_str(),
            "https://relay.internal/custom-prefix/health"
        );
    }

    #[test]
    fn readiness_urls_reject_embedded_credentials() {
        assert!(service_url("https://secret@mint.internal", "/readyz", "Mint").is_err());
    }

    #[test]
    fn mint_readiness_credentials_are_marked_sensitive() {
        let probe = DependencyReadiness::new(
            Arc::new(vend_testkit::InMemoryStore::new()),
            "https://mint.internal",
            "mint-secret",
            "acct_clearing",
            "https://relay.internal",
            None,
            Duration::from_secs(3),
        )
        .unwrap();
        assert!(probe.mint_api_key.is_sensitive());
        assert_eq!(
            probe.mint_ready_url.as_str(),
            "https://mint.internal/v1/accounts/acct_clearing"
        );
    }
}
