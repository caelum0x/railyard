//! Dynamic HTTP 402 recovery for Relay/Turnstile clients.
//!
//! The protected server owns settlement. The caller sends the request once,
//! decodes an `AITP-Challenge` from a 402, signs it, and retries exactly once
//! with `AITP-Authorization`. It does not pre-settle directly with Mint; doing
//! so would consume the nonce before the protected server can verify it.

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::codec::{
    decode_challenge, decode_receipt, encode_authorization, HEADER_AUTHORIZATION, HEADER_CHALLENGE,
    HEADER_RECEIPT, HEADER_REQUEST_ID, HEADER_VERSION,
};
use crate::error::PayError;
use crate::hold_attestation::{HoldSettlementAttestation, HoldSettlementAttestationClaims};
use crate::signer::{op_hash, op_hash_hex, AuthSigner, AuthorizationIntent};
use crate::transport::{HttpRequest, HttpResponse, HttpTransport, StreamingHttpResponse};
use crate::types::{PaymentAuthorization, PaymentChallenge, Receipt};
use ed25519_dalek::VerifyingKey;

#[derive(serde::Deserialize)]
struct SettlementEvidenceEnvelope {
    data: SettlementEvidenceData,
}

#[derive(serde::Deserialize)]
struct SettlementEvidenceData {
    request_id: String,
    principal: String,
    receipt: Receipt,
    attestation: Option<HoldSettlementAttestation>,
    verified_at_ms: Option<i64>,
}

struct VerifiedSettlementEvidence {
    principal: String,
    receipt: Receipt,
    attestation: HoldSettlementAttestation,
    verified_at_ms: Option<i64>,
}

/// Caller-pinned Mint key used to independently authenticate Relay's durable
/// settlement projection. Relay's envelope and key id are data, not trust
/// roots: both the Ed25519 signature and its exact receipt binding are checked.
#[derive(Clone)]
pub struct SettlementEvidenceVerifier {
    expected_key_id: Arc<str>,
    key: VerifyingKey,
}

impl SettlementEvidenceVerifier {
    pub fn from_public_key_bytes(
        expected_key_id: impl Into<String>,
        public_key: [u8; 32],
    ) -> Result<Self, PayError> {
        let expected_key_id = expected_key_id.into();
        if expected_key_id.trim().is_empty() {
            return Err(PayError::Policy(
                "Mint settlement verifier key id is empty".into(),
            ));
        }
        let key = VerifyingKey::from_bytes(&public_key).map_err(|_| {
            PayError::Policy("Mint settlement verifier public key is invalid".into())
        })?;
        Ok(Self {
            expected_key_id: Arc::from(expected_key_id),
            key,
        })
    }

    /// Authenticate the Mint envelope and bind it to both Relay's receipt and
    /// the funding `payer`.
    ///
    /// The envelope's `request_id` is Relay-supplied, not Mint-signed, so a
    /// misbehaving Relay could otherwise pair a request id with a validly
    /// Mint-signed but *unrelated* settlement attestation. Binding
    /// `payer_commitment` to `hold_party_commitment("payer", payer, op_hash)`
    /// rejects that substitution: only an attestation Mint issued for this
    /// caller's own hold operation can pass.
    fn verify(
        &self,
        receipt: &Receipt,
        attestation: &HoldSettlementAttestation,
        payer: &str,
    ) -> Result<HoldSettlementAttestationClaims, PayError> {
        let claims = attestation
            .verify(&self.expected_key_id, &self.key)
            .map_err(|error| {
                PayError::Gateway(format!(
                    "Mint settlement attestation verification failed: {error}"
                ))
            })?;
        if receipt.seq != claims.sequence
            || receipt.op != claims.operation
            || receipt.op_hash != claims.operation_hash
            || receipt.amount_micros.get() != claims.amount_micros
            || receipt.hash != claims.receipt_hash
        {
            return Err(PayError::Gateway(
                "Mint settlement attestation does not bind Relay's receipt".into(),
            ));
        }
        // The attestation must commit to *this* caller as the payer. Without
        // this check, an unrelated Mint-signed attestation could be relayed as
        // evidence for this request and feed Abacus/Ledgerline a false proof.
        let expected_payer_commitment =
            crate::hold_attestation::hold_party_commitment("payer", payer, &claims.operation_hash);
        if claims.payer_commitment != expected_payer_commitment {
            return Err(PayError::Gateway(
                "Mint settlement attestation is not bound to the funding payer".into(),
            ));
        }
        Ok(claims)
    }
}

