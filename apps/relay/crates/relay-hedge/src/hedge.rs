//! Hedged dispatch: race N deferred attempts, take the first `Ok`, cancel the
//! rest by dropping their futures. Additive over the existing per-provider call
//! helpers — an [`Attempt`] just wraps one of those calls behind a deferred
//! launch closure so we only ever send bytes upstream for attempts we actually
//! launch.

use std::collections::VecDeque;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use futures::stream::{FuturesUnordered, StreamExt};

use crate::config::HedgeConfig;

/// A boxed, in-flight attempt future. Resolving to `Ok` makes this attempt a
/// candidate winner; the same shape works for non-streaming (resolves at full
/// response) and streaming (resolves at first chunk / TTFT) races.
pub type BoxAttempt<T, E> = Pin<Box<dyn Future<Output = Result<T, E>> + Send>>;

/// One hedge attempt against a single endpoint.
///
/// `launch` is deferred (an `FnOnce`) so that:
///   * an attempt blocked by the cost cap never runs (no upstream bytes, no
///     cost), and
///   * a staggered attempt only fires its upstream call after its delay, so a
///     fast primary can win before any hedge is sent.
pub struct Attempt<T, E> {
    /// Endpoint identifier (the routing/provider name). Carried through so the
    /// winner and each accumulated error stay attributable.
    pub endpoint: Arc<str>,
    /// Worst-case upstream cost (micros) if this attempt ran to completion. Used
    /// only for the concurrent cost cap; never a ledger charge.
    pub worst_case_cost_micros: u64,
    /// Deferred factory producing the in-flight future.
    pub launch: Box<dyn FnOnce() -> BoxAttempt<T, E> + Send>,
}

impl<T, E> Attempt<T, E> {
    /// Convenience constructor from any `FnOnce` returning a `Send` future.
    pub fn new<F, Fut>(
        endpoint: impl Into<Arc<str>>,
        worst_case_cost_micros: u64,
        launch: F,
    ) -> Self
    where
        F: FnOnce() -> Fut + Send + 'static,
        Fut: Future<Output = Result<T, E>> + Send + 'static,
    {
        Self {
            endpoint: endpoint.into(),
            worst_case_cost_micros,
            launch: Box::new(move || Box::pin(launch())),
        }
    }
}

/// The single winning result of a race.
pub struct HedgeOutcome<T> {
    /// The endpoint whose attempt won (first `Ok`).
    pub winner: Arc<str>,
    /// The winning value — the ONLY value the race yields. All losers dropped.
    pub value: T,
    /// How many attempts were actually launched (`>= 1`).
    pub launched: usize,
    /// `true` iff the concurrent (`FuturesUnordered`) path ran. `false` on the
    /// sequential path taken when `max_concurrency <= 1`.
    pub raced: bool,
}

/// Failure of a whole race.
#[derive(Debug, thiserror::Error)]
pub enum HedgeError<E> {
    /// No attempts were supplied.
    #[error("no hedge attempts supplied")]
    NoAttempts,
    /// Every launched attempt failed. Errors are kept per-endpoint in
    /// launch/completion order, mirroring the existing `provider_errors ->
    /// AllModelProvidersFailed` accumulation.
    #[error("all {} hedge attempt(s) failed", .errors.len())]
    AllFailed { errors: Vec<(Arc<str>, E)> },
}

/// Race `attempts` according to `cfg`, returning the first `Ok` and dropping the
/// rest.
///
/// * `effective_concurrency() <= 1` (the default) => sequential fallback: run
///   attempts one at a time, in order, first `Ok` wins. This is byte-for-byte
///   the pre-existing behavior and takes NO `FuturesUnordered` branch.
/// * otherwise => concurrent race of up to `max_concurrency` attempts, honoring
///   `stagger_ms` and `max_cost_micros`, with a sequential tail refill.
pub async fn race<T, E>(
    attempts: Vec<Attempt<T, E>>,
    cfg: &HedgeConfig,
) -> Result<HedgeOutcome<T>, HedgeError<E>>
where
    T: Send + 'static,
    E: Send + 'static,
{
    if attempts.is_empty() {
        return Err(HedgeError::NoAttempts);
    }
    if cfg.effective_concurrency() <= 1 {
        return sequential(attempts).await;
    }
    raced(attempts, cfg).await
}

