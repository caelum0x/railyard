//! Live-Postgres proof of a real cross-tenant fund-custody gap:
//! `ControlPlaneRepo::provision_end_user` (backing both
//! `POST /v1/control/apps/:app_id/end-users` — operator, trusted by design,
//! per the README's documented "map an existing Mint account by hand"
//! feature — and `POST /v1/customer/apps/:app_id/end-users` — any self-serve
//! tenant admin) accepts a caller-supplied `mint_account_id` with ZERO check
//! that the account was ever created for, or belongs to, the app/tenant doing
//! the binding.
//!
//! This test shows the blast radius through the REAL resolution path a
//! metered request actually takes: once a second, unrelated tenant ("B",
//! playing the attacker) provisions an end-user pointed at tenant A's real
//! wallet account, `vend_tenancy::resolve` + `funding_account_id` — the exact
//! functions `vend-server::run_flow` calls on every `/v1/chat/completions`
//! request — hand back tenant A's account as the funding source for tenant
//! B's own metered traffic. Every hold Mint opens for that traffic would be
//! placed against tenant A's money, not tenant B's.
//!
//! The fix belongs in `vend-server::control.rs`'s customer-facing handler
//! (the only place with both a `MintTransport` and the untrusted caller),
//! not in this shared store method — the admin route's documented
//! hand-mapping capability must keep working unmodified. See
//! `crates/vend-server/src/control.rs`'s `verify_customer_owns_mint_account`
//! and its own unit test for the other half of this proof.
//!
//! Gated: skipped (no-op pass) unless a Postgres URL is provided via
//! `VEND_TEST_DATABASE_URL`, `VEND_DATABASE_URL`, or `DATABASE_URL`.

use std::time::Duration;

use sqlx::postgres::PgPoolOptions;

use vend_core::money::{FundingMode, MarkupPolicy};
use vend_core::records::KeyScope;
use vend_keys::KeyEnv;
use vend_store::{BootstrapConfig, ControlPlaneRepo, CreateAppKey, VendStore};
use vend_tenancy::{funding_account_id, resolve, TenantRepo};

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

async fn bootstrap_app(store: &VendStore, label: &str) -> vend_core::ids::AppId {
    let out = store
        .bootstrap(BootstrapConfig {
            tenant_name: format!("xtenant-{label}"),
            app_name: format!("xapp-{label}"),
            mint_account_id: format!("acct_platform_{label}"),
            model_allowlist: vec!["tensorzero::model_name::gpt-4o-mini".to_string()],
            zdr: false,
            markup_policy: MarkupPolicy {
                markup_bps: 2_000,
                min_fee_micros: 0,
                platform_fee_bps: 1_000,
                // The vulnerable funding mode: a request is held/captured on
                // the resolved end-user's OWN Mint wallet.
                funding_mode: FundingMode::EndUserReserve,
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
async fn unrelated_tenant_can_bind_a_victim_tenants_real_wallet_as_its_own_end_user() {
    if database_url().is_none() {
        eprintln!(
            "skipping unrelated_tenant_can_bind_a_victim_tenants_real_wallet_as_its_own_end_user: no test DB url"
        );
        return;
    }
    let store = connect_and_migrate().await;
    let suffix = vend_core::ids::SubKeyId::generate().to_string();
    let app_victim = bootstrap_app(&store, &format!("victim-{suffix}")).await;
    let app_attacker = bootstrap_app(&store, &format!("attacker-{suffix}")).await;

    // Tenant A (victim): a real wallet, the way `ensure_wallet` actually
    // provisions one in production — one real Mint account, bound to this
    // app's own end user.
    const VICTIM_WALLET: &str = "acct_victim_real_wallet";
    store
        .provision_end_user(&app_victim, "victim-user", VICTIM_WALLET)
        .await
        .expect("victim's own wallet is provisioned legitimately");

    // Tenant B (attacker, fully unrelated — a different bootstrap, different
    // tenant row, owns a different Mint account entirely): calls the exact
    // same `provision_end_user` that both
    // `POST /v1/control/apps/:app_id/end-users` (operator) and
    // `POST /v1/customer/apps/:app_id/end-users` (self-serve customer admin)
    // invoke, naming the VICTIM's real wallet as ITS OWN end-user's funding
    // account. Nothing here proves B ever controlled that account.
    let bound = store
        .provision_end_user(&app_attacker, "attacker-user", VICTIM_WALLET)
        .await;
    assert!(
        bound.is_ok(),
        "the vulnerable path accepts an arbitrary, unverified mint_account_id \
         from a completely unrelated tenant: {bound:?}"
    );

    // Prove the blast radius: issue an EndUser-scoped key for the attacker's
    // app, bound to "attacker-user", and run it through the REAL resolution
    // path `vend-server::run_flow` calls on every metered request.
    let issued = store
        .issue_app_key(
            &app_attacker,
            CreateAppKey {
                env: KeyEnv::Test,
                scope: KeyScope::EndUser,
                end_user_external_ref: Some("attacker-user".to_string()),
                daily_cap_micros: None,
                per_end_user_daily_cap_micros: None,
            },
        )
        .await
        .expect("issue the attacker's own end-user-scoped key");
    let key = vend_keys::verify(&store, &issued.secret)
        .await
        .expect("verify the attacker's freshly issued key");

    let ctx = resolve(&store, &key, Some("attacker-user"))
        .await
        .expect("resolve succeeds — nothing here detects the cross-tenant binding");

    // The smoking gun: funding_account_id (what `run_flow` step 1 uses to
    // open every Mint hold) resolves to the VICTIM's wallet for a request
    // authenticated entirely under the ATTACKER's own tenant/app/key.
    let funding = funding_account_id(&ctx).expect("end_user_reserve funding account");
    assert_eq!(
        funding, VICTIM_WALLET,
        "every /v1/chat/completions request the attacker makes under this key \
         would open a real Mint hold against the victim's wallet, not the \
         attacker's own money"
    );

    // Sanity: the attacker's app really is a separate tenant/app from the
    // victim's, so this is not merely one tenant re-using its own account.
    let attacker_app = store
        .load_app(&app_attacker)
        .await
        .expect("load attacker app")
        .expect("attacker app exists");
    let victim_app = store
        .load_app(&app_victim)
        .await
        .expect("load victim app")
        .expect("victim app exists");
    assert_ne!(attacker_app.tenant_id, victim_app.tenant_id);
}
