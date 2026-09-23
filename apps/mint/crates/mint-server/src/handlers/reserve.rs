//! Proof-of-reserves (§6) + ledger-side reconciliation.

use axum::extract::State;
use mint_store::{
    provider_tag_of, LedgerStore, PayoutQuery, PayoutRecordStatus, ReserveMovementTotal,
    ReversalStatus,
};
use serde_json::{json, Value};
use std::collections::BTreeMap;

use crate::error::{ok, ApiResult};
use crate::state::AppState;

/// `GET /v1/reserve` → `ReserveLedger::solvency`. A storage outage is a 5xx,
/// never a vacuous `supply == backed == 0 ⇒ ok`.
pub async fn get<S: LedgerStore + 'static>(State(st): State<AppState<S>>) -> ApiResult {
    let proof = st.reserve.solvency().await?;
    let cushion = proof
        .backed_micros
        .get()
        .saturating_sub(proof.supply_micros.get());
    Ok(ok(json!({
        "supply_micros": proof.supply_micros.get(),
        "backing_micros": proof.backed_micros.get(),
        "cushion": cushion,
        "ok": proof.ok,
    })))
}

/// Upper bound of payout rows scanned per status for reconciliation totals.
const RECON_PAYOUT_SCAN: usize = 100_000;

#[derive(Default)]
struct ProviderRecon {
    minted_count: u64,
    minted_micros: i64,
    burned_count: u64,
    burned_micros: i64,
    paid_micros: i64,
    pending_micros: i64,
    failed_micros: i64,
    failed_count: u64,
    pending_count: u64,
    cancelled_micros: i64,
    cancelled_count: u64,
    reversed_micros: i64,
    clawed_back_micros: i64,
    reversal_shortfall_micros: i64,
}

