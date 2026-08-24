//! The AP0 rebuild gate: fold the append-only journal (ORDER BY seq) back into
//! each projection and assert it equals the live table bit-for-bit. Ported
//! verbatim from `InMemoryStore` so both stores share one canonical fold.

use std::collections::HashMap;

use mint_core::{AccountId, HoldId, Micros, MintError, StreamId};
use mint_receipts::{ReceiptDraft, ReceiptLog, ReceiptOp, Signer};
use mint_store::{
    Balance, Bucket, HoldRecord, HoldStatus, JournalEntry, OpKind, StreamRecord, StreamStatus,
    StreamUnit,
};
use sqlx::{PgPool, Row};

use crate::rows;
use crate::tx::db_err;

/// Load the whole journal in `seq` order as [`JournalEntry`] rows.
pub async fn load_journal(pool: &PgPool) -> Result<Vec<JournalEntry>, MintError> {
    let rows = sqlx::query(
        "SELECT seq, op, op_hash, idem_key, account_id, counter_id, delta_micros, bucket, ref, ts \
         FROM journal ORDER BY seq ASC",
    )
    .fetch_all(pool)
    .await
    .map_err(db_err)?;

    let mut out = Vec::with_capacity(rows.len());
    for r in rows {
        let counter: Option<String> = r.get("counter_id");
        let counter_id = match counter {
            Some(c) => Some(AccountId::parse(&c)?),
            None => None,
        };
        out.push(JournalEntry {
            seq: r.get::<i64, _>("seq") as u64,
            op: rows::op_from_text(&r.get::<String, _>("op")),
            op_hash: rows::ophash_from_slice(&r.get::<Vec<u8>, _>("op_hash")),
            idem_key: r.get("idem_key"),
            account_id: AccountId::parse(&r.get::<String, _>("account_id"))?,
            counter_id,
            delta_micros: Micros::new(r.get::<i64, _>("delta_micros")),
            bucket: rows::bucket_from_i16(r.get::<i16, _>("bucket")),
            ref_: r.get("ref"),
            ts: r.get("ts"),
        });
    }
    Ok(out)
}

/// All known account ids (so accounts with no journal rows rebuild to zero).
pub async fn load_account_ids(pool: &PgPool) -> Result<Vec<AccountId>, MintError> {
    let rows = sqlx::query("SELECT id FROM accounts")
        .fetch_all(pool)
        .await
        .map_err(db_err)?;
    rows.iter()
        .map(|r| AccountId::parse(&r.get::<String, _>("id")))
        .collect()
}

/// Fold the journal into per-account balances (available + held legs).
pub async fn rebuild_balances(pool: &PgPool) -> Result<HashMap<AccountId, Balance>, MintError> {
    let journal = load_journal(pool).await?;
    let mut rebuilt: HashMap<AccountId, Balance> = load_account_ids(pool)
        .await?
        .into_iter()
        .map(|id| (id.clone(), Balance::zeroed(id)))
        .collect();

    for entry in &journal {
        let bal = rebuilt
            .entry(entry.account_id.clone())
            .or_insert_with(|| Balance::zeroed(entry.account_id.clone()));
        match entry.bucket {
            Bucket::Available => {
                bal.available_micros = bal.available_micros.checked_add(entry.delta_micros)?;
            }
            Bucket::Held => {
                bal.held_micros = bal.held_micros.checked_add(entry.delta_micros)?;
            }
        }
        bal.updated_seq = entry.seq;
    }
    Ok(rebuilt)
}

/// Fold the hold journal rows into the holds projection.
pub async fn rebuild_holds(pool: &PgPool) -> Result<HashMap<HoldId, HoldRecord>, MintError> {
    let journal = load_journal(pool).await?;
    let mut holds: HashMap<HoldId, HoldRecord> = HashMap::new();
    for entry in &journal {
        let hold_id = match (entry.op, entry.ref_.as_deref()) {
            (OpKind::HoldCreate | OpKind::HoldCapture | OpKind::HoldRelease, Some(r)) => {
                HoldId::parse(r)?
            }
            _ => continue,
        };
        match entry.op {
            OpKind::HoldCreate if entry.bucket == Bucket::Held => {
                holds.entry(hold_id.clone()).or_insert(HoldRecord {
                    id: hold_id.clone(),
                    account_id: entry.account_id.clone(),
                    amount_micros: entry.delta_micros,
                    captured_micros: Micros::zero(),
                    status: HoldStatus::Open,
                    ref_: None,
                });
            }
            OpKind::HoldCapture if entry.bucket == Bucket::Available => {
                if let Some(rec) = holds.get_mut(&hold_id) {
                    if entry.account_id != rec.account_id {
                        rec.captured_micros = entry.delta_micros;
                        rec.status = HoldStatus::Captured;
                    }
                }
            }
            OpKind::HoldRelease if entry.bucket == Bucket::Held => {
                if let Some(rec) = holds.get_mut(&hold_id) {
                    rec.status = HoldStatus::Released;
                }
            }
            _ => {}
        }
    }
    Ok(holds)
}

