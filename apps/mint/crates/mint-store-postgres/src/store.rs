//! [`PgStore`] — the production Postgres [`LedgerStore`]. Uses the sqlx RUNTIME
//! query API only (`sqlx::query` / `.bind`), never the `query!` macro, so it
//! compiles with NO live `DATABASE_URL`. Every `commit_*` opens one transaction,
//! takes `FOR UPDATE` balance locks in `AccountId` sort order (deadlock-free),
//! checks the idempotency seam, appends the append-only journal + hash-linked
//! receipts, and updates the projections — all-or-nothing.

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use mint_core::{
    seed_bytes16, AccountId, AccountKind, Address, AllowanceId, HoldId, Micros, MintError, StreamId,
};
use mint_receipts::{
    merkle_root, FixedSigner, Receipt, ReceiptBody, ReceiptDraft, ReceiptHash, ReceiptLog,
    ReceiptOp, Signer,
};
use mint_store::{
    AllowanceGrantCommit, AllowanceGrantOutcome, AllowanceRecord, AllowanceRevokeCommit,
    AllowanceScheme, AllowanceScope, AllowanceSpendCommit, AllowanceSpendOutcome, AllowanceWindow,
    Balance, Bucket, Clock, CommitOutcome, HoldCaptureCommit, HoldCreateCommit, HoldOutcome,
    HoldRecord, HoldReleaseCommit, HoldStatus, LedgerStore, MintCommit, MintOutcome, OpKind,
    RedeemCommit, RedeemOutcome, StreamCloseCommit, StreamCloseOutcome, StreamOpenCommit,
    StreamOpenOutcome, StreamRecord, StreamStatus, StreamTickCommit, StreamTickOutcome,
    SystemClock, TransferCommit,
};
use sqlx::postgres::PgPoolOptions;
use sqlx::{PgConnection, PgPool, Row};
use ulid::Ulid;

use crate::rebuild;
use crate::rows;
use crate::tx::{
    account_exists, append_journal, append_receipt, check_idem, db_err, lock_balance,
    read_balance_row, record_idem, write_balance, IdemCheck,
};

/// Optional pool tuning for [`PgStore::connect_with`].
#[derive(Clone, Debug)]
pub struct PgStoreConfig {
    /// Maximum pooled connections.
    pub max_connections: u32,
}

impl Default for PgStoreConfig {
    fn default() -> Self {
        PgStoreConfig {
            max_connections: 16,
        }
    }
}

/// The production Postgres-backed [`LedgerStore`].
pub struct PgStore {
    pool: PgPool,
    signer: Arc<dyn Signer>,
    clock: Arc<dyn Clock>,
}

impl PgStore {
    /// Connect to `url` with the given receipt `signer` and default pool config.
    pub async fn connect(url: &str, signer: Arc<dyn Signer>) -> Result<Self, sqlx::Error> {
        Self::connect_with(url, signer, PgStoreConfig::default(), Arc::new(SystemClock)).await
    }

    /// Connect with an explicit pool config + clock (deterministic in tests).
    pub async fn connect_with(
        url: &str,
        signer: Arc<dyn Signer>,
        cfg: PgStoreConfig,
        clock: Arc<dyn Clock>,
    ) -> Result<Self, sqlx::Error> {
        let pool = PgPoolOptions::new()
            .max_connections(cfg.max_connections)
            .connect(url)
            .await?;
        Ok(PgStore {
            pool,
            signer,
            clock,
        })
    }

    /// Wrap an existing pool with the default deterministic signer/clock (for
    /// tests that own the pool).
    pub fn from_pool(pool: PgPool) -> Self {
        PgStore {
            pool,
            signer: Arc::new(FixedSigner::new()),
            clock: Arc::new(SystemClock),
        }
    }

    /// The underlying pool (for the readiness probe / live tests).
    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    /// Apply every pending migration.
    pub async fn run_migrations(&self) -> Result<(), sqlx::migrate::MigrateError> {
        crate::migrate::run_migrations(&self.pool).await
    }

    // ---- private helpers ---------------------------------------------------

    fn signer(&self) -> &dyn Signer {
        self.signer.as_ref()
    }

    /// Lock two accounts' balance rows in `AccountId` sort order → deadlock-free.
    async fn lock_pair(
        &self,
        conn: &mut PgConnection,
        a: &AccountId,
        b: &AccountId,
    ) -> Result<(Balance, Balance), MintError> {
        if a <= b {
            let ba = lock_balance(conn, a).await?;
            let bb = lock_balance(conn, b).await?;
            Ok((ba, bb))
        } else {
            let bb = lock_balance(conn, b).await?;
            let ba = lock_balance(conn, a).await?;
            Ok((ba, bb))
        }
    }

    /// Lock + read the single-row reserve total.
    async fn lock_reserve(&self, conn: &mut PgConnection) -> Result<Micros, MintError> {
        let row = sqlx::query("SELECT total_micros FROM reserve WHERE id = 0 FOR UPDATE")
            .fetch_optional(&mut *conn)
            .await
            .map_err(db_err)?;
        Ok(Micros::new(
            row.map(|r| r.get::<i64, _>("total_micros")).unwrap_or(0),
        ))
    }

    async fn write_reserve(&self, conn: &mut PgConnection, total: Micros) -> Result<(), MintError> {
        sqlx::query(
            "INSERT INTO reserve (id, total_micros) VALUES (0, $1) \
             ON CONFLICT (id) DO UPDATE SET total_micros = EXCLUDED.total_micros",
        )
        .bind(total.get())
        .execute(&mut *conn)
        .await
        .map_err(db_err)?;
        Ok(())
    }

    /// The receipt rows (available/held balance-after) for one op, in chain order.
    async fn op_receipts(
        &self,
        conn: &mut PgConnection,
        op_hash: &mint_core::OpHash,
    ) -> Result<Vec<(AccountId, i64, i64, i64)>, MintError> {
        let rows = sqlx::query(
            "SELECT account_id, balance_after_micros, balance_after_held, journal_seq \
             FROM receipts WHERE op_hash = $1 ORDER BY seq ASC",
        )
        .bind(rows::ophash_to_vec(op_hash))
        .fetch_all(&mut *conn)
        .await
        .map_err(db_err)?;
        let mut out = Vec::with_capacity(rows.len());
        for r in rows {
            out.push((
                AccountId::parse(&r.get::<String, _>("account_id"))?,
                r.get::<i64, _>("balance_after_micros"),
                r.get::<i64, _>("balance_after_held"),
                r.get::<i64, _>("journal_seq"),
            ));
        }
        Ok(out)
    }

    /// Read a live hold record by id.
    /// Read a live hold record by id. When `for_update` is set, the row is
    /// locked `FOR UPDATE` so concurrent capture/release on the same hold
    /// serialize on this row and re-evaluate `status` against fresh state
    /// (prevents the stale-read double-capture / double-release race).
    async fn get_hold(
        &self,
        conn: &mut PgConnection,
        id: &HoldId,
        for_update: bool,
    ) -> Result<HoldRecord, MintError> {
        let sql = if for_update {
            "SELECT account_id, amount_micros, captured_micros, status, ref FROM holds WHERE id = $1 FOR UPDATE"
        } else {
            "SELECT account_id, amount_micros, captured_micros, status, ref FROM holds WHERE id = $1"
        };
        let row = sqlx::query(sql)
            .bind(id.as_str())
            .fetch_optional(&mut *conn)
            .await
            .map_err(db_err)?
            .ok_or(MintError::HoldNotFound)?;
        Ok(HoldRecord {
            id: id.clone(),
            account_id: AccountId::parse(&row.get::<String, _>("account_id"))?,
            amount_micros: Micros::new(row.get::<i64, _>("amount_micros")),
            captured_micros: Micros::new(row.get::<i64, _>("captured_micros")),
            status: rows::hold_status_from_i16(row.get::<i16, _>("status")),
            ref_: row.get("ref"),
        })
    }

