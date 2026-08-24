use vend_core::error::VendError;
use vend_core::ids::EndUserId;
use vend_core::money::FundingMode;
use vend_core::records::KeyScope;
use vend_tenancy::{resolve, EndUser};
use vend_testkit::fixtures::two_tenants;

#[tokio::test]
async fn disabled_app_and_unknown_end_user_fail_before_funding() {
    let fixture = two_tenants();
    assert_eq!(
        resolve(&fixture.store, &fixture.a.key, Some("unknown-customer"))
            .await
            .unwrap_err(),
        VendError::NotFound
    );

    let mut disabled = fixture.a.app.clone();
    disabled.enabled = false;
    fixture.store.add_app(disabled);
    assert_eq!(
        resolve(&fixture.store, &fixture.a.key, None)
            .await
            .unwrap_err(),
        VendError::Unauthorized
    );
}

#[tokio::test]
async fn end_user_reserve_never_falls_back_to_app_account() {
    let fixture = two_tenants();
    let mut app = fixture.a.app.clone();
    app.markup_policy.funding_mode = FundingMode::EndUserReserve;
    fixture.store.add_app(app);

    assert_eq!(
        resolve(&fixture.store, &fixture.a.key, None)
            .await
            .unwrap_err(),
        VendError::NotFound
    );
}

#[tokio::test]
async fn key_cannot_cross_tenant_app_ownership() {
    let fixture = two_tenants();
    let mut key = fixture.a.key.clone();
    key.app_id = Some(fixture.b.app.id.clone());
    assert_eq!(
        resolve(&fixture.store, &key, None).await.unwrap_err(),
        VendError::Unauthorized
    );
}

#[tokio::test]
async fn end_user_key_resolves_bound_typed_id_and_checks_external_ref() {
    let fixture = two_tenants();
    let end_user = EndUser {
        id: EndUserId::generate(),
        app_id: fixture.a.app.id.clone(),
        external_ref: "customer-123".into(),
        mint_account_id: Some("acct_customer_123".into()),
    };
    fixture.store.add_end_user(end_user.clone());
    let mut key = fixture.a.key.clone();
    key.scope = KeyScope::EndUser;
    key.end_user_id = Some(end_user.id.clone());

    let resolved = resolve(&fixture.store, &key, Some("customer-123"))
        .await
        .unwrap();
    assert_eq!(resolved.end_user.unwrap().id, end_user.id);
    assert_eq!(
        resolve(&fixture.store, &key, Some("someone-else"))
            .await
            .unwrap_err(),
        VendError::Unauthorized
    );
}
