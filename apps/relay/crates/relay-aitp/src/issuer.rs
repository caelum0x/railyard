use std::sync::Arc;

use aitp_client::{PaymentAuthorization, Scheme};
use relay_contract::{AitpChallenge, AitpScheme, Microdollars};
use uuid::Uuid;

use crate::{
    AitpError, ChallengeRecord, ChallengeState, ChallengeStore, Clock, NonceSource, Result,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChallengePolicy {
    pub payee: String,
    pub ttl_ms: u64,
    pub max_amount_micros: Microdollars,
}

impl ChallengePolicy {
    pub fn new(
        payee: impl Into<String>,
        ttl_ms: u64,
        max_amount_micros: Microdollars,
    ) -> Result<Self> {
        let payee = payee.into();
        if payee.trim().is_empty() || ttl_ms == 0 || max_amount_micros.get() <= 0 {
            return Err(AitpError::InvalidAmount);
        }
        Ok(Self {
            payee,
            ttl_ms,
            max_amount_micros,
        })
    }
}

#[derive(Clone)]
pub struct ChallengeIssuer {
    policy: ChallengePolicy,
    store: Arc<dyn ChallengeStore>,
    clock: Arc<dyn Clock>,
    nonces: Arc<dyn NonceSource>,
}

impl ChallengeIssuer {
    pub fn new(
        policy: ChallengePolicy,
        store: Arc<dyn ChallengeStore>,
        clock: Arc<dyn Clock>,
        nonces: Arc<dyn NonceSource>,
    ) -> Self {
        Self {
            policy,
            store,
            clock,
            nonces,
        }
    }

    pub async fn issue(
        &self,
        owner_principal: impl Into<String>,
        request_id: Uuid,
        amount_micros: Microdollars,
        resource: impl Into<String>,
    ) -> Result<AitpChallenge> {
        if amount_micros.get() <= 0 || amount_micros > self.policy.max_amount_micros {
            return Err(AitpError::InvalidAmount);
        }
        let owner_principal = owner_principal.into();
        let resource = resource.into();
        if owner_principal.trim().is_empty()
            || resource.trim().is_empty()
            || resource.contains(['\r', '\n'])
        {
            return Err(AitpError::InvalidResource);
        }
        let now_ms = self.clock.now_ms();
        let now_u64 = u64::try_from(now_ms).map_err(|_| AitpError::ExpiryOverflow)?;
        let expiry = now_u64
            .checked_add(self.policy.ttl_ms)
            .ok_or(AitpError::ExpiryOverflow)?;
        let challenge = AitpChallenge {
            payee: self.policy.payee.clone(),
            amount_micros,
            currency: "AIT".into(),
            resource,
            nonce: self.nonces.nonce(),
            expiry,
            scheme: AitpScheme::Hold,
        };
        challenge
            .validate()
            .map_err(|_| AitpError::InvalidResource)?;
        self.store
            .issue(ChallengeRecord {
                request_id,
                owner_principal,
                challenge: challenge.clone(),
                state: ChallengeState::Issued,
                issued_at_ms: now_ms,
                updated_at_ms: now_ms,
            })
            .await?;
        Ok(challenge)
    }

    pub async fn authorize(
        &self,
        owner_principal: &str,
        request_id: Uuid,
        authorization: &PaymentAuthorization,
    ) -> Result<ChallengeRecord> {
        let record = self
            .store
            .get(request_id)
            .await?
            .ok_or(AitpError::NotFound)?;
        ensure_matches(&record.challenge, authorization)?;
        self.store
            .authorize(
                owner_principal,
                request_id,
                &authorization.op_hash,
                self.clock.now_ms(),
            )
            .await
    }

    pub async fn consume(
        &self,
        owner_principal: &str,
        request_id: Uuid,
        operation_hash: &str,
        receipt_hash: &str,
    ) -> Result<ChallengeRecord> {
        if receipt_hash.is_empty() {
            return Err(AitpError::AuthorizationMismatch);
        }
        self.store
            .consume(
                owner_principal,
                request_id,
                operation_hash,
                receipt_hash,
                self.clock.now_ms(),
            )
            .await
    }

    pub async fn purge_before(&self, cutoff_ms: i64, limit: usize) -> Result<u64> {
        if limit == 0 {
            return Err(AitpError::InvalidRetentionLimit);
        }
        self.store.purge_before(cutoff_ms, limit).await
    }
}

fn ensure_matches(challenge: &AitpChallenge, authorization: &PaymentAuthorization) -> Result<()> {
    let expiry = i64::try_from(challenge.expiry).map_err(|_| AitpError::AuthorizationMismatch)?;
    if authorization.payee != challenge.payee
        || authorization.amount_micros.get() != challenge.amount_micros.get()
        || authorization.nonce != challenge.nonce
        || authorization.resource != challenge.resource
        || authorization.expiry != expiry
        || authorization.scheme != Scheme::Hold
        || authorization.op_hash.is_empty()
        || authorization.sig.is_empty()
    {
        return Err(AitpError::AuthorizationMismatch);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU64, Ordering};

    use aitp_client::AmountMicros;

    use super::*;
    use crate::InMemoryChallengeStore;

    struct FixedClock(i64);
    impl Clock for FixedClock {
        fn now_ms(&self) -> i64 {
            self.0
        }
    }

    struct CountingNonce(AtomicU64);
    impl NonceSource for CountingNonce {
        fn nonce(&self) -> String {
            format!("nonce-{}", self.0.fetch_add(1, Ordering::SeqCst))
        }
    }

    fn authorization(challenge: &AitpChallenge, hash: &str) -> PaymentAuthorization {
        PaymentAuthorization {
            payer: "acct_payer".into(),
            payee: challenge.payee.clone(),
            amount_micros: AmountMicros::new(challenge.amount_micros.get()),
            nonce: challenge.nonce.clone(),
            resource: challenge.resource.clone(),
            expiry: i64::try_from(challenge.expiry).unwrap(),
            scheme: Scheme::Hold,
            idempotency_key: "idem".into(),
            pubkey_id: "key".into(),
            op_hash: hash.into(),
            sig: "signature".into(),
        }
    }

    fn issuer() -> ChallengeIssuer {
        ChallengeIssuer::new(
            ChallengePolicy::new("ait1qrelay", 300_000, Microdollars::new(1_000_000)).unwrap(),
            Arc::new(InMemoryChallengeStore::default()),
            Arc::new(FixedClock(1_700_000_000_000)),
            Arc::new(CountingNonce(AtomicU64::new(1))),
        )
    }

    #[tokio::test]
    async fn owner_can_authorize_idempotently_and_consume_once() {
        let issuer = issuer();
        let request_id = Uuid::now_v7();
        let challenge = issuer
            .issue(
                "acct_owner",
                request_id,
                Microdollars::new(50),
                "POST /openai/v1/chat/completions#m@id",
            )
            .await
            .unwrap();
        let authorization = authorization(&challenge, "op-hash");
        issuer
            .authorize("acct_owner", request_id, &authorization)
            .await
            .unwrap();
        issuer
            .authorize("acct_owner", request_id, &authorization)
            .await
            .unwrap();
        issuer
            .consume("acct_owner", request_id, "op-hash", "receipt")
            .await
            .unwrap();
        issuer
            .consume("acct_owner", request_id, "op-hash", "receipt")
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn another_principal_and_conflicting_replay_fail_closed() {
        let issuer = issuer();
        let request_id = Uuid::now_v7();
        let challenge = issuer
            .issue(
                "acct_owner",
                request_id,
                Microdollars::new(50),
                "POST /inference#m@id",
            )
            .await
            .unwrap();
        let first_authorization = authorization(&challenge, "first");
        assert_eq!(
            issuer
                .authorize("acct_other", request_id, &first_authorization)
                .await,
            Err(AitpError::Ownership)
        );
        issuer
            .authorize("acct_owner", request_id, &first_authorization)
            .await
            .unwrap();
        let conflict = authorization(&challenge, "second");
        assert_eq!(
            issuer.authorize("acct_owner", request_id, &conflict).await,
            Err(AitpError::ConflictingReplay)
        );
    }

    #[tokio::test]
    async fn stale_challenges_are_purged_in_bounded_batches() {
        let store = Arc::new(InMemoryChallengeStore::default());
        let issuer = ChallengeIssuer::new(
            ChallengePolicy::new("ait1qrelay", 300_000, Microdollars::new(1_000_000)).unwrap(),
            store.clone(),
            Arc::new(FixedClock(1_700_000_000_000)),
            Arc::new(CountingNonce(AtomicU64::new(1))),
        );
        let first = Uuid::now_v7();
        let second = Uuid::now_v7();
        issuer
            .issue(
                "acct_owner",
                first,
                Microdollars::new(1),
                "POST /inference#m",
            )
            .await
            .unwrap();
        issuer
            .issue(
                "acct_owner",
                second,
                Microdollars::new(1),
                "POST /inference#m",
            )
            .await
            .unwrap();

        assert_eq!(issuer.purge_before(i64::MAX, 1).await.unwrap(), 1);
        assert_eq!(issuer.purge_before(i64::MAX, 1).await.unwrap(), 1);
        assert_eq!(issuer.purge_before(i64::MAX, 1).await.unwrap(), 0);
        assert!(store.get(first).await.unwrap().is_none());
        assert!(store.get(second).await.unwrap().is_none());
        assert_eq!(
            issuer.purge_before(i64::MAX, 0).await,
            Err(AitpError::InvalidRetentionLimit)
        );
    }
}
