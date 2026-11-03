//! Live-Postgres proof that `ControlPlaneRepo::issue_app_key` can actually
//! mint `KeyScope::EndUser` keys — the finding this closes: `resolve()` /
//! `wallets.rs` fully enforce `KeyScope::EndUser`, but before this fix every
//! real `mint()` call hardcoded `scope: KeyScope::App, end_user_id: None`, so
//! an `EndUser`-scoped key could never be created through any real path.
//!
//! Gated: skipped (no-op pass) unless a Postgres URL is provided via
//! `VEND_TEST_DATABASE_URL`, `VEND_DATABASE_URL`, or `DATABASE_URL`.

use std::time::Duration;

use sqlx::postgres::PgPoolOptions;

use vend_core::error::VendError;
use vend_core::money::{FundingMode, MarkupPolicy};
use vend_core::records::KeyScope;
use vend_keys::KeyEnv;
use vend_store::{BootstrapConfig, ControlPlaneRepo, CreateAppKey, VendStore};

fn database_url() -> Option<String> {
    for var in [
        "VEND_TEST_DATABASE_URL",
        "VEND_DATABASE_URL",
        "DATABASE_URL",
    ] {
        if let Ok(value) = std::env::var(var) {
            if !value.trim().is_empty() {
                return Some(value);
            }
        }
    }
    None
}

async fn connect_and_migrate() -> VendStore {
    let url = database_url().expect("database url present (checked by caller)");
    let pool = PgPoolOptions::new()
        .max_connections(4)
        .acquire_timeout(Duration::from_secs(30))
        .connect(&url)
        .await
        .expect("connect to test Postgres");
    let store = VendStore::from_pool(pool);
    store.migrate().await.expect("run migrations");
    store
}

/// Bootstrap a fresh tenant + app, returning the app id. Each call uses a
/// fresh random suffix so concurrent test runs never collide.
async fn bootstrap_app(store: &VendStore, label: &str) -> vend_core::ids::AppId {
    let out = store
        .bootstrap(BootstrapConfig {
            tenant_name: format!("eu-scope-tenant-{label}"),
            app_name: format!("eu-scope-app-{label}"),
            mint_account_id: format!("acct_eu_scope_{label}"),
            model_allowlist: vec!["tensorzero::model_name::gpt-4o-mini".to_string()],
            zdr: false,
            markup_policy: MarkupPolicy {
                markup_bps: 2_000,
                min_fee_micros: 0,
                platform_fee_bps: 1_000,
                funding_mode: FundingMode::TenantReserve,
            },
            key_env: KeyEnv::Test,
            daily_cap_micros: Some(100_000_000),
            per_end_user_daily_cap_micros: None,
            owner_email: format!("owner-{label}@example.com"),
            owner_display_name: "Test Owner".to_string(),
            setup_token_ttl_seconds: 3600,
        })
        .await
        .expect("bootstrap tenant+app");
    out.app_id
}

#[tokio::test]
async fn issuing_an_end_user_scoped_key_binds_the_resolved_end_user_id() {
    if database_url().is_none() {
        eprintln!("skipping issuing_an_end_user_scoped_key_binds_the_resolved_end_user_id: no test DB url");
        return;
    }
    let store = connect_and_migrate().await;
    let app_id = bootstrap_app(&store, &vend_core::ids::SubKeyId::generate().to_string()).await;

    let end_user = store
        .provision_end_user(&app_id, "customer-abc", "acct_customer_abc")
        .await
        .expect("provision end user");

    let issued = store
        .issue_app_key(
            &app_id,
            CreateAppKey {
                env: KeyEnv::Test,
                scope: KeyScope::EndUser,
                end_user_external_ref: Some("customer-abc".to_string()),
                daily_cap_micros: None,
                per_end_user_daily_cap_micros: None,
            },
        )
        .await
        .expect("issue end-user-scoped key");

    // The summary reflects the requested scope.
    assert_eq!(issued.key.scope, KeyScope::EndUser);

    // Ground truth: the persisted row (reached the same way the data plane
    // reaches it, via `verify`) is actually bound to the resolved end-user's
    // typed id — this is what `resolve()` / `wallets.rs` require to accept
    // the key at all.
    let record = vend_keys::verify(&store, &issued.secret)
        .await
        .expect("verify newly issued key");
    assert_eq!(record.scope, KeyScope::EndUser);
    assert_eq!(record.end_user_id, Some(end_user.id));
}