    /// Read a live stream record by id. When `for_update` is set, the row is
    /// locked `FOR UPDATE` so concurrent ticks/close on the same stream
    /// serialize on this row and re-evaluate `last_seq`/`settled`/`status`
    /// against fresh state (prevents the stale-read double-tick / cap bypass).
    async fn get_stream_row(
        &self,
        conn: &mut PgConnection,
        id: &StreamId,
        for_update: bool,
    ) -> Result<StreamRecord, MintError> {
        let sql = if for_update {
            "SELECT from_id, to_id, rate_micros, unit, cap_micros, settled_micros, last_seq, status, ref \
             FROM streams WHERE id = $1 FOR UPDATE"
        } else {
            "SELECT from_id, to_id, rate_micros, unit, cap_micros, settled_micros, last_seq, status, ref \
             FROM streams WHERE id = $1"
        };
        let row = sqlx::query(sql)
            .bind(id.as_str())
            .fetch_optional(&mut *conn)
            .await
            .map_err(db_err)?
            .ok_or(MintError::StreamNotFound)?;
        Ok(StreamRecord {
            id: id.clone(),
            from_id: AccountId::parse(&row.get::<String, _>("from_id"))?,
            to_id: AccountId::parse(&row.get::<String, _>("to_id"))?,
            rate_micros: Micros::new(row.get::<i64, _>("rate_micros")),
            unit: rows::stream_unit_from_i16(row.get::<i16, _>("unit")),
            cap_micros: Micros::new(row.get::<i64, _>("cap_micros")),
            settled_micros: Micros::new(row.get::<i64, _>("settled_micros")),
            last_seq: row.get::<i64, _>("last_seq") as u64,
            status: rows::stream_status_from_i16(row.get::<i16, _>("status")),
            ref_: row.get("ref"),
        })
    }

    async fn get_allowance_row(
        &self,
        conn: &mut PgConnection,
        id: &AllowanceId,
    ) -> Result<AllowanceRecord, MintError> {
        let row = sqlx::query(
            "SELECT account_id, key_id, limit_micros, spent_micros, window_kind, window_ms, \
                    window_start, scope, revoked_at FROM allowances WHERE id = $1",
        )
        .bind(id.as_str())
        .fetch_optional(&mut *conn)
        .await
        .map_err(db_err)?
        .ok_or(MintError::AllowanceNotFound)?;
        let scope: Option<serde_json::Value> = row.get("scope");
        let scope = match scope {
            Some(v) => serde_json::from_value::<AllowanceScope>(v).ok(),
            None => None,
        };
        Ok(AllowanceRecord {
            id: id.clone(),
            granter: AccountId::parse(&row.get::<String, _>("account_id"))?,
            to_key: row.get("key_id"),
            limit_micros: Micros::new(row.get::<i64, _>("limit_micros")),
            spent_micros: Micros::new(row.get::<i64, _>("spent_micros")),
            window: rows::window_from_cols(
                row.get::<i16, _>("window_kind"),
                row.get::<i64, _>("window_ms"),
            ),
            window_start_ms: row.get::<i64, _>("window_start"),
            scope,
            revoked: row.get::<Option<i64>, _>("revoked_at").is_some(),
        })
    }

    /// Shared atomic double-entry apply used by both `commit_transfer` and the
    /// allowance `Transfer` auto-settlement (mirrors `settle_transfer_locked`).
    #[allow(clippy::too_many_arguments)]
    async fn settle_transfer(
        &self,
        conn: &mut PgConnection,
        from: &AccountId,
        to: &AccountId,
        amount: Micros,
        op_hash: &mint_core::OpHash,
        idem_key: &str,
        ref_: Option<&str>,
        ts: i64,
    ) -> Result<CommitOutcome, MintError> {
        if from == to {
            return Err(MintError::BadAccountId);
        }
        let (from_bal, to_bal) = self.lock_pair(conn, from, to).await?;

        let new_from = from_bal.available_micros.checked_sub(amount)?;
        if new_from.is_negative() {
            return Err(MintError::InsufficientFunds);
        }
        let new_to = to_bal.available_micros.checked_add(amount)?;
        let neg = amount.checked_neg()?;

        let debit_seq = append_journal(
            conn,
            OpKind::Transfer,
            op_hash,
            idem_key,
            from,
            Some(to),
            neg,
            Bucket::Available,
            ref_,
            ts,
        )
        .await?;
        let credit_seq = append_journal(
            conn,
            OpKind::Transfer,
            op_hash,
            idem_key,
            to,
            Some(from),
            amount,
            Bucket::Available,
            ref_,
            ts,
        )
        .await?;

        let from_balance = Balance {
            account_id: from.clone(),
            available_micros: new_from,
            held_micros: from_bal.held_micros,
            updated_seq: debit_seq as u64,
        };
        let to_balance = Balance {
            account_id: to.clone(),
            available_micros: new_to,
            held_micros: to_bal.held_micros,
            updated_seq: credit_seq as u64,
        };
        write_balance(conn, &from_balance).await?;
        write_balance(conn, &to_balance).await?;

        append_receipt(
            conn,
            self.signer(),
            ReceiptDraft {
                account_id: from.clone(),
                journal_seq: debit_seq as u64,
                op: ReceiptOp::Transfer,
                op_hash: *op_hash,
                amount_micros: amount,
                from: Some(from.clone()),
                to: Some(to.clone()),
                balance_after_available_micros: from_balance.available_micros,
                balance_after_held_micros: from_balance.held_micros,
                ts,
            },
        )
        .await?;
        append_receipt(
            conn,
            self.signer(),
            ReceiptDraft {
                account_id: to.clone(),
                journal_seq: credit_seq as u64,
                op: ReceiptOp::Transfer,
                op_hash: *op_hash,
                amount_micros: amount,
                from: Some(from.clone()),
                to: Some(to.clone()),
                balance_after_available_micros: to_balance.available_micros,
                balance_after_held_micros: to_balance.held_micros,
                ts,
            },
        )
        .await?;

        Ok(CommitOutcome {
            first_seq: debit_seq as u64,
            from_balance,
            to_balance,
            idempotent_replay: false,
        })
    }

    /// Shared `hold.create` apply used by both `commit_hold_create` and the
    /// allowance `Hold` auto-settlement (mirrors `settle_hold_create_locked`).
    #[allow(clippy::too_many_arguments)]
    async fn settle_hold_create(
        &self,
        conn: &mut PgConnection,
        from: &AccountId,
        amount: Micros,
        op_hash: &mint_core::OpHash,
        idem_key: &str,
        ref_: Option<&str>,
        ts: i64,
    ) -> Result<HoldOutcome, MintError> {
        let from_bal = lock_balance(conn, from).await?;
        let new_available = from_bal.available_micros.checked_sub(amount)?;
        if new_available.is_negative() {
            return Err(MintError::InsufficientFunds);
        }
        let new_held = from_bal.held_micros.checked_add(amount)?;
        let neg = amount.checked_neg()?;

        let hold_id = HoldId::new(Ulid::new());
        let hold_ref = hold_id.as_str().to_string();

        let avail_seq = append_journal(
            conn,
            OpKind::HoldCreate,
            op_hash,
            idem_key,
            from,
            None,
            neg,
            Bucket::Available,
            Some(&hold_ref),
            ts,
        )
        .await?;
        let held_seq = append_journal(
            conn,
            OpKind::HoldCreate,
            op_hash,
            idem_key,
            from,
            None,
            amount,
            Bucket::Held,
            Some(&hold_ref),
            ts,
        )
        .await?;

        let holder_balance = Balance {
            account_id: from.clone(),
            available_micros: new_available,
            held_micros: new_held,
            updated_seq: held_seq as u64,
        };
        write_balance(conn, &holder_balance).await?;

        sqlx::query(
            "INSERT INTO holds (id, account_id, amount_micros, captured_micros, status, ref, created_at) \
             VALUES ($1,$2,$3,0,$4,$5,$6)",
        )
        .bind(hold_id.as_str())
        .bind(from.as_str())
        .bind(amount.get())
        .bind(rows::hold_status_to_i16(HoldStatus::Open))
        .bind(ref_)
        .bind(ts)
        .execute(&mut *conn)
        .await
        .map_err(db_err)?;

        append_receipt(
            conn,
            self.signer(),
            ReceiptDraft {
                account_id: from.clone(),
                journal_seq: held_seq as u64,
                op: ReceiptOp::HoldCreate,
                op_hash: *op_hash,
                amount_micros: amount,
                from: Some(from.clone()),
                to: None,
                balance_after_available_micros: holder_balance.available_micros,
                balance_after_held_micros: holder_balance.held_micros,
                ts,
            },
        )
        .await?;

        Ok(HoldOutcome {
            hold_id,
            first_seq: avail_seq as u64,
            holder_balance,
            payee_balance: None,
            status: HoldStatus::Open,
            captured_micros: Micros::zero(),
            idempotent_replay: false,
        })
    }
}

