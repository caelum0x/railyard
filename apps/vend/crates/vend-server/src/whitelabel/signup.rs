//! Self-serve workspace signup for the white-label kit.
//!
//! `POST /v1/auth/signup` creates, in one request: two Mint accounts (the
//! tenant's payout account and the first app's reserve), the tenant, its first
//! app with the operator's default pricing, a live provisioning key, and the
//! owner identity; then sets the owner's password and returns a signed-in
//! console session plus the one-time provisioning key.
//!
//! Off by default. The operator opts in with `VEND_SIGNUP_ENABLED=true` and
//! must state the launch economics explicitly (no silent defaults for money):
//!
//! | Variable | Required | Meaning |
//! |---|---|---|
//! | `VEND_SIGNUP_ENABLED` | - | `true` to enable |
//! | `VEND_SIGNUP_MODELS` | yes | comma-separated Relay model ids new apps may call |
//! | `VEND_SIGNUP_PLATFORM_FEE_BPS` | yes | platform cut of each tenant's margin |
//! | `VEND_SIGNUP_DAILY_CAP_MICROS` | yes | daily spend cap of the first key |
//! | `VEND_SIGNUP_MARKUP_BPS` | no (2000) | default markup of the first app |
//! | `VEND_SIGNUP_FUNDING_MODE` | no (`end_user_reserve`) | or `tenant_reserve` |
//! | `VEND_SIGNUP_PER_END_USER_DAILY_CAP_MICROS` | no | per-wallet daily cap |
//! | `VEND_SIGNUP_MAX_PER_HOUR` | no (30) | platform-wide signup rate limit |
//!
//! `GET /v1/auth/signup` tells the console whether the form should show.

use axum::extract::State;
use axum::response::Response;
use axum::Json;
use serde::Deserialize;
use serde_json::json;

use vend_core::error::VendError;
use vend_core::money::{FundingMode, MarkupPolicy};
use vend_core::transport::CreateAccountRequest;
use vend_keys::KeyEnv;
use vend_store::{AuditActorKind, AuditOutcome, BootstrapConfig};

use super::{audit, fail, ok_no_store, ConsoleApiState};
use crate::security_ops::{enforce_rate_limit, RateLimitPolicy};

/// Operator-chosen launch economics for self-serve workspaces.
#[derive(Clone, Debug)]
pub struct SignupConfig {
    pub models: Vec<String>,
    pub markup_bps: u32,
    pub platform_fee_bps: u32,
    pub funding_mode: FundingMode,
    pub daily_cap_micros: i64,
    pub per_end_user_daily_cap_micros: Option<i64>,
    pub max_per_hour: i32,
}

impl SignupConfig {
    /// `Ok(None)` unless `VEND_SIGNUP_ENABLED=true`.
    pub fn from_env() -> Result<Option<Self>, VendError> {
        Self::from_lookup(|key| std::env::var(key).ok())
    }

