//! GraphQL Query root — faithful port of `Types::QueryType`'s core fields.
//! Every resolver is gated behind the authenticated organization and delegates
//! to the shared `rago-db` repositories (same reads as the REST API).

use async_graphql::{Context, Object, Result, ID};
use uuid::Uuid;

use crate::context::{db, require_org};
use crate::types::*;

const PER_PAGE: i64 = 100;

fn page(page: Option<i64>) -> i64 {
    page.filter(|p| *p > 0).unwrap_or(1)
}
fn limit(per_page: Option<i64>) -> i64 {
    per_page.filter(|p| *p > 0).unwrap_or(PER_PAGE)
}

fn first_of_next_month(date: chrono::NaiveDate) -> chrono::NaiveDate {
    use chrono::Datelike;
    if date.month() == 12 {
        chrono::NaiveDate::from_ymd_opt(date.year() + 1, 1, 1).unwrap()
    } else {
        chrono::NaiveDate::from_ymd_opt(date.year(), date.month() + 1, 1).unwrap()
    }
}

#[derive(Default)]
pub struct Query;

#[Object(name = "CoreQuery")]
impl Query {
    /// `currentOrganization` — the authenticated principal's organization.
    async fn current_organization(&self, ctx: &Context<'_>) -> Result<Organization> {
        Ok(Organization(require_org(ctx)?.clone()))
    }

    /// `customers` — paginated customers.
    async fn customers(
        &self,
        ctx: &Context<'_>,
        page: Option<i64>,
        per_page: Option<i64>,
    ) -> Result<CustomerCollection> {
        let org = require_org(ctx)?;
        let (pg, lim) = (self::page(page), self::limit(per_page));
        let items = db(ctx).list_customers(org.id, lim, (pg - 1) * lim).await?;
        let total = db(ctx).count_customers(org.id).await?;
        Ok(CustomerCollection {
            collection: items.into_iter().map(Customer).collect(),
            metadata: CollectionMetadata::build(pg, lim, total),
        })
    }

    /// `customer(externalId:)`.
    async fn customer(&self, ctx: &Context<'_>, external_id: String) -> Result<Option<Customer>> {
        let org = require_org(ctx)?;
        Ok(db(ctx)
            .find_customer_by_external_id(org.id, &external_id)
            .await?
            .map(Customer))
    }

    /// `plans` — paginated plans (with their charges).
    async fn plans(
        &self,
        ctx: &Context<'_>,
        page: Option<i64>,
        per_page: Option<i64>,
    ) -> Result<PlanCollection> {
        let org = require_org(ctx)?;
        let (pg, lim) = (self::page(page), self::limit(per_page));
        let plans = db(ctx).list_plans(org.id, lim, (pg - 1) * lim).await?;
        let total = db(ctx).count_plans(org.id).await?;
        let mut collection = Vec::with_capacity(plans.len());
        for plan in plans {
            let charges = db(ctx).charges_for_plan(plan.id).await?;
            collection.push(Plan { plan, charges });
        }
        Ok(PlanCollection {
            collection,
            metadata: CollectionMetadata::build(pg, lim, total),
        })
    }

    /// `plan(code:)`.
    async fn plan(&self, ctx: &Context<'_>, code: String) -> Result<Option<Plan>> {
        let org = require_org(ctx)?;
        let Some(plan) = db(ctx).find_plan_by_code(org.id, &code).await? else {
            return Ok(None);
        };
        let charges = db(ctx).charges_for_plan(plan.id).await?;
        Ok(Some(Plan { plan, charges }))
    }

    /// `billableMetrics`.
    async fn billable_metrics(
        &self,
        ctx: &Context<'_>,
        page: Option<i64>,
        per_page: Option<i64>,
    ) -> Result<BillableMetricCollection> {
        let org = require_org(ctx)?;
        let (pg, lim) = (self::page(page), self::limit(per_page));
        let items = db(ctx).list_billable_metrics(org.id, lim, (pg - 1) * lim).await?;
        let total = db(ctx).count_billable_metrics(org.id).await?;
        Ok(BillableMetricCollection {
            collection: items.into_iter().map(BillableMetric).collect(),
            metadata: CollectionMetadata::build(pg, lim, total),
        })
    }

    /// `coupons`.
    async fn coupons(
        &self,
        ctx: &Context<'_>,
        page: Option<i64>,
        per_page: Option<i64>,
    ) -> Result<CouponCollection> {
        let org = require_org(ctx)?;
        let (pg, lim) = (self::page(page), self::limit(per_page));
        let items = db(ctx).list_coupons(org.id, lim, (pg - 1) * lim).await?;
        let total = db(ctx).count_coupons(org.id).await?;
        Ok(CouponCollection {
            collection: items.into_iter().map(Coupon).collect(),
            metadata: CollectionMetadata::build(pg, lim, total),
        })
    }

