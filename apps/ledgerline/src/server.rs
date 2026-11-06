//! Axum product API and the idempotent ingest workflow.

use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::routing::{get, post};
use axum::{Json, Router};
use chrono::Utc;
use serde::Serialize;
use subtle::ConstantTimeEq;
use tower_http::trace::TraceLayer;

use crate::domain::{MarginFilter, MarginRecord, MarginSummaryQuery, MarginSummaryRow, MeterEvent};
use crate::error::LedgerlineError;
use crate::mint::MintSettlement;
use crate::policy::AccountPolicy;
use crate::receipt_verify::SourceReceiptVerifier;
use crate::store::EventStore;

#[derive(Clone)]
pub struct AppState {
    pub mint: Arc<dyn MintSettlement>,
    pub store: Arc<dyn EventStore>,
    pub api_key: Arc<str>,
    /// Which Mint accounts an ingested event may name (enforced at the HTTP
    /// boundary before any settlement).
    pub policy: Arc<AccountPolicy>,
    /// Trusted Relay/Mint public keys for cryptographically verifying any
    /// `source_settlement_receipt` an event carries (enforced before any
    /// settlement, alongside `policy`).
    pub receipt_verifier: Arc<SourceReceiptVerifier>,
}

const ADMIN_HTML: &str = include_str!("../assets/admin.html");

#[derive(Serialize)]
struct Data<T> {
    data: T,
}

pub fn build_router(state: AppState) -> Router {
    Router::new()
        .route("/healthz", get(healthz))
        .route("/readyz", get(readyz))
        .route("/v1/events", post(create_event))
        .route("/admin", get(admin_console))
        .route("/v1/margins", get(list_margins))
        .route("/v1/margins/summary", get(margin_summary))
        .route("/v1/margins/:request_id", get(get_margin))
        .with_state(state)
        .layer(TraceLayer::new_for_http())
}

async fn healthz() -> StatusCode {
    StatusCode::NO_CONTENT
}

