//! Deposit reversals: a provider refund or chargeback takes back money that
//! Mint already minted. The minted supply must be clawed back from the
//! credited account or the solvency proof goes false with nobody noticing.
//!
//! A reversal burns (a redeem-style journal row that decrements the reserve)
//! whatever the account still holds, up to the reversed amount. If the account
//! already spent the funds, the remainder is recorded as a `shortfall` — an
//! operator worklist item that can be retried later (after the account is
//! re-funded) or written off.
//!
//! Claw-backs deliberately bypass the account freeze guard: freezing an
//! account under a chargeback must never block recovering the funds.

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::Json;
use mint_core::{op_hash, Micros, MintError, TransferAmount};
use mint_store::{provider_tag_of, DepositReversal, LedgerStore, RedeemCommit, ReversalStatus};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::dto::parse_account;
use crate::error::{ok, ok_status, ApiError, ApiResult};
use crate::state::AppState;

/// Upper bound of reserve movements scanned to find the original deposit.
const DEPOSIT_SCAN: usize = 500_000;
const DEFAULT_PAGE: usize = 100;
const MAX_PAGE: usize = 1_000;

/// Outcome of [`apply_reversal`].
pub struct ReversalOutcome {
    /// The persisted reversal.
    pub record: DepositReversal,
    /// Amount burned by THIS call.
    pub burned_now_micros: i64,
}

/// JSON projection of a reversal.
pub fn reversal_view(r: &DepositReversal) -> Value {
    json!({
        "deposit_ref": r.deposit_ref,
        "provider": provider_tag_of(&r.deposit_ref),
        "account_id": r.account_id,
        "amount_micros": r.amount_micros,
        "clawed_micros": r.clawed_micros,
        "outstanding_micros": r.amount_micros.saturating_sub(r.clawed_micros),
        "reason": r.reason,
        "status": r.status.as_str(),
        "attempts": r.attempts,
        "note": r.note,
        "created_ms": r.created_ms,
        "updated_ms": r.updated_ms,
    })
}

/// Record (or extend) a reversal of `deposit_ref` for a CUMULATIVE
/// `amount_micros` (defaults to the full minted amount) and burn as much of the
/// outstanding amount as the account still holds. Idempotent: repeating the
/// same cumulative amount burns nothing new once fully clawed back.
pub async fn apply_reversal<S: LedgerStore + 'static>(
    st: &AppState<S>,
    deposit_ref: &str,
    amount_micros: Option<i64>,
    reason: &str,
    note: Option<String>,
) -> Result<ReversalOutcome, ApiError> {
    let store = st.ledger.store();
    let provider = provider_tag_of(deposit_ref);
    let original = store
        .list_reserve_movements(&provider, None, None, DEPOSIT_SCAN)
        .await?
        .into_iter()
        .find(|m| m.kind == "mint" && m.reference == deposit_ref)
        .ok_or_else(|| {
            ApiError::NotFound(format!("no minted deposit with reference {deposit_ref}"))
        })?;
    let account_raw = original
        .account_id
        .clone()
        .ok_or_else(|| ApiError::Internal("minted deposit has no recorded account".into()))?;
    let account = parse_account(&account_raw).map_err(ApiError::from)?;

    let requested = amount_micros.unwrap_or(original.amount_micros);
    if requested <= 0 {
        return Err(ApiError::BadRequest(
            "amount_micros must be positive".into(),
        ));
    }
    if requested > original.amount_micros {
        return Err(ApiError::BadRequest(format!(
            "reversal of {requested} µ$ exceeds the minted deposit ({} µ$)",
            original.amount_micros
        )));
    }
    let existing = store.get_deposit_reversal(deposit_ref).await?;
    if let Some(r) = existing.as_ref() {
        if r.status == ReversalStatus::WrittenOff && requested <= r.amount_micros {
            return Ok(ReversalOutcome {
                record: r.clone(),
                burned_now_micros: 0,
            });
        }
    }
    let base = existing.unwrap_or_else(|| DepositReversal {
        deposit_ref: deposit_ref.to_string(),
        account_id: account_raw.clone(),
        amount_micros: requested,
        clawed_micros: 0,
        reason: reason.to_string(),
        status: ReversalStatus::Shortfall,
        attempts: 0,
        note: None,
        created_ms: 0,
        updated_ms: 0,
    });
    let target = base.amount_micros.max(requested);
    let outstanding = target.saturating_sub(base.clawed_micros);

    let mut burned_now = 0i64;
    let mut attempts = base.attempts;
    if outstanding > 0 {
        let available = store.get_balance(&account).await?.available_micros.get();
        let burn = outstanding.min(available.max(0));
        if burn > 0 {
            attempts = attempts.saturating_add(1);
            let amount = TransferAmount::new(Micros::new(burn)).map_err(ApiError::from)?;
            let payout_ref = format!("{deposit_ref}#reversal{attempts}");
            let hash = op_hash(&json!({
                "op": "deposit_reversal",
                "from": account.as_str(),
                "amount": burn,
                "deposit_ref": deposit_ref,
                "attempt": attempts,
            }));
            // Bypass the freeze guard (see module docs) via the inner store.
            let out = store
                .inner()
                .commit_redeem(RedeemCommit {
                    from: account.clone(),
                    amount,
                    payout_ref,
                    idem_key: format!("reversal:{deposit_ref}#{attempts}"),
                    op_hash: hash,
                })
                .await;
            match out {
                Ok(o) if !o.idempotent_replay => burned_now = burn,
                Ok(_) => {}
                // A concurrent spend drained the account between the balance
                // read and the burn: keep it as a shortfall.
                Err(MintError::InsufficientFunds) => {}
                Err(e) => return Err(e.into()),
            }
        }
    }
    let clawed = base.clawed_micros.saturating_add(burned_now);
    let status = if clawed >= target {
        ReversalStatus::ClawedBack
    } else {
        ReversalStatus::Shortfall
    };
    let note = note
        .map(|n| n.trim().to_string())
        .filter(|n| !n.is_empty())
        .or(base.note.clone());
    let record = store
        .upsert_deposit_reversal(DepositReversal {
            amount_micros: target,
            clawed_micros: clawed,
            reason: if base.reason.is_empty() {
                reason.to_string()
            } else {
                base.reason.clone()
            },
            status,
            attempts,
            note,
            ..base
        })
        .await?;
    tracing::info!(
        deposit = %record.deposit_ref,
        burned_now,
        status = record.status.as_str(),
        "deposit reversal applied"
    );
    Ok(ReversalOutcome {
        record,
        burned_now_micros: burned_now,
    })
}

