//! Deterministic fixtures: two isolated tenants, each with an app, a markup
//! policy, and a seeded provisioning key (whose one-time secret is retained so
//! tests can present it as a Bearer token and have `verify` succeed).

use vend_core::ids::{AppId, TenantId};
use vend_core::money::{FundingMode, MarkupPolicy};
use vend_core::records::{KeyScope, SubKeyRecord};
use vend_keys::{mint, KeyEnv, MintParams};
use vend_tenancy::{App, Tenant};

use crate::store::InMemoryStore;

/// A seeded tenant with its app, key row, and the plaintext key secret.
pub struct TenantFixture {
    /// The tenant.
    pub tenant: Tenant,
    /// The tenant's single app.
    pub app: App,
    /// The persisted key row (hash + prefix).
    pub key: SubKeyRecord,
    /// The one-time plaintext secret matching `key`.
    pub key_secret: String,
}

fn make_tenant(name: &str, markup_bps: u32, min_fee: i64) -> TenantFixture {
    let tenant = Tenant {
        id: TenantId::generate(),
        name: name.to_string(),
        mint_account_id: format!("acct_{name}_tenant"),
    };
    let app_id = AppId::generate();

    let app = App {
        id: app_id.clone(),
        tenant_id: tenant.id.clone(),
        name: format!("{name}-app"),
        model_allowlist: vec!["gpt-4o-mini".to_string()],
        zdr: false,
        enabled: true,
        markup_policy: MarkupPolicy {
            markup_bps,
            min_fee_micros: min_fee,
            platform_fee_bps: 1_000, // platform keeps 10% of the tenant's margin
            funding_mode: FundingMode::TenantReserve,
        },
        mint_account_id: format!("acct_{name}_app"),
        model_markups: Default::default(),
    };

    // Mint the key ONCE, bound to the real app id, so its hash + prefix match the
    // retained secret and `verify` resolves.
    let minted = mint(MintParams {
        tenant_id: tenant.id.clone(),
        app_id: Some(app_id),
        end_user_id: None,
        scope: KeyScope::App,
        env: KeyEnv::Test,
        daily_cap_micros: Some(50_000_000),
        per_end_user_daily_cap_micros: Some(5_000_000),
    })
    .expect("mint fixture key");

    TenantFixture {
        tenant,
        app,
        key: minted.record,
        key_secret: minted.secret,
    }
}

/// Two fully isolated tenants seeded into a fresh [`InMemoryStore`].
pub struct TwoTenants {
    /// The seeded store.
    pub store: InMemoryStore,
    /// First tenant fixture.
    pub a: TenantFixture,
    /// Second tenant fixture.
    pub b: TenantFixture,
}

/// Build two isolated tenants (distinct policies) seeded into an in-memory store,
/// each with an App-scoped key and a funded reserve mirror.
pub fn two_tenants() -> TwoTenants {
    let store = InMemoryStore::new();
    let a = make_tenant("alpha", 3_000, 1_000);
    let b = make_tenant("beta", 5_000, 0);

    for f in [&a, &b] {
        store.add_tenant(f.tenant.clone());
        store.add_app(f.app.clone());
        store.add_sub_key(f.key.clone());
        store.set_reserve(&f.tenant.id, 100_000_000);
    }

    TwoTenants { store, a, b }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn two_tenants_are_isolated() {
        let f = two_tenants();
        assert_ne!(f.a.tenant.id.as_str(), f.b.tenant.id.as_str());
        assert_ne!(f.a.app.id.as_str(), f.b.app.id.as_str());
        assert!(f.a.key_secret.starts_with("vk_test_"));
    }

    #[tokio::test]
    async fn seeded_key_verifies() {
        let f = two_tenants();
        // Both tenants' keys are minted back-to-back; their lookup shards must be
        // distinct so neither overwrites the other in the prefix-keyed store.
        assert_ne!(f.a.key.prefix, f.b.key.prefix);
        let rec_a = vend_keys::verify(&f.store, &f.a.key_secret).await.unwrap();
        assert_eq!(rec_a.id, f.a.key.id);
        let rec_b = vend_keys::verify(&f.store, &f.b.key_secret).await.unwrap();
        assert_eq!(rec_b.id, f.b.key.id);
    }
}
