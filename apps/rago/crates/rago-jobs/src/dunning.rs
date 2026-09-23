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
//! reuses [`crate::collection::collect_invoice`], so the deterministic-payment-id
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

use crate::collection::{collect_invoice, CollectOutcome};
use crate::webhook_emit::record_webhook;
use crate::JobError;

/// Configuration for a dunning sweep.
pub struct DunningConfig {
    /// Whether to collect the overdue invoices of a fired payment request.
    /// Provider, credentials and provider customer are resolved per
    /// organization/customer by [`crate::collection`] — there is no shared
    /// fallback customer or key.
    pub charge: bool,
}

impl DunningConfig {
    /// A no-charge config (records payment requests + webhooks only).
    pub fn record_only() -> Self {
        Self { charge: false }
    }

    /// Build a config from the environment: charging is on unless
    /// `RAGO_DUNNING_CHARGE=false`.
    pub fn from_env() -> Self {
        let off = matches!(
            std::env::var("RAGO_DUNNING_CHARGE").as_deref(),
            Ok("false") | Ok("0")
        );
        Self { charge: !off }
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
                match create_and_charge_payment_request(
                    db,
                    transport,
                    cfg,
                    org,
                    &candidate,
                    &campaign,
                    &fired.currency,
                )
                .await
                {
                    Ok(Some((_request_id, charged))) => {
                        report.payment_requests_created += 1;
                        report.invoices_charged += charged;
                    }
                    Ok(None) => {}
                    Err(e) => tracing::error!(
                        organization_id = %org,
                        customer_id = %candidate.customer_id,
                        error = %e,
                        "dunning attempt failed; continuing sweep"
                    ),
                }
            }

            if decision.finished {
                record_webhook(
                    db,
                    org,
                    WebhookKind::DunningCampaignFinished,
                    json!({
                        "lago_customer_id": candidate.customer_id,
                        "external_id": candidate.external_id,
                        "dunning_campaign_code": campaign.code,
                    }),
                )
                .await?;
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
            total_paid_amount_cents: r.total_paid_amount_cents,
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

    // payment_request.created webhook (queued for the delivery worker).
    record_webhook(
        db,
        org,
        WebhookKind::PaymentRequestCreated,
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

    let charged = if cfg.charge {
        charge_payment_request(
            db,
            transport,
            &PaymentRequestTarget {
                organization_id: org,
                request_id,
                customer_id: candidate.customer_id,
                external_customer_id: &candidate.external_id,
                amount_cents: plan.amount_cents,
                currency: &plan.currency,
                invoice_ids: &plan.invoice_ids,
            },
        )
        .await?
        .0
    } else {
        0
    };

    Ok(Some((request_id, charged)))
}

/// A payment request to collect.
pub struct PaymentRequestTarget<'a> {
    pub organization_id: Uuid,
    pub request_id: Uuid,
    pub customer_id: Uuid,
    pub external_customer_id: &'a str,
    pub amount_cents: i64,
    pub currency: &'a str,
    pub invoice_ids: &'a [Uuid],
}

/// Collect every invoice of a payment request through the organization's
/// provider, then record the request's aggregate status and queue
/// `payment_request.payment_status_updated` (+ `payment_request.payment_failure`).
/// Shared by the dunning sweep and `POST /payment_requests`. Returns the number
/// of invoices charged and the aggregate status.
pub async fn charge_payment_request<T: HttpTransport>(
    db: &Db,
    transport: &T,
    target: &PaymentRequestTarget<'_>,
) -> Result<(u64, &'static str), JobError> {
    // Collect each overdue invoice through the org's provider. The per-invoice
    // guard (never re-charge a non-failed payment) means a re-run records no
    // duplicate charge; each retry of a declined payment uses a fresh
    // idempotency key. One invoice's outage never blocks the others.
    let org = target.organization_id;
    let request_id = target.request_id;
    let mut charged = 0u64;
    let mut succeeded = 0usize;
    let mut failures: Vec<serde_json::Value> = Vec::new();
    for invoice_id in target.invoice_ids {
        let Some(invoice) = db.find_invoice(*invoice_id).await? else { continue };
        match collect_invoice(db, transport, &invoice).await {
            Ok(CollectOutcome::Charged(status)) => {
                charged += 1;
                match status {
                    rago_integrations::PayableStatus::Succeeded => succeeded += 1,
                    rago_integrations::PayableStatus::Failed => {
                        failures.push(json!({ "lago_invoice_id": invoice.id, "message": "payment_failed" }))
                    }
                    rago_integrations::PayableStatus::Pending => {}
                }
            }
            Ok(CollectOutcome::Skipped(reason)) => {
                tracing::info!(invoice_id = %invoice.id, reason = reason.code(), "dunning: invoice not charged");
                if matches!(reason, crate::SkipReason::NoPaymentMethod | crate::SkipReason::ProviderCustomerMissing) {
                    failures.push(json!({ "lago_invoice_id": invoice.id, "message": reason.code() }));
                }
            }
            Err(e) => tracing::error!(invoice_id = %invoice.id, error = %e, "dunning: collection error"),
        }
    }

    // Aggregate payment-request outcome → `payment_request.payment_status_updated`
    // (+ `payment_request.payment_failure` when any invoice could not be paid).
    let status: &'static str = if !failures.is_empty() {
        "failed"
    } else if succeeded == target.invoice_ids.len() {
        "succeeded"
    } else {
        "pending"
    };
    if db.set_payment_request_status(request_id, status).await? {
        record_webhook(
            db,
            org,
            WebhookKind::PaymentRequestPaymentStatusUpdated,
            json!({
                "lago_id": request_id,
                "external_customer_id": target.external_customer_id,
                "amount_cents": target.amount_cents,
                "amount_currency": target.currency,
                "payment_status": status,
                "lago_invoice_ids": target.invoice_ids,
            }),
        )
        .await?;
    }
    if !failures.is_empty() {
        record_webhook(
            db,
            org,
            WebhookKind::PaymentRequestPaymentFailure,
            json!({
                "lago_payable_id": request_id,
                "lago_customer_id": target.customer_id,
                "external_customer_id": target.external_customer_id,
                "provider_error": { "failures": failures },
            }),
        )
        .await?;
    }
    Ok((charged, status))
}
