//! The atomic seam helpers used by every `commit_*` method: `FOR UPDATE` balance
//! locks (taken in `AccountId` sort order → deadlock-free), the append-only
//! journal INSERT, the hash-linked receipt append, and the idempotency guard.
//!
//! All helpers run against a `&mut PgConnection` borrowed from an open
//! `Transaction`, so every mutation participates in ONE all-or-nothing commit.

use mint_core::{AccountId, Micros, MintError, OpHash};
use mint_receipts::{ReceiptDraft, ReceiptHash, Signer};
use mint_store::{AccountStatus, Balance, Bucket, OpKind};
use sqlx::{PgConnection, Row};

use crate::rows;

/// Map an infrastructure `sqlx` error onto the stable [`MintError`] surface.
///
/// Unexpected DB failures are traced and surfaced as [`MintError::Storage`] —
/// which the HTTP layer maps to `500`, exactly like `Overflow`, but keeps the
/// wire/log `code` honest: a connection drop or query failure is reported as
/// `storage`, not mislabelled as an arithmetic `overflow`. `RowNotFound` is
/// handled at each call site where a missing row is semantically meaningful
/// (account/hold/stream not found).
pub fn db_err(e: sqlx::Error) -> MintError {
    tracing::error!(error = %e, "mint-store-postgres: database error");
    MintError::Storage
}

/// The result of the idempotency guard.
pub enum IdemCheck {
    /// No prior op under this key — proceed to do the work.
    Fresh,
    /// A prior op under this key with the SAME body — replay it.
    Replay {
        /// First journal `seq` of the recorded op (`0` for control-plane ops).
        first_seq: i64,
        /// Minted hold/stream/allowance id, when the op minted one.
        result_ref: Option<String>,
    },
    /// A prior op under this key with a DIFFERENT body — 409 conflict.
    Conflict,
}

/// Look up any prior op recorded under `idem_key` and classify it.
pub async fn check_idem(
    conn: &mut PgConnection,
    idem_key: &str,
    op_hash: &OpHash,
) -> Result<IdemCheck, MintError> {
    let row =
        sqlx::query("SELECT op_hash, first_seq, result_ref FROM idempotency WHERE idem_key = $1")
            .bind(idem_key)
            .fetch_optional(&mut *conn)
            .await
            .map_err(db_err)?;
    match row {
        None => Ok(IdemCheck::Fresh),
        Some(r) => {
            let stored: Vec<u8> = r.get("op_hash");
            let first_seq: i64 = r.get("first_seq");
            let result_ref: Option<String> = r.get("result_ref");
            if rows::ophash_from_slice(&stored) == *op_hash {
                Ok(IdemCheck::Replay {
                    first_seq,
                    result_ref,
                })
            } else {
                Ok(IdemCheck::Conflict)
            }
        }
    }
}

/// Record the op under `idem_key` (the exactly-once seam). A concurrent
/// duplicate loses the `UNIQUE(idem_key)` race and surfaces here as a unique
/// violation, which the caller maps back to a replay.
pub async fn record_idem(
    conn: &mut PgConnection,
    idem_key: &str,
    op_hash: &OpHash,
    first_seq: i64,
    result_ref: Option<&str>,
    kind: &str,
    ts: i64,
) -> Result<(), MintError> {
    sqlx::query(
        "INSERT INTO idempotency (idem_key, op_hash, first_seq, result_ref, kind, created_at) \
         VALUES ($1, $2, $3, $4, $5, $6)",
    )
    .bind(idem_key)
    .bind(rows::ophash_to_vec(op_hash))
    .bind(first_seq)
    .bind(result_ref)
    .bind(kind)
    .bind(ts)
    .execute(&mut *conn)
    .await
    .map_err(db_err)?;
    Ok(())
}

/// `true` if the account row exists.
pub async fn account_exists(conn: &mut PgConnection, id: &AccountId) -> Result<bool, MintError> {
    let row = sqlx::query("SELECT 1 FROM accounts WHERE id = $1")
        .bind(id.as_str())
        .fetch_optional(&mut *conn)
        .await
        .map_err(db_err)?;
    Ok(row.is_some())
}

