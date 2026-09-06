//! Automated failed-payment recovery — the dunning sweep runtime.
//!
//! Ports Lago's `Clock::ProcessDunningCampaignsJob` →
//! `DunningCampaigns::BulkProcessService` → `ProcessAttemptService` →
//! `PaymentRequests::CreateService` / `PaymentRequests::Payments::CreateService`
//! pipeline: on each run it visits every org with overdue invoices, evaluates
//! each eligible customer against their applicable campaign, and for each
//! currency whose overdue balance clears a threshold it creates a payment
//! request (linking the overdue invoices), emits `payment_request.created`,
//! charges the invoices through the provider, and — when every dunned currency
//! is exhausted — emits `dunning_campaign.finished`.
//!
//! The pure decision logic (which currencies fire, attempt advancement,
//! finish transition, net amount) lives in [`rago_billing::dunning`]; this
//! module supplies the DB-loaded inputs and performs the side effects. Charging
//! reuses [`crate::payments::charge_invoice`], so the deterministic-payment-id
//! and never-re-charge-a-non-failed-payment invariants are preserved.

use chrono::{DateTime, Utc};
use rago_billing::dunning::{
    build_payment_request, evaluate_customer, DunningPolicy, OverdueInvoice, Threshold,
};
use rago_db::{Db, DunningCampaignPolicy, DunningCandidate};
use rago_integrations::webhooks::WebhookKind;
use rago_integrations::HttpTransport;
use serde_json::json;
use uuid::Uuid;

use crate::payments::charge_invoice;
use crate::webhook_delivery::emit_webhook;
use crate::JobError;

/// Configuration for a dunning sweep.
pub struct DunningConfig {
    /// Provider secret key (owner-gated; only signs the request — the live
    /// network call is behind the transport's `live` feature).
    pub stripe_secret_key: String,
    /// Payment provider code to charge through (matches
    /// `payment_provider_customers.provider`).
    pub provider: String,
    /// Fallback provider customer id when a customer has no linked one. When
    /// `None`, an unlinkable customer is NOT charged (fail-closed: never
    /// fabricate a charge against an unknown provider customer).
    pub provider_customer_fallback: Option<String>,
    /// Fallback webhook endpoint when an org has none configured.
    pub default_webhook_endpoint: Option<String>,
}

impl DunningConfig {
    /// A no-charge config (records requests + webhooks only). Useful in the
    /// worker default where provider credentials are owner-gated.
    pub fn record_only(default_webhook_endpoint: Option<String>) -> Self {
        Self {
            stripe_secret_key: String::new(),
            provider: "stripe".into(),
            provider_customer_fallback: None,
            default_webhook_endpoint,
        }
    }

    /// Build a config from the environment. With no `RAGO_STRIPE_SECRET_KEY`
    /// present this is effectively record-only (no charge) — charging is
    /// owner-gated behind the provider secret + the transport's `live` feature.
    pub fn from_env() -> Self {
        Self {
            stripe_secret_key: std::env::var("RAGO_STRIPE_SECRET_KEY").unwrap_or_default(),
            provider: std::env::var("RAGO_PAYMENT_PROVIDER").unwrap_or_else(|_| "stripe".into()),
            provider_customer_fallback: std::env::var("RAGO_PROVIDER_CUSTOMER_ID").ok(),
            default_webhook_endpoint: std::env::var("RAGO_DEFAULT_WEBHOOK_ENDPOINT").ok(),
        }
    }
}

/// What a dunning sweep did.
#[derive(Debug, Default)]
pub struct DunningReport {
    pub invoices_marked_overdue: u64,
    pub customers_evaluated: u64,
    pub payment_requests_created: u64,
    pub invoices_charged: u64,
    pub campaigns_finished: u64,
}

/// Run one dunning sweep across every org with overdue invoices, at `now`.
pub async fn process_dunning_campaigns<T: HttpTransport>(
    db: &Db,
    transport: &T,
    cfg: &DunningConfig,
    now: DateTime<Utc>,
) -> Result<DunningReport, JobError> {
    let mut report = DunningReport::default();

    for org in db.dunning_organizations().await? {
        let org_default = db.org_default_dunning_campaign(org).await?;
        let endpoint = db
            .webhook_endpoint(org)
            .await?
            .or_else(|| cfg.default_webhook_endpoint.clone());
        let hmac_key = db
            .organization_hmac_key(org)
            .await?
            .unwrap_or_else(|| "rago-default-key".into());

        for candidate in db.dunning_candidates(org).await? {
            // Resolve the applicable campaign: customer override → org default.
            let campaign = match resolve_campaign(db, org, &candidate, &org_default).await? {
                Some(c) => c,
                None => continue,
            };
            let thresholds = db.dunning_campaign_thresholds(org, campaign.id).await?;
            if thresholds.is_empty() {
                continue;
            }

            let (attempts, last_attempt) =
                db.customer_dunning_attempts(candidate.customer_id).await?;
            let overdue = db
                .customer_overdue_balances(org, candidate.customer_id)
                .await?;

            let policy = DunningPolicy {
                max_attempts: campaign.max_attempts,
                days_between_attempts: campaign.days_between_attempts,
            };
            let threshold_domain: Vec<Threshold> = thresholds
                .iter()
                .map(|t| Threshold {
                    currency: t.currency.clone(),
                    amount_cents: t.amount_cents,
                })
                .collect();
            let last_utc = last_attempt.map(|n| DateTime::<Utc>::from_naive_utc_and_offset(n, Utc));

            let decision = evaluate_customer(
                policy,
                &threshold_domain,
                &attempts,
                last_utc,
                &overdue,
                now,
            );
            report.customers_evaluated += 1;

            let Some(attempt_at) = decision.attempt_at else {
                continue; // nothing fired (gated or below threshold)
            };

            // Advance attempts first so a concurrent/immediate re-run is gated
            // by `days_between_attempts` — the idempotency anchor.
            db.update_customer_dunning_attempts(
                candidate.customer_id,
                &decision.new_attempts,
                attempt_at.naive_utc(),
            )
            .await?;

            for fired in &decision.fired {
                if let Some((_request_id, charged)) = create_and_charge_payment_request(
                    db,
                    transport,
                    cfg,
                    org,
                    &candidate,
                    &campaign,
                    &fired.currency,
                    endpoint.as_deref(),
                    &hmac_key,
                )
                .await?
                {
                    report.payment_requests_created += 1;
                    report.invoices_charged += charged;
                }
            }

            if decision.finished {
                if let Some(ep) = endpoint.as_deref() {
                    emit_webhook(
                        db,
                        transport,
                        org,
                        ep,
                        &hmac_key,
                        WebhookKind::DunningCampaignFinished.webhook_type(),
                        WebhookKind::DunningCampaignFinished.object_type(),
                        json!({
                            "lago_customer_id": candidate.customer_id,
                            "external_id": candidate.external_id,
                            "dunning_campaign_code": campaign.code,
                        }),
                    )
                    .await?;
                }
                report.campaigns_finished += 1;
            }
        }
    }

    Ok(report)
}

