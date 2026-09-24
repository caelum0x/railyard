//! Stable, minimal Rust client for Mint's production HTTP contract.
//!
//! Amounts are always integer microdollars. Callers own idempotency keys and
//! secrets; this crate never invents financial identity or stores credentials.

use reqwest::{header::HeaderMap, Client, Method, StatusCode, Url};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

mod aitp;
mod receipts;
mod reserve;
mod streams;
mod verification;

pub use aitp::{AitpChallengeRequest, AitpSettlement};
pub use mint_protocol::{AitpReceipt, Authorization, Challenge, Scheme};
pub use receipts::{AuditProof, ReceiptBody, SignedReceipt};
pub use reserve::{MintRequest, MintResponse, Payout, RedeemRequest, RedeemResponse, ReserveProof};
pub use streams::{
    CloseStreamRequest, OpenStreamRequest, OpenStreamResponse, Stream, StreamCloseResponse,
    StreamTickResponse, TickStreamRequest,
};
pub use verification::{VerificationKey, VerificationKeys};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("invalid Mint base URL: {0}")]
    InvalidBaseUrl(String),
    #[error("Mint transport failed: {0}")]
    Transport(#[from] reqwest::Error),
    #[error("Mint returned HTTP {status} ({code}): {message}")]
    Api {
        status: StatusCode,
        code: String,
        message: String,
    },
    #[error("invalid Mint response: {0}")]
    Protocol(String),
}

#[derive(Clone)]
pub struct MintClient {
    base: Url,
    api_key: String,
    http: Client,
}

impl MintClient {
    pub fn new(base_url: &str, api_key: impl Into<String>) -> Result<Self, Error> {
        Self::with_http(base_url, api_key, Client::new())
    }

    pub fn with_http(
        base_url: &str,
        api_key: impl Into<String>,
        http: Client,
    ) -> Result<Self, Error> {
        let mut base =
            Url::parse(base_url).map_err(|error| Error::InvalidBaseUrl(error.to_string()))?;
        if !matches!(base.scheme(), "http" | "https") || base.host_str().is_none() {
            return Err(Error::InvalidBaseUrl(
                "expected an absolute http(s) URL".into(),
            ));
        }
        base.set_query(None);
        base.set_fragment(None);
        if !base.path().ends_with('/') {
            base.set_path(&format!("{}/", base.path()));
        }
        Ok(Self {
            base,
            api_key: api_key.into(),
            http,
        })
    }

    pub async fn create_account(
        &self,
        kind: &str,
        owner_ref: Option<&str>,
    ) -> Result<CreateAccountResponse, Error> {
        self.send(
            Method::POST,
            "v1/accounts",
            Some(&CreateAccountRequest { kind, owner_ref }),
        )
        .await
    }

    pub async fn account(&self, account_id: &str) -> Result<Account, Error> {
        self.get(&format!("v1/accounts/{account_id}")).await
    }

    pub async fn balance(&self, account_id: &str) -> Result<Balance, Error> {
        self.get(&format!("v1/accounts/{account_id}/balance")).await
    }

    /// Create a public routing address for an account. Possession of the
    /// address does not authorize debits from that account.
    pub async fn create_address(
        &self,
        account_id: &str,
        purpose: &str,
    ) -> Result<CreateAddressResponse, Error> {
        self.send(
            Method::POST,
            &format!("v1/accounts/{account_id}/addresses"),
            Some(&CreateAddressRequest { purpose }),
        )
        .await
    }

    /// Resolve a public Mint address to its authoritative account id.
    pub async fn lookup_address(&self, address: &str) -> Result<AddressLookup, Error> {
        self.get(&format!("v1/addresses/{address}")).await
    }

    /// Create a provider-hosted on-ramp bound directly to `account_id`.
    pub async fn create_checkout(
        &self,
        provider: &str,
        account_id: &str,
        amount_micros: i64,
    ) -> Result<Checkout, Error> {
        self.send(
            Method::POST,
            "v1/checkouts",
            Some(&CreateCheckoutRequest {
                provider,
                account_id,
                amount_micros,
            }),
        )
        .await
    }

