//! Real-time (Mint) wallet console API: live Mint balance, the wallet's
//! holds, top-ups and the Mint ledger for the customer's account, plus the
//! operator actions (hosted top-up checkout, retry reversals of voided
//! invoices).
//!
//! Balances and the ledger are read from Mint on every request (nothing is
//! cached). When Mint is not configured or unreachable the response says so in
//! `mint.error` and still returns Rago's own hold records.

use axum::extract::{Path, Query, State};
use axum::Json;
use chrono::Utc;
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::auth::Authed;
use crate::error::{ApiError, ApiResult};
use crate::handlers::{i64_of, s, wrapped};
use crate::state::AppState;

/// Longest ledger window the console asks Mint for.
const MAX_LEDGER_DAYS: i64 = 90;
const DEFAULT_LEDGER_DAYS: i64 = 30;
/// Largest single top-up checkout, in cents (USD 100,000).
const MAX_TOP_UP_CENTS: i64 = 10_000_000;
/// Mint's minimum checkout (USD 1.00).
const MIN_TOP_UP_CENTS: i64 = 100;
const MICROS_PER_CENT: i64 = 10_000;

struct Scope {
    wallet: rago_core::models::Wallet,
    enabled: bool,
    policy: String,
    mint_account_id: Option<String>,
}

async fn scope(st: &AppState, org: Uuid, id: Uuid) -> ApiResult<Scope> {
    let wallet = st
        .db
        .find_wallet(id)
        .await?
        .filter(|w| w.organization_id == org)
        .ok_or_else(|| ApiError::NotFound("wallet".into()))?;
    let (enabled, policy, mint_account_id) = st
        .db
        .wallet_realtime_settings(wallet.id)
        .await?
        .ok_or_else(|| ApiError::NotFound("wallet".into()))?;
    Ok(Scope { wallet, enabled, policy, mint_account_id: mint_account_id.filter(|a| !a.is_empty()) })
}

fn client(config: &rago_jobs::wallet_realtime::MintConfig) -> rago_integrations::MintHoldsClient<rago_integrations::DefaultTransport> {
    rago_integrations::MintHoldsClient::new(config.url.clone(), config.api_key.clone(), rago_integrations::default_transport())
}

#[derive(Debug, Deserialize)]
pub struct OverviewQuery {
    pub days: Option<i64>,
}

/// Classify a Mint statement entry for the console: money in from a funding
/// deposit or an inbound transfer is a top-up.
fn is_top_up(entry: &Value, account: &str) -> bool {
    let op = entry["op"].as_str().unwrap_or("");
    let delta = entry["delta_micros"].as_i64().unwrap_or(0);
    delta > 0
        && (op.eq_ignore_ascii_case("mint")
            || (op.eq_ignore_ascii_case("transfer") && entry["to"].as_str() == Some(account)))
}

/// `GET /wallets/:id/realtime/overview?days=30`.
pub async fn overview(
    State(st): State<AppState>,
    auth: Authed,
    Path(id): Path<Uuid>,
    Query(q): Query<OverviewQuery>,
) -> ApiResult<Json<Value>> {
    let sc = scope(&st, auth.organization.id, id).await?;
    let totals = st.db.wallet_hold_totals(sc.wallet.id).await?;
    let days = q.days.unwrap_or(DEFAULT_LEDGER_DAYS).clamp(1, MAX_LEDGER_DAYS);
    let config = rago_jobs::wallet_realtime::mint_config();

    let mut mint = json!({ "configured": config.is_some(), "error": null });
    match (&config, &sc.mint_account_id) {
        (None, _) => mint["error"] = json!("Mint is not configured (RAGO_MINT_URL, RAGO_MINT_API_KEY, RAGO_MINT_PAYEE_ACCOUNT)."),
        (Some(_), None) => mint["error"] = json!("This wallet has no Mint account."),
        (Some(cfg), Some(account)) => {
            let c = client(cfg);
            match c.balance(account).await {
                Ok((available, held)) => {
                    mint["available_micros"] = json!(available);
                    mint["held_micros"] = json!(held);
                }
                Err(e) => mint["error"] = json!(format!("Mint balance unavailable: {e}")),
            }
            let to_ms = Utc::now().timestamp_millis();
            let from_ms = to_ms - days * 86_400_000;
            match c.statement(account, from_ms, to_ms).await {
                Ok(statement) => {
                    let entries = statement["entries"].as_array().cloned().unwrap_or_default();
                    let top_ups: Vec<Value> = entries.iter().filter(|e| is_top_up(e, account)).cloned().collect();
                    mint["statement"] = statement;
                    mint["top_ups"] = json!(top_ups);
                }
                Err(e) => {
                    if mint["error"].is_null() {
                        mint["error"] = json!(format!("Mint ledger unavailable: {e}"));
                    }
                }
            }
            if let Ok(providers) = c.providers().await {
                mint["providers"] = providers;
            }
        }
    }
    Ok(Json(json!({ "wallet_realtime_overview": {
        "lago_wallet_id": sc.wallet.id,
        "wallet_name": sc.wallet.name,
        "wallet_status": sc.wallet.status.as_str(),
        "currency": sc.wallet.balance_currency,
        "realtime_enabled": sc.enabled,
        "realtime_policy": sc.policy,
        "mint_account_id": sc.mint_account_id,
        "ledger_days": days,
        "holds": {
            "open_cents": totals.open_cents,
            "open_count": totals.open_count,
            "captured_micros": totals.captured_micros,
            "reversed_micros": totals.reversed_micros,
            "flagged_cents": totals.flagged_cents,
            "flagged_count": totals.flagged_count,
            "failed_reversals": totals.failed_reversals,
        },
        "mint": mint,
    }})))
}