/// Resolve a customer's applicable campaign: their `applied_dunning_campaign_id`
/// override, else the org default. Ports `applicable_dunning_campaign`.
async fn resolve_campaign(
    db: &Db,
    org: Uuid,
    candidate: &DunningCandidate,
    org_default: &Option<DunningCampaignPolicy>,
) -> Result<Option<DunningCampaignPolicy>, JobError> {
    if let Some(id) = candidate.applied_dunning_campaign_id {
        return Ok(db.dunning_campaign_policy(org, id).await?);
    }
    Ok(org_default.clone())
}

/// Create a payment request for one currency's overdue invoices and charge
/// them. Returns `(payment_request_id, invoices_charged)` or `None` when there
/// is nothing collectible (no overdue invoices in the currency, or the
/// preconditions reject the batch).
#[allow(clippy::too_many_arguments)]
async fn create_and_charge_payment_request<T: HttpTransport>(
    db: &Db,
    transport: &T,
    cfg: &DunningConfig,
    org: Uuid,
    candidate: &DunningCandidate,
    campaign: &DunningCampaignPolicy,
    currency: &str,
    endpoint: Option<&str>,
    hmac_key: &str,
) -> Result<Option<(Uuid, u64)>, JobError> {
    let rows = db
        .overdue_invoices_for_currency(org, candidate.customer_id, currency)
        .await?;
    let invoices: Vec<OverdueInvoice> = rows
        .iter()
        .map(|r| OverdueInvoice {
            id: r.id,
            currency: currency.to_string(),
            total_amount_cents: r.total_amount_cents,
            total_paid_amount_cents: 0,
            credit_note_offset_cents: r.credit_note_offset_cents,
            payment_overdue: true,
        })
        .collect();

    let plan = match build_payment_request(&invoices) {
        Ok(p) => p,
        Err(_) => return Ok(None), // no invoices / preconditions unmet
    };

    let request_id = Uuid::new_v4();
    db.insert_dunning_payment_request(
        request_id,
        org,
        candidate.customer_id,
        &candidate.external_id,
        candidate.email.as_deref(),
        plan.amount_cents,
        &plan.currency,
        campaign.id,
    )
    .await?;
    for invoice_id in &plan.invoice_ids {
        db.insert_payment_request_applied_invoice(Uuid::new_v4(), request_id, *invoice_id, org)
            .await?;
    }

    // payment_request.created webhook (immediate delivery over the transport).
    if let Some(ep) = endpoint {
        emit_webhook(
            db,
            transport,
            org,
            ep,
            hmac_key,
            WebhookKind::PaymentRequestCreated.webhook_type(),
            WebhookKind::PaymentRequestCreated.object_type(),
            json!({
                "lago_id": request_id,
                "external_customer_id": candidate.external_id,
                "email": candidate.email,
                "amount_cents": plan.amount_cents,
                "amount_currency": plan.currency,
                "payment_status": "pending",
                "lago_invoice_ids": plan.invoice_ids,
            }),
        )
        .await?;
    }

    // Charge each overdue invoice through the deterministic-id path. The
    // per-invoice guard (never re-charge a non-failed payment) means a re-run
    // records no duplicate charge.
    let mut charged = 0u64;
    let provider_customer_id = db
        .provider_customer_id(candidate.customer_id, &cfg.provider)
        .await?
        .or_else(|| cfg.provider_customer_fallback.clone());
    if let Some(pcid) = provider_customer_id {
        if !cfg.stripe_secret_key.is_empty() {
            for invoice_id in &plan.invoice_ids {
                if let Some(invoice) = db.find_invoice(*invoice_id).await? {
                    charge_invoice(db, transport, &cfg.stripe_secret_key, &invoice, &pcid).await?;
                    charged += 1;
                }
            }
        }
    }

    Ok(Some((request_id, charged)))
}
