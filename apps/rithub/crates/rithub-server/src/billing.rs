//! Railyard billing hookup: **Rago** (Lago-compatible usage billing) plan
//! enforcement + usage metering for the native Rust forge.
//!
//! This is the in-process equivalent of the Forgejo fork's
//! `integrations/railyard/{quota-guard,webhook-bridge}` and uses the same Rago
//! contract, so one Rago plan configuration serves either forge:
//!
//! - one rithub **org** == one Rago customer (`external_customer_id = <org>`)
//!   with subscription `external_subscription_id = <SUB_PREFIX><org>`;
//! - the private-repo limit is the `private_repos` entitlement's `max`
//!   privilege (`GET /api/v1/subscriptions/{sub}/entitlements`); no entitlement
//!   means unmetered;
//! - usage is posted as `POST /api/v1/events {event:{...}}` with the metric
//!   codes `rithub_private_repo`, `rithub_seat`, and `rithub_ci_push`, and a
//!   **deterministic** `transaction_id` so retries dedupe in Rago.
//!
//! Settlement (invoice → Mint transfer) stays with Rago's own webhook → Mint
//! path; the forge only meters and enforces.
//!
//! ## Configuration
//! | Var | Meaning |
//! |-----|---------|
//! | `RAGO_BASE_URL` | Rago base URL. Unset = billing disabled. |
//! | `RAGO_API_KEY` | Rago API key (Bearer). Required when `RAGO_BASE_URL` is set. |
//! | `RAGO_SUB_PREFIX` | Subscription id prefix (default `sub_`). |
//! | `RITHUB_FREE_PRIVATE_REPOS` | Local private-repo cap used when Rago is not configured. Unset = unlimited. |
//!
//! Enforcement fails **closed**: when Rago is configured but unreachable, a
//! private-repo creation is refused with 503 rather than silently allowed.

use rithub_core::{DirectoryStore, Organization};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::error::AppError;
use crate::state::AppState;

/// Rago metric: a private repository (+1 created / -1 removed).
pub const METRIC_PRIVATE_REPO: &str = "rithub_private_repo";
/// Rago metric: an org seat (member added / removed).
pub const METRIC_SEAT: &str = "rithub_seat";
/// Rago metric: one accepted push (CI/compute metering).
pub const METRIC_CI_PUSH: &str = "rithub_ci_push";

/// Resolved Rago connection settings.
struct RagoConfig {
    base_url: String,
    api_key: String,
    sub_prefix: String,
}

/// Billing configuration state, surfaced on the org settings page.
pub enum BillingMode {
    /// Rago is configured: limits come from the org's subscription entitlements.
    Rago,
    /// No Rago; a local private-repo cap applies.
    LocalCap(i64),
    /// No billing at all (unlimited, unmetered).
    Disabled,
    /// `RAGO_BASE_URL` is set without `RAGO_API_KEY`.
    Misconfigured,
}

fn rago_config() -> Result<Option<RagoConfig>, ()> {
    let base_url = match std::env::var("RAGO_BASE_URL") {
        Ok(v) if !v.trim().is_empty() => v.trim().trim_end_matches('/').to_string(),
        _ => return Ok(None),
    };
    let api_key = match std::env::var("RAGO_API_KEY") {
        Ok(v) if !v.trim().is_empty() => v.trim().to_string(),
        _ => return Err(()),
    };
    let sub_prefix = std::env::var("RAGO_SUB_PREFIX").unwrap_or_else(|_| "sub_".to_string());
    Ok(Some(RagoConfig {
        base_url,
        api_key,
        sub_prefix,
    }))
}

fn local_cap() -> Option<i64> {
    std::env::var("RITHUB_FREE_PRIVATE_REPOS")
        .ok()
        .and_then(|v| v.trim().parse::<i64>().ok())
        .filter(|n| *n >= 0)
}

/// Which billing mode is active.
pub fn mode() -> BillingMode {
    match rago_config() {
        Err(()) => BillingMode::Misconfigured,
        Ok(Some(_)) => BillingMode::Rago,
        Ok(None) => match local_cap() {
            Some(cap) => BillingMode::LocalCap(cap),
            None => BillingMode::Disabled,
        },
    }
}