#[derive(Debug, Deserialize)]
pub struct HoldsQuery {
    pub status: Option<String>,
    pub page: Option<i64>,
    pub per_page: Option<i64>,
}

/// `GET /wallets/:id/realtime/holds?status=open|captured|released|flagged|reversed`.
pub async fn holds(
    State(st): State<AppState>,
    auth: Authed,
    Path(id): Path<Uuid>,
    Query(q): Query<HoldsQuery>,
) -> ApiResult<Json<Value>> {
    let sc = scope(&st, auth.organization.id, id).await?;
    let status = q.status.as_deref().map(str::trim).filter(|s| !s.is_empty());
    if let Some(s) = status {
        if !matches!(s, "open" | "captured" | "released" | "flagged" | "reversed") {
            return Err(ApiError::validation("status", "value_is_invalid"));
        }
    }
    let page = crate::pagination::PageParams { page: q.page, per_page: q.per_page.map(|p| p.min(100)) };
    let (rows, total) = st
        .db
        .wallet_holds_page(sc.wallet.id, status, page.per_page(), page.offset())
        .await?;
    let items: Vec<Value> = rows
        .iter()
        .map(|r| {
            json!({
                "lago_id": r.hold.id,
                "event_transaction_id": r.hold.event_transaction_id,
                "event_timestamp": r.hold.event_timestamp.and_utc().to_rfc3339(),
                "mint_hold_id": r.hold.mint_hold_id,
                "status": r.hold.status,
                "amount_cents": r.hold.amount_cents,
                "amount_micros": r.hold.amount_micros,
                "captured_micros": r.hold.captured_micros,
                "reversed_micros": r.reversed_micros,
                "lago_invoice_id": r.hold.invoice_id,
                "invoice_number": r.invoice_number,
                "last_error": r.last_error,
                "created_at": r.created_at.and_utc().to_rfc3339(),
                "updated_at": r.updated_at.and_utc().to_rfc3339(),
            })
        })
        .collect();
    Ok(Json(json!({
        "holds": items,
        "meta": crate::pagination::meta(page.page(), page.per_page(), total),
    })))
}

/// `POST /wallets/:id/realtime/top_ups` with
/// `{ "top_up": { "amount_cents": 2500, "provider": "stripe" } }` — open a Mint
/// hosted checkout that funds the customer's Mint account once paid. The
/// operator sends the returned `checkout_url` to the customer.
pub async fn create_top_up(
    State(st): State<AppState>,
    auth: Authed,
    Path(id): Path<Uuid>,
    Json(body): Json<Value>,
) -> ApiResult<Json<Value>> {
    let sc = scope(&st, auth.organization.id, id).await?;
    let p = wrapped(&body, "top_up");
    let cents = i64_of(p, "amount_cents").ok_or_else(|| ApiError::validation("amount_cents", "value_is_mandatory"))?;
    if !(MIN_TOP_UP_CENTS..=MAX_TOP_UP_CENTS).contains(&cents) {
        return Err(ApiError::validation("amount_cents", "value_is_out_of_range"));
    }
    let provider = s(p, "provider").ok_or_else(|| ApiError::validation("provider", "value_is_mandatory"))?;
    let config = rago_jobs::wallet_realtime::mint_config()
        .ok_or_else(|| ApiError::validation("mint", "mint_not_configured"))?;
    let account = sc
        .mint_account_id
        .ok_or_else(|| ApiError::validation("mint_account_id", "value_is_mandatory"))?;
    let checkout = client(&config)
        .create_checkout(&provider, &account, cents * MICROS_PER_CENT)
        .await
        .map_err(|e| ApiError::validation("top_up", &format!("mint_refused: {e}")))?;
    Ok(Json(json!({ "top_up": checkout })))
}

/// `POST /wallets/:id/realtime/reversals` — retry moving captured money of
/// this wallet's voided invoices back to the customer.
pub async fn retry_reversals(
    State(st): State<AppState>,
    auth: Authed,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let sc = scope(&st, auth.organization.id, id).await?;
    let transport = rago_integrations::default_transport();
    let mut reversed = 0usize;
    let mut reversed_micros = 0i64;
    let mut failed = 0usize;
    for invoice_id in st.db.voided_invoices_with_captures(sc.wallet.id).await? {
        let r = rago_jobs::wallet_realtime::reverse_invoice_captures(&st.db, &transport, invoice_id)
            .await
            .map_err(|e| ApiError::Internal(e.to_string()))?;
        reversed += r.reversed;
        reversed_micros += r.reversed_micros;
        failed += r.failed;
    }
    Ok(Json(json!({ "reversals": { "reversed": reversed, "reversed_micros": reversed_micros, "failed": failed } })))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn top_ups_are_funding_or_inbound_transfers() {
        let acct = "acct_c";
        assert!(is_top_up(&json!({ "op": "Mint", "delta_micros": 5 }), acct));
        assert!(is_top_up(&json!({ "op": "Transfer", "delta_micros": 5, "to": acct }), acct));
        assert!(!is_top_up(&json!({ "op": "Transfer", "delta_micros": -5, "from": acct, "to": "x" }), acct));
        assert!(!is_top_up(&json!({ "op": "HoldRelease", "delta_micros": 0 }), acct));
        assert!(!is_top_up(&json!({ "op": "HoldCapture", "delta_micros": -5 }), acct));
    }
}