    /// `subscriptions(externalCustomerId:)` — a customer's subscriptions.
    async fn subscriptions(
        &self,
        ctx: &Context<'_>,
        external_customer_id: String,
        page: Option<i64>,
        per_page: Option<i64>,
    ) -> Result<SubscriptionCollection> {
        let org = require_org(ctx)?;
        let (pg, lim) = (self::page(page), self::limit(per_page));
        let customer = db(ctx)
            .find_customer_by_external_id(org.id, &external_customer_id)
            .await?
            .ok_or_else(|| async_graphql::Error::new("customer_not_found"))?;
        let subs = db(ctx)
            .subscriptions_for_customer(customer.id, lim, (pg - 1) * lim)
            .await?;
        let total = db(ctx).count_subscriptions_for_customer(customer.id).await?;

        let mut collection = Vec::with_capacity(subs.len());
        for sub in subs {
            let plan = db(ctx).find_plan(sub.plan_id).await?;
            collection.push(Subscription {
                plan_code: plan.code.clone(),
                plan_amount_cents: plan.amount_cents,
                plan_amount_currency: plan.amount_currency.clone(),
                external_customer_id: external_customer_id.clone(),
                sub,
            });
        }
        Ok(SubscriptionCollection {
            collection,
            metadata: CollectionMetadata::build(pg, lim, total),
        })
    }

    /// `invoice(id:)`.
    async fn invoice(&self, ctx: &Context<'_>, id: ID) -> Result<Option<Invoice>> {
        let _org = require_org(ctx)?;
        let uuid = Uuid::parse_str(id.as_str()).map_err(|_| async_graphql::Error::new("invalid_id"))?;
        let Some(invoice) = db(ctx).find_invoice(uuid).await? else {
            return Ok(None);
        };
        let fees = db(ctx).fees_for_invoice(invoice.id).await?;
        Ok(Some(Invoice { invoice, fees }))
    }

    /// `customerInvoices(externalCustomerId:)`.
    async fn customer_invoices(
        &self,
        ctx: &Context<'_>,
        external_customer_id: String,
        page: Option<i64>,
        per_page: Option<i64>,
    ) -> Result<InvoiceCollection> {
        let org = require_org(ctx)?;
        let (pg, lim) = (self::page(page), self::limit(per_page));
        let customer = db(ctx)
            .find_customer_by_external_id(org.id, &external_customer_id)
            .await?
            .ok_or_else(|| async_graphql::Error::new("customer_not_found"))?;
        let invoices = db(ctx)
            .invoices_for_customer(customer.id, lim, (pg - 1) * lim)
            .await?;
        let total = db(ctx).count_invoices_for_customer(customer.id).await?;
        let mut collection = Vec::with_capacity(invoices.len());
        for invoice in invoices {
            let fees = db(ctx).fees_for_invoice(invoice.id).await?;
            collection.push(Invoice { invoice, fees });
        }
        Ok(InvoiceCollection {
            collection,
            metadata: CollectionMetadata::build(pg, lim, total),
        })
    }

    /// `wallets(externalCustomerId:)`.
    async fn wallets(
        &self,
        ctx: &Context<'_>,
        external_customer_id: String,
    ) -> Result<WalletCollection> {
        let org = require_org(ctx)?;
        let customer = db(ctx)
            .find_customer_by_external_id(org.id, &external_customer_id)
            .await?
            .ok_or_else(|| async_graphql::Error::new("customer_not_found"))?;
        let wallets = db(ctx).wallets_for_customer(customer.id).await?;
        let total = wallets.len() as i64;
        Ok(WalletCollection {
            collection: wallets
                .into_iter()
                .map(|w| Wallet {
                    wallet: w,
                    external_customer_id: external_customer_id.clone(),
                })
                .collect(),
            metadata: CollectionMetadata::build(1, PER_PAGE, total),
        })
    }

    // ---- customer portal (end-customer read surface) ---------------------

    /// `customerPortalInvoices(externalCustomerId:)` — the customer's invoices.
    async fn customer_portal_invoices(
        &self,
        ctx: &Context<'_>,
        external_customer_id: String,
        page: Option<i64>,
        per_page: Option<i64>,
    ) -> Result<InvoiceCollection> {
        // Same read as `customerInvoices`; the portal principal scopes it to the
        // authenticated organization's customer.
        self.customer_invoices(ctx, external_customer_id, page, per_page).await
    }

