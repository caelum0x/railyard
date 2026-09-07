//! Relay liveness and dependency-health response contracts.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct GatewayStatus {
    pub status: String,
    pub version: String,
    pub config_hash: String,
}

impl GatewayStatus {
    pub fn is_ready(&self) -> bool {
        self.status == "ok"
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct GatewayHealth {
    pub gateway: String,
    pub clickhouse: String,
    pub postgres: String,
    pub valkey: String,
    pub valkey_cache: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub relay_catalog: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub relay_billing: Option<String>,
}

impl GatewayHealth {
    pub fn is_healthy(&self) -> bool {
        [
            &self.gateway,
            &self.clickhouse,
            &self.postgres,
            &self.valkey,
            &self.valkey_cache,
        ]
        .into_iter()
        .all(|status| status == "ok")
            && self
                .relay_catalog
                .as_deref()
                .is_none_or(|status| status == "ok")
            && self
                .relay_billing
                .as_deref()
                .is_none_or(|status| matches!(status, "ok" | "disabled"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn health_requires_every_dependency() {
        let mut health = GatewayHealth {
            gateway: "ok".into(),
            clickhouse: "ok".into(),
            postgres: "ok".into(),
            valkey: "ok".into(),
            valkey_cache: "ok".into(),
            relay_catalog: Some("ok".into()),
            relay_billing: Some("ok".into()),
        };
        assert!(health.is_healthy());
        health.postgres = "error".into();
        assert!(!health.is_healthy());
        health.postgres = "ok".into();
        health.relay_billing = Some("error".into());
        assert!(!health.is_healthy());
        health.relay_billing = Some("disabled".into());
        assert!(health.is_healthy());
    }
}
