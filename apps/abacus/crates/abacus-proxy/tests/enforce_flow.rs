//! End-to-end enforcement flow over the injectable fakes: a real hold → capture →
//! receipt on success, and a hard KILL (no upstream call) when the budget is out.

use std::sync::Arc;

use axum::http::{HeaderMap, HeaderValue};

use abacus_core::ids::{AccountId, AgentId, Ait1Address};
use abacus_core::receipt::{ReceiptOutcome, ReceiptStore};
use abacus_core::transport::Usage;
use abacus_proxy::handler::{run_flow, AGENT_HEADER, REQUEST_HEADER};
use abacus_proxy::mem_hold_store::InMemoryHoldStore;
use abacus_proxy::{AppState, PricingTable};
use abacus_testkit::{Call, FakeBudgetRepo, FakeReceiptStore, LedgerFakeMint, ScriptedUpstream};

fn headers(agent: &AgentId) -> HeaderMap {
    let mut h = HeaderMap::new();
    h.insert(AGENT_HEADER, HeaderValue::from_str(agent.as_str()).unwrap());
    h.insert(
        "authorization",
        HeaderValue::from_static("Bearer sk-provider-test"),
    );
    h
}

fn body() -> serde_json::Value {
    serde_json::json!({
        "model": "gpt-4o-mini",
        "messages": [{"role": "user", "content": "hello there"}],
        // max_tokens ceiling prices est_max ABOVE the realized usage below, so the
        // exact-actual capture (<= the reserved hold) never exceeds it.
        "max_tokens": 2000
    })
}

#[tokio::test]
async fn success_path_captures_actual_and_writes_receipt() {
    let agent = AgentId::generate();
    let treasury = AccountId::parse("acct_treasury01").unwrap();
    let payee = Ait1Address::parse("ait1qpayee").unwrap();

    let mint = Arc::new(LedgerFakeMint::new().with_balance("acct_treasury01", 1_000_000));
    let budgets = Arc::new(FakeBudgetRepo::new().with_direct_hold_agent(&agent, &treasury, &payee));
    let receipts = Arc::new(FakeReceiptStore::new());
    let upstream = Arc::new(ScriptedUpstream::new().push_response(
        ScriptedUpstream::success_with_usage(Usage {
            input_tokens: 1000,
            output_tokens: 1000,
        }),
    ));

    let state = AppState {
        mint: mint.clone(),
        upstream,
        budgets,
        receipts: receipts.clone(),
        holds: Arc::new(InMemoryHoldStore::new()),
        ingress_api_key: "ignored-here".into(),
        admin_api_key: None,
        pricing: Arc::new(PricingTable::builtin()),
        provider: "openai".into(),
        upstream_path: "/v1/chat/completions".into(),
    };

    let mut request_headers = headers(&agent);
    request_headers.insert(REQUEST_HEADER, HeaderValue::from_static("request_shared_1"));
    run_flow(&state, &request_headers, body()).await.unwrap();

    // A receipt was written as the system-of-record, marked captured.
    assert_eq!(receipts.len(), 1);

    // The mint saw a hold then a capture to the ait1 payee (resolved by the real
    // client in prod; the fake records the payee verbatim).
    let calls = mint.calls();
    assert!(matches!(calls[0], Call::CreateHold { .. }));
    assert!(matches!(&calls[1], Call::Capture { to, .. } if to == "ait1qpayee"));

    // Actual cost (150 + 600 = 750 µ$) was captured; the held remainder returned.
    assert_eq!(mint.balance("acct_treasury01"), 1_000_000 - 750);

    let stored = receipts
        .get(match &calls[1] {
            Call::Capture { hold_id, .. } => hold_id,
            _ => unreachable!(),
        })
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored.outcome, ReceiptOutcome::Captured);
    assert_eq!(stored.captured_micros, 750);
    assert_eq!(stored.request_id, "request_shared_1");
}

