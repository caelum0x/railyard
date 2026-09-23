//! GraphQL admin mutations — slice 3: the external-service config cluster.
//! Payment-provider configs, provider-customer connections, payment methods,
//! manual payments + payment requests, and integrations orchestration (config
//! rows + collection/resource mappings + customer connections + fetch/sync).
//!
//! These resolvers persist CONFIG and MAPPINGS. Where a real external call is
//! required (fetch items/accounts, mint a checkout URL, sync to a provider) the
//! resolver does the honest local part (persist config, enqueue the sync job, or
//! read the local cache) and never fabricates a remote response — the live call
//! runs in `rago-integrations` with owner-provisioned credentials.

use async_graphql::{Context, Object, Result, ID};
use chrono::Utc;
use serde_json::json;
use uuid::Uuid;

use crate::admin_inputs3::*;
use crate::context::{db, require_org};
use crate::types::Integration;
use crate::types_admin2::Payment;
use crate::types_admin3::*;

fn uuid(id: &ID) -> Result<Uuid> {
    Uuid::parse_str(id.as_str()).map_err(|_| async_graphql::Error::new("invalid_id"))
}

/// Mask a secret to a `****last4` reference; never stores the raw value.
fn mask(secret: &Option<String>) -> Option<String> {
    secret.as_deref().map(|v| {
        if v.len() <= 4 {
            "****".to_string()
        } else {
            format!("****{}", &v[v.len() - 4..])
        }
    })
}

/// Seal and store a provider secret submitted from the dashboard (the display
/// column keeps only `****last4`). Fails with a clear configuration error when
/// `RAGO_ENCRYPTION_KEY` is not set, rather than dropping the credential.
async fn persist_secret(ctx: &Context<'_>, provider_id: &ID, secret: &Option<String>) -> Result<()> {
    let Some(secret) = secret.as_deref().filter(|v| !v.is_empty()) else {
        return Ok(());
    };
    let id = uuid(provider_id)?;
    db(ctx)
        .store_payment_provider_secret(id, secret)
        .await
        .map_err(|e| async_graphql::Error::new(match e {
            rago_db::StoreSecretError::Secret(rago_db::secrets::SecretError::KeyMissing) => {
                "encryption_key_missing: set RAGO_ENCRYPTION_KEY to store provider credentials".to_string()
            }
            other => other.to_string(),
        }))
}

/// Seal and store a provider's inbound webhook signing secret.
async fn persist_webhook_secret(ctx: &Context<'_>, provider_id: &ID, secret: &Option<String>) -> Result<()> {
    let Some(secret) = secret.as_deref().map(str::trim).filter(|v| !v.is_empty()) else {
        return Ok(());
    };
    db(ctx)
        .store_payment_provider_webhook_secret(uuid(provider_id)?, secret)
        .await
        .map_err(|e| async_graphql::Error::new(match e {
            rago_db::StoreSecretError::Secret(rago_db::secrets::SecretError::KeyMissing) => {
                "encryption_key_missing: set RAGO_ENCRYPTION_KEY to store provider credentials".to_string()
            }
            other => other.to_string(),
        }))
}

/// Seal an integration credential for storage in the integration's settings
/// (`None` when not provided). Fails with a clear configuration error when
/// `RAGO_ENCRYPTION_KEY` is not set rather than dropping the credential.
fn seal_setting(secret: &Option<String>) -> Result<Option<String>> {
    let Some(v) = secret.as_deref().map(str::trim).filter(|v| !v.is_empty()) else {
        return Ok(None);
    };
    rago_db::secrets::seal(v).map(Some).map_err(|e| async_graphql::Error::new(match e {
        rago_db::secrets::SecretError::KeyMissing => {
            "encryption_key_missing: set RAGO_ENCRYPTION_KEY to store integration credentials".to_string()
        }
        other => other.to_string(),
    }))
}

/// Re-read a provider after its secrets were stored so masked refs are current.
async fn reload(ctx: &Context<'_>, provider: PaymentProvider) -> Result<PaymentProvider> {
    Ok(db(ctx).find_payment_provider(uuid(&provider.id)?).await?.map(Into::into).unwrap_or(provider))
}