#[async_trait]
impl LedgerStore for PgStore {
    async fn create_account(
        &self,
        kind: AccountKind,
        owner_ref: Option<String>,
    ) -> Result<AccountId, MintError> {
        let id = AccountId::new(kind, Ulid::new());
        let mut tx = self.pool.begin().await.map_err(db_err)?;
        sqlx::query("INSERT INTO accounts (id, kind, owner_ref, status) VALUES ($1,$2,$3,0)")
            .bind(id.as_str())
            .bind(rows::kind_to_i16(kind))
            .bind(owner_ref)
            .execute(&mut *tx)
            .await
            .map_err(db_err)?;
        sqlx::query(
            "INSERT INTO balances (account_id, available_micros, held_micros, updated_seq) \
             VALUES ($1,0,0,0)",
        )
        .bind(id.as_str())
        .execute(&mut *tx)
        .await
        .map_err(db_err)?;
        tx.commit().await.map_err(db_err)?;
        Ok(id)
    }

    async fn create_address(
        &self,
        account_id: &AccountId,
        purpose: Option<String>,
    ) -> Result<Address, MintError> {
        let mut conn = self.pool.acquire().await.map_err(db_err)?;
        if !account_exists(&mut conn, account_id).await? {
            return Err(MintError::AccountNotFound);
        }
        let purpose_str = purpose.clone().unwrap_or_default();
        let data = seed_bytes16(&[account_id.as_str(), &purpose_str]);
        let addr = Address::encode(account_id.kind(), &data);
        sqlx::query(
            "INSERT INTO addresses (address, account_id, purpose) VALUES ($1,$2,$3) \
             ON CONFLICT (address) DO NOTHING",
        )
        .bind(addr.as_str())
        .bind(account_id.as_str())
        .bind(purpose)
        .execute(&mut *conn)
        .await
        .map_err(db_err)?;
        Ok(addr)
    }

    async fn lookup_address(&self, addr: &Address) -> Result<AccountId, MintError> {
        addr.decode()?;
        let row = sqlx::query("SELECT account_id FROM addresses WHERE address = $1")
            .bind(addr.as_str())
            .fetch_optional(&self.pool)
            .await
            .map_err(db_err)?
            .ok_or(MintError::AccountNotFound)?;
        AccountId::parse(&row.get::<String, _>("account_id"))
    }

    async fn commit_transfer(&self, commit: TransferCommit) -> Result<CommitOutcome, MintError> {
        let ts = self.clock.now();
        let mut tx = self.pool.begin().await.map_err(db_err)?;
        // Lock both balance rows first (sorted) so the idempotency check and the
        // apply share one serialized critical section per account pair.
        let _ = self.lock_pair(&mut tx, &commit.from, &commit.to).await?;

        match check_idem(&mut tx, &commit.idem_key, &commit.op_hash).await? {
            IdemCheck::Conflict => return Err(MintError::IdempotencyConflict),
            IdemCheck::Replay { .. } => {
                let recs = self.op_receipts(&mut tx, &commit.op_hash).await?;
                if recs.len() < 2 {
                    return Err(MintError::Overflow);
                }
                let from_balance = Balance {
                    account_id: recs[0].0.clone(),
                    available_micros: Micros::new(recs[0].1),
                    held_micros: Micros::new(recs[0].2),
                    updated_seq: recs[0].3 as u64,
                };
                let to_balance = Balance {
                    account_id: recs[1].0.clone(),
                    available_micros: Micros::new(recs[1].1),
                    held_micros: Micros::new(recs[1].2),
                    updated_seq: recs[1].3 as u64,
                };
                return Ok(CommitOutcome {
                    first_seq: recs[0].3 as u64,
                    from_balance,
                    to_balance,
                    idempotent_replay: true,
                });
            }
            IdemCheck::Fresh => {}
        }

        let outcome = self
            .settle_transfer(
                &mut tx,
                &commit.from,
                &commit.to,
                commit.amount.micros(),
                &commit.op_hash,
                &commit.idem_key,
                commit.ref_.as_deref(),
                ts,
            )
            .await?;
        record_idem(
            &mut tx,
            &commit.idem_key,
            &commit.op_hash,
            outcome.first_seq as i64,
            None,
            "transfer",
            ts,
        )
        .await?;
        tx.commit().await.map_err(db_err)?;
        Ok(outcome)
    }

    async fn commit_hold_create(&self, commit: HoldCreateCommit) -> Result<HoldOutcome, MintError> {
        let ts = self.clock.now();
        let mut tx = self.pool.begin().await.map_err(db_err)?;
        let _ = lock_balance(&mut tx, &commit.from).await?;

        match check_idem(&mut tx, &commit.idem_key, &commit.op_hash).await? {
            IdemCheck::Conflict => return Err(MintError::IdempotencyConflict),
            IdemCheck::Replay {
                first_seq,
                result_ref,
            } => {
                let hold_id = HoldId::parse(&result_ref.ok_or(MintError::HoldNotFound)?)?;
                let recs = self.op_receipts(&mut tx, &commit.op_hash).await?;
                let holder = recs.first().cloned().ok_or(MintError::Overflow)?;
                return Ok(HoldOutcome {
                    hold_id,
                    first_seq: first_seq as u64,
                    holder_balance: Balance {
                        account_id: holder.0,
                        available_micros: Micros::new(holder.1),
                        held_micros: Micros::new(holder.2),
                        updated_seq: holder.3 as u64,
                    },
                    payee_balance: None,
                    status: HoldStatus::Open,
                    captured_micros: Micros::zero(),
                    idempotent_replay: true,
                });
            }
            IdemCheck::Fresh => {}
        }

        let outcome = self
            .settle_hold_create(
                &mut tx,
                &commit.from,
                commit.amount.micros(),
                &commit.op_hash,
                &commit.idem_key,
                commit.ref_.as_deref(),
                ts,
            )
            .await?;
        record_idem(
            &mut tx,
            &commit.idem_key,
            &commit.op_hash,
            outcome.first_seq as i64,
            Some(outcome.hold_id.as_str()),
            "hold.create",
            ts,
        )
        .await?;
        tx.commit().await.map_err(db_err)?;
        Ok(outcome)
    }

