//! `GET /metrics` in the Prometheus text format (RY-MT1).
//!
//! What it reports:
//! - readiness, journal length and reserve (one store round trip each);
//! - open holds (count and micros) and the hold-expiry sweep lag: how long the
//!   oldest expired-but-still-open hold has been waiting for auto-release;
//! - pending payouts;
//! - the receipt hash-chain verification: every receipt's hash, `prev_hash` link,
//!   `account_seq` and Ed25519 signature are checked against the server's signer
//!   ([`mint_receipts::verify_log`]). It runs on the first scrape and then again
//!   once `MINT_CHAIN_VERIFY_EVERY_S` (default 86400, nightly) has passed, in the
//!   background, so a scrape never waits on a long verification.
//!
//! Access: with `MINT_METRICS_TOKEN` set, a scrape must send
//! `Authorization: Bearer <token>`. Without it, only direct scrapes are served:
//! a request carrying a proxy header (`X-Forwarded-For`, `Forwarded`,
//! `X-Real-IP`, which Caddy always sets) gets 404, so the public Mint site never
//! exposes ledger figures.

use std::fmt::Write as _;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::extract::State;
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Extension;
use mint_receipts::{verify_log, Keyring};
use mint_store::{HoldStatus, LedgerStore, PayoutQuery, PayoutRecordStatus};
use subtle::ConstantTimeEq;

use crate::jobs::now_ms;
use crate::state::AppState;

/// Default cache for the store-derived gauges (seconds).
const DEFAULT_CACHE_S: u64 = 15;
/// Default interval between receipt-chain verifications (nightly).
const DEFAULT_VERIFY_EVERY_S: u64 = 86_400;
/// Expired holds inspected per scrape for the sweep-lag gauge.
const LAG_SAMPLE: usize = 500;
/// Pending payouts counted per scrape (the gauge saturates here).
const PAYOUT_SAMPLE: usize = 1_000;
/// How long a scrape waits for a verification it just started.
const FIRST_VERIFY_WAIT: Duration = Duration::from_secs(2);

/// Result of the last receipt-chain verification.
#[derive(Clone, Debug, Default)]
struct ChainStatus {
    ok: Option<bool>,
    finished_unix: i64,
    receipts: u64,
    duration_ms: u64,
    started: Option<Instant>,
    runs: u64,
    failures: u64,
}

/// Per-router metrics state (installed as an `Extension` on the `/metrics` route).
pub struct MetricsState {
    token: Option<String>,
    shadow: bool,
    started: Instant,
    cache_ttl: Duration,
    verify_every: Duration,
    cache: tokio::sync::Mutex<Option<(Instant, String)>>,
    chain: Mutex<ChainStatus>,
    verifying: AtomicBool,
}

fn env_u64(key: &str) -> Option<u64> {
    std::env::var(key).ok().and_then(|v| v.trim().parse().ok())
}

impl MetricsState {
    /// Build from explicit settings (tests) .
    pub fn new(
        token: Option<String>,
        shadow: bool,
        cache_ttl: Duration,
        verify_every: Duration,
    ) -> Self {
        MetricsState {
            token: token.filter(|t| !t.trim().is_empty()),
            shadow,
            started: Instant::now(),
            cache_ttl,
            verify_every,
            cache: tokio::sync::Mutex::new(None),
            chain: Mutex::new(ChainStatus::default()),
            verifying: AtomicBool::new(false),
        }
    }

    /// Build from `MINT_METRICS_TOKEN`, `MINT_SHADOW`, `MINT_METRICS_CACHE_S` and
    /// `MINT_CHAIN_VERIFY_EVERY_S`.
    pub fn from_env() -> Arc<Self> {
        Arc::new(Self::new(
            std::env::var("MINT_METRICS_TOKEN").ok(),
            crate::shadow::shadow_enabled(),
            Duration::from_secs(env_u64("MINT_METRICS_CACHE_S").unwrap_or(DEFAULT_CACHE_S)),
            Duration::from_secs(
                env_u64("MINT_CHAIN_VERIFY_EVERY_S")
                    .unwrap_or(DEFAULT_VERIFY_EVERY_S)
                    .max(60),
            ),
        ))
    }

