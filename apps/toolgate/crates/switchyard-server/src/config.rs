//! Configuration from the environment. Secrets are read once at startup and
//! never logged.

use std::collections::BTreeMap;

#[derive(Clone, Debug)]
pub struct Settings {
    /// Public base URL (OAuth issuer, MCP resource, llms.txt links).
    pub public_url: String,
    /// Mint account captured calls settle to (Switchyard revenue).
    pub mint_revenue_account: Option<String>,
    /// Checkout provider for top-ups (`dodo` by default).
    pub topup_provider: String,
    pub topup_success_url: Option<String>,
    /// Operator token for `/admin`.
    pub admin_token: Option<String>,
    /// Secret mixed into consent-form CSRF tokens.
    pub form_secret: String,
    /// Non-secret adapter config (`relay_url`, `rally_url`, `searxng_url`, ...).
    pub adapter_config: BTreeMap<String, String>,
    /// Platform service credentials for our own products, by provider id.
    pub platform_credentials: BTreeMap<String, String>,
    pub cache_bytes: usize,
    pub idempotency_hours: i64,
}

impl Settings {
    pub fn for_tests(public_url: &str) -> Self {
        Settings {
            public_url: public_url.trim_end_matches('/').into(),
            mint_revenue_account: Some("acct_switchyard_revenue".into()),
            topup_provider: "dodo".into(),
            topup_success_url: None,
            admin_token: Some("admin-test-token".into()),
            form_secret: "test-form-secret".into(),
            adapter_config: BTreeMap::new(),
            platform_credentials: BTreeMap::new(),
            cache_bytes: 32 * 1024 * 1024,
            idempotency_hours: 24,
        }
    }
}

fn env(k: &str) -> Option<String> {
    std::env::var(k).ok().filter(|v| !v.trim().is_empty())
}

/// Adapter config keys and the env vars they come from.
pub const ADAPTER_ENV: &[(&str, &str)] = &[
    ("relay_url", "SWITCHYARD_RELAY_URL"),
    ("scribase_url", "SWITCHYARD_SCRIBASE_URL"),
    ("scribase_project_domain", "SWITCHYARD_SCRIBASE_PROJECT_DOMAIN"),
    ("rally_url", "SWITCHYARD_RALLY_URL"),
    ("menivor_url", "SWITCHYARD_MENIVOR_URL"),
    ("searxng_url", "SWITCHYARD_SEARXNG_URL"),
    ("contact_email", "SWITCHYARD_CONTACT_EMAIL"),
];

/// Platform credentials for own products (used when a team has none stored).
pub const PLATFORM_ENV: &[(&str, &str)] = &[("rally", "SWITCHYARD_RALLY_TOKEN"), ("menivor", "SWITCHYARD_MENIVOR_TOKEN")];

impl Settings {
    pub fn from_env() -> Self {
        let public_url = env("SWITCHYARD_PUBLIC_URL").unwrap_or_else(|| "http://127.0.0.1:8130".into());
        let mut adapter_config = BTreeMap::new();
        for (k, e) in ADAPTER_ENV {
            if let Some(v) = env(e) {
                adapter_config.insert(k.to_string(), v);
            }
        }
        let mut platform_credentials = BTreeMap::new();
        for (p, e) in PLATFORM_ENV {
            if let Some(v) = env(e) {
                platform_credentials.insert(p.to_string(), v);
            }
        }
        Settings {
            public_url: public_url.trim_end_matches('/').into(),
            mint_revenue_account: env("SWITCHYARD_MINT_REVENUE_ACCOUNT"),
            topup_provider: env("SWITCHYARD_TOPUP_PROVIDER").unwrap_or_else(|| "dodo".into()),
            topup_success_url: env("SWITCHYARD_TOPUP_SUCCESS_URL"),
            admin_token: env("SWITCHYARD_ADMIN_TOKEN"),
            form_secret: env("SWITCHYARD_FORM_SECRET").unwrap_or_else(switchyard_auth::random_token),
            adapter_config,
            platform_credentials,
            cache_bytes: env("SWITCHYARD_CACHE_MB").and_then(|v| v.parse::<usize>().ok()).unwrap_or(32) * 1024 * 1024,
            idempotency_hours: 24,
        }
    }
}
