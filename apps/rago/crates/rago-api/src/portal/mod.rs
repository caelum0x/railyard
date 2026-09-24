//! Free hosted customer portal (`/portal/:token`).
//!
//! Lago keeps its customer portal behind a paid plan, and the only "portal" Rago
//! inherited was a redirect into Stripe's Billing Portal. This portal is
//! provider neutral and needs no payment provider at all:
//!
//! * invoices with PDF download and a Pay button (collected through the
//!   organization's provider: Stripe, GoCardless, Adyen, Creem, Dodo or Iyzico;
//!   hosted-checkout providers redirect the customer to their pay page);
//! * current-period usage per billable metric, priced by the billing engine;
//! * prepaid wallet balances with a top-up that creates the paid-credit
//!   invoice and takes the customer straight to payment;
//! * a "change or cancel" page per subscription: switch to a cheaper plan at
//!   the end of the period (scheduled downgrade, see [`crate::plan_change`]) or
//!   cancel now after an explicit confirmation (same terminate service as the
//!   API);
//! * payment-method updates, offered only when Stripe is the provider (the
//!   only one with a hosted payment-method page).
//!
//! Access: `POST /api/v1/customers/:external_id/portal_sessions` issues a
//! signed, expiring link (see [`token`]) backed by a revocable
//! `customer_portal_sessions` row. `DELETE` on the same path revokes every open
//! link of the customer. Pages are served with `no-store`, `no-referrer` and a
//! restrictive CSP, so the token never leaks through caches, referers or frames.

pub mod pages;
pub mod token;

use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderValue, StatusCode};
use axum::response::{Html, IntoResponse, Response};
use axum::Json;
use chrono::{DateTime, Duration, Utc};
use rago_core::enums::{SubscriptionStatus, WalletStatus};
use rago_core::models::Customer;
use rust_decimal::Decimal;
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::auth::Authed;
use crate::error::{ApiError, ApiResult};
use crate::state::AppState;

use self::token::{PortalClaims, TokenError};

/// Default link lifetime: 24 hours.
pub const DEFAULT_TTL_MINUTES: i64 = 24 * 60;
/// Longest link lifetime an operator may request: 30 days.
pub const MAX_TTL_MINUTES: i64 = 30 * 24 * 60;
/// Largest single top-up (credits).
const MAX_TOP_UP_CREDITS: i64 = 1_000_000;
/// Invoices shown on the portal.
const INVOICE_LIMIT: i64 = 50;

/// A freshly issued portal link.
#[derive(Debug, Clone)]
pub struct IssuedPortal {
    pub session_id: Uuid,
    pub token: String,
    pub url: String,
    pub expires_at: DateTime<Utc>,
}

/// The public URL of a portal token (`RAGO_PUBLIC_URL/portal/<token>`, or a
/// path when no public URL is configured).
pub fn portal_link(token: &str) -> String {
    let base = std::env::var(rago_jobs::webhook_emit::PUBLIC_URL_ENV)
        .ok()
        .map(|b| b.trim().trim_end_matches('/').to_string())
        .unwrap_or_default();
    format!("{base}/portal/{token}")
}

async fn org_key(st: &AppState, organization_id: Uuid) -> ApiResult<Option<[u8; 32]>> {
    Ok(st
        .db
        .organization_hmac_key(organization_id)
        .await?
        .filter(|k| !k.is_empty())
        .map(|k| token::derive_key(&k)))
}

/// Issue a portal link for `customer`, valid for `ttl_minutes`.
pub async fn issue(st: &AppState, customer: &Customer, ttl_minutes: i64) -> ApiResult<IssuedPortal> {
    if !(1..=MAX_TTL_MINUTES).contains(&ttl_minutes) {
        return Err(ApiError::validation("expires_in_minutes", "value_is_out_of_range"));
    }
    let key = org_key(st, customer.organization_id)
        .await?
        .ok_or_else(|| ApiError::validation("portal", "organization_signing_key_missing"))?;
    let session_id = Uuid::new_v4();
    let expires_at = Utc::now() + Duration::minutes(ttl_minutes);
    st.db
        .insert_portal_session(session_id, customer.organization_id, customer.id, expires_at)
        .await?;
    let claims = PortalClaims {
        session_id,
        organization_id: customer.organization_id,
        customer_id: customer.id,
        expires_at: expires_at.timestamp(),
    };
    let token = token::sign(&claims, &key);
    Ok(IssuedPortal { session_id, url: portal_link(&token), token, expires_at })
}

