//! End-user wallets: one Mint account per end user of a founder's app.
//!
//! Provisioning-key API (the founder's backend):
//! - `POST /v1/wallets` `{external_ref}` — create (idempotent) the wallet for
//!   one of the founder's users;
//! - `GET /v1/wallets/:external_ref` — live balance, held amount and today's
//!   spend against the per-end-user daily cap;
//! - `POST /v1/wallets/:external_ref/top-ups` `{provider, amount_micros}` — a
//!   hosted checkout (Creem / Polar / PayPal / Stripe / iyzico / USDC) whose
//!   signed provider webhook deposits straight into that wallet on Mint.
//!
//! Customer console mirrors: `/v1/customer/apps/:app_id/wallets[...]`.
//!
//! An `end_user_reserve` app then charges each request to the calling user's
//! wallet (`Vend-End-User: <external_ref>`), never to the app account.

use axum::extract::{Extension, Path, Query, State};
use axum::http::HeaderMap;
use axum::response::Response;
use axum::Json;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::json;

use vend_core::error::VendError;
use vend_core::ids::AppId;
use vend_core::money::FundingMode;
use vend_core::records::{KeyScope, SubKeyRecord};
use vend_core::transport::{CheckoutRequest, CreateAccountRequest, MintTransport};
use vend_keys::verify;
use vend_store::{AuditActorKind, AuditOutcome, CustomerPrincipal, CustomerRole};
use vend_tenancy::{resolve_app, App, EndUser, TenantRepo};

use super::{audit, fail, ok, ok_no_store, require_role, ConsoleApiState, WalletApiState};
use crate::{bearer, preflight_key};

/// Hosted top-up providers Mint can open a checkout for.
pub const TOP_UP_PROVIDERS: [&str; 6] = ["creem", "polar", "paypal", "stripe", "iyzico", "usdc"];
/// Largest single top-up: $10,000.
const MAX_TOP_UP_MICROS: i64 = 10_000 * 1_000_000;
/// Longest accepted external reference.
const MAX_EXTERNAL_REF: usize = 128;

#[derive(Deserialize)]
pub struct CreateWalletRequest {
    pub external_ref: String,
}

#[derive(Deserialize)]
pub struct TopUpRequest {
    pub provider: String,
    pub amount_micros: i64,
}

/// Public wallet view.
#[derive(Serialize)]
pub struct WalletView {
    pub external_ref: String,
    pub end_user_id: String,
    pub mint_account_id: String,
    pub available_micros: i64,
    pub held_micros: i64,
    pub spent_today_micros: i64,
    pub funding_mode: FundingMode,
    pub created: bool,
}

fn validate_external_ref(value: &str) -> Result<&str, VendError> {
    let value = value.trim();
    if value.is_empty()
        || value.len() > MAX_EXTERNAL_REF
        || !value.bytes().all(|b| b.is_ascii_graphic())
    {
        return Err(VendError::BadId);
    }
    Ok(value)
}

fn validate_top_up(input: &TopUpRequest) -> Result<(), VendError> {
    if !TOP_UP_PROVIDERS.contains(&input.provider.as_str()) {
        return Err(VendError::InvalidAmount);
    }
    // Whole cents only (providers charge in cents), bounded per checkout.
    if input.amount_micros <= 0
        || input.amount_micros % 10_000 != 0
        || input.amount_micros > MAX_TOP_UP_MICROS
    {
        return Err(VendError::InvalidAmount);
    }
    Ok(())
}