    async fn commit_hold_capture(
        &self,
        commit: HoldCaptureCommit,
    ) -> Result<HoldOutcome, MintError> {
        let ts = self.clock.now();
        let mut tx = self.pool.begin().await.map_err(db_err)?;

        // Lock the hold row FOR UPDATE first so concurrent captures/releases of
        // the same hold serialize here; then lock holder+payee balances sorted.
        // The status guard below reads this freshly-locked row, so the second
        // concurrent capture blocks, then sees status=Captured -> HoldNotFound.
        let hold = self.get_hold(&mut tx, &commit.hold_id, true).await?;
        let holder = hold.account_id.clone();
        let _ = self.lock_pair(&mut tx, &holder, &commit.to).await?;

        match check_idem(&mut tx, &commit.idem_key, &commit.op_hash).await? {
            IdemCheck::Conflict => return Err(MintError::IdempotencyConflict),
            IdemCheck::Replay { first_seq, .. } => {
                let rec = self.get_hold(&mut tx, &commit.hold_id, false).await?;
                let recs = self.op_receipts(&mut tx, &commit.op_hash).await?;
                if recs.len() < 2 {
                    return Err(MintError::Overflow);
                }
                return Ok(HoldOutcome {
                    hold_id: commit.hold_id.clone(),
                    first_seq: first_seq as u64,
                    holder_balance: Balance {
                        account_id: recs[0].0.clone(),
                        available_micros: Micros::new(recs[0].1),
                        held_micros: Micros::new(recs[0].2),
                        updated_seq: recs[0].3 as u64,
                    },
                    payee_balance: Some(Balance {
                        account_id: recs[1].0.clone(),
                        available_micros: Micros::new(recs[1].1),
                        held_micros: Micros::new(recs[1].2),
                        updated_seq: recs[1].3 as u64,
                    }),
                    status: rec.status,
                    captured_micros: rec.captured_micros,
                    idempotent_replay: true,
                });
            }
            IdemCheck::Fresh => {}
        }

        if hold.status != HoldStatus::Open {
            return Err(MintError::HoldNotFound);
        }
        if commit.to == holder {
            return Err(MintError::BadAccountId);
        }
        let capture = commit.amount.micros();
        let hold_amt = hold.amount_micros;
        if capture.get() > hold_amt.get() {
            return Err(MintError::CaptureExceedsHold);
        }
        let remainder = hold_amt.checked_sub(capture)?;

        let holder_bal = read_balance_row(&mut tx, &holder).await?;
        let to_bal = read_balance_row(&mut tx, &commit.to).await?;
        let new_holder_held = holder_bal.held_micros.checked_sub(hold_amt)?;
        if new_holder_held.is_negative() {
            return Err(MintError::InsufficientFunds);
        }
        let new_holder_available = holder_bal.available_micros.checked_add(remainder)?;
        let new_payee_available = to_bal.available_micros.checked_add(capture)?;
        let neg_hold = hold_amt.checked_neg()?;
        let hold_ref = commit.hold_id.as_str().to_string();

        let held_seq = append_journal(
            &mut tx,
            OpKind::HoldCapture,
            &commit.op_hash,
            &commit.idem_key,
            &holder,
            Some(&commit.to),
            neg_hold,
            Bucket::Held,
            Some(&hold_ref),
            ts,
        )
        .await?;
        let payee_seq = append_journal(
            &mut tx,
            OpKind::HoldCapture,
            &commit.op_hash,
            &commit.idem_key,
            &commit.to,
            Some(&holder),
            capture,
            Bucket::Available,
            Some(&hold_ref),
            ts,
        )
        .await?;
        let mut holder_updated_seq = held_seq;
        if remainder.is_positive() {
            let rem_seq = append_journal(
                &mut tx,
                OpKind::HoldCapture,
                &commit.op_hash,
                &commit.idem_key,
                &holder,
                None,
                remainder,
                Bucket::Available,
                Some(&hold_ref),
                ts,
            )
            .await?;
            holder_updated_seq = rem_seq;
        }

        let holder_balance = Balance {
            account_id: holder.clone(),
            available_micros: new_holder_available,
            held_micros: new_holder_held,
            updated_seq: holder_updated_seq as u64,
        };
        let payee_balance = Balance {
            account_id: commit.to.clone(),
            available_micros: new_payee_available,
            held_micros: to_bal.held_micros,
            updated_seq: payee_seq as u64,
        };
        write_balance(&mut tx, &holder_balance).await?;
        write_balance(&mut tx, &payee_balance).await?;

        append_receipt(
            &mut tx,
            self.signer(),
            ReceiptDraft {
                account_id: holder.clone(),
                journal_seq: holder_updated_seq as u64,
                op: ReceiptOp::HoldCapture,
                op_hash: commit.op_hash,
                amount_micros: capture,
                from: Some(holder.clone()),
                to: Some(commit.to.clone()),
                balance_after_available_micros: holder_balance.available_micros,
                balance_after_held_micros: holder_balance.held_micros,
                ts,
            },
        )
        .await?;
        append_receipt(
            &mut tx,
            self.signer(),
            ReceiptDraft {
                account_id: commit.to.clone(),
                journal_seq: payee_seq as u64,
                op: ReceiptOp::HoldCapture,
                op_hash: commit.op_hash,
                amount_micros: capture,
                from: Some(holder.clone()),
                to: Some(commit.to.clone()),
                balance_after_available_micros: payee_balance.available_micros,
                balance_after_held_micros: payee_balance.held_micros,
                ts,
            },
        )
        .await?;

        sqlx::query(
            "UPDATE holds SET captured_micros = $1, status = $2, closed_at = $3 WHERE id = $4",
        )
        .bind(capture.get())
        .bind(rows::hold_status_to_i16(HoldStatus::Captured))
        .bind(ts)
        .bind(commit.hold_id.as_str())
        .execute(&mut *tx)
        .await
        .map_err(db_err)?;

        record_idem(
            &mut tx,
            &commit.idem_key,
            &commit.op_hash,
            held_seq,
            Some(commit.hold_id.as_str()),
            "hold.capture",
            ts,
        )
        .await?;
        tx.commit().await.map_err(db_err)?;

        Ok(HoldOutcome {
            hold_id: commit.hold_id,
            first_seq: held_seq as u64,
            holder_balance,
            payee_balance: Some(payee_balance),
            status: HoldStatus::Captured,
            captured_micros: capture,
            idempotent_replay: false,
        })
    }

    async fn commit_hold_release(
        &self,
        commit: HoldReleaseCommit,
    ) -> Result<HoldOutcome, MintError> {
        let ts = self.clock.now();
        let mut tx = self.pool.begin().await.map_err(db_err)?;
        // Lock the hold row FOR UPDATE first so concurrent release/capture on the
        // same hold serialize here; the status guard below then reads fresh state
        // and the second operation sees status != Open -> HoldNotFound.
        let hold = self.get_hold(&mut tx, &commit.hold_id, true).await?;
        let holder = hold.account_id.clone();
        let _ = lock_balance(&mut tx, &holder).await?;

        match check_idem(&mut tx, &commit.idem_key, &commit.op_hash).await? {
            IdemCheck::Conflict => return Err(MintError::IdempotencyConflict),
            IdemCheck::Replay { first_seq, .. } => {
                let recs = self.op_receipts(&mut tx, &commit.op_hash).await?;
                let holder_rec = recs.first().cloned().ok_or(MintError::Overflow)?;
                return Ok(HoldOutcome {
                    hold_id: commit.hold_id.clone(),
                    first_seq: first_seq as u64,
                    holder_balance: Balance {
                        account_id: holder_rec.0,
                        available_micros: Micros::new(holder_rec.1),
                        held_micros: Micros::new(holder_rec.2),
                        updated_seq: holder_rec.3 as u64,
                    },
                    payee_balance: None,
                    status: HoldStatus::Released,
                    captured_micros: Micros::zero(),
                    idempotent_replay: true,
                });
            }
            IdemCheck::Fresh => {}
        }

        if hold.status != HoldStatus::Open {
            return Err(MintError::HoldNotFound);
        }
        let holder_bal = read_balance_row(&mut tx, &holder).await?;
        let hold_amt = hold.amount_micros;
        let new_held = holder_bal.held_micros.checked_sub(hold_amt)?;
        if new_held.is_negative() {
            return Err(MintError::InsufficientFunds);
        }
        let new_available = holder_bal.available_micros.checked_add(hold_amt)?;
        let neg_hold = hold_amt.checked_neg()?;
        let hold_ref = commit.hold_id.as_str().to_string();

        let held_seq = append_journal(
            &mut tx,
            OpKind::HoldRelease,
            &commit.op_hash,
            &commit.idem_key,
            &holder,
            None,
            neg_hold,
            Bucket::Held,
            Some(&hold_ref),
            ts,
        )
        .await?;
        let avail_seq = append_journal(
            &mut tx,
            OpKind::HoldRelease,
            &commit.op_hash,
            &commit.idem_key,
            &holder,
            None,
            hold_amt,
            Bucket::Available,
            Some(&hold_ref),
            ts,
        )
        .await?;

        let holder_balance = Balance {
            account_id: holder.clone(),
            available_micros: new_available,
            held_micros: new_held,
            updated_seq: avail_seq as u64,
        };
        write_balance(&mut tx, &holder_balance).await?;

        append_receipt(
            &mut tx,
            self.signer(),
            ReceiptDraft {
                account_id: holder.clone(),
                journal_seq: avail_seq as u64,
                op: ReceiptOp::HoldRelease,
                op_hash: commit.op_hash,
                amount_micros: hold_amt,
                from: Some(holder.clone()),
                to: None,
                balance_after_available_micros: holder_balance.available_micros,
                balance_after_held_micros: holder_balance.held_micros,
                ts,
            },
        )
        .await?;

        sqlx::query("UPDATE holds SET status = $1, closed_at = $2 WHERE id = $3")
            .bind(rows::hold_status_to_i16(HoldStatus::Released))
            .bind(ts)
            .bind(commit.hold_id.as_str())
            .execute(&mut *tx)
            .await
            .map_err(db_err)?;

        record_idem(
            &mut tx,
            &commit.idem_key,
            &commit.op_hash,
            held_seq,
            Some(commit.hold_id.as_str()),
            "hold.release",
            ts,
        )
        .await?;
        tx.commit().await.map_err(db_err)?;

        Ok(HoldOutcome {
            hold_id: commit.hold_id,
            first_seq: held_seq as u64,
            holder_balance,
            payee_balance: None,
            status: HoldStatus::Released,
            captured_micros: Micros::zero(),
            idempotent_replay: false,
        })
    }