/// Providers Rago can actually collect with (charge, refund, inbound webhooks).
const UNSUPPORTED_PROVIDER: &str =
    "payment_provider_not_supported: Rago collects payments with Stripe, GoCardless or Adyen";

/// Admin CRUD mutation root — slice 3 (merged into the schema's `Mutation`).
#[derive(Default)]
pub struct AdminMutation3;

#[Object]
impl AdminMutation3 {
    // ---- payment providers ----------------------------------------------

    /// `AddStripePaymentProvider` / `UpdateStripePaymentProvider`.
    async fn add_stripe_payment_provider(&self, ctx: &Context<'_>, input: StripePaymentProviderInput) -> Result<PaymentProvider> {
        let org = require_org(ctx)?;
        let settings = json!({
            "supports_3ds": input.supports_3ds.unwrap_or(false),
            "require_terms_of_service_consent": input.require_terms_of_service_consent.unwrap_or(false),
        });
        let row = db(ctx)
            .upsert_payment_provider(org.id, "stripe", &input.code, Some(&input.name), mask(&input.secret_key).as_deref(), input.success_redirect_url.as_deref(), &settings)
            .await?;
        let provider: PaymentProvider = row.into();
        persist_secret(ctx, &provider.id, &input.secret_key).await?;
        persist_webhook_secret(ctx, &provider.id, &input.webhook_secret).await?;
        reload(ctx, provider).await
    }
    async fn update_stripe_payment_provider(&self, ctx: &Context<'_>, input: StripePaymentProviderInput) -> Result<PaymentProvider> {
        self.add_stripe_payment_provider(ctx, input).await
    }

    /// `AddAdyenPaymentProvider` / `UpdateAdyenPaymentProvider`.
    async fn add_adyen_payment_provider(&self, ctx: &Context<'_>, input: AdyenPaymentProviderInput) -> Result<PaymentProvider> {
        let org = require_org(ctx)?;
        let settings = json!({
            "merchant_account": input.merchant_account,
            "live_prefix": input.live_prefix,
        });
        let row = db(ctx)
            .upsert_payment_provider(org.id, "adyen", &input.code, Some(&input.name), mask(&input.api_key).as_deref(), input.success_redirect_url.as_deref(), &settings)
            .await?;
        let provider: PaymentProvider = row.into();
        persist_secret(ctx, &provider.id, &input.api_key).await?;
        // The HMAC key verifies inbound Adyen notifications for this org.
        persist_webhook_secret(ctx, &provider.id, &input.hmac_key).await?;
        reload(ctx, provider).await
    }
    async fn update_adyen_payment_provider(&self, ctx: &Context<'_>, input: AdyenPaymentProviderInput) -> Result<PaymentProvider> {
        self.add_adyen_payment_provider(ctx, input).await
    }

    /// `AddGocardlessPaymentProvider` / `UpdateGocardlessPaymentProvider`.
    async fn add_gocardless_payment_provider(&self, ctx: &Context<'_>, input: GocardlessPaymentProviderInput) -> Result<PaymentProvider> {
        let org = require_org(ctx)?;
        let row = db(ctx)
            .upsert_payment_provider(org.id, "gocardless", &input.code, Some(&input.name), mask(&input.access_code).as_deref(), input.success_redirect_url.as_deref(), &json!({}))
            .await?;
        let provider: PaymentProvider = row.into();
        persist_secret(ctx, &provider.id, &input.access_code).await?;
        persist_webhook_secret(ctx, &provider.id, &input.webhook_secret).await?;
        reload(ctx, provider).await
    }
    async fn update_gocardless_payment_provider(&self, ctx: &Context<'_>, input: GocardlessPaymentProviderInput) -> Result<PaymentProvider> {
        self.add_gocardless_payment_provider(ctx, input).await
    }

    /// `AddCashfreePaymentProvider`.
    async fn add_cashfree_payment_provider(&self, ctx: &Context<'_>, input: CashfreePaymentProviderInput) -> Result<PaymentProvider> {
        // Rago has no cashfree collection, refund or webhook integration; accepting
        // the credentials would leave invoices silently uncollected.
        require_org(ctx)?;
        let _ = input;
        Err(async_graphql::Error::new(UNSUPPORTED_PROVIDER))
    }

