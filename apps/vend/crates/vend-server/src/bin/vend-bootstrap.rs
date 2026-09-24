//! One-shot operator bootstrap for the Vend product.

use anyhow::{bail, Context};
use vend_core::money::{FundingMode, MarkupPolicy};
use vend_keys::KeyEnv;
use vend_store::{BootstrapConfig, VendStore};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let database_url = require("DATABASE_URL")?;
    let tenant_name = require("VEND_TENANT_NAME")?;
    let app_name = require("VEND_APP_NAME")?;
    let mint_account_id = require("VEND_MINT_ACCOUNT_ID")?;
    let model_allowlist: Vec<String> = require("VEND_MODELS")?
        .split(',')
        .map(str::trim)
        .filter(|model| !model.is_empty())
        .map(str::to_string)
        .collect();

    let config = BootstrapConfig {
        tenant_name,
        app_name,
        mint_account_id,
        model_allowlist,
        zdr: parse_bool("VEND_ZDR", false)?,
        markup_policy: MarkupPolicy {
            markup_bps: parse_required("VEND_MARKUP_BPS")?,
            min_fee_micros: parse_required("VEND_MIN_FEE_MICROS")?,
            platform_fee_bps: parse_required("VEND_PLATFORM_FEE_BPS")?,
            funding_mode: funding_mode()?,
        },
        key_env: match optional("VEND_KEY_ENV").as_deref() {
            None | Some("live") => KeyEnv::Live,
            Some("test") => KeyEnv::Test,
            Some(value) => bail!("VEND_KEY_ENV must be live or test, got {value}"),
        },
        daily_cap_micros: Some(positive_required("VEND_DAILY_CAP_MICROS")?),
        per_end_user_daily_cap_micros: parse_optional("VEND_PER_END_USER_DAILY_CAP_MICROS")?,
        owner_email: require("VEND_OWNER_EMAIL")?,
        owner_display_name: require("VEND_OWNER_DISPLAY_NAME")?,
        setup_token_ttl_seconds: parse_optional("VEND_CUSTOMER_SETUP_TTL_SECONDS")?
            .unwrap_or(604_800),
    };

    let store = VendStore::connect(&database_url)
        .await
        .context("connecting to Vend Postgres")?;
    store.migrate().await.context("running Vend migrations")?;
    let output = store
        .bootstrap_product(config)
        .await
        .context("bootstrapping Vend product")?;

    println!(
        "{}",
        serde_json::to_string_pretty(&serde_json::json!({
            "tenant_id": output.tenant_id.as_str(),
            "app_id": output.app_id.as_str(),
            "sub_key_id": output.sub_key_id.as_str(),
            "provisioning_key": output.secret,
            "owner": output.owner,
            "owner_setup_token": output.owner_setup_token,
            "warning": "Store both one-time secrets now; deliver owner_setup_token out of band. Vend sends no email."
        }))?
    );
    Ok(())
}

fn require(name: &str) -> anyhow::Result<String> {
    optional(name).ok_or_else(|| anyhow::anyhow!("{name} must be set"))
}

fn optional(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn parse_required<T>(name: &str) -> anyhow::Result<T>
where
    T: std::str::FromStr,
    T::Err: std::fmt::Display,
{
    require(name)?
        .parse()
        .map_err(|error| anyhow::anyhow!("{name} is invalid: {error}"))
}

fn positive_required(name: &str) -> anyhow::Result<i64> {
    let value: i64 = parse_required(name)?;
    if value <= 0 {
        bail!("{name} must be positive")
    }
    Ok(value)
}

fn parse_optional<T>(name: &str) -> anyhow::Result<Option<T>>
where
    T: std::str::FromStr,
    T::Err: std::fmt::Display,
{
    optional(name)
        .map(|value| {
            value
                .parse()
                .map_err(|error| anyhow::anyhow!("{name} is invalid: {error}"))
        })
        .transpose()
}

fn parse_bool(name: &str, default: bool) -> anyhow::Result<bool> {
    match optional(name).as_deref() {
        None => Ok(default),
        Some("1" | "true" | "yes") => Ok(true),
        Some("0" | "false" | "no") => Ok(false),
        Some(value) => bail!("{name} must be true or false, got {value}"),
    }
}

fn funding_mode() -> anyhow::Result<FundingMode> {
    match optional("VEND_FUNDING_MODE").as_deref() {
        None | Some("tenant_reserve") => Ok(FundingMode::TenantReserve),
        Some("end_user_reserve") => Ok(FundingMode::EndUserReserve),
        Some(value) => {
            bail!("VEND_FUNDING_MODE must be tenant_reserve or end_user_reserve, got {value}")
        }
    }
}