/// A verified portal request.
struct Access {
    token: String,
    claims: PortalClaims,
    customer: Customer,
    expires_at: DateTime<Utc>,
}

/// Verify signature, expiry, revocation and ownership. `None` = refuse.
async fn authorize(st: &AppState, raw: &str) -> Result<Option<Access>, ApiError> {
    let Ok(peeked) = token::peek(raw) else { return Ok(None) };
    let Some(key) = org_key(st, peeked.organization_id).await? else { return Ok(None) };
    let claims = match token::verify(raw, &key, Utc::now().timestamp()) {
        Ok(c) => c,
        Err(TokenError::Malformed | TokenError::BadSignature | TokenError::Expired) => return Ok(None),
    };
    let Some(session) = st.db.touch_portal_session(claims.session_id).await? else { return Ok(None) };
    if session.revoked_at.is_some()
        || session.expires_at <= Utc::now()
        || session.organization_id != claims.organization_id
        || session.customer_id != claims.customer_id
    {
        return Ok(None);
    }
    let Some(customer) = st.db.find_customer(claims.customer_id).await? else { return Ok(None) };
    if customer.organization_id != claims.organization_id || customer.deleted_at.is_some() {
        return Ok(None);
    }
    Ok(Some(Access { token: raw.to_string(), claims, customer, expires_at: session.expires_at }))
}

/// Security headers for every portal response.
fn harden(mut resp: Response) -> Response {
    let h = resp.headers_mut();
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    h.insert(header::REFERRER_POLICY, HeaderValue::from_static("no-referrer"));
    h.insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
    h.insert(header::X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    h.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(
            "default-src 'none'; style-src 'unsafe-inline'; form-action 'self' https:; base-uri 'none'; frame-ancestors 'none'",
        ),
    );
    h.insert("x-robots-tag", HeaderValue::from_static("noindex, nofollow"));
    resp
}

fn invalid() -> Response {
    harden((StatusCode::NOT_FOUND, Html(pages::invalid_link())).into_response())
}

fn server_error(e: impl std::fmt::Debug) -> Response {
    tracing::error!(error = ?e, "customer portal error");
    harden((StatusCode::INTERNAL_SERVER_ERROR, Html(pages::invalid_link().replace(
        "This link is no longer valid",
        "Something went wrong",
    ).replace(
        "Billing links expire and can be revoked. Ask the company that bills you for a new link.",
        "Please try again in a moment.",
    ))).into_response())
}

/// 303 back to the portal home with a notice code.
fn back(token: &str, notice: &str) -> Response {
    let target = format!("/portal/{token}?notice={notice}");
    harden((StatusCode::SEE_OTHER, [(header::LOCATION, target)]).into_response())
}

/// 303 to a provider-hosted page (https only).
fn away(url: &str) -> Option<Response> {
    url.starts_with("https://")
        .then(|| harden((StatusCode::SEE_OTHER, [(header::LOCATION, url.to_string())]).into_response()))
}

#[derive(Debug, Deserialize)]
pub struct PageQuery {
    #[serde(default)]
    pub notice: Option<String>,
}

/// `GET /portal/:token`.
pub async fn home(
    State(st): State<AppState>,
    Path(raw): Path<String>,
    Query(q): Query<PageQuery>,
) -> Response {
    let access = match authorize(&st, &raw).await {
        Ok(Some(a)) => a,
        Ok(None) => return invalid(),
        Err(e) => return server_error(e),
    };
    match render_home(&st, &access, q.notice.as_deref()).await {
        Ok(html) => harden((StatusCode::OK, Html(html)).into_response()),
        Err(e) => server_error(e),
    }
}