    /// `AddFlutterwavePaymentProvider`.
    async fn add_flutterwave_payment_provider(&self, ctx: &Context<'_>, input: FlutterwavePaymentProviderInput) -> Result<PaymentProvider> {
        // Rago has no flutterwave collection, refund or webhook integration; accepting
        // the credentials would leave invoices silently uncollected.
        require_org(ctx)?;
        let _ = input;
        Err(async_graphql::Error::new(UNSUPPORTED_PROVIDER))
    }

    /// `AddMoneyhashPaymentProvider`.
    async fn add_moneyhash_payment_provider(&self, ctx: &Context<'_>, input: MoneyhashPaymentProviderInput) -> Result<PaymentProvider> {
        // Rago has no moneyhash collection, refund or webhook integration; accepting
        // the credentials would leave invoices silently uncollected.
        require_org(ctx)?;
        let _ = input;
        Err(async_graphql::Error::new(UNSUPPORTED_PROVIDER))
    }

    /// `DestroyPaymentProvider`.
    async fn destroy_payment_provider(&self, ctx: &Context<'_>, id: ID) -> Result<ID> {
        require_org(ctx)?;
        let uid = uuid(&id)?;
        db(ctx).soft_delete_payment_provider(uid).await?;
        Ok(uid.into())
    }

    // ---- payment provider customers -------------------------------------

    /// `CreatePaymentProviderCustomer`.
    async fn create_payment_provider_customer(&self, ctx: &Context<'_>, input: CreatePaymentProviderCustomerInput) -> Result<PaymentProviderCustomer> {
        let org = require_org(ctx)?;
        let customer = db(ctx)
            .find_customer(uuid(&input.customer_id)?)
            .await?
            .filter(|c| c.organization_id == org.id)
            .ok_or_else(|| async_graphql::Error::new("customer_not_found"))?;
        let row = db(ctx)
            .insert_payment_provider_customer(Uuid::new_v4(), org.id, customer.id, &input.payment_provider, input.provider_customer_id.as_deref(), input.payment_provider_code.as_deref())
            .await?;
        Ok(row.into())
    }

    /// `UpdatePaymentProviderCustomer`.
    async fn update_payment_provider_customer(&self, ctx: &Context<'_>, input: UpdatePaymentProviderCustomerInput) -> Result<PaymentProviderCustomer> {
        require_org(ctx)?;
        let row = db(ctx)
            .update_payment_provider_customer(uuid(&input.id)?, input.provider_customer_id.as_deref(), input.payment_provider_code.as_deref())
            .await?
            .ok_or_else(|| async_graphql::Error::new("payment_provider_customer_not_found"))?;
        Ok(row.into())
    }

    /// `DestroyPaymentProviderCustomer`.
    async fn destroy_payment_provider_customer(&self, ctx: &Context<'_>, id: ID) -> Result<ID> {
        require_org(ctx)?;
        let uid = uuid(&id)?;
        db(ctx).soft_delete_payment_provider_customer(uid).await?;
        Ok(uid.into())
    }

    /// `SetPaymentProviderCustomerAsDefault`.
    async fn set_payment_provider_customer_as_default(&self, ctx: &Context<'_>, id: ID) -> Result<PaymentProviderCustomer> {
        require_org(ctx)?;
        let row = db(ctx)
            .set_default_payment_provider_customer(uuid(&id)?)
            .await?
            .ok_or_else(|| async_graphql::Error::new("payment_provider_customer_not_found"))?;
        Ok(row.into())
    }

    // ---- payment methods -------------------------------------------------

    /// `DestroyPaymentMethod`.
    async fn destroy_payment_method(&self, ctx: &Context<'_>, id: ID) -> Result<ID> {
        require_org(ctx)?;
        let uid = uuid(&id)?;
        db(ctx).soft_delete_payment_method(uid).await?;
        Ok(uid.into())
    }

    /// `SetPaymentMethodAsDefault`.
    async fn set_payment_method_as_default(&self, ctx: &Context<'_>, id: ID) -> Result<PaymentMethod> {
        require_org(ctx)?;
        let row = db(ctx)
            .set_default_payment_method(uuid(&id)?)
            .await?
            .ok_or_else(|| async_graphql::Error::new("payment_method_not_found"))?;
        Ok(row.into())
    }