/// Principal-scoped evidence returned by a paid HTTP journey. The request id
/// correlates the initial request and retry; the optional receipt is Mint's
/// signed, hash-linked proof of final settlement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaymentEvidence {
    pub request_id: Option<String>,
    /// Authenticated Relay principal that owns the evidence lookup. This may
    /// differ from the funding payer and is absent when only an inline receipt
    /// header was available.
    pub owner_principal: Option<String>,
    pub payer: String,
    pub receipt: Option<Receipt>,
    /// Exact Mint envelope authenticated with the caller-pinned verifier key.
    /// Inline legacy receipt headers have no attestation and leave this empty.
    pub attestation: Option<HoldSettlementAttestation>,
    pub verified_at_ms: Option<i64>,
}

/// Raw HTTP response plus normalized payment evidence for audit/telemetry
/// consumers such as Abacus and Ledgerline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaidHttpResponse {
    pub response: HttpResponse,
    pub evidence: PaymentEvidence,
}

/// Builds real payer-signed authorizations without talking to the settlement
/// rail. Mint verification happens at the protected server after the retry.
pub struct AuthorizationBuilder {
    payer: String,
    signer: Arc<dyn AuthSigner>,
    policy: AuthorizationPolicy,
}

/// Explicit limits on what a protected endpoint may ask the payer to sign.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthorizationPolicy {
    pub expected_payee: String,
    pub resource_prefix: String,
    pub max_amount_micros: crate::AmountMicros,
}

impl AuthorizationPolicy {
    #[must_use]
    pub fn new(
        expected_payee: impl Into<String>,
        resource_prefix: impl Into<String>,
        max_amount_micros: crate::AmountMicros,
    ) -> Self {
        Self {
            expected_payee: expected_payee.into(),
            resource_prefix: resource_prefix.into(),
            max_amount_micros,
        }
    }

    fn validate(&self, challenge: &PaymentChallenge) -> Result<(), PayError> {
        if challenge.payee != self.expected_payee {
            return Err(PayError::Policy("unexpected payee".into()));
        }
        if challenge.currency != "AIT" {
            return Err(PayError::Policy("unexpected currency".into()));
        }
        if challenge.scheme != crate::Scheme::Hold {
            return Err(PayError::Policy("only aitp-hold is allowed".into()));
        }
        if challenge.amount_micros.get() <= 0 || challenge.amount_micros > self.max_amount_micros {
            return Err(PayError::Policy("amount exceeds caller limit".into()));
        }
        let Some(resource_scope) = challenge.resource.strip_prefix(&self.resource_prefix) else {
            return Err(PayError::Policy("unexpected protected resource".into()));
        };
        if resource_scope.is_empty() {
            return Err(PayError::Policy(
                "protected resource is missing its request scope".into(),
            ));
        }
        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_millis().min(i64::MAX as u128) as i64)
            .unwrap_or(i64::MAX);
        if challenge.expiry <= now_ms {
            return Err(PayError::Policy("challenge is expired".into()));
        }
        if challenge.nonce.is_empty() {
            return Err(PayError::Policy("challenge nonce is empty".into()));
        }
        Ok(())
    }
}

impl AuthorizationBuilder {
    #[must_use]
    pub fn new(
        payer: impl Into<String>,
        signer: Arc<dyn AuthSigner>,
        policy: AuthorizationPolicy,
    ) -> Self {
        Self {
            payer: payer.into(),
            signer,
            policy,
        }
    }

    pub fn authorize(
        &self,
        challenge: &PaymentChallenge,
    ) -> Result<PaymentAuthorization, PayError> {
        self.policy.validate(challenge)?;
        let idempotency_key = format!("aitp:{}", challenge.nonce);
        let intent = AuthorizationIntent {
            payer: &self.payer,
            payee: &challenge.payee,
            amount_micros: challenge.amount_micros,
            nonce: &challenge.nonce,
            resource: &challenge.resource,
            expiry: challenge.expiry,
            scheme: challenge.scheme,
            idempotency_key: &idempotency_key,
        };
        let hash = op_hash(&intent);
        Ok(PaymentAuthorization {
            payer: self.payer.clone(),
            payee: challenge.payee.clone(),
            amount_micros: challenge.amount_micros,
            nonce: challenge.nonce.clone(),
            resource: challenge.resource.clone(),
            expiry: challenge.expiry,
            scheme: challenge.scheme,
            idempotency_key,
            pubkey_id: self.signer.pubkey_id(),
            op_hash: op_hash_hex(&hash),
            sig: self.signer.sign_op_hash(&hash),
        })
    }