    pub fn from_lookup<F: Fn(&str) -> Option<String>>(
        lookup: F,
    ) -> Result<Option<Self>, VendError> {
        let get = |key: &str| {
            lookup(key)
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty())
        };
        let enabled = get("VEND_SIGNUP_ENABLED")
            .is_some_and(|v| matches!(v.to_ascii_lowercase().as_str(), "1" | "true" | "yes"));
        if !enabled {
            return Ok(None);
        }
        let missing = |key: &str| {
            VendError::Config(format!("{key} must be set when VEND_SIGNUP_ENABLED=true"))
        };
        let models: Vec<String> = get("VEND_SIGNUP_MODELS")
            .ok_or_else(|| missing("VEND_SIGNUP_MODELS"))?
            .split(',')
            .map(|m| m.trim().to_string())
            .filter(|m| !m.is_empty())
            .collect();
        if models.is_empty()
            || models.iter().any(|m| {
                !(m.starts_with("tensorzero::model_name::")
                    || m.starts_with("tensorzero::function_name::"))
            })
        {
            return Err(VendError::Config(
                "VEND_SIGNUP_MODELS must list Relay model ids (tensorzero::model_name::...)".into(),
            ));
        }
        let int = |key: &str| -> Result<Option<i64>, VendError> {
            get(key)
                .map(|raw| {
                    raw.parse::<i64>()
                        .map_err(|_| VendError::Config(format!("{key} must be an integer")))
                })
                .transpose()
        };
        let bps = |key: &str, value: i64| -> Result<u32, VendError> {
            u32::try_from(value)
                .ok()
                .filter(|v| *v <= 10_000)
                .ok_or_else(|| VendError::Config(format!("{key} must be 0..=10000")))
        };
        let platform_fee_bps = bps(
            "VEND_SIGNUP_PLATFORM_FEE_BPS",
            int("VEND_SIGNUP_PLATFORM_FEE_BPS")?
                .ok_or_else(|| missing("VEND_SIGNUP_PLATFORM_FEE_BPS"))?,
        )?;
        let markup_bps = bps(
            "VEND_SIGNUP_MARKUP_BPS",
            int("VEND_SIGNUP_MARKUP_BPS")?.unwrap_or(2_000),
        )?;
        let daily_cap_micros = int("VEND_SIGNUP_DAILY_CAP_MICROS")?
            .ok_or_else(|| missing("VEND_SIGNUP_DAILY_CAP_MICROS"))?;
        if daily_cap_micros <= 0 {
            return Err(VendError::Config(
                "VEND_SIGNUP_DAILY_CAP_MICROS must be positive".into(),
            ));
        }
        let per_end_user_daily_cap_micros = int("VEND_SIGNUP_PER_END_USER_DAILY_CAP_MICROS")?;
        if per_end_user_daily_cap_micros.is_some_and(|cap| cap <= 0) {
            return Err(VendError::Config(
                "VEND_SIGNUP_PER_END_USER_DAILY_CAP_MICROS must be positive".into(),
            ));
        }
        let funding_mode = match get("VEND_SIGNUP_FUNDING_MODE").as_deref() {
            None | Some("end_user_reserve") => FundingMode::EndUserReserve,
            Some("tenant_reserve") => FundingMode::TenantReserve,
            Some(_) => {
                return Err(VendError::Config(
                    "VEND_SIGNUP_FUNDING_MODE must be end_user_reserve or tenant_reserve".into(),
                ))
            }
        };
        let max_per_hour = int("VEND_SIGNUP_MAX_PER_HOUR")?.unwrap_or(30);
        let max_per_hour = i32::try_from(max_per_hour)
            .ok()
            .filter(|v| (1..=10_000).contains(v))
            .ok_or_else(|| {
                VendError::Config("VEND_SIGNUP_MAX_PER_HOUR must be 1..=10000".into())
            })?;
        Ok(Some(Self {
            models,
            markup_bps,
            platform_fee_bps,
            funding_mode,
            daily_cap_micros,
            per_end_user_daily_cap_micros,
            max_per_hour,
        }))
    }
}

#[derive(Deserialize)]
pub struct SignupRequest {
    pub workspace_name: String,
    pub email: String,
    pub display_name: String,
    pub password: String,
    #[serde(default)]
    pub app_name: Option<String>,
}

fn disabled() -> VendError {
    VendError::FeatureDisabled(
        "self-serve signup is disabled on this Vend; ask the operator for an invitation".into(),
    )
}

/// `GET /v1/auth/signup` — whether the console should offer signup.
pub async fn signup_options(State(state): State<ConsoleApiState>) -> Response {
    match &state.signup {
        Some(cfg) => ok_no_store(json!({
            "enabled": true,
            "funding_mode": cfg.funding_mode,
            "default_markup_bps": cfg.markup_bps,
            "models": cfg.models,
        })),
        None => ok_no_store(json!({ "enabled": false })),
    }
}