/// Find or create the wallet for `external_ref`. Opening the Mint account
/// happens before the local row; a concurrent creator that loses the unique
/// `(app_id, external_ref)` race re-reads the winner's row (the loser's Mint
/// account stays empty and unused).
pub async fn ensure_wallet(
    tenants: &dyn TenantRepo,
    mint: &dyn MintTransport,
    app: &App,
    external_ref: &str,
) -> Result<(EndUser, bool), VendError> {
    let external_ref = validate_external_ref(external_ref)?;
    if let Some(existing) = tenants.find_end_user(&app.id, external_ref).await? {
        return match existing.mint_account_id.as_deref() {
            Some(account) if !account.is_empty() => Ok((existing, false)),
            _ => Err(VendError::WalletUnprovisioned),
        };
    }
    let owner_ref: String = format!("vend:{}:{external_ref}", app.id.as_str())
        .chars()
        .take(256)
        .collect();
    let account = mint
        .create_account(CreateAccountRequest {
            kind: "user".into(),
            owner_ref: Some(owner_ref),
        })
        .await?;
    match tenants
        .create_end_user(&app.id, external_ref, &account.account_id)
        .await
    {
        Ok(end_user) => Ok((end_user, true)),
        Err(error) => match tenants.find_end_user(&app.id, external_ref).await? {
            Some(winner) if winner.mint_account_id.is_some() => Ok((winner, false)),
            _ => Err(error),
        },
    }
}

async fn wallet_view(
    mint: &dyn MintTransport,
    usage: Option<&dyn vend_core::ports::UsageRepo>,
    app: &App,
    end_user: &EndUser,
    created: bool,
) -> Result<WalletView, VendError> {
    let account = end_user
        .mint_account_id
        .clone()
        .filter(|a| !a.is_empty())
        .ok_or(VendError::WalletUnprovisioned)?;
    let balance = mint.get_balance(&account).await?;
    let spent_today_micros = match usage {
        Some(usage) => {
            let day = Utc::now().format("%Y-%m-%d").to_string();
            usage
                .sum_charge_for_end_user_day(&end_user.id, &day)
                .await?
        }
        None => 0,
    };
    Ok(WalletView {
        external_ref: end_user.external_ref.clone(),
        end_user_id: end_user.id.as_str().to_string(),
        mint_account_id: account,
        available_micros: balance.available_micros,
        held_micros: balance.held_micros,
        spent_today_micros,
        funding_mode: app.markup_policy.funding_mode,
        created,
    })
}

async fn top_up(
    tenants: &dyn TenantRepo,
    mint: &dyn MintTransport,
    app: &App,
    external_ref: &str,
    input: TopUpRequest,
) -> Result<vend_core::transport::CheckoutResponse, VendError> {
    validate_top_up(&input)?;
    let external_ref = validate_external_ref(external_ref)?;
    let end_user = tenants
        .find_end_user(&app.id, external_ref)
        .await?
        .ok_or(VendError::NotFound)?;
    let account = end_user
        .mint_account_id
        .filter(|a| !a.is_empty())
        .ok_or(VendError::WalletUnprovisioned)?;
    mint.create_checkout(CheckoutRequest {
        provider: input.provider,
        account_id: account,
        amount_micros: input.amount_micros,
    })
    .await
}

// --- provisioning-key API ------------------------------------------------------

/// Verify the key and its app. End-user-scoped keys act AS a user; they may
/// read and top up only their own wallet and never create wallets.
async fn authenticate(
    state: &WalletApiState,
    headers: &HeaderMap,
) -> Result<(SubKeyRecord, App), Response> {
    preflight_key(state.keys.as_ref(), state.security.as_ref(), headers).await?;
    let result = async {
        let key = verify(state.keys.as_ref(), bearer(headers)?).await?;
        let (_, app) = resolve_app(state.tenants.as_ref(), &key).await?;
        Ok::<_, VendError>((key, app))
    }
    .await;
    result.map_err(fail)
}

async fn authorize_end_user_key(
    state: &WalletApiState,
    key: &SubKeyRecord,
    app: &App,
    external_ref: &str,
) -> Result<(), VendError> {
    if key.scope != KeyScope::EndUser {
        return Ok(());
    }
    let bound = key.end_user_id.as_ref().ok_or(VendError::Unauthorized)?;
    let end_user = state
        .tenants
        .load_end_user(&app.id, bound)
        .await?
        .ok_or(VendError::Unauthorized)?;
    if end_user.external_ref != external_ref.trim() {
        return Err(VendError::Unauthorized);
    }
    Ok(())
}

