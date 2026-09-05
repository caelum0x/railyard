//! Live-DB tests for slice-4: catalog v2 + auth/RBAC. Auth tests exercise the
//! real argon2 hash/verify + JWT round-trip: register→login, wrong-password
//! rejection, invite→accept→membership, password reset. Runs on an isolated
//! migrated Postgres via `#[sqlx::test(migrator = "rago_db::MIGRATOR")]`.

use async_graphql::{Request, Variables};
use chrono::Utc;
use rago_core::models as m;
use rago_db::Db;
use rago_graphql::{build_schema, Principal, RagoSchema};
use serde_json::json;
use sqlx::PgPool;
use uuid::Uuid;

fn org() -> m::Organization {
    let now = Utc::now();
    m::Organization {
        id: Uuid::new_v4(),
        name: "Acme".into(),
        slug: format!("acme-{}", &Uuid::new_v4().to_string()[..8]),
        default_currency: "USD".into(),
        email: None,
        country: None,
        tax_identification_number: None,
        timezone: "UTC".into(),
        document_locale: "en".into(),
        finalize_zero_amount_invoice: true,
        hmac_key: Some(Uuid::new_v4().to_string()),
        created_at: now,
        updated_at: now,
    }
}

fn plan(org: Uuid, code: &str) -> m::Plan {
    let now = Utc::now();
    m::Plan {
        id: Uuid::new_v4(),
        organization_id: org,
        name: "Starter".into(),
        code: code.into(),
        interval: Some(rago_core::enums::PlanInterval::Monthly),
        description: None,
        amount_cents: Some(1000),
        amount_currency: "USD".into(),
        trial_period: None,
        pay_in_advance: false,
        bill_charges_monthly: None,
        parent_id: None,
        invoice_display_name: None,
        created_at: now,
        updated_at: now,
        deleted_at: None,
    }
}

async fn setup(pool: PgPool) -> (RagoSchema, Db, m::Organization) {
    let db = Db::new(pool.clone());
    let organization = org();
    db.insert_organization(&organization).await.unwrap();
    let schema = build_schema(Db::new(pool));
    (schema, db, organization)
}

/// Execute authenticated (org principal injected).
async fn run(schema: &RagoSchema, org: &m::Organization, doc: &str, vars: serde_json::Value) -> async_graphql::Response {
    let req = Request::new(doc).variables(Variables::from_json(vars)).data(Principal { organization: org.clone() });
    schema.execute(req).await
}

/// Execute unauthenticated (no principal) — for register/login/accept.
async fn run_anon(schema: &RagoSchema, doc: &str, vars: serde_json::Value) -> async_graphql::Response {
    schema.execute(Request::new(doc).variables(Variables::from_json(vars))).await
}

fn data(resp: &async_graphql::Response) -> serde_json::Value {
    assert!(resp.errors.is_empty(), "graphql errors: {:?}", resp.errors);
    serde_json::to_value(&resp.data).unwrap()
}

// ---- auth: register / login round-trip ------------------------------------