async fn render_home(st: &AppState, a: &Access, notice: Option<&str>) -> ApiResult<String> {
    let org = a.claims.organization_id;
    let org_name = st.db.organization_name(org).await?.unwrap_or_else(|| "Billing".into());
    let invoices = st.db.invoices_for_customer(a.customer.id, INVOICE_LIMIT, 0).await?;
    let subs = st.db.subscriptions_for_customer(a.customer.id, 100, 0).await?;
    let mut subscriptions = Vec::new();
    let mut usage = Vec::new();
    let mut scheduled = Vec::new();
    for sub in subs {
        if !matches!(sub.status, SubscriptionStatus::Active | SubscriptionStatus::Pending) {
            continue;
        }
        // A scheduled downgrade is shown on the subscription it replaces.
        if sub.previous_subscription_id.is_some() && matches!(sub.status, SubscriptionStatus::Pending) {
            if let (Some(prev), Some(at)) = (sub.previous_subscription_id, sub.subscription_at) {
                let next = st.db.find_plan(sub.plan_id).await?;
                scheduled.push((prev, next.name.clone(), at.date_naive()));
            }
            continue;
        }
        let plan = st.db.find_plan(sub.plan_id).await?;
        if matches!(sub.status, SubscriptionStatus::Active) {
            let (currency, from, to, rows) = crate::usage::current_period_usage(st, org, &sub).await?;
            usage.push(pages::UsageBlock {
                subscription_name: sub.name.clone().filter(|n| !n.is_empty()).unwrap_or_else(|| plan.name.clone()),
                from,
                to,
                currency: currency.code(),
                rows: rows.into_iter().map(|r| (r.metric_name, r.units, r.amount_cents)).collect(),
            });
        }
        subscriptions.push((sub, plan.name.clone()));
    }
    let wallets = st.db.wallets_for_customer(a.customer.id).await?;
    let can_manage_payment_method = matches!(
        rago_jobs::collection::resolve_provider(&st.db, org).await,
        Ok(Some(p)) if p.kind == "stripe"
    );
    let customer_name = a
        .customer
        .name
        .clone()
        .filter(|n| !n.trim().is_empty())
        .unwrap_or_else(|| a.customer.external_id.clone());
    Ok(pages::home(&pages::PortalView {
        token: &a.token,
        org_name: &org_name,
        customer_name: &customer_name,
        expires_at: a.expires_at,
        notice,
        invoices: &invoices,
        subscriptions: &subscriptions,
        usage: &usage,
        wallets: &wallets,
        can_manage_payment_method,
        scheduled: &scheduled,
    }))
}

/// The customer's own invoice, or `None`.
async fn own_invoice(st: &AppState, a: &Access, invoice_id: Uuid) -> ApiResult<Option<rago_core::models::Invoice>> {
    Ok(st
        .db
        .find_invoice(invoice_id)
        .await?
        .filter(|i| i.organization_id == a.claims.organization_id && i.customer_id == Some(a.customer.id)))
}

/// `GET /portal/:token/invoices/:invoice_id/download` — the invoice PDF.
pub async fn download_invoice(
    State(st): State<AppState>,
    Path((raw, invoice_id)): Path<(String, Uuid)>,
) -> Response {
    let a = match authorize(&st, &raw).await {
        Ok(Some(a)) => a,
        Ok(None) => return invalid(),
        Err(e) => return server_error(e),
    };
    let invoice = match own_invoice(&st, &a, invoice_id).await {
        Ok(Some(i)) if !matches!(i.status, rago_core::enums::InvoiceStatus::Draft) => i,
        Ok(_) => return harden((StatusCode::NOT_FOUND, "invoice not found").into_response()),
        Err(e) => return server_error(e),
    };
    let fees = match st.db.fees_for_invoice(invoice.id).await {
        Ok(f) => f,
        Err(e) => return server_error(e),
    };
    let name = a.customer.name.clone().unwrap_or_else(|| a.customer.external_id.clone());
    let pdf = rago_documents::invoice_pdf(&invoice, &fees, &name, &a.customer.external_id);
    let disposition = format!(
        "attachment; filename=\"{}.pdf\"",
        invoice.number.replace(['"', '\\', '\r', '\n'], "")
    );
    harden(([(header::CONTENT_TYPE, "application/pdf".to_string()), (header::CONTENT_DISPOSITION, disposition)], pdf).into_response())
}

