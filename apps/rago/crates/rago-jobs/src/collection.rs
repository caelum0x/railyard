//! Production payment collection — `Invoices::Payments::CreateService` with the
//! per-organization provider resolution Lago does:
//!
//! 1. resolve the organization's payment provider (dashboard-configured
//!    `payment_providers` row with its sealed secret, else the env fallback);
//! 2. resolve the customer's account at that provider — for Stripe, create the
//!    Stripe customer on first charge (`Stripe::Customers::CreateService`) and
//!    look up the saved payment method;
//! 3. charge through [`crate::payments::charge_invoice_with`] (advisory lock,
//!    never re-charge a non-failed payment, attempt-scoped idempotency keys,
//!    declines recorded as failed payments);
//! 4. queue the outbound `invoice.payment_status_updated` (+
//!    `payment.succeeded` / `invoice.payment_failure`) webhooks.
//!
//! No shared fallback customer, no fabricated charges: a customer that cannot
//! be charged (provider not configured, no payment method) is reported as
//! such, and in the no-payment-method case the invoice enters dunning.

use rago_core::enums::{InvoiceStatus, PaymentStatus};
use rago_core::models::{Customer, Invoice};
use rago_db::{Db, ProviderCredentials};
use rago_integrations::payments::PaymentProvider;
use rago_integrations::webhooks::WebhookKind;
use rago_integrations::{
    AdyenProvider, CreemProvider, DodoProvider, GocardlessProvider, HttpTransport, IyzicoBuyer,
    IyzicoProvider, PayableStatus, StripeProvider,
};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::payments::{charge_invoice_with, record_uncollectable_attempt};
use crate::webhook_emit::record_webhook;
use crate::JobError;

/// A resolved payment provider for an organization.
#[derive(Debug, Clone)]
pub struct ResolvedProvider {
    /// `stripe` | `gocardless` | `adyen` | `creem` | `dodo` | `iyzico`.
    pub kind: String,
    pub secret: String,
    pub settings: Value,
    pub success_redirect_url: Option<String>,
}

/// Why an invoice was not charged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SkipReason {
    NothingToCollect,
    AlreadyPaid,
    NotFinalized,
    NoCustomer,
    ProviderNotConfigured,
    UnsupportedProvider(String),
    NoPaymentMethod,
    ProviderCustomerMissing,
}

impl SkipReason {
    pub fn code(&self) -> &str {
        match self {
            SkipReason::NothingToCollect => "nothing_to_collect",
            SkipReason::AlreadyPaid => "already_paid",
            SkipReason::NotFinalized => "invoice_not_finalized",
            SkipReason::NoCustomer => "customer_not_found",
            SkipReason::ProviderNotConfigured => "payment_provider_not_configured",
            SkipReason::UnsupportedProvider(_) => "payment_provider_not_supported",
            SkipReason::NoPaymentMethod => "no_payment_method",
            SkipReason::ProviderCustomerMissing => "provider_customer_missing",
        }
    }
}

/// The result of a collection attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CollectOutcome {
    Charged(PayableStatus),
    Skipped(SkipReason),
}

fn env_nonempty(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.trim().is_empty())
}

/// Env fallback secret for a provider kind (single-tenant deployments).
fn env_secret(kind: &str) -> Option<String> {
    match kind {
        "stripe" => env_nonempty("RAGO_STRIPE_SECRET_KEY").or_else(|| env_nonempty("RAGO_STRIPE_SECRET")),
        "gocardless" => env_nonempty("RAGO_GOCARDLESS_ACCESS_TOKEN"),
        "adyen" => env_nonempty("RAGO_ADYEN_API_KEY"),
        "creem" => env_nonempty("RAGO_CREEM_API_KEY"),
        "dodo" => env_nonempty("RAGO_DODO_API_KEY"),
        "iyzico" => env_nonempty("RAGO_IYZICO_SECRET_KEY"),
        _ => None,
    }
}