/// `POST /v1/reversals` body.
#[derive(Debug, Deserialize)]
pub struct CreateReversalReq {
    /// The original `<provider>:<ref>` deposit reference.
    pub deposit_ref: String,
    /// Cumulative reversed amount; defaults to the whole deposit.
    #[serde(default)]
    pub amount_micros: Option<i64>,
    /// `refund` (default) or `dispute`.
    #[serde(default)]
    pub reason: Option<String>,
    #[serde(default)]
    pub note: Option<String>,
}

/// `POST /v1/reversals` → record a refund / chargeback and claw back.
pub async fn create<S: LedgerStore + 'static>(
    State(st): State<AppState<S>>,
    Json(body): Json<CreateReversalReq>,
) -> ApiResult {
    let deposit_ref = body.deposit_ref.trim().to_string();
    if !deposit_ref.contains(':') {
        return Err(ApiError::BadRequest(
            "deposit_ref must be the namespaced <provider>:<ref> of a minted deposit".into(),
        ));
    }
    let reason = match body.reason.as_deref().unwrap_or("refund") {
        r @ ("refund" | "dispute") => r.to_string(),
        other => {
            return Err(ApiError::BadRequest(format!(
                "reason '{other}' must be refund or dispute"
            )))
        }
    };
    let out = apply_reversal(&st, &deposit_ref, body.amount_micros, &reason, body.note).await?;
    let mut view = reversal_view(&out.record);
    view["burned_now_micros"] = json!(out.burned_now_micros);
    Ok(ok_status(StatusCode::CREATED, view))
}

/// `POST /v1/reversals/:deposit_ref/retry` → burn what the account now holds
/// toward an outstanding shortfall.
pub async fn retry<S: LedgerStore + 'static>(
    State(st): State<AppState<S>>,
    Path(deposit_ref): Path<String>,
) -> ApiResult {
    let rec = st
        .ledger
        .store()
        .get_deposit_reversal(&deposit_ref)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("no reversal for {deposit_ref}")))?;
    if rec.status != ReversalStatus::Shortfall {
        return Ok(ok(reversal_view(&rec)));
    }
    let reason = rec.reason.clone();
    let out = apply_reversal(&st, &deposit_ref, Some(rec.amount_micros), &reason, None).await?;
    let mut view = reversal_view(&out.record);
    view["burned_now_micros"] = json!(out.burned_now_micros);
    Ok(ok(view))
}

/// `POST /v1/reversals/:deposit_ref/write-off` body.
#[derive(Debug, Default, Deserialize)]
pub struct WriteOffReq {
    #[serde(default)]
    pub note: Option<String>,
}

/// `POST /v1/reversals/:deposit_ref/write-off` → accept an unrecoverable
/// shortfall as a loss (removes it from the worklist).
pub async fn write_off<S: LedgerStore + 'static>(
    State(st): State<AppState<S>>,
    Path(deposit_ref): Path<String>,
    body: Option<Json<WriteOffReq>>,
) -> ApiResult {
    let store = st.ledger.store();
    let rec = store
        .get_deposit_reversal(&deposit_ref)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("no reversal for {deposit_ref}")))?;
    if rec.status != ReversalStatus::Shortfall {
        return Err(ApiError::BadRequest(format!(
            "only a shortfall can be written off (this reversal is {})",
            rec.status.as_str()
        )));
    }
    let note = body
        .and_then(|Json(b)| b.note)
        .map(|n| n.trim().to_string())
        .filter(|n| !n.is_empty())
        .or(rec.note.clone());
    let updated = store
        .upsert_deposit_reversal(DepositReversal {
            status: ReversalStatus::WrittenOff,
            note,
            ..rec
        })
        .await?;
    Ok(ok(reversal_view(&updated)))
}

/// `GET /v1/reversals` query.
#[derive(Debug, Default, Deserialize)]
pub struct ListParams {
    /// `clawed_back` | `shortfall` | `written_off`.
    pub status: Option<String>,
    pub limit: Option<usize>,
}

/// `GET /v1/reversals` → reversals newest first.
pub async fn list<S: LedgerStore + 'static>(
    State(st): State<AppState<S>>,
    Query(p): Query<ListParams>,
) -> ApiResult {
    let status = match p.status.as_deref().filter(|s| !s.is_empty()) {
        Some(raw) => Some(ReversalStatus::parse(raw).ok_or_else(|| {
            ApiError::BadRequest(
                "status must be one of: clawed_back, shortfall, written_off".into(),
            )
        })?),
        None => None,
    };
    let limit = p.limit.unwrap_or(DEFAULT_PAGE).clamp(1, MAX_PAGE);
    let rows = st
        .ledger
        .store()
        .list_deposit_reversals(status, limit)
        .await?;
    Ok(ok(json!({
        "items": rows.iter().map(reversal_view).collect::<Vec<_>>(),
    })))
}