/// The untouched fallback: one attempt at a time, first `Ok` wins.
async fn sequential<T, E>(attempts: Vec<Attempt<T, E>>) -> Result<HedgeOutcome<T>, HedgeError<E>> {
    let mut errors = Vec::new();
    let mut launched = 0usize;
    for attempt in attempts {
        let Attempt {
            endpoint, launch, ..
        } = attempt;
        launched += 1;
        match launch().await {
            Ok(value) => {
                return Ok(HedgeOutcome {
                    winner: endpoint,
                    value,
                    launched,
                    raced: false,
                });
            }
            Err(e) => errors.push((endpoint, e)),
        }
    }
    Err(HedgeError::AllFailed { errors })
}

/// Each in-flight wrapper future carries its endpoint and committed cost so we
/// can attribute the result and release the cost slot on failure.
type Wrapped<T, E> = Pin<Box<dyn Future<Output = (Arc<str>, u64, Result<T, E>)> + Send>>;

async fn raced<T, E>(
    attempts: Vec<Attempt<T, E>>,
    cfg: &HedgeConfig,
) -> Result<HedgeOutcome<T>, HedgeError<E>>
where
    T: Send + 'static,
    E: Send + 'static,
{
    let concurrency = cfg.effective_concurrency();
    let cap = cfg.max_cost_micros;
    let stagger = cfg
        .stagger_ms
        .filter(|&ms| ms > 0)
        .map(Duration::from_millis);

    let mut pending: VecDeque<Attempt<T, E>> = attempts.into();
    let mut inflight: FuturesUnordered<Wrapped<T, E>> = FuturesUnordered::new();
    let mut errors: Vec<(Arc<str>, E)> = Vec::new();
    let mut launched = 0usize;
    let mut cost_committed: u64 = 0;

    fill(
        &mut pending,
        &mut inflight,
        &mut cost_committed,
        &mut launched,
        concurrency,
        cap,
        stagger,
    );

    while let Some((endpoint, cost, result)) = inflight.next().await {
        match result {
            Ok(value) => {
                // Returning here drops `inflight`, which drops every losing
                // wrapper future and — transitively — its in-flight upstream
                // call, canceling it at the socket. Exactly one value escapes.
                return Ok(HedgeOutcome {
                    winner: endpoint,
                    value,
                    launched,
                    raced: true,
                });
            }
            Err(e) => {
                errors.push((endpoint, e));
                // A slot (and its cost) freed up; try to launch more from the
                // sequential tail.
                cost_committed = cost_committed.saturating_sub(cost);
                fill(
                    &mut pending,
                    &mut inflight,
                    &mut cost_committed,
                    &mut launched,
                    concurrency,
                    cap,
                    stagger,
                );
            }
        }
    }

    Err(HedgeError::AllFailed { errors })
}

/// Launch attempts until the concurrency window is full or the cost cap blocks
/// the next one. The very first attempt of a race always launches (even over
/// the cap) so a request never silently runs zero attempts.
fn fill<T, E>(
    pending: &mut VecDeque<Attempt<T, E>>,
    inflight: &mut FuturesUnordered<Wrapped<T, E>>,
    cost_committed: &mut u64,
    launched: &mut usize,
    concurrency: usize,
    cap: Option<u64>,
    stagger: Option<Duration>,
) where
    T: Send + 'static,
    E: Send + 'static,
{
    while inflight.len() < concurrency {
        let Some(next) = pending.front() else {
            break;
        };
        let is_first = *launched == 0;
        // Cost cap: don't launch attempt k if it would push committed
        // worst-case spend over the cap. The first attempt is exempt so we
        // always degrade to at least a single (sequential) attempt.
        if !is_first {
            if let Some(cap) = cap {
                if cost_committed.saturating_add(next.worst_case_cost_micros) > cap {
                    break;
                }
            }
        }

        let Attempt {
            endpoint,
            worst_case_cost_micros,
            launch,
        } = pending
            .pop_front()
            .expect("front() returned Some immediately above");

        *cost_committed = cost_committed.saturating_add(worst_case_cost_micros);
        *launched += 1;

        // Stagger every attempt after the very first, so a fast primary can win
        // before a hedge's upstream call is even fired.
        let delay = if is_first { None } else { stagger };
        let ep = endpoint.clone();
        let wrapped: Wrapped<T, E> = Box::pin(async move {
            if let Some(d) = delay {
                tokio::time::sleep(d).await;
            }
            let result = launch().await;
            (ep, worst_case_cost_micros, result)
        });
        inflight.push(wrapped);
    }
}