#[tokio::test]
async fn exhausted_budget_kills_before_any_upstream_call() {
    let agent = AgentId::generate();
    let treasury = AccountId::parse("acct_treasury01").unwrap();
    let payee = Ait1Address::parse("ait1qpayee").unwrap();

    // Treasury holds far less than the est_max ceiling of the request.
    let mint = Arc::new(LedgerFakeMint::new().with_balance("acct_treasury01", 10));
    let budgets = Arc::new(FakeBudgetRepo::new().with_direct_hold_agent(&agent, &treasury, &payee));
    let receipts = Arc::new(FakeReceiptStore::new());
    let upstream = Arc::new(ScriptedUpstream::new());

    let state = AppState {
        mint: mint.clone(),
        upstream: upstream.clone(),
        budgets,
        receipts: receipts.clone(),
        holds: Arc::new(InMemoryHoldStore::new()),
        ingress_api_key: "ignored-here".into(),
        admin_api_key: None,
        pricing: Arc::new(PricingTable::builtin()),
        provider: "openai".into(),
        upstream_path: "/v1/chat/completions".into(),
    };

    let err = run_flow(&state, &headers(&agent), body())
        .await
        .unwrap_err();
    assert_eq!(err.status_for(), 402);

    // The request was killed at the gate: a hold was attempted, but NO capture and
    // NO receipt — and crucially ZERO bytes forwarded upstream.
    let calls = mint.calls();
    assert!(calls.iter().all(|c| !matches!(c, Call::Capture { .. })));
    assert_eq!(
        upstream.call_count(),
        0,
        "no upstream call may occur on a KILL"
    );
    assert!(receipts.is_empty());
    // Spendable is unchanged: the reserve failed, so the 10 µ$ never moved.
    assert_eq!(mint.balance("acct_treasury01"), 10);
}

#[tokio::test]
async fn two_identical_requests_on_one_run_each_get_an_independent_budget_check() {
    // Regression for the Mint idempotency-key collision (handler.rs budget bypass):
    // two distinct requests in the SAME run with the SAME est_max must NOT collapse
    // onto one hold via idempotent replay. With a per-invocation key, each request
    // gets its own budget check, so a treasury funding only ONE must 402 the second.
    let agent = AgentId::generate();
    let treasury = AccountId::parse("acct_treasury01").unwrap();
    let payee = Ait1Address::parse("ait1qpayee").unwrap();
    let run = abacus_core::ids::RunId::generate();

    // Fund the treasury with EXACTLY one est_max — enough for a single reservation,
    // one µ$ short of two — so a genuine second budget check is guaranteed to 402.
    let pricing = PricingTable::builtin();
    let est_max = pricing
        .estimate("openai", &body())
        .unwrap()
        .reserve_amount()
        .get();
    let mint = Arc::new(LedgerFakeMint::new().with_balance("acct_treasury01", est_max));
    let budgets = Arc::new(FakeBudgetRepo::new().with_direct_hold_agent(&agent, &treasury, &payee));
    let receipts = Arc::new(FakeReceiptStore::new());
    // Two successes queued; only the funded request should ever reach upstream.
    let upstream = Arc::new(
        ScriptedUpstream::new()
            .push_response(ScriptedUpstream::success_with_usage(Usage {
                input_tokens: 1000,
                output_tokens: 1000,
            }))
            .push_response(ScriptedUpstream::success_with_usage(Usage {
                input_tokens: 1000,
                output_tokens: 1000,
            })),
    );

    let state = AppState {
        mint: mint.clone(),
        upstream: upstream.clone(),
        budgets,
        receipts: receipts.clone(),
        holds: Arc::new(InMemoryHoldStore::new()),
        ingress_api_key: "ignored-here".into(),
        admin_api_key: None,
        pricing: Arc::new(PricingTable::builtin()),
        provider: "openai".into(),
        upstream_path: "/v1/chat/completions".into(),
    };

    // Pin the run id on BOTH requests so the only differentiator is the per-request
    // nonce the handler mints. No Idempotency-Key header ⇒ these are NOT retries.
    let mut h = headers(&agent);
    h.insert("x-abacus-run", HeaderValue::from_str(run.as_str()).unwrap());

    let r1 = run_flow(&state, &h, body()).await;
    let r2 = run_flow(&state, &h, body()).await;

    // Exactly one succeeds; the other is refused at the gate with a 402. If the key
    // collided the second would replay the first hold (reserving zero) and wrongly
    // be Allowed — the bypass this guards against.
    let successes = [r1.is_ok(), r2.is_ok()].iter().filter(|&&b| b).count();
    assert_eq!(
        successes, 1,
        "one funded slot ⇒ exactly one request may proceed"
    );
    let refused = r1.err().or(r2.err()).expect("one request must be refused");
    assert_eq!(refused.status_for(), 402);

    // Only the funded request forwarded upstream; the refused one sent zero bytes.
    assert_eq!(
        upstream.call_count(),
        1,
        "the refused request must not forward"
    );
}