    pub async fn transfer(&self, request: &TransferRequest<'_>) -> Result<TransferReceipt, Error> {
        if request.to.starts_with("ait1") {
            return Err(Error::Protocol(
                "public Mint addresses must use transfer_to_address so the signed receipt can be bound to the resolved recipient"
                    .into(),
            ));
        }
        let receipt: TransferReceipt = self
            .send(Method::POST, "v1/transfers", Some(request))
            .await?;
        receipt.validate_for(request)?;
        Ok(receipt)
    }

    /// Resolve a public receive address through Mint and transfer to the
    /// authoritative account it names. This two-step flow lets the client bind
    /// the returned signed receipt to the actual recipient instead of trusting
    /// an opaque routing alias.
    pub async fn transfer_to_address(
        &self,
        from: &str,
        address: &str,
        amount_micros: i64,
        idem_key: &str,
        reference: Option<&str>,
    ) -> Result<TransferReceipt, Error> {
        if !address.starts_with("ait1") {
            return Err(Error::Protocol(
                "public Mint addresses must begin with ait1".into(),
            ));
        }
        let recipient = self.lookup_address(address).await?;
        self.transfer(&TransferRequest {
            from,
            to: &recipient.account_id,
            amount_micros,
            idem_key,
            reference,
        })
        .await
    }

    /// `POST /v1/holds` — reserve `amount_micros` of `from`'s balance. Set
    /// `expires_in_ms` to have Mint auto-release the hold if it is still open
    /// after that window (the deadline commits atomically with the hold).
    pub async fn create_hold(&self, request: &CreateHoldRequest<'_>) -> Result<HoldReceipt, Error> {
        if request.amount_micros <= 0 {
            return Err(Error::Protocol("hold amount must be positive".into()));
        }
        let receipt = self.hold_call("v1/holds", request).await?;
        if receipt.holder_balance.account_id != request.from {
            return Err(Error::Protocol(
                "Mint hold receipt is for a different holder than requested".into(),
            ));
        }
        Ok(receipt)
    }

    pub async fn hold(&self, hold_id: &str) -> Result<Hold, Error> {
        self.get(&format!("v1/holds/{}", path_segment(hold_id)?)).await
    }

    /// `POST /v1/holds/:id/capture` — settle up to the held amount to
    /// `request.to` and release the remainder back to the holder. Replaying
    /// the same `idem_key` returns the original receipt.
    pub async fn capture_hold(
        &self,
        hold_id: &str,
        request: &CaptureHoldRequest<'_>,
    ) -> Result<HoldReceipt, Error> {
        if request.amount_micros <= 0 {
            return Err(Error::Protocol("capture amount must be positive".into()));
        }
        let path = format!("v1/holds/{}/capture", path_segment(hold_id)?);
        let receipt = self.hold_call(&path, request).await?;
        receipt.expect_terminal(hold_id, HoldState::Captured)?;
        if receipt.captured_micros > request.amount_micros {
            return Err(Error::Protocol(
                "Mint captured more than the requested amount".into(),
            ));
        }
        Ok(receipt)
    }

    /// Capture the ENTIRE held amount to `to`: reads the hold, then captures
    /// its full `amount_micros`. Fails with [`Error::Protocol`] when the hold
    /// is no longer open.
    pub async fn capture_hold_in_full(
        &self,
        hold_id: &str,
        to: &str,
        idem_key: &str,
        reference: Option<&str>,
    ) -> Result<HoldReceipt, Error> {
        let hold = self.hold(hold_id).await?;
        if hold.state() != HoldState::Open {
            return Err(Error::Protocol(format!(
                "hold {hold_id} is {} and can no longer be captured",
                hold.status
            )));
        }
        self.capture_hold(
            hold_id,
            &CaptureHoldRequest {
                amount_micros: hold.amount_micros,
                to,
                idem_key,
                reference,
            },
        )
        .await
    }