    /// `GenerateCheckoutUrl` (`Customers::GenerateCheckoutUrlService`) — the URL
    /// is minted by the configured provider; absent a provider config it is
    /// `null` (the live call runs in rago-integrations with owner credentials).
    async fn generate_checkout_url(&self, ctx: &Context<'_>, customer_id: ID) -> Result<CheckoutUrl> {
        let org = require_org(ctx)?;
        let customer = db(ctx)
            .find_customer(uuid(&customer_id)?)
            .await?
            .filter(|c| c.organization_id == org.id)
            .ok_or_else(|| async_graphql::Error::new("customer_not_found"))?;
        let transport = rago_integrations::default_transport();
        match rago_jobs::collection::checkout_url(db(ctx), &transport, &customer).await {
            Ok(Ok(url)) => Ok(CheckoutUrl { checkout_url: Some(url) }),
            Ok(Err(code)) => Err(async_graphql::Error::new(code)),
            Err(e) => Err(async_graphql::Error::new(e.to_string())),
        }
    }

    // ---- payments + payment requests ------------------------------------

    /// `CreatePayment` (`Payments::ManualCreateService`).
    async fn create_payment(&self, ctx: &Context<'_>, input: CreatePaymentInput) -> Result<Payment> {
        let org = require_org(ctx)?;
        let invoice = db(ctx)
            .find_invoice(uuid(&input.invoice_id)?)
            .await?
            .filter(|i| i.organization_id == org.id)
            .ok_or_else(|| async_graphql::Error::new("invoice_not_found"))?;
        let currency = invoice.currency.clone().unwrap_or_else(|| "USD".into());
        let id = Uuid::new_v4();
        db(ctx)
            .insert_payment(id, org.id, invoice.id, "manual", Some(&input.reference), Some("succeeded"), "succeeded", input.amount_cents, &currency)
            .await?;
        Ok(Payment::from((id, "manual".to_string(), "succeeded".to_string(), input.amount_cents, currency)))
    }

    /// `CreatePaymentRequest` (`PaymentRequests::CreateService`).
    async fn create_payment_request(&self, ctx: &Context<'_>, input: CreatePaymentRequestInput) -> Result<PaymentRequest> {
        let org = require_org(ctx)?;
        let customer = db(ctx).find_customer_by_external_id(org.id, &input.external_customer_id).await?;
        let row = db(ctx)
            .insert_payment_request(
                Uuid::new_v4(),
                org.id,
                customer.as_ref().map(|c| c.id),
                &input.external_customer_id,
                input.email.as_deref(),
                input.amount_cents.unwrap_or(0),
                input.amount_currency.as_deref(),
            )
            .await?;
        Ok(row.into())
    }

    // ---- integrations (config) ------------------------------------------

    /// `CreateNetsuiteIntegration`.
    async fn create_netsuite_integration(&self, ctx: &Context<'_>, input: CreateNetsuiteIntegrationInput) -> Result<Integration> {
        let org = require_org(ctx)?;
        let settings = json!({
            "account_id": input.account_id,
            "client_id": input.client_id,
            "client_secret_ref": mask(&input.client_secret),
            "client_secret_sealed": seal_setting(&input.client_secret)?,
            "connection_id": input.connection_id,
            "script_endpoint_url": input.script_endpoint_url,
            "token_id": input.token_id,
            "token_secret_ref": mask(&input.token_secret),
            "token_secret_sealed": seal_setting(&input.token_secret)?,
            "sync_invoices": input.sync_invoices.unwrap_or(false),
            "sync_credit_notes": input.sync_credit_notes.unwrap_or(false),
            "sync_payments": input.sync_payments.unwrap_or(false),
        });
        let row = db(ctx).insert_integration(Uuid::new_v4(), org.id, "accounting", "netsuite", Some(&input.code), Some(&input.name), &settings).await?;
        Ok(row.into())
    }

    /// `CreateXeroIntegration`.
    async fn create_xero_integration(&self, ctx: &Context<'_>, input: CreateXeroIntegrationInput) -> Result<Integration> {
        let org = require_org(ctx)?;
        let settings = json!({
            "connection_id": input.connection_id,
            "sync_invoices": input.sync_invoices.unwrap_or(false),
            "sync_credit_notes": input.sync_credit_notes.unwrap_or(false),
            "sync_payments": input.sync_payments.unwrap_or(false),
        });
        let row = db(ctx).insert_integration(Uuid::new_v4(), org.id, "accounting", "xero", Some(&input.code), Some(&input.name), &settings).await?;
        Ok(row.into())
    }