fn validate(input: &SignupRequest) -> Result<(), VendError> {
    let name_ok = |v: &str| !v.trim().is_empty() && v.trim().len() <= 120;
    if !name_ok(&input.workspace_name)
        || !name_ok(&input.display_name)
        || input.app_name.as_deref().is_some_and(|n| !name_ok(n))
        || !(12..=256).contains(&input.password.len())
    {
        return Err(VendError::InvalidAmount);
    }
    let email = input.email.trim();
    if email.len() > 320
        || email.matches('@').count() != 1
        || email.starts_with('@')
        || email.ends_with('@')
    {
        return Err(VendError::InvalidAmount);
    }
    Ok(())
}

/// `POST /v1/auth/signup`.
pub async fn signup(
    State(state): State<ConsoleApiState>,
    Json(input): Json<SignupRequest>,
) -> Response {
    let Some(cfg) = state.signup.clone() else {
        return fail(disabled());
    };
    if let Err(error) = validate(&input) {
        return fail(error);
    }
    let email = input.email.trim().to_ascii_lowercase();
    // Per-email and platform-wide throttles (shared across replicas).
    let limits = [
        (
            "signup_email",
            email.as_str(),
            RateLimitPolicy::new(3_600, 3, 3_600),
        ),
        (
            "signup_global",
            "all",
            RateLimitPolicy::new(3_600, cfg.max_per_hour, 900),
        ),
    ];
    for (bucket, subject, policy) in limits {
        let policy = match policy {
            Ok(policy) => policy,
            Err(error) => return fail(error),
        };
        if let Err(response) = enforce_rate_limit(
            &state.security_repo,
            &state.security_config,
            bucket,
            "global",
            subject,
            policy,
        )
        .await
        {
            return response;
        }
    }
    match create_workspace(&state, &cfg, &input).await {
        Ok(body) => ok_no_store(body),
        Err(error) => fail(error),
    }
}

