//! [`AppState`] — the shared handle every handler receives. Generic over the
//! store `S` so tests inject `InMemoryStore` and production injects `PgStore`,
//! with the four domain facades (ledger, streams, reserve, AITP-402) all driving
//! ONE `Arc`-shared store.

use std::sync::Arc;

use mint_ledger::Ledger;
use mint_payments::PaymentRegistry;
use mint_protocol::{Aitp402, PayerKeys};
use mint_receipts::Signer;
use mint_reserve::{KycGate, Ramp, ReserveLedger};
use mint_store::{Clock, LedgerStore};
use mint_stream::StreamLedger;

use crate::shared::{DynKyc, DynPayerKeys, DynRamp, SharedStore};

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
    /// The API key required on every non-health route (constant-time compared).
    pub api_key: Arc<String>,
    /// The Stripe webhook signing secret (HMAC-SHA256 over the raw body).
    pub stripe_secret: Arc<String>,
    /// The BYOK provider registry driving the per-provider on-ramp webhooks.
    /// `None` when payments are unconfigured (dev/tests) ⇒ every webhook route
    /// returns `"provider not configured"`.
    pub payments: Option<Arc<PaymentRegistry>>,
    /// Receipt/attestation signer. Callers pass the same signer to the store,
    /// so ledger receipts and transfer attestations share one trust root.
    pub receipt_signer: Arc<dyn Signer>,
}

impl<S: LedgerStore> Clone for AppState<S> {
    fn clone(&self) -> Self {
        AppState {
            ledger: self.ledger.clone(),
            streams: self.streams.clone(),
            reserve: self.reserve.clone(),
            aitp: self.aitp.clone(),
            api_key: self.api_key.clone(),
            stripe_secret: self.stripe_secret.clone(),
            payments: self.payments.clone(),
            receipt_signer: self.receipt_signer.clone(),
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
        let aitp = Arc::new(Aitp402::new(
            Ledger::new(shared.clone()),
            clock,
            DynPayerKeys(payer_keys),
        ));

        AppState {
            ledger,
            streams,
            reserve,
            aitp,
            api_key: Arc::new(api_key.into()),
            stripe_secret: Arc::new(stripe_secret.into()),
            payments: None,
            receipt_signer,
        }
    }

    /// Attach the BYOK payment registry that drives the on-ramp webhook routes.
    /// Called by the binary after building the registry from the environment;
    /// tests leave it unset (webhook routes then report "not configured").
    pub fn with_payments(mut self, payments: Arc<PaymentRegistry>) -> Self {
        self.payments = Some(payments);
        self
    }
}
