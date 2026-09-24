//! [`AppState`] — the shared handle every handler receives. Generic over the
//! store `S` so tests inject `InMemoryStore` and production injects `PgStore`,
//! with the four domain facades (ledger, streams, reserve, AITP-402) all driving
//! ONE `Arc`-shared store.

use std::sync::Arc;

use mint_ledger::Ledger;
use mint_payments::{KycOnboarding, PaymentRegistry};
use mint_protocol::{Aitp402, PayerKeys};
use mint_receipts::Signer;
use mint_reserve::{KycGate, Ramp, ReserveLedger};
use mint_store::{Clock, LedgerStore};
use mint_stream::StreamLedger;

use crate::payer_registry::PayerKeyRegistry;
use crate::shared::{DynKyc, DynPayerKeys, DynRamp, SharedStore};

/// Public-facing URLs Mint needs to hand buyers to providers and back.
#[derive(Clone, Debug, Default)]
pub struct PublicConfig {
    /// Mint's own externally reachable base URL (`MINT_PUBLIC_URL`, no trailing
    /// slash). Hosts `/checkout/*` pages and the PayPal capture return.
    pub public_url: Option<String>,
    /// Default post-payment landing page (`MINT_CHECKOUT_SUCCESS_URL`).
    pub checkout_success_url: Option<String>,
    /// Default abandoned-checkout landing page (`MINT_CHECKOUT_CANCEL_URL`).
    pub checkout_cancel_url: Option<String>,
}

impl PublicConfig {
    /// Read from the process environment (empty ⇒ unset, trailing `/` trimmed).
    pub fn from_env() -> Self {
        let get = |k: &str| {
            std::env::var(k)
                .ok()
                .map(|v| v.trim().trim_end_matches('/').to_string())
                .filter(|v| !v.is_empty())
        };
        PublicConfig {
            public_url: get("MINT_PUBLIC_URL"),
            checkout_success_url: get("MINT_CHECKOUT_SUCCESS_URL"),
            checkout_cancel_url: get("MINT_CHECKOUT_CANCEL_URL"),
        }
    }
}

/// Concrete AITP-402 state-machine type used by the server (shared store,
/// dyn clock, shared payer keys, in-memory nonce store).
pub type ServerAitp<S> = Aitp402<SharedStore<S>, Arc<dyn Clock>, DynPayerKeys>;

/// Concrete reserve-ledger type used by the server.
pub type ServerReserve<S> = ReserveLedger<SharedStore<S>, DynRamp, DynKyc>;

/// The shared, cheaply-cloneable application state.
pub struct AppState<S: LedgerStore> {
    /// Transfers / holds / allowances / accounts / addresses / receipts.
    pub ledger: Arc<Ledger<SharedStore<S>>>,
    /// Metered payment streams.
    pub streams: Arc<StreamLedger<SharedStore<S>>>,
    /// Deposit-gated mint / KYC-gated redeem / solvency.
    pub reserve: Arc<ServerReserve<S>>,
    /// AITP-402 challenge / verify / settle / capture / release.
    pub aitp: Arc<ServerAitp<S>>,
    /// The master API key (full authority; constant-time compared).
    pub api_key: Arc<String>,
    /// Named, scoped service keys (`MINT_API_KEYS_JSON`).
    pub scoped_keys: Arc<Vec<crate::auth::ScopedKey>>,
    /// The Stripe webhook signing secret (HMAC-SHA256 over the raw body).
    pub stripe_secret: Arc<String>,
    /// The BYOK provider registry driving the per-provider on-ramp webhooks.
    /// `None` when payments are unconfigured (dev/tests) ⇒ every webhook route
    /// returns `"provider not configured"`.
    pub payments: Option<Arc<PaymentRegistry>>,
    /// Receipt/attestation signer. Callers pass the same signer to the store,
    /// so ledger receipts and transfer attestations share one trust root.
    pub receipt_signer: Arc<dyn Signer>,
    /// Public URLs for hosted checkout return paths.
    pub public: Arc<PublicConfig>,
    /// The live KYC provider used to start verifications and report status.
    /// `None` ⇒ no provider configured (redemptions stay denied).
    pub kyc_onboarding: Option<Arc<dyn KycOnboarding>>,
    /// AITP-402 payer keys: the DB-backed mirror (managed via
    /// `/v1/payer-keys`) layered over the static bootstrap set. The AITP
    /// verifier resolves keys through this registry.
    pub payer_keys: Arc<PayerKeyRegistry>,
}