/// Read an account's lifecycle status INSIDE the caller's money transaction.
///
/// Must be called AFTER the account's balance row is locked `FOR UPDATE`:
/// `set_account_status` locks that same balance row before it flips the status
/// and commits both together, so once the caller holds the balance lock any
/// freeze/close is either already committed (and visible to this fresh
/// READ COMMITTED statement) or blocked until the caller commits. That makes
/// the check and the money write one atomic step with no race window, without
/// taking a second row lock (which could invert lock order against
/// `set_account_status`).
pub async fn account_status(
    conn: &mut PgConnection,
    id: &AccountId,
) -> Result<AccountStatus, MintError> {
    let row = sqlx::query("SELECT status FROM accounts WHERE id = $1")
        .bind(id.as_str())
        .fetch_optional(&mut *conn)
        .await
        .map_err(db_err)?
        .ok_or(MintError::AccountNotFound)?;
    Ok(AccountStatus::from_i16(row.get::<i16, _>("status")))
}

/// Refuse an OUTGOING movement from a frozen or closed account (see
/// [`account_status`] for the locking contract).
pub async fn ensure_can_send(conn: &mut PgConnection, id: &AccountId) -> Result<(), MintError> {
    match account_status(conn, id).await? {
        AccountStatus::Active => Ok(()),
        AccountStatus::Frozen => Err(MintError::AccountFrozen),
        AccountStatus::Closed => Err(MintError::AccountClosed),
    }
}

/// Refuse an INCOMING credit to a closed account; a frozen account still
/// receives so a customer's deposit is never bounced.
pub async fn ensure_can_receive(conn: &mut PgConnection, id: &AccountId) -> Result<(), MintError> {
    match account_status(conn, id).await? {
        AccountStatus::Closed => Err(MintError::AccountClosed),
        AccountStatus::Active | AccountStatus::Frozen => Ok(()),
    }
}

/// Lock and read an account's balance row (`SELECT ... FOR UPDATE`). Returns the
/// zeroed default (without a row) only when the account exists but has no balance
/// row yet; `AccountNotFound` when the account itself is unknown.
pub async fn lock_balance(conn: &mut PgConnection, id: &AccountId) -> Result<Balance, MintError> {
    let row = sqlx::query(
        "SELECT available_micros, held_micros, updated_seq FROM balances WHERE account_id = $1 FOR UPDATE",
    )
    .bind(id.as_str())
    .fetch_optional(&mut *conn)
    .await
    .map_err(db_err)?;
    match row {
        Some(r) => Ok(Balance {
            account_id: id.clone(),
            available_micros: Micros::new(r.get::<i64, _>("available_micros")),
            held_micros: Micros::new(r.get::<i64, _>("held_micros")),
            updated_seq: r.get::<i64, _>("updated_seq") as u64,
        }),
        None => {
            if account_exists(conn, id).await? {
                Ok(Balance::zeroed(id.clone()))
            } else {
                Err(MintError::AccountNotFound)
            }
        }
    }
}

/// Write an account's projected balance (upsert).
pub async fn write_balance(conn: &mut PgConnection, bal: &Balance) -> Result<(), MintError> {
    sqlx::query(
        "INSERT INTO balances (account_id, available_micros, held_micros, updated_seq) \
         VALUES ($1, $2, $3, $4) \
         ON CONFLICT (account_id) DO UPDATE SET \
             available_micros = EXCLUDED.available_micros, \
             held_micros = EXCLUDED.held_micros, \
             updated_seq = EXCLUDED.updated_seq",
    )
    .bind(bal.account_id.as_str())
    .bind(bal.available_micros.get())
    .bind(bal.held_micros.get())
    .bind(bal.updated_seq as i64)
    .execute(&mut *conn)
    .await
    .map_err(db_err)?;
    Ok(())
}

/// Append one append-only journal row, returning its assigned `seq`.
#[allow(clippy::too_many_arguments)]
pub async fn append_journal(
    conn: &mut PgConnection,
    op: OpKind,
    op_hash: &OpHash,
    idem_key: &str,
    account_id: &AccountId,
    counter_id: Option<&AccountId>,
    delta: Micros,
    bucket: Bucket,
    ref_: Option<&str>,
    ts: i64,
) -> Result<i64, MintError> {
    let row = sqlx::query(
        "INSERT INTO journal (op, op_hash, idem_key, account_id, counter_id, delta_micros, bucket, ref, ts) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9) RETURNING seq",
    )
    .bind(rows::op_to_text(op))
    .bind(rows::ophash_to_vec(op_hash))
    .bind(idem_key)
    .bind(account_id.as_str())
    .bind(counter_id.map(|c| c.as_str()))
    .bind(delta.get())
    .bind(rows::bucket_to_i16(bucket))
    .bind(ref_)
    .bind(ts)
    .fetch_one(&mut *conn)
    .await
    .map_err(db_err)?;
    Ok(row.get::<i64, _>("seq"))
}