/// Collect `invoice` now and send the customer where it leads.
async fn pay(st: &AppState, a: &Access, invoice: &rago_core::models::Invoice) -> Response {
    use rago_jobs::collection::{collect_invoice, CollectOutcome};
    if !pages::payable(invoice) {
        return back(&a.token, "already_paid");
    }
    // An open pay link (3-D Secure or a hosted checkout) is reused.
    match st.db.invoice_payment_action(invoice.id).await {
        Ok(Some(action)) => {
            if let Some(resp) = action.next_action_url.as_deref().and_then(away) {
                return resp;
            }
        }
        Ok(None) => {}
        Err(e) => return server_error(e),
    }
    let transport = rago_integrations::default_transport();
    let outcome = match collect_invoice(&st.db, &transport, invoice).await {
        Ok(o) => o,
        Err(e) => {
            tracing::warn!(error = %e, invoice_id = %invoice.id, "portal payment attempt failed");
            return back(&a.token, "payment_unavailable");
        }
    };
    if let Ok(Some(action)) = st.db.invoice_payment_action(invoice.id).await {
        if let Some(resp) = action.next_action_url.as_deref().and_then(away) {
            return resp;
        }
    }
    let notice = match outcome {
        CollectOutcome::Charged(rago_integrations::PayableStatus::Succeeded) => "invoice_paid",
        CollectOutcome::Charged(rago_integrations::PayableStatus::Pending) => "payment_pending",
        CollectOutcome::Charged(rago_integrations::PayableStatus::Failed) => "payment_failed",
        CollectOutcome::Skipped(_) => "payment_unavailable",
    };
    back(&a.token, notice)
}

/// `POST /portal/:token/invoices/:invoice_id/pay`.
pub async fn pay_invoice(
    State(st): State<AppState>,
    Path((raw, invoice_id)): Path<(String, Uuid)>,
) -> Response {
    let a = match authorize(&st, &raw).await {
        Ok(Some(a)) => a,
        Ok(None) => return invalid(),
        Err(e) => return server_error(e),
    };
    match own_invoice(&st, &a, invoice_id).await {
        Ok(Some(invoice)) => pay(&st, &a, &invoice).await,
        Ok(None) => harden((StatusCode::NOT_FOUND, "invoice not found").into_response()),
        Err(e) => server_error(e),
    }
}

/// Parse a positive credit amount from the top-up form.
fn parse_credits(body: &[u8]) -> Option<Decimal> {
    let form = crate::checkout_webhooks::parse_form(std::str::from_utf8(body).ok()?);
    let raw = form.iter().find(|(k, _)| k == "credits").map(|(_, v)| v.trim().replace(',', ""))?;
    let credits: Decimal = raw.parse().ok()?;
    (credits > Decimal::ZERO && credits <= Decimal::from(MAX_TOP_UP_CREDITS) && credits.scale() <= 5)
        .then_some(credits)
}

/// `POST /portal/:token/wallets/:wallet_id/top_up` (form `credits`).
pub async fn top_up(
    State(st): State<AppState>,
    Path((raw, wallet_id)): Path<(String, Uuid)>,
    body: Bytes,
) -> Response {
    let a = match authorize(&st, &raw).await {
        Ok(Some(a)) => a,
        Ok(None) => return invalid(),
        Err(e) => return server_error(e),
    };
    let Some(credits) = parse_credits(&body) else {
        return back(&a.token, "top_up_invalid");
    };
    let wallet = match st.db.find_wallet(wallet_id).await {
        Ok(Some(w))
            if w.customer_id == a.customer.id
                && w.organization_id == a.claims.organization_id
                && matches!(w.status, WalletStatus::Active) =>
        {
            w
        }
        Ok(_) => return back(&a.token, "wallet_unavailable"),
        Err(e) => return server_error(e),
    };
    let invoice = match rago_jobs::wallet_credits::purchase_paid_credits(&st.db, &wallet, credits).await {
        Ok((_tx, invoice)) => invoice,
        Err(e) => return server_error(e),
    };
    if invoice.total_amount_cents <= 0 {
        return back(&a.token, "top_up_created");
    }
    let resp = pay(&st, &a, &invoice).await;
    // A redirect back home after a pending top-up reads better as "created".
    if resp.status() == StatusCode::SEE_OTHER {
        let to_home = resp
            .headers()
            .get(header::LOCATION)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|l| l.contains("notice=payment_unavailable") || l.contains("notice=payment_pending"));
        if to_home {
            return back(&a.token, "top_up_created");
        }
    }
    resp
}