    /// `POST /v1/holds/:id/release` — free the whole hold back to the holder
    /// with no settlement. Replaying the same `idem_key` returns the original
    /// receipt.
    pub async fn release_hold(
        &self,
        hold_id: &str,
        request: &ReleaseHoldRequest<'_>,
    ) -> Result<HoldReceipt, Error> {
        let path = format!("v1/holds/{}/release", path_segment(hold_id)?);
        let receipt = self.hold_call(&path, request).await?;
        receipt.expect_terminal(hold_id, HoldState::Released)?;
        if receipt.captured_micros != 0 {
            return Err(Error::Protocol(
                "Mint release receipt reports a captured amount".into(),
            ));
        }
        Ok(receipt)
    }

    /// POST a hold operation and attach the signed `aitp-receipt` header.
    async fn hold_call<B: Serialize + ?Sized>(
        &self,
        path: &str,
        body: &B,
    ) -> Result<HoldReceipt, Error> {
        let (mut receipt, headers): (HoldReceipt, HeaderMap) = self
            .send_with_headers(Method::POST, path, Some(body))
            .await?;
        receipt.signed_receipt = headers
            .get("aitp-receipt")
            .and_then(|v| v.to_str().ok())
            .map(str::to_string);
        if receipt.signed_receipt.is_none() {
            return Err(Error::Protocol(
                "Mint hold response is missing its signed aitp-receipt".into(),
            ));
        }
        Ok(receipt)
    }

    pub async fn grant_allowance(
        &self,
        request: &GrantAllowanceRequest<'_>,
    ) -> Result<GrantAllowanceResponse, Error> {
        self.send(Method::POST, "v1/allowances", Some(request))
            .await
    }

    pub async fn allowance(&self, allowance_id: &str) -> Result<Allowance, Error> {
        self.get(&format!("v1/allowances/{allowance_id}")).await
    }

    pub async fn spend_allowance(
        &self,
        allowance_id: &str,
        request: &SpendAllowanceRequest<'_>,
    ) -> Result<serde_json::Value, Error> {
        self.send(
            Method::POST,
            &format!("v1/allowances/{allowance_id}/spend"),
            Some(request),
        )
        .await
    }

    pub async fn revoke_allowance(
        &self,
        allowance_id: &str,
        idem_key: &str,
    ) -> Result<RevokeAllowanceResponse, Error> {
        self.send(
            Method::POST,
            &format!("v1/allowances/{allowance_id}/revoke"),
            Some(&RevokeAllowanceRequest { idem_key }),
        )
        .await
    }

    async fn get<T: DeserializeOwned>(&self, path: &str) -> Result<T, Error> {
        self.send::<T, serde_json::Value>(Method::GET, path, None)
            .await
    }

    async fn send<T: DeserializeOwned, B: Serialize + ?Sized>(
        &self,
        method: Method,
        path: &str,
        body: Option<&B>,
    ) -> Result<T, Error> {
        Ok(self.send_with_headers(method, path, body).await?.0)
    }

    async fn send_with_headers<T: DeserializeOwned, B: Serialize + ?Sized>(
        &self,
        method: Method,
        path: &str,
        body: Option<&B>,
    ) -> Result<(T, HeaderMap), Error> {
        let url = self
            .base
            .join(path)
            .map_err(|error| Error::InvalidBaseUrl(error.to_string()))?;
        let mut request = self
            .http
            .request(method, url)
            .header("x-api-key", &self.api_key)
            .header("accept", "application/json");
        if let Some(body) = body {
            request = request.json(body);
        }
        let response = request.send().await?;
        let status = response.status();
        let headers = response.headers().clone();
        let bytes = response.bytes().await?;
        if !status.is_success() {
            let envelope: ErrorEnvelope =
                serde_json::from_slice(&bytes).unwrap_or_else(|_| ErrorEnvelope {
                    error: ErrorBody {
                        code: "http_error".into(),
                        message: String::from_utf8_lossy(&bytes).into_owned(),
                    },
                });
            return Err(Error::Api {
                status,
                code: envelope.error.code,
                message: envelope.error.message,
            });
        }
        let envelope: SuccessEnvelope<T> =
            serde_json::from_slice(&bytes).map_err(|error| Error::Protocol(error.to_string()))?;
        if !envelope.success {
            return Err(Error::Protocol(
                "success response carried success=false".into(),
            ));
        }
        Ok((envelope.data, headers))
    }
}