    async fn commit_allowance_grant(
        &self,
        commit: AllowanceGrantCommit,
    ) -> Result<AllowanceGrantOutcome, MintError> {
        let now = self.clock.now();
        let mut tx = self.pool.begin().await.map_err(db_err)?;

        match check_idem(&mut tx, &commit.idem_key, &commit.op_hash).await? {
            IdemCheck::Conflict => return Err(MintError::IdempotencyConflict),
            IdemCheck::Replay { result_ref, .. } => {
                let id = AllowanceId::parse(&result_ref.ok_or(MintError::AllowanceNotFound)?)?;
                return Ok(AllowanceGrantOutcome {
                    allowance_id: id,
                    idempotent_replay: true,
                });
            }
            IdemCheck::Fresh => {}
        }

        if !account_exists(&mut tx, &commit.granter).await? {
            return Err(MintError::AccountNotFound);
        }
        let id = AllowanceId::new(Ulid::new());
        let (wkind, wms) = rows::window_to_cols(commit.window);
        let scope_json = commit
            .scope
            .as_ref()
            .map(serde_json::to_value)
            .transpose()
            .map_err(|_| MintError::Overflow)?;
        sqlx::query(
            "INSERT INTO allowances \
             (id, account_id, key_id, limit_micros, spent_micros, window_kind, window_ms, window_start, scope, revoked_at) \
             VALUES ($1,$2,$3,$4,0,$5,$6,$7,$8,NULL)",
        )
        .bind(id.as_str())
        .bind(commit.granter.as_str())
        .bind(&commit.to_key)
        .bind(commit.limit.micros().get())
        .bind(wkind)
        .bind(wms)
        .bind(now)
        .bind(scope_json)
        .execute(&mut *tx)
        .await
        .map_err(db_err)?;

        record_idem(
            &mut tx,
            &commit.idem_key,
            &commit.op_hash,
            0,
            Some(id.as_str()),
            "allowance.grant",
            now,
        )
        .await?;
        tx.commit().await.map_err(db_err)?;

        Ok(AllowanceGrantOutcome {
            allowance_id: id,
            idempotent_replay: false,
        })
    }

    async fn commit_allowance_spend(
        &self,
        commit: AllowanceSpendCommit,
    ) -> Result<AllowanceSpendOutcome, MintError> {
        let ts = self.clock.now();
        let mut tx = self.pool.begin().await.map_err(db_err)?;

        // Lock the allowance row FOR UPDATE (serializes concurrent spends).
        let locked = sqlx::query("SELECT 1 FROM allowances WHERE id = $1 FOR UPDATE")
            .bind(commit.allowance_id.as_str())
            .fetch_optional(&mut *tx)
            .await
            .map_err(db_err)?;
        if locked.is_none() {
            return Err(MintError::AllowanceNotFound);
        }

        match check_idem(&mut tx, &commit.idem_key, &commit.op_hash).await? {
            IdemCheck::Conflict => return Err(MintError::IdempotencyConflict),
            IdemCheck::Replay {
                first_seq,
                result_ref,
            } => {
                let rec = self
                    .get_allowance_row(&mut tx, &commit.allowance_id)
                    .await?;
                let remaining = rec.limit_micros.checked_sub(rec.spent_micros)?;
                let hold_id = match result_ref {
                    Some(r) => Some(HoldId::parse(&r)?),
                    None => None,
                };
                return Ok(AllowanceSpendOutcome {
                    allowance_id: commit.allowance_id.clone(),
                    remaining_micros: remaining,
                    settlement_seq: first_seq as u64,
                    hold_id,
                    idempotent_replay: true,
                });
            }
            IdemCheck::Fresh => {}
        }

        let mut record = self
            .get_allowance_row(&mut tx, &commit.allowance_id)
            .await?;
        if record.revoked {
            return Err(MintError::AllowanceExceeded);
        }
        if let AllowanceWindow::Rolling { window_ms } = record.window {
            if ts.saturating_sub(record.window_start_ms) >= window_ms {
                record.spent_micros = Micros::zero();
                record.window_start_ms = ts;
            }
        }
        if let Some(scope) = &record.scope {
            if let Some(payee) = &scope.payee {
                if payee != commit.to.as_str() {
                    return Err(MintError::AllowanceExceeded);
                }
            }
            if let Some(prefix) = &scope.resource_prefix {
                let matches = commit
                    .ref_
                    .as_deref()
                    .map(|r| r.starts_with(prefix.as_str()))
                    .unwrap_or(false);
                if !matches {
                    return Err(MintError::AllowanceExceeded);
                }
            }
        }
        let amount = commit.amount.micros();
        let new_spent = record.spent_micros.checked_add(amount)?;
        if new_spent.get() > record.limit_micros.get() {
            return Err(MintError::AllowanceExceeded);
        }

        let (settlement_seq, hold_id) = match commit.scheme {
            AllowanceScheme::Transfer => {
                let out = self
                    .settle_transfer(
                        &mut tx,
                        &record.granter,
                        &commit.to,
                        amount,
                        &commit.op_hash,
                        &commit.idem_key,
                        commit.ref_.as_deref(),
                        ts,
                    )
                    .await?;
                (out.first_seq, None)
            }
            AllowanceScheme::Hold => {
                let out = self
                    .settle_hold_create(
                        &mut tx,
                        &record.granter,
                        amount,
                        &commit.op_hash,
                        &commit.idem_key,
                        commit.ref_.as_deref(),
                        ts,
                    )
                    .await?;
                (out.first_seq, Some(out.hold_id))
            }
        };

        let remaining = record.limit_micros.checked_sub(new_spent)?;
        sqlx::query("UPDATE allowances SET spent_micros = $1, window_start = $2 WHERE id = $3")
            .bind(new_spent.get())
            .bind(record.window_start_ms)
            .bind(commit.allowance_id.as_str())
            .execute(&mut *tx)
            .await
            .map_err(db_err)?;

        record_idem(
            &mut tx,
            &commit.idem_key,
            &commit.op_hash,
            settlement_seq as i64,
            hold_id.as_ref().map(|h| h.as_str()),
            "allowance.spend",
            ts,
        )
        .await?;
        tx.commit().await.map_err(db_err)?;

        Ok(AllowanceSpendOutcome {
            allowance_id: commit.allowance_id,
            remaining_micros: remaining,
            settlement_seq,
            hold_id,
            idempotent_replay: false,
        })
    }

    async fn commit_allowance_revoke(
        &self,
        commit: AllowanceRevokeCommit,
    ) -> Result<AllowanceId, MintError> {
        let mut tx = self.pool.begin().await.map_err(db_err)?;
        let updated = sqlx::query("UPDATE allowances SET revoked_at = $1 WHERE id = $2")
            .bind(self.clock.now())
            .bind(commit.allowance_id.as_str())
            .execute(&mut *tx)
            .await
            .map_err(db_err)?;
        if updated.rows_affected() == 0 {
            return Err(MintError::AllowanceNotFound);
        }
        tx.commit().await.map_err(db_err)?;
        Ok(commit.allowance_id)
    }

    async fn get_allowance(&self, id: &AllowanceId) -> Result<AllowanceRecord, MintError> {
        let mut conn = self.pool.acquire().await.map_err(db_err)?;
        self.get_allowance_row(&mut conn, id).await
    }