/// Fold the stream journal rows into the streams projection.
pub async fn rebuild_streams(pool: &PgPool) -> Result<HashMap<StreamId, StreamRecord>, MintError> {
    let journal = load_journal(pool).await?;
    let mut streams: HashMap<StreamId, StreamRecord> = HashMap::new();
    for entry in &journal {
        let stream_id = match (entry.op, entry.ref_.as_deref()) {
            (OpKind::StreamOpen | OpKind::StreamTick | OpKind::StreamClose, Some(r)) => {
                StreamId::parse(r)?
            }
            _ => continue,
        };
        match entry.op {
            OpKind::StreamOpen if entry.bucket == Bucket::Held => {
                let to = entry.counter_id.clone().ok_or(MintError::AccountNotFound)?;
                streams.entry(stream_id.clone()).or_insert(StreamRecord {
                    id: stream_id.clone(),
                    from_id: entry.account_id.clone(),
                    to_id: to,
                    rate_micros: Micros::zero(),
                    unit: StreamUnit::Second,
                    cap_micros: entry.delta_micros,
                    settled_micros: Micros::zero(),
                    last_seq: 0,
                    status: StreamStatus::Open,
                    ref_: None,
                });
            }
            OpKind::StreamTick if entry.bucket == Bucket::Available => {
                if let Some(rec) = streams.get_mut(&stream_id) {
                    if entry.account_id == rec.to_id {
                        rec.settled_micros = rec.settled_micros.checked_add(entry.delta_micros)?;
                    }
                }
            }
            OpKind::StreamClose if entry.bucket == Bucket::Available => {
                if let Some(rec) = streams.get_mut(&stream_id) {
                    if entry.account_id == rec.to_id {
                        rec.settled_micros = rec.settled_micros.checked_add(entry.delta_micros)?;
                    }
                    rec.status = StreamStatus::Closed;
                }
            }
            OpKind::StreamClose if entry.bucket == Bucket::Held => {
                if let Some(rec) = streams.get_mut(&stream_id) {
                    rec.status = StreamStatus::Closed;
                }
            }
            _ => {}
        }
    }
    Ok(streams)
}