#[sqlx::test(migrator = "rago_db::MIGRATOR")]
async fn register_login_roundtrip(pool: PgPool) {
    let db = Db::new(pool.clone());
    let schema = build_schema(Db::new(pool));

    // register creates user + org + membership, returns a signed token
    let resp = run_anon(
        &schema,
        r#"mutation { registerUser(email: "owner@acme.test", password: "hunter2pw", organizationName: "Acme Inc") { token user { id email } } }"#,
        json!({}),
    )
    .await;
    let d = data(&resp);
    assert_eq!(d["registerUser"]["user"]["email"], "owner@acme.test");
    let token = d["registerUser"]["token"].as_str().unwrap();
    assert_eq!(token.split('.').count(), 3); // real JWT

    // stored digest is an argon2 hash, never the plaintext
    let (_, _, digest) = db.find_user_by_email("owner@acme.test").await.unwrap().unwrap();
    let digest = digest.unwrap();
    assert!(digest.starts_with("$argon2"));
    assert!(!digest.contains("hunter2pw"));

    // login with correct password succeeds
    let resp = run_anon(&schema, r#"mutation { loginUser(email: "owner@acme.test", password: "hunter2pw") { token user { email } } }"#, json!({})).await;
    assert_eq!(data(&resp)["loginUser"]["user"]["email"], "owner@acme.test");

    // login with wrong password is rejected
    let resp = run_anon(&schema, r#"mutation { loginUser(email: "owner@acme.test", password: "wrong") { token } }"#, json!({})).await;
    assert!(!resp.errors.is_empty());
    assert!(resp.errors[0].message.contains("incorrect_login_or_password"));
}

// ---- auth: password reset --------------------------------------------------

#[sqlx::test(migrator = "rago_db::MIGRATOR")]
async fn password_reset_flow(pool: PgPool) {
    let db = Db::new(pool.clone());
    let schema = build_schema(Db::new(pool.clone()));
    run_anon(&schema, r#"mutation { registerUser(email: "u@acme.test", password: "oldpass12", organizationName: "Acme") { token } }"#, json!({})).await;

    // request reset
    let resp = run_anon(&schema, r#"mutation { createPasswordReset(email: "u@acme.test") }"#, json!({})).await;
    data(&resp);
    // token is emailed out of band; read it from the DB for the test
    let token: String = sqlx::query_scalar("SELECT token FROM password_resets ORDER BY created_at DESC LIMIT 1").fetch_one(&db.pool).await.unwrap();

    // reset password and log in
    let resp = run_anon(&schema, r#"mutation($t: String!) { resetPassword(token: $t, newPassword: "newpass34") { token user { email } } }"#, json!({ "t": token })).await;
    assert_eq!(data(&resp)["resetPassword"]["user"]["email"], "u@acme.test");

    // old password no longer works, new one does
    let resp = run_anon(&schema, r#"mutation { loginUser(email: "u@acme.test", password: "oldpass12") { token } }"#, json!({})).await;
    assert!(!resp.errors.is_empty());
    let resp = run_anon(&schema, r#"mutation { loginUser(email: "u@acme.test", password: "newpass34") { token } }"#, json!({})).await;
    assert!(resp.errors.is_empty());
}

// ---- RBAC: roles + invites -> membership -----------------------------------

#[sqlx::test(migrator = "rago_db::MIGRATOR")]
async fn roles_invites_and_membership(pool: PgPool) {
    let (schema, db, org) = setup(pool).await;

    // create a custom role with a permission set
    let resp = run(
        &schema,
        &org,
        r#"mutation { createRole(input: { code: "finance", name: "Finance", permissions: ["invoices:view", "credit_notes:create"] }) { id code permissions } }"#,
        json!({}),
    )
    .await;
    let d = data(&resp);
    assert_eq!(d["createRole"]["permissions"].as_array().unwrap().len(), 2);
    let role_id = d["createRole"]["id"].as_str().unwrap().to_string();
    let resp = run(&schema, &org, r#"mutation($id: ID!) { updateRole(input: { id: $id, permissions: ["invoices:view"] }) { permissions } }"#, json!({ "id": role_id })).await;
    assert_eq!(data(&resp)["updateRole"]["permissions"].as_array().unwrap().len(), 1);

    // create an invite
    let resp = run(&schema, &org, r#"mutation { createInvite(email: "member@acme.test", roles: ["finance"]) { id token status } }"#, json!({})).await;
    let d = data(&resp);
    assert_eq!(d["createInvite"]["status"], "pending");
    let token = d["createInvite"]["token"].as_str().unwrap().to_string();

    // accept the invite (unauthenticated) → creates user + membership
    let resp = run_anon(
        &schema,
        r#"mutation($t: String!) { acceptInvite(email: "member@acme.test", password: "memberpw1", token: $t) { token user { email } } }"#,
        json!({ "t": token }),
    )
    .await;
    assert_eq!(data(&resp)["acceptInvite"]["user"]["email"], "member@acme.test");

    // membership now exists in the org; invite marked accepted
    let members = db.list_memberships(org.id).await.unwrap();
    assert_eq!(members.len(), 1);
    assert_eq!(members[0].roles, json!(["finance"]));
    let inv = db.find_invite_by_token(&token).await.unwrap().unwrap();
    assert_eq!(inv.status, "accepted");

    // query memberships + roles
    let resp = run(&schema, &org, r#"{ memberships { status } roles { code } }"#, json!({})).await;
    let d = data(&resp);
    assert_eq!(d["memberships"].as_array().unwrap().len(), 1);
    assert_eq!(d["roles"][0]["code"], "finance");

    // revoke membership
    let mid = members[0].id.to_string();
    let resp = run(&schema, &org, r#"mutation($id: ID!) { revokeMembership(id: $id) { status } }"#, json!({ "id": mid })).await;
    assert_eq!(data(&resp)["revokeMembership"]["status"], "revoked");
}

// ---- api keys: rotate + destroy -------------------------------------------

#[sqlx::test(migrator = "rago_db::MIGRATOR")]
async fn api_key_rotate_and_destroy(pool: PgPool) {
    let (schema, db, org) = setup(pool).await;
    let key = m::ApiKey {
        id: Uuid::new_v4(),
        organization_id: org.id,
        value: "rago_original".into(),
        name: Some("prod".into()),
        permissions: json!({}),
        expires_at: None,
        last_used_at: None,
        created_at: Utc::now(),
        updated_at: Utc::now(),
    };
    db.insert_api_key(&key).await.unwrap();

    // rotate: old expired, new minted (different value, name carried)
    let resp = run(&schema, &org, r#"mutation($id: ID!) { rotateApiKey(id: $id) { id value name } }"#, json!({ "id": key.id.to_string() })).await;
    let d = data(&resp);
    assert_ne!(d["rotateApiKey"]["value"], "rago_original");
    assert_eq!(d["rotateApiKey"]["name"], "prod");
    // old key is now expired
    let expired: Option<chrono::NaiveDateTime> = sqlx::query_scalar("SELECT expires_at FROM api_keys WHERE id = $1").bind(key.id).fetch_one(&db.pool).await.unwrap();
    assert!(expired.is_some());

    // destroy the new key
    let new_id = d["rotateApiKey"]["id"].as_str().unwrap().to_string();
    let resp = run(&schema, &org, r#"mutation($id: ID!) { destroyApiKey(id: $id) }"#, json!({ "id": new_id })).await;
    data(&resp);
}

// ---- catalog v2: products + rate cards + pricing units + commitments ------

#[sqlx::test(migrator = "rago_db::MIGRATOR")]
async fn catalog_v2_crud(pool: PgPool) {
    let (schema, db, org) = setup(pool).await;

    // product category + product
    let resp = run(&schema, &org, r#"mutation { createProductCategory(input: { code: "saas", name: "SaaS" }) { id } }"#, json!({})).await;
    let cat = data(&resp)["createProductCategory"]["id"].as_str().unwrap().to_string();
    let resp = run(
        &schema,
        &org,
        r#"mutation($cat: ID!) { createProduct(input: { code: "seat", name: "Seat", productType: "standard", productCategoryId: $cat }) { id code productType } }"#,
        json!({ "cat": cat }),
    )
    .await;
    let d = data(&resp);
    assert_eq!(d["createProduct"]["productType"], "standard");
    let product = d["createProduct"]["id"].as_str().unwrap().to_string();

    // product filter
    let resp = run(&schema, &org, r#"mutation($p: ID!) { createProductFilter(input: { productId: $p, code: "region", name: "Region", values: ["eu","us"] }) { id code } }"#, json!({ "p": product })).await;
    data(&resp);
    let resp = run(&schema, &org, r#"query($p: ID!) { productFilters(productId: $p) { code } }"#, json!({ "p": product })).await;
    assert_eq!(data(&resp)["productFilters"][0]["code"], "region");

    // rate card + rate
    let resp = run(
        &schema,
        &org,
        r#"mutation($p: ID!) { createRateCard(input: { productId: $p, code: "rc1", name: "Rate Card", currency: "USD", proration: true }) { id proration } }"#,
        json!({ "p": product }),
    )
    .await;
    let d = data(&resp);
    assert_eq!(d["createRateCard"]["proration"], true);
    let rc = d["createRateCard"]["id"].as_str().unwrap().to_string();
    let resp = run(&schema, &org, r#"mutation($rc: ID!) { createRateCardRate(input: { rateCardId: $rc, chargeModel: "standard", properties: { amount: "5" } }) { id chargeModel } }"#, json!({ "rc": rc })).await;
    assert_eq!(data(&resp)["createRateCardRate"]["chargeModel"], "standard");
    assert_eq!(db.list_rate_card_rates(Uuid::parse_str(&rc).unwrap()).await.unwrap().len(), 1);

    // plan-applied rate card + rate phase
    let p = plan(org.id, "starter");
    db.insert_plan(&p).await.unwrap();
    let resp = run(&schema, &org, r#"mutation($plan: ID!, $rc: ID!) { createPlanAppliedRateCard(input: { planId: $plan, rateCardId: $rc }) { id } }"#, json!({ "plan": p.id.to_string(), "rc": rc })).await;
    let parc = data(&resp)["createPlanAppliedRateCard"]["id"].as_str().unwrap().to_string();
    let resp = run(&schema, &org, r#"mutation($id: ID!) { createRatePhase(input: { planAppliedRateCardId: $id, code: "phase1", position: 1 }) { code position } }"#, json!({ "id": parc })).await;
    assert_eq!(data(&resp)["createRatePhase"]["position"], 1);

    // pricing unit
    let resp = run(&schema, &org, r#"mutation { createPricingUnit(input: { code: "credits", name: "Credits", shortName: "cr" }) { code shortName } }"#, json!({})).await;
    assert_eq!(data(&resp)["createPricingUnit"]["shortName"], "cr");

    // commitment on the plan
    let resp = run(&schema, &org, r#"mutation($plan: ID!) { createCommitment(input: { planId: $plan, amountCents: 100000 }) { id amountCents commitmentType } }"#, json!({ "plan": p.id.to_string() })).await;
    let d = data(&resp);
    assert_eq!(d["createCommitment"]["amountCents"], 100000);
    assert_eq!(d["createCommitment"]["commitmentType"], "minimum_commitment");

    // list queries
    let resp = run(&schema, &org, r#"{ products { code } rateCards { code } pricingUnits { code } }"#, json!({})).await;
    let d = data(&resp);
    assert_eq!(d["products"].as_array().unwrap().len(), 1);
    assert_eq!(d["rateCards"].as_array().unwrap().len(), 1);
    assert_eq!(d["pricingUnits"].as_array().unwrap().len(), 1);
}

// ---- billing entity taxes --------------------------------------------------

#[sqlx::test(migrator = "rago_db::MIGRATOR")]
async fn billing_entity_apply_taxes(pool: PgPool) {
    let (schema, db, org) = setup(pool).await;
    // seed a billing entity + a tax
    let be = Uuid::new_v4();
    db.insert_billing_entity(be, org.id, "eu", "EU", None, None, "EUR", "UTC").await.unwrap();
    db.insert_tax(Uuid::new_v4(), org.id, "VAT", "vat", None, 20.0, false).await.unwrap();

    let resp = run(&schema, &org, r#"mutation($be: ID!) { applyBillingEntityTaxes(billingEntityId: $be, taxCodes: ["vat"]) { code rate } }"#, json!({ "be": be.to_string() })).await;
    let d = data(&resp);
    assert_eq!(d["applyBillingEntityTaxes"][0]["code"], "vat");
    assert_eq!(db.billing_entity_taxes(be).await.unwrap().len(), 1);

    let resp = run(&schema, &org, r#"mutation($be: ID!) { removeBillingEntityTaxes(billingEntityId: $be, taxCodes: ["vat"]) { code } }"#, json!({ "be": be.to_string() })).await;
    assert_eq!(data(&resp)["removeBillingEntityTaxes"].as_array().unwrap().len(), 0);
}