/// The org's private-repo limit: `Ok(None)` = unlimited.
pub async fn private_repo_limit(org: &Organization) -> Result<Option<i64>, AppError> {
    let cfg = match rago_config() {
        Err(()) => {
            tracing::error!("RAGO_BASE_URL is set but RAGO_API_KEY is missing");
            return Err(AppError::service_unavailable(
                "billing is misconfigured on this server (RAGO_API_KEY missing)",
            ));
        }
        Ok(None) => return Ok(local_cap()),
        Ok(Some(cfg)) => cfg,
    };
    let url = format!(
        "{}/api/v1/subscriptions/{}{}/entitlements",
        cfg.base_url, cfg.sub_prefix, org.name
    );
    let response = reqwest::Client::new()
        .get(&url)
        .bearer_auth(&cfg.api_key)
        .timeout(std::time::Duration::from_secs(5))
        .send()
        .await
        .map_err(|e| {
            tracing::warn!(error = %e, org = %org.name, "rago entitlements request failed");
            AppError::service_unavailable("billing service unavailable; try again shortly")
        })?;
    if response.status() == reqwest::StatusCode::NOT_FOUND {
        // No subscription for this org: fall back to the free local cap.
        return Ok(Some(local_cap().unwrap_or(0)));
    }
    if !response.status().is_success() {
        tracing::warn!(status = %response.status(), org = %org.name, "rago entitlements error");
        return Err(AppError::service_unavailable(
            "billing service unavailable; try again shortly",
        ));
    }
    let body: Value = response.json().await.map_err(|e| {
        tracing::warn!(error = %e, "rago entitlements body was not JSON");
        AppError::service_unavailable("billing service returned an invalid response")
    })?;
    Ok(parse_private_repo_limit(&body))
}

/// Extract `private_repos.max` from a Rago entitlements body (`None` = unmetered).
fn parse_private_repo_limit(body: &Value) -> Option<i64> {
    let feature = body
        .get("entitlements")?
        .as_array()?
        .iter()
        .find(|e| e.get("code").and_then(Value::as_str) == Some("private_repos"))?;
    let max = feature
        .get("privileges")?
        .as_array()?
        .iter()
        .find(|p| p.get("code").and_then(Value::as_str) == Some("max"))?;
    match max.get("value")? {
        Value::Number(n) => n.as_i64(),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

/// Refuse with **402** when adding one more private repo to `org` would exceed
/// its plan. Call before creating a private repo or flipping one to private.
pub async fn ensure_private_repo_quota(state: &AppState, org: &Organization) -> Result<(), AppError> {
    let Some(limit) = private_repo_limit(org).await? else {
        return Ok(());
    };
    let current = state.store.count_private_repos(org.id).await?;
    if current >= limit {
        return Err(AppError::payment_required(format!(
            "organization '{}' has reached its plan limit of {limit} private repositor{}; \
             upgrade the plan or make the repository public",
            org.name,
            if limit == 1 { "y" } else { "ies" }
        )));
    }
    Ok(())
}

/// A deterministic Rago transaction id (retries of the same logical event dedupe).
fn transaction_id(code: &str, key: &str) -> String {
    let digest = Sha256::digest(format!("{code}|{key}").as_bytes());
    let hex: String = digest.iter().take(20).map(|b| format!("{b:02x}")).collect();
    format!("rithub_{hex}")
}

/// Post a usage event to Rago in the background. No-op when Rago is not
/// configured. `dedupe_key` must uniquely identify the logical event.
pub fn meter(org: &str, code: &'static str, dedupe_key: String, properties: Value) {
    let Ok(Some(cfg)) = rago_config() else {
        return;
    };
    let org = org.to_string();
    tokio::spawn(async move {
        let event = json!({
            "event": {
                "transaction_id": transaction_id(code, &dedupe_key),
                "external_subscription_id": format!("{}{}", cfg.sub_prefix, org),
                "external_customer_id": org,
                "code": code,
                "timestamp": time::OffsetDateTime::now_utc().unix_timestamp(),
                "properties": properties,
            }
        });
        let result = reqwest::Client::new()
            .post(format!("{}/api/v1/events", cfg.base_url))
            .bearer_auth(&cfg.api_key)
            .timeout(std::time::Duration::from_secs(10))
            .json(&event)
            .send()
            .await;
        match result {
            Ok(r) if r.status().is_success() => {
                tracing::debug!(code, org = %org, "rago usage event recorded");
            }
            Ok(r) => tracing::warn!(code, org = %org, status = %r.status(), "rago rejected usage event"),
            Err(e) => tracing::warn!(code, org = %org, error = %e, "rago usage event failed"),
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_numeric_and_string_limits() {
        let body = json!({"entitlements":[{"code":"private_repos","privileges":[{"code":"max","value":3}]}]});
        assert_eq!(parse_private_repo_limit(&body), Some(3));
        let body = json!({"entitlements":[{"code":"private_repos","privileges":[{"code":"max","value":"7"}]}]});
        assert_eq!(parse_private_repo_limit(&body), Some(7));
    }

    #[test]
    fn missing_entitlement_is_unmetered() {
        assert_eq!(parse_private_repo_limit(&json!({"entitlements":[]})), None);
    }

    #[test]
    fn transaction_ids_are_deterministic() {
        assert_eq!(transaction_id("a", "k"), transaction_id("a", "k"));
        assert_ne!(transaction_id("a", "k"), transaction_id("b", "k"));
    }
}