    /// `CreateAnrokIntegration`.
    async fn create_anrok_integration(&self, ctx: &Context<'_>, input: CreateAnrokIntegrationInput) -> Result<Integration> {
        let org = require_org(ctx)?;
        let settings = json!({
            "api_key_ref": mask(&input.api_key),
            "api_key_sealed": seal_setting(&input.api_key)?,
            "connection_id": input.connection_id,
        });
        let row = db(ctx).insert_integration(Uuid::new_v4(), org.id, "tax", "anrok", Some(&input.code), Some(&input.name), &settings).await?;
        Ok(row.into())
    }

    /// `CreateAvalaraIntegration`.
    async fn create_avalara_integration(&self, ctx: &Context<'_>, input: CreateAvalaraIntegrationInput) -> Result<Integration> {
        let org = require_org(ctx)?;
        let settings = json!({
            "account_id": input.account_id,
            "license_key_ref": mask(&input.license_key),
            "license_key_sealed": seal_setting(&input.license_key)?,
            "company_code": input.company_code,
            "connection_id": input.connection_id,
        });
        let row = db(ctx).insert_integration(Uuid::new_v4(), org.id, "tax", "avalara", Some(&input.code), Some(&input.name), &settings).await?;
        Ok(row.into())
    }

    /// `CreateHubspotIntegration`.
    async fn create_hubspot_integration(&self, ctx: &Context<'_>, input: CreateHubspotIntegrationInput) -> Result<Integration> {
        let org = require_org(ctx)?;
        let settings = json!({
            "connection_id": input.connection_id,
            "default_targeted_object": input.default_targeted_object,
            "sync_invoices": input.sync_invoices.unwrap_or(false),
            "sync_subscriptions": input.sync_subscriptions.unwrap_or(false),
        });
        let row = db(ctx).insert_integration(Uuid::new_v4(), org.id, "crm", "hubspot", Some(&input.code), Some(&input.name), &settings).await?;
        Ok(row.into())
    }

    /// `CreateSalesforceIntegration`.
    async fn create_salesforce_integration(&self, ctx: &Context<'_>, input: CreateSalesforceIntegrationInput) -> Result<Integration> {
        let org = require_org(ctx)?;
        let settings = json!({ "instance_id": input.instance_id, "connection_id": input.connection_id });
        let row = db(ctx).insert_integration(Uuid::new_v4(), org.id, "crm", "salesforce", Some(&input.code), Some(&input.name), &settings).await?;
        Ok(row.into())
    }

    /// `CreateOktaIntegration`.
    async fn create_okta_integration(&self, ctx: &Context<'_>, input: CreateOktaIntegrationInput) -> Result<Integration> {
        let org = require_org(ctx)?;
        let settings = json!({
            "domain": input.domain,
            "organization_name": input.organization_name,
            "client_id": input.client_id,
            "client_secret_ref": mask(&input.client_secret),
            "client_secret_sealed": seal_setting(&input.client_secret)?,
        });
        let code = input.code.clone().unwrap_or_else(|| input.domain.clone());
        let row = db(ctx).insert_integration(Uuid::new_v4(), org.id, "auth", "okta", Some(&code), input.name.as_deref(), &settings).await?;
        Ok(row.into())
    }

    /// `Update*Integration` (generic — updates the code/name; provider-specific
    /// setting updates re-POST the create input).
    async fn update_integration(&self, ctx: &Context<'_>, input: UpdateIntegrationInput) -> Result<Integration> {
        require_org(ctx)?;
        let row = db(ctx)
            .update_integration(uuid(&input.id)?, input.code.as_deref(), input.name.as_deref(), None)
            .await?
            .ok_or_else(|| async_graphql::Error::new("integration_not_found"))?;
        Ok(row.into())
    }

    /// `DestroyIntegration`.
    async fn destroy_integration(&self, ctx: &Context<'_>, id: ID) -> Result<ID> {
        require_org(ctx)?;
        let uid = uuid(&id)?;
        db(ctx).soft_delete_integration(uid).await?;
        Ok(uid.into())
    }

