//! Account listing + period statements (opening balance, activity, closing
//! balance) backed by bounded per-account receipt queries.

use axum::extract::{Path, Query, State};
use mint_core::AccountId;
use mint_receipts::{Receipt, ReceiptOp};
use mint_store::{LedgerStore, ReceiptQuery};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::dto::parse_account;
use crate::error::{ok, ApiError, ApiResult};
use crate::state::AppState;

const DEFAULT_ACCOUNT_PAGE: usize = 50;
const MAX_ACCOUNT_PAGE: usize = 500;
/// A statement period may hold at most this many entries; larger periods must
/// be split (the response says so instead of silently truncating).
const MAX_STATEMENT_ENTRIES: usize = 10_000;
/// Default statement window: the trailing 30 days.
const DEFAULT_PERIOD_MS: i64 = 30 * 24 * 60 * 60 * 1000;

/// Query parameters for `GET /v1/accounts`.
#[derive(Debug, Default, Deserialize)]
pub struct AccountListParams {
    /// Exclusive keyset cursor (the last `account_id` of the previous page).
    pub after: Option<String>,
    /// Page size (default 50, max 500).
    pub limit: Option<usize>,
}

/// `GET /v1/accounts` → accounts with live balances, ordered by id.
pub async fn list<S: LedgerStore + 'static>(
    State(st): State<AppState<S>>,
    Query(p): Query<AccountListParams>,
) -> ApiResult {
    let after: Option<AccountId> = match p.after.as_deref().filter(|s| !s.is_empty()) {
        Some(raw) => Some(parse_account(raw).map_err(ApiError::from)?),
        None => None,
    };
    let limit = p
        .limit
        .unwrap_or(DEFAULT_ACCOUNT_PAGE)
        .clamp(1, MAX_ACCOUNT_PAGE);
    let rows = st.ledger.store().list_accounts(after, limit).await?;
    let next_cursor = (rows.len() == limit)
        .then(|| rows.last().map(|r| r.balance.account_id.as_str().to_string()))
        .flatten();
    let items: Vec<Value> = rows
        .iter()
        .map(|r| {
            json!({
                "account_id": r.balance.account_id.as_str(),
                "kind": r.balance.account_id.kind(),
                "owner_ref": r.owner_ref,
                "created_ms": r.created_ms,
                "available_micros": r.balance.available_micros.get(),
                "held_micros": r.balance.held_micros.get(),
                "updated_seq": r.balance.updated_seq,
            })
        })
        .collect();
    Ok(ok(json!({ "items": items, "next_cursor": next_cursor })))
}

/// Query parameters for `GET /v1/accounts/:id/statement`.
#[derive(Debug, Default, Deserialize)]
pub struct StatementParams {
    /// Period start (inclusive, unix ms). Default: `to_ms - 30 days`.
    pub from_ms: Option<i64>,
    /// Period end (exclusive, unix ms). Default: now.
    pub to_ms: Option<i64>,
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn op_name(op: &ReceiptOp) -> String {
    serde_json::to_value(op)
        .ok()
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_else(|| format!("{op:?}").to_lowercase())
}

/// Signed change of `available + held` carried by a receipt, derived from the
/// running balance so every op kind (transfer, hold, stream, mint, redeem) is
/// accounted for uniformly.
fn total_after(r: &Receipt) -> i64 {
    r.body
        .balance_after_available_micros
        .get()
        .saturating_add(r.body.balance_after_held_micros.get())
}

/// `GET /v1/accounts/:id/statement?from_ms=&to_ms=` → opening balance, every
/// entry in the period (with signed delta), totals and closing balance.
pub async fn statement<S: LedgerStore + 'static>(
    State(st): State<AppState<S>>,
    Path(id): Path<String>,
    Query(p): Query<StatementParams>,
) -> ApiResult {
    let account = parse_account(&id).map_err(ApiError::from)?;
    st.ledger.read_balance(&account).await?;
    let to_ms = p.to_ms.unwrap_or_else(now_ms);
    let from_ms = p.from_ms.unwrap_or(to_ms.saturating_sub(DEFAULT_PERIOD_MS));
    if from_ms >= to_ms {
        return Err(ApiError::BadRequest("from_ms must be before to_ms".into()));
    }
    let store = st.ledger.store();
    let opening = store.account_receipt_before(&account, from_ms).await?;
    let entries = store
        .query_account_receipts(
            &account,
            ReceiptQuery {
                after_seq: None,
                from_ms: Some(from_ms),
                to_ms: Some(to_ms),
                limit: MAX_STATEMENT_ENTRIES + 1,
            },
        )
        .await?;
    if entries.len() > MAX_STATEMENT_ENTRIES {
        return Err(ApiError::BadRequest(format!(
            "period holds more than {MAX_STATEMENT_ENTRIES} entries; request a shorter range"
        )));
    }

    let (open_avail, open_held) = opening
        .as_ref()
        .map(|r| {
            (
                r.body.balance_after_available_micros.get(),
                r.body.balance_after_held_micros.get(),
            )
        })
        .unwrap_or((0, 0));
    let mut prev_total = open_avail.saturating_add(open_held);
    let mut credits: i64 = 0;
    let mut debits: i64 = 0;
    let mut lines = Vec::with_capacity(entries.len());
    for r in &entries {
        let after = total_after(r);
        let delta = after.saturating_sub(prev_total);
        if delta >= 0 {
            credits = credits.saturating_add(delta);
        } else {
            debits = debits.saturating_add(delta.saturating_abs());
        }
        prev_total = after;
        lines.push(json!({
            "account_seq": r.body.account_seq,
            "journal_seq": r.body.journal_seq,
            "ts": r.body.ts,
            "op": op_name(&r.body.op),
            "amount_micros": r.body.amount_micros.get(),
            "delta_micros": delta,
            "from": r.body.from.as_ref().map(|a| a.as_str().to_string()),
            "to": r.body.to.as_ref().map(|a| a.as_str().to_string()),
            "available_after_micros": r.body.balance_after_available_micros.get(),
            "held_after_micros": r.body.balance_after_held_micros.get(),
            "receipt_hash": r.hash.to_hex(),
        }));
    }
    let (close_avail, close_held) = entries
        .last()
        .map(|r| {
            (
                r.body.balance_after_available_micros.get(),
                r.body.balance_after_held_micros.get(),
            )
        })
        .unwrap_or((open_avail, open_held));

    Ok(ok(json!({
        "account_id": account.as_str(),
        "from_ms": from_ms,
        "to_ms": to_ms,
        "opening": { "available_micros": open_avail, "held_micros": open_held },
        "closing": { "available_micros": close_avail, "held_micros": close_held },
        "totals": {
            "credits_micros": credits,
            "debits_micros": debits,
            "net_micros": credits.saturating_sub(debits),
            "entries": lines.len(),
        },
        "entries": lines,
    })))
}