/// Env fallback settings (single-tenant deployments) for a provider kind.
fn env_settings(kind: &str) -> Value {
    let product_id = match kind {
        "creem" => env_nonempty("RAGO_CREEM_PRODUCT_ID"),
        "dodo" => env_nonempty("RAGO_DODO_PRODUCT_ID"),
        _ => None,
    };
    json!({
        "merchant_account": env_nonempty("RAGO_ADYEN_MERCHANT_ACCOUNT"),
        "product_id": product_id,
        "currency": env_nonempty("RAGO_CREEM_PRODUCT_CURRENCY"),
        "mode": env_nonempty("RAGO_PAYMENT_PROVIDER_MODE"),
        "api_key": env_nonempty("RAGO_IYZICO_API_KEY"),
    })
}

fn setting(settings: &Value, key: &str) -> Option<String> {
    settings
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_string)
}

/// `true` when the provider settings select test/sandbox mode.
fn test_mode(settings: &Value) -> Option<bool> {
    setting(settings, "mode").map(|m| matches!(m.as_str(), "test" | "sandbox"))
}

/// Rago's public Iyzico callback for an organization
/// (`RAGO_PUBLIC_URL/webhooks/iyzico/:organization_id`).
pub fn iyzico_callback_url(organization_id: Uuid) -> Option<String> {
    env_nonempty(crate::webhook_emit::PUBLIC_URL_ENV)
        .map(|base| format!("{}/webhooks/iyzico/{organization_id}", base.trim_end_matches('/')))
}

/// Build the Iyzico provider for an organization's resolved credentials.
pub fn iyzico_provider<T: HttpTransport>(
    provider: &ResolvedProvider,
    organization_id: Uuid,
    transport: T,
) -> Option<IyzicoProvider<T>> {
    let api_key = setting(&provider.settings, "api_key")?;
    let mut iyzico = IyzicoProvider::new(api_key, provider.secret.clone(), transport);
    if let Some(sandbox) = test_mode(&provider.settings) {
        iyzico = iyzico.with_sandbox(sandbox);
    }
    if let Some(url) = iyzico_callback_url(organization_id) {
        iyzico = iyzico.with_callback_url(url);
    }
    Some(iyzico)
}

/// The Iyzico buyer, read entirely from the customer record (name, email,
/// country and the stored billing address / identity number). Missing fields
/// stay empty so the provider refuses with the list of what to complete.
async fn iyzico_buyer(db: &Db, customer: &Customer) -> Result<IyzicoBuyer, JobError> {
    let address = db.customer_address(customer.id).await?;
    let street = [address.address_line1.as_deref(), address.address_line2.as_deref()]
        .into_iter()
        .flatten()
        .filter(|l| !l.trim().is_empty())
        .collect::<Vec<_>>()
        .join(", ");
    let city = address.city.clone().unwrap_or_default();
    Ok(IyzicoBuyer {
        id: customer.external_id.clone(),
        name: customer.name.clone().unwrap_or_default(),
        email: customer.email.clone(),
        country: customer.country.clone().unwrap_or_default(),
        city,
        address: if address.address_line1.as_deref().is_some_and(|l| !l.trim().is_empty()) { street } else { String::new() },
        zip_code: address.zipcode,
        phone: address.phone,
        identity_number: address.identity_number.or_else(|| customer.tax_identification_number.clone()),
        ip: None,
    })
}