/// Rebuild the full signed receipt log by folding the journal per op group and
/// re-appending drafts through `signer`. Must equal the live receipts bit-exact.
pub async fn rebuild_receipts(pool: &PgPool, signer: &dyn Signer) -> Result<ReceiptLog, MintError> {
    let journal = load_journal(pool).await?;
    let mut log = ReceiptLog::new();

    let mut avail: HashMap<AccountId, Micros> = HashMap::new();
    let mut held: HashMap<AccountId, Micros> = HashMap::new();
    let read = |avail: &HashMap<AccountId, Micros>,
                held: &HashMap<AccountId, Micros>,
                acct: &AccountId| {
        (
            avail.get(acct).copied().unwrap_or_else(Micros::zero),
            held.get(acct).copied().unwrap_or_else(Micros::zero),
        )
    };

    let mut i = 0;
    while i < journal.len() {
        let op_hash = journal[i].op_hash;
        let mut j = i;
        while j < journal.len() && journal[j].op_hash == op_hash {
            j += 1;
        }
        let group = &journal[i..j];
        if group.iter().any(|r| r.op != group[0].op) {
            return Err(MintError::ReceiptRebuildMismatch);
        }

        for row in group {
            match row.bucket {
                Bucket::Available => {
                    let e = avail
                        .entry(row.account_id.clone())
                        .or_insert_with(Micros::zero);
                    *e = e.checked_add(row.delta_micros)?;
                }
                Bucket::Held => {
                    let e = held
                        .entry(row.account_id.clone())
                        .or_insert_with(Micros::zero);
                    *e = e.checked_add(row.delta_micros)?;
                }
            }
        }

        let ts = group[0].ts;
        let op_hash = group[0].op_hash;
        match group[0].op {
            OpKind::Seed => {
                let acct = group[0].account_id.clone();
                let (a, h) = read(&avail, &held, &acct);
                log.append(
                    ReceiptDraft {
                        account_id: acct.clone(),
                        journal_seq: group[0].seq,
                        op: ReceiptOp::Seed,
                        op_hash,
                        amount_micros: group[0].delta_micros,
                        from: None,
                        to: Some(acct),
                        balance_after_available_micros: a,
                        balance_after_held_micros: h,
                        ts,
                    },
                    signer,
                );
            }
            OpKind::Transfer => {
                let from = group[0].account_id.clone();
                let to = group[1].account_id.clone();
                let amount = group[1].delta_micros;
                let (fa, fh) = read(&avail, &held, &from);
                log.append(
                    ReceiptDraft {
                        account_id: from.clone(),
                        journal_seq: group[0].seq,
                        op: ReceiptOp::Transfer,
                        op_hash,
                        amount_micros: amount,
                        from: Some(from.clone()),
                        to: Some(to.clone()),
                        balance_after_available_micros: fa,
                        balance_after_held_micros: fh,
                        ts,
                    },
                    signer,
                );
                let (ta, th) = read(&avail, &held, &to);
                log.append(
                    ReceiptDraft {
                        account_id: to.clone(),
                        journal_seq: group[1].seq,
                        op: ReceiptOp::Transfer,
                        op_hash,
                        amount_micros: amount,
                        from: Some(from),
                        to: Some(to),
                        balance_after_available_micros: ta,
                        balance_after_held_micros: th,
                        ts,
                    },
                    signer,
                );
            }
            OpKind::HoldCreate => {
                let holder = group[0].account_id.clone();
                let amount = group[1].delta_micros;
                let (a, h) = read(&avail, &held, &holder);
                log.append(
                    ReceiptDraft {
                        account_id: holder.clone(),
                        journal_seq: group[1].seq,
                        op: ReceiptOp::HoldCreate,
                        op_hash,
                        amount_micros: amount,
                        from: Some(holder),
                        to: None,
                        balance_after_available_micros: a,
                        balance_after_held_micros: h,
                        ts,
                    },
                    signer,
                );
            }
            OpKind::HoldCapture => {
                let holder = group[0].account_id.clone();
                let payee = group[1].account_id.clone();
                let capture = group[1].delta_micros;
                let holder_seq = if group.len() == 3 {
                    group[2].seq
                } else {
                    group[0].seq
                };
                let (ha, hh) = read(&avail, &held, &holder);
                log.append(
                    ReceiptDraft {
                        account_id: holder.clone(),
                        journal_seq: holder_seq,
                        op: ReceiptOp::HoldCapture,
                        op_hash,
                        amount_micros: capture,
                        from: Some(holder.clone()),
                        to: Some(payee.clone()),
                        balance_after_available_micros: ha,
                        balance_after_held_micros: hh,
                        ts,
                    },
                    signer,
                );
                let (pa, ph) = read(&avail, &held, &payee);
                log.append(
                    ReceiptDraft {
                        account_id: payee.clone(),
                        journal_seq: group[1].seq,
                        op: ReceiptOp::HoldCapture,
                        op_hash,
                        amount_micros: capture,
                        from: Some(holder),
                        to: Some(payee),
                        balance_after_available_micros: pa,
                        balance_after_held_micros: ph,
                        ts,
                    },
                    signer,
                );
            }
            OpKind::HoldRelease => {
                let holder = group[0].account_id.clone();
                let amount = group[1].delta_micros;
                let (a, h) = read(&avail, &held, &holder);
                log.append(
                    ReceiptDraft {
                        account_id: holder.clone(),
                        journal_seq: group[1].seq,
                        op: ReceiptOp::HoldRelease,
                        op_hash,
                        amount_micros: amount,
                        from: Some(holder),
                        to: None,
                        balance_after_available_micros: a,
                        balance_after_held_micros: h,
                        ts,
                    },
                    signer,
                );
            }
            OpKind::StreamOpen => {
                let holder = group[0].account_id.clone();
                let to = group[0].counter_id.clone();
                let cap = group[1].delta_micros;
                let (a, h) = read(&avail, &held, &holder);
                log.append(
                    ReceiptDraft {
                        account_id: holder.clone(),
                        journal_seq: group[1].seq,
                        op: ReceiptOp::StreamOpen,
                        op_hash,
                        amount_micros: cap,
                        from: Some(holder),
                        to,
                        balance_after_available_micros: a,
                        balance_after_held_micros: h,
                        ts,
                    },
                    signer,
                );
            }
            OpKind::StreamTick => {
                let from = group[0].account_id.clone();
                let to = group[1].account_id.clone();
                let amount = group[1].delta_micros;
                let (fa, fh) = read(&avail, &held, &from);
                log.append(
                    ReceiptDraft {
                        account_id: from.clone(),
                        journal_seq: group[0].seq,
                        op: ReceiptOp::StreamTick,
                        op_hash,
                        amount_micros: amount,
                        from: Some(from.clone()),
                        to: Some(to.clone()),
                        balance_after_available_micros: fa,
                        balance_after_held_micros: fh,
                        ts,
                    },
                    signer,
                );
                let (ta, th) = read(&avail, &held, &to);
                log.append(
                    ReceiptDraft {
                        account_id: to.clone(),
                        journal_seq: group[1].seq,
                        op: ReceiptOp::StreamTick,
                        op_hash,
                        amount_micros: amount,
                        from: Some(from),
                        to: Some(to),
                        balance_after_available_micros: ta,
                        balance_after_held_micros: th,
                        ts,
                    },
                    signer,
                );
            }
            OpKind::StreamClose => {
                let holder = group[0].account_id.clone();
                let to = group[0].counter_id.clone();
                let payee_row = group
                    .iter()
                    .find(|r| r.bucket == Bucket::Available && Some(&r.account_id) == to.as_ref());
                let mut held_cleared = Micros::zero();
                for r in group {
                    if r.bucket == Bucket::Held && r.account_id == holder {
                        held_cleared = held_cleared.checked_sub(r.delta_micros)?;
                    }
                }
                if let Some(pr) = payee_row {
                    let payee = pr.account_id.clone();
                    let (pa, ph) = read(&avail, &held, &payee);
                    log.append(
                        ReceiptDraft {
                            account_id: payee.clone(),
                            journal_seq: pr.seq,
                            op: ReceiptOp::StreamClose,
                            op_hash,
                            amount_micros: pr.delta_micros,
                            from: Some(holder.clone()),
                            to: Some(payee),
                            balance_after_available_micros: pa,
                            balance_after_held_micros: ph,
                            ts,
                        },
                        signer,
                    );
                }
                let (ha, hh) = read(&avail, &held, &holder);
                log.append(
                    ReceiptDraft {
                        account_id: holder.clone(),
                        journal_seq: group[0].seq,
                        op: ReceiptOp::StreamClose,
                        op_hash,
                        amount_micros: held_cleared,
                        from: Some(holder),
                        to,
                        balance_after_available_micros: ha,
                        balance_after_held_micros: hh,
                        ts,
                    },
                    signer,
                );
            }
            OpKind::Mint => {
                let acct = group[0].account_id.clone();
                let (a, h) = read(&avail, &held, &acct);
                log.append(
                    ReceiptDraft {
                        account_id: acct.clone(),
                        journal_seq: group[0].seq,
                        op: ReceiptOp::Mint,
                        op_hash,
                        amount_micros: group[0].delta_micros,
                        from: None,
                        to: Some(acct),
                        balance_after_available_micros: a,
                        balance_after_held_micros: h,
                        ts,
                    },
                    signer,
                );
            }
            OpKind::Redeem => {
                let acct = group[0].account_id.clone();
                let amount = group[0].delta_micros.checked_neg()?;
                let (a, h) = read(&avail, &held, &acct);
                log.append(
                    ReceiptDraft {
                        account_id: acct.clone(),
                        journal_seq: group[0].seq,
                        op: ReceiptOp::Redeem,
                        op_hash,
                        amount_micros: amount,
                        from: Some(acct),
                        to: None,
                        balance_after_available_micros: a,
                        balance_after_held_micros: h,
                        ts,
                    },
                    signer,
                );
            }
        }
        i = j;
    }
    Ok(log)
}