    // ---- integration collection mappings --------------------------------

    /// `CreateIntegrationCollectionMapping`.
    async fn create_integration_collection_mapping(&self, ctx: &Context<'_>, input: CreateIntegrationCollectionMappingInput) -> Result<IntegrationCollectionMapping> {
        let org = require_org(ctx)?;
        let row = db(ctx)
            .insert_integration_collection_mapping(Uuid::new_v4(), org.id, uuid(&input.integration_id)?, &input.mapping_type, input.external_id.as_deref(), input.external_account_code.as_deref(), input.external_name.as_deref())
            .await?;
        Ok(row.into())
    }

    /// `UpdateIntegrationCollectionMapping`.
    async fn update_integration_collection_mapping(&self, ctx: &Context<'_>, input: UpdateIntegrationCollectionMappingInput) -> Result<IntegrationCollectionMapping> {
        require_org(ctx)?;
        let row = db(ctx)
            .update_integration_collection_mapping(uuid(&input.id)?, input.external_id.as_deref(), input.external_account_code.as_deref(), input.external_name.as_deref())
            .await?
            .ok_or_else(|| async_graphql::Error::new("integration_collection_mapping_not_found"))?;
        Ok(row.into())
    }

    /// `DestroyIntegrationCollectionMapping`.
    async fn destroy_integration_collection_mapping(&self, ctx: &Context<'_>, id: ID) -> Result<ID> {
        require_org(ctx)?;
        let uid = uuid(&id)?;
        db(ctx).soft_delete_integration_collection_mapping(uid).await?;
        Ok(uid.into())
    }

    // ---- integration mappings -------------------------------------------

    /// `CreateIntegrationMapping`.
    async fn create_integration_mapping(&self, ctx: &Context<'_>, input: CreateIntegrationMappingInput) -> Result<IntegrationMapping> {
        let org = require_org(ctx)?;
        let row = db(ctx)
            .insert_integration_mapping_v2(Uuid::new_v4(), org.id, uuid(&input.integration_id)?, &input.mappable_type, uuid(&input.mappable_id)?, &input.external_id, input.external_account_code.as_deref(), input.external_name.as_deref())
            .await?;
        Ok(row.into())
    }

    /// `UpdateIntegrationMapping`.
    async fn update_integration_mapping(&self, ctx: &Context<'_>, input: UpdateIntegrationMappingInput) -> Result<IntegrationMapping> {
        require_org(ctx)?;
        let row = db(ctx)
            .update_integration_mapping_v2(uuid(&input.id)?, input.external_id.as_deref(), input.external_account_code.as_deref(), input.external_name.as_deref())
            .await?
            .ok_or_else(|| async_graphql::Error::new("integration_mapping_not_found"))?;
        Ok(row.into())
    }

    /// `DestroyIntegrationMapping`.
    async fn destroy_integration_mapping(&self, ctx: &Context<'_>, id: ID) -> Result<ID> {
        require_org(ctx)?;
        let uid = uuid(&id)?;
        db(ctx).soft_delete_integration_mapping(uid).await?;
        Ok(uid.into())
    }

    // ---- integration customers ------------------------------------------

    /// `CreateIntegrationCustomer`.
    async fn create_integration_customer(&self, ctx: &Context<'_>, input: CreateIntegrationCustomerInput) -> Result<IntegrationCustomer> {
        let org = require_org(ctx)?;
        let customer = db(ctx)
            .find_customer(uuid(&input.customer_id)?)
            .await?
            .filter(|c| c.organization_id == org.id)
            .ok_or_else(|| async_graphql::Error::new("customer_not_found"))?;
        let row = db(ctx)
            .insert_integration_customer(Uuid::new_v4(), org.id, uuid(&input.integration_id)?, customer.id, input.external_customer_id.as_deref(), input.code.as_deref())
            .await?;
        Ok(row.into())
    }

    /// `UpdateIntegrationCustomer`.
    async fn update_integration_customer(&self, ctx: &Context<'_>, input: UpdateIntegrationCustomerInput) -> Result<IntegrationCustomer> {
        require_org(ctx)?;
        let row = db(ctx)
            .update_integration_customer(uuid(&input.id)?, input.external_customer_id.as_deref(), input.code.as_deref())
            .await?
            .ok_or_else(|| async_graphql::Error::new("integration_customer_not_found"))?;
        Ok(row.into())
    }