/// Resolve the organization's payment provider: a dashboard-configured
/// provider with a stored secret wins; otherwise the env fallback
/// (`RAGO_PAYMENT_PROVIDER`, default `stripe`, plus its secret variable).
pub async fn resolve_provider(db: &Db, org: Uuid) -> Result<Option<ResolvedProvider>, JobError> {
    let configured: Option<ProviderCredentials> = db.provider_credentials(org, None).await?;
    if let Some(c) = configured {
        let secret = c.secret.clone().or_else(|| env_secret(&c.provider_type));
        if let Some(secret) = secret {
            return Ok(Some(ResolvedProvider {
                kind: c.provider_type,
                secret,
                settings: c.settings,
                success_redirect_url: c.success_redirect_url,
            }));
        }
    }
    let kind = env_nonempty("RAGO_PAYMENT_PROVIDER").unwrap_or_else(|| "stripe".into());
    Ok(env_secret(&kind).map(|secret| ResolvedProvider {
        settings: env_settings(&kind),
        kind,
        secret,
        success_redirect_url: env_nonempty("RAGO_CHECKOUT_SUCCESS_URL"),
    }))
}

/// Resolve the organization's credentials for one specific provider kind (the
/// provider that captured a payment, for refunds): the dashboard-configured
/// row of that type with its sealed secret, else the env fallback.
pub async fn resolve_provider_of_kind(db: &Db, org: Uuid, kind: &str) -> Result<Option<ResolvedProvider>, JobError> {
    let configured = db.provider_credentials(org, Some(kind)).await?;
    let (settings, redirect, secret) = match configured {
        Some(c) => (c.settings, c.success_redirect_url, c.secret.or_else(|| env_secret(kind))),
        None => (env_settings(kind), env_nonempty("RAGO_CHECKOUT_SUCCESS_URL"), env_secret(kind)),
    };
    Ok(secret.map(|secret| ResolvedProvider {
        kind: kind.to_string(),
        secret,
        settings,
        success_redirect_url: redirect,
    }))
}

/// Resolve (creating on first use) the customer's Stripe customer id, and its
/// payment method (stored, else looked up at Stripe and stored).
pub async fn ensure_stripe_customer<T: HttpTransport>(
    db: &Db,
    transport: &T,
    secret: &str,
    customer: &Customer,
) -> Result<(String, Option<String>), JobError> {
    let stripe = StripeProvider::new(secret, transport);
    let (cus_id, stored_pm) = ensure_stripe_customer_no_pm(db, transport, secret, customer).await?;
    if let Some(pm) = stored_pm.filter(|p| !p.is_empty()) {
        return Ok((cus_id, Some(pm)));
    }
    let pm = stripe
        .default_payment_method(&cus_id)
        .await
        .map_err(|e| JobError::Payment(format!("stripe payment method lookup failed: {e}")))?;
    if let Some(ref p) = pm {
        db.set_provider_customer_payment_method(customer.id, "stripe", p).await?;
    }
    Ok((cus_id, pm))
}