/// `GET /v1/reconciliation` → per-provider reconciliation of the ledger's
/// reserve movements (deposits minted, burns for cash-out) against the durable
/// payout outcomes, plus the solvency proof. `burned_unpaid_micros` is money
/// burned whose payout is not (yet) confirmed paid — the operator worklist.
pub async fn reconciliation<S: LedgerStore + 'static>(
    State(st): State<AppState<S>>,
) -> ApiResult {
    let store = st.ledger.store();
    let proof = st.reserve.solvency().await?;
    let movements: Vec<ReserveMovementTotal> = store.reserve_movement_totals().await?;

    let mut by_provider: BTreeMap<String, ProviderRecon> = BTreeMap::new();
    for m in &movements {
        let slot = by_provider.entry(m.provider.clone()).or_default();
        match m.kind.as_str() {
            "mint" => {
                slot.minted_count += m.count;
                slot.minted_micros = slot.minted_micros.saturating_add(m.total_micros);
            }
            _ => {
                slot.burned_count += m.count;
                slot.burned_micros = slot.burned_micros.saturating_add(m.total_micros);
            }
        }
    }
    for status in [
        PayoutRecordStatus::Paid,
        PayoutRecordStatus::Pending,
        PayoutRecordStatus::Failed,
        PayoutRecordStatus::Cancelled,
    ] {
        let rows = store
            .list_payouts(PayoutQuery {
                status: Some(status),
                account_id: None,
                before: None,
                limit: RECON_PAYOUT_SCAN,
            })
            .await?;
        for r in rows {
            let slot = by_provider.entry(provider_tag_of(&r.payout_ref)).or_default();
            let amt = r.amount_micros.get();
            match status {
                PayoutRecordStatus::Paid => slot.paid_micros = slot.paid_micros.saturating_add(amt),
                PayoutRecordStatus::Pending => {
                    slot.pending_micros = slot.pending_micros.saturating_add(amt);
                    slot.pending_count += 1;
                }
                PayoutRecordStatus::Failed => {
                    slot.failed_micros = slot.failed_micros.saturating_add(amt);
                    slot.failed_count += 1;
                }
                PayoutRecordStatus::Cancelled => {
                    slot.cancelled_micros = slot.cancelled_micros.saturating_add(amt);
                    slot.cancelled_count += 1;
                }
            }
        }
    }
    // Deposit reversals (refunds / chargebacks): the claw-back burns are
    // redeem rows in the journal, so they are netted out of the payout math.
    let reversals = store
        .list_deposit_reversals(None, RECON_PAYOUT_SCAN)
        .await?;
    for r in &reversals {
        let slot = by_provider.entry(provider_tag_of(&r.deposit_ref)).or_default();
        slot.reversed_micros = slot.reversed_micros.saturating_add(r.amount_micros);
        slot.clawed_back_micros = slot.clawed_back_micros.saturating_add(r.clawed_micros);
        if r.status == ReversalStatus::Shortfall {
            slot.reversal_shortfall_micros = slot
                .reversal_shortfall_micros
                .saturating_add(r.amount_micros.saturating_sub(r.clawed_micros));
        }
    }

    let active: Vec<&'static str> = st
        .payments
        .as_ref()
        .map(|p| p.active_providers().into_iter().map(|p| p.tag()).collect())
        .unwrap_or_default();
    let providers: Vec<Value> = by_provider
        .iter()
        .map(|(tag, r)| {
            json!({
                "provider": tag,
                "active": active.contains(&tag.as_str()),
                // Cancelled payouts are re-credited with a mint and claw-backs
                // are burns; both are netted out so these figures describe
                // real customer deposits and real cash-outs.
                "deposits_minted": {
                    "count": r.minted_count.saturating_sub(r.cancelled_count),
                    "micros": r.minted_micros.saturating_sub(r.cancelled_micros),
                },
                "burned_for_payout": {
                    "count": r.burned_count,
                    "micros": r.burned_micros.saturating_sub(r.clawed_back_micros),
                },
                "payouts": {
                    "paid_micros": r.paid_micros,
                    "pending_micros": r.pending_micros,
                    "pending_count": r.pending_count,
                    "failed_micros": r.failed_micros,
                    "failed_count": r.failed_count,
                    "cancelled_micros": r.cancelled_micros,
                    "cancelled_count": r.cancelled_count,
                },
                "reversals": {
                    "reversed_micros": r.reversed_micros,
                    "clawed_back_micros": r.clawed_back_micros,
                    "shortfall_micros": r.reversal_shortfall_micros,
                },
                "burned_unpaid_micros": r
                    .burned_micros
                    .saturating_sub(r.clawed_back_micros)
                    .saturating_sub(r.paid_micros)
                    .saturating_sub(r.cancelled_micros),
            })
        })
        .collect();
    let totals = by_provider.values().fold((0i64, 0i64, 0i64, 0i64), |acc, r| {
        (
            acc.0.saturating_add(r.minted_micros),
            acc.1.saturating_add(r.burned_micros),
            acc.2.saturating_add(r.paid_micros),
            acc.3
                .saturating_add(r.cancelled_micros)
                .saturating_add(r.clawed_back_micros),
        )
    });
    let shortfall: i64 = by_provider
        .values()
        .map(|r| r.reversal_shortfall_micros)
        .sum();
    Ok(ok(json!({
        "solvency": {
            "supply_micros": proof.supply_micros.get(),
            "backing_micros": proof.backed_micros.get(),
            "ok": proof.ok,
        },
        "totals": {
            "minted_micros": totals.0,
            "burned_micros": totals.1,
            "paid_out_micros": totals.2,
            "burned_unpaid_micros": totals.1.saturating_sub(totals.2).saturating_sub(totals.3),
            "reversal_shortfall_micros": shortfall,
            // Supply identity: everything minted minus everything burned is
            // exactly the outstanding reserve.
            "expected_supply_micros": totals.0.saturating_sub(totals.1),
            "supply_matches_movements":
                totals.0.saturating_sub(totals.1) == proof.supply_micros.get(),
        },
        "active_providers": active,
        "providers": providers,
    })))
}