    #[must_use]
    pub fn payer(&self) -> &str {
        &self.payer
    }
}

/// A transport wrapper that recovers one real AITP-402 challenge per request.
pub struct PaidHttpClient {
    transport: Arc<dyn HttpTransport>,
    authorizer: AuthorizationBuilder,
    evidence_verifier: SettlementEvidenceVerifier,
}

impl PaidHttpClient {
    #[must_use]
    pub fn new(
        transport: Arc<dyn HttpTransport>,
        authorizer: AuthorizationBuilder,
        evidence_verifier: SettlementEvidenceVerifier,
    ) -> Self {
        Self {
            transport,
            authorizer,
            evidence_verifier,
        }
    }

    /// Send `request`, then sign and retry once when the response is a valid
    /// AITP 402. A second 402 is returned to the caller unchanged.
    pub async fn send(&self, request: HttpRequest) -> Result<HttpResponse, PayError> {
        let first = self.transport.send(request.clone()).await?;
        if first.status != 402 {
            return Ok(first);
        }
        let encoded = first.header(HEADER_CHALLENGE).ok_or_else(|| {
            PayError::Codec("402 response missing AITP-Challenge header".to_string())
        })?;
        let challenge = decode_challenge(encoded)?;
        let auth = self.authorizer.authorize(&challenge)?;
        let encoded_auth = encode_authorization(&auth)?;
        let request_id = first
            .header(HEADER_REQUEST_ID)
            .unwrap_or(&challenge.nonce)
            .to_string();
        let retry = request
            .with_replaced_header(HEADER_VERSION, crate::codec::AITP_VERSION)
            .with_replaced_header(HEADER_REQUEST_ID, &request_id)
            .with_replaced_header(HEADER_AUTHORIZATION, encoded_auth);
        let response = self.transport.send(retry).await?;
        if response.header(HEADER_REQUEST_ID).is_some() {
            Ok(response)
        } else {
            Ok(response.with_header(HEADER_REQUEST_ID, request_id))
        }
    }

    /// Stream a protected response without buffering model bytes. The 402
    /// handshake completes before the returned body becomes visible, and a
    /// second 402 is returned unchanged rather than retried again.
    pub async fn send_streaming(
        &self,
        request: HttpRequest,
    ) -> Result<StreamingHttpResponse, PayError> {
        let first = self.transport.send_streaming(request.clone()).await?;
        if first.status != 402 {
            return Ok(first);
        }
        let encoded = first.header(HEADER_CHALLENGE).ok_or_else(|| {
            PayError::Codec("402 response missing AITP-Challenge header".to_string())
        })?;
        let challenge = decode_challenge(encoded)?;
        let auth = self.authorizer.authorize(&challenge)?;
        let encoded_auth = encode_authorization(&auth)?;
        let request_id = first
            .header(HEADER_REQUEST_ID)
            .unwrap_or(&challenge.nonce)
            .to_string();
        let retry = request
            .with_replaced_header(HEADER_VERSION, crate::codec::AITP_VERSION)
            .with_replaced_header(HEADER_REQUEST_ID, &request_id)
            .with_replaced_header(HEADER_AUTHORIZATION, encoded_auth);
        let response = self.transport.send_streaming(retry).await?;
        if response.header(HEADER_REQUEST_ID).is_some() {
            Ok(response)
        } else {
            Ok(response.with_header(HEADER_REQUEST_ID, request_id))
        }
    }

    /// Retrieve the durable, principal-scoped settlement proof for a completed
    /// stream. A missing receipt stays explicit instead of being treated as a
    /// successful paid terminal state.
    pub async fn settlement_evidence(
        &self,
        request_id: &str,
        headers: Vec<(String, String)>,
    ) -> Result<PaymentEvidence, PayError> {
        let lookup = self.lookup_settlement(request_id, headers).await?;
        Ok(PaymentEvidence {
            request_id: Some(request_id.to_string()),
            owner_principal: lookup.as_ref().map(|proof| proof.principal.clone()),
            payer: self.authorizer.payer().to_string(),
            receipt: lookup.as_ref().map(|proof| proof.receipt.clone()),
            attestation: lookup.as_ref().map(|proof| proof.attestation.clone()),
            verified_at_ms: lookup.and_then(|proof| proof.verified_at_ms),
        })
    }

