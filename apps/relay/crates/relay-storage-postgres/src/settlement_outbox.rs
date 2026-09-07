//! Restart-durable Relay -> Mint billing state machine.
//!
//! The exact reserve and terminal commands land in Postgres before Mint sees
//! them. Recovery replays those immutable commands under the same idempotency
//! keys; it never invokes TensorZero inference.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, RwLock};

use async_trait::async_trait;
use relay_billing::aitp_client::PaymentAuthorization;
use relay_billing::{
    AccountId, BillingError, BillingLedger, JobId, Micros, ModelId, Reservation,
    TerminalSettlement, Usage,
};
use relay_contract::{Microdollars, Usage as ContractUsage};
use relay_cost_feed::{CostEvent, InferenceRoute};
use serde::{Deserialize, Serialize};
use sqlx::{PgPool, Row};
use uuid::Uuid;

const ACTIVE_REQUEST_LEASE_SECONDS: i64 = 3_600;
const RETRY_DELAY_SECONDS: i64 = 5;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[expect(
    clippy::large_enum_variant,
    reason = "the established durable JSON schema keeps authorization inline"
)]
pub enum ReservePayload {
    Direct,
    Authorized { authorization: PaymentAuthorization },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case")]
pub enum TerminalPayload {
    Capture {
        usage: Usage,
        model: ModelId,
        #[serde(default)]
        route: InferenceRoute,
    },
    Release,
}

impl TerminalPayload {
    fn operation(&self) -> &'static str {
        match self {
            Self::Capture { .. } => "capture",
            Self::Release => "release",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CommandState {
    ReservePending,
    Held,
    InferenceStarted,
    BatchPending,
    TerminalPending,
    Completed,
}

impl CommandState {
    fn parse(value: &str) -> Result<Self, BillingError> {
        match value {
            "reserve_pending" => Ok(Self::ReservePending),
            "held" => Ok(Self::Held),
            "inference_started" => Ok(Self::InferenceStarted),
            "batch_pending" => Ok(Self::BatchPending),
            "terminal_pending" => Ok(Self::TerminalPending),
            "completed" => Ok(Self::Completed),
            other => Err(BillingError::Store(format!(
                "unknown Relay billing outbox state '{other}'"
            ))),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct BillingCommand {
    owner_principal: AccountId,
    payer_account: AccountId,
    payee_account: Option<AccountId>,
    request_id: JobId,
    estimate_micros: Micros,
    held_amount_micros: Micros,
    reserve_idem_key: String,
    reserve_payload: ReservePayload,
    hold_id: Option<String>,
    state: CommandState,
    terminal_idem_key: Option<String>,
    terminal_payload: Option<TerminalPayload>,
    lease_owner: Option<Uuid>,
    batch_id: Option<Uuid>,
    billing_model: Option<ModelId>,
}

impl BillingCommand {
    fn reservation(&self) -> Reservation {
        Reservation {
            job_id: self.request_id,
            owner_principal: self.owner_principal.clone(),
            account_id: self.payer_account.clone(),
            estimate_micros: self.estimate_micros,
            held_amount_micros: self.held_amount_micros,
            backend_reference: self.hold_id.clone(),
            settlement_payee: self.payee_account.clone(),
            execution_lease: self.lease_owner,
        }
    }
}

struct ReserveStart {
    command: BillingCommand,
    created: bool,
}

/// Result of trying to become the sole reconciler for one paid batch.
pub enum BatchClaim {
    /// This caller owns the short-lived poll lease and may invoke TensorZero.
    Acquired {
        reservation: Reservation,
        model: ModelId,
    },
    /// Another request currently owns the poll lease.
    Busy,
    /// The authenticated principal does not own this batch id.
    NotFound,
    /// Capture/release is durably selected but Mint has not acknowledged it.
    Settling,
    /// The batch already selected capture/release (or finished it); polling may
    /// pass through but must not create another terminal charge.
    Terminal,
}

enum StoredBatchClaim {
    Acquired(Box<BillingCommand>),
    Busy,
    NotFound,
}

#[async_trait]
trait SettlementCommandStore: Send + Sync {
    async fn begin_reserve(&self, command: &BillingCommand) -> Result<ReserveStart, BillingError>;
    async fn mark_held(&self, reservation: &Reservation) -> Result<(), BillingError>;
    async fn cancel_unheld(&self, command: &BillingCommand) -> Result<(), BillingError>;
    async fn mark_inference_started(&self, reservation: &Reservation) -> Result<(), BillingError>;
    async fn renew_inference_lease(&self, reservation: &Reservation) -> Result<(), BillingError>;
    async fn mark_batch_pending(
        &self,
        reservation: &Reservation,
        batch_id: Uuid,
        model: &ModelId,
    ) -> Result<(), BillingError>;
    async fn claim_batch(
        &self,
        owner: &AccountId,
        batch_id: Uuid,
        worker: Uuid,
    ) -> Result<StoredBatchClaim, BillingError>;
    async fn release_batch_poll(&self, reservation: &Reservation) -> Result<(), BillingError>;
    async fn stage_terminal(
        &self,
        reservation: &Reservation,
        payload: &TerminalPayload,
    ) -> Result<BillingCommand, BillingError>;
    async fn complete(
        &self,
        command: &BillingCommand,
        settlement: &TerminalSettlement,
    ) -> Result<(), BillingError>;
    async fn retry(
        &self,
        command: &BillingCommand,
        error: &BillingError,
    ) -> Result<(), BillingError>;
    async fn claim_due(
        &self,
        worker: Uuid,
        limit: i64,
    ) -> Result<Vec<BillingCommand>, BillingError>;
}

#[derive(Clone)]
pub struct DurableBillingCoordinator {
    ledger: Arc<dyn BillingLedger>,
    store: Arc<dyn SettlementCommandStore>,
}

impl DurableBillingCoordinator {
    pub fn postgres(ledger: Arc<dyn BillingLedger>, pool: PgPool) -> Self {
        Self {
            ledger,
            store: Arc::new(PostgresSettlementCommandStore { pool }),
        }
    }

    pub fn in_memory(ledger: Arc<dyn BillingLedger>) -> Self {
        Self {
            ledger,
            store: Arc::new(InMemorySettlementCommandStore::default()),
        }
    }

    pub async fn reserve(
        &self,
        owner: &AccountId,
        estimate: Micros,
        job: JobId,
    ) -> Result<Reservation, BillingError> {
        let payer = self.ledger.resolve_payer(owner);
        self.reserve_inner(owner, &payer, estimate, job, ReservePayload::Direct)
            .await
    }

    pub async fn reserve_authorized(
        &self,
        owner: &AccountId,
        authorization: &PaymentAuthorization,
        estimate: Micros,
        job: JobId,
    ) -> Result<Reservation, BillingError> {
        let payer = AccountId::new(authorization.payer.clone());
        self.reserve_inner(
            owner,
            &payer,
            estimate,
            job,
            ReservePayload::Authorized {
                authorization: authorization.clone(),
            },
        )
        .await
    }

    async fn reserve_inner(
        &self,
        owner: &AccountId,
        payer: &AccountId,
        estimate: Micros,
        job: JobId,
        payload: ReservePayload,
    ) -> Result<Reservation, BillingError> {
        let reserve_idem_key = match &payload {
            ReservePayload::Direct => format!("relay:{payer}:{job}"),
            ReservePayload::Authorized { authorization } => format!(
                "aitp:{}:{}:{}",
                authorization.payer, authorization.nonce, authorization.idempotency_key
            ),
        };
        let command = BillingCommand {
            owner_principal: owner.clone(),
            payer_account: payer.clone(),
            payee_account: self.ledger.settlement_payee(),
            request_id: job,
            estimate_micros: estimate,
            held_amount_micros: match &payload {
                ReservePayload::Direct => estimate,
                ReservePayload::Authorized { authorization } => {
                    Micros::new(authorization.amount_micros.get())
                }
            },
            reserve_idem_key,
            reserve_payload: payload,
            hold_id: None,
            state: CommandState::ReservePending,
            terminal_idem_key: None,
            terminal_payload: None,
            lease_owner: Some(Uuid::now_v7()),
            batch_id: None,
            billing_model: None,
        };
        let start = self.store.begin_reserve(&command).await?;
        if !start.created {
            return Err(BillingError::Store(
                "request id is already owned by another execution; refusing duplicate inference"
                    .into(),
            ));
        }
        let existing = start.command;

        let reserve_result = match &existing.reserve_payload {
            ReservePayload::Direct => {
                self.ledger
                    .reserve_resolved(owner, payer, estimate, job, &existing.reserve_idem_key)
                    .await
            }
            ReservePayload::Authorized { authorization } => {
                self.ledger
                    .reserve_authorized(owner, authorization, estimate, job)
                    .await
            }
        };
        let mut reservation = match reserve_result {
            Ok(reservation) => reservation,
            Err(error @ BillingError::InsufficientCredits { .. }) => {
                self.store.cancel_unheld(&existing).await?;
                return Err(error);
            }
            Err(error) => {
                self.store.retry(&existing, &error).await?;
                return Err(error);
            }
        };
        reservation.owner_principal = owner.clone();
        reservation.account_id = payer.clone();
        reservation.held_amount_micros = existing.held_amount_micros;
        // Recovery must use the payee committed before the first Mint call,
        // even if process configuration changed while the request was pending.
        reservation.settlement_payee = existing.payee_account.clone();
        reservation.execution_lease = existing.lease_owner;
        self.store.mark_held(&reservation).await?;
        Ok(reservation)
    }

    pub async fn mark_inference_started(
        &self,
        reservation: &Reservation,
    ) -> Result<(), BillingError> {
        self.store.mark_inference_started(reservation).await
    }

    pub async fn renew_inference_lease(
        &self,
        reservation: &Reservation,
    ) -> Result<(), BillingError> {
        self.store.renew_inference_lease(reservation).await
    }

    /// Persist the provider batch id before returning the accepted response.
    /// `batch_pending` deliberately has no recovery lease: the provider owns
    /// the asynchronous work and a later whole-batch poll chooses the terminal
    /// settlement direction.
    pub async fn mark_batch_pending(
        &self,
        reservation: &Reservation,
        batch_id: Uuid,
        model: &ModelId,
    ) -> Result<(), BillingError> {
        self.store
            .mark_batch_pending(reservation, batch_id, model)
            .await
    }

    /// Acquire exclusive ownership for one provider poll/reconciliation pass.
    pub async fn claim_batch(
        &self,
        owner: &AccountId,
        batch_id: Uuid,
    ) -> Result<BatchClaim, BillingError> {
        let command = self
            .store
            .claim_batch(owner, batch_id, Uuid::now_v7())
            .await?;
        let command = match command {
            StoredBatchClaim::Acquired(command) => *command,
            StoredBatchClaim::Busy => return Ok(BatchClaim::Busy),
            StoredBatchClaim::NotFound => return Ok(BatchClaim::NotFound),
        };
        if command.state == CommandState::TerminalPending {
            return Ok(BatchClaim::Settling);
        }
        if command.state != CommandState::BatchPending {
            return Ok(BatchClaim::Terminal);
        }
        let model = command.billing_model.clone().ok_or_else(|| {
            BillingError::Store("paid batch is missing its immutable billing model".into())
        })?;
        Ok(BatchClaim::Acquired {
            reservation: command.reservation(),
            model,
        })
    }

    /// Yield a poll lease when the provider still reports `pending`.
    pub async fn release_batch_poll(&self, reservation: &Reservation) -> Result<(), BillingError> {
        self.store.release_batch_poll(reservation).await
    }

    pub async fn settle(
        &self,
        reservation: &Reservation,
        usage: Usage,
        model: ModelId,
        route: InferenceRoute,
    ) -> Result<TerminalSettlement, BillingError> {
        let payload = TerminalPayload::Capture {
            usage,
            model,
            route,
        };
        let command = self.store.stage_terminal(reservation, &payload).await?;
        self.dispatch(command).await
    }

    /// Commit the immutable capture direction without requiring Mint to be
    /// reachable. Streaming uses this barrier before exposing terminal success.
    pub async fn stage_settle(
        &self,
        reservation: &Reservation,
        usage: Usage,
        model: ModelId,
        route: InferenceRoute,
    ) -> Result<(), BillingError> {
        self.store
            .stage_terminal(
                reservation,
                &TerminalPayload::Capture {
                    usage,
                    model,
                    route,
                },
            )
            .await
            .map(|_| ())
    }

    pub async fn release(&self, reservation: &Reservation) -> Result<(), BillingError> {
        let command = self
            .store
            .stage_terminal(reservation, &TerminalPayload::Release)
            .await?;
        self.dispatch(command).await.map(|_| ())
    }

    /// Commit the immutable release direction before best-effort Mint replay.
    pub async fn stage_release(&self, reservation: &Reservation) -> Result<(), BillingError> {
        self.store
            .stage_terminal(reservation, &TerminalPayload::Release)
            .await
            .map(|_| ())
    }

    async fn dispatch(&self, command: BillingCommand) -> Result<TerminalSettlement, BillingError> {
        let reservation = command.reservation();
        let result = match command.terminal_payload.clone() {
            Some(TerminalPayload::Capture {
                usage,
                model,
                route: _,
            }) => {
                self.ledger
                    .settle_with_evidence(&reservation, usage, model)
                    .await
            }
            Some(TerminalPayload::Release) => self.ledger.release_with_evidence(&reservation).await,
            None => Err(BillingError::Store(
                "attempted to dispatch a billing command without a terminal operation".into(),
            )),
        };
        match result {
            Ok(settlement) => {
                self.store.complete(&command, &settlement).await?;
                Ok(settlement)
            }
            Err(error) => {
                self.store.retry(&command, &error).await?;
                Err(error)
            }
        }
    }

    /// Replay stale commands without ever invoking TensorZero inference.
    pub async fn recover_once(&self, limit: i64) -> Result<usize, BillingError> {
        let worker = Uuid::now_v7();
        let commands = self.store.claim_due(worker, limit).await?;
        let count = commands.len();
        for command in commands {
            match command.state {
                CommandState::ReservePending => {
                    let reserve = match &command.reserve_payload {
                        ReservePayload::Direct => {
                            self.ledger
                                .reserve_resolved(
                                    &command.owner_principal,
                                    &command.payer_account,
                                    command.estimate_micros,
                                    command.request_id,
                                    &command.reserve_idem_key,
                                )
                                .await
                        }
                        ReservePayload::Authorized { authorization } => {
                            self.ledger
                                .reserve_authorized(
                                    &command.owner_principal,
                                    authorization,
                                    command.estimate_micros,
                                    command.request_id,
                                )
                                .await
                        }
                    };
                    match reserve {
                        Ok(mut reservation) => {
                            reservation.owner_principal = command.owner_principal.clone();
                            reservation.account_id = command.payer_account.clone();
                            reservation.held_amount_micros = command.held_amount_micros;
                            reservation.settlement_payee = command.payee_account.clone();
                            reservation.execution_lease = command.lease_owner;
                            self.store.mark_held(&reservation).await?;
                            if let Err(error) = self.release(&reservation).await {
                                tracing::error!(error = %error, request_id = %command.request_id, "Relay reserve recovery release failed");
                            }
                        }
                        Err(BillingError::InsufficientCredits { .. }) => {
                            self.store.cancel_unheld(&command).await?;
                        }
                        Err(error) => self.store.retry(&command, &error).await?,
                    }
                }
                CommandState::Held | CommandState::InferenceStarted => {
                    let reservation = command.reservation();
                    if let Err(error) = self.release(&reservation).await {
                        tracing::error!(error = %error, request_id = %command.request_id, "Relay orphan hold release failed");
                    }
                }
                // A provider batch is durable work, not an orphaned HTTP
                // execution. It is reconciled by an authenticated whole-batch
                // poll and must never be refunded by generic request recovery.
                CommandState::BatchPending => {}
                CommandState::TerminalPending => {
                    if let Err(error) = self.dispatch(command).await {
                        tracing::error!(error = %error, "Relay terminal settlement replay failed");
                    }
                }
                CommandState::Completed => {}
            }
        }
        Ok(count)
    }
}

#[derive(Clone)]
struct PostgresSettlementCommandStore {
    pool: PgPool,
}

impl PostgresSettlementCommandStore {
    async fn load(
        &self,
        owner: &AccountId,
        request: JobId,
    ) -> Result<BillingCommand, BillingError> {
        let row = sqlx::query(
            "SELECT owner_principal, payer_account, payee_account, request_id, estimate_micros, held_amount_micros, \
             reserve_idem_key, reserve_payload, hold_id, state, terminal_idem_key, \
             terminal_payload, lease_owner, batch_id, billing_model FROM relay_billing_outbox \
             WHERE owner_principal = $1 AND request_id = $2",
        )
        .bind(owner.as_str())
        .bind(request.as_uuid())
        .fetch_optional(&self.pool)
        .await
        .map_err(db_error)?
        .ok_or_else(|| BillingError::Store("Relay billing outbox row disappeared".into()))?;
        decode_row(&row)
    }
}

#[async_trait]
impl SettlementCommandStore for PostgresSettlementCommandStore {
    async fn begin_reserve(&self, command: &BillingCommand) -> Result<ReserveStart, BillingError> {
        let payload = serde_json::to_value(&command.reserve_payload)
            .map_err(|error| BillingError::Store(format!("encode reserve command: {error}")))?;
        let inserted = sqlx::query(
            "INSERT INTO relay_billing_outbox \
             (owner_principal, payer_account, payee_account, request_id, estimate_micros, held_amount_micros, reserve_idem_key, reserve_payload, state, available_at, lease_owner, lease_until) \
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,'reserve_pending',NOW(),$9,NOW() + make_interval(secs => $10)) \
             ON CONFLICT (owner_principal, request_id) DO NOTHING",
        )
        .bind(command.owner_principal.as_str())
        .bind(command.payer_account.as_str())
        .bind(command.payee_account.as_ref().map(AccountId::as_str))
        .bind(command.request_id.as_uuid())
        .bind(command.estimate_micros.get())
        .bind(command.held_amount_micros.get())
        .bind(&command.reserve_idem_key)
        .bind(payload)
        .bind(command.lease_owner)
        .bind(ACTIVE_REQUEST_LEASE_SECONDS as f64)
        .execute(&self.pool)
        .await
        .map_err(db_error)?;
        let stored = self
            .load(&command.owner_principal, command.request_id)
            .await?;
        if stored.owner_principal != command.owner_principal
            || stored.payer_account != command.payer_account
            || stored.payee_account != command.payee_account
            || stored.estimate_micros != command.estimate_micros
            || stored.held_amount_micros != command.held_amount_micros
            || stored.reserve_idem_key != command.reserve_idem_key
            || stored.reserve_payload != command.reserve_payload
        {
            return Err(BillingError::Store(
                "conflicting immutable reserve command for principal/request".into(),
            ));
        }
        Ok(ReserveStart {
            command: stored,
            created: inserted.rows_affected() == 1,
        })
    }

    async fn mark_held(&self, reservation: &Reservation) -> Result<(), BillingError> {
        let hold_id = reservation.backend_reference.as_deref().ok_or_else(|| {
            BillingError::Store("Mint reservation returned no durable hold id".into())
        })?;
        let result = sqlx::query(
            "UPDATE relay_billing_outbox SET hold_id=$3, payee_account=$4, state='held', updated_at=NOW(), \
             lease_until=NOW() + make_interval(secs => $5), last_error=NULL \
             WHERE owner_principal=$1 AND request_id=$2 \
             AND (hold_id IS NULL OR hold_id=$3) \
             AND payee_account IS NOT DISTINCT FROM $4 \
             AND lease_owner=$6 \
             AND state IN ('reserve_pending','held')",
        )
        .bind(reservation.owner_principal.as_str())
        .bind(reservation.job_id.as_uuid())
        .bind(hold_id)
        .bind(
            reservation
                .settlement_payee
                .as_ref()
                .map(AccountId::as_str),
        )
        .bind(ACTIVE_REQUEST_LEASE_SECONDS as f64)
        .bind(reservation.execution_lease)
        .execute(&self.pool)
        .await
        .map_err(db_error)?;
        require_one(result.rows_affected(), "mark durable Mint hold")
    }

    async fn cancel_unheld(&self, command: &BillingCommand) -> Result<(), BillingError> {
        sqlx::query(
            "DELETE FROM relay_billing_outbox WHERE owner_principal=$1 AND request_id=$2 \
             AND state='reserve_pending' AND hold_id IS NULL AND lease_owner=$3",
        )
        .bind(command.owner_principal.as_str())
        .bind(command.request_id.as_uuid())
        .bind(command.lease_owner)
        .execute(&self.pool)
        .await
        .map_err(db_error)?;
        Ok(())
    }

    async fn mark_inference_started(&self, reservation: &Reservation) -> Result<(), BillingError> {
        let result = sqlx::query(
            "UPDATE relay_billing_outbox SET state='inference_started', updated_at=NOW(), \
             lease_until=NOW() + make_interval(secs => $3) \
             WHERE owner_principal=$1 AND request_id=$2 AND state='held' AND lease_owner=$4",
        )
        .bind(reservation.owner_principal.as_str())
        .bind(reservation.job_id.as_uuid())
        .bind(ACTIVE_REQUEST_LEASE_SECONDS as f64)
        .bind(reservation.execution_lease)
        .execute(&self.pool)
        .await
        .map_err(db_error)?;
        require_one(result.rows_affected(), "mark inference started")
    }

    async fn renew_inference_lease(&self, reservation: &Reservation) -> Result<(), BillingError> {
        let result = sqlx::query(
            "UPDATE relay_billing_outbox SET lease_until=NOW() + make_interval(secs => $3), updated_at=NOW() \
             WHERE owner_principal=$1 AND request_id=$2 AND state='inference_started' AND lease_owner=$4",
        )
        .bind(reservation.owner_principal.as_str())
        .bind(reservation.job_id.as_uuid())
        .bind(ACTIVE_REQUEST_LEASE_SECONDS as f64)
        .bind(reservation.execution_lease)
        .execute(&self.pool)
        .await
        .map_err(db_error)?;
        require_one(result.rows_affected(), "renew inference execution lease")
    }

    async fn mark_batch_pending(
        &self,
        reservation: &Reservation,
        batch_id: Uuid,
        model: &ModelId,
    ) -> Result<(), BillingError> {
        let result = sqlx::query(
            "UPDATE relay_billing_outbox SET state='batch_pending', batch_id=$3, billing_model=$4, \
             lease_owner=NULL, lease_until=NULL, updated_at=NOW(), last_error=NULL \
             WHERE owner_principal=$1 AND request_id=$2 AND state='inference_started' \
             AND lease_owner=$5 AND batch_id IS NULL AND billing_model IS NULL",
        )
        .bind(reservation.owner_principal.as_str())
        .bind(reservation.job_id.as_uuid())
        .bind(batch_id)
        .bind(model.as_str())
        .bind(reservation.execution_lease)
        .execute(&self.pool)
        .await
        .map_err(db_error)?;
        require_one(result.rows_affected(), "mark paid batch pending")
    }

    async fn claim_batch(
        &self,
        owner: &AccountId,
        batch_id: Uuid,
        worker: Uuid,
    ) -> Result<StoredBatchClaim, BillingError> {
        let row = sqlx::query(
            "UPDATE relay_billing_outbox SET lease_owner=$3, \
             lease_until=NOW()+make_interval(secs => $4), updated_at=NOW() \
             WHERE owner_principal=$1 AND batch_id=$2 AND state='batch_pending' \
             AND (lease_until IS NULL OR lease_until <= NOW()) \
             RETURNING owner_principal,payer_account,payee_account,request_id,estimate_micros,held_amount_micros, \
             reserve_idem_key,reserve_payload,hold_id,state,terminal_idem_key,terminal_payload,lease_owner,batch_id,billing_model",
        )
        .bind(owner.as_str())
        .bind(batch_id)
        .bind(worker)
        .bind(ACTIVE_REQUEST_LEASE_SECONDS as f64)
        .fetch_optional(&self.pool)
        .await
        .map_err(db_error)?;
        if let Some(row) = row {
            return decode_row(&row)
                .map(Box::new)
                .map(StoredBatchClaim::Acquired);
        }

        let row = sqlx::query(
            "SELECT owner_principal,payer_account,payee_account,request_id,estimate_micros,held_amount_micros, \
             reserve_idem_key,reserve_payload,hold_id,state,terminal_idem_key,terminal_payload,lease_owner,batch_id,billing_model \
             FROM relay_billing_outbox WHERE owner_principal=$1 AND batch_id=$2",
        )
        .bind(owner.as_str())
        .bind(batch_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(db_error)?;
        let Some(row) = row else {
            return Ok(StoredBatchClaim::NotFound);
        };
        let command = decode_row(&row)?;
        if command.state == CommandState::BatchPending {
            Ok(StoredBatchClaim::Busy)
        } else {
            Ok(StoredBatchClaim::Acquired(Box::new(command)))
        }
    }

    async fn release_batch_poll(&self, reservation: &Reservation) -> Result<(), BillingError> {
        let result = sqlx::query(
            "UPDATE relay_billing_outbox SET lease_owner=NULL, lease_until=NULL, updated_at=NOW() \
             WHERE owner_principal=$1 AND request_id=$2 AND state='batch_pending' AND lease_owner=$3",
        )
        .bind(reservation.owner_principal.as_str())
        .bind(reservation.job_id.as_uuid())
        .bind(reservation.execution_lease)
        .execute(&self.pool)
        .await
        .map_err(db_error)?;
        require_one(result.rows_affected(), "release paid batch poll lease")
    }

    async fn stage_terminal(
        &self,
        reservation: &Reservation,
        payload: &TerminalPayload,
    ) -> Result<BillingCommand, BillingError> {
        let encoded = serde_json::to_value(payload)
            .map_err(|error| BillingError::Store(format!("encode terminal command: {error}")))?;
        let operation = payload.operation();
        let idem = format!(
            "relay:{}:{}:{operation}",
            reservation.account_id, reservation.job_id
        );
        sqlx::query(
            "UPDATE relay_billing_outbox SET terminal_operation=$3, terminal_idem_key=$4, \
             terminal_payload=$5, state='terminal_pending', available_at=NOW(), \
             lease_owner=NULL, lease_until=NULL, updated_at=NOW() \
             WHERE owner_principal=$1 AND request_id=$2 \
             AND ((state IN ('held','inference_started','batch_pending') AND lease_owner=$6) OR state='terminal_pending') \
             AND (terminal_payload IS NULL OR terminal_payload=$5)",
        )
        .bind(reservation.owner_principal.as_str())
        .bind(reservation.job_id.as_uuid())
        .bind(operation)
        .bind(idem)
        .bind(encoded)
        .bind(reservation.execution_lease)
        .execute(&self.pool)
        .await
        .map_err(db_error)?;
        let stored = self
            .load(&reservation.owner_principal, reservation.job_id)
            .await?;
        if stored.terminal_payload.as_ref() != Some(payload) {
            return Err(BillingError::Store(
                "conflicting terminal command; capture/release is immutable once selected".into(),
            ));
        }
        Ok(stored)
    }

    async fn complete(
        &self,
        command: &BillingCommand,
        settlement: &TerminalSettlement,
    ) -> Result<(), BillingError> {
        let mut tx = self.pool.begin().await.map_err(db_error)?;
        let receipt_json = settlement
            .receipt
            .as_ref()
            .map(serde_json::to_value)
            .transpose()
            .map_err(|error| BillingError::Store(format!("encode Mint receipt: {error}")))?;
        let attestation_json = settlement
            .attestation
            .as_ref()
            .map(serde_json::to_value)
            .transpose()
            .map_err(|error| BillingError::Store(format!("encode Mint attestation: {error}")))?;
        if let Some(receipt) = settlement.receipt.as_ref() {
            let proof = relay_billing::SettlementReceiptProof::from(receipt.clone());
            let proof = serde_json::to_value(proof).map_err(|error| {
                BillingError::Store(format!("encode settlement evidence: {error}"))
            })?;
            let evidence_result = sqlx::query(
                "INSERT INTO relay_settlement_evidence \
                 (principal, request_id, receipt, attestation, verified_at_ms, settled_at_ms) \
                 VALUES ($1,$2,$3,$4,$5,$6) ON CONFLICT (principal,request_id) DO UPDATE SET \
                 receipt=EXCLUDED.receipt, attestation=EXCLUDED.attestation, verified_at_ms=EXCLUDED.verified_at_ms \
                 WHERE relay_settlement_evidence.receipt=EXCLUDED.receipt \
                 AND relay_settlement_evidence.attestation IS NOT DISTINCT FROM EXCLUDED.attestation",
            )
            .bind(command.owner_principal.as_str())
            .bind(command.request_id.as_uuid())
            .bind(proof)
            .bind(attestation_json.clone())
            .bind(settlement.attestation.as_ref().map(|_| relay_billing::now_ms()))
            .bind(relay_billing::now_ms())
            .execute(&mut *tx)
            .await
            .map_err(db_error)?;
            require_one(
                evidence_result.rows_affected(),
                "persist immutable settlement evidence",
            )?;
            if let Some(TerminalPayload::Capture {
                usage,
                model,
                route,
            }) = command.terminal_payload.as_ref()
            {
                let input_tokens = u64::from(usage.prompt_tokens);
                let output_tokens = u64::from(usage.completion_tokens);
                let cost_event = CostEvent::captured(
                    command.request_id.as_uuid(),
                    command.owner_principal.as_str(),
                    model.as_str(),
                    *route,
                    ContractUsage {
                        input_tokens,
                        output_tokens,
                        total_tokens: u64::from(usage.total_tokens),
                        provider_cost_micros: None,
                    },
                    Microdollars::new(command.held_amount_micros.get()),
                    Microdollars::new(usage.cost_micros.get()),
                    &receipt.hash,
                    relay_billing::now_ms(),
                );
                cost_event.validate().map_err(|error| {
                    BillingError::Store(format!("validate Relay cost event: {error}"))
                })?;
                let event_json = serde_json::to_value(&cost_event).map_err(|error| {
                    BillingError::Store(format!("encode Relay cost event: {error}"))
                })?;
                let event_result = sqlx::query(
                    "INSERT INTO relay_cost_events \
                     (event_id, request_id, owner_principal, event, next_attempt_at) \
                     VALUES ($1,$2,$3,$4,NOW()) ON CONFLICT (event_id) DO UPDATE SET \
                     event=EXCLUDED.event WHERE relay_cost_events.event=EXCLUDED.event",
                )
                .bind(cost_event.event_id)
                .bind(cost_event.request_id)
                .bind(&cost_event.owner_principal)
                .bind(event_json)
                .execute(&mut *tx)
                .await
                .map_err(db_error)?;
                require_one(
                    event_result.rows_affected(),
                    "persist immutable Relay cost event",
                )?;
            }
            if let ReservePayload::Authorized { authorization } = &command.reserve_payload {
                let challenge_result = sqlx::query(
                    "UPDATE relay_aitp_challenges SET state='consumed',receipt_hash=$3,updated_at_ms=$4 \
                     WHERE request_id=$1 AND owner_principal=$2 AND state='authorized' \
                     AND operation_hash=$5",
                )
                .bind(command.request_id.as_uuid())
                .bind(command.owner_principal.as_str())
                .bind(&receipt.hash)
                .bind(relay_billing::now_ms())
                .bind(&authorization.op_hash)
                .execute(&mut *tx)
                .await
                .map_err(db_error)?;
                require_one(
                    challenge_result.rows_affected(),
                    "consume principal-bound AITP challenge",
                )?;
            }
        }
        let result = sqlx::query(
            "UPDATE relay_billing_outbox SET state='completed', receipt=$3, attestation=$4, completed_at=NOW(), \
             updated_at=NOW(), lease_owner=NULL, lease_until=NULL, last_error=NULL \
             WHERE owner_principal=$1 AND request_id=$2 AND state IN ('terminal_pending','completed')",
        )
        .bind(command.owner_principal.as_str())
        .bind(command.request_id.as_uuid())
        .bind(receipt_json)
        .bind(attestation_json)
        .execute(&mut *tx)
        .await
        .map_err(db_error)?;
        require_one(result.rows_affected(), "complete settlement command")?;
        tx.commit().await.map_err(db_error)
    }

    async fn retry(
        &self,
        command: &BillingCommand,
        error: &BillingError,
    ) -> Result<(), BillingError> {
        sqlx::query(
            "UPDATE relay_billing_outbox SET attempt_count=attempt_count+1, last_error=$3, \
             available_at=NOW() + make_interval(secs => $4), lease_owner=NULL, lease_until=NULL, updated_at=NOW() \
             WHERE owner_principal=$1 AND request_id=$2 AND state <> 'completed' \
             AND (state='terminal_pending' OR lease_owner=$5)",
        )
        .bind(command.owner_principal.as_str())
        .bind(command.request_id.as_uuid())
        .bind(error.to_string())
        .bind(RETRY_DELAY_SECONDS as f64)
        .bind(command.lease_owner)
        .execute(&self.pool)
        .await
        .map_err(db_error)?;
        Ok(())
    }

    async fn claim_due(
        &self,
        worker: Uuid,
        limit: i64,
    ) -> Result<Vec<BillingCommand>, BillingError> {
        let rows = sqlx::query(
            "WITH due AS (SELECT owner_principal,request_id FROM relay_billing_outbox \
             WHERE state NOT IN ('batch_pending','completed') AND available_at <= NOW() \
             AND (lease_until IS NULL OR lease_until <= NOW()) \
             ORDER BY available_at FOR UPDATE SKIP LOCKED LIMIT $1) \
             UPDATE relay_billing_outbox o SET lease_owner=$2, lease_until=NOW()+INTERVAL '30 seconds', \
             attempt_count=attempt_count+1, updated_at=NOW() FROM due \
             WHERE o.owner_principal=due.owner_principal AND o.request_id=due.request_id \
             RETURNING o.owner_principal,o.payer_account,o.payee_account,o.request_id,o.estimate_micros,o.held_amount_micros, \
             o.reserve_idem_key,o.reserve_payload,o.hold_id,o.state,o.terminal_idem_key,o.terminal_payload,o.lease_owner,o.batch_id,o.billing_model",
        )
        .bind(limit)
        .bind(worker)
        .fetch_all(&self.pool)
        .await
        .map_err(db_error)?;
        rows.iter().map(decode_row).collect()
    }
}

fn decode_row(row: &sqlx::postgres::PgRow) -> Result<BillingCommand, BillingError> {
    let reserve_payload = serde_json::from_value(row.try_get("reserve_payload").map_err(db_error)?)
        .map_err(|error| BillingError::Store(format!("decode reserve command: {error}")))?;
    let terminal_payload = row
        .try_get::<Option<serde_json::Value>, _>("terminal_payload")
        .map_err(db_error)?
        .map(serde_json::from_value)
        .transpose()
        .map_err(|error| BillingError::Store(format!("decode terminal command: {error}")))?;
    Ok(BillingCommand {
        owner_principal: AccountId::new(
            row.try_get::<String, _>("owner_principal")
                .map_err(db_error)?,
        ),
        payer_account: AccountId::new(
            row.try_get::<String, _>("payer_account")
                .map_err(db_error)?,
        ),
        payee_account: row
            .try_get::<Option<String>, _>("payee_account")
            .map_err(db_error)?
            .map(AccountId::new),
        request_id: JobId::from_uuid(row.try_get("request_id").map_err(db_error)?),
        estimate_micros: Micros::new(row.try_get("estimate_micros").map_err(db_error)?),
        held_amount_micros: Micros::new(row.try_get("held_amount_micros").map_err(db_error)?),
        reserve_idem_key: row.try_get("reserve_idem_key").map_err(db_error)?,
        reserve_payload,
        hold_id: row.try_get("hold_id").map_err(db_error)?,
        state: CommandState::parse(&row.try_get::<String, _>("state").map_err(db_error)?)?,
        terminal_idem_key: row.try_get("terminal_idem_key").map_err(db_error)?,
        terminal_payload,
        lease_owner: row.try_get("lease_owner").map_err(db_error)?,
        batch_id: row.try_get("batch_id").map_err(db_error)?,
        billing_model: row
            .try_get::<Option<String>, _>("billing_model")
            .map_err(db_error)?
            .map(ModelId::new),
    })
}

fn db_error(error: sqlx::Error) -> BillingError {
    BillingError::Store(format!("Relay billing outbox database error: {error}"))
}

fn require_one(rows: u64, operation: &str) -> Result<(), BillingError> {
    if rows == 1 {
        Ok(())
    } else {
        Err(BillingError::Store(format!(
            "could not {operation}; durable state conflict"
        )))
    }
}

#[derive(Default)]
struct InMemorySettlementCommandStore {
    rows: RwLock<HashMap<(AccountId, JobId), BillingCommand>>,
    /// Commands waiting for a recovery pass. The production Postgres adapter
    /// uses `available_at` plus a database lease; this credential-free adapter
    /// deliberately makes retries immediately due while preserving the same
    /// single-claimer and replay semantics.
    due: RwLock<HashSet<(AccountId, JobId)>>,
}

#[async_trait]
impl SettlementCommandStore for InMemorySettlementCommandStore {
    async fn begin_reserve(&self, command: &BillingCommand) -> Result<ReserveStart, BillingError> {
        let mut rows = self
            .rows
            .write()
            .map_err(|_| BillingError::Store("outbox lock poisoned".into()))?;
        let key = (command.owner_principal.clone(), command.request_id);
        match rows.get(&key) {
            Some(existing)
                if existing.owner_principal != command.owner_principal
                    || existing.payer_account != command.payer_account
                    || existing.payee_account != command.payee_account
                    || existing.request_id != command.request_id
                    || existing.estimate_micros != command.estimate_micros
                    || existing.held_amount_micros != command.held_amount_micros
                    || existing.reserve_idem_key != command.reserve_idem_key
                    || existing.reserve_payload != command.reserve_payload =>
            {
                Err(BillingError::Store(
                    "conflicting immutable reserve command for principal/request".into(),
                ))
            }
            Some(existing) => Ok(ReserveStart {
                command: existing.clone(),
                created: false,
            }),
            None => {
                rows.insert(key, command.clone());
                Ok(ReserveStart {
                    command: command.clone(),
                    created: true,
                })
            }
        }
    }

    async fn mark_held(&self, reservation: &Reservation) -> Result<(), BillingError> {
        let mut rows = self
            .rows
            .write()
            .map_err(|_| BillingError::Store("outbox lock poisoned".into()))?;
        let row = rows
            .get_mut(&(reservation.owner_principal.clone(), reservation.job_id))
            .ok_or_else(|| BillingError::Store("missing reserve command".into()))?;
        if row
            .hold_id
            .as_ref()
            .is_some_and(|id| Some(id) != reservation.backend_reference.as_ref())
        {
            return Err(BillingError::Store("conflicting hold id".into()));
        }
        if row.payee_account != reservation.settlement_payee {
            return Err(BillingError::Store("conflicting settlement payee".into()));
        }
        if row.lease_owner != reservation.execution_lease {
            return Err(BillingError::Store("stale reservation lease owner".into()));
        }
        row.hold_id.clone_from(&reservation.backend_reference);
        row.payee_account.clone_from(&reservation.settlement_payee);
        row.state = CommandState::Held;
        Ok(())
    }

    async fn cancel_unheld(&self, command: &BillingCommand) -> Result<(), BillingError> {
        let mut rows = self
            .rows
            .write()
            .map_err(|_| BillingError::Store("outbox lock poisoned".into()))?;
        let key = (command.owner_principal.clone(), command.request_id);
        rows.remove(&key);
        self.due
            .write()
            .map_err(|_| BillingError::Store("outbox retry lock poisoned".into()))?
            .remove(&key);
        Ok(())
    }

    async fn mark_inference_started(&self, reservation: &Reservation) -> Result<(), BillingError> {
        let mut rows = self
            .rows
            .write()
            .map_err(|_| BillingError::Store("outbox lock poisoned".into()))?;
        let row = rows
            .get_mut(&(reservation.owner_principal.clone(), reservation.job_id))
            .ok_or_else(|| BillingError::Store("missing reserve command".into()))?;
        if row.state != CommandState::Held {
            return Err(BillingError::Store(
                "request id already entered inference; refusing duplicate execution".into(),
            ));
        }
        if row.lease_owner != reservation.execution_lease {
            return Err(BillingError::Store("stale inference lease owner".into()));
        }
        row.state = CommandState::InferenceStarted;
        Ok(())
    }

    async fn renew_inference_lease(&self, reservation: &Reservation) -> Result<(), BillingError> {
        let rows = self
            .rows
            .read()
            .map_err(|_| BillingError::Store("outbox lock poisoned".into()))?;
        let row = rows
            .get(&(reservation.owner_principal.clone(), reservation.job_id))
            .ok_or_else(|| BillingError::Store("missing reserve command".into()))?;
        if row.state != CommandState::InferenceStarted
            || row.lease_owner != reservation.execution_lease
        {
            return Err(BillingError::Store("stale inference lease owner".into()));
        }
        Ok(())
    }

    async fn mark_batch_pending(
        &self,
        reservation: &Reservation,
        batch_id: Uuid,
        model: &ModelId,
    ) -> Result<(), BillingError> {
        let mut rows = self
            .rows
            .write()
            .map_err(|_| BillingError::Store("outbox lock poisoned".into()))?;
        if rows.values().any(|row| row.batch_id == Some(batch_id)) {
            return Err(BillingError::Store("paid batch id already exists".into()));
        }
        let row = rows
            .get_mut(&(reservation.owner_principal.clone(), reservation.job_id))
            .ok_or_else(|| BillingError::Store("missing reserve command".into()))?;
        if row.state != CommandState::InferenceStarted
            || row.lease_owner != reservation.execution_lease
        {
            return Err(BillingError::Store(
                "stale batch execution lease owner".into(),
            ));
        }
        row.state = CommandState::BatchPending;
        row.batch_id = Some(batch_id);
        row.billing_model = Some(model.clone());
        row.lease_owner = None;
        Ok(())
    }

    async fn claim_batch(
        &self,
        owner: &AccountId,
        batch_id: Uuid,
        worker: Uuid,
    ) -> Result<StoredBatchClaim, BillingError> {
        let mut rows = self
            .rows
            .write()
            .map_err(|_| BillingError::Store("outbox lock poisoned".into()))?;
        let Some(row) = rows
            .values_mut()
            .find(|row| row.owner_principal == *owner && row.batch_id == Some(batch_id))
        else {
            return Ok(StoredBatchClaim::NotFound);
        };
        if row.state != CommandState::BatchPending {
            return Ok(StoredBatchClaim::Acquired(Box::new(row.clone())));
        }
        if row.lease_owner.is_some() {
            return Ok(StoredBatchClaim::Busy);
        }
        row.lease_owner = Some(worker);
        Ok(StoredBatchClaim::Acquired(Box::new(row.clone())))
    }

    async fn release_batch_poll(&self, reservation: &Reservation) -> Result<(), BillingError> {
        let mut rows = self
            .rows
            .write()
            .map_err(|_| BillingError::Store("outbox lock poisoned".into()))?;
        let row = rows
            .get_mut(&(reservation.owner_principal.clone(), reservation.job_id))
            .ok_or_else(|| BillingError::Store("missing reserve command".into()))?;
        if row.state != CommandState::BatchPending || row.lease_owner != reservation.execution_lease
        {
            return Err(BillingError::Store("stale batch poll lease owner".into()));
        }
        row.lease_owner = None;
        Ok(())
    }

    async fn stage_terminal(
        &self,
        reservation: &Reservation,
        payload: &TerminalPayload,
    ) -> Result<BillingCommand, BillingError> {
        let mut rows = self
            .rows
            .write()
            .map_err(|_| BillingError::Store("outbox lock poisoned".into()))?;
        let row = rows
            .get_mut(&(reservation.owner_principal.clone(), reservation.job_id))
            .ok_or_else(|| BillingError::Store("missing reserve command".into()))?;
        if row
            .terminal_payload
            .as_ref()
            .is_some_and(|existing| existing != payload)
        {
            return Err(BillingError::Store(
                "conflicting terminal command; capture/release is immutable once selected".into(),
            ));
        }
        if matches!(
            row.state,
            CommandState::Held | CommandState::InferenceStarted | CommandState::BatchPending
        ) && row.lease_owner != reservation.execution_lease
        {
            return Err(BillingError::Store("stale terminal lease owner".into()));
        }
        row.terminal_payload = Some(payload.clone());
        row.terminal_idem_key = Some(format!(
            "relay:{}:{}:{}",
            reservation.account_id,
            reservation.job_id,
            payload.operation()
        ));
        row.state = CommandState::TerminalPending;
        row.lease_owner = None;
        Ok(row.clone())
    }

    async fn complete(
        &self,
        command: &BillingCommand,
        _settlement: &TerminalSettlement,
    ) -> Result<(), BillingError> {
        let mut rows = self
            .rows
            .write()
            .map_err(|_| BillingError::Store("outbox lock poisoned".into()))?;
        let row = rows
            .get_mut(&(command.owner_principal.clone(), command.request_id))
            .ok_or_else(|| BillingError::Store("missing command".into()))?;
        row.state = CommandState::Completed;
        row.lease_owner = None;
        self.due
            .write()
            .map_err(|_| BillingError::Store("outbox retry lock poisoned".into()))?
            .remove(&(command.owner_principal.clone(), command.request_id));
        Ok(())
    }

    async fn retry(
        &self,
        command: &BillingCommand,
        _error: &BillingError,
    ) -> Result<(), BillingError> {
        let key = (command.owner_principal.clone(), command.request_id);
        {
            let mut rows = self
                .rows
                .write()
                .map_err(|_| BillingError::Store("outbox lock poisoned".into()))?;
            let row = rows
                .get_mut(&key)
                .ok_or_else(|| BillingError::Store("missing command".into()))?;
            if row.state == CommandState::Completed {
                return Ok(());
            }
            row.lease_owner = None;
        }
        self.due
            .write()
            .map_err(|_| BillingError::Store("outbox retry lock poisoned".into()))?
            .insert(key);
        Ok(())
    }

    async fn claim_due(
        &self,
        worker: Uuid,
        limit: i64,
    ) -> Result<Vec<BillingCommand>, BillingError> {
        let limit = usize::try_from(limit).unwrap_or(0);
        if limit == 0 {
            return Ok(Vec::new());
        }

        // Remove keys from the due set before releasing its lock. Concurrent
        // recovery passes therefore cannot claim the same command. A failed
        // dispatch calls `retry` and makes the command due again.
        let claimed_keys = {
            let mut due = self
                .due
                .write()
                .map_err(|_| BillingError::Store("outbox retry lock poisoned".into()))?;
            let keys = due.iter().take(limit).cloned().collect::<Vec<_>>();
            for key in &keys {
                due.remove(key);
            }
            keys
        };

        let mut rows = self
            .rows
            .write()
            .map_err(|_| BillingError::Store("outbox lock poisoned".into()))?;
        let mut commands = Vec::with_capacity(claimed_keys.len());
        for key in claimed_keys {
            let Some(row) = rows.get_mut(&key) else {
                continue;
            };
            if matches!(
                row.state,
                CommandState::BatchPending | CommandState::Completed
            ) {
                continue;
            }
            row.lease_owner = Some(worker);
            commands.push(row.clone());
        }
        Ok(commands)
    }
}

#[cfg(test)]
mod pg_live_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use relay_billing::{HttpRequest, HttpResponse, HttpTransport, MintLedger, TransportError};
    use relay_billing::{InMemoryStore, Ledger, LedgerEntry, LedgerKind, LedgerStore, Store};

    struct FailOnceSettlementLedger {
        inner: Arc<Ledger>,
        settle_failures_remaining: AtomicUsize,
        settle_attempts: AtomicUsize,
    }

    struct FailFirstMintCaptureTransport {
        inner: relay_billing::mint::fake::FakeMint,
        capture_failures_remaining: AtomicUsize,
        requests: Mutex<Vec<HttpRequest>>,
    }

    impl FailFirstMintCaptureTransport {
        fn new() -> Self {
            Self {
                inner: relay_billing::mint::fake::FakeMint::funded(1_000)
                    .with_hold_id("hold_restart"),
                capture_failures_remaining: AtomicUsize::new(1),
                requests: Mutex::new(Vec::new()),
            }
        }

        fn captures(&self) -> Vec<HttpRequest> {
            self.requests
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .iter()
                .filter(|request| request.path.ends_with("/capture"))
                .cloned()
                .collect()
        }

        fn releases(&self) -> Vec<HttpRequest> {
            self.requests
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .iter()
                .filter(|request| request.path.ends_with("/release"))
                .cloned()
                .collect()
        }
    }

    #[async_trait]
    impl HttpTransport for FailFirstMintCaptureTransport {
        async fn send(&self, request: HttpRequest) -> Result<HttpResponse, TransportError> {
            self.requests
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .push(request.clone());
            if request.path.ends_with("/capture")
                && self
                    .capture_failures_remaining
                    .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |remaining| {
                        remaining.checked_sub(1)
                    })
                    .is_ok()
            {
                return Err(TransportError("injected Mint connection loss".into()));
            }
            self.inner.send(request).await
        }
    }

    impl FailOnceSettlementLedger {
        fn new(inner: Arc<Ledger>) -> Self {
            Self {
                inner,
                settle_failures_remaining: AtomicUsize::new(1),
                settle_attempts: AtomicUsize::new(0),
            }
        }
    }

    #[async_trait]
    impl BillingLedger for FailOnceSettlementLedger {
        async fn reserve(
            &self,
            account_id: &AccountId,
            estimate: Micros,
            job: JobId,
        ) -> Result<Reservation, BillingError> {
            self.inner.reserve(account_id, estimate, job).await
        }

        async fn reserve_authorized(
            &self,
            owner_principal: &AccountId,
            authorization: &PaymentAuthorization,
            estimate: Micros,
            job: JobId,
        ) -> Result<Reservation, BillingError> {
            BillingLedger::reserve_authorized(
                self.inner.as_ref(),
                owner_principal,
                authorization,
                estimate,
                job,
            )
            .await
        }

        async fn settle(
            &self,
            reservation: &Reservation,
            usage: Usage,
            model: ModelId,
        ) -> Result<(), BillingError> {
            self.settle_attempts.fetch_add(1, Ordering::SeqCst);
            let should_fail = self
                .settle_failures_remaining
                .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |remaining| {
                    remaining.checked_sub(1)
                })
                .is_ok();
            if should_fail {
                return Err(BillingError::Store(
                    "injected Mint settlement outage".into(),
                ));
            }
            self.inner.settle(reservation, usage, model).await
        }

        async fn release(&self, reservation: &Reservation) -> Result<(), BillingError> {
            self.inner.release(reservation).await
        }

        async fn available_micros(&self, account_id: &AccountId) -> Result<Micros, BillingError> {
            self.inner.available_micros(account_id).await
        }
    }

    fn coordinator() -> DurableBillingCoordinator {
        let store: Arc<dyn Store> = Arc::new(InMemoryStore::new());
        DurableBillingCoordinator::in_memory(Arc::new(Ledger::new(store)))
    }

    #[tokio::test]
    async fn failed_capture_survives_coordinator_restart_and_replays_once() {
        let payer = AccountId::new("payer");
        let ledger_store = InMemoryStore::new();
        ledger_store
            .append_ledger(vec![LedgerEntry::new(
                payer.clone(),
                LedgerKind::Deposit,
                Micros::new(1_000),
                "test funding",
                None,
            )])
            .await
            .unwrap();

        let local_ledger = Arc::new(Ledger::new(Arc::new(ledger_store.clone())));
        let flaky_ledger = Arc::new(FailOnceSettlementLedger::new(local_ledger));
        let outbox = Arc::new(InMemorySettlementCommandStore::default());
        let first_process = DurableBillingCoordinator {
            ledger: flaky_ledger.clone(),
            store: outbox.clone(),
        };

        let reservation = first_process
            .reserve(&payer, Micros::new(50), JobId::new())
            .await
            .unwrap();
        first_process
            .mark_inference_started(&reservation)
            .await
            .unwrap();
        let first_error = first_process
            .settle(
                &reservation,
                Usage::new(1, 1, 2, Micros::new(4)),
                ModelId::new("tensorzero::model_name::test"),
                InferenceRoute::Unknown,
            )
            .await
            .unwrap_err();
        assert_eq!(
            first_error,
            BillingError::Store("injected Mint settlement outage".into())
        );
        assert_eq!(
            flaky_ledger.available_micros(&payer).await.unwrap(),
            Micros::new(950),
            "a failed Mint capture must retain the authoritative hold"
        );
        drop(first_process);

        // Two replacement coordinators race a recovery pass over the same
        // durable state. Exactly one may lease and dispatch this command.
        let replacement_a = DurableBillingCoordinator {
            ledger: flaky_ledger.clone(),
            store: outbox.clone(),
        };
        let replacement_b = DurableBillingCoordinator {
            ledger: flaky_ledger.clone(),
            store: outbox,
        };
        let (recovered_a, recovered_b) = tokio::join!(
            replacement_a.recover_once(10),
            replacement_b.recover_once(10)
        );
        assert_eq!(recovered_a.unwrap() + recovered_b.unwrap(), 1);
        assert_eq!(flaky_ledger.settle_attempts.load(Ordering::SeqCst), 2);
        assert_eq!(
            flaky_ledger.available_micros(&payer).await.unwrap(),
            Micros::new(996),
            "recovery must release the hold and debit exactly the actual cost"
        );
        assert_eq!(replacement_a.recover_once(10).await.unwrap(), 0);

        let entries = ledger_store.ledger_for_account(&payer).await.unwrap();
        assert_eq!(
            entries
                .iter()
                .filter(|entry| entry.kind == LedgerKind::Debit)
                .count(),
            1,
            "recovery must never double-debit"
        );
    }

    #[tokio::test]
    async fn mint_capture_recovery_replays_the_exact_wire_command() {
        let transport = Arc::new(FailFirstMintCaptureTransport::new());
        let ledger: Arc<dyn BillingLedger> = Arc::new(MintLedger::new(
            transport.clone(),
            "mint-test-key",
            "acct_payee",
            None,
        ));
        let outbox = Arc::new(InMemorySettlementCommandStore::default());
        let first_process = DurableBillingCoordinator {
            ledger: ledger.clone(),
            store: outbox.clone(),
        };
        let payer = AccountId::new("acct_payer");
        let reservation = first_process
            .reserve(&payer, Micros::new(50), JobId::new())
            .await
            .unwrap();
        first_process
            .mark_inference_started(&reservation)
            .await
            .unwrap();
        first_process
            .settle(
                &reservation,
                Usage::new(2, 3, 5, Micros::new(4)),
                ModelId::new("tensorzero::model_name::test"),
                InferenceRoute::Unknown,
            )
            .await
            .expect_err("the first Mint capture is intentionally unavailable");
        drop(first_process);

        let replacement = DurableBillingCoordinator {
            ledger,
            store: outbox,
        };
        assert_eq!(replacement.recover_once(10).await.unwrap(), 1);
        assert_eq!(replacement.recover_once(10).await.unwrap(), 0);

        let captures = transport.captures();
        assert_eq!(captures.len(), 2, "one failed attempt plus one replay");
        assert_eq!(captures[0].path, "/v1/holds/hold_restart/capture");
        assert_eq!(captures[0].path, captures[1].path);
        assert_eq!(captures[0].body, captures[1].body);
        let body: serde_json::Value = serde_json::from_slice(&captures[0].body).unwrap();
        assert_eq!(body["amount_micros"], 4);
        assert_eq!(body["to"], "acct_payee");
        assert_eq!(
            body["idem_key"],
            format!("relay:{payer}:{}:capture", reservation.job_id)
        );
        assert!(
            transport.releases().is_empty(),
            "a successful provider result must never become a free release"
        );
    }

    #[tokio::test]
    async fn terminal_choice_is_immutable() {
        let coordinator = coordinator();
        let reservation = Reservation {
            job_id: JobId::new(),
            owner_principal: AccountId::new("owner"),
            account_id: AccountId::new("payer"),
            estimate_micros: Micros::new(50),
            held_amount_micros: Micros::new(50),
            backend_reference: Some("hold_1".into()),
            settlement_payee: None,
            execution_lease: Some(Uuid::now_v7()),
        };
        let command = BillingCommand {
            owner_principal: reservation.owner_principal.clone(),
            payer_account: reservation.account_id.clone(),
            payee_account: reservation.settlement_payee.clone(),
            request_id: reservation.job_id,
            estimate_micros: reservation.estimate_micros,
            held_amount_micros: reservation.held_amount_micros,
            reserve_idem_key: "reserve".into(),
            reserve_payload: ReservePayload::Direct,
            hold_id: reservation.backend_reference.clone(),
            state: CommandState::Held,
            terminal_idem_key: None,
            terminal_payload: None,
            lease_owner: reservation.execution_lease,
            batch_id: None,
            billing_model: None,
        };
        coordinator.store.begin_reserve(&command).await.unwrap();
        coordinator
            .store
            .stage_terminal(
                &reservation,
                &TerminalPayload::Capture {
                    usage: Usage::new(1, 1, 2, Micros::new(4)),
                    model: ModelId::new("m"),
                    route: InferenceRoute::Unknown,
                },
            )
            .await
            .unwrap();
        assert!(
            coordinator
                .store
                .stage_terminal(&reservation, &TerminalPayload::Release)
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn owner_and_payer_are_not_conflated() {
        let coordinator = coordinator();
        let command = BillingCommand {
            owner_principal: AccountId::new("api_key_owner"),
            payer_account: AccountId::new("wallet_payer"),
            payee_account: None,
            request_id: JobId::new(),
            estimate_micros: Micros::new(10),
            held_amount_micros: Micros::new(10),
            reserve_idem_key: "r".into(),
            reserve_payload: ReservePayload::Direct,
            hold_id: None,
            state: CommandState::ReservePending,
            terminal_idem_key: None,
            terminal_payload: None,
            lease_owner: Some(Uuid::now_v7()),
            batch_id: None,
            billing_model: None,
        };
        let stored = coordinator.store.begin_reserve(&command).await.unwrap();
        assert_eq!(stored.command.owner_principal.as_str(), "api_key_owner");
        assert_eq!(stored.command.payer_account.as_str(), "wallet_payer");
    }

    #[tokio::test]
    async fn duplicate_reserve_has_only_one_execution_owner() {
        let coordinator = coordinator();
        let command = BillingCommand {
            owner_principal: AccountId::new("owner"),
            payer_account: AccountId::new("payer"),
            payee_account: None,
            request_id: JobId::new(),
            estimate_micros: Micros::new(10),
            held_amount_micros: Micros::new(10),
            reserve_idem_key: "reserve-once".into(),
            reserve_payload: ReservePayload::Direct,
            hold_id: None,
            state: CommandState::ReservePending,
            terminal_idem_key: None,
            terminal_payload: None,
            lease_owner: Some(Uuid::now_v7()),
            batch_id: None,
            billing_model: None,
        };
        let first = coordinator.store.begin_reserve(&command).await.unwrap();
        let second = coordinator.store.begin_reserve(&command).await.unwrap();
        assert!(first.created);
        assert!(!second.created);
        assert_eq!(first.command.lease_owner, second.command.lease_owner);
    }

    #[tokio::test]
    async fn concurrent_capture_and_release_choose_one_terminal_direction() {
        let coordinator = coordinator();
        let reservation = Reservation {
            job_id: JobId::new(),
            owner_principal: AccountId::new("owner"),
            account_id: AccountId::new("payer"),
            estimate_micros: Micros::new(50),
            held_amount_micros: Micros::new(50),
            backend_reference: Some("hold_race".into()),
            settlement_payee: None,
            execution_lease: Some(Uuid::now_v7()),
        };
        let command = BillingCommand {
            owner_principal: reservation.owner_principal.clone(),
            payer_account: reservation.account_id.clone(),
            payee_account: None,
            request_id: reservation.job_id,
            estimate_micros: reservation.estimate_micros,
            held_amount_micros: reservation.held_amount_micros,
            reserve_idem_key: "reserve-race".into(),
            reserve_payload: ReservePayload::Direct,
            hold_id: reservation.backend_reference.clone(),
            state: CommandState::Held,
            terminal_idem_key: None,
            terminal_payload: None,
            lease_owner: reservation.execution_lease,
            batch_id: None,
            billing_model: None,
        };
        coordinator.store.begin_reserve(&command).await.unwrap();

        let capture_store = coordinator.store.clone();
        let release_store = coordinator.store.clone();
        let capture_reservation = reservation.clone();
        let release_reservation = reservation.clone();
        let capture = async move {
            capture_store
                .stage_terminal(
                    &capture_reservation,
                    &TerminalPayload::Capture {
                        usage: Usage::new(1, 1, 2, Micros::new(4)),
                        model: ModelId::new("m"),
                        route: InferenceRoute::Unknown,
                    },
                )
                .await
        };
        let release = async move {
            release_store
                .stage_terminal(&release_reservation, &TerminalPayload::Release)
                .await
        };
        let (capture, release) = tokio::join!(capture, release);
        let succeeded = usize::from(capture.is_ok()) + usize::from(release.is_ok());
        assert_eq!(succeeded, 1, "exactly one terminal direction must win");
    }

    #[tokio::test]
    async fn inference_execution_transition_is_compare_and_set() {
        let coordinator = coordinator();
        let reservation = Reservation {
            job_id: JobId::new(),
            owner_principal: AccountId::new("owner"),
            account_id: AccountId::new("payer"),
            estimate_micros: Micros::new(50),
            held_amount_micros: Micros::new(50),
            backend_reference: Some("hold_once".into()),
            settlement_payee: None,
            execution_lease: Some(Uuid::now_v7()),
        };
        let command = BillingCommand {
            owner_principal: reservation.owner_principal.clone(),
            payer_account: reservation.account_id.clone(),
            payee_account: None,
            request_id: reservation.job_id,
            estimate_micros: reservation.estimate_micros,
            held_amount_micros: reservation.held_amount_micros,
            reserve_idem_key: "reserve-once".into(),
            reserve_payload: ReservePayload::Direct,
            hold_id: reservation.backend_reference.clone(),
            state: CommandState::Held,
            terminal_idem_key: None,
            terminal_payload: None,
            lease_owner: reservation.execution_lease,
            batch_id: None,
            billing_model: None,
        };
        coordinator.store.begin_reserve(&command).await.unwrap();
        coordinator
            .mark_inference_started(&reservation)
            .await
            .unwrap();
        assert!(
            coordinator
                .mark_inference_started(&reservation)
                .await
                .is_err(),
            "same request id must never enter TensorZero twice"
        );

        let mut stale = reservation.clone();
        stale.execution_lease = Some(Uuid::now_v7());
        assert!(
            coordinator
                .store
                .stage_terminal(&stale, &TerminalPayload::Release)
                .await
                .is_err(),
            "a stale worker must not release the active execution"
        );
    }

    #[tokio::test]
    async fn paid_batch_has_durable_owner_and_single_poller() {
        let coordinator = coordinator();
        let reservation = Reservation {
            job_id: JobId::new(),
            owner_principal: AccountId::new("owner"),
            account_id: AccountId::new("payer"),
            estimate_micros: Micros::new(50),
            held_amount_micros: Micros::new(50),
            backend_reference: Some("hold_batch".into()),
            settlement_payee: None,
            execution_lease: Some(Uuid::now_v7()),
        };
        let command = BillingCommand {
            owner_principal: reservation.owner_principal.clone(),
            payer_account: reservation.account_id.clone(),
            payee_account: None,
            request_id: reservation.job_id,
            estimate_micros: reservation.estimate_micros,
            held_amount_micros: reservation.held_amount_micros,
            reserve_idem_key: "reserve-batch".into(),
            reserve_payload: ReservePayload::Direct,
            hold_id: reservation.backend_reference.clone(),
            state: CommandState::InferenceStarted,
            terminal_idem_key: None,
            terminal_payload: None,
            lease_owner: reservation.execution_lease,
            batch_id: None,
            billing_model: None,
        };
        coordinator.store.begin_reserve(&command).await.unwrap();
        let batch_id = Uuid::now_v7();
        let model = ModelId::new("tensorzero::function_name::summarize");
        coordinator
            .mark_batch_pending(&reservation, batch_id, &model)
            .await
            .unwrap();

        let first = coordinator
            .claim_batch(&AccountId::new("owner"), batch_id)
            .await
            .unwrap();
        let BatchClaim::Acquired {
            reservation: poll_reservation,
            model: claimed_model,
        } = first
        else {
            panic!("first poll should acquire the batch");
        };
        assert_eq!(claimed_model, model);
        assert!(matches!(
            coordinator
                .claim_batch(&AccountId::new("owner"), batch_id)
                .await
                .unwrap(),
            BatchClaim::Busy
        ));
        assert!(matches!(
            coordinator
                .claim_batch(&AccountId::new("different-owner"), batch_id)
                .await
                .unwrap(),
            BatchClaim::NotFound
        ));

        coordinator
            .release_batch_poll(&poll_reservation)
            .await
            .unwrap();
        assert!(matches!(
            coordinator
                .claim_batch(&AccountId::new("owner"), batch_id)
                .await
                .unwrap(),
            BatchClaim::Acquired { .. }
        ));
    }
}