#[derive(Debug, Deserialize)]
struct SuccessEnvelope<T> {
    success: bool,
    data: T,
}

#[derive(Debug, Deserialize)]
struct ErrorEnvelope {
    error: ErrorBody,
}

#[derive(Debug, Deserialize)]
struct ErrorBody {
    code: String,
    message: String,
}

#[derive(Debug, Serialize)]
struct CreateAccountRequest<'a> {
    kind: &'a str,
    owner_ref: Option<&'a str>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct CreateAccountResponse {
    pub account_id: String,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct Account {
    pub id: String,
    pub kind: String,
    pub status: String,
    pub created_at: Option<String>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct Balance {
    pub account_id: String,
    pub available_micros: i64,
    pub held_micros: i64,
    pub updated_seq: u64,
}

#[derive(Debug, Serialize)]
struct CreateAddressRequest<'a> {
    purpose: &'a str,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct CreateAddressResponse {
    pub address: String,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct AddressLookup {
    pub account_id: String,
}

#[derive(Debug, Serialize)]
struct CreateCheckoutRequest<'a> {
    provider: &'a str,
    account_id: &'a str,
    amount_micros: i64,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct Checkout {
    pub provider: String,
    pub checkout_id: String,
    pub checkout_url: String,
    pub deposit_ref: String,
    pub account_id: String,
    pub amount_micros: i64,
}

#[derive(Debug, Serialize)]
pub struct TransferRequest<'a> {
    pub from: &'a str,
    pub to: &'a str,
    pub amount_micros: i64,
    pub idem_key: &'a str,
    #[serde(rename = "ref", skip_serializing_if = "Option::is_none")]
    pub reference: Option<&'a str>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct TransferReceipt {
    pub first_seq: u64,
    pub from_balance: serde_json::Value,
    pub to_balance: serde_json::Value,
    pub idempotent_replay: bool,
    /// Exact signed sender-chain receipt for the committed transfer.
    pub receipt: SignedTransferReceipt,
    /// Privacy-safe, independently verifiable evidence for the same command.
    pub attestation: TransferAttestation,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct SignedTransferReceipt {
    pub body: TransferReceiptBody,
    pub hash: String,
    pub sig: String,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct TransferReceiptBody {
    pub account_id: String,
    pub account_seq: u64,
    pub journal_seq: u64,
    pub op: String,
    pub op_hash: [u8; 32],
    pub amount_micros: i64,
    pub from: Option<String>,
    pub to: Option<String>,
    pub balance_after_available_micros: i64,
    pub balance_after_held_micros: i64,
    pub ts: i64,
    pub prev_hash: String,
    pub signer: String,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct TransferAttestation {
    pub format: String,
    pub payload_base64url: String,
    pub digest_sha256: String,
    pub signer_key_id: String,
    pub signature_hex: String,
}

impl TransferReceipt {
    fn validate_for(&self, request: &TransferRequest<'_>) -> Result<(), Error> {
        let body = &self.receipt.body;
        if body.account_id != request.from
            || body.from.as_deref() != Some(request.from)
            || body.to.as_deref() != Some(request.to)
            || body.journal_seq != self.first_seq
            || body.amount_micros != request.amount_micros
            || !body.op.eq_ignore_ascii_case("transfer")
            || self.attestation.format != "mint-transfer-attestation-v1"
            || self.attestation.signer_key_id != body.signer
            || self.receipt.hash.is_empty()
            || self.receipt.sig.is_empty()
            || self.attestation.payload_base64url.is_empty()
            || self.attestation.digest_sha256.is_empty()
            || self.attestation.signature_hex.is_empty()
        {
            return Err(Error::Protocol(
                "Mint transfer proof does not match the requested sender, recipient, amount, sequence, and signer"
                    .into(),
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Serialize)]
pub struct CreateHoldRequest<'a> {
    pub from: &'a str,
    pub amount_micros: i64,
    pub idem_key: &'a str,
    #[serde(rename = "ref", skip_serializing_if = "Option::is_none")]
    pub reference: Option<&'a str>,
    /// Auto-release window in milliseconds (max 30 days). `None` ⇒ the hold
    /// stays open until captured or released.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_in_ms: Option<i64>,
}

#[derive(Debug, Serialize)]
pub struct CaptureHoldRequest<'a> {
    pub amount_micros: i64,
    pub to: &'a str,
    pub idem_key: &'a str,
    #[serde(rename = "ref", skip_serializing_if = "Option::is_none")]
    pub reference: Option<&'a str>,
}

#[derive(Debug, Serialize)]
pub struct ReleaseHoldRequest<'a> {
    pub idem_key: &'a str,
    #[serde(rename = "ref", skip_serializing_if = "Option::is_none")]
    pub reference: Option<&'a str>,
}

/// The lifecycle state of a hold.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HoldState {
    Open,
    Captured,
    Released,
    /// A state this SDK version does not know.
    Unknown,
}

impl HoldState {
    fn parse(s: &str) -> Self {
        match s.to_ascii_lowercase().as_str() {
            "open" => Self::Open,
            "captured" => Self::Captured,
            "released" => Self::Released,
            _ => Self::Unknown,
        }
    }
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct Hold {
    pub id: String,
    pub account_id: String,
    pub amount_micros: i64,
    pub captured_micros: i64,
    pub status: String,
    #[serde(rename = "ref")]
    pub reference: Option<String>,
}

impl Hold {
    pub fn state(&self) -> HoldState {
        HoldState::parse(&self.status)
    }
}

/// The result of a hold operation (create / capture / release).
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct HoldReceipt {
    pub hold_id: String,
    pub first_seq: u64,
    /// The holder's balance after the operation.
    pub holder_balance: Balance,
    /// The payee's balance after a capture (`None` for create / release).
    #[serde(default)]
    pub payee_balance: Option<Balance>,
    pub status: String,
    pub captured_micros: i64,
    pub idempotent_replay: bool,
    /// Settlement attestation Mint attaches to terminal operations, if any.
    #[serde(default)]
    pub attestation: Option<serde_json::Value>,
    /// The encoded signed receipt from the `aitp-receipt` response header.
    #[serde(skip)]
    pub signed_receipt: Option<String>,
}

impl HoldReceipt {
    pub fn state(&self) -> HoldState {
        HoldState::parse(&self.status)
    }

    fn expect_terminal(&self, hold_id: &str, want: HoldState) -> Result<(), Error> {
        if self.hold_id != hold_id {
            return Err(Error::Protocol(
                "Mint hold receipt is for a different hold than requested".into(),
            ));
        }
        if self.state() != want {
            return Err(Error::Protocol(format!(
                "Mint hold receipt status is {} (expected {want:?})",
                self.status
            )));
        }
        Ok(())
    }
}

/// Percent-encode an id for use as one URL path segment; empty ids are rejected.
fn path_segment(id: &str) -> Result<String, Error> {
    if id.is_empty() {
        return Err(Error::Protocol("empty id".into()));
    }
    Ok(id
        .bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            other => format!("%{other:02X}"),
        })
        .collect())
}

#[derive(Debug, Serialize)]
pub struct GrantAllowanceRequest<'a> {
    pub granter: &'a str,
    pub to_key: &'a str,
    pub limit_micros: i64,
    pub window: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scope: Option<serde_json::Value>,
    pub idem_key: &'a str,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct GrantAllowanceResponse {
    pub allowance_id: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Allowance {
    pub id: String,
    pub granter: String,
    pub to_key: String,
    pub limit_micros: i64,
    pub spent_micros: i64,
    pub window: String,
    pub window_start_ms: i64,
    pub scope: Option<serde_json::Value>,
    pub revoked: bool,
}

#[derive(Debug, Serialize)]
pub struct SpendAllowanceRequest<'a> {
    pub amount_micros: i64,
    pub to: &'a str,
    pub scheme: &'a str,
    pub idem_key: &'a str,
    #[serde(rename = "ref", skip_serializing_if = "Option::is_none")]
    pub reference: Option<&'a str>,
}