/// The customer's own live subscription (active, or pending that is not a
/// scheduled downgrade), or `None`.
async fn own_subscription(st: &AppState, a: &Access, subscription_id: Uuid) -> ApiResult<Option<rago_core::models::Subscription>> {
    let subs = st.db.subscriptions_for_customer(a.customer.id, 100, 0).await?;
    Ok(subs.into_iter().find(|s| {
        s.id == subscription_id
            && s.organization_id == a.claims.organization_id
            && (matches!(s.status, SubscriptionStatus::Active)
                || (matches!(s.status, SubscriptionStatus::Pending) && s.previous_subscription_id.is_none()))
    }))
}

fn plan_price(plan: &rago_core::models::Plan) -> String {
    let interval = plan.interval.map(|i| format!(" / {}", i.as_str())).unwrap_or_default();
    format!("{}{interval}", pages::money(plan.amount_cents.unwrap_or(0), &plan.amount_currency))
}

/// `GET /portal/:token/subscriptions/:subscription_id/cancel` — the change or
/// cancel page: cheaper plans to switch to at period end, and a cancellation
/// that must be explicitly confirmed.
pub async fn manage_subscription(
    State(st): State<AppState>,
    Path((raw, subscription_id)): Path<(String, Uuid)>,
    Query(q): Query<PageQuery>,
) -> Response {
    let a = match authorize(&st, &raw).await {
        Ok(Some(a)) => a,
        Ok(None) => return invalid(),
        Err(e) => return server_error(e),
    };
    match render_manage(&st, &a, subscription_id, q.notice.as_deref()).await {
        Ok(Some(html)) => harden((StatusCode::OK, Html(html)).into_response()),
        Ok(None) => back(&a.token, "subscription_unavailable"),
        Err(e) => server_error(e),
    }
}

async fn render_manage(st: &AppState, a: &Access, subscription_id: Uuid, notice: Option<&str>) -> ApiResult<Option<String>> {
    let Some(sub) = own_subscription(st, a, subscription_id).await? else { return Ok(None) };
    let org = a.claims.organization_id;
    let plan = st.db.find_plan(sub.plan_id).await?;
    let active = matches!(sub.status, SubscriptionStatus::Active);
    let options = if active {
        crate::plan_change::downgrade_options(st, org, &plan)
            .await?
            .into_iter()
            .map(|p| pages::DowngradeOption { plan_id: p.id, price: plan_price(&p), name: p.name })
            .collect()
    } else {
        Vec::new()
    };
    let scheduled = match st.db.scheduled_downgrade_for(sub.id).await? {
        Some(next) => {
            let next_plan = st.db.find_plan(next.plan_id).await?;
            next.subscription_at.map(|at| (next_plan.name, at.date_naive()))
        }
        None => None,
    };
    let org_name = st.db.organization_name(org).await?.unwrap_or_else(|| "Billing".into());
    let customer_name = a
        .customer
        .name
        .clone()
        .filter(|n| !n.trim().is_empty())
        .unwrap_or_else(|| a.customer.external_id.clone());
    let sub_name = sub.name.clone().filter(|n| !n.is_empty()).unwrap_or_else(|| plan.name.clone());
    Ok(Some(pages::manage_subscription(&pages::ManageView {
        token: &a.token,
        org_name: &org_name,
        customer_name: &customer_name,
        expires_at: a.expires_at,
        notice,
        subscription_id: sub.id,
        subscription_name: &sub_name,
        plan_price: plan_price(&plan),
        active,
        period_end: active.then(|| crate::plan_change::effective_at(&sub, &plan).date_naive()),
        scheduled,
        options: &options,
    })))
}