    async fn commit_stream_open(
        &self,
        commit: StreamOpenCommit,
    ) -> Result<StreamOpenOutcome, MintError> {
        let ts = self.clock.now();
        let mut tx = self.pool.begin().await.map_err(db_err)?;
        let _ = self.lock_pair(&mut tx, &commit.from, &commit.to).await?;

        match check_idem(&mut tx, &commit.idem_key, &commit.op_hash).await? {
            IdemCheck::Conflict => return Err(MintError::IdempotencyConflict),
            IdemCheck::Replay {
                first_seq,
                result_ref,
            } => {
                let stream_id = StreamId::parse(&result_ref.ok_or(MintError::StreamNotFound)?)?;
                let recs = self.op_receipts(&mut tx, &commit.op_hash).await?;
                let holder = recs.first().cloned().ok_or(MintError::Overflow)?;
                return Ok(StreamOpenOutcome {
                    stream_id,
                    first_seq: first_seq as u64,
                    holder_balance: Balance {
                        account_id: holder.0,
                        available_micros: Micros::new(holder.1),
                        held_micros: Micros::new(holder.2),
                        updated_seq: holder.3 as u64,
                    },
                    idempotent_replay: true,
                });
            }
            IdemCheck::Fresh => {}
        }

        if commit.from == commit.to {
            return Err(MintError::BadAccountId);
        }
        if !account_exists(&mut tx, &commit.to).await? {
            return Err(MintError::AccountNotFound);
        }
        let from_bal = read_balance_row(&mut tx, &commit.from).await?;
        let cap = commit.cap.micros();
        let new_available = from_bal.available_micros.checked_sub(cap)?;
        if new_available.is_negative() {
            return Err(MintError::InsufficientFunds);
        }
        let new_held = from_bal.held_micros.checked_add(cap)?;
        let neg_cap = cap.checked_neg()?;
        let stream_id = StreamId::new(Ulid::new());
        let stream_ref = stream_id.as_str().to_string();

        let avail_seq = append_journal(
            &mut tx,
            OpKind::StreamOpen,
            &commit.op_hash,
            &commit.idem_key,
            &commit.from,
            Some(&commit.to),
            neg_cap,
            Bucket::Available,
            Some(&stream_ref),
            ts,
        )
        .await?;
        let held_seq = append_journal(
            &mut tx,
            OpKind::StreamOpen,
            &commit.op_hash,
            &commit.idem_key,
            &commit.from,
            Some(&commit.to),
            cap,
            Bucket::Held,
            Some(&stream_ref),
            ts,
        )
        .await?;

        let holder_balance = Balance {
            account_id: commit.from.clone(),
            available_micros: new_available,
            held_micros: new_held,
            updated_seq: held_seq as u64,
        };
        write_balance(&mut tx, &holder_balance).await?;

        sqlx::query(
            "INSERT INTO streams \
             (id, from_id, to_id, rate_micros, unit, cap_micros, settled_micros, last_seq, status, ref, created_at) \
             VALUES ($1,$2,$3,$4,$5,$6,0,0,$7,$8,$9)",
        )
        .bind(stream_id.as_str())
        .bind(commit.from.as_str())
        .bind(commit.to.as_str())
        .bind(commit.rate.micros().get())
        .bind(rows::stream_unit_to_i16(commit.unit))
        .bind(cap.get())
        .bind(rows::stream_status_to_i16(StreamStatus::Open))
        .bind(commit.ref_.as_deref())
        .bind(ts)
        .execute(&mut *tx)
        .await
        .map_err(db_err)?;

        append_receipt(
            &mut tx,
            self.signer(),
            ReceiptDraft {
                account_id: commit.from.clone(),
                journal_seq: held_seq as u64,
                op: ReceiptOp::StreamOpen,
                op_hash: commit.op_hash,
                amount_micros: cap,
                from: Some(commit.from.clone()),
                to: Some(commit.to.clone()),
                balance_after_available_micros: holder_balance.available_micros,
                balance_after_held_micros: holder_balance.held_micros,
                ts,
            },
        )
        .await?;

        record_idem(
            &mut tx,
            &commit.idem_key,
            &commit.op_hash,
            avail_seq,
            Some(stream_id.as_str()),
            "stream.open",
            ts,
        )
        .await?;
        tx.commit().await.map_err(db_err)?;

        Ok(StreamOpenOutcome {
            stream_id,
            first_seq: avail_seq as u64,
            holder_balance,
            idempotent_replay: false,
        })
    }

    async fn commit_stream_tick(
        &self,
        commit: StreamTickCommit,
    ) -> Result<StreamTickOutcome, MintError> {
        let ts = self.clock.now();
        let mut tx = self.pool.begin().await.map_err(db_err)?;
        // Lock the stream row FOR UPDATE first so concurrent ticks/close on the
        // same stream serialize here; the last_seq/settled/status guards below
        // then read fresh state and a duplicate seq is rejected (SeqRegressed).
        let stream = self
            .get_stream_row(&mut tx, &commit.stream_id, true)
            .await?;
        let from = stream.from_id.clone();
        let to = stream.to_id.clone();
        let _ = self.lock_pair(&mut tx, &from, &to).await?;

        match check_idem(&mut tx, &commit.idem_key, &commit.op_hash).await? {
            IdemCheck::Conflict => return Err(MintError::IdempotencyConflict),
            IdemCheck::Replay { .. } => {
                let cur = self
                    .get_stream_row(&mut tx, &commit.stream_id, false)
                    .await?;
                let recs = self.op_receipts(&mut tx, &commit.op_hash).await?;
                if recs.len() < 2 {
                    return Err(MintError::Overflow);
                }
                return Ok(StreamTickOutcome {
                    stream_id: commit.stream_id.clone(),
                    tick_seq: commit.seq,
                    settled_total_micros: cur.settled_micros,
                    from_balance: Balance {
                        account_id: recs[0].0.clone(),
                        available_micros: Micros::new(recs[0].1),
                        held_micros: Micros::new(recs[0].2),
                        updated_seq: recs[0].3 as u64,
                    },
                    to_balance: Balance {
                        account_id: recs[1].0.clone(),
                        available_micros: Micros::new(recs[1].1),
                        held_micros: Micros::new(recs[1].2),
                        updated_seq: recs[1].3 as u64,
                    },
                    idempotent_replay: true,
                });
            }
            IdemCheck::Fresh => {}
        }

        if stream.status != StreamStatus::Open {
            return Err(MintError::StreamClosed);
        }
        if commit.units == 0 {
            return Err(MintError::InvalidAmount);
        }
        if commit.seq <= stream.last_seq {
            return Err(MintError::StreamSeqRegressed);
        }
        let amount = tick_amount(commit.units, stream.rate_micros)?;
        let new_settled = stream.settled_micros.checked_add(amount)?;
        if new_settled.get() > stream.cap_micros.get() {
            return Err(MintError::StreamCapExceeded);
        }

        let from_bal = read_balance_row(&mut tx, &from).await?;
        let to_bal = read_balance_row(&mut tx, &to).await?;
        let new_from_held = from_bal.held_micros.checked_sub(amount)?;
        if new_from_held.is_negative() {
            return Err(MintError::InsufficientFunds);
        }
        let new_to_available = to_bal.available_micros.checked_add(amount)?;
        let neg = amount.checked_neg()?;
        let stream_ref = commit.stream_id.as_str().to_string();

        let held_seq = append_journal(
            &mut tx,
            OpKind::StreamTick,
            &commit.op_hash,
            &commit.idem_key,
            &from,
            Some(&to),
            neg,
            Bucket::Held,
            Some(&stream_ref),
            ts,
        )
        .await?;
        let avail_seq = append_journal(
            &mut tx,
            OpKind::StreamTick,
            &commit.op_hash,
            &commit.idem_key,
            &to,
            Some(&from),
            amount,
            Bucket::Available,
            Some(&stream_ref),
            ts,
        )
        .await?;

        let from_balance = Balance {
            account_id: from.clone(),
            available_micros: from_bal.available_micros,
            held_micros: new_from_held,
            updated_seq: held_seq as u64,
        };
        let to_balance = Balance {
            account_id: to.clone(),
            available_micros: new_to_available,
            held_micros: to_bal.held_micros,
            updated_seq: avail_seq as u64,
        };
        write_balance(&mut tx, &from_balance).await?;
        write_balance(&mut tx, &to_balance).await?;

        append_receipt(
            &mut tx,
            self.signer(),
            ReceiptDraft {
                account_id: from.clone(),
                journal_seq: held_seq as u64,
                op: ReceiptOp::StreamTick,
                op_hash: commit.op_hash,
                amount_micros: amount,
                from: Some(from.clone()),
                to: Some(to.clone()),
                balance_after_available_micros: from_balance.available_micros,
                balance_after_held_micros: from_balance.held_micros,
                ts,
            },
        )
        .await?;
        append_receipt(
            &mut tx,
            self.signer(),
            ReceiptDraft {
                account_id: to.clone(),
                journal_seq: avail_seq as u64,
                op: ReceiptOp::StreamTick,
                op_hash: commit.op_hash,
                amount_micros: amount,
                from: Some(from.clone()),
                to: Some(to.clone()),
                balance_after_available_micros: to_balance.available_micros,
                balance_after_held_micros: to_balance.held_micros,
                ts,
            },
        )
        .await?;

        sqlx::query("UPDATE streams SET settled_micros = $1, last_seq = $2 WHERE id = $3")
            .bind(new_settled.get())
            .bind(commit.seq as i64)
            .bind(commit.stream_id.as_str())
            .execute(&mut *tx)
            .await
            .map_err(db_err)?;

        record_idem(
            &mut tx,
            &commit.idem_key,
            &commit.op_hash,
            held_seq,
            None,
            "stream.tick",
            ts,
        )
        .await?;
        tx.commit().await.map_err(db_err)?;

        Ok(StreamTickOutcome {
            stream_id: commit.stream_id,
            tick_seq: commit.seq,
            settled_total_micros: new_settled,
            from_balance,
            to_balance,
            idempotent_replay: false,
        })
    }

