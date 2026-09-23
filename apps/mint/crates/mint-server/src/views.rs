//! JSON projections for the domain receipt types (which are not themselves
//! `Serialize`). Amounts stay bare `i64` µ$.

use mint_ledger::{HoldReceipt, SpendReceipt, TransferReceipt};
use mint_reserve::{MintReceipt, Payout, RedeemReceipt};
use mint_stream::{StreamCloseReceipt, StreamOpenReceipt, StreamTickReceipt};
use serde_json::{json, Value};

fn balance(b: &mint_store::Balance) -> Value {
    serde_json::to_value(b).unwrap_or(Value::Null)
}

pub fn transfer_receipt(r: &TransferReceipt) -> Value {
    json!({
        "first_seq": r.first_seq,
        "from_balance": balance(&r.from_balance),
        "to_balance": balance(&r.to_balance),
        "idempotent_replay": r.idempotent_replay,
    })
}

pub fn hold_receipt(r: &HoldReceipt) -> Value {
    json!({
        "hold_id": r.hold_id.as_str(),
        "first_seq": r.first_seq,
        "holder_balance": balance(&r.holder_balance),
        "payee_balance": r.payee_balance.as_ref().map(balance),
        "status": r.status,
        "captured_micros": r.captured_micros.get(),
        "idempotent_replay": r.idempotent_replay,
    })
}

pub fn spend_receipt(r: &SpendReceipt) -> Value {
    json!({
        "allowance_id": r.allowance_id.as_str(),
        "remaining_micros": r.remaining_micros.get(),
        "settlement_seq": r.settlement_seq,
        "hold_id": r.hold_id.as_ref().map(|h| h.as_str()),
        "idempotent_replay": r.idempotent_replay,
    })
}

pub fn stream_open_receipt(r: &StreamOpenReceipt) -> Value {
    json!({
        "stream_id": r.stream_id.as_str(),
        "first_seq": r.first_seq,
        "holder_balance": balance(&r.holder_balance),
        "idempotent_replay": r.idempotent_replay,
    })
}

pub fn stream_tick_receipt(r: &StreamTickReceipt) -> Value {
    json!({
        "stream_id": r.stream_id.as_str(),
        "tick_seq": r.tick_seq,
        "settled_total_micros": r.settled_total_micros.get(),
        "from_balance": balance(&r.from_balance),
        "to_balance": balance(&r.to_balance),
        "idempotent_replay": r.idempotent_replay,
    })
}

pub fn stream_close_receipt(r: &StreamCloseReceipt) -> Value {
    json!({
        "stream_id": r.stream_id.as_str(),
        "settled_total_micros": r.settled_total_micros.get(),
        "released_micros": r.released_micros.get(),
        "from_balance": balance(&r.from_balance),
        "to_balance": balance(&r.to_balance),
        "idempotent_replay": r.idempotent_replay,
    })
}

fn payout(p: &Payout) -> Value {
    json!({
        "payout_ref": p.payout_ref,
        "amount_micros": p.amount_micros.get(),
        "status": format!("{:?}", p.status).to_lowercase(),
        "provider_ref": p.provider_ref,
    })
}

/// A persisted payout record (operator payout tracking).
pub fn payout_record(r: &mint_store::PayoutRecord) -> Value {
    json!({
        "payout_ref": r.payout_ref,
        "provider": mint_store::provider_tag_of(&r.payout_ref),
        "account_id": r.account_id.as_str(),
        "amount_micros": r.amount_micros.get(),
        "dest": r.dest,
        "status": r.status.as_str(),
        "provider_ref": r.provider_ref,
        "attempts": r.attempts,
        "last_error": r.last_error,
        "burn_seq": r.burn_seq,
        "created_ms": r.created_ms,
        "updated_ms": r.updated_ms,
    })
}

pub fn mint_receipt(r: &MintReceipt) -> Value {
    json!({
        "first_seq": r.first_seq,
        "to_balance": balance(&r.to_balance),
        "reserve_after_micros": r.reserve_after_micros.get(),
        "deposit_ref": r.deposit_ref,
        "idempotent_replay": r.idempotent_replay,
    })
}

pub fn redeem_receipt(r: &RedeemReceipt) -> Value {
    json!({
        "first_seq": r.first_seq,
        "from_balance": balance(&r.from_balance),
        "reserve_after_micros": r.reserve_after_micros.get(),
        "payout_ref": r.payout_ref,
        "payout": payout(&r.payout),
        "idempotent_replay": r.idempotent_replay,
    })
}