/// Charge one invoice through the organization's provider and queue the
/// resulting webhooks. Declines are an `Ok(Charged(Failed))` outcome; only an
/// outage (outcome unknown) is an `Err`.
pub async fn collect_invoice<T: HttpTransport>(
    db: &Db,
    transport: &T,
    invoice: &Invoice,
) -> Result<CollectOutcome, JobError> {
    if invoice.total_amount_cents <= 0 {
        return Ok(CollectOutcome::Skipped(SkipReason::NothingToCollect));
    }
    if matches!(invoice.payment_status, PaymentStatus::Succeeded | PaymentStatus::Refunded) {
        return Ok(CollectOutcome::Skipped(SkipReason::AlreadyPaid));
    }
    if !matches!(invoice.status, InvoiceStatus::Finalized) {
        return Ok(CollectOutcome::Skipped(SkipReason::NotFinalized));
    }
    let Some(customer_id) = invoice.customer_id else {
        return Ok(CollectOutcome::Skipped(SkipReason::NoCustomer));
    };
    let Some(customer) = db.find_customer(customer_id).await? else {
        return Ok(CollectOutcome::Skipped(SkipReason::NoCustomer));
    };
    let Some(provider) = resolve_provider(db, invoice.organization_id).await? else {
        tracing::warn!(
            organization_id = %invoice.organization_id,
            invoice_id = %invoice.id,
            "payment provider not configured; invoice left for manual collection"
        );
        return Ok(CollectOutcome::Skipped(SkipReason::ProviderNotConfigured));
    };

    let status = match provider.kind.as_str() {
        "stripe" => {
            let (cus_id, pm) = ensure_stripe_customer(db, transport, &provider.secret, &customer).await?;
            let Some(pm) = pm else {
                record_uncollectable_attempt(db, invoice, "stripe", "no_payment_method").await?;
                emit_payment_outcome(db, invoice, &customer, PayableStatus::Failed, Some("no_payment_method")).await;
                return Ok(CollectOutcome::Skipped(SkipReason::NoPaymentMethod));
            };
            let secret = provider.secret.clone();
            charge_invoice_with(db, invoice, &cus_id, None, |attempt| {
                Box::new(
                    StripeProvider::new(secret.clone(), transport)
                        .with_payment_method(Some(pm.clone()))
                        .with_attempt(attempt),
                ) as Box<dyn PaymentProvider + '_>
            })
            .await?
        }
        "gocardless" => {
            // GoCardless charges a direct-debit mandate; the mandate id is
            // stored as the provider customer's payment method.
            let Some((Some(pcid), mandate)) = db.provider_customer(customer.id, "gocardless").await? else {
                return Ok(CollectOutcome::Skipped(SkipReason::ProviderCustomerMissing));
            };
            let Some(mandate) = mandate.filter(|m| !m.is_empty()) else {
                record_uncollectable_attempt(db, invoice, "gocardless", "no_mandate").await?;
                emit_payment_outcome(db, invoice, &customer, PayableStatus::Failed, Some("no_mandate")).await;
                return Ok(CollectOutcome::Skipped(SkipReason::NoPaymentMethod));
            };
            let secret = provider.secret.clone();
            charge_invoice_with(db, invoice, &pcid, Some(&mandate), |attempt| {
                Box::new(GocardlessProvider::new(secret.clone(), transport).with_attempt(attempt))
                    as Box<dyn PaymentProvider + '_>
            })
            .await?
        }
        "adyen" => {
            let merchant = provider
                .settings
                .get("merchant_account")
                .and_then(Value::as_str)
                .map(str::to_string)
                .or_else(|| env_nonempty("RAGO_ADYEN_MERCHANT_ACCOUNT"));
            let Some(merchant) = merchant else {
                return Ok(CollectOutcome::Skipped(SkipReason::ProviderNotConfigured));
            };
            // Adyen charges a stored shopper reference (the provider customer id).
            let Some((Some(shopper), _)) = db.provider_customer(customer.id, "adyen").await? else {
                return Ok(CollectOutcome::Skipped(SkipReason::ProviderCustomerMissing));
            };
            let secret = provider.secret.clone();
            charge_invoice_with(db, invoice, &shopper, None, |attempt| {
                Box::new(AdyenProvider::new(secret.clone(), merchant.clone(), transport).with_attempt(attempt))
                    as Box<dyn PaymentProvider + '_>
            })
            .await?
        }
        "creem" => {
            // Hosted checkout for the org's one-time credit product at the
            // invoice amount; the customer pays through the link.
            let Some(product_id) = setting(&provider.settings, "product_id") else {
                return Ok(CollectOutcome::Skipped(SkipReason::ProviderNotConfigured));
            };
            let currency = setting(&provider.settings, "currency").unwrap_or_else(|| "USD".into());
            let secret = provider.secret.clone();
            let success_url = provider.success_redirect_url.clone();
            let mode = test_mode(&provider.settings);
            charge_invoice_with(db, invoice, &customer.external_id, None, |attempt| {
                let mut creem = CreemProvider::new(secret.clone(), product_id.clone(), currency.clone(), transport)
                    .with_success_url(success_url.clone())
                    .with_attempt(attempt);
                if let Some(test) = mode {
                    creem = creem.with_test_mode(test);
                }
                Box::new(creem) as Box<dyn PaymentProvider + '_>
            })
            .await?
        }
        "dodo" => {
            let Some(product_id) = setting(&provider.settings, "product_id") else {
                return Ok(CollectOutcome::Skipped(SkipReason::ProviderNotConfigured));
            };
            let provider_customer = db
                .provider_customer(customer.id, "dodo")
                .await?
                .and_then(|(pcid, _)| pcid)
                .unwrap_or_else(|| customer.external_id.clone());
            let secret = provider.secret.clone();
            let return_url = provider.success_redirect_url.clone();
            let test = test_mode(&provider.settings).unwrap_or(false);
            charge_invoice_with(db, invoice, &provider_customer, None, |_attempt| {
                Box::new(
                    DodoProvider::new(secret.clone(), product_id.clone(), test, transport)
                        .with_return_url(return_url.clone()),
                ) as Box<dyn PaymentProvider + '_>
            })
            .await?
        }
        "iyzico" => {
            if iyzico_provider(&provider, invoice.organization_id, transport).is_none() {
                return Ok(CollectOutcome::Skipped(SkipReason::ProviderNotConfigured));
            }
            let buyer = iyzico_buyer(db, &customer).await?;
            let org = invoice.organization_id;
            charge_invoice_with(db, invoice, &customer.external_id, None, |_attempt| {
                let iyzico = iyzico_provider(&provider, org, transport)
                    .expect("checked above")
                    .with_buyer(buyer.clone());
                Box::new(iyzico) as Box<dyn PaymentProvider + '_>
            })
            .await?
        }
        other => {
            return Ok(CollectOutcome::Skipped(SkipReason::UnsupportedProvider(other.to_string())));
        }
    };

    emit_payment_outcome(db, invoice, &customer, status, None).await;
    if status == PayableStatus::Pending {
        notify_requires_action(db, transport, invoice, &customer).await?;
    }
    Ok(CollectOutcome::Charged(status))
}