    async fn commit_stream_close(
        &self,
        commit: StreamCloseCommit,
    ) -> Result<StreamCloseOutcome, MintError> {
        let ts = self.clock.now();
        let mut tx = self.pool.begin().await.map_err(db_err)?;
        // Lock the stream row FOR UPDATE first so a close racing a tick on the
        // same stream serializes here and re-evaluates status against fresh state.
        let stream = self
            .get_stream_row(&mut tx, &commit.stream_id, true)
            .await?;
        let from = stream.from_id.clone();
        let to = stream.to_id.clone();
        let _ = self.lock_pair(&mut tx, &from, &to).await?;

        match check_idem(&mut tx, &commit.idem_key, &commit.op_hash).await? {
            IdemCheck::Conflict => return Err(MintError::IdempotencyConflict),
            IdemCheck::Replay { .. } => {
                let cur = self
                    .get_stream_row(&mut tx, &commit.stream_id, false)
                    .await?;
                let from_balance = read_balance_row(&mut tx, &from).await?;
                let to_balance = read_balance_row(&mut tx, &to).await?;
                let released = cur.cap_micros.checked_sub(cur.settled_micros)?;
                return Ok(StreamCloseOutcome {
                    stream_id: commit.stream_id.clone(),
                    settled_total_micros: cur.settled_micros,
                    released_micros: released,
                    from_balance,
                    to_balance,
                    idempotent_replay: true,
                });
            }
            IdemCheck::Fresh => {}
        }

        if stream.status != StreamStatus::Open {
            return Err(MintError::StreamClosed);
        }
        let amount = if commit.units > 0 {
            if commit.seq <= stream.last_seq {
                return Err(MintError::StreamSeqRegressed);
            }
            tick_amount(commit.units, stream.rate_micros)?
        } else {
            Micros::zero()
        };
        let new_settled = stream.settled_micros.checked_add(amount)?;
        if new_settled.get() > stream.cap_micros.get() {
            return Err(MintError::StreamCapExceeded);
        }
        let remainder = stream.cap_micros.checked_sub(new_settled)?;
        let held_cleared = amount.checked_add(remainder)?;

        let from_bal = read_balance_row(&mut tx, &from).await?;
        let to_bal = read_balance_row(&mut tx, &to).await?;
        let new_from_held = from_bal.held_micros.checked_sub(held_cleared)?;
        if new_from_held.is_negative() {
            return Err(MintError::InsufficientFunds);
        }
        let new_from_available = from_bal.available_micros.checked_add(remainder)?;
        let new_to_available = to_bal.available_micros.checked_add(amount)?;
        let stream_ref = commit.stream_id.as_str().to_string();

        let mut payee_row_seq: Option<i64> = None;
        // Build legs in the fixed order the memory store uses.
        let mut legs: Vec<(AccountId, Micros, Bucket, bool)> = Vec::new(); // bool = is payee-avail
        if amount.is_positive() {
            legs.push((from.clone(), amount.checked_neg()?, Bucket::Held, false));
            legs.push((to.clone(), amount, Bucket::Available, true));
        }
        if remainder.is_positive() {
            legs.push((from.clone(), remainder.checked_neg()?, Bucket::Held, false));
            legs.push((from.clone(), remainder, Bucket::Available, false));
        }
        if legs.is_empty() {
            legs.push((from.clone(), Micros::zero(), Bucket::Held, false));
        }

        let mut seqs: Vec<i64> = Vec::with_capacity(legs.len());
        for (account, delta, bucket, is_payee) in &legs {
            let s = append_journal(
                &mut tx,
                OpKind::StreamClose,
                &commit.op_hash,
                &commit.idem_key,
                account,
                Some(&to),
                *delta,
                *bucket,
                Some(&stream_ref),
                ts,
            )
            .await?;
            if *is_payee {
                payee_row_seq = Some(s);
            }
            seqs.push(s);
        }
        let first_close_seq = seqs[0];

        let from_balance = Balance {
            account_id: from.clone(),
            available_micros: new_from_available,
            held_micros: new_from_held,
            updated_seq: *seqs.last().unwrap() as u64,
        };
        write_balance(&mut tx, &from_balance).await?;
        let to_balance = if amount.is_positive() {
            let b = Balance {
                account_id: to.clone(),
                available_micros: new_to_available,
                held_micros: to_bal.held_micros,
                updated_seq: payee_row_seq.expect("payee row seq set when amount > 0") as u64,
            };
            write_balance(&mut tx, &b).await?;
            b
        } else {
            to_bal
        };

        if amount.is_positive() {
            append_receipt(
                &mut tx,
                self.signer(),
                ReceiptDraft {
                    account_id: to.clone(),
                    journal_seq: payee_row_seq.expect("payee row seq set when amount > 0") as u64,
                    op: ReceiptOp::StreamClose,
                    op_hash: commit.op_hash,
                    amount_micros: amount,
                    from: Some(from.clone()),
                    to: Some(to.clone()),
                    balance_after_available_micros: to_balance.available_micros,
                    balance_after_held_micros: to_balance.held_micros,
                    ts,
                },
            )
            .await?;
        }
        append_receipt(
            &mut tx,
            self.signer(),
            ReceiptDraft {
                account_id: from.clone(),
                journal_seq: first_close_seq as u64,
                op: ReceiptOp::StreamClose,
                op_hash: commit.op_hash,
                amount_micros: held_cleared,
                from: Some(from.clone()),
                to: Some(to.clone()),
                balance_after_available_micros: from_balance.available_micros,
                balance_after_held_micros: from_balance.held_micros,
                ts,
            },
        )
        .await?;

        let last_seq = if commit.units > 0 {
            commit.seq as i64
        } else {
            stream.last_seq as i64
        };
        sqlx::query(
            "UPDATE streams SET settled_micros = $1, last_seq = $2, status = $3 WHERE id = $4",
        )
        .bind(new_settled.get())
        .bind(last_seq)
        .bind(rows::stream_status_to_i16(StreamStatus::Closed))
        .bind(commit.stream_id.as_str())
        .execute(&mut *tx)
        .await
        .map_err(db_err)?;

        record_idem(
            &mut tx,
            &commit.idem_key,
            &commit.op_hash,
            first_close_seq,
            None,
            "stream.close",
            ts,
        )
        .await?;
        tx.commit().await.map_err(db_err)?;

        Ok(StreamCloseOutcome {
            stream_id: commit.stream_id,
            settled_total_micros: new_settled,
            released_micros: remainder,
            from_balance,
            to_balance,
            idempotent_replay: false,
        })
    }

    async fn get_stream(&self, id: &StreamId) -> Result<StreamRecord, MintError> {
        let mut conn = self.pool.acquire().await.map_err(db_err)?;
        self.get_stream_row(&mut conn, id, false).await
    }

    async fn rebuild_streams(&self) -> Result<HashMap<StreamId, StreamRecord>, MintError> {
        rebuild::rebuild_streams(&self.pool).await
    }

    async fn commit_mint(&self, commit: MintCommit) -> Result<MintOutcome, MintError> {
        let ts = self.clock.now();
        let mut tx = self.pool.begin().await.map_err(db_err)?;
        let _ = lock_balance(&mut tx, &commit.to).await?;
        let reserve = self.lock_reserve(&mut tx).await?;

        match check_idem(&mut tx, &commit.idem_key, &commit.op_hash).await? {
            IdemCheck::Conflict => return Err(MintError::IdempotencyConflict),
            IdemCheck::Replay { first_seq, .. } => {
                let recs = self.op_receipts(&mut tx, &commit.op_hash).await?;
                let holder = recs.first().cloned().ok_or(MintError::Overflow)?;
                return Ok(MintOutcome {
                    first_seq: first_seq as u64,
                    to_balance: Balance {
                        account_id: holder.0,
                        available_micros: Micros::new(holder.1),
                        held_micros: Micros::new(holder.2),
                        updated_seq: holder.3 as u64,
                    },
                    reserve_after_micros: reserve,
                    idempotent_replay: true,
                });
            }
            IdemCheck::Fresh => {}
        }

        let to_bal = read_balance_row(&mut tx, &commit.to).await?;
        let amount = commit.amount.micros();
        let new_available = to_bal.available_micros.checked_add(amount)?;
        let new_reserve = reserve.checked_add(amount)?;

        let seq = append_journal(
            &mut tx,
            OpKind::Mint,
            &commit.op_hash,
            &commit.idem_key,
            &commit.to,
            None,
            amount,
            Bucket::Available,
            Some(&commit.deposit_ref),
            ts,
        )
        .await?;
        self.write_reserve(&mut tx, new_reserve).await?;

        let to_balance = Balance {
            account_id: commit.to.clone(),
            available_micros: new_available,
            held_micros: to_bal.held_micros,
            updated_seq: seq as u64,
        };
        write_balance(&mut tx, &to_balance).await?;

        append_receipt(
            &mut tx,
            self.signer(),
            ReceiptDraft {
                account_id: commit.to.clone(),
                journal_seq: seq as u64,
                op: ReceiptOp::Mint,
                op_hash: commit.op_hash,
                amount_micros: amount,
                from: None,
                to: Some(commit.to.clone()),
                balance_after_available_micros: to_balance.available_micros,
                balance_after_held_micros: to_balance.held_micros,
                ts,
            },
        )
        .await?;

        sqlx::query(
            "INSERT INTO reserve_ledger (kind, amount_micros, source, journal_seq, ts) \
             VALUES ('mint', $1, $2, $3, $4)",
        )
        .bind(amount.get())
        .bind(&commit.deposit_ref)
        .bind(seq)
        .bind(ts)
        .execute(&mut *tx)
        .await
        .map_err(db_err)?;

        record_idem(
            &mut tx,
            &commit.idem_key,
            &commit.op_hash,
            seq,
            None,
            "mint",
            ts,
        )
        .await?;
        tx.commit().await.map_err(db_err)?;

        Ok(MintOutcome {
            first_seq: seq as u64,
            to_balance,
            reserve_after_micros: new_reserve,
            idempotent_replay: false,
        })
    }