    fn allowed(&self, headers: &HeaderMap) -> bool {
        match &self.token {
            Some(token) => headers
                .get(header::AUTHORIZATION)
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.strip_prefix("Bearer "))
                .map(|got| bool::from(got.trim().as_bytes().ct_eq(token.trim().as_bytes())))
                .unwrap_or(false),
            None => !["x-forwarded-for", "forwarded", "x-real-ip"]
                .iter()
                .any(|h| headers.contains_key(*h)),
        }
    }

    fn chain_due(&self) -> bool {
        let chain = self.chain.lock().expect("chain status lock");
        match chain.started {
            None => true,
            Some(at) => at.elapsed() >= self.verify_every,
        }
    }
}

/// Verify the whole receipt log once and record the result.
pub async fn verify_receipt_chain<S: LedgerStore + 'static>(st: &AppState<S>, m: &MetricsState) {
    let t0 = Instant::now();
    {
        let mut chain = m.chain.lock().expect("chain status lock");
        chain.started = Some(t0);
    }
    let keyring = Keyring::with_signer(st.receipt_signer.as_ref());
    let outcome = match st.ledger.store().try_snapshot_receipts().await {
        Ok(all) => {
            let n = all.len() as u64;
            match verify_log(&all, &keyring) {
                Ok(()) => (Some(true), n),
                Err(e) => {
                    tracing::error!(error = ?e, "receipt chain verification FAILED");
                    (Some(false), n)
                }
            }
        }
        Err(e) => {
            tracing::warn!(error = %e, "receipt chain verification could not read the log");
            (Some(false), 0)
        }
    };
    let mut chain = m.chain.lock().expect("chain status lock");
    chain.ok = outcome.0;
    chain.receipts = outcome.1;
    chain.finished_unix = now_ms() / 1000;
    chain.duration_ms = t0.elapsed().as_millis() as u64;
    chain.runs += 1;
    if outcome.0 == Some(false) {
        chain.failures += 1;
    }
}

fn spawn_verify_if_due<S: LedgerStore + 'static>(st: &AppState<S>, m: &Arc<MetricsState>) -> bool {
    if !m.chain_due() || m.verifying.swap(true, Ordering::AcqRel) {
        return false;
    }
    let (st, m) = (st.clone(), m.clone());
    tokio::spawn(async move {
        verify_receipt_chain(&st, &m).await;
        m.verifying.store(false, Ordering::Release);
    });
    true
}

fn gauge(out: &mut String, name: &str, help: &str, value: impl std::fmt::Display) {
    let _ = writeln!(
        out,
        "# HELP {name} {help}\n# TYPE {name} gauge\n{name} {value}"
    );
}

/// The store-derived gauges (cached for `cache_ttl`).
async fn store_section<S: LedgerStore + 'static>(st: &AppState<S>) -> String {
    let store = st.ledger.store();
    let mut out = String::new();
    let mut errors: Vec<&str> = Vec::new();

    let journal = store.try_journal_len().await;
    let reserve = store.try_reserve_micros().await;
    gauge(
        &mut out,
        "mint_ready",
        "1 when the ledger store answers (same checks as /readyz).",
        u8::from(journal.is_ok() && reserve.is_ok()),
    );
    match journal {
        Ok(n) => gauge(
            &mut out,
            "mint_journal_entries",
            "Committed journal entries.",
            n,
        ),
        Err(_) => errors.push("journal"),
    }
    match reserve {
        Ok(r) => gauge(
            &mut out,
            "mint_reserve_micros",
            "Reserve backing minted balances, in microdollars.",
            r.get(),
        ),
        Err(_) => errors.push("reserve"),
    }

    match store.rebuild_holds().await {
        Ok(holds) => {
            let open: Vec<_> = holds
                .values()
                .filter(|h| h.status == HoldStatus::Open)
                .collect();
            let micros: i64 = open.iter().map(|h| h.amount_micros.get()).sum();
            gauge(
                &mut out,
                "mint_holds_open",
                "Holds in the OPEN state.",
                open.len(),
            );
            gauge(
                &mut out,
                "mint_holds_open_micros",
                "Microdollars reserved by open holds.",
                micros,
            );
        }
        Err(_) => errors.push("holds"),
    }

    let now = now_ms();
    match store.list_expired_holds(now, LAG_SAMPLE).await {
        Ok(ids) => {
            let mut oldest: Option<i64> = None;
            for id in &ids {
                if let Ok(Some(at)) = store.hold_expiry(id).await {
                    oldest = Some(oldest.map_or(at, |o| o.min(at)));
                }
            }
            let lag_s = oldest.map_or(0, |at| (now - at).max(0) / 1000);
            gauge(
                &mut out,
                "mint_holds_expired_unreleased",
                "Expired holds still open (the sweep releases them; capped at 500).",
                ids.len(),
            );
            gauge(&mut out, "mint_hold_sweep_lag_seconds",
                "Age past its deadline of the oldest expired hold not yet released (0 = sweep is current).", lag_s);
        }
        Err(_) => errors.push("hold_expiry"),
    }

    match store
        .list_payouts(PayoutQuery {
            status: Some(PayoutRecordStatus::Pending),
            account_id: None,
            before: None,
            limit: PAYOUT_SAMPLE,
        })
        .await
    {
        Ok(p) => gauge(
            &mut out,
            "mint_payouts_pending",
            "Payouts waiting on their provider (capped at 1000).",
            p.len(),
        ),
        Err(_) => errors.push("payouts"),
    }

    let _ = writeln!(out, "# HELP mint_metrics_check_failed 1 when a metrics check could not read the store.\n# TYPE mint_metrics_check_failed gauge");
    for check in ["journal", "reserve", "holds", "hold_expiry", "payouts"] {
        let _ = writeln!(
            out,
            "mint_metrics_check_failed{{check=\"{check}\"}} {}",
            u8::from(errors.contains(&check))
        );
    }
    out
}

