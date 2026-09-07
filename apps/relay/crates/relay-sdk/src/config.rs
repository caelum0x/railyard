use std::fmt;
use std::time::Duration;

use url::Url;

use crate::error::{RelaySdkError, Result};
use crate::recovery::SettlementEvidenceValidator;

#[derive(Clone)]
pub enum RelayCredential {
    Bearer(String),
    None,
}

impl RelayCredential {
    pub fn bearer(secret: impl Into<String>) -> Result<Self> {
        let secret = secret.into();
        if secret.trim().is_empty() || secret.contains(['\r', '\n']) {
            return Err(RelaySdkError::InvalidConfig(
                "Relay bearer credential cannot be empty or contain newlines".into(),
            ));
        }
        Ok(Self::Bearer(secret))
    }

    pub(crate) fn authorization_value(&self) -> Option<String> {
        match self {
            Self::Bearer(secret) => Some(format!("Bearer {secret}")),
            Self::None => None,
        }
    }
}

impl fmt::Debug for RelayCredential {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Bearer(_) => formatter.write_str("Bearer([REDACTED])"),
            Self::None => formatter.write_str("None"),
        }
    }
}

#[derive(Clone)]
pub struct RelayClientConfig {
    pub(crate) base_url: Url,
    pub(crate) credential: RelayCredential,
    pub(crate) connect_timeout: Duration,
    pub(crate) request_timeout: Duration,
    pub(crate) stream_timeout: Duration,
    pub(crate) read_timeout: Duration,
    pub(crate) max_response_bytes: usize,
    pub(crate) max_sse_event_bytes: usize,
    pub(crate) user_agent: String,
    pub(crate) evidence_validator: Option<std::sync::Arc<dyn SettlementEvidenceValidator>>,
}

impl fmt::Debug for RelayClientConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RelayClientConfig")
            .field("base_url", &self.base_url)
            .field("credential", &self.credential)
            .field("connect_timeout", &self.connect_timeout)
            .field("request_timeout", &self.request_timeout)
            .field("stream_timeout", &self.stream_timeout)
            .field("read_timeout", &self.read_timeout)
            .field("max_response_bytes", &self.max_response_bytes)
            .field("max_sse_event_bytes", &self.max_sse_event_bytes)
            .field("user_agent", &self.user_agent)
            .field(
                "evidence_validator",
                &self.evidence_validator.as_ref().map(|_| "configured"),
            )
            .finish()
    }
}

impl RelayClientConfig {
    pub fn builder(base_url: impl AsRef<str>) -> Result<RelayClientConfigBuilder> {
        RelayClientConfigBuilder::new(base_url)
    }

    pub fn base_url(&self) -> &Url {
        &self.base_url
    }
}

#[derive(Clone, Debug)]
pub struct RelayClientConfigBuilder {
    config: RelayClientConfig,
}

impl RelayClientConfigBuilder {
    fn new(base_url: impl AsRef<str>) -> Result<Self> {
        let base_url = normalize_base_url(base_url.as_ref())?;
        Ok(Self {
            config: RelayClientConfig {
                base_url,
                credential: RelayCredential::None,
                connect_timeout: Duration::from_secs(5),
                request_timeout: Duration::from_secs(120),
                stream_timeout: Duration::from_secs(900),
                read_timeout: Duration::from_secs(45),
                max_response_bytes: 16 * 1024 * 1024,
                max_sse_event_bytes: 2 * 1024 * 1024,
                user_agent: format!("relay-sdk/{}", env!("CARGO_PKG_VERSION")),
                evidence_validator: None,
            },
        })
    }

    pub fn credential(mut self, credential: RelayCredential) -> Self {
        self.config.credential = credential;
        self
    }

    pub fn connect_timeout(mut self, timeout: Duration) -> Self {
        self.config.connect_timeout = timeout;
        self
    }

    pub fn request_timeout(mut self, timeout: Duration) -> Self {
        self.config.request_timeout = timeout;
        self
    }

    pub fn stream_timeout(mut self, timeout: Duration) -> Self {
        self.config.stream_timeout = timeout;
        self
    }

    pub fn read_timeout(mut self, timeout: Duration) -> Self {
        self.config.read_timeout = timeout;
        self
    }

    pub fn max_response_bytes(mut self, bytes: usize) -> Self {
        self.config.max_response_bytes = bytes;
        self
    }

    pub fn max_sse_event_bytes(mut self, bytes: usize) -> Self {
        self.config.max_sse_event_bytes = bytes;
        self
    }

    pub fn user_agent(mut self, user_agent: impl Into<String>) -> Self {
        self.config.user_agent = user_agent.into();
        self
    }

    /// Install a caller-controlled validator, normally
    /// `relay_aitp::MintAttestationValidator` constructed from a pinned Mint
    /// Ed25519 key. Without this, a 409 replay is exposed only as unverified
    /// evidence and is never promoted to `AlreadySettled`.
    pub fn settlement_evidence_validator(
        mut self,
        validator: std::sync::Arc<dyn SettlementEvidenceValidator>,
    ) -> Self {
        self.config.evidence_validator = Some(validator);
        self
    }

    pub fn build(self) -> Result<RelayClientConfig> {
        let config = self.config;
        if config.connect_timeout.is_zero()
            || config.request_timeout.is_zero()
            || config.stream_timeout.is_zero()
            || config.read_timeout.is_zero()
        {
            return Err(RelaySdkError::InvalidConfig(
                "Relay timeouts must be greater than zero".into(),
            ));
        }
        if config.max_response_bytes == 0 || config.max_sse_event_bytes == 0 {
            return Err(RelaySdkError::InvalidConfig(
                "Relay response limits must be greater than zero".into(),
            ));
        }
        if config.user_agent.trim().is_empty() || config.user_agent.contains(['\r', '\n']) {
            return Err(RelaySdkError::InvalidConfig(
                "Relay user agent cannot be empty or contain newlines".into(),
            ));
        }
        Ok(config)
    }
}

fn normalize_base_url(raw: &str) -> Result<Url> {
    let mut url = Url::parse(raw)
        .map_err(|_| RelaySdkError::InvalidConfig("Relay base URL must be absolute".into()))?;
    if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
        return Err(RelaySdkError::InvalidConfig(
            "Relay base URL must use http or https and include a host".into(),
        ));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(RelaySdkError::InvalidConfig(
            "Relay base URL must not contain credentials".into(),
        ));
    }
    if url.query().is_some() || url.fragment().is_some() {
        return Err(RelaySdkError::InvalidConfig(
            "Relay base URL must not contain a query or fragment".into(),
        ));
    }
    if !url.path().ends_with('/') {
        let path = format!("{}/", url.path());
        url.set_path(&path);
    }
    Ok(url)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_rejects_credentials_in_url_and_redacts_bearer() {
        assert!(RelayClientConfig::builder("https://user:secret@relay.test").is_err());
        let credential = RelayCredential::bearer("top-secret").unwrap();
        assert_eq!(format!("{credential:?}"), "Bearer([REDACTED])");
    }

    #[test]
    fn base_path_is_preserved_with_trailing_slash() {
        let config = RelayClientConfig::builder("https://relay.test/tenant")
            .unwrap()
            .build()
            .unwrap();
        assert_eq!(config.base_url().as_str(), "https://relay.test/tenant/");
    }
}
