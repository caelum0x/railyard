//! Live end-to-end proof driver for the PRODUCTION `MintLedger` money primitive.
//!
//! This is the exact `relay_billing::MintLedger` that the gateway's
//! `billing_middleware` reserves/captures/releases through in production
//! (`gateway/src/router.rs::build_billing_ledger`). Here we point it at a real,
//! running `mint-devserver` (the genuine mint-server router + handlers + ledger
//! over a real TCP socket) and drive a full hold -> capture against a funded
//! account, reading the balances back over HTTP to prove the money actually
//! moved. Nothing is stubbed: every RPC is a real HTTP round-trip to Mint.
//!
//! Run:
//!   1. In another shell: `MINT_API_KEY=dev-mint-key cargo run -p mint-server \
//!        --bin mint-devserver`
//!   2. `cargo run -p relay-billing --example live_mint_capture`
//!
//! Env:
//!   MINT_URL      base URL of the running devserver (default http://127.0.0.1:8080)
//!   MINT_API_KEY  X-API-Key for the devserver (default dev-mint-key)

use relay_billing::{AccountId, BillingLedger, JobId, Micros, MintLedger, ModelId, Usage};
use serde_json::{json, Value};

const CONTENT_TYPE: &str = "application/json";

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let base = std::env::var("MINT_URL").unwrap_or_else(|_| "http://127.0.0.1:8080".to_string());
    let api_key = std::env::var("MINT_API_KEY").unwrap_or_else(|_| "dev-mint-key".to_string());
    let http = reqwest::Client::new();

    println!("== live_mint_capture: driving the PRODUCTION MintLedger against {base} ==");

    // 1. Provision a payer (the account that pays for inference) and a payee
    //    (the provider treasury that captures). Both are real Mint accounts.
    let payer = create_account(&http, &base, &api_key, "User").await?;
    let payee = create_account(&http, &base, &api_key, "Model").await?;
    println!("created payer account: {payer}");
    println!("created payee account: {payee}");

    // 2. Fund the payer through the real mint route (auto-confirm ramp).
    let funding: i64 = 5_000_000; // $5.00
    mint_funds(&http, &base, &api_key, &payer, funding, "seed-deposit-1").await?;
    println!("funded payer with {funding} micro-USD via POST /v1/mint");

    let payer_start = balance(&http, &base, &api_key, &payer).await?;
    let payee_start = balance(&http, &base, &api_key, &payee).await?;
    println!(
        "start balances  -> payer available={} held={} | payee available={}",
        payer_start.0, payer_start.1, payee_start.0
    );

    // 3. Build the SAME production ledger the gateway wires. `payer` is forced so
    //    every hold bills that account; captures target `payee`.
    let ledger = MintLedger::with_reqwest(&base, &api_key, payee.clone(), Some(payer.clone()))?;

    // 4. RESERVE: place a hold (this is what the middleware does before calling
    //    the model). We over-hold (a $2.50 estimate) as production does.
    let job = JobId::new();
    let estimate = Micros::new(2_500_000);
    let reservation = ledger
        .reserve(&AccountId::new(payer.clone()), estimate, job)
        .await?;
    println!(
        "RESERVE ok      -> hold_id={:?} estimate={} held={}",
        reservation.backend_reference, reservation.estimate_micros.get(), reservation.held_amount_micros.get()
    );

    let payer_held = balance(&http, &base, &api_key, &payer).await?;
    println!(
        "after reserve   -> payer available={} held={} (funds are now on hold)",
        payer_held.0, payer_held.1
    );

    // 5. SETTLE: capture the EXACT actual cost (what the middleware does after the
    //    model returns usage). Capture $0.75; Mint auto-releases the remainder.
    let actual_cost: i64 = 750_000; // $0.75
    let usage = Usage::new(100, 50, 150, Micros::new(actual_cost));
    let model = ModelId::namespace("openai/gpt-4o");
    let evidence = ledger.settle_with_evidence(&reservation, usage, model).await?;
    println!(
        "SETTLE ok       -> captured {actual_cost} micro-USD; signed_receipt_present={}",
        evidence.receipt.is_some()
    );

    // 6. Read balances back from Mint and assert the money moved correctly.
    let payer_end = balance(&http, &base, &api_key, &payer).await?;
    let payee_end = balance(&http, &base, &api_key, &payee).await?;
    println!(
        "end balances    -> payer available={} held={} | payee available={}",
        payer_end.0, payer_end.1, payee_end.0
    );

    let payer_debit = payer_start.0 - payer_end.0;
    let payee_credit = payee_end.0 - payee_start.0;
    println!("net payer debit = {payer_debit} | net payee credit = {payee_credit}");

    assert_eq!(payer_debit, actual_cost, "payer debited the EXACT captured cost");
    assert_eq!(payee_credit, actual_cost, "payee credited the EXACT captured cost");
    assert_eq!(payer_end.1, 0, "hold remainder auto-released; nothing left on hold");

    println!("\nPROOF: a real Mint hold -> capture happened over HTTP.");
    println!("       payer net-debited exactly {actual_cost} micro-USD, payee credited the same,");
    println!("       remainder of the ${} hold auto-released.", estimate.get());
    Ok(())
}

/// `POST /v1/accounts` -> the new `acct_...` id.
async fn create_account(
    http: &reqwest::Client,
    base: &str,
    api_key: &str,
    kind: &str,
) -> Result<String, Box<dyn std::error::Error>> {
    let resp = http
        .post(format!("{base}/v1/accounts"))
        .header("X-API-Key", api_key)
        .header("content-type", CONTENT_TYPE)
        .body(json!({ "kind": kind }).to_string())
        .send()
        .await?;
    let status = resp.status();
    let body: Value = resp.json().await?;
    if !status.is_success() {
        return Err(format!("create_account failed ({status}): {body}").into());
    }
    Ok(body["data"]["account_id"]
        .as_str()
        .ok_or("missing account_id")?
        .to_string())
}

/// `POST /v1/mint` -> credit `amount` micro-USD to `account` (auto-confirmed).
async fn mint_funds(
    http: &reqwest::Client,
    base: &str,
    api_key: &str,
    account: &str,
    amount: i64,
    deposit_ref: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let resp = http
        .post(format!("{base}/v1/mint"))
        .header("X-API-Key", api_key)
        .header("content-type", CONTENT_TYPE)
        .body(
            json!({
                "to": account,
                "amount_micros": amount,
                "deposit_ref": deposit_ref,
                "idem_key": deposit_ref,
            })
            .to_string(),
        )
        .send()
        .await?;
    let status = resp.status();
    if !status.is_success() {
        let body: Value = resp.json().await.unwrap_or(Value::Null);
        return Err(format!("mint failed ({status}): {body}").into());
    }
    Ok(())
}

/// `GET /v1/accounts/{id}/balance` -> (available_micros, held_micros).
async fn balance(
    http: &reqwest::Client,
    base: &str,
    api_key: &str,
    account: &str,
) -> Result<(i64, i64), Box<dyn std::error::Error>> {
    let resp = http
        .get(format!("{base}/v1/accounts/{account}/balance"))
        .header("X-API-Key", api_key)
        .send()
        .await?;
    let status = resp.status();
    let body: Value = resp.json().await?;
    if !status.is_success() {
        return Err(format!("balance failed ({status}): {body}").into());
    }
    let available = body["data"]["available_micros"].as_i64().unwrap_or_default();
    let held = body["data"]["held_micros"].as_i64().unwrap_or_default();
    Ok((available, held))
}