async fn create_workspace(
    state: &ConsoleApiState,
    cfg: &SignupConfig,
    input: &SignupRequest,
) -> Result<serde_json::Value, VendError> {
    // 1. Mint accounts: the tenant's payout account and the app reserve. They
    //    are empty until funded, so an abandoned signup leaves no money behind.
    let tenant_account = state
        .mint
        .create_account(CreateAccountRequest {
            kind: "org".into(),
            owner_ref: Some("vend:signup:tenant".into()),
        })
        .await?;
    let app_account = state
        .mint
        .create_account(CreateAccountRequest {
            kind: "org".into(),
            owner_ref: Some("vend:signup:app".into()),
        })
        .await?;
    // 2. Tenant + app + key + owner in one store transaction. The tenant row's
    //    Mint account is the payout destination; the app gets its own reserve.
    let app_name = input
        .app_name
        .clone()
        .filter(|n| !n.trim().is_empty())
        .unwrap_or_else(|| format!("{} API", input.workspace_name.trim()));
    let created = state
        .control
        .bootstrap(BootstrapConfig {
            tenant_name: input.workspace_name.trim().to_string(),
            app_name,
            mint_account_id: tenant_account.account_id.clone(),
            model_allowlist: cfg.models.clone(),
            zdr: false,
            markup_policy: MarkupPolicy {
                markup_bps: cfg.markup_bps,
                min_fee_micros: 0,
                platform_fee_bps: cfg.platform_fee_bps,
                funding_mode: cfg.funding_mode,
            },
            key_env: KeyEnv::Live,
            daily_cap_micros: Some(cfg.daily_cap_micros),
            per_end_user_daily_cap_micros: cfg.per_end_user_daily_cap_micros,
            owner_email: input.email.trim().to_string(),
            owner_display_name: input.display_name.trim().to_string(),
            setup_token_ttl_seconds: state.setup_ttl_seconds,
        })
        .await?;
    // The bootstrap app shares the tenant account; point it at its own reserve
    // so app spend and tenant earnings never mix.
    let app_reserve = match state
        .control
        .set_app_mint_account(&created.app_id, &app_account.account_id)
        .await
    {
        Ok(()) => app_account.account_id.clone(),
        Err(error) => {
            tracing::error!(%error, app_id = created.app_id.as_str(), "signup: app reserve not assigned; app uses the tenant account");
            tenant_account.account_id.clone()
        }
    };
    audit(
        &state.security_repo,
        Some(created.tenant_id.clone()),
        (AuditActorKind::Customer, Some(created.owner.id.clone())),
        "auth.self_signup",
        ("tenant", created.tenant_id.as_str()),
        AuditOutcome::Succeeded,
        json!({"app_id": created.app_id.as_str()}),
    )
    .await;
    // 3. Set the owner's password and sign in.
    let signed_in = async {
        state
            .control
            .setup_customer_password(&created.owner_setup_token, &input.password)
            .await?;
        state
            .control
            .login_customer(
                &created.tenant_id,
                input.email.trim(),
                &input.password,
                state.session_ttl_seconds,
            )
            .await
    }
    .await;
    let session = match signed_in {
        Ok(session) => session,
        Err(error) => {
            tracing::error!(%error, tenant_id = created.tenant_id.as_str(), "signup: workspace created but owner sign-in failed");
            return Err(VendError::FeatureDisabled(format!(
                "workspace {} was created but sign-in failed; sign in from the console with your email and password, or contact the operator",
                created.tenant_id.as_str()
            )));
        }
    };
    Ok(json!({
        "tenant_id": created.tenant_id,
        "app_id": created.app_id,
        "app_mint_account_id": app_reserve,
        "tenant_mint_account_id": tenant_account.account_id,
        "key_id": created.sub_key_id,
        "provisioning_key": created.secret,
        "session_token": session.session_token,
        "expires_at": session.expires_at,
        "funding_mode": cfg.funding_mode,
        "warning": "Store the provisioning key now; it is shown once.",
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn load(pairs: &[(&str, &str)]) -> Result<Option<SignupConfig>, VendError> {
        let map: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| ((*k).into(), (*v).into()))
            .collect();
        SignupConfig::from_lookup(|k| map.get(k).cloned())
    }

    const MODEL: &str = "tensorzero::model_name::openai::gpt-4o-mini";

    #[test]
    fn disabled_by_default_and_requires_explicit_economics() {
        assert!(load(&[]).unwrap().is_none());
        let err = load(&[("VEND_SIGNUP_ENABLED", "true")]).unwrap_err();
        assert!(err.to_string().contains("VEND_SIGNUP_MODELS"));
        let err = load(&[
            ("VEND_SIGNUP_ENABLED", "true"),
            ("VEND_SIGNUP_MODELS", MODEL),
        ])
        .unwrap_err();
        assert!(err.to_string().contains("VEND_SIGNUP_PLATFORM_FEE_BPS"));
    }

    #[test]
    fn full_config_parses_with_wallet_first_default() {
        let cfg = load(&[
            ("VEND_SIGNUP_ENABLED", "true"),
            ("VEND_SIGNUP_MODELS", MODEL),
            ("VEND_SIGNUP_PLATFORM_FEE_BPS", "1000"),
            ("VEND_SIGNUP_DAILY_CAP_MICROS", "50000000"),
        ])
        .unwrap()
        .unwrap();
        assert_eq!(cfg.funding_mode, FundingMode::EndUserReserve);
        assert_eq!(cfg.markup_bps, 2_000);
        assert_eq!(cfg.max_per_hour, 30);
        assert!(load(&[
            ("VEND_SIGNUP_ENABLED", "true"),
            ("VEND_SIGNUP_MODELS", "gpt-4o"),
            ("VEND_SIGNUP_PLATFORM_FEE_BPS", "1000"),
            ("VEND_SIGNUP_DAILY_CAP_MICROS", "1"),
        ])
        .is_err());
    }

    #[test]
    fn signup_input_is_validated() {
        let good = SignupRequest {
            workspace_name: "Acme".into(),
            email: "founder@acme.dev".into(),
            display_name: "Ada".into(),
            password: "correct horse battery".into(),
            app_name: None,
        };
        assert!(validate(&good).is_ok());
        let short = SignupRequest {
            password: "short".into(),
            ..good
        };
        assert!(validate(&short).is_err());
    }
}
