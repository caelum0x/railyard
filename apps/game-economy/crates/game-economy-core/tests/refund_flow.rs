//! Refund / revoke flow: money returns through Mint exactly once, the grant is
//! removed, the purchase is marked, and a refunded single-owner good can be
//! bought again.

use game_economy_core::{Catalog, GameError, GameStore, ReverseOutcome};
use game_economy_testkit::{Call, FakeMint};

const PLAYER: &str = "acct_player";
const REVENUE: &str = "acct_revenue";

#[tokio::test]
async fn refund_returns_money_once_and_frees_the_good() {
    let mint = FakeMint::new().with_balance(PLAYER, 10_000_000);
    let store = GameStore::new(mint, Catalog::demo(), REVENUE);
    let out = store
        .purchase(PLAYER, "skin.aeon.blade", "buy-skin")
        .await
        .unwrap();

    let refund = store
        .refund(PLAYER, &out.receipt_id, Some("support"))
        .await
        .unwrap();
    assert!(!refund.idempotent_replay);
    assert_eq!(refund.purchase.status(), "refunded");
    assert_eq!(store.mint().balance(PLAYER), 10_000_000);
    assert_eq!(store.mint().balance(REVENUE), 0);
    assert!(store.inventory(PLAYER).cosmetics.is_empty());

    // Second refund replays; Mint is not asked to pay again.
    let again = store.refund(PLAYER, &out.receipt_id, None).await.unwrap();
    assert!(again.idempotent_replay);
    assert_eq!(
        store
            .mint()
            .count_calls(|c| matches!(c, Call::Refund { .. })),
        1
    );

    // History shows the status; the good can be bought again with a new key.
    assert_eq!(store.purchase_history(PLAYER).unwrap()[0].status(), "refunded");
    store
        .purchase(PLAYER, "skin.aeon.blade", "buy-skin-2")
        .await
        .expect("refunded good is buyable again");
}

#[tokio::test]
async fn revoked_purchase_cannot_be_refunded() {
    let mint = FakeMint::new().with_balance(PLAYER, 10_000_000);
    let store = GameStore::new(mint, Catalog::demo(), REVENUE);
    let out = store.purchase(PLAYER, "pack.standard", "buy-1").await.unwrap();

    match store
        .revoke_entitlement(PLAYER, &out.receipt_id, Some("chargeback"))
        .unwrap()
    {
        ReverseOutcome::Reversed(p) => assert_eq!(p.status(), "revoked"),
        other => panic!("expected Reversed, got {other:?}"),
    }
    assert!(matches!(
        store.revoke_entitlement(PLAYER, &out.receipt_id, None).unwrap(),
        ReverseOutcome::AlreadyReversed(_)
    ));
    assert_eq!(
        store.refund(PLAYER, &out.receipt_id, None).await.unwrap_err(),
        GameError::AlreadyReversed(out.receipt_id.clone())
    );
    assert_eq!(store.summary().unwrap().revoked_count, 1);
}

#[tokio::test]
async fn refund_of_unknown_receipt_is_not_found() {
    let store = GameStore::new(FakeMint::new(), Catalog::demo(), REVENUE);
    assert_eq!(
        store.refund(PLAYER, "nope", None).await.unwrap_err(),
        GameError::PurchaseNotFound("nope".into())
    );
}