#[tokio::test]
async fn upstream_failure_releases_hold_and_charges_nothing() {
    let agent = AgentId::generate();
    let treasury = AccountId::parse("acct_treasury01").unwrap();
    let payee = Ait1Address::parse("ait1qpayee").unwrap();

    let mint = Arc::new(LedgerFakeMint::new().with_balance("acct_treasury01", 1_000_000));
    let budgets = Arc::new(FakeBudgetRepo::new().with_direct_hold_agent(&agent, &treasury, &payee));
    let receipts = Arc::new(FakeReceiptStore::new());
    // The reserve succeeds, then the provider returns a non-2xx failure.
    let upstream = Arc::new(ScriptedUpstream::new().push_response(ScriptedUpstream::failure(502)));

    let state = AppState {
        mint: mint.clone(),
        upstream: upstream.clone(),
        budgets,
        receipts: receipts.clone(),
        holds: Arc::new(InMemoryHoldStore::new()),
        ingress_api_key: "ignored-here".into(),
        admin_api_key: None,
        pricing: Arc::new(PricingTable::builtin()),
        provider: "openai".into(),
        upstream_path: "/v1/chat/completions".into(),
    };

    // ADR-0002 success-only: a failed upstream must NOT charge the agent.
    run_flow(&state, &headers(&agent), body()).await.unwrap();

    let calls = mint.calls();
    // The hold opened, upstream was called once, and the hold was RELEASED —
    // never captured.
    assert!(matches!(calls[0], Call::CreateHold { .. }));
    assert_eq!(upstream.call_count(), 1);
    assert!(calls.iter().all(|c| !matches!(c, Call::Capture { .. })));
    let release_hold_id = calls
        .iter()
        .find_map(|c| match c {
            Call::Release { hold_id } => Some(hold_id.clone()),
            _ => None,
        })
        .expect("a release must have been issued");

    // The agent pays nothing: the full reservation is restored.
    assert_eq!(mint.balance("acct_treasury01"), 1_000_000);

    // A Released receipt is the system-of-record for the no-charge terminal.
    let stored = receipts.get(&release_hold_id).await.unwrap().unwrap();
    assert_eq!(stored.outcome, ReceiptOutcome::Released);
    assert_eq!(stored.captured_micros, 0);
}

// The allowance-ceiling funding path, end-to-end on the happy path: a hold-backed
// spend opens against the delegated allowance, the exact actual is captured to the
// payee, the ceiling is drawn down by ONLY that actual, and a Captured receipt is
// written. This is "receipted budget enforcement" proven for the allowance path
// (the success_path test above proves it for DirectHold).
#[tokio::test]
async fn allowance_ceiling_success_captures_actual_and_draws_down_ceiling() {
    let agent = AgentId::generate();
    let treasury = AccountId::parse("acct_treasury01").unwrap();
    let payee = Ait1Address::parse("ait1qpayee").unwrap();

    // Ceiling of 10_000 µ$, none spent — comfortably above the est_max (1_201).
    let mint = Arc::new(LedgerFakeMint::new().with_allowance("alw_x", 10_000, 0));
    let budgets =
        Arc::new(FakeBudgetRepo::new().with_allowance_agent(&agent, &treasury, &payee, "alw_x"));
    let receipts = Arc::new(FakeReceiptStore::new());
    let holds = Arc::new(InMemoryHoldStore::new());
    let upstream = Arc::new(ScriptedUpstream::new().push_response(
        ScriptedUpstream::success_with_usage(Usage {
            input_tokens: 1000,
            output_tokens: 1000,
        }),
    ));

    let state = AppState {
        mint: mint.clone(),
        upstream: upstream.clone(),
        budgets,
        receipts: receipts.clone(),
        holds: holds.clone(),
        ingress_api_key: "ignored-here".into(),
        admin_api_key: None,
        pricing: Arc::new(PricingTable::builtin()),
        provider: "openai".into(),
        upstream_path: "/v1/chat/completions".into(),
    };

    let mut request_headers = headers(&agent);
    request_headers.insert(REQUEST_HEADER, HeaderValue::from_static("request_alw_ok"));
    run_flow(&state, &request_headers, body()).await.unwrap();

    // Reserve was a spend_allowance, settlement a capture to the ait1 payee.
    let calls = mint.calls();
    assert!(matches!(calls[0], Call::SpendAllowance { .. }));
    assert!(matches!(&calls[1], Call::Capture { to, .. } if to == "ait1qpayee"));
    assert_eq!(upstream.call_count(), 1);

    // The ceiling is drawn down by ONLY the 750 µ$ actual (not the 1_201 reserve).
    assert_eq!(mint.allowance_remaining("alw_x"), 10_000 - 750);

    // A Captured receipt records the settled charge; the tracking hold is cleared.
    let stored = receipts
        .get_by_request_id("request_alw_ok")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored.outcome, ReceiptOutcome::Captured);
    assert_eq!(stored.captured_micros, 750);
    assert!(
        holds.is_empty(),
        "the tracking hold is removed after settle"
    );
}