pub async fn create_wallet(
    State(state): State<WalletApiState>,
    headers: HeaderMap,
    Json(input): Json<CreateWalletRequest>,
) -> Response {
    let (key, app) = match authenticate(&state, &headers).await {
        Ok(found) => found,
        Err(response) => return response,
    };
    if key.scope == KeyScope::EndUser {
        return fail(VendError::Unauthorized);
    }
    let result = async {
        let (end_user, created) = ensure_wallet(
            state.tenants.as_ref(),
            state.mint.as_ref(),
            &app,
            &input.external_ref,
        )
        .await?;
        wallet_view(
            state.mint.as_ref(),
            Some(state.usage.as_ref()),
            &app,
            &end_user,
            created,
        )
        .await
    }
    .await;
    if let (Ok(view), Some(security)) = (&result, &state.security) {
        if view.created {
            audit(
                &security.repo,
                Some(app.tenant_id.clone()),
                (
                    AuditActorKind::ProvisioningKey,
                    Some(key.id.as_str().to_string()),
                ),
                "wallet.created",
                ("end_user", &view.end_user_id),
                AuditOutcome::Succeeded,
                json!({"app_id": app.id.as_str()}),
            )
            .await;
        }
    }
    match result {
        Ok(view) => ok(view),
        Err(error) => fail(error),
    }
}

pub async fn get_wallet(
    State(state): State<WalletApiState>,
    headers: HeaderMap,
    Path(external_ref): Path<String>,
) -> Response {
    let (key, app) = match authenticate(&state, &headers).await {
        Ok(found) => found,
        Err(response) => return response,
    };
    let result = async {
        authorize_end_user_key(&state, &key, &app, &external_ref).await?;
        let reference = validate_external_ref(&external_ref)?;
        let end_user = state
            .tenants
            .find_end_user(&app.id, reference)
            .await?
            .ok_or(VendError::NotFound)?;
        wallet_view(
            state.mint.as_ref(),
            Some(state.usage.as_ref()),
            &app,
            &end_user,
            false,
        )
        .await
    }
    .await;
    match result {
        Ok(view) => ok(view),
        Err(error) => fail(error),
    }
}

pub async fn create_top_up(
    State(state): State<WalletApiState>,
    headers: HeaderMap,
    Path(external_ref): Path<String>,
    Json(input): Json<TopUpRequest>,
) -> Response {
    let (key, app) = match authenticate(&state, &headers).await {
        Ok(found) => found,
        Err(response) => return response,
    };
    let result = async {
        authorize_end_user_key(&state, &key, &app, &external_ref).await?;
        top_up(
            state.tenants.as_ref(),
            state.mint.as_ref(),
            &app,
            &external_ref,
            input,
        )
        .await
    }
    .await;
    match result {
        Ok(checkout) => ok_no_store(checkout),
        Err(error) => fail(error),
    }
}

// --- customer console ------------------------------------------------------------

/// The app, if the signed-in customer's tenant owns it (else `NotFound`).
pub(crate) async fn owned_app(
    state: &ConsoleApiState,
    principal: &CustomerPrincipal,
    raw: &str,
) -> Result<App, VendError> {
    let id = AppId::parse(raw)?;
    let app = state
        .tenants
        .load_app(&id)
        .await?
        .ok_or(VendError::NotFound)?;
    if app.tenant_id != principal.tenant_id {
        return Err(VendError::NotFound);
    }
    Ok(app)
}

#[derive(Deserialize)]
pub struct ListQuery {
    pub limit: Option<i64>,
    pub offset: Option<i64>,
}