    /// `customerPortalWallets(externalCustomerId:)`.
    async fn customer_portal_wallets(
        &self,
        ctx: &Context<'_>,
        external_customer_id: String,
    ) -> Result<WalletCollection> {
        self.wallets(ctx, external_customer_id).await
    }

    /// `customerPortalUsage(externalCustomerId:, externalSubscriptionId:)` —
    /// the customer's live usage for a subscription, priced but not invoiced
    /// (`Invoices::CustomerUsageService`).
    async fn customer_portal_usage(
        &self,
        ctx: &Context<'_>,
        external_customer_id: String,
        external_subscription_id: String,
    ) -> Result<CustomerUsage> {
        let org = require_org(ctx)?;
        let customer = db(ctx)
            .find_customer_by_external_id(org.id, &external_customer_id)
            .await?
            .ok_or_else(|| async_graphql::Error::new("customer_not_found"))?;
        let sub = db(ctx)
            .find_subscription_by_external_id(org.id, &external_subscription_id)
            .await?
            .ok_or_else(|| async_graphql::Error::new("subscription_not_found"))?;
        if sub.customer_id != customer.id {
            return Err(async_graphql::Error::new("subscription_not_found"));
        }
        let plan = db(ctx).find_plan(sub.plan_id).await?;
        let currency = plan.currency();
        let charges = db(ctx).charges_for_plan(plan.id).await?;

        // Current calendar-month period.
        let today = chrono::Utc::now().date_naive();
        let billing_at = first_of_next_month(today);
        let dates = rago_billing::dates::MonthlyDates {
            subscription_at: sub.subscription_at.map(|t| t.date_naive()).unwrap_or(today),
            billing_at,
            anniversary: matches!(sub.billing_time, rago_core::enums::BillingTime::Anniversary),
            pay_in_advance: plan.pay_in_advance,
            current_usage: false,
        };
        let from = dates.boundaries().from_date.and_hms_opt(0, 0, 0).unwrap();
        let to = chrono::Utc::now().naive_utc();

        let mut amount_cents = 0i64;
        let mut charges_usage = Vec::new();
        for charge in &charges {
            let Some(mid) = charge.billable_metric_id else { continue };
            let metric = db(ctx).find_billable_metric(mid).await?;
            let events = db(ctx)
                .events_for_external_subscription(org.id, &sub.external_id, &metric.code, from, to)
                .await?;
            let fee = rago_billing::charge_to_fee(&metric, charge, &sub, &events, currency, None)
                .map_err(|e| async_graphql::Error::new(e.to_string()))?;
            amount_cents += fee.amount_cents;
            charges_usage.push(ChargeUsage {
                units: fee.units.to_string(),
                amount_cents: fee.amount_cents,
                billable_metric_code: metric.code.clone(),
                billable_metric_name: metric.name.clone(),
            });
        }
        Ok(CustomerUsage {
            currency: currency.code(),
            amount_cents,
            total_amount_cents: amount_cents,
            charges_usage,
        })
    }

    /// `grossRevenues` — finalized-invoice revenue by month.
    async fn gross_revenues(&self, ctx: &Context<'_>) -> Result<AnalyticsCollection> {
        let org = require_org(ctx)?;
        let points = db(ctx).gross_revenues(org.id).await?;
        Ok(AnalyticsCollection {
            collection: points.into_iter().map(Into::into).collect(),
        })
    }

    /// `mrrs` — monthly recurring revenue (subscription fees) by month.
    async fn mrrs(&self, ctx: &Context<'_>) -> Result<AnalyticsCollection> {
        let org = require_org(ctx)?;
        let points = db(ctx).mrrs(org.id).await?;
        Ok(AnalyticsCollection {
            collection: points.into_iter().map(Into::into).collect(),
        })
    }

    /// `invoiceCollections` — invoiced amount by month + payment status.
    async fn invoice_collections(&self, ctx: &Context<'_>) -> Result<InvoiceCollectionAnalytics> {
        let org = require_org(ctx)?;
        let points = db(ctx).invoice_collections(org.id).await?;
        Ok(InvoiceCollectionAnalytics {
            collection: points.into_iter().map(Into::into).collect(),
        })
    }

    /// `overdueBalances` — outstanding (finalized, not succeeded) amount.
    async fn overdue_balances(&self, ctx: &Context<'_>) -> Result<AnalyticsCollection> {
        let org = require_org(ctx)?;
        let points = db(ctx).overdue_balances(org.id).await?;
        Ok(AnalyticsCollection {
            collection: points.into_iter().map(Into::into).collect(),
        })
    }