fn chain_section(m: &MetricsState) -> String {
    let chain = m.chain.lock().expect("chain status lock").clone();
    let mut out = String::new();
    if let Some(ok) = chain.ok {
        gauge(
            &mut out,
            "mint_receipt_chain_verify_ok",
            "1 when the last full receipt hash-chain and signature verification passed.",
            u8::from(ok),
        );
        gauge(
            &mut out,
            "mint_receipt_chain_last_verify_timestamp_seconds",
            "Unix time the last receipt-chain verification finished.",
            chain.finished_unix,
        );
        gauge(
            &mut out,
            "mint_receipt_chain_verified_receipts",
            "Receipts checked by the last verification.",
            chain.receipts,
        );
        gauge(
            &mut out,
            "mint_receipt_chain_verify_duration_seconds",
            "Duration of the last verification.",
            format!("{:.3}", chain.duration_ms as f64 / 1000.0),
        );
    }
    gauge(
        &mut out,
        "mint_receipt_chain_verify_running",
        "1 while a verification runs.",
        u8::from(m.verifying.load(Ordering::Acquire)),
    );
    let _ = writeln!(out, "# HELP mint_receipt_chain_verify_runs_total Verifications finished.\n# TYPE mint_receipt_chain_verify_runs_total counter\nmint_receipt_chain_verify_runs_total {}", chain.runs);
    let _ = writeln!(out, "# HELP mint_receipt_chain_verify_failures_total Verifications that failed.\n# TYPE mint_receipt_chain_verify_failures_total counter\nmint_receipt_chain_verify_failures_total {}", chain.failures);
    out
}

/// `GET /metrics`.
pub async fn metrics<S: LedgerStore + 'static>(
    State(st): State<AppState<S>>,
    Extension(m): Extension<Arc<MetricsState>>,
    headers: HeaderMap,
) -> Response {
    if !m.allowed(&headers) {
        return StatusCode::NOT_FOUND.into_response();
    }
    if spawn_verify_if_due(&st, &m) {
        // Give a short verification the chance to land in this scrape.
        let deadline = Instant::now() + FIRST_VERIFY_WAIT;
        while m.verifying.load(Ordering::Acquire) && Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    let store_part = {
        let mut cache = m.cache.lock().await;
        match cache.as_ref() {
            Some((at, text)) if at.elapsed() < m.cache_ttl => text.clone(),
            _ => {
                let text = store_section(&st).await;
                *cache = Some((Instant::now(), text.clone()));
                text
            }
        }
    };

    let mut out = String::new();
    gauge(
        &mut out,
        "mint_up",
        "1 while mint-server serves requests.",
        1,
    );
    gauge(
        &mut out,
        "mint_shadow_mode",
        "1 when MINT_SHADOW=1 (deposits and payouts refused).",
        u8::from(m.shadow),
    );
    gauge(
        &mut out,
        "mint_uptime_seconds",
        "Seconds since this router was built.",
        m.started.elapsed().as_secs(),
    );
    out.push_str(&store_part);
    out.push_str(&chain_section(&m));
    (
        [(
            header::CONTENT_TYPE,
            "text/plain; version=0.0.4; charset=utf-8",
        )],
        out,
    )
        .into_response()
}