    /// `DestroyIntegrationCustomer`.
    async fn destroy_integration_customer(&self, ctx: &Context<'_>, id: ID) -> Result<ID> {
        require_org(ctx)?;
        let uid = uuid(&id)?;
        db(ctx).soft_delete_integration_customer(uid).await?;
        Ok(uid.into())
    }

    /// `SetIntegrationCustomerAsDefault` (Rago admin convenience).
    async fn set_integration_customer_as_default(&self, ctx: &Context<'_>, id: ID) -> Result<IntegrationCustomer> {
        require_org(ctx)?;
        let row = db(ctx)
            .set_default_integration_customer(uuid(&id)?)
            .await?
            .ok_or_else(|| async_graphql::Error::new("integration_customer_not_found"))?;
        Ok(row.into())
    }

    // ---- integration fetch + sync ---------------------------------------

    /// `FetchIntegrationItems` — fetch the provider catalog (NetSuite items /
    /// accounts / subsidiaries via SuiteQL, Xero items / accounts) with the
    /// integration's own credentials, refresh the local cache, and return the
    /// items.
    async fn fetch_integration_items(&self, ctx: &Context<'_>, integration_id: ID) -> Result<Vec<IntegrationItem>> {
        Self::fetch_catalog(ctx, &integration_id, "item").await
    }

    /// `FetchIntegrationAccounts` — same fetch; returns the ledger accounts.
    async fn fetch_integration_accounts(&self, ctx: &Context<'_>, integration_id: ID) -> Result<Vec<IntegrationItem>> {
        Self::fetch_catalog(ctx, &integration_id, "account").await
    }

    /// `SyncIntegrationInvoice` — enqueue the invoice-sync job (the same durable
    /// queue the accounting sync worker drains) and return the invoice id.
    async fn sync_integration_invoice(&self, ctx: &Context<'_>, invoice_id: ID) -> Result<ID> {
        let org = require_org(ctx)?;
        let inv = uuid(&invoice_id)?;
        db(ctx)
            .find_invoice(inv)
            .await?
            .filter(|i| i.organization_id == org.id)
            .ok_or_else(|| async_graphql::Error::new("invoice_not_found"))?;
        db(ctx)
            .enqueue_job("integrations.sync_invoice", &json!({ "invoice_id": inv }), Utc::now().naive_utc(), 5)
            .await?;
        Ok(inv.into())
    }

    /// `SyncIntegrationCreditNote` — enqueue the credit-note-sync job.
    async fn sync_integration_credit_note(&self, ctx: &Context<'_>, credit_note_id: ID) -> Result<ID> {
        let org = require_org(ctx)?;
        let cn = uuid(&credit_note_id)?;
        db(ctx)
            .find_credit_note(cn)
            .await?
            .filter(|c| c.organization_id == org.id)
            .ok_or_else(|| async_graphql::Error::new("credit_note_not_found"))?;
        db(ctx)
            .enqueue_job("integrations.sync_credit_note", &json!({ "credit_note_id": cn }), Utc::now().naive_utc(), 5)
            .await?;
        Ok(cn.into())
    }
}

impl AdminMutation3 {
    /// Live catalog fetch for one integration, then the cached entries of
    /// `kind`. Provider failures surface as the resolver error (never an empty
    /// list that hides a broken connection).
    async fn fetch_catalog(ctx: &Context<'_>, integration_id: &ID, kind: &str) -> Result<Vec<IntegrationItem>> {
        let org = require_org(ctx)?;
        let id = uuid(integration_id)?;
        let transport = rago_integrations::default_transport();
        rago_jobs::integrations_sync::refresh_integration_catalog(db(ctx), &transport, org.id, id)
            .await
            .map_err(|e| async_graphql::Error::new(e.to_string()))?;
        let rows = db(ctx).integration_items_of_type(id, Some(kind)).await?;
        Ok(rows
            .into_iter()
            .map(|(id, i)| IntegrationItem {
                id: id.into(),
                external_id: i.external_id,
                item_type: Some(i.item_type),
                external_name: i.external_name,
                external_account_code: i.external_account_code,
            })
            .collect())
    }
}