    /// `creditNote(id:)`.
    async fn credit_note(&self, ctx: &Context<'_>, id: ID) -> Result<Option<CreditNote>> {
        let _org = require_org(ctx)?;
        let uuid = Uuid::parse_str(id.as_str()).map_err(|_| async_graphql::Error::new("invalid_id"))?;
        Ok(db(ctx).find_credit_note(uuid).await?.map(CreditNote))
    }

    // ---- Relay Connection pagination (first/after cursors) ---------------

    /// `customersConnection(first:, after:)` — Relay-paginated customers.
    async fn customers_connection(
        &self,
        ctx: &Context<'_>,
        first: Option<i64>,
        after: Option<String>,
    ) -> Result<CustomerConnection> {
        let org = require_org(ctx)?;
        let (lim, start) = relay_window(first, after);
        let items = db(ctx).list_customers(org.id, lim, start).await?;
        let total = db(ctx).count_customers(org.id).await?;
        Ok(CustomerConnection::build(items.into_iter().map(Customer).collect(), start, total))
    }

    /// `subscriptionsConnection(first:, after:, status:, externalId:)` —
    /// Relay-paginated subscriptions across the org, filtered by status /
    /// external-id substring.
    async fn subscriptions_connection(
        &self,
        ctx: &Context<'_>,
        first: Option<i64>,
        after: Option<String>,
        status: Option<String>,
        external_id: Option<String>,
    ) -> Result<SubscriptionConnection> {
        let org = require_org(ctx)?;
        let (lim, start) = relay_window(first, after);
        let status_i = status.as_deref().and_then(subscription_status_i32);
        let subs = db(ctx).list_subscriptions(org.id, status_i, external_id.as_deref(), lim, start).await?;
        let total = db(ctx).count_subscriptions(org.id, status_i, external_id.as_deref()).await?;
        let mut nodes = Vec::with_capacity(subs.len());
        for sub in subs {
            let plan = db(ctx).find_plan(sub.plan_id).await?;
            let ext = db(ctx).customer_external_id(sub.customer_id).await?.unwrap_or_default();
            nodes.push(Subscription {
                plan_code: plan.code.clone(),
                plan_amount_cents: plan.amount_cents,
                plan_amount_currency: plan.amount_currency.clone(),
                external_customer_id: ext,
                sub,
            });
        }
        Ok(SubscriptionConnection::build(nodes, start, total))
    }

    /// `invoicesConnection(first:, after:, status:, paymentStatus:)` —
    /// Relay-paginated invoices across the org, filtered by status /
    /// payment-status.
    async fn invoices_connection(
        &self,
        ctx: &Context<'_>,
        first: Option<i64>,
        after: Option<String>,
        status: Option<String>,
        payment_status: Option<String>,
    ) -> Result<InvoiceConnection> {
        let org = require_org(ctx)?;
        let (lim, start) = relay_window(first, after);
        let status_i = status.as_deref().and_then(invoice_status_i32);
        let payment_i = payment_status.as_deref().and_then(payment_status_i32);
        let invoices = db(ctx).list_invoices(org.id, status_i, payment_i, None, None, lim, start).await?;
        let total = db(ctx).count_invoices(org.id, status_i, payment_i, None, None).await?;
        let mut nodes = Vec::with_capacity(invoices.len());
        for invoice in invoices {
            let fees = db(ctx).fees_for_invoice(invoice.id).await?;
            nodes.push(Invoice { invoice, fees });
        }
        Ok(InvoiceConnection::build(nodes, start, total))
    }

    /// `eventsConnection(first:, after:, code:)`.
    async fn events_connection(
        &self,
        ctx: &Context<'_>,
        first: Option<i64>,
        after: Option<String>,
        code: Option<String>,
    ) -> Result<EventConnection> {
        let org = require_org(ctx)?;
        let (lim, start) = relay_window(first, after);
        let events = db(ctx).list_events(org.id, code.as_deref(), None, lim, start).await?;
        let total = db(ctx).count_events(org.id, code.as_deref(), None).await?;
        Ok(EventConnection::build(events.into_iter().map(Event).collect(), start, total))
    }

    // ---- add-ons + usage alerts (GraphQL tail) ---------------------------

    /// `addOnsConnection(first:, after:)`.
    async fn add_ons_connection(
        &self,
        ctx: &Context<'_>,
        first: Option<i64>,
        after: Option<String>,
    ) -> Result<AddOnConnection> {
        let org = require_org(ctx)?;
        let (lim, start) = relay_window(first, after);
        let (rows, total) = db(ctx).list_add_ons(org.id, lim, start).await?;
        let nodes = rows
            .into_iter()
            .map(|(id, name, code, amount_cents, currency)| AddOn {
                id: id.into(),
                name,
                code,
                amount_cents,
                amount_currency: currency,
            })
            .collect();
        Ok(AddOnConnection::build(nodes, start, total))
    }