impl<S: LedgerStore> Clone for AppState<S> {
    fn clone(&self) -> Self {
        AppState {
            ledger: self.ledger.clone(),
            streams: self.streams.clone(),
            reserve: self.reserve.clone(),
            aitp: self.aitp.clone(),
            api_key: self.api_key.clone(),
            scoped_keys: self.scoped_keys.clone(),
            stripe_secret: self.stripe_secret.clone(),
            payments: self.payments.clone(),
            receipt_signer: self.receipt_signer.clone(),
            public: self.public.clone(),
            kyc_onboarding: self.kyc_onboarding.clone(),
            payer_keys: self.payer_keys.clone(),
        }
    }
}

impl<S: LedgerStore + 'static> AppState<S> {
    /// Assemble the state from one store plus the injected external seams. The
    /// store is `Arc`-shared across all four facades so they see one ledger.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        store: S,
        clock: Arc<dyn Clock>,
        ramp: Arc<dyn Ramp>,
        kyc: Arc<dyn KycGate>,
        payer_keys: Arc<dyn PayerKeys>,
        receipt_signer: Arc<dyn Signer>,
        api_key: impl Into<String>,
        stripe_secret: impl Into<String>,
    ) -> Self {
        let shared = SharedStore::new(store);

        let ledger = Arc::new(Ledger::new(shared.clone()));
        let streams = Arc::new(StreamLedger::new(shared.clone()));
        let reserve = Arc::new(ReserveLedger::new(
            shared.clone(),
            DynRamp(ramp),
            DynKyc(kyc),
        ));
        let payer_registry = Arc::new(PayerKeyRegistry::new(payer_keys));
        let aitp = Arc::new(Aitp402::new(
            Ledger::new(shared.clone()),
            clock,
            DynPayerKeys(payer_registry.clone()),
        )
        // Hold settlements get their auto-release deadline in the same
        // transaction as the hold itself.
        .with_hold_ttl_ms(crate::jobs::aitp_hold_ttl_ms()));

        AppState {
            ledger,
            streams,
            reserve,
            aitp,
            api_key: Arc::new(api_key.into()),
            scoped_keys: Arc::new(Vec::new()),
            stripe_secret: Arc::new(stripe_secret.into()),
            payments: None,
            receipt_signer,
            public: Arc::new(PublicConfig::default()),
            kyc_onboarding: None,
            payer_keys: payer_registry,
        }
    }

    /// Install named, scoped service keys alongside the master key.
    pub fn with_scoped_keys(mut self, keys: Vec<crate::auth::ScopedKey>) -> Self {
        self.scoped_keys = Arc::new(keys);
        self
    }

    /// Attach the public checkout URLs (from [`PublicConfig::from_env`]).
    pub fn with_public_config(mut self, public: PublicConfig) -> Self {
        self.public = Arc::new(public);
        self
    }

    /// Attach the live KYC provider for onboarding + status reads.
    pub fn with_kyc_onboarding(mut self, kyc: Arc<dyn KycOnboarding>) -> Self {
        self.kyc_onboarding = Some(kyc);
        self
    }

    /// Attach the BYOK payment registry that drives the on-ramp webhook routes.
    /// Called by the binary after building the registry from the environment;
    /// tests leave it unset (webhook routes then report "not configured").
    pub fn with_payments(mut self, payments: Arc<PaymentRegistry>) -> Self {
        self.payments = Some(payments);
        self
    }
}