/// Wallets with live Mint balances. Balances are read concurrently; a wallet
/// whose balance cannot be read is returned with `balance_error` set.
pub async fn customer_list(
    State(state): State<ConsoleApiState>,
    Extension(principal): Extension<CustomerPrincipal>,
    Path(raw): Path<String>,
    Query(query): Query<ListQuery>,
) -> Response {
    let app = match owned_app(&state, &principal, &raw).await {
        Ok(app) => app,
        Err(error) => return fail(error),
    };
    let day = Utc::now().format("%Y-%m-%d").to_string();
    let limit = query.limit.unwrap_or(50).clamp(1, 100);
    let wallets = match state
        .whitelabel
        .list_wallets(&app.id, &day, limit, query.offset.unwrap_or(0))
        .await
    {
        Ok(wallets) => wallets,
        Err(error) => return fail(error),
    };
    let balances = futures::future::join_all(wallets.iter().map(|w| {
        let mint = state.mint.clone();
        let account = w.mint_account_id.clone();
        async move {
            match account {
                Some(account) if !account.is_empty() => Some(mint.get_balance(&account).await),
                _ => None,
            }
        }
    }))
    .await;
    let rows: Vec<serde_json::Value> = wallets
        .iter()
        .zip(balances)
        .map(|(w, balance)| {
            let mut row = json!({
                "end_user_id": w.end_user_id,
                "external_ref": w.external_ref,
                "mint_account_id": w.mint_account_id,
                "spent_today_micros": w.spent_today_micros,
                "created_at_ms": w.created_at_ms,
            });
            match balance {
                Some(Ok(b)) => {
                    row["available_micros"] = json!(b.available_micros);
                    row["held_micros"] = json!(b.held_micros);
                }
                Some(Err(error)) => row["balance_error"] = json!(error.client_message()),
                None => row["balance_error"] = json!("no wallet (attribution only)"),
            }
            row
        })
        .collect();
    ok(json!({
        "funding_mode": app.markup_policy.funding_mode,
        "wallets": rows,
    }))
}

pub async fn customer_create(
    State(state): State<ConsoleApiState>,
    Extension(principal): Extension<CustomerPrincipal>,
    Path(raw): Path<String>,
    Json(input): Json<CreateWalletRequest>,
) -> Response {
    if let Err(response) = require_role(&principal, CustomerRole::Admin) {
        return response;
    }
    let app = match owned_app(&state, &principal, &raw).await {
        Ok(app) => app,
        Err(error) => return fail(error),
    };
    let result = async {
        let (end_user, created) = ensure_wallet(
            state.tenants.as_ref(),
            state.mint.as_ref(),
            &app,
            &input.external_ref,
        )
        .await?;
        wallet_view(state.mint.as_ref(), None, &app, &end_user, created).await
    }
    .await;
    if let Ok(view) = &result {
        if view.created {
            audit(
                &state.security_repo,
                Some(app.tenant_id.clone()),
                (AuditActorKind::Customer, Some(principal.user_id.clone())),
                "wallet.created",
                ("end_user", &view.end_user_id),
                AuditOutcome::Succeeded,
                json!({"app_id": app.id.as_str()}),
            )
            .await;
        }
    }
    match result {
        Ok(view) => ok(view),
        Err(error) => fail(error),
    }
}

pub async fn customer_top_up(
    State(state): State<ConsoleApiState>,
    Extension(principal): Extension<CustomerPrincipal>,
    Path((raw, external_ref)): Path<(String, String)>,
    Json(input): Json<TopUpRequest>,
) -> Response {
    if let Err(response) = require_role(&principal, CustomerRole::Admin) {
        return response;
    }
    let app = match owned_app(&state, &principal, &raw).await {
        Ok(app) => app,
        Err(error) => return fail(error),
    };
    match top_up(
        state.tenants.as_ref(),
        state.mint.as_ref(),
        &app,
        &external_ref,
        input,
    )
    .await
    {
        Ok(checkout) => ok_no_store(checkout),
        Err(error) => fail(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn external_refs_are_bounded_printable_ascii() {
        assert!(validate_external_ref("user-123").is_ok());
        assert!(validate_external_ref("  ").is_err());
        assert!(validate_external_ref("has space").is_err());
        assert!(validate_external_ref(&"x".repeat(129)).is_err());
    }

    #[test]
    fn top_ups_are_whole_cents_on_known_providers() {
        let ok = TopUpRequest {
            provider: "creem".into(),
            amount_micros: 5_000_000,
        };
        assert!(validate_top_up(&ok).is_ok());
        let fractional = TopUpRequest {
            provider: "creem".into(),
            amount_micros: 5_000_001,
        };
        assert!(validate_top_up(&fractional).is_err());
        let unknown = TopUpRequest {
            provider: "venmo".into(),
            amount_micros: 5_000_000,
        };
        assert!(validate_top_up(&unknown).is_err());
        let huge = TopUpRequest {
            provider: "polar".into(),
            amount_micros: MAX_TOP_UP_MICROS + 10_000,
        };
        assert!(validate_top_up(&huge).is_err());
    }
}