    /// `usageAlerts(externalSubscriptionId:)` — the subscription's alerts.
    async fn usage_alerts(
        &self,
        ctx: &Context<'_>,
        external_subscription_id: String,
    ) -> Result<Vec<UsageAlert>> {
        let org = require_org(ctx)?;
        let rows = db(ctx).list_usage_alerts(org.id, &external_subscription_id).await?;
        Ok(rows
            .into_iter()
            .map(|(id, alert_type, metric, threshold, recurring, _prev, last)| UsageAlert {
                id: id.into(),
                alert_type,
                billable_metric_code: metric,
                threshold_cents: threshold,
                recurring,
                last_triggered_at: last.map(|t| t.and_utc().to_rfc3339()),
            })
            .collect())
    }

    // ---- dashboard admin (api keys / webhook endpoints / integrations) ---

    /// `apiKeys` — the org's API keys.
    async fn api_keys(&self, ctx: &Context<'_>) -> Result<Vec<ApiKey>> {
        let org = require_org(ctx)?;
        let rows = db(ctx).list_api_keys(org.id).await?;
        Ok(rows.into_iter().map(|(id, name, value, created)| ApiKey {
            id: id.into(),
            name,
            value,
            created_at: created.and_utc().to_rfc3339(),
        }).collect())
    }

    /// `webhookEndpoints` — the org's outbound webhook endpoints.
    async fn webhook_endpoints(&self, ctx: &Context<'_>) -> Result<Vec<WebhookEndpoint>> {
        let org = require_org(ctx)?;
        let rows = db(ctx).list_webhook_endpoints(org.id).await?;
        Ok(rows.into_iter().map(|(id, url, algo)| WebhookEndpoint {
            id: id.into(),
            webhook_url: url,
            signature_algo: algo,
        }).collect())
    }

    /// `integrations` — the org's connected integrations (config rows).
    async fn integrations(&self, ctx: &Context<'_>) -> Result<Vec<Integration>> {
        let org = require_org(ctx)?;
        Ok(db(ctx).list_integrations(org.id).await?.into_iter().map(Into::into).collect())
    }

    /// `dunningCampaigns` — the org's dunning-campaign configurations.
    async fn dunning_campaigns(&self, ctx: &Context<'_>) -> Result<Vec<DunningCampaign>> {
        let org = require_org(ctx)?;
        let rows = db(ctx).list_dunning_campaigns(org.id).await?;
        Ok(rows.into_iter().map(|(id, name, code, max, days, applied)| DunningCampaign {
            id: id.into(),
            name,
            code,
            max_attempts: max as i64,
            days_between_attempts: days as i64,
            applied_to_organization: applied,
        }).collect())
    }
}

/// Decode a Relay `first`/`after` into `(limit, start_offset)`.
fn relay_window(first: Option<i64>, after: Option<String>) -> (i64, i64) {
    let lim = first.filter(|n| *n > 0).unwrap_or(PER_PAGE);
    // `after` is the cursor of the last item already seen; start after it.
    let start = after.as_deref().and_then(crate::types::decode_cursor).map(|o| o + 1).unwrap_or(0);
    (lim, start)
}

fn subscription_status_i32(s: &str) -> Option<i32> {
    use rago_core::enums::SubscriptionStatus;
    Some(match s {
        "pending" => SubscriptionStatus::Pending,
        "active" => SubscriptionStatus::Active,
        "terminated" => SubscriptionStatus::Terminated,
        "canceled" => SubscriptionStatus::Canceled,
        _ => return None,
    }.as_i32())
}
fn invoice_status_i32(s: &str) -> Option<i32> {
    use rago_core::enums::InvoiceStatus;
    Some(match s {
        "draft" => InvoiceStatus::Draft,
        "finalized" => InvoiceStatus::Finalized,
        "voided" => InvoiceStatus::Voided,
        "pending" => InvoiceStatus::Pending,
        _ => return None,
    }.as_i32())
}
fn payment_status_i32(s: &str) -> Option<i32> {
    use rago_core::enums::PaymentStatus;
    Some(match s {
        "pending" => PaymentStatus::Pending,
        "succeeded" => PaymentStatus::Succeeded,
        "failed" => PaymentStatus::Failed,
        "refunded" => PaymentStatus::Refunded,
        _ => return None,
    }.as_i32())
}
