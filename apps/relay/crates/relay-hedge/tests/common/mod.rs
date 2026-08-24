//! Shared FAKE transport for hedge tests. No live providers: each attempt is a
//! scripted future with a canned latency and outcome, instrumented with a call
//! counter and a Drop sentinel so tests can prove that losing attempts are
//! canceled (dropped) rather than run to completion.
//!
// This module is compiled independently into every integration-test binary, so
// a helper used by only some of them looks "dead" to the others.
#![allow(dead_code)]

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use relay_hedge::Attempt;

/// Scripted terminal outcome of a fake attempt once its latency elapses.
#[derive(Clone, Copy)]
pub enum Script {
    /// Resolve `Ok(endpoint_name)`.
    Ok,
    /// Resolve `Err(..)`.
    Err,
}

/// Observation handles shared with the fake future via atomics.
#[derive(Clone)]
pub struct Handle {
    started: Arc<AtomicUsize>,
    completed: Arc<AtomicUsize>,
    dropped_incomplete: Arc<AtomicBool>,
}

impl Handle {
    /// How many times the attempt's future actually began executing (i.e. was
    /// launched upstream). `0` means the attempt was never launched.
    pub fn started(&self) -> usize {
        self.started.load(Ordering::SeqCst)
    }

    /// How many times the attempt produced a successful value.
    pub fn completed(&self) -> usize {
        self.completed.load(Ordering::SeqCst)
    }

    /// `true` if the attempt's future was dropped before completing — the
    /// signature of an upstream call canceled at the socket.
    pub fn dropped_incomplete(&self) -> bool {
        self.dropped_incomplete.load(Ordering::SeqCst)
    }
}

/// Fires the cancellation sentinel if the future is dropped before it finishes.
struct DropSentinel {
    dropped_incomplete: Arc<AtomicBool>,
    done: bool,
}

impl Drop for DropSentinel {
    fn drop(&mut self) {
        if !self.done {
            self.dropped_incomplete.store(true, Ordering::SeqCst);
        }
    }
}

/// Build a fake [`Attempt`] with a scripted latency/outcome and its [`Handle`].
///
/// `T = String` (the winning endpoint name / first "chunk"), `E = String`.
pub fn fake_attempt(
    endpoint: &str,
    worst_case_cost_micros: u64,
    latency: Duration,
    script: Script,
) -> (Attempt<String, String>, Handle) {
    let handle = Handle {
        started: Arc::new(AtomicUsize::new(0)),
        completed: Arc::new(AtomicUsize::new(0)),
        dropped_incomplete: Arc::new(AtomicBool::new(false)),
    };
    let started = handle.started.clone();
    let completed = handle.completed.clone();
    let dropped = handle.dropped_incomplete.clone();
    let name = endpoint.to_string();

    let attempt = Attempt::new(endpoint, worst_case_cost_micros, move || async move {
        // Counts as "launched upstream" only when the future is polled — which,
        // for a staggered or cost-capped attempt, may never happen.
        started.fetch_add(1, Ordering::SeqCst);
        let mut sentinel = DropSentinel {
            dropped_incomplete: dropped,
            done: false,
        };
        tokio::time::sleep(latency).await;
        sentinel.done = true; // reached a terminal outcome; not a cancellation.
        match script {
            Script::Ok => {
                completed.fetch_add(1, Ordering::SeqCst);
                Ok(name)
            }
            Script::Err => Err(format!("{name} failed")),
        }
    });

    (attempt, handle)
}