fn form_value(body: &[u8], key: &str) -> Option<String> {
    let form = crate::checkout_webhooks::parse_form(std::str::from_utf8(body).ok()?);
    form.into_iter().find(|(k, _)| k == key).map(|(_, v)| v)
}

/// 303 to the change-or-cancel page with a notice code.
fn back_to_manage(token: &str, subscription_id: Uuid, notice: &str) -> Response {
    let target = format!("/portal/{token}/subscriptions/{subscription_id}/cancel?notice={notice}");
    harden((StatusCode::SEE_OTHER, [(header::LOCATION, target)]).into_response())
}

/// `POST /portal/:token/subscriptions/:subscription_id/cancel` (form
/// `confirm=yes`). Without the confirmation the customer is sent back to the
/// confirmation page and nothing changes.
pub async fn cancel_subscription(
    State(st): State<AppState>,
    Path((raw, subscription_id)): Path<(String, Uuid)>,
    body: Bytes,
) -> Response {
    let a = match authorize(&st, &raw).await {
        Ok(Some(a)) => a,
        Ok(None) => return invalid(),
        Err(e) => return server_error(e),
    };
    let sub = match own_subscription(&st, &a, subscription_id).await {
        Ok(Some(s)) => s,
        Ok(None) => return back(&a.token, "subscription_unavailable"),
        Err(e) => return server_error(e),
    };
    if form_value(&body, "confirm").as_deref() != Some("yes") {
        return back_to_manage(&a.token, sub.id, "cancel_not_confirmed");
    }
    match crate::handlers::terminate_subscription_now(&st, a.claims.organization_id, sub).await {
        Ok(_) => back(&a.token, "subscription_cancelled"),
        Err(e) => server_error(e),
    }
}

/// `POST /portal/:token/subscriptions/:subscription_id/downgrade` (form
/// `plan_id`) — schedule a switch to a cheaper plan at the end of the period.
pub async fn downgrade_subscription(
    State(st): State<AppState>,
    Path((raw, subscription_id)): Path<(String, Uuid)>,
    body: Bytes,
) -> Response {
    let a = match authorize(&st, &raw).await {
        Ok(Some(a)) => a,
        Ok(None) => return invalid(),
        Err(e) => return server_error(e),
    };
    let org = a.claims.organization_id;
    let sub = match own_subscription(&st, &a, subscription_id).await {
        Ok(Some(s)) if matches!(s.status, SubscriptionStatus::Active) => s,
        Ok(_) => return back(&a.token, "subscription_unavailable"),
        Err(e) => return server_error(e),
    };
    let Some(plan_id) = form_value(&body, "plan_id").and_then(|v| Uuid::parse_str(v.trim()).ok()) else {
        return back_to_manage(&a.token, sub.id, "downgrade_unavailable");
    };
    let current = match st.db.find_plan(sub.plan_id).await {
        Ok(p) => p,
        Err(e) => return server_error(e),
    };
    // Only plans offered on the page are accepted.
    let target = match crate::plan_change::downgrade_options(&st, org, &current).await {
        Ok(options) => options.into_iter().find(|p| p.id == plan_id),
        Err(e) => return server_error(e),
    };
    let Some(target) = target else {
        return back_to_manage(&a.token, sub.id, "downgrade_unavailable");
    };
    match crate::plan_change::schedule(&st, org, &sub, &target).await {
        Ok(_) => back(&a.token, "downgrade_scheduled"),
        Err(ApiError::Validation(_)) => back_to_manage(&a.token, sub.id, "downgrade_unavailable"),
        Err(e) => server_error(e),
    }
}

