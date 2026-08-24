//! Property storms mixing AITP-402 authorizations (transfer + hold/capture +
//! hold/release) with delegated allowance spends (transfer + hold), all driven
//! through the ONE ledger apply path. After any random interleaving the crate
//! invariants must hold:
//!
//! - live balances == balances rebuilt by folding the journal;
//! - live receipts == receipts rebuilt from the journal (bit-exact) — chain
//!   integrity;
//! - the whole signed receipt log verifies offline;
//! - value is CONSERVED: `Σ (available + held)` over all accounts equals the
//!   fixed seeded supply (every op is either a within-account reservation or a
//!   double-entry move, so no op mints or burns).

use std::sync::Arc;

use mint_ledger::{
    AccountId, AccountKind, Address, AllowanceScheme, AllowanceSpendRequest, AllowanceWindow,
    Ed25519Signer, FixedSigner, InMemoryStore, Keyring, Ledger, LedgerStore, Micros,
};
use mint_protocol::{
    build_authorization, Aitp402, InMemoryNonceStore, InMemoryPayerKeys, MintError, Scheme,
    Settlement,
};
use mint_store::FixedClock;
use proptest::prelude::*;

const NUM_PROVIDERS: usize = 3;
const SEED_MICROS: i64 = 1_000_000_000; // 1000 AIT
const UNIT: i64 = 1_000_000; // 1 AIT
const ALLOWANCE_LIMIT: i64 = 100 * UNIT; // 100 AIT delegated budget
const TTL: i64 = 10_000_000;
const PAYER_SEED: [u8; 32] = [7u8; 32];

#[derive(Debug, Clone)]
enum Op {
    Transfer { p: usize, units: i64 },
    HoldCapture { p: usize, units: i64 },
    HoldRelease { p: usize, units: i64 },
    AllowanceTransfer { p: usize, units: i64 },
    AllowanceHold { p: usize, units: i64 },
}

fn op_strategy() -> impl Strategy<Value = Op> {
    (0usize..5, 0usize..NUM_PROVIDERS, 1i64..14).prop_map(|(kind, p, units)| match kind {
        0 => Op::Transfer { p, units },
        1 => Op::HoldCapture { p, units },
        2 => Op::HoldRelease { p, units },
        3 => Op::AllowanceTransfer { p, units },
        _ => Op::AllowanceHold { p, units },
    })
}

/// Errors an over-eager random op may legitimately hit; anything else is a bug.
fn is_expected(err: &MintError) -> bool {
    matches!(
        err,
        MintError::InsufficientFunds | MintError::AllowanceExceeded
    )
}

type Proto = Aitp402<InMemoryStore, Arc<FixedClock>, InMemoryPayerKeys, InMemoryNonceStore>;

struct World {
    proto: Proto,
    payer: AccountId,
    providers: Vec<AccountId>,
    addrs: Vec<Address>,
    allowance: mint_ledger::AllowanceId,
    signer: Ed25519Signer,
    seq: u64,
}

async fn build_world() -> World {
    let clock = Arc::new(FixedClock::new(1_700_000_000_000));
    let store = InMemoryStore::with_clock_and_signer(clock.clone(), Arc::new(FixedSigner::new()));
    let ledger = Ledger::new(store);

    let payer = ledger
        .create_account(AccountKind::Agent, None)
        .await
        .unwrap();
    ledger
        .store()
        .seed_account(&payer, Micros::new(SEED_MICROS))
        .await
        .unwrap();

    let mut providers = Vec::new();
    let mut addrs = Vec::new();
    for i in 0..NUM_PROVIDERS {
        let p = ledger
            .create_account(AccountKind::Provider, None)
            .await
            .unwrap();
        let a = ledger
            .create_address(&p, Some(format!("payout-{i}")))
            .await
            .unwrap();
        providers.push(p);
        addrs.push(a);
    }

    let signer = Ed25519Signer::from_seed(PAYER_SEED, "payer-key-1");
    let keys = InMemoryPayerKeys::with_payer_signer(payer.as_str(), &signer);
    let nonces = InMemoryNonceStore::new(clock.clone());
    let proto = Aitp402::new(ledger, clock, keys, nonces);

    let allowance = proto
        .ledger()
        .grant_allowance(mint_ledger::AllowanceGrantRequest {
            granter: payer.clone(),
            to_key: "delegate".into(),
            limit_micros: ALLOWANCE_LIMIT,
            window: AllowanceWindow::PerCall,
            scope: None,
            idem_key: "grant-1".into(),
        })
        .await
        .unwrap();

    World {
        proto,
        payer,
        providers,
        addrs,
        allowance,
        signer,
        seq: 0,
    }
}

impl World {
    fn tick(&mut self) -> u64 {
        let s = self.seq;
        self.seq += 1;
        s
    }

