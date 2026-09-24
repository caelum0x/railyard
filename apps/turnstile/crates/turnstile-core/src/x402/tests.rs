use std::sync::Mutex;

use aitp_client::transport::{HttpRequest, HttpResponse, HttpTransport};
use aitp_client::PayError;
use async_trait::async_trait;
use serde_json::{json, Value};

use super::*;

const PAY_TO: &str = "0x2222222222222222222222222222222222222222";
const PAYER: &str = "0x1111111111111111111111111111111111111111";
const TX: &str = "0xabababababababababababababababababababababababababababababababab";

#[derive(Default)]
struct FakeFacilitator {
    reject_verify: Option<String>,
    fail_settle: bool,
    verified: Mutex<u32>,
    settled: Mutex<u32>,
}

#[async_trait]
impl Facilitator for FakeFacilitator {
    async fn verify(
        &self,
        _payload: &PaymentPayload,
        _requirements: &PaymentRequirements,
    ) -> Result<VerifyResponse, String> {
        *self.verified.lock().unwrap() += 1;
        Ok(VerifyResponse {
            is_valid: self.reject_verify.is_none(),
            invalid_reason: self.reject_verify.clone(),
            payer: Some(PAYER.into()),
        })
    }

    async fn settle(
        &self,
        _payload: &PaymentPayload,
        requirements: &PaymentRequirements,
    ) -> Result<SettleResponse, String> {
        *self.settled.lock().unwrap() += 1;
        if self.fail_settle {
            return Err("connection reset".into());
        }
        Ok(SettleResponse {
            success: true,
            error_reason: None,
            transaction: TX.into(),
            network: requirements.network.clone(),
            payer: Some(PAYER.into()),
        })
    }
}

/// Mint `/v1/mint` fake answering with a scripted status sequence.
struct FakeMint {
    statuses: Mutex<Vec<u16>>,
    bodies: Mutex<Vec<Value>>,
}

impl FakeMint {
    fn new(statuses: Vec<u16>) -> Self {
        Self {
            statuses: Mutex::new(statuses),
            bodies: Mutex::new(Vec::new()),
        }
    }
}

#[async_trait]
impl HttpTransport for FakeMint {
    async fn send(&self, req: HttpRequest) -> Result<HttpResponse, PayError> {
        assert_eq!(req.path, "/v1/mint");
        assert!(req
            .headers
            .iter()
            .any(|(k, v)| k == "x-api-key" && v == "mint-key"));
        self.bodies
            .lock()
            .unwrap()
            .push(serde_json::from_slice(&req.body).unwrap());
        let mut statuses = self.statuses.lock().unwrap();
        let status = if statuses.len() > 1 {
            statuses.remove(0)
        } else {
            statuses[0]
        };
        Ok(HttpResponse {
            status,
            headers: Vec::new(),
            body: b"{}".to_vec(),
        })
    }
}

fn config() -> X402Config {
    X402Config {
        pay_to: PAY_TO.into(),
        network: "base-sepolia".into(),
        facilitator_url: "https://x402.org/facilitator".into(),
        facilitator_auth: None,
        cdp_key: None,
        asset: config::USDC_BASE_SEPOLIA.into(),
        asset_name: "USD Coin".into(),
        asset_version: "2".into(),
        max_timeout_seconds: 60,
        mint_account: Some("acct_payee".into()),
        public_url: Some("https://tools.example.com".into()),
    }
}

fn gate(
    facilitator: Arc<FakeFacilitator>,
    mint: Option<Arc<FakeMint>>,
) -> (X402Gate, Arc<InMemoryX402Journal>) {
    let journal = Arc::new(InMemoryX402Journal::new());
    let poster = mint.map(|m| {
        Arc::new(MintDepositPoster::new(
            m,
            "mint-key".into(),
            "acct_payee".into(),
        ))
    });
    (
        X402Gate::new(config(), facilitator, journal.clone(), poster),
        journal,
    )
}

fn header(value: &str, nonce: &str, to: &str) -> String {
    let payload = PaymentPayload::v1("exact", "base-sepolia", authorization(value, nonce, to));
    wire::encode_header(&payload).unwrap()
}

fn authorization(value: &str, nonce: &str, to: &str) -> Value {
    json!({
        "signature": "0xsig",
        "authorization": {
            "from": PAYER, "to": to, "value": value,
            "validAfter": "0", "validBefore": "99999999999", "nonce": nonce
        }
    })
}

/// A v2 `PAYMENT-SIGNATURE` payload choosing `accepted` from the offer.
fn v2_payload(accepted: PaymentRequirementsV2, nonce: &str) -> PaymentPayload {
    PaymentPayload {
        x402_version: 2,
        accepted: Some(accepted.clone()),
        payload: authorization(&accepted.amount, nonce, &accepted.pay_to),
        ..PaymentPayload::default()
    }
}