/// When the charge is waiting on the customer (Stripe SCA / 3-D Secure), make
/// sure the payment carries a confirm link — Stripe's hosted redirect when it
/// gave one, else a one-time Checkout link for the invoice — and queue
/// `payment.requires_action` so the integrator can notify the customer.
/// Returns the confirm URL, if one could be produced.
pub async fn notify_requires_action<T: HttpTransport>(
    db: &Db,
    transport: &T,
    invoice: &Invoice,
    customer: &Customer,
) -> Result<Option<String>, JobError> {
    let Some(action) = db.invoice_payment_action(invoice.id).await? else {
        return Ok(None);
    };
    let url = match action.next_action_url.clone() {
        Some(u) => Some(u),
        None => match invoice_payment_url(db, transport, invoice).await? {
            Ok(u) => {
                db.set_payment_next_action_url(action.payment_id, &u).await?;
                Some(u)
            }
            Err(reason) => {
                tracing::warn!(
                    invoice_id = %invoice.id,
                    reason,
                    "payment requires customer action but no confirm link could be generated"
                );
                None
            }
        },
    };
    let _ = record_webhook(
        db,
        invoice.organization_id,
        WebhookKind::PaymentRequiresAction,
        json!({
            "lago_id": action.payment_id,
            "lago_payable_id": invoice.id,
            "payable_type": "Invoice",
            "invoice_number": invoice.number,
            "lago_customer_id": customer.id,
            "external_customer_id": customer.external_id,
            "provider": action.provider,
            "provider_payment_id": action.provider_payment_id,
            "status": "requires_action",
            "amount_cents": invoice.total_amount_cents,
            "amount_currency": invoice.currency().code(),
            "next_action": action.next_action,
            "payment_url": url,
        }),
    )
    .await;
    Ok(url)
}