    async fn apply(&mut self, op: Op) {
        match op {
            Op::Transfer { p, units } => {
                let n = self.tick();
                let challenge = self.proto.issue_challenge_with_nonce(
                    &format!("nonce-{n}"),
                    &self.addrs[p],
                    units * UNIT,
                    "res:transfer",
                    Scheme::Transfer,
                    TTL,
                );
                let auth = build_authorization(
                    self.payer.as_str(),
                    &challenge,
                    &format!("idem-{n}"),
                    &self.signer,
                );
                self.expect_settle(self.proto.verify_and_settle(&auth).await);
            }
            Op::HoldCapture { p, units } => {
                let n = self.tick();
                let challenge = self.proto.issue_challenge_with_nonce(
                    &format!("nonce-{n}"),
                    &self.addrs[p],
                    units * UNIT,
                    "res:hold",
                    Scheme::Hold,
                    TTL,
                );
                let auth = build_authorization(
                    self.payer.as_str(),
                    &challenge,
                    &format!("idem-{n}"),
                    &self.signer,
                );
                if let Some(hold_id) = self.settle_hold(self.proto.verify_and_settle(&auth).await) {
                    // Capture roughly half the reservation (releases the rest).
                    let capture = (units * UNIT) / 2 + 1;
                    let r = self
                        .proto
                        .capture(
                            hold_id,
                            capture,
                            self.providers[p].clone(),
                            format!("cap-{n}"),
                        )
                        .await;
                    assert!(r.is_ok(), "capture failed: {:?}", r.err());
                }
            }
            Op::HoldRelease { p, units } => {
                let n = self.tick();
                let challenge = self.proto.issue_challenge_with_nonce(
                    &format!("nonce-{n}"),
                    &self.addrs[p],
                    units * UNIT,
                    "res:hold",
                    Scheme::Hold,
                    TTL,
                );
                let auth = build_authorization(
                    self.payer.as_str(),
                    &challenge,
                    &format!("idem-{n}"),
                    &self.signer,
                );
                if let Some(hold_id) = self.settle_hold(self.proto.verify_and_settle(&auth).await) {
                    let r = self.proto.release(hold_id, format!("rel-{n}")).await;
                    assert!(r.is_ok(), "release failed: {:?}", r.err());
                }
            }
            Op::AllowanceTransfer { p, units } => {
                let n = self.tick();
                let r = self
                    .proto
                    .ledger()
                    .spend_allowance(AllowanceSpendRequest {
                        allowance_id: self.allowance.clone(),
                        amount_micros: units * UNIT,
                        to: self.providers[p].clone(),
                        scheme: AllowanceScheme::Transfer,
                        ref_: None,
                        idem_key: format!("aspend-{n}"),
                    })
                    .await;
                if let Err(e) = r {
                    assert!(is_expected(&e), "unexpected allowance error: {e:?}");
                }
            }
            Op::AllowanceHold { p, units } => {
                let n = self.tick();
                let r = self
                    .proto
                    .ledger()
                    .spend_allowance(AllowanceSpendRequest {
                        allowance_id: self.allowance.clone(),
                        amount_micros: units * UNIT,
                        to: self.providers[p].clone(),
                        scheme: AllowanceScheme::Hold,
                        ref_: None,
                        idem_key: format!("aspend-{n}"),
                    })
                    .await;
                if let Err(e) = r {
                    assert!(is_expected(&e), "unexpected allowance error: {e:?}");
                }
            }
        }
    }

    fn expect_settle(&self, r: Result<Settlement, MintError>) {
        if let Err(e) = r {
            assert!(is_expected(&e), "unexpected settle error: {e:?}");
        }
    }

    /// Extract the hold id from a hold settlement, tolerating an expected
    /// funding failure (in which case no hold was created).
    fn settle_hold(&self, r: Result<Settlement, MintError>) -> Option<mint_core::HoldId> {
        match r {
            Ok(Settlement::Hold { hold_id, .. }) => Some(hold_id),
            Ok(Settlement::Transfer { .. }) => panic!("hold challenge settled as transfer"),
            Err(e) => {
                assert!(is_expected(&e), "unexpected hold settle error: {e:?}");
                None
            }
        }
    }

    async fn assert_invariants(&self) {
        let store = self.proto.ledger().store();

        // 1. Live balance projection == journal rebuild (bit-for-bit).
        let live = store.balances_snapshot();
        let rebuilt = store.rebuild_balances().await.unwrap();
        assert_eq!(live, rebuilt, "live balances != rebuild");

        // 2. Live receipts == journal-rebuilt receipts (chain integrity).
        let live_r = store.receipts_snapshot();
        let rebuilt_r = store.rebuild_receipts().await.unwrap();
        assert_eq!(
            rebuilt_r.global(),
            live_r.as_slice(),
            "receipt rebuild drift"
        );

        // 3. The whole signed log verifies offline.
        self.proto
            .ledger()
            .verify_receipts(&Keyring::with_signer(&FixedSigner::new()))
            .await
            .expect("receipt log verifies");

        // 4. Conservation: Σ (available + held) == the seeded supply.
        let total: i64 = live
            .values()
            .map(|b| b.available_micros.get() + b.held_micros.get())
            .sum();
        assert_eq!(total, SEED_MICROS, "value not conserved");
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(48))]

    #[test]
    fn storm_preserves_conservation_and_chain_integrity(
        ops in proptest::collection::vec(op_strategy(), 0..40)
    ) {
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        rt.block_on(async {
            let mut world = build_world().await;
            for op in ops {
                world.apply(op).await;
            }
            world.assert_invariants().await;
        });
    }
}