/// `POST /portal/:token/subscriptions/:subscription_id/keep_plan` — cancel a
/// scheduled downgrade.
pub async fn keep_plan(
    State(st): State<AppState>,
    Path((raw, subscription_id)): Path<(String, Uuid)>,
) -> Response {
    let a = match authorize(&st, &raw).await {
        Ok(Some(a)) => a,
        Ok(None) => return invalid(),
        Err(e) => return server_error(e),
    };
    let sub = match own_subscription(&st, &a, subscription_id).await {
        Ok(Some(s)) => s,
        Ok(None) => return back(&a.token, "subscription_unavailable"),
        Err(e) => return server_error(e),
    };
    match crate::plan_change::cancel(&st, a.claims.organization_id, &sub).await {
        Ok(_) => back(&a.token, "downgrade_kept"),
        Err(e) => server_error(e),
    }
}

/// `POST /portal/:token/payment_method` — Stripe's hosted payment-method page
/// (only when Stripe is the organization's provider).
pub async fn payment_method(State(st): State<AppState>, Path(raw): Path<String>) -> Response {
    let a = match authorize(&st, &raw).await {
        Ok(Some(a)) => a,
        Ok(None) => return invalid(),
        Err(e) => return server_error(e),
    };
    let transport = rago_integrations::default_transport();
    match rago_jobs::collection::portal_url(&st.db, &transport, &a.customer).await {
        Ok(Ok(url)) => away(&url).unwrap_or_else(|| back(&a.token, "payment_method_unavailable")),
        Ok(Err(_)) => back(&a.token, "payment_method_unavailable"),
        Err(e) => {
            tracing::warn!(error = %e, "stripe portal session failed");
            back(&a.token, "payment_method_unavailable")
        }
    }
}

// ---- operator API ---------------------------------------------------------------

async fn customer_or_404(st: &AppState, org: Uuid, external_id: &str) -> ApiResult<Customer> {
    st.db
        .find_customer_by_external_id(org, external_id)
        .await?
        .ok_or_else(|| ApiError::NotFound("customer".into()))
}

/// `POST /customers/:external_id/portal_sessions` — issue a portal link.
/// Body (optional): `{ "portal_session": { "expires_in_minutes": 1440 } }`.
pub async fn create_session(
    State(st): State<AppState>,
    auth: Authed,
    Path(external_id): Path<String>,
    body: Option<Json<Value>>,
) -> ApiResult<Json<Value>> {
    let customer = customer_or_404(&st, auth.organization.id, &external_id).await?;
    let ttl = body
        .as_ref()
        .and_then(|Json(v)| {
            v.get("portal_session")
                .unwrap_or(v)
                .get("expires_in_minutes")
                .and_then(Value::as_i64)
        })
        .unwrap_or(DEFAULT_TTL_MINUTES);
    let issued = issue(&st, &customer, ttl).await?;
    Ok(Json(json!({
        "portal_session": {
            "lago_id": issued.session_id,
            "external_customer_id": customer.external_id,
            "portal_url": issued.url,
            "token": issued.token,
            "expires_at": issued.expires_at.to_rfc3339(),
        }
    })))
}

/// `DELETE /customers/:external_id/portal_sessions` — revoke every open link.
pub async fn revoke_sessions(
    State(st): State<AppState>,
    auth: Authed,
    Path(external_id): Path<String>,
) -> ApiResult<Json<Value>> {
    let customer = customer_or_404(&st, auth.organization.id, &external_id).await?;
    let revoked = st.db.revoke_portal_sessions(auth.organization.id, customer.id).await?;
    Ok(Json(json!({ "portal_sessions": { "external_customer_id": customer.external_id, "revoked": revoked } })))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn top_up_amounts_are_validated() {
        assert_eq!(parse_credits(b"credits=12.5"), Some(Decimal::new(125, 1)));
        assert_eq!(parse_credits(b"credits=1%2C000"), Some(Decimal::from(1000)));
        assert_eq!(parse_credits(b"credits=0"), None);
        assert_eq!(parse_credits(b"credits=-3"), None);
        assert_eq!(parse_credits(b"credits=2000000"), None);
        assert_eq!(parse_credits(b"credits=abc"), None);
        assert_eq!(parse_credits(b"other=1"), None);
    }

    #[test]
    fn redirects_leave_only_to_https() {
        assert!(away("https://checkout.test/x").is_some());
        assert!(away("http://checkout.test/x").is_none());
        assert!(away("javascript:alert(1)").is_none());
    }
}