fn requirements(g: &X402Gate) -> PaymentRequirements {
    g.requirements(
        "/v1/tools/search/call",
        "Search",
        AmountMicros(10_000),
        DEFAULT_MIME,
    )
    .unwrap()
}

#[test]
fn requirements_quote_usdc_atomic_units_at_absolute_url() {
    let (g, _) = gate(Arc::new(FakeFacilitator::default()), None);
    let req = requirements(&g);
    assert_eq!(req.max_amount_required, "10000");
    assert_eq!(
        req.resource,
        "https://tools.example.com/v1/tools/search/call"
    );
    assert_eq!(req.pay_to, PAY_TO);
    assert_eq!(req.extra.as_ref().unwrap()["name"], "USD Coin");
    assert!(g
        .requirements("/free", "free", AmountMicros(0), DEFAULT_MIME)
        .is_none());
}

#[tokio::test]
async fn verify_then_settle_journals_and_credits_mint() {
    let facilitator = Arc::new(FakeFacilitator::default());
    let mint = Arc::new(FakeMint::new(vec![201]));
    let (g, journal) = gate(facilitator.clone(), Some(mint.clone()));
    let req = requirements(&g);

    let admission = g
        .verify(&header("10000", "0x01", PAY_TO), &req)
        .await
        .unwrap();
    assert_eq!(admission.amount_micros, 10_000);
    let settled = g.settle(&admission).await.unwrap();
    assert_eq!(settled.transaction, TX);

    let rows = journal.recent(10).await.unwrap();
    assert_eq!(rows[0].phase, X402Phase::Settled);
    assert_eq!(rows[0].mint_status, MintCreditStatus::Credited);
    let body = &mint.bodies.lock().unwrap()[0];
    assert_eq!(body["deposit_ref"], format!("usdc:{TX}"));
    assert_eq!(body["to"], "acct_payee");
    assert_eq!(body["amount_micros"], 10_000);
    let summary = journal.summary().await.unwrap();
    assert_eq!(summary.settled_micros, 10_000);
    assert_eq!(summary.credited_micros, 10_000);
}

#[tokio::test]
async fn replayed_payment_is_refused_before_the_facilitator() {
    let facilitator = Arc::new(FakeFacilitator::default());
    let (g, _) = gate(facilitator.clone(), None);
    let req = requirements(&g);
    let h = header("10000", "0x02", PAY_TO);
    g.verify(&h, &req).await.unwrap();
    let err = g.verify(&h, &req).await.unwrap_err();
    assert_eq!(
        err.code,
        "invalid_exact_evm_payload_authorization_nonce_used"
    );
    assert_eq!(*facilitator.verified.lock().unwrap(), 1);
}

#[tokio::test]
async fn underpayment_and_wrong_recipient_are_rejected_locally() {
    let facilitator = Arc::new(FakeFacilitator::default());
    let (g, _) = gate(facilitator.clone(), None);
    let req = requirements(&g);
    let low = g
        .verify(&header("9999", "0x03", PAY_TO), &req)
        .await
        .unwrap_err();
    assert_eq!(low.code, "invalid_exact_evm_payload_authorization_value");
    let wrong = g
        .verify(&header("10000", "0x04", PAYER), &req)
        .await
        .unwrap_err();
    assert_eq!(wrong.code, "invalid_exact_evm_payload_recipient_mismatch");
    assert_eq!(*facilitator.verified.lock().unwrap(), 0);
}

#[tokio::test]
async fn facilitator_rejection_abandons_the_claim() {
    let facilitator = Arc::new(FakeFacilitator {
        reject_verify: Some("insufficient_funds".into()),
        ..FakeFacilitator::default()
    });
    let (g, journal) = gate(facilitator, None);
    let req = requirements(&g);
    let err = g
        .verify(&header("10000", "0x05", PAY_TO), &req)
        .await
        .unwrap_err();
    assert_eq!(err.code, "insufficient_funds");
    assert!(!err.retryable);
    assert_eq!(
        journal.recent(1).await.unwrap()[0].phase,
        X402Phase::Abandoned
    );
}

#[tokio::test]
async fn settle_in_transit_failure_is_marked_unknown_not_retried() {
    let facilitator = Arc::new(FakeFacilitator {
        fail_settle: true,
        ..FakeFacilitator::default()
    });
    let (g, journal) = gate(facilitator, None);
    let req = requirements(&g);
    let admission = g
        .verify(&header("10000", "0x06", PAY_TO), &req)
        .await
        .unwrap();
    let err = g.settle(&admission).await.unwrap_err();
    assert!(err.retryable);
    assert_eq!(
        journal.recent(1).await.unwrap()[0].phase,
        X402Phase::Unknown
    );
    assert_eq!(journal.summary().await.unwrap().unknown_count, 1);
}