async fn readyz(State(state): State<AppState>) -> Result<StatusCode, LedgerlineError> {
    state.store.ready().await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn create_event(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(event): Json<MeterEvent>,
) -> Result<(StatusCode, Json<Data<MarginRecord>>), LedgerlineError> {
    authorize(&state, &headers)?;
    // Validate shape first (clear 422s), then the account policy, so no Mint
    // call is ever made for an event naming accounts outside the policy.
    event.validate()?;
    state.policy.check(&event)?;
    let (record, created) = ingest_event(&state, event).await?;
    let status = if created {
        StatusCode::CREATED
    } else {
        StatusCode::OK
    };
    Ok((status, Json(Data { data: record })))
}

async fn get_margin(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(request_id): Path<String>,
) -> Result<Json<Data<MarginRecord>>, LedgerlineError> {
    authorize(&state, &headers)?;
    let record = state
        .store
        .by_request_id(&request_id)
        .await?
        .ok_or(LedgerlineError::NotFound)?;
    Ok(Json(Data { data: record }))
}

async fn list_margins(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(filter): Query<MarginFilter>,
) -> Result<Json<Data<Vec<MarginRecord>>>, LedgerlineError> {
    authorize(&state, &headers)?;
    let records = state.store.list(&filter).await?;
    Ok(Json(Data { data: records }))
}

/// `GET /v1/margins/summary?group_by=customer|feature|model|day&from=&to=` —
/// margin rollups with totals and negative-margin counts.
async fn margin_summary(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<MarginSummaryQuery>,
) -> Result<Json<Data<SummaryBody>>, LedgerlineError> {
    authorize(&state, &headers)?;
    if let (Some(from), Some(to)) = (query.from, query.to) {
        if from >= to {
            return Err(LedgerlineError::Invalid("from must be before to".into()));
        }
    }
    let groups = state.store.summarize(&query).await?;
    let totals = groups
        .iter()
        .fold(MarginSummaryRow::default(), |mut acc, g| {
            acc.requests += g.requests;
            acc.failed_requests += g.failed_requests;
            acc.negative_margin_requests += g.negative_margin_requests;
            acc.revenue_micros = acc.revenue_micros.saturating_add(g.revenue_micros);
            acc.cogs_micros = acc.cogs_micros.saturating_add(g.cogs_micros);
            acc.margin_micros = acc.margin_micros.saturating_add(g.margin_micros);
            acc
        });
    let totals = MarginSummaryRow {
        key: "total".into(),
        ..totals
    }
    .finish();
    let negative_margin_groups = groups
        .iter()
        .filter(|g| g.margin_micros < 0)
        .map(|g| g.key.clone())
        .collect();
    Ok(Json(Data {
        data: SummaryBody {
            group_by: query.group_by,
            totals,
            negative_margin_groups,
            groups,
        },
    }))
}

#[derive(Serialize)]
struct SummaryBody {
    group_by: crate::domain::GroupBy,
    totals: MarginSummaryRow,
    /// Buckets losing money overall — the alert list.
    negative_margin_groups: Vec<String>,
    groups: Vec<MarginSummaryRow>,
}

/// `GET /admin` — the margin console (static; data calls use `X-API-Key`).
async fn admin_console() -> impl axum::response::IntoResponse {
    use axum::http::header;
    (
        [
            (header::CONTENT_TYPE, "text/html; charset=utf-8"),
            (header::CACHE_CONTROL, "no-store"),
            (header::X_FRAME_OPTIONS, "DENY"),
            (
                header::CONTENT_SECURITY_POLICY,
                "default-src 'none'; script-src 'unsafe-inline'; style-src 'unsafe-inline'; \
                 connect-src 'self'; frame-ancestors 'none'; base-uri 'none'",
            ),
        ],
        ADMIN_HTML,
    )
}

fn authorize(state: &AppState, headers: &HeaderMap) -> Result<(), LedgerlineError> {
    let presented = headers
        .get("X-API-Key")
        .and_then(|value| value.to_str().ok())
        .ok_or(LedgerlineError::Unauthorized)?;
    if presented.len() != state.api_key.len()
        || !bool::from(presented.as_bytes().ct_eq(state.api_key.as_bytes()))
    {
        return Err(LedgerlineError::Unauthorized);
    }
    Ok(())
}

/// Execute one event's complete product workflow. Mint calls are deterministic,
/// so a crash before the Postgres write can safely resume without double charge.
pub async fn ingest_event(
    state: &AppState,
    event: MeterEvent,
) -> Result<(MarginRecord, bool), LedgerlineError> {
    event.validate()?;
    // `validate()` only checks that a receipt's fields are present and
    // correlate with this request's own principal/amount — it is a pure
    // function and cannot hold key material. Prove the receipt was genuinely
    // issued by Relay/Mint (not just typed in by whoever holds the
    // Ledgerline API key) before any money moves.
    if let Some(receipt) = &event.source_settlement_receipt {
        state
            .receipt_verifier
            .verify(receipt)
            .map_err(LedgerlineError::Invalid)?;
    }

    if let Some(mut existing) = state
        .store
        .by_idempotency_key(&event.idempotency_key)
        .await?
    {
        if existing.event != event {
            return Err(LedgerlineError::IdempotencyConflict);
        }
        existing.idempotent_replay = true;
        return Ok((existing, false));
    }

    // A request is one financial event. Catch a second caller key before any
    // external settlement; the Mint transport also keys operations by request
    // id to make the concurrent form of this race safe.
    if state
        .store
        .by_request_id(&event.request_id)
        .await?
        .is_some()
    {
        return Err(LedgerlineError::IdempotencyConflict);
    }

    let settlement = state.mint.settle(&event).await?;
    let candidate = MarginRecord::new(event, settlement, Utc::now())?;
    let (mut stored, created) = state.store.save(&candidate).await?;
    if stored.event != candidate.event {
        return Err(LedgerlineError::IdempotencyConflict);
    }
    stored.idempotent_replay = !created;
    Ok((stored, created))
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use async_trait::async_trait;
    use axum::body::{to_bytes, Body};
    use axum::http::Request;
    use ed25519_dalek::{Signer as _, SigningKey};
    use tower::ServiceExt;

    use super::*;
    use crate::domain::{EventOutcome, EventSource, SettlementRefs, SourceSettlementReceipt};
    use crate::store::MemoryStore;

    #[derive(Default)]
    struct CountingMint(AtomicUsize);

    #[async_trait]
    impl MintSettlement for CountingMint {
        async fn settle(&self, event: &MeterEvent) -> Result<SettlementRefs, LedgerlineError> {
            if event.outcome == EventOutcome::Failed {
                return Ok(SettlementRefs::failed_request());
            }
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(SettlementRefs {
                revenue_hold_id: Some("hold_1".into()),
                revenue_sequence: Some(10),
                cogs_sequence: Some(12),
            })
        }
    }

    fn event() -> MeterEvent {
        MeterEvent {
            idempotency_key: "idem-1".into(),
            request_id: "request-1".into(),
            customer_id: "customer-1".into(),
            feature: "render".into(),
            model: "wan-2.2".into(),
            source: EventSource::Reelforge,
            outcome: EventOutcome::Succeeded,
            revenue_micros: 900_000,
            cogs_micros: 600_000,
            customer_account: "acct_customer".into(),
            revenue_account: "acct_revenue".into(),
            cogs_account: "acct_cogs".into(),
            provider_account: "acct_provider".into(),
            source_settlement_receipt: None,
        }
    }

    #[tokio::test]
    async fn exact_replay_does_not_settle_twice() {
        let mint = Arc::new(CountingMint::default());
        let state = AppState {
            mint: mint.clone(),
            store: Arc::new(MemoryStore::default()),
            api_key: Arc::from("secret"),
            policy: Arc::new(AccountPolicy::unrestricted()),
            receipt_verifier: Arc::new(SourceReceiptVerifier::none()),
        };

        let (first, created) = ingest_event(&state, event()).await.expect("first ingest");
        let (replay, replay_created) = ingest_event(&state, event()).await.expect("replay");

        assert!(created);
        assert!(!first.idempotent_replay);
        assert!(!replay_created);
        assert!(replay.idempotent_replay);
        assert_eq!(mint.0.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn changed_replay_is_a_conflict() {
        let state = AppState {
            mint: Arc::new(CountingMint::default()),
            store: Arc::new(MemoryStore::default()),
            api_key: Arc::from("secret"),
            policy: Arc::new(AccountPolicy::unrestricted()),
            receipt_verifier: Arc::new(SourceReceiptVerifier::none()),
        };
        ingest_event(&state, event()).await.expect("first ingest");
        let mut changed = event();
        changed.cogs_micros += 1;
        assert_eq!(
            ingest_event(&state, changed).await,
            Err(LedgerlineError::IdempotencyConflict)
        );
    }

    #[tokio::test]
    async fn second_key_for_one_request_is_rejected_before_settlement() {
        let mint = Arc::new(CountingMint::default());
        let state = AppState {
            mint: mint.clone(),
            store: Arc::new(MemoryStore::default()),
            api_key: Arc::from("secret"),
            policy: Arc::new(AccountPolicy::unrestricted()),
            receipt_verifier: Arc::new(SourceReceiptVerifier::none()),
        };
        ingest_event(&state, event()).await.expect("first ingest");
        let mut duplicate_request = event();
        duplicate_request.idempotency_key = "idem-2".into();
        assert_eq!(
            ingest_event(&state, duplicate_request).await,
            Err(LedgerlineError::IdempotencyConflict)
        );
        assert_eq!(mint.0.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn failed_request_records_zero_without_mint_movements() {
        let mint = Arc::new(CountingMint::default());
        let state = AppState {
            mint: mint.clone(),
            store: Arc::new(MemoryStore::default()),
            api_key: Arc::from("secret"),
            policy: Arc::new(AccountPolicy::unrestricted()),
            receipt_verifier: Arc::new(SourceReceiptVerifier::none()),
        };
        let mut failed = event();
        failed.outcome = EventOutcome::Failed;
        failed.revenue_micros = 0;
        failed.cogs_micros = 0;
        let (record, _) = ingest_event(&state, failed).await.expect("failed event");
        assert_eq!(record.margin_micros, 0);
        assert_eq!(mint.0.load(Ordering::SeqCst), 0);
        assert_eq!(record.settlement, SettlementRefs::failed_request());
    }

    #[tokio::test]
    async fn http_surface_requires_auth_and_returns_queryable_margin() {
        let state = AppState {
            mint: Arc::new(CountingMint::default()),
            store: Arc::new(MemoryStore::default()),
            api_key: Arc::from("secret"),
            policy: Arc::new(AccountPolicy::unrestricted()),
            receipt_verifier: Arc::new(SourceReceiptVerifier::none()),
        };
        let app = build_router(state);
        let body = serde_json::to_vec(&event()).expect("serialize event");

        let unauthorized = app
            .clone()
            .oneshot(
                Request::post("/v1/events")
                    .header("content-type", "application/json")
                    .body(Body::from(body.clone()))
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);

        let created = app
            .clone()
            .oneshot(
                Request::post("/v1/events")
                    .header("content-type", "application/json")
                    .header("x-api-key", "secret")
                    .body(Body::from(body))
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(created.status(), StatusCode::CREATED);
        let created_body = to_bytes(created.into_body(), 64 * 1024)
            .await
            .expect("response body");
        let created_json: serde_json::Value =
            serde_json::from_slice(&created_body).expect("response json");
        assert_eq!(created_json["data"]["margin_micros"], 300_000);

        let queried = app
            .oneshot(
                Request::get("/v1/margins/request-1")
                    .header("x-api-key", "secret")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(queried.status(), StatusCode::OK);
    }

    fn relay_gateway_event(receipt: SourceSettlementReceipt) -> MeterEvent {
        let mut e = event();
        e.source = EventSource::RelayGateway;
        e.source_settlement_receipt = Some(receipt);
        e
    }

    fn signed_receipt(signing_key: &SigningKey, event: &MeterEvent) -> SourceSettlementReceipt {
        let hash = [11u8; 32];
        let mut receipt = SourceSettlementReceipt {
            request_id: event.request_id.clone(),
            payer_account: event.customer_account.clone(),
            sequence: 42,
            operation: "hold.capture".into(),
            amount_micros: event.cogs_micros,
            operation_hash: "op_hash".into(),
            receipt_hash: hex::encode(hash),
            signature: String::new(),
        };
        // Sign the full claim commitment (crate::receipt_verify::commitment_digest),
        // not the bare receipt_hash — see receipt_verify.rs's 2026-11-06 fix.
        let signature = signing_key.sign(&crate::receipt_verify::commitment_digest(&receipt));
        receipt.signature = hex::encode(signature.to_bytes());
        receipt
    }

    #[tokio::test]
    async fn relay_gateway_event_with_a_fabricated_receipt_is_rejected() {
        let signing_key = SigningKey::from_bytes(&[21u8; 32]);
        let verifier = Arc::new(
            SourceReceiptVerifier::from_hex_keys([hex::encode(
                signing_key.verifying_key().as_bytes(),
            )])
            .expect("valid key"),
        );
        let state = AppState {
            mint: Arc::new(CountingMint::default()),
            store: Arc::new(MemoryStore::default()),
            api_key: Arc::from("secret"),
            policy: Arc::new(AccountPolicy::unrestricted()),
            receipt_verifier: verifier,
        };

        // Shape-valid (passes `MeterEvent::validate`), but the signature is
        // fabricated, not a genuine Relay/Mint attestation.
        let fabricated = relay_gateway_event(SourceSettlementReceipt {
            request_id: "request-1".into(),
            payer_account: "acct_customer".into(),
            sequence: 42,
            operation: "hold.capture".into(),
            amount_micros: 600_000,
            operation_hash: "op_hash".into(),
            receipt_hash: hex::encode([11u8; 32]),
            signature: hex::encode([0u8; 64]),
        });
        assert!(fabricated.validate().is_ok());
        assert_eq!(
            ingest_event(&state, fabricated).await,
            Err(LedgerlineError::Invalid(
                "source settlement receipt signature does not verify against any trusted \
                 Relay/Mint key for this exact claim"
                    .into()
            ))
        );
    }

    #[tokio::test]
    async fn relay_gateway_event_with_a_genuinely_signed_receipt_settles() {
        let signing_key = SigningKey::from_bytes(&[21u8; 32]);
        let verifier = Arc::new(
            SourceReceiptVerifier::from_hex_keys([hex::encode(
                signing_key.verifying_key().as_bytes(),
            )])
            .expect("valid key"),
        );
        let mint = Arc::new(CountingMint::default());
        let state = AppState {
            mint: mint.clone(),
            store: Arc::new(MemoryStore::default()),
            api_key: Arc::from("secret"),
            policy: Arc::new(AccountPolicy::unrestricted()),
            receipt_verifier: verifier,
        };

        let base = event();
        let genuine = relay_gateway_event(signed_receipt(&signing_key, &base));
        let (record, created) = ingest_event(&state, genuine)
            .await
            .expect("genuine receipt");
        assert!(created);
        assert_eq!(mint.0.load(Ordering::SeqCst), 1);
        assert_eq!(record.margin_micros, 300_000);
    }

    /// End-to-end proof (through the real `ingest_event` chokepoint, not just
    /// `receipt_verify`'s own unit tests) that a genuine, signed receipt for
    /// one real request cannot be replayed verbatim to back a forged claim
    /// for a different request/amount. This is the exact gap the 2026-11-05
    /// fix (`600d177e`) left open and the 2026-11-06 follow-up closes.
    #[tokio::test]
    async fn relay_gateway_event_cannot_replay_a_different_requests_genuine_receipt() {
        let signing_key = SigningKey::from_bytes(&[21u8; 32]);
        let verifier = Arc::new(
            SourceReceiptVerifier::from_hex_keys([hex::encode(
                signing_key.verifying_key().as_bytes(),
            )])
            .expect("valid key"),
        );
        let mint = Arc::new(CountingMint::default());
        let state = AppState {
            mint: mint.clone(),
            store: Arc::new(MemoryStore::default()),
            api_key: Arc::from("secret"),
            policy: Arc::new(AccountPolicy::unrestricted()),
            receipt_verifier: verifier,
        };

        // A genuine receipt, correctly signed for request-1's real $0.60 COGS.
        let base = event();
        let genuine_receipt = signed_receipt(&signing_key, &base);

        // Replay that EXACT receipt_hash + signature onto a different
        // request claiming a 1000x larger COGS amount against the same
        // payer. Pre-fix, `receipt_verify::verify` only checked the
        // signature against the bare `receipt_hash` — unchanged by this
        // substitution — so this would have incorrectly settled.
        let mut forged = relay_gateway_event(genuine_receipt);
        forged.idempotency_key = "idem-forged".into();
        forged.request_id = "request-forged".into();
        if let Some(receipt) = forged.source_settlement_receipt.as_mut() {
            receipt.request_id = "request-forged".into();
        }
        forged.cogs_micros = 600_000_000;
        if let Some(receipt) = forged.source_settlement_receipt.as_mut() {
            receipt.amount_micros = 600_000_000;
        }

        let result = ingest_event(&state, forged).await;
        assert!(
            matches!(result, Err(LedgerlineError::Invalid(_))),
            "a genuine receipt replayed onto a forged claim must be rejected, got {result:?}"
        );
        assert_eq!(
            mint.0.load(Ordering::SeqCst),
            0,
            "no Mint settlement must occur for the rejected forged claim"
        );
    }
}