    /// Send with one-shot 402 recovery and normalize the returned correlation
    /// and settlement receipt into a principal-scoped evidence object.
    pub async fn send_with_evidence(
        &self,
        request: HttpRequest,
    ) -> Result<PaidHttpResponse, PayError> {
        let lookup_headers = request.headers.clone();
        let response = self.send(request).await?;
        let request_id = response.header(HEADER_REQUEST_ID).map(str::to_string);
        let mut owner_principal = None;
        let mut attestation = None;
        let mut verified_at_ms = None;
        let mut receipt = response
            .header(HEADER_RECEIPT)
            .map(decode_receipt)
            .transpose()?;
        // An inline receipt header is useful correlation data but does not carry
        // Mint's complete attestation. Every successful correlated response
        // therefore resolves the durable projection and independently verifies
        // it before exposing local evidence. Streaming headers are committed
        // before terminal usage; buffered responses use the same trust rule.
        if response.is_success() {
            if let Some(id) = request_id.as_deref() {
                let proof = self
                    .lookup_settlement(id, lookup_headers)
                    .await?
                    .ok_or_else(|| {
                        PayError::Gateway(
                            "successful paid response has no durable Mint evidence".into(),
                        )
                    })?;
                if receipt
                    .as_ref()
                    .is_some_and(|inline| !same_receipt_projection(inline, &proof.receipt))
                {
                    return Err(PayError::Gateway(
                        "inline receipt conflicts with durable Mint evidence".into(),
                    ));
                }
                owner_principal = Some(proof.principal);
                receipt = Some(proof.receipt);
                attestation = Some(proof.attestation);
                verified_at_ms = proof.verified_at_ms;
            }
        }
        Ok(PaidHttpResponse {
            evidence: PaymentEvidence {
                request_id,
                owner_principal,
                payer: self.authorizer.payer().to_string(),
                receipt,
                attestation,
                verified_at_ms,
            },
            response,
        })
    }

    async fn lookup_settlement(
        &self,
        request_id: &str,
        headers: Vec<(String, String)>,
    ) -> Result<Option<VerifiedSettlementEvidence>, PayError> {
        let mut request = HttpRequest::get(format!("/v1/billing/settlements/{request_id}"));
        request.headers = headers
            .into_iter()
            .filter(|(name, _)| {
                !matches!(
                    name.to_ascii_lowercase().as_str(),
                    "content-length" | "content-type" | "transfer-encoding"
                )
            })
            .collect();
        let response = self.transport.send(request).await?;
        if response.status == 404 {
            return Ok(None);
        }
        if !response.is_success() {
            return Err(PayError::Gateway(format!(
                "settlement evidence lookup returned status {}",
                response.status
            )));
        }
        let envelope: SettlementEvidenceEnvelope = serde_json::from_slice(&response.body)
            .map_err(|error| PayError::Codec(format!("settlement evidence: {error}")))?;
        if envelope.data.request_id != request_id {
            return Err(PayError::Gateway(
                "settlement evidence request id mismatch".into(),
            ));
        }
        let attestation = envelope.data.attestation.ok_or_else(|| {
            PayError::Gateway("settlement evidence omitted Mint attestation".into())
        })?;
        self.evidence_verifier.verify(
            &envelope.data.receipt,
            &attestation,
            self.authorizer.payer(),
        )?;
        Ok(Some(VerifiedSettlementEvidence {
            principal: envelope.data.principal,
            receipt: envelope.data.receipt,
            attestation,
            verified_at_ms: envelope.data.verified_at_ms,
        }))
    }
}

