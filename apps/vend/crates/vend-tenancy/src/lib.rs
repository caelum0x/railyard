//! # vend-tenancy — Tenant/App/EndUser hierarchy + resolution.
//!
//! Resolves `(sub_key, Vend-End-User external_ref)` into a concrete
//! `(tenant, app, end_user)` triple with Mint account back-pointers, loading the
//! app's `model_allowlist`/`zdr` flags and the effective [`MarkupPolicy`]. All
//! logic here is pure over the [`TenantRepo`] port; the concrete impl lives in
//! `vend-store`.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use vend_core::error::VendError;
use vend_core::ids::{AppId, EndUserId, TenantId};
use vend_core::money::MarkupPolicy;
use vend_core::records::{KeyScope, KeyStatus, SubKeyRecord};

/// A reseller. Owns apps; its Mint account is the tenant-net payout destination.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Tenant {
    /// Tenant id.
    pub id: TenantId,
    /// Human display name.
    pub name: String,
    /// Existing Mint account receiving the tenant-net leg of every split payout.
    pub mint_account_id: String,
}

/// A tenant's product surface. Carries routing + policy knobs.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct App {
    /// App id.
    pub id: AppId,
    /// Owning tenant.
    pub tenant_id: TenantId,
    /// Human display name.
    pub name: String,
    /// Models this app may call. Empty = allow all.
    pub model_allowlist: Vec<String>,
    /// Do not persist this app's inference in Relay observability. This does not
    /// by itself guarantee the selected upstream provider's retention policy.
    pub zdr: bool,
    /// Whether this app accepts new data-plane requests.
    pub enabled: bool,
    /// Effective pricing policy for the app.
    pub markup_policy: MarkupPolicy,
    /// Back-pointer to the app's Mint account (funding source).
    pub mint_account_id: String,
}

impl App {
    /// `true` iff `model` is permitted by the allowlist (empty allowlist = all).
    pub fn allows_model(&self, model: &str) -> bool {
        self.model_allowlist.is_empty() || self.model_allowlist.iter().any(|m| m == model)
    }
}

/// An app's customer. May have its own Mint reserve sub-account.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EndUser {
    /// End-user id.
    pub id: EndUserId,
    /// Owning app.
    pub app_id: AppId,
    /// Caller-supplied stable external reference (the `Vend-End-User` header).
    pub external_ref: String,
    /// Back-pointer to the end-user's Mint account, if provisioned.
    pub mint_account_id: Option<String>,
}

/// The fully-resolved request context: the verified key plus its tenant, app,
/// and (optionally) end-user.
#[derive(Clone, Debug)]
pub struct ResolvedContext {
    /// The verified provisioning key row.
    pub sub_key: SubKeyRecord,
    /// Resolved tenant.
    pub tenant: Tenant,
    /// Resolved app.
    pub app: App,
    /// Resolved end-user, when the request targets one.
    pub end_user: Option<EndUser>,
}

/// Persistence port for the tenancy hierarchy. Implemented in `vend-store`.
#[async_trait]
pub trait TenantRepo: Send + Sync {
    /// Look up a key row by its non-secret prefix shard.
    async fn find_sub_key_by_prefix(&self, prefix: &str)
        -> Result<Option<SubKeyRecord>, VendError>;
    /// Load a tenant by id.
    async fn load_tenant(&self, id: &TenantId) -> Result<Option<Tenant>, VendError>;
    /// Load an app by id.
    async fn load_app(&self, id: &AppId) -> Result<Option<App>, VendError>;
    /// Find an end-user by `(app, external_ref)`.
    async fn find_end_user(
        &self,
        app_id: &AppId,
        external_ref: &str,
    ) -> Result<Option<EndUser>, VendError>;
    /// Load an end-user by its typed id, scoped to its owning app.
    async fn load_end_user(
        &self,
        app_id: &AppId,
        id: &EndUserId,
    ) -> Result<Option<EndUser>, VendError>;
    /// Create an end-user with a freshly-provisioned Mint account back-pointer.
    async fn create_end_user(
        &self,
        app_id: &AppId,
        external_ref: &str,
        mint_account_id: &str,
    ) -> Result<EndUser, VendError>;
}

/// Resolve a verified key + optional end-user `external_ref` into a full
/// [`ResolvedContext`]. Pure over the repo port.
///
/// Rules:
/// - a revoked key is rejected (`Unauthorized`),
/// - the key must bind an app (directly, or via App/EndUser scope),
/// - for `EndUser` scope the key's own typed end-user binding wins,
/// - an `external_ref` header must resolve to a provisioned end user.
pub async fn resolve<R: TenantRepo + ?Sized>(
    repo: &R,
    key: &SubKeyRecord,
    external_ref: Option<&str>,
) -> Result<ResolvedContext, VendError> {
    if key.status == KeyStatus::Revoked {
        return Err(VendError::Unauthorized);
    }

    let tenant = repo
        .load_tenant(&key.tenant_id)
        .await?
        .ok_or(VendError::NotFound)?;

    let app_id = key.app_id.clone().ok_or(VendError::Unauthorized)?;
    let app = repo.load_app(&app_id).await?.ok_or(VendError::NotFound)?;
    if app.tenant_id != key.tenant_id {
        return Err(VendError::Unauthorized);
    }
    if !app.enabled {
        return Err(VendError::Unauthorized);
    }

    let end_user = match key.scope {
        KeyScope::EndUser => {
            let eu_id = key.end_user_id.as_ref().ok_or(VendError::Unauthorized)?;
            let eu = repo
                .load_end_user(&app_id, eu_id)
                .await?
                .ok_or(VendError::NotFound)?;
            // Header, if present, must name the same bound end-user.
            if external_ref.is_some_and(|value| value != eu.external_ref) {
                return Err(VendError::Unauthorized);
            }
            Some(eu)
        }
        KeyScope::Tenant | KeyScope::App => match external_ref {
            Some(ext) => Some(
                repo.find_end_user(&app_id, ext)
                    .await?
                    .ok_or(VendError::NotFound)?,
            ),
            None => None,
        },
    };

    if app.markup_policy.funding_mode == vend_core::money::FundingMode::EndUserReserve
        && end_user.is_none()
    {
        // Never silently charge the app account for an end-user-funded product.
        // A control-plane caller must provision the end user with a Mint account.
        return Err(VendError::NotFound);
    }

    Ok(ResolvedContext {
        sub_key: key.clone(),
        tenant,
        app,
        end_user,
    })
}

/// Choose the Mint account that should back a request's hold, honoring the app's
/// funding mode: an end-user reserve when present, else the app account.
pub fn funding_account_id(ctx: &ResolvedContext) -> &str {
    match ctx.app.markup_policy.funding_mode {
        vend_core::money::FundingMode::EndUserReserve => ctx
            .end_user
            .as_ref()
            .and_then(|eu| eu.mint_account_id.as_deref())
            .expect("resolved EndUserReserve context must have a Mint account"),
        _ => &ctx.app.mint_account_id,
    }
}
