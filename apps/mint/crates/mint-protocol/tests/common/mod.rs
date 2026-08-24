//! Deterministic test harness for the AITP-402 handshake: `FixedClock` +
//! `FixedSigner` (receipts) + a distinct `Ed25519Signer` (payer) + `InMemoryStore`
//! + `InMemoryNonceStore` + `InMemoryPayerKeys`. No live network, no randomness.

#![allow(dead_code)]

use std::sync::Arc;

use mint_ledger::{
    AccountId, AccountKind, Address, Ed25519Signer, FixedSigner, InMemoryStore, Ledger, Micros,
};
use mint_protocol::{Aitp402, InMemoryNonceStore, InMemoryPayerKeys};
use mint_store::FixedClock;

/// The genesis test timestamp (unix ms).
pub const START_TS: i64 = 1_700_000_000_000;

/// A distinct 32-byte seed for the PAYER key (different from the receipt
/// signer's `FIXED_SEED`, so the two keyspaces stay visibly separate).
pub const PAYER_SEED: [u8; 32] = [
    0x03, 0x14, 0x15, 0x92, 0x65, 0x35, 0x89, 0x79, 0x32, 0x38, 0x46, 0x26, 0x43, 0x38, 0x32, 0x79,
    0x50, 0x28, 0x84, 0x19, 0x71, 0x69, 0x39, 0x93, 0x75, 0x10, 0x58, 0x20, 0x97, 0x49, 0x44, 0x59,
];

/// The payer key id stamped into every authorization's `pubkey_id`.
pub const PAYER_KEY_ID: &str = "payer-key-1";

/// The concrete state-machine type under test.
pub type Proto = Aitp402<InMemoryStore, Arc<FixedClock>, InMemoryPayerKeys, InMemoryNonceStore>;

/// A wired-up settlement engine plus the identities a test drives it with.
pub struct Harness {
    /// The state machine under test.
    pub proto: Proto,
    /// The paying (agent) account, richly seeded.
    pub payer: AccountId,
    /// The receiving (provider) account.
    pub provider: AccountId,
    /// The provider's registered receive address.
    pub payee_addr: Address,
    /// The payer's signing key (also registered in the payer-key registry).
    pub payer_signer: Ed25519Signer,
    /// The shared clock (advance it to drive expiry/window tests).
    pub clock: Arc<FixedClock>,
}

/// Microdollars the payer is seeded with (1000 AIT).
pub const PAYER_SEED_MICROS: i64 = 1_000_000_000;

/// Build a fully-wired harness with one seeded payer and one provider address.
pub async fn setup() -> Harness {
    let clock = Arc::new(FixedClock::new(START_TS));
    let store = InMemoryStore::with_clock_and_signer(clock.clone(), Arc::new(FixedSigner::new()));
    let ledger = Ledger::new(store);

    let payer = ledger
        .create_account(AccountKind::Agent, None)
        .await
        .unwrap();
    ledger
        .store()
        .seed_account(&payer, Micros::new(PAYER_SEED_MICROS))
        .await
        .unwrap();
    let provider = ledger
        .create_account(AccountKind::Provider, None)
        .await
        .unwrap();
    let payee_addr = ledger
        .create_address(&provider, Some("payout".into()))
        .await
        .unwrap();

    let payer_signer = Ed25519Signer::from_seed(PAYER_SEED, PAYER_KEY_ID);
    let keys = InMemoryPayerKeys::with_payer_signer(payer.as_str(), &payer_signer);
    let nonces = InMemoryNonceStore::new(clock.clone());
    let proto = Aitp402::new(ledger, clock.clone(), keys, nonces);

    Harness {
        proto,
        payer,
        provider,
        payee_addr,
        payer_signer,
        clock,
    }
}