/// Append one hash-linked, signed receipt derived from `draft`, deriving
/// `account_seq` + `prev_hash` from the account's current chain head. Returns the
/// new chain hash.
pub async fn append_receipt(
    conn: &mut PgConnection,
    signer: &dyn Signer,
    draft: ReceiptDraft,
) -> Result<ReceiptHash, MintError> {
    // Head of this account's chain (locked implicitly by the surrounding tx).
    let head = sqlx::query(
        "SELECT seq, hash FROM receipts WHERE account_id = $1 ORDER BY seq DESC LIMIT 1",
    )
    .bind(draft.account_id.as_str())
    .fetch_optional(&mut *conn)
    .await
    .map_err(db_err)?;

    let (account_seq, prev_hash) = match head {
        Some(r) => {
            let seq: i64 = r.get("seq");
            let hash: Vec<u8> = r.get("hash");
            ((seq + 1) as u64, rows::hash_from_slice(&hash))
        }
        None => (0u64, ReceiptHash::ZERO),
    };

    let body = mint_receipts::ReceiptBody {
        account_id: draft.account_id.clone(),
        account_seq,
        journal_seq: draft.journal_seq,
        op: draft.op,
        op_hash: draft.op_hash,
        amount_micros: draft.amount_micros,
        from: draft.from.clone(),
        to: draft.to.clone(),
        balance_after_available_micros: draft.balance_after_available_micros,
        balance_after_held_micros: draft.balance_after_held_micros,
        ts: draft.ts,
        prev_hash,
        signer: signer.key_id(),
    };
    let hash = body.compute_hash();
    let sig = signer.sign(hash.as_bytes());

    sqlx::query(
        "INSERT INTO receipts \
         (account_id, seq, journal_seq, op, op_hash, amount_micros, from_id, to_id, \
          balance_after_micros, balance_after_held, ts, prev_hash, hash, sig, signer) \
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15)",
    )
    .bind(draft.account_id.as_str())
    .bind(account_seq as i64)
    .bind(draft.journal_seq as i64)
    .bind(rows::receipt_op_to_text(draft.op))
    .bind(rows::ophash_to_vec(&draft.op_hash))
    .bind(draft.amount_micros.get())
    .bind(draft.from.as_ref().map(|a| a.as_str()))
    .bind(draft.to.as_ref().map(|a| a.as_str()))
    .bind(draft.balance_after_available_micros.get())
    .bind(draft.balance_after_held_micros.get())
    .bind(draft.ts)
    .bind(rows::hash_to_vec(&prev_hash))
    .bind(rows::hash_to_vec(&hash))
    .bind(rows::sig_to_vec(&sig))
    .bind(signer.key_id().as_str())
    .execute(&mut *conn)
    .await
    .map_err(db_err)?;

    Ok(hash)
}

/// Read an account's current available/held balance (no lock) for replay
/// outcome reconstruction.
pub async fn read_balance_row(
    conn: &mut PgConnection,
    id: &AccountId,
) -> Result<Balance, MintError> {
    let row = sqlx::query(
        "SELECT available_micros, held_micros, updated_seq FROM balances WHERE account_id = $1",
    )
    .bind(id.as_str())
    .fetch_optional(&mut *conn)
    .await
    .map_err(db_err)?;
    match row {
        Some(r) => Ok(Balance {
            account_id: id.clone(),
            available_micros: Micros::new(r.get::<i64, _>("available_micros")),
            held_micros: Micros::new(r.get::<i64, _>("held_micros")),
            updated_seq: r.get::<i64, _>("updated_seq") as u64,
        }),
        None => Err(MintError::AccountNotFound),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn db_err_maps_infra_faults_to_storage_not_overflow() {
        // A lost/closed pool is an infrastructure fault: it must surface as a
        // distinct `Storage` error (HTTP 500) and NOT be mislabelled as an
        // arithmetic `Overflow`, which would hide a real outage behind a
        // math-error code.
        assert_eq!(db_err(sqlx::Error::PoolClosed), MintError::Storage);
        assert_ne!(db_err(sqlx::Error::PoolClosed), MintError::Overflow);
    }
}