#[derive(Debug, Serialize)]
struct RevokeAllowanceRequest<'a> {
    idem_key: &'a str,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct RevokeAllowanceResponse {
    pub revoked: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_non_http_and_normalizes_base() {
        assert!(MintClient::new("file:///tmp/mint", "secret").is_err());
        let client = MintClient::new("https://mint.example/internal", "secret").unwrap();
        assert_eq!(client.base.as_str(), "https://mint.example/internal/");
    }

    #[test]
    fn request_money_is_integer_microdollars() {
        let body = serde_json::to_value(CreateHoldRequest {
            from: "acct_user",
            amount_micros: 1,
            idem_key: "idem",
            reference: None,
            expires_in_ms: None,
        })
        .unwrap();
        assert_eq!(body["amount_micros"], 1);
        assert!(body.get("expires_in_ms").is_none());
        assert!(body.get("amount").is_none());
    }

    #[test]
    fn hold_receipt_parses_server_shape_and_checks_terminal_state() {
        let receipt: HoldReceipt = serde_json::from_value(serde_json::json!({
            "hold_id": "hold_1",
            "first_seq": 9,
            "holder_balance": {"account_id": "acct_a", "available_micros": 10, "held_micros": 0, "updated_seq": 9},
            "payee_balance": {"account_id": "acct_b", "available_micros": 5, "held_micros": 0, "updated_seq": 9},
            "status": "Captured",
            "captured_micros": 5,
            "idempotent_replay": false,
        }))
        .unwrap();
        assert_eq!(receipt.state(), HoldState::Captured);
        assert!(receipt.expect_terminal("hold_1", HoldState::Captured).is_ok());
        assert!(receipt.expect_terminal("hold_1", HoldState::Released).is_err());
        assert!(receipt.expect_terminal("hold_2", HoldState::Captured).is_err());
    }

    #[test]
    fn hold_ids_are_encoded_as_one_path_segment() {
        assert_eq!(path_segment("hold_1").unwrap(), "hold_1");
        assert_eq!(path_segment("a/../b").unwrap(), "a%2F..%2Fb");
        assert!(path_segment("").is_err());
    }

    #[test]
    fn transfer_receipt_requires_complete_signed_evidence() {
        let response = serde_json::json!({
            "first_seq": 7,
            "from_balance": {"available_micros": 750_000, "held_micros": 0},
            "to_balance": {"available_micros": 250_000, "held_micros": 0},
            "idempotent_replay": false,
            "receipt": {
                "body": {
                    "account_id": "acct_sender",
                    "account_seq": 3,
                    "journal_seq": 7,
                    "op": "Transfer",
                    "op_hash": vec![17; 32],
                    "amount_micros": 250_000,
                    "from": "acct_sender",
                    "to": "acct_recipient",
                    "balance_after_available_micros": 750_000,
                    "balance_after_held_micros": 0,
                    "ts": 1_700_000_000_000_i64,
                    "prev_hash": "00".repeat(32),
                    "signer": "mint-verifier-key-1"
                },
                "hash": "22".repeat(32),
                "sig": "33".repeat(64)
            },
            "attestation": {
                "format": "mint-transfer-attestation-v1",
                "payload_base64url": "cGF5bG9hZA",
                "digest_sha256": "44".repeat(32),
                "signer_key_id": "mint-verifier-key-1",
                "signature_hex": "55".repeat(64)
            }
        });
        let parsed: TransferReceipt = serde_json::from_value(response.clone()).unwrap();
        assert_eq!(parsed.receipt.body.to.as_deref(), Some("acct_recipient"));
        assert_eq!(parsed.attestation.format, "mint-transfer-attestation-v1");
        parsed
            .validate_for(&TransferRequest {
                from: "acct_sender",
                to: "acct_recipient",
                amount_micros: 250_000,
                idem_key: "send-1",
                reference: None,
            })
            .unwrap();

        let wrong_recipient = TransferRequest {
            from: "acct_sender",
            to: "acct_attacker",
            amount_micros: 250_000,
            idem_key: "send-1",
            reference: None,
        };
        assert!(parsed.validate_for(&wrong_recipient).is_err());

        let mut missing = response;
        missing.as_object_mut().unwrap().remove("attestation");
        assert!(serde_json::from_value::<TransferReceipt>(missing).is_err());
    }
}