#[tokio::test]
async fn end_user_scoped_key_request_is_rejected_without_a_valid_external_ref() {
    if database_url().is_none() {
        eprintln!("skipping end_user_scoped_key_request_is_rejected_without_a_valid_external_ref: no test DB url");
        return;
    }
    let store = connect_and_migrate().await;
    let app_id = bootstrap_app(&store, &vend_core::ids::SubKeyId::generate().to_string()).await;

    // Missing ref entirely.
    let missing_ref = store
        .issue_app_key(
            &app_id,
            CreateAppKey {
                env: KeyEnv::Test,
                scope: KeyScope::EndUser,
                end_user_external_ref: None,
                daily_cap_micros: None,
                per_end_user_daily_cap_micros: None,
            },
        )
        .await;
    match missing_ref {
        Err(err) => assert_eq!(err, VendError::InvalidAmount),
        Ok(_) => panic!("expected InvalidAmount for a missing end-user ref"),
    }

    // A ref that was never provisioned for this app.
    let unknown_ref = store
        .issue_app_key(
            &app_id,
            CreateAppKey {
                env: KeyEnv::Test,
                scope: KeyScope::EndUser,
                end_user_external_ref: Some("never-provisioned".to_string()),
                daily_cap_micros: None,
                per_end_user_daily_cap_micros: None,
            },
        )
        .await;
    match unknown_ref {
        Err(err) => assert_eq!(err, VendError::NotFound),
        Ok(_) => panic!("expected NotFound for an unprovisioned end-user ref"),
    }
}

#[tokio::test]
async fn end_user_scoped_key_cannot_bind_another_apps_end_user() {
    if database_url().is_none() {
        eprintln!("skipping end_user_scoped_key_cannot_bind_another_apps_end_user: no test DB url");
        return;
    }
    let store = connect_and_migrate().await;
    let app_a = bootstrap_app(
        &store,
        &format!("a-{}", vend_core::ids::SubKeyId::generate()),
    )
    .await;
    let app_b = bootstrap_app(
        &store,
        &format!("b-{}", vend_core::ids::SubKeyId::generate()),
    )
    .await;

    // "shared-ref" is provisioned under app A only.
    store
        .provision_end_user(&app_a, "shared-ref", "acct_shared_ref")
        .await
        .expect("provision end user under app A");

    // Issuing an EndUser-scoped key under app B with that same external_ref
    // must NOT resolve to app A's end-user — `find_end_user` is scoped by
    // `app_id`, so this is itself the belongs-to-this-app check.
    let cross_app = store
        .issue_app_key(
            &app_b,
            CreateAppKey {
                env: KeyEnv::Test,
                scope: KeyScope::EndUser,
                end_user_external_ref: Some("shared-ref".to_string()),
                daily_cap_micros: None,
                per_end_user_daily_cap_micros: None,
            },
        )
        .await;
    match cross_app {
        Err(err) => assert_eq!(err, VendError::NotFound),
        Ok(_) => panic!("expected NotFound: app B must not resolve app A's end-user"),
    }
}

#[tokio::test]
async fn app_scoped_key_request_rejects_an_end_user_ref() {
    if database_url().is_none() {
        eprintln!("skipping app_scoped_key_request_rejects_an_end_user_ref: no test DB url");
        return;
    }
    let store = connect_and_migrate().await;
    let app_id = bootstrap_app(&store, &vend_core::ids::SubKeyId::generate().to_string()).await;

    // Scope "app" (the only scope mintable before this fix) must keep
    // working exactly as before, and must reject a stray end-user ref rather
    // than silently ignoring it.
    let ok = store
        .issue_app_key(
            &app_id,
            CreateAppKey {
                env: KeyEnv::Test,
                scope: KeyScope::App,
                end_user_external_ref: None,
                daily_cap_micros: Some(50_000_000),
                per_end_user_daily_cap_micros: None,
            },
        )
        .await
        .expect("app-scoped key issuance is unaffected by this fix");
    assert_eq!(ok.key.scope, KeyScope::App);

    let rejected = store
        .issue_app_key(
            &app_id,
            CreateAppKey {
                env: KeyEnv::Test,
                scope: KeyScope::App,
                end_user_external_ref: Some("should-not-be-here".to_string()),
                daily_cap_micros: None,
                per_end_user_daily_cap_micros: None,
            },
        )
        .await;
    match rejected {
        Err(err) => assert_eq!(err, VendError::InvalidAmount),
        Ok(_) => panic!("expected InvalidAmount: App scope must reject a stray end-user ref"),
    }
}