/// Queue `invoice.payment_status_updated` plus the success/failure webhook.
async fn emit_payment_outcome(
    db: &Db,
    invoice: &Invoice,
    customer: &Customer,
    status: PayableStatus,
    reason: Option<&str>,
) {
    let payment_status = match status {
        PayableStatus::Succeeded => "succeeded",
        PayableStatus::Failed => "failed",
        PayableStatus::Pending => "pending",
    };
    let org = invoice.organization_id;
    let _ = record_webhook(
        db,
        org,
        WebhookKind::InvoicePaymentStatusUpdated,
        json!({
            "lago_id": invoice.id,
            "number": invoice.number,
            "status": invoice.status.as_str(),
            "payment_status": payment_status,
            "currency": invoice.currency,
            "total_amount_cents": invoice.total_amount_cents,
            "external_customer_id": customer.external_id,
        }),
    )
    .await;
    match status {
        PayableStatus::Succeeded => {
            let _ = record_webhook(
                db,
                org,
                WebhookKind::PaymentSucceeded,
                json!({
                    "lago_id": crate::payments::deterministic_payment_id(invoice.id),
                    "lago_invoice_id": invoice.id,
                    "status": "succeeded",
                    "amount_cents": invoice.total_amount_cents,
                    "amount_currency": invoice.currency().code(),
                }),
            )
            .await;
        }
        PayableStatus::Failed => {
            let _ = record_webhook(
                db,
                org,
                WebhookKind::InvoicePaymentFailure,
                json!({
                    "lago_invoice_id": invoice.id,
                    "lago_customer_id": customer.id,
                    "external_customer_id": customer.external_id,
                    "provider_error": { "message": reason.unwrap_or("payment_failed") },
                }),
            )
            .await;
        }
        PayableStatus::Pending => {}
    }
}

/// Stripe Checkout (setup mode) URL where the customer saves a card — the
/// `customer.checkout_url_generated` flow.
pub async fn checkout_url<T: HttpTransport>(
    db: &Db,
    transport: &T,
    customer: &Customer,
) -> Result<Result<String, &'static str>, JobError> {
    let Some(provider) = resolve_provider(db, customer.organization_id).await? else {
        return Ok(Err("payment_provider_not_configured"));
    };
    if provider.kind != "stripe" {
        return Ok(Err("checkout_not_supported_for_provider"));
    }
    let Some(success_url) = provider
        .success_redirect_url
        .clone()
        .filter(|u| !u.is_empty())
        .or_else(|| env_nonempty("RAGO_CHECKOUT_SUCCESS_URL"))
    else {
        return Ok(Err("success_redirect_url_missing"));
    };
    let (cus_id, _) = ensure_stripe_customer_no_pm(db, transport, &provider.secret, customer).await?;
    let url = StripeProvider::new(provider.secret.clone(), transport)
        .create_checkout_session(&cus_id, &success_url)
        .await
        .map_err(|e| JobError::Payment(e.to_string()))?;
    let _ = record_webhook(
        db,
        customer.organization_id,
        WebhookKind::CustomerCheckoutUrlGenerated,
        json!({
            "lago_customer_id": customer.id,
            "external_customer_id": customer.external_id,
            "payment_provider": "stripe",
            "checkout_url": url,
        }),
    )
    .await;
    Ok(Ok(url))
}

/// Stripe Billing Portal URL for the customer.
pub async fn portal_url<T: HttpTransport>(
    db: &Db,
    transport: &T,
    customer: &Customer,
) -> Result<Result<String, &'static str>, JobError> {
    let Some(provider) = resolve_provider(db, customer.organization_id).await? else {
        return Ok(Err("payment_provider_not_configured"));
    };
    if provider.kind != "stripe" {
        return Ok(Err("portal_not_supported_for_provider"));
    }
    let Some(return_url) = provider
        .success_redirect_url
        .clone()
        .filter(|u| !u.is_empty())
        .or_else(|| env_nonempty("RAGO_CHECKOUT_SUCCESS_URL"))
    else {
        return Ok(Err("success_redirect_url_missing"));
    };
    let (cus_id, _) = ensure_stripe_customer_no_pm(db, transport, &provider.secret, customer).await?;
    let url = StripeProvider::new(provider.secret.clone(), transport)
        .create_portal_session(&cus_id, &return_url)
        .await
        .map_err(|e| JobError::Payment(e.to_string()))?;
    Ok(Ok(url))
}