fn same_receipt_projection(left: &Receipt, right: &Receipt) -> bool {
    left.seq == right.seq
        && left.op == right.op
        && left.op_hash == right.op_hash
        && left.amount_micros == right.amount_micros
        && left.hash == right.hash
        && left.sig == right.sig
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use async_trait::async_trait;
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use base64::Engine as _;
    use ed25519_dalek::{Signer as _, SigningKey};
    use sha2::{Digest, Sha256};

    use super::*;
    use crate::codec::{decode_authorization, encode_challenge, encode_receipt, HEADER_REQUEST_ID};
    use crate::{AmountMicros, Ed25519Signer, Receipt, Scheme};

    fn authorizer() -> AuthorizationBuilder {
        AuthorizationBuilder::new(
            "acct_payer",
            Arc::new(Ed25519Signer::from_seed("payer-key", &[7; 32])),
            AuthorizationPolicy::new("ait1qrelay", "", AmountMicros(1_000_000)),
        )
    }

    fn evidence_verifier() -> SettlementEvidenceVerifier {
        SettlementEvidenceVerifier::from_public_key_bytes(
            "mint-test-key",
            SigningKey::from_bytes(&[9; 32]).verifying_key().to_bytes(),
        )
        .expect("test verifier key")
    }

    /// The account `authorizer()` funds from; the attestation must commit to it.
    const TEST_PAYER: &str = "acct_payer";

    fn signed_attestation(receipt: &Receipt) -> HoldSettlementAttestation {
        signed_attestation_for_payer(receipt, TEST_PAYER)
    }

    fn signed_attestation_for_payer(receipt: &Receipt, payer: &str) -> HoldSettlementAttestation {
        let claims = HoldSettlementAttestationClaims {
            version: 1,
            asset: "AIT".into(),
            unit: "micro_usd".into(),
            sequence: receipt.seq,
            operation: receipt.op.clone(),
            operation_hash: receipt.op_hash.clone(),
            amount_micros: receipt.amount_micros.get(),
            receipt_hash: receipt.hash.clone(),
            timestamp_ms: 1_700_000_000_000,
            reference_hash: "55".repeat(32),
            hold_commitment: "66".repeat(32),
            payer_commitment: crate::hold_attestation::hold_party_commitment(
                "payer",
                payer,
                &receipt.op_hash,
            ),
            payee_commitment: "88".repeat(32),
            signer_key_id: "mint-test-key".into(),
        };
        let payload = serde_json::to_vec(&claims).expect("claims serialize");
        let mut hasher = Sha256::new();
        hasher.update(b"mint-hold-settlement-attestation-v1\0");
        hasher.update(&payload);
        let digest: [u8; 32] = hasher.finalize().into();
        let signature = SigningKey::from_bytes(&[9; 32]).sign(&digest);
        HoldSettlementAttestation {
            format: crate::HOLD_SETTLEMENT_ATTESTATION_FORMAT.into(),
            payload_base64url: URL_SAFE_NO_PAD.encode(payload),
            digest_sha256: hex::encode(digest),
            signer_key_id: "mint-test-key".into(),
            signature_hex: hex::encode(signature.to_bytes()),
        }
    }

    fn policy_challenge() -> PaymentChallenge {
        PaymentChallenge {
            payee: "ait1qrelay".into(),
            amount_micros: AmountMicros(42_000),
            currency: "AIT".into(),
            resource: "POST /openai/v1/chat/completions#openrouter/openai/gpt-4o@request".into(),
            nonce: "policy-nonce".into(),
            expiry: i64::MAX,
            scheme: Scheme::Hold,
        }
    }

    #[test]
    fn signer_rejects_challenges_outside_explicit_money_policy() {
        let strict = AuthorizationBuilder::new(
            "acct_payer",
            Arc::new(Ed25519Signer::from_seed("payer-key", &[7; 32])),
            AuthorizationPolicy::new(
                "ait1qrelay",
                "POST /openai/v1/chat/completions#openrouter/openai/gpt-4o@",
                AmountMicros(100_000),
            ),
        );
        assert!(strict.authorize(&policy_challenge()).is_ok());

        let mut invalid = Vec::new();
        let mut challenge = policy_challenge();
        challenge.payee = "ait1qattacker".into();
        invalid.push(challenge);
        let mut challenge = policy_challenge();
        challenge.amount_micros = AmountMicros(100_001);
        invalid.push(challenge);
        let mut challenge = policy_challenge();
        challenge.currency = "USD".into();
        invalid.push(challenge);
        let mut challenge = policy_challenge();
        challenge.scheme = Scheme::Transfer;
        invalid.push(challenge);
        let mut challenge = policy_challenge();
        challenge.resource = "POST /admin/export#secret".into();
        invalid.push(challenge);
        let mut challenge = policy_challenge();
        challenge.resource = "POST /openai/v1/chat/completions#openrouter/openai/gpt-4o@".into();
        invalid.push(challenge);
        let mut challenge = policy_challenge();
        challenge.expiry = 0;
        invalid.push(challenge);
        let mut challenge = policy_challenge();
        challenge.nonce.clear();
        invalid.push(challenge);

        for challenge in invalid {
            assert!(matches!(
                strict.authorize(&challenge),
                Err(PayError::Policy(_))
            ));
        }
    }

    struct ChallengeThenOk {
        seen: Mutex<Vec<HttpRequest>>,
        challenge: PaymentChallenge,
    }

    #[async_trait]
    impl HttpTransport for ChallengeThenOk {
        async fn send(&self, req: HttpRequest) -> Result<HttpResponse, PayError> {
            let mut seen = self.seen.lock().unwrap_or_else(|e| e.into_inner());
            seen.push(req);
            if seen.len() == 1 {
                Ok(HttpResponse::new(402, Vec::new())
                    .with_header(HEADER_CHALLENGE, encode_challenge(&self.challenge)?))
            } else {
                Ok(HttpResponse::new(200, br#"{"ok":true}"#.to_vec()))
            }
        }
    }

    #[tokio::test]
    async fn real_402_is_signed_and_retried_once() {
        let challenge = PaymentChallenge {
            payee: "ait1qrelay".to_string(),
            amount_micros: AmountMicros(42_000),
            currency: "AIT".to_string(),
            resource: "POST /v1/chat/completions#openai/gpt-4o".to_string(),
            nonce: "relay-nonce".to_string(),
            expiry: 9_999_999_999_999,
            scheme: Scheme::Hold,
        };
        let transport = Arc::new(ChallengeThenOk {
            seen: Mutex::new(Vec::new()),
            challenge,
        });
        let client = PaidHttpClient::new(transport.clone(), authorizer(), evidence_verifier());

        let response = client
            .send(
                HttpRequest::post_json(
                    "/v1/chat/completions",
                    br#"{"model":"openai/gpt-4o"}"#.to_vec(),
                )
                .with_header(HEADER_VERSION, "stale-version")
                .with_header(HEADER_REQUEST_ID, "stale-request")
                .with_header(HEADER_AUTHORIZATION, "stale-authorization"),
            )
            .await
            .expect("paid retry succeeds");
        assert_eq!(response.status, 200);
        assert_eq!(response.header(HEADER_REQUEST_ID), Some("relay-nonce"));

        let seen = transport.seen.lock().unwrap_or_else(|e| e.into_inner());
        assert_eq!(seen.len(), 2);
        assert_eq!(seen[0].body, seen[1].body);
        assert_eq!(
            seen[1].header(HEADER_VERSION),
            Some(crate::codec::AITP_VERSION)
        );
        assert_eq!(seen[1].header(HEADER_REQUEST_ID), Some("relay-nonce"));
        for controlled in [HEADER_VERSION, HEADER_REQUEST_ID, HEADER_AUTHORIZATION] {
            assert_eq!(
                seen[1]
                    .headers
                    .iter()
                    .filter(|(name, _)| name == &controlled.to_ascii_lowercase())
                    .count(),
                1,
                "retry must carry one authoritative {controlled} value"
            );
        }
        let encoded = seen[1]
            .headers
            .iter()
            .find(|(name, _)| name == &HEADER_AUTHORIZATION.to_ascii_lowercase())
            .map(|(_, value)| value)
            .expect("retry authorization header");
        let auth = decode_authorization(encoded).expect("authorization decodes");
        assert_eq!(auth.payer, "acct_payer");
        assert_eq!(auth.nonce, "relay-nonce");
        assert_eq!(auth.scheme, Scheme::Hold);
        assert!(!auth.op_hash.is_empty());
        assert!(!auth.sig.is_empty());
    }

    #[tokio::test]
    async fn paid_response_exposes_principal_request_and_signed_receipt() {
        struct ChallengeThenReceipted {
            seen: Mutex<usize>,
            challenge: PaymentChallenge,
            receipt: Receipt,
        }

        #[async_trait]
        impl HttpTransport for ChallengeThenReceipted {
            async fn send(&self, req: HttpRequest) -> Result<HttpResponse, PayError> {
                let mut seen = self.seen.lock().unwrap_or_else(|e| e.into_inner());
                *seen += 1;
                match *seen {
                    1 => Ok(HttpResponse::new(402, Vec::new())
                        .with_header(HEADER_CHALLENGE, encode_challenge(&self.challenge)?)
                        .with_header(HEADER_REQUEST_ID, "request-42")),
                    2 => Ok(HttpResponse::new(200, Vec::new())
                        .with_header(HEADER_REQUEST_ID, "request-42")
                        .with_header(HEADER_RECEIPT, encode_receipt(&self.receipt)?)),
                    3 => {
                        assert_eq!(req.path, "/v1/billing/settlements/request-42");
                        Ok(HttpResponse::new(
                            200,
                            serde_json::to_vec(&serde_json::json!({
                                "data": {
                                    "request_id": "request-42",
                                    "principal": "acct_api_owner",
                                    "receipt": {
                                        "sequence": self.receipt.seq,
                                        "operation": self.receipt.op,
                                        "operation_hash": self.receipt.op_hash,
                                        "amount_micros": self.receipt.amount_micros.get(),
                                        "receipt_hash": self.receipt.hash,
                                        "signature": self.receipt.sig
                                    },
                                    "attestation": signed_attestation(&self.receipt),
                                    "verified_at_ms": 1233,
                                    "settled_at_ms": 1234
                                }
                            }))
                            .expect("evidence serializes"),
                        ))
                    }
                    _ => panic!("unexpected transport call"),
                }
            }
        }

        let challenge = PaymentChallenge {
            payee: "ait1qrelay".to_string(),
            amount_micros: AmountMicros(42_000),
            currency: "AIT".to_string(),
            resource: "POST /v1/chat/completions#openai/gpt-4o".to_string(),
            nonce: "relay-nonce".to_string(),
            expiry: 9_999_999_999_999,
            scheme: Scheme::Hold,
        };
        let receipt = Receipt {
            seq: 9,
            op: "hold.capture".to_string(),
            op_hash: "11".repeat(32),
            amount_micros: AmountMicros(41_000),
            balance_after_micros: AmountMicros(99_000),
            prev_hash: "22".repeat(32),
            hash: "33".repeat(32),
            sig: "44".repeat(64),
        };
        let client = PaidHttpClient::new(
            Arc::new(ChallengeThenReceipted {
                seen: Mutex::new(0),
                challenge,
                receipt: receipt.clone(),
            }),
            authorizer(),
            evidence_verifier(),
        );

        let paid = client
            .send_with_evidence(HttpRequest::post_json("/paid", Vec::new()))
            .await
            .unwrap();
        assert_eq!(paid.response.status, 200);
        assert_eq!(paid.evidence.payer, "acct_payer");
        assert_eq!(
            paid.evidence.owner_principal.as_deref(),
            Some("acct_api_owner")
        );
        assert_eq!(paid.evidence.request_id.as_deref(), Some("request-42"));
        let projected = paid.evidence.receipt.as_ref().expect("verified receipt");
        assert!(same_receipt_projection(projected, &receipt));
        assert_eq!(projected.balance_after_micros, AmountMicros::ZERO);
        assert!(paid.evidence.attestation.is_some());
    }

    #[tokio::test]
    async fn streaming_response_recovers_receipt_from_principal_scoped_lookup() {
        struct StreamingThenEvidence {
            seen: Mutex<Vec<HttpRequest>>,
            challenge: PaymentChallenge,
            receipt: Receipt,
        }

        #[async_trait]
        impl HttpTransport for StreamingThenEvidence {
            async fn send(&self, req: HttpRequest) -> Result<HttpResponse, PayError> {
                let mut seen = self.seen.lock().unwrap_or_else(|e| e.into_inner());
                seen.push(req.clone());
                match seen.len() {
                    1 => Ok(HttpResponse::new(402, Vec::new())
                        .with_header(HEADER_CHALLENGE, encode_challenge(&self.challenge)?)
                        .with_header(HEADER_REQUEST_ID, "11111111-1111-4111-8111-111111111111")),
                    2 => Ok(HttpResponse::new(200, b"data: [DONE]\n\n".to_vec())
                        .with_header(HEADER_REQUEST_ID, "11111111-1111-4111-8111-111111111111")),
                    3 => {
                        assert_eq!(req.method, "GET");
                        assert_eq!(
                            req.path,
                            "/v1/billing/settlements/11111111-1111-4111-8111-111111111111"
                        );
                        assert_eq!(req.header("authorization"), Some("Bearer relay-key"));
                        Ok(HttpResponse::new(
                            200,
                            serde_json::to_vec(&serde_json::json!({
                                "data": {
                                    "request_id": "11111111-1111-4111-8111-111111111111",
                                    "principal": "acct_payer",
                                    "receipt": self.receipt.clone(),
                                    "attestation": signed_attestation(&self.receipt),
                                    "verified_at_ms": 1233,
                                    "settled_at_ms": 1234
                                }
                            }))
                            .unwrap(),
                        ))
                    }
                    _ => panic!("unexpected transport call"),
                }
            }
        }

        let challenge = PaymentChallenge {
            payee: "ait1qrelay".into(),
            amount_micros: AmountMicros(42_000),
            currency: "AIT".into(),
            resource: "POST /v1/chat/completions#openai/gpt-4o".into(),
            nonce: "relay-stream".into(),
            expiry: 9_999_999_999_999,
            scheme: Scheme::Hold,
        };
        let receipt = Receipt {
            seq: 10,
            op: "hold.capture".into(),
            op_hash: "11".repeat(32),
            amount_micros: AmountMicros(40_000),
            balance_after_micros: AmountMicros(60_000),
            prev_hash: "22".repeat(32),
            hash: "33".repeat(32),
            sig: "44".repeat(64),
        };
        let transport = Arc::new(StreamingThenEvidence {
            seen: Mutex::new(Vec::new()),
            challenge,
            receipt: receipt.clone(),
        });
        let client = PaidHttpClient::new(transport.clone(), authorizer(), evidence_verifier());
        let request = HttpRequest::post_json("/v1/chat/completions", Vec::new())
            .with_header("authorization", "Bearer relay-key");
        let paid = client.send_with_evidence(request).await.unwrap();

        assert_eq!(paid.evidence.receipt, Some(receipt));
        assert!(paid.evidence.attestation.is_some());
        assert_eq!(paid.evidence.verified_at_ms, Some(1233));
        assert_eq!(paid.evidence.owner_principal.as_deref(), Some("acct_payer"));
        assert_eq!(transport.seen.lock().unwrap().len(), 3);
    }

    #[test]
    fn pinned_mint_verifier_rejects_a_receipt_not_bound_by_the_attestation() {
        let receipt = Receipt {
            seq: 10,
            op: "hold.capture".into(),
            op_hash: "11".repeat(32),
            amount_micros: AmountMicros(40_000),
            balance_after_micros: AmountMicros::ZERO,
            prev_hash: String::new(),
            hash: "33".repeat(32),
            sig: "44".repeat(64),
        };
        let attestation = signed_attestation(&receipt);
        let mut projected = receipt;
        projected.amount_micros = AmountMicros(40_001);

        let error = evidence_verifier()
            .verify(&projected, &attestation, TEST_PAYER)
            .expect_err("tampered projection must fail");
        assert!(error.to_string().contains("does not bind"));
    }

    #[test]
    fn pinned_mint_verifier_rejects_attestation_bound_to_a_different_payer() {
        let receipt = Receipt {
            seq: 10,
            op: "hold.capture".into(),
            op_hash: "11".repeat(32),
            amount_micros: AmountMicros(40_000),
            balance_after_micros: AmountMicros::ZERO,
            prev_hash: String::new(),
            hash: "33".repeat(32),
            sig: "44".repeat(64),
        };
        // A perfectly Mint-signed attestation whose receipt fields match this
        // request, but which Mint issued for a *different* payer's hold. A
        // misbehaving Relay could relay it under this request's id.
        let substituted = signed_attestation_for_payer(&receipt, "acct_someone_else");

        // Receipt binding passes; the payer binding must still reject it.
        let error = evidence_verifier()
            .verify(&receipt, &substituted, TEST_PAYER)
            .expect_err("attestation bound to a different payer must be rejected");
        assert!(error.to_string().contains("funding payer"));

        // The same attestation is accepted for the payer it was actually issued
        // to, proving the check binds payer identity rather than rejecting all.
        evidence_verifier()
            .verify(&receipt, &substituted, "acct_someone_else")
            .expect("matching payer accepts the Mint-signed attestation");
    }
}