#[tokio::test]
async fn unconfirmed_mint_credit_is_retried_then_bounded() {
    let facilitator = Arc::new(FakeFacilitator::default());
    // Inline attempt 402 (chain not final), reaper: 402, then 402 -> give up at 3.
    let mint = Arc::new(FakeMint::new(vec![402]));
    let (g, journal) = gate(facilitator, Some(mint.clone()));
    let req = requirements(&g);
    let admission = g
        .verify(&header("10000", "0x07", PAY_TO), &req)
        .await
        .unwrap();
    g.settle(&admission).await.unwrap();
    assert_eq!(
        journal.recent(1).await.unwrap()[0].mint_status,
        MintCreditStatus::Pending
    );
    let poster = g.mint().unwrap().clone();
    credit_pending_once(journal.as_ref(), &poster, 10, 3)
        .await
        .unwrap();
    assert_eq!(
        journal.recent(1).await.unwrap()[0].mint_status,
        MintCreditStatus::Pending
    );
    credit_pending_once(journal.as_ref(), &poster, 10, 3)
        .await
        .unwrap();
    let row = journal.recent(1).await.unwrap().remove(0);
    assert_eq!(row.mint_status, MintCreditStatus::Failed);
    assert_eq!(row.mint_attempts, 3);
}

#[tokio::test]
async fn abandoned_handler_failure_is_not_settled() {
    let facilitator = Arc::new(FakeFacilitator::default());
    let (g, journal) = gate(facilitator.clone(), None);
    let req = requirements(&g);
    let admission = g
        .verify(&header("10000", "0x08", PAY_TO), &req)
        .await
        .unwrap();
    g.abandon(&admission, "handler returned 500").await;
    assert_eq!(*facilitator.settled.lock().unwrap(), 0);
    assert_eq!(
        journal.recent(1).await.unwrap()[0].phase,
        X402Phase::Abandoned
    );
}

#[tokio::test]
async fn v2_payment_signature_verifies_and_settles() {
    let facilitator = Arc::new(FakeFacilitator::default());
    let (g, journal) = gate(facilitator.clone(), None);
    let req = requirements(&g);
    let v2 = req.to_v2();
    assert_eq!(v2.network, "eip155:84532");
    let header = wire::encode_header(&v2_payload(v2, "0x20")).unwrap();
    let admission = g.verify(&header, &req).await.unwrap();
    assert_eq!(admission.version(), 2);
    assert_eq!(admission.amount_micros, 10_000);
    g.settle(&admission).await.unwrap();
    assert_eq!(journal.recent(1).await.unwrap()[0].phase, X402Phase::Settled);
}

#[tokio::test]
async fn v2_accepted_must_match_the_offer() {
    let facilitator = Arc::new(FakeFacilitator::default());
    let (g, _) = gate(facilitator.clone(), None);
    let req = requirements(&g);

    let mut cheaper = req.to_v2();
    cheaper.amount = "1".into();
    let err = g
        .verify_payload(v2_payload(cheaper, "0x21"), &req)
        .await
        .unwrap_err();
    assert_eq!(err.code, "invalid_payment_requirements");

    let mut other_asset = req.to_v2();
    other_asset.asset = "0x3333333333333333333333333333333333333333".into();
    let err = g
        .verify_payload(v2_payload(other_asset, "0x22"), &req)
        .await
        .unwrap_err();
    assert_eq!(err.code, "invalid_payment_requirements");

    let mut mainnet = req.to_v2();
    mainnet.network = "eip155:8453".into();
    let err = g
        .verify_payload(v2_payload(mainnet, "0x23"), &req)
        .await
        .unwrap_err();
    assert_eq!(err.code, "invalid_network");
    assert_eq!(*facilitator.verified.lock().unwrap(), 0);
}

#[test]
fn caip2_configured_network_advertises_v1_name_in_v1_requirements() {
    let mut cfg = config();
    cfg.network = "eip155:84532".into();
    let g = X402Gate::new(
        cfg,
        Arc::new(FakeFacilitator::default()),
        Arc::new(InMemoryX402Journal::new()),
        None,
    );
    let req = g
        .requirements_for_url("mcp://tool/search", "Search", AmountMicros(5), DEFAULT_MIME)
        .unwrap();
    assert_eq!(req.network, "base-sepolia");
    assert_eq!(req.resource, "mcp://tool/search");
    assert_eq!(req.to_v2().network, "eip155:84532");
}