/// Like [`ensure_stripe_customer`] but without the payment-method lookup
/// (checkout exists precisely to add one).
async fn ensure_stripe_customer_no_pm<T: HttpTransport>(
    db: &Db,
    transport: &T,
    secret: &str,
    customer: &Customer,
) -> Result<(String, Option<String>), JobError> {
    if let Some((Some(id), pm)) = db.provider_customer(customer.id, "stripe").await? {
        if !id.is_empty() {
            return Ok((id, pm));
        }
    }
    let id = match StripeProvider::new(secret, transport)
        .create_customer(&customer.id, customer.name.as_deref(), customer.email.as_deref())
        .await
    {
        Ok(id) => id,
        Err(e) => {
            // `customer.payment_provider_error` — the operator must fix the
            // customer (or provider credentials) before it can be charged.
            let _ = record_webhook(
                db,
                customer.organization_id,
                WebhookKind::CustomerPaymentProviderError,
                json!({
                    "lago_customer_id": customer.id,
                    "external_customer_id": customer.external_id,
                    "payment_provider": "stripe",
                    "provider_error": { "message": e.to_string(), "error_code": "customer_creation_failed" },
                }),
            )
            .await;
            return Err(JobError::Payment(format!("stripe customer creation failed: {e}")));
        }
    };
    db.upsert_provider_customer(customer.organization_id, customer.id, "stripe", &id)
        .await?;
    let _ = record_webhook(
        db,
        customer.organization_id,
        WebhookKind::CustomerPaymentProviderCreated,
        json!({
            "lago_id": customer.id,
            "external_id": customer.external_id,
            "billing_configuration": {
                "payment_provider": "stripe",
                "provider_customer_id": id,
            },
        }),
    )
    .await;
    Ok((id, None))
}

/// A one-time Stripe Checkout URL for paying `invoice` (the operator /
/// customer "pay now" link for a failed or pending invoice).
pub async fn invoice_payment_url<T: HttpTransport>(
    db: &Db,
    transport: &T,
    invoice: &Invoice,
) -> Result<Result<String, &'static str>, JobError> {
    if matches!(invoice.payment_status, PaymentStatus::Succeeded) {
        return Ok(Err("invoice_already_paid"));
    }
    if !matches!(invoice.status, InvoiceStatus::Finalized) {
        return Ok(Err("invoice_not_finalized"));
    }
    let Some(customer_id) = invoice.customer_id else {
        return Ok(Err("customer_not_found"));
    };
    let Some(customer) = db.find_customer(customer_id).await? else {
        return Ok(Err("customer_not_found"));
    };
    let Some(provider) = resolve_provider(db, invoice.organization_id).await? else {
        return Ok(Err("payment_provider_not_configured"));
    };
    if provider.kind != "stripe" {
        return Ok(Err("payment_url_not_supported_for_provider"));
    }
    let Some(success_url) = provider
        .success_redirect_url
        .clone()
        .filter(|u| !u.is_empty())
        .or_else(|| env_nonempty("RAGO_CHECKOUT_SUCCESS_URL"))
    else {
        return Ok(Err("success_redirect_url_missing"));
    };
    let (cus_id, _) = ensure_stripe_customer_no_pm(db, transport, &provider.secret, &customer).await?;
    let url = StripeProvider::new(provider.secret.clone(), transport)
        .create_payment_checkout(
            &cus_id,
            invoice.total_amount_cents,
            &invoice.currency().code(),
            &invoice.id,
            &invoice.number,
            &success_url,
        )
        .await
        .map_err(|e| JobError::Payment(e.to_string()))?;
    Ok(Ok(url))
}