    async fn commit_redeem(&self, commit: RedeemCommit) -> Result<RedeemOutcome, MintError> {
        let ts = self.clock.now();
        let mut tx = self.pool.begin().await.map_err(db_err)?;
        let _ = lock_balance(&mut tx, &commit.from).await?;
        let reserve = self.lock_reserve(&mut tx).await?;

        match check_idem(&mut tx, &commit.idem_key, &commit.op_hash).await? {
            IdemCheck::Conflict => return Err(MintError::IdempotencyConflict),
            IdemCheck::Replay { first_seq, .. } => {
                let recs = self.op_receipts(&mut tx, &commit.op_hash).await?;
                let holder = recs.first().cloned().ok_or(MintError::Overflow)?;
                return Ok(RedeemOutcome {
                    first_seq: first_seq as u64,
                    from_balance: Balance {
                        account_id: holder.0,
                        available_micros: Micros::new(holder.1),
                        held_micros: Micros::new(holder.2),
                        updated_seq: holder.3 as u64,
                    },
                    reserve_after_micros: reserve,
                    idempotent_replay: true,
                });
            }
            IdemCheck::Fresh => {}
        }

        let from_bal = read_balance_row(&mut tx, &commit.from).await?;
        let amount = commit.amount.micros();
        let new_available = from_bal.available_micros.checked_sub(amount)?;
        if new_available.is_negative() {
            return Err(MintError::InsufficientFunds);
        }
        if reserve.get() < amount.get() {
            return Err(MintError::ReserveInsufficient);
        }
        let new_reserve = reserve.checked_sub(amount)?;
        let neg = amount.checked_neg()?;

        let seq = append_journal(
            &mut tx,
            OpKind::Redeem,
            &commit.op_hash,
            &commit.idem_key,
            &commit.from,
            None,
            neg,
            Bucket::Available,
            Some(&commit.payout_ref),
            ts,
        )
        .await?;
        self.write_reserve(&mut tx, new_reserve).await?;

        let from_balance = Balance {
            account_id: commit.from.clone(),
            available_micros: new_available,
            held_micros: from_bal.held_micros,
            updated_seq: seq as u64,
        };
        write_balance(&mut tx, &from_balance).await?;

        append_receipt(
            &mut tx,
            self.signer(),
            ReceiptDraft {
                account_id: commit.from.clone(),
                journal_seq: seq as u64,
                op: ReceiptOp::Redeem,
                op_hash: commit.op_hash,
                amount_micros: amount,
                from: Some(commit.from.clone()),
                to: None,
                balance_after_available_micros: from_balance.available_micros,
                balance_after_held_micros: from_balance.held_micros,
                ts,
            },
        )
        .await?;

        sqlx::query(
            "INSERT INTO reserve_ledger (kind, amount_micros, source, journal_seq, ts) \
             VALUES ('redeem', $1, $2, $3, $4)",
        )
        .bind(amount.get())
        .bind(&commit.payout_ref)
        .bind(seq)
        .bind(ts)
        .execute(&mut *tx)
        .await
        .map_err(db_err)?;

        record_idem(
            &mut tx,
            &commit.idem_key,
            &commit.op_hash,
            seq,
            None,
            "redeem",
            ts,
        )
        .await?;
        tx.commit().await.map_err(db_err)?;

        Ok(RedeemOutcome {
            first_seq: seq as u64,
            from_balance,
            reserve_after_micros: new_reserve,
            idempotent_replay: false,
        })
    }

    async fn reserve_micros(&self) -> Micros {
        let row = sqlx::query("SELECT total_micros FROM reserve WHERE id = 0")
            .fetch_optional(&self.pool)
            .await;
        match row {
            Ok(Some(r)) => Micros::new(r.get::<i64, _>("total_micros")),
            _ => Micros::zero(),
        }
    }

    async fn get_balance(&self, account_id: &AccountId) -> Result<Balance, MintError> {
        let mut conn = self.pool.acquire().await.map_err(db_err)?;
        read_balance_row(&mut conn, account_id).await
    }

    async fn rebuild_balances(&self) -> Result<HashMap<AccountId, Balance>, MintError> {
        rebuild::rebuild_balances(&self.pool).await
    }

    async fn rebuild_holds(&self) -> Result<HashMap<HoldId, HoldRecord>, MintError> {
        rebuild::rebuild_holds(&self.pool).await
    }

    async fn journal_len(&self) -> u64 {
        sqlx::query("SELECT count(*) AS n FROM journal")
            .fetch_one(&self.pool)
            .await
            .map(|r| r.get::<i64, _>("n") as u64)
            .unwrap_or(0)
    }

    async fn rebuild_receipts(&self) -> Result<ReceiptLog, MintError> {
        rebuild::rebuild_receipts(&self.pool, self.signer()).await
    }

    async fn snapshot_receipts(&self) -> Vec<Receipt> {
        let rows = sqlx::query(
            "SELECT account_id, seq, journal_seq, op, op_hash, amount_micros, from_id, to_id, \
                    balance_after_micros, balance_after_held, ts, prev_hash, hash, sig, signer \
             FROM receipts ORDER BY global_seq ASC",
        )
        .fetch_all(&self.pool)
        .await;
        let rows = match rows {
            Ok(r) => r,
            Err(_) => return Vec::new(),
        };
        let mut out = Vec::with_capacity(rows.len());
        for r in rows {
            let account_id = match AccountId::parse(&r.get::<String, _>("account_id")) {
                Ok(a) => a,
                Err(_) => continue,
            };
            let from = r
                .get::<Option<String>, _>("from_id")
                .and_then(|s| AccountId::parse(&s).ok());
            let to = r
                .get::<Option<String>, _>("to_id")
                .and_then(|s| AccountId::parse(&s).ok());
            let body = ReceiptBody {
                account_id,
                account_seq: r.get::<i64, _>("seq") as u64,
                journal_seq: r.get::<i64, _>("journal_seq") as u64,
                op: rows::receipt_op_from_text(&r.get::<String, _>("op")),
                op_hash: rows::ophash_from_slice(&r.get::<Vec<u8>, _>("op_hash")),
                amount_micros: Micros::new(r.get::<i64, _>("amount_micros")),
                from,
                to,
                balance_after_available_micros: Micros::new(
                    r.get::<i64, _>("balance_after_micros"),
                ),
                balance_after_held_micros: Micros::new(r.get::<i64, _>("balance_after_held")),
                ts: r.get::<i64, _>("ts"),
                prev_hash: rows::hash_from_slice(&r.get::<Vec<u8>, _>("prev_hash")),
                signer: mint_receipts::SignerKeyId::new(r.get::<String, _>("signer")),
            };
            out.push(Receipt {
                body,
                hash: rows::hash_from_slice(&r.get::<Vec<u8>, _>("hash")),
                sig: rows::sig_from_slice(&r.get::<Vec<u8>, _>("sig")),
            });
        }
        out
    }

    async fn receipt_merkle_root(&self) -> ReceiptHash {
        merkle_root(&self.snapshot_receipts().await)
    }
}

/// `units * rate` in microdollars, checked (mirror of the memory store helper).
fn tick_amount(units: u64, rate: Micros) -> Result<Micros, MintError> {
    let u = i64::try_from(units).map_err(|_| MintError::Overflow)?;
    u.checked_mul(rate.get())
        .map(Micros::new)
        .ok_or(MintError::Overflow)
}