// The allowance-ceiling over-budget KILL, end-to-end: when the remaining headroom
// is below est_max, the spend is refused at the gate — 402, ZERO upstream bytes,
// no capture, no receipt, no persisted tracking hold, and the ceiling untouched.
#[tokio::test]
async fn allowance_ceiling_over_budget_kills_before_any_upstream_call() {
    let agent = AgentId::generate();
    let treasury = AccountId::parse("acct_treasury01").unwrap();
    let payee = Ait1Address::parse("ait1qpayee").unwrap();

    // Remaining headroom (1_000 - 900 = 100 µ$) is far below the est_max (1_201).
    let mint = Arc::new(LedgerFakeMint::new().with_allowance("alw_x", 1_000, 900));
    let budgets =
        Arc::new(FakeBudgetRepo::new().with_allowance_agent(&agent, &treasury, &payee, "alw_x"));
    let receipts = Arc::new(FakeReceiptStore::new());
    let holds = Arc::new(InMemoryHoldStore::new());
    let upstream = Arc::new(ScriptedUpstream::new());

    let state = AppState {
        mint: mint.clone(),
        upstream: upstream.clone(),
        budgets,
        receipts: receipts.clone(),
        holds: holds.clone(),
        ingress_api_key: "ignored-here".into(),
        admin_api_key: None,
        pricing: Arc::new(PricingTable::builtin()),
        provider: "openai".into(),
        upstream_path: "/v1/chat/completions".into(),
    };

    let err = run_flow(&state, &headers(&agent), body())
        .await
        .unwrap_err();
    assert_eq!(err.status_for(), 402);

    // The spend was attempted and refused: no capture, no upstream byte, no receipt.
    let calls = mint.calls();
    assert!(calls.iter().all(|c| !matches!(c, Call::Capture { .. })));
    assert_eq!(
        upstream.call_count(),
        0,
        "no upstream call may occur on a KILL"
    );
    assert!(receipts.is_empty());
    assert!(
        holds.is_empty(),
        "a killed request persists no tracking hold"
    );
    // The ceiling never moved: the reserve failed before drawing anything down.
    assert_eq!(mint.allowance_remaining("alw_x"), 100);
}

// The LOCAL per-request ceiling (defence-in-depth) end-to-end: a request priced
// above `per_request_max_micros` is killed BEFORE Mint is touched at all — no hold,
// no upstream byte, no receipt — even though the treasury is amply funded. This is
// the belt-and-braces guard that fires ahead of the Mint ceiling.
#[tokio::test]
async fn local_per_request_ceiling_kills_before_touching_mint() {
    let agent = AgentId::generate();
    let treasury = AccountId::parse("acct_treasury01").unwrap();
    let payee = Ait1Address::parse("ait1qpayee").unwrap();

    // Treasury is richly funded, but the local ceiling (1_000 µ$) is below the
    // est_max (1_201) — so the Mint balance is never the binding constraint.
    let mint = Arc::new(LedgerFakeMint::new().with_balance("acct_treasury01", 1_000_000));
    let budgets = Arc::new(FakeBudgetRepo::new().with_capped_direct_hold_agent(
        &agent,
        &treasury,
        &payee,
        Some(1_000),
    ));
    let receipts = Arc::new(FakeReceiptStore::new());
    let holds = Arc::new(InMemoryHoldStore::new());
    let upstream = Arc::new(ScriptedUpstream::new());

    let state = AppState {
        mint: mint.clone(),
        upstream: upstream.clone(),
        budgets,
        receipts: receipts.clone(),
        holds: holds.clone(),
        ingress_api_key: "ignored-here".into(),
        admin_api_key: None,
        pricing: Arc::new(PricingTable::builtin()),
        provider: "openai".into(),
        upstream_path: "/v1/chat/completions".into(),
    };

    let err = run_flow(&state, &headers(&agent), body())
        .await
        .unwrap_err();
    assert_eq!(err.status_for(), 402);

    // The kill fired ahead of Mint: NO transport call whatsoever.
    assert!(
        mint.calls().is_empty(),
        "the local ceiling must kill before any Mint call"
    );
    assert_eq!(
        upstream.call_count(),
        0,
        "no upstream call may occur on a local-ceiling KILL"
    );
    assert!(receipts.is_empty());
    assert!(
        holds.is_empty(),
        "a killed request persists no tracking hold"
    );
    assert_eq!(mint.balance("acct_treasury01"), 1_000_000);
}
