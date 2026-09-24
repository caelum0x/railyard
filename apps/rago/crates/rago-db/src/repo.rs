//! Repository — read the billing entities and persist invoices/fees. Uses
//! runtime `sqlx::query` (no compile-time DB needed). Mirrors the ActiveRecord
//! finders the billing services rely on.

use chrono::{NaiveDate, NaiveDateTime};
use rago_core::models::*;
use sqlx::pool::PoolConnection;
use sqlx::{PgPool, Postgres, Row};
use uuid::Uuid;

use crate::error::Result;
use crate::mapping;

/// A leased durable-queue job.
#[derive(Debug, Clone)]
pub struct LeasedJob {
    pub id: Uuid,
    pub job_type: String,
    pub payload: serde_json::Value,
    pub attempts: i32,
    pub max_attempts: i32,
}

/// A webhook due for re-delivery.
#[derive(Debug, Clone)]
pub struct RetryingWebhook {
    pub id: Uuid,
    pub organization_id: Uuid,
    pub webhook_type: String,
    pub object_type: String,
    pub endpoint: String,
    pub payload: serde_json::Value,
    pub retries: i32,
}

/// A time-series analytics data point (month + currency + amount).
#[derive(Debug, Clone)]
pub struct AnalyticsPoint {
    pub month: String,
    pub currency: String,
    pub amount_cents: i64,
    pub count: i64,
}

/// An invoice-collections point (also carries the payment status).
#[derive(Debug, Clone)]
pub struct InvoiceCollectionPoint {
    pub month: String,
    pub currency: String,
    pub payment_status: i32,
    pub invoices_count: i64,
    pub amount_cents: i64,
}

fn analytics_point(row: &sqlx::postgres::PgRow) -> AnalyticsPoint {
    AnalyticsPoint {
        month: row.get("month"),
        currency: row.get("currency"),
        amount_cents: row.get("amount"),
        count: row.get("n"),
    }
}

/// Handle around a Postgres connection pool.
#[derive(Clone)]
pub struct Db {
    pub pool: PgPool,
}

/// A held session-level Postgres advisory lock.
///
/// Acquired by [`Db::try_advisory_lock`], it owns a dedicated pooled
/// connection for the lock's lifetime (a session advisory lock is bound to the
/// connection that took it). Call [`AdvisoryLock::release`] to unlock and hand
/// the connection back to the pool; dropping without releasing keeps the lock
/// held until that connection is closed, so always release on every path.
pub struct AdvisoryLock {
    conn: PoolConnection<Postgres>,
    key: i64,
}

impl AdvisoryLock {
    /// Release the advisory lock (`pg_advisory_unlock`) and return the
    /// connection to the pool.
    pub async fn release(mut self) -> Result<()> {
        sqlx::query("SELECT pg_advisory_unlock($1)")
            .bind(self.key)
            .execute(&mut *self.conn)
            .await?;
        Ok(())
    }
}

impl Db {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Connect and return a [`Db`].
    pub async fn connect(database_url: &str) -> Result<Self> {
        let pool = PgPool::connect(database_url).await?;
        Ok(Self { pool })
    }

    /// Run the embedded migrations.
    pub async fn migrate(&self) -> Result<()> {
        crate::MIGRATOR
            .run(&self.pool)
            .await
            .map_err(sqlx::Error::from)?;
        Ok(())
    }

    /// Try to take a session-level Postgres advisory lock on `key` without
    /// blocking (`pg_try_advisory_lock`). Returns `Some(AdvisoryLock)` when this
    /// caller now owns the lock, or `None` when another session already holds it
    /// — the caller should then skip the guarded critical section rather than
    /// wait. Used to serialize the invoice charge path so two concurrent workers
    /// cannot both pass the "already charged?" check and double-charge.
    pub async fn try_advisory_lock(&self, key: i64) -> Result<Option<AdvisoryLock>> {
        let mut conn = self.pool.acquire().await?;
        let locked: bool = sqlx::query_scalar("SELECT pg_try_advisory_lock($1)")
            .bind(key)
            .fetch_one(&mut *conn)
            .await?;
        Ok(if locked {
            Some(AdvisoryLock { conn, key })
        } else {
            None
        })
    }

    pub async fn find_charge(&self, id: Uuid) -> Result<Charge> {
        let row = sqlx::query("SELECT * FROM charges WHERE id = $1")
            .bind(id)
            .fetch_one(&self.pool)
            .await?;
        mapping::charge(&row)
    }

    pub async fn find_billable_metric(&self, id: Uuid) -> Result<BillableMetric> {
        let row = sqlx::query("SELECT * FROM billable_metrics WHERE id = $1")
            .bind(id)
            .fetch_one(&self.pool)
            .await?;
        mapping::billable_metric(&row)
    }

    pub async fn find_plan(&self, id: Uuid) -> Result<Plan> {
        let row = sqlx::query("SELECT * FROM plans WHERE id = $1")
            .bind(id)
            .fetch_one(&self.pool)
            .await?;
        mapping::plan(&row)
    }

    pub async fn find_subscription(&self, id: Uuid) -> Result<Subscription> {
        let row = sqlx::query("SELECT * FROM subscriptions WHERE id = $1")
            .bind(id)
            .fetch_one(&self.pool)
            .await?;
        mapping::subscription(&row)
    }

    /// Charges attached to a plan (the plan's usage-based pricing).
    pub async fn charges_for_plan(&self, plan_id: Uuid) -> Result<Vec<Charge>> {
        let rows = sqlx::query("SELECT * FROM charges WHERE plan_id = $1 AND deleted_at IS NULL")
            .bind(plan_id)
            .fetch_all(&self.pool)
            .await?;
        rows.iter().map(mapping::charge).collect()
    }

    /// Taxes applicable at the organization level.
    pub async fn organization_taxes(&self, organization_id: Uuid) -> Result<Vec<Tax>> {
        let rows = sqlx::query("SELECT * FROM taxes WHERE organization_id = $1")
            .bind(organization_id)
            .fetch_all(&self.pool)
            .await?;
        rows.iter().map(mapping::tax).collect()
    }

    /// Events for a subscription/metric within a billing window.
    pub async fn events_in_period(
        &self,
        subscription_id: Uuid,
        code: &str,
        from: NaiveDateTime,
        to: NaiveDateTime,
    ) -> Result<Vec<Event>> {
        let rows = sqlx::query(
            "SELECT * FROM events \
             WHERE subscription_id = $1 AND code = $2 \
             AND timestamp >= $3 AND timestamp < $4 AND deleted_at IS NULL \
             ORDER BY timestamp ASC",
        )
        .bind(subscription_id)
        .bind(code)
        .bind(from)
        .bind(to)
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(mapping::event).collect()
    }

    /// `Events::Stores::PostgresStore#events` — the faithful read scope the
    /// aggregators consume: events for `external_subscription_id` + org + code
    /// within `[from, to]`, ordered by timestamp ascending. This is the Postgres
    /// event store's read path (the ClickHouse backend is a documented follow-up).
    pub async fn events_for_external_subscription(
        &self,
        organization_id: Uuid,
        external_subscription_id: &str,
        code: &str,
        from: NaiveDateTime,
        to: NaiveDateTime,
    ) -> Result<Vec<Event>> {
        let rows = sqlx::query(
            "SELECT * FROM events \
             WHERE external_subscription_id = $1 AND organization_id = $2 AND code = $3 \
             AND timestamp >= $4 AND timestamp <= $5 AND deleted_at IS NULL \
             ORDER BY timestamp ASC",
        )
        .bind(external_subscription_id)
        .bind(organization_id)
        .bind(code)
        .bind(from)
        .bind(to)
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(mapping::event).collect()
    }

    /// `Events::Stores::PostgresStore#events` with charge-filter scoping — the
    /// same read scope as [`Self::events_for_external_subscription`], additionally
    /// narrowed to events whose JSON `properties` match every `(key, values)`
    /// filter (`properties->>key = ANY(values)`), mirroring Lago's
    /// `filters_scope`. Passing an empty `filters` is identical to the unfiltered
    /// read. Grouping (`grouped_by`) is then applied in the aggregator over this
    /// scope (see `rago_billing::aggregation::aggregate_grouped`).
    pub async fn events_for_external_subscription_filtered(
        &self,
        organization_id: Uuid,
        external_subscription_id: &str,
        code: &str,
        from: NaiveDateTime,
        to: NaiveDateTime,
        filters: &[(String, Vec<String>)],
    ) -> Result<Vec<Event>> {
        let mut sql = String::from(
            "SELECT * FROM events \
             WHERE external_subscription_id = $1 AND organization_id = $2 AND code = $3 \
             AND timestamp >= $4 AND timestamp <= $5 AND deleted_at IS NULL",
        );
        // Filters are bound as parameters $6.. (never interpolated) to keep the
        // query injection-safe.
        let mut idx = 6;
        for _ in filters {
            sql.push_str(&format!(" AND properties->>${} = ANY(${})", idx, idx + 1));
            idx += 2;
        }
        sql.push_str(" ORDER BY timestamp ASC");

        let mut query = sqlx::query(&sql)
            .bind(external_subscription_id)
            .bind(organization_id)
            .bind(code)
            .bind(from)
            .bind(to);
        for (key, values) in filters {
            query = query.bind(key.clone()).bind(values.clone());
        }
        let rows = query.fetch_all(&self.pool).await?;
        rows.iter().map(mapping::event).collect()
    }

    /// Persist an event (`Events::CreateService` / bulk insert path).
    ///
    /// Dedup is enforced by the DB, not a racy check-then-insert: the unique
    /// index `index_unique_transaction_id` on
    /// `(organization_id, external_subscription_id, transaction_id)` (faithful to
    /// Lago) makes this an atomic `ON CONFLICT DO NOTHING`. Returns `true` when a
    /// new row was written and `false` when the event was a duplicate (the
    /// idempotent no-op) so callers never double-meter under concurrency.
    pub async fn insert_event(&self, event: &Event) -> Result<bool> {
        let result = sqlx::query(
            "INSERT INTO events (\
                id, organization_id, customer_id, subscription_id, transaction_id, code, \
                properties, external_customer_id, external_subscription_id, timestamp, \
                precise_total_amount_cents, created_at, updated_at) \
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13) \
             ON CONFLICT (organization_id, external_subscription_id, transaction_id) DO NOTHING",
        )
        .bind(event.id)
        .bind(event.organization_id)
        .bind(event.customer_id)
        .bind(event.subscription_id)
        .bind(&event.transaction_id)
        .bind(&event.code)
        .bind(&event.properties)
        .bind(event.external_customer_id.as_deref())
        .bind(event.external_subscription_id.as_deref())
        .bind(event.timestamp)
        .bind(event.precise_total_amount_cents)
        .bind(event.created_at.naive_utc())
        .bind(event.updated_at.naive_utc())
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected() > 0)
    }

    /// Persist an invoice (the rollup columns computed by the billing engine).
    pub async fn insert_invoice(&self, inv: &Invoice) -> Result<()> {
        insert_invoice_exec(&self.pool, inv).await
    }

    /// Persist a fee.
    pub async fn insert_fee(&self, fee: &Fee) -> Result<()> {
        insert_fee_exec(&self.pool, fee).await
    }

    /// Persist a subscription invoice + its fees behind the re-bill guard.
    ///
    /// At most one recurring invoice may exist per `(subscription_id, charges
    /// period)`. The whole write runs in a single transaction: first it reserves
    /// the period by inserting the `invoice_subscriptions` guard row (the unique
    /// partial index makes a duplicate a no-op); only if the reservation wins does
    /// it write the invoice and fees. A scheduler/job re-run for the same
    /// subscription+period therefore produces exactly one invoice — re-runs return
    /// [`BilledInvoice::AlreadyBilled`] with the invoice that already exists,
    /// instead of creating a duplicate (which the payment path would double-charge).
    pub async fn insert_subscription_invoice_guarded(
        &self,
        invoice: &Invoice,
        fees: &[Fee],
        subscription_id: Uuid,
        charges_from: NaiveDateTime,
        charges_to: NaiveDateTime,
    ) -> Result<BilledInvoice> {
        let mut invoice = invoice.clone();
        self.insert_subscription_invoice_guarded_inner(
            &mut invoice,
            fees,
            &PrepaidSettlement::NONE,
            subscription_id,
            charges_from,
            charges_to,
        )
        .await
    }

    /// Guarded subscription-invoice insert that *also* settles prepaid wallet
    /// credit atomically.
    ///
    /// The wallet draw-down (`wallet_transactions` inserts + wallet balance
    /// decrements) and the invoice/fees write share the **same** transaction and
    /// are gated behind the same period reservation. So:
    /// * on the winning `Created` path the invoice's reduced total, the
    ///   outbound wallet transactions, and the decreased balances all commit
    ///   together (conservation: `Σ wallet draw-down == invoice.prepaid_credit`);
    /// * on a lost race / re-run the guard fires *before* any wallet mutation,
    ///   so the wallet is **never** double-charged — exactly-once settlement,
    ///   mirroring the re-bill invoice guarantee.
    ///
    /// When `invoice.number` is empty it is assigned the organization's next
    /// sequential number inside the same transaction (only on the winning
    /// reservation, so re-runs never consume a number).
    pub async fn insert_subscription_invoice_with_prepaid_guarded(
        &self,
        invoice: &mut Invoice,
        fees: &[Fee],
        settlement: &PrepaidSettlement<'_>,
        subscription_id: Uuid,
        charges_from: NaiveDateTime,
        charges_to: NaiveDateTime,
    ) -> Result<BilledInvoice> {
        self.insert_subscription_invoice_guarded_inner(
            invoice,
            fees,
            settlement,
            subscription_id,
            charges_from,
            charges_to,
        )
        .await
    }

    async fn insert_subscription_invoice_guarded_inner(
        &self,
        invoice: &mut Invoice,
        fees: &[Fee],
        settlement: &PrepaidSettlement<'_>,
        subscription_id: Uuid,
        charges_from: NaiveDateTime,
        charges_to: NaiveDateTime,
    ) -> Result<BilledInvoice> {
        let mut tx = self.pool.begin().await?;

        // Reserve the (subscription, period). `ON CONFLICT DO NOTHING` against the
        // unique partial index is the single source of truth: a concurrent racer
        // (or a plain re-run) affects zero rows.
        let reserved = sqlx::query(
            "INSERT INTO invoice_subscriptions (\
                id, organization_id, invoice_id, subscription_id, recurring, \
                timestamp, from_datetime, to_datetime, charges_from_datetime, charges_to_datetime, \
                created_at, updated_at) \
             VALUES (gen_random_uuid(), $1, $2, $3, true, $4, $4, $5, $4, $5, now(), now()) \
             ON CONFLICT (subscription_id, charges_from_datetime, charges_to_datetime) \
                 WHERE recurring IS TRUE \
             DO NOTHING",
        )
        .bind(invoice.organization_id)
        .bind(invoice.id)
        .bind(subscription_id)
        .bind(charges_from)
        .bind(charges_to)
        .execute(&mut *tx)
        .await?;

        if reserved.rows_affected() == 0 {
            // Period already billed — discard this attempt and return the winner.
            // Crucially, we return *before* touching any wallet, so a re-run or a
            // lost race never re-draws prepaid credit.
            tx.rollback().await?;
            let existing = self
                .existing_period_invoice(subscription_id, charges_from, charges_to)
                .await?
                .ok_or_else(|| {
                    sqlx::Error::Protocol(
                        "re-bill guard fired but no existing invoice found".into(),
                    )
                })?;
            return Ok(BilledInvoice::AlreadyBilled(existing));
        }

        crate::numbering::number_if_needed(&mut *tx, invoice).await?;
        insert_invoice_exec(&mut *tx, invoice).await?;
        for fee in fees {
            insert_fee_exec(&mut *tx, fee).await?;
        }
        // Prepaid settlement — same transaction, so it is all-or-nothing with the
        // invoice write and only happens on the winning reservation.
        for wtx in settlement.transactions {
            insert_wallet_transaction_exec(&mut *tx, wtx).await?;
        }
        for wallet in settlement.updated_wallets {
            update_wallet_balance_exec(&mut *tx, wallet).await?;
        }
        tx.commit().await?;
        Ok(BilledInvoice::Created)
    }

    /// The recurring invoice already billed for `(subscription, charges period)`,
    /// if any (the re-bill guard's lookup).
    pub async fn existing_period_invoice(
        &self,
        subscription_id: Uuid,
        charges_from: NaiveDateTime,
        charges_to: NaiveDateTime,
    ) -> Result<Option<Invoice>> {
        let row = sqlx::query(
            "SELECT i.* FROM invoices i \
             JOIN invoice_subscriptions s ON s.invoice_id = i.id \
             WHERE s.subscription_id = $1 AND s.charges_from_datetime = $2 \
               AND s.charges_to_datetime = $3 AND s.recurring IS TRUE \
             LIMIT 1",
        )
        .bind(subscription_id)
        .bind(charges_from)
        .bind(charges_to)
        .fetch_optional(&self.pool)
        .await?;
        row.as_ref().map(mapping::invoice).transpose()
    }

    /// `PastUsageQuery` — finalized invoice periods for a subscription, newest
    /// first: one row per past charges period with its invoice rollups. Powers
    /// `GET /customers/:id/past_usage`. Returns
    /// `(invoice_id, charges_from, charges_to, issuing_date, currency, fees_amount_cents, taxes_amount_cents)`.
    pub async fn subscription_past_usage_periods(
        &self,
        subscription_id: Uuid,
    ) -> Result<Vec<(Uuid, NaiveDateTime, NaiveDateTime, NaiveDate, String, i64, i64)>> {
        let rows = sqlx::query(
            "SELECT i.id AS invoice_id, \
                    s.charges_from_datetime AS charges_from, \
                    s.charges_to_datetime AS charges_to, \
                    i.issuing_date AS issuing_date, \
                    i.currency AS currency, \
                    i.fees_amount_cents AS fees_amount_cents, \
                    i.taxes_amount_cents AS taxes_amount_cents \
             FROM invoices i \
             JOIN invoice_subscriptions s ON s.invoice_id = i.id \
             WHERE s.subscription_id = $1 \
               AND s.charges_from_datetime IS NOT NULL \
               AND s.charges_to_datetime IS NOT NULL \
             ORDER BY s.charges_to_datetime DESC",
        )
        .bind(subscription_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .iter()
            .map(|r| {
                (
                    r.get::<Uuid, _>("invoice_id"),
                    r.get::<NaiveDateTime, _>("charges_from"),
                    r.get::<NaiveDateTime, _>("charges_to"),
                    r.get::<NaiveDate, _>("issuing_date"),
                    r.get::<Option<String>, _>("currency").unwrap_or_else(|| "USD".into()),
                    r.get::<i64, _>("fees_amount_cents"),
                    r.get::<i64, _>("taxes_amount_cents"),
                )
            })
            .collect())
    }

    /// Count rows in a table — a trivial connectivity probe used by examples.
    pub async fn count(&self, table: &str) -> Result<i64> {
        // table is a fixed, caller-controlled identifier; not user input.
        let row = sqlx::query(&format!("SELECT COUNT(*) AS n FROM {table}"))
            .fetch_one(&self.pool)
            .await?;
        Ok(row.try_get("n")?)
    }

    // ---- REST API repositories -------------------------------------------

    /// `ApiKeys::CacheService` — resolve a bearer token to its key + org.
    pub async fn authenticate(&self, value: &str) -> Result<Option<(ApiKey, Organization)>> {
        let Some(key_row) = sqlx::query("SELECT * FROM api_keys WHERE value = $1")
            .bind(value)
            .fetch_optional(&self.pool)
            .await?
        else {
            return Ok(None);
        };
        let key = mapping::api_key(&key_row)?;
        let org_row = sqlx::query("SELECT * FROM organizations WHERE id = $1")
            .bind(key.organization_id)
            .fetch_one(&self.pool)
            .await?;
        Ok(Some((key, mapping::organization(&org_row)?)))
    }

    pub async fn insert_organization(&self, o: &Organization) -> Result<()> {
        let hmac_key = o
            .hmac_key
            .clone()
            .unwrap_or_else(|| Uuid::new_v4().to_string());
        sqlx::query(
            "INSERT INTO organizations (id, name, slug, default_currency, email, country, \
             tax_identification_number, timezone, document_locale, finalize_zero_amount_invoice, \
             hmac_key, created_at, updated_at) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13)",
        )
        .bind(o.id)
        .bind(&o.name)
        .bind(&o.slug)
        .bind(&o.default_currency)
        .bind(o.email.as_deref())
        .bind(o.country.as_deref())
        .bind(o.tax_identification_number.as_deref())
        .bind(&o.timezone)
        .bind(&o.document_locale)
        .bind(o.finalize_zero_amount_invoice)
        .bind(&hmac_key)
        .bind(o.created_at.naive_utc())
        .bind(o.updated_at.naive_utc())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn insert_api_key(&self, k: &ApiKey) -> Result<()> {
        sqlx::query(
            "INSERT INTO api_keys (id, organization_id, value, name, permissions, expires_at, \
             last_used_at, created_at, updated_at) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9)",
        )
        .bind(k.id)
        .bind(k.organization_id)
        .bind(&k.value)
        .bind(k.name.as_deref())
        .bind(&k.permissions)
        .bind(k.expires_at.map(|t| t.naive_utc()))
        .bind(k.last_used_at.map(|t| t.naive_utc()))
        .bind(k.created_at.naive_utc())
        .bind(k.updated_at.naive_utc())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn insert_customer(&self, c: &Customer) -> Result<()> {
        sqlx::query(
            "INSERT INTO customers (id, organization_id, billing_entity_id, external_id, name, \
             email, country, currency, timezone, tax_identification_number, created_at, updated_at) \
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12)",
        )
        .bind(c.id).bind(c.organization_id).bind(c.billing_entity_id).bind(&c.external_id)
        .bind(c.name.as_deref()).bind(c.email.as_deref()).bind(c.country.as_deref())
        .bind(c.currency.as_deref()).bind(c.timezone.as_deref())
        .bind(c.tax_identification_number.as_deref())
        .bind(c.created_at.naive_utc()).bind(c.updated_at.naive_utc())
        .execute(&self.pool).await?;
        Ok(())
    }

    pub async fn find_customer_by_external_id(
        &self,
        org: Uuid,
        external_id: &str,
    ) -> Result<Option<Customer>> {
        let row = sqlx::query("SELECT * FROM customers WHERE organization_id = $1 AND external_id = $2 AND deleted_at IS NULL")
            .bind(org).bind(external_id).fetch_optional(&self.pool).await?;
        row.as_ref().map(mapping::customer).transpose()
    }

    /// Update a customer's mutable profile fields (name/email/country/currency/etc.).
    pub async fn update_customer(&self, c: &Customer) -> Result<()> {
        sqlx::query(
            "UPDATE customers SET name = $2, email = $3, country = $4, currency = $5, \
             timezone = $6, tax_identification_number = $7, updated_at = $8 WHERE id = $1",
        )
        .bind(c.id)
        .bind(c.name.as_deref())
        .bind(c.email.as_deref())
        .bind(c.country.as_deref())
        .bind(c.currency.as_deref())
        .bind(c.timezone.as_deref())
        .bind(c.tax_identification_number.as_deref())
        .bind(c.updated_at.naive_utc())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn list_customers(
        &self,
        org: Uuid,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<Customer>> {
        let rows = sqlx::query("SELECT * FROM customers WHERE organization_id = $1 AND deleted_at IS NULL ORDER BY created_at DESC LIMIT $2 OFFSET $3")
            .bind(org).bind(limit).bind(offset).fetch_all(&self.pool).await?;
        rows.iter().map(mapping::customer).collect()
    }

    pub async fn count_customers(&self, org: Uuid) -> Result<i64> {
        let row = sqlx::query(
            "SELECT COUNT(*) AS n FROM customers WHERE organization_id = $1 AND deleted_at IS NULL",
        )
        .bind(org)
        .fetch_one(&self.pool)
        .await?;
        Ok(row.try_get("n")?)
    }

    pub async fn insert_plan(&self, p: &Plan) -> Result<()> {
        sqlx::query(
            "INSERT INTO plans (id, organization_id, name, code, interval, description, amount_cents, \
             amount_currency, trial_period, pay_in_advance, bill_charges_monthly, parent_id, \
             invoice_display_name, created_at, updated_at) \
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15)",
        )
        .bind(p.id).bind(p.organization_id).bind(&p.name).bind(&p.code)
        .bind(p.interval.map(|i| i.as_i32())).bind(p.description.as_deref())
        .bind(p.amount_cents).bind(&p.amount_currency).bind(p.trial_period)
        .bind(p.pay_in_advance).bind(p.bill_charges_monthly).bind(p.parent_id)
        .bind(p.invoice_display_name.as_deref())
        .bind(p.created_at.naive_utc()).bind(p.updated_at.naive_utc())
        .execute(&self.pool).await?;
        Ok(())
    }

    pub async fn find_plan_by_code(&self, org: Uuid, code: &str) -> Result<Option<Plan>> {
        let row = sqlx::query(
            "SELECT * FROM plans WHERE organization_id = $1 AND code = $2 AND deleted_at IS NULL",
        )
        .bind(org)
        .bind(code)
        .fetch_optional(&self.pool)
        .await?;
        row.as_ref().map(mapping::plan).transpose()
    }

    pub async fn insert_billable_metric(&self, m: &BillableMetric) -> Result<()> {
        sqlx::query(
            "INSERT INTO billable_metrics (id, organization_id, name, code, description, \
             aggregation_type, field_name, recurring, expression, rounding_function, \
             rounding_precision, created_at, updated_at, custom_aggregator) \
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14)",
        )
        .bind(m.id).bind(m.organization_id).bind(&m.name).bind(&m.code).bind(m.description.as_deref())
        .bind(m.aggregation_type.as_i32()).bind(m.field_name.as_deref()).bind(m.recurring)
        .bind(m.expression.as_deref()).bind(m.rounding_function.as_deref()).bind(m.rounding_precision)
        .bind(m.created_at.naive_utc()).bind(m.updated_at.naive_utc())
        .bind(m.custom_aggregator.as_deref())
        .execute(&self.pool).await?;
        Ok(())
    }

    pub async fn find_metric_by_code(
        &self,
        org: Uuid,
        code: &str,
    ) -> Result<Option<BillableMetric>> {
        let row = sqlx::query("SELECT * FROM billable_metrics WHERE organization_id = $1 AND code = $2 AND deleted_at IS NULL")
            .bind(org).bind(code).fetch_optional(&self.pool).await?;
        row.as_ref().map(mapping::billable_metric).transpose()
    }

    pub async fn insert_charge(&self, c: &Charge) -> Result<()> {
        sqlx::query(
            "INSERT INTO charges (id, organization_id, billable_metric_id, plan_id, code, \
             charge_model, properties, pay_in_advance, prorated, invoiceable, min_amount_cents, \
             amount_currency, accepts_target_wallet, invoice_display_name, parent_id, created_at, updated_at) \
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17)",
        )
        .bind(c.id).bind(c.organization_id).bind(c.billable_metric_id).bind(c.plan_id).bind(&c.code)
        .bind(c.charge_model.as_i32()).bind(&c.properties).bind(c.pay_in_advance).bind(c.prorated)
        .bind(c.invoiceable).bind(c.min_amount_cents).bind(c.amount_currency.as_deref())
        .bind(c.accepts_target_wallet).bind(c.invoice_display_name.as_deref()).bind(c.parent_id)
        .bind(c.created_at.naive_utc()).bind(c.updated_at.naive_utc())
        .execute(&self.pool).await?;
        Ok(())
    }

    pub async fn insert_subscription(&self, s: &Subscription) -> Result<()> {
        sqlx::query(
            "INSERT INTO subscriptions (id, organization_id, customer_id, plan_id, billing_entity_id, \
             external_id, name, status, billing_time, subscription_at, started_at, ending_at, \
             trial_ended_at, canceled_at, terminated_at, previous_subscription_id, created_at, updated_at) \
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17,$18)",
        )
        .bind(s.id).bind(s.organization_id).bind(s.customer_id).bind(s.plan_id).bind(s.billing_entity_id)
        .bind(&s.external_id).bind(s.name.as_deref()).bind(s.status.as_i32()).bind(s.billing_time.as_i32())
        .bind(s.subscription_at.map(|t| t.naive_utc())).bind(s.started_at.map(|t| t.naive_utc()))
        .bind(s.ending_at.map(|t| t.naive_utc())).bind(s.trial_ended_at.map(|t| t.naive_utc()))
        .bind(s.canceled_at.map(|t| t.naive_utc())).bind(s.terminated_at.map(|t| t.naive_utc()))
        .bind(s.previous_subscription_id).bind(s.created_at.naive_utc()).bind(s.updated_at.naive_utc())
        .execute(&self.pool).await?;
        Ok(())
    }

    pub async fn update_subscription(&self, s: &Subscription) -> Result<()> {
        sqlx::query(
            "UPDATE subscriptions SET status = $2, terminated_at = $3, canceled_at = $4, updated_at = $5 WHERE id = $1",
        )
        .bind(s.id).bind(s.status.as_i32()).bind(s.terminated_at.map(|t| t.naive_utc()))
        .bind(s.canceled_at.map(|t| t.naive_utc())).bind(s.updated_at.naive_utc())
        .execute(&self.pool).await?;
        Ok(())
    }

    pub async fn find_subscription_by_external_id(
        &self,
        org: Uuid,
        external_id: &str,
    ) -> Result<Option<Subscription>> {
        let row = sqlx::query("SELECT * FROM subscriptions WHERE organization_id = $1 AND external_id = $2 AND status != 2 ORDER BY created_at DESC LIMIT 1")
            .bind(org).bind(external_id).fetch_optional(&self.pool).await?;
        row.as_ref().map(mapping::subscription).transpose()
    }

    pub async fn insert_wallet(&self, w: &Wallet) -> Result<()> {
        let allowed_fee_types: Vec<i32> = w.allowed_fee_types.iter().map(|t| t.as_i32()).collect();
        sqlx::query(
            "INSERT INTO wallets (id, organization_id, customer_id, status, name, rate_amount, \
             credits_balance, consumed_credits, balance_cents, balance_currency, consumed_amount_cents, \
             ongoing_balance_cents, ongoing_usage_balance_cents, credits_ongoing_balance, \
             credits_ongoing_usage_balance, expiration_at, terminated_at, lock_version, priority, \
             created_at, updated_at, allowed_fee_types) \
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17,$18,$19,$20,$21,$22)",
        )
        .bind(w.id).bind(w.organization_id).bind(w.customer_id).bind(w.status.as_i32()).bind(w.name.as_deref())
        .bind(w.rate_amount).bind(w.credits_balance).bind(w.consumed_credits).bind(w.balance_cents)
        .bind(&w.balance_currency).bind(w.consumed_amount_cents).bind(w.ongoing_balance_cents)
        .bind(w.ongoing_usage_balance_cents).bind(w.credits_ongoing_balance).bind(w.credits_ongoing_usage_balance)
        .bind(w.expiration_at.map(|t| t.naive_utc())).bind(w.terminated_at.map(|t| t.naive_utc()))
        .bind(w.lock_version).bind(w.priority).bind(w.created_at.naive_utc()).bind(w.updated_at.naive_utc())
        .bind(allowed_fee_types)
        .execute(&self.pool).await?;
        Ok(())
    }

    pub async fn update_wallet_balance(&self, w: &Wallet) -> Result<()> {
        sqlx::query(
            "UPDATE wallets SET balance_cents = $2, credits_balance = $3, consumed_credits = $4, \
             consumed_amount_cents = $5, updated_at = $6 WHERE id = $1",
        )
        .bind(w.id)
        .bind(w.balance_cents)
        .bind(w.credits_balance)
        .bind(w.consumed_credits)
        .bind(w.consumed_amount_cents)
        .bind(w.updated_at.naive_utc())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn find_wallet(&self, id: Uuid) -> Result<Option<Wallet>> {
        let row = sqlx::query("SELECT * FROM wallets WHERE id = $1")
            .bind(id)
            .fetch_optional(&self.pool)
            .await?;
        row.as_ref().map(mapping::wallet).transpose()
    }

    pub async fn wallets_for_customer(&self, customer_id: Uuid) -> Result<Vec<Wallet>> {
        let rows = sqlx::query("SELECT * FROM wallets WHERE customer_id = $1 AND status = 0 ORDER BY priority ASC, created_at ASC")
            .bind(customer_id).fetch_all(&self.pool).await?;
        rows.iter().map(mapping::wallet).collect()
    }

    /// A page of a wallet's transaction ledger (credits/debits), newest first.
    /// Reads the existing `wallet_transactions` store — the same rows the
    /// top-up + draw-down paths write.
    pub async fn wallet_transactions_for_wallet(
        &self,
        wallet_id: Uuid,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<WalletTransaction>> {
        let rows = sqlx::query(
            "SELECT * FROM wallet_transactions WHERE wallet_id = $1 \
             ORDER BY created_at DESC LIMIT $2 OFFSET $3",
        )
        .bind(wallet_id)
        .bind(limit)
        .bind(offset)
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(mapping::wallet_transaction).collect()
    }

    /// Total transactions for a wallet (for pagination metadata).
    pub async fn count_wallet_transactions(&self, wallet_id: Uuid) -> Result<i64> {
        let row = sqlx::query(
            "SELECT COUNT(*)::bigint AS n FROM wallet_transactions WHERE wallet_id = $1",
        )
        .bind(wallet_id)
        .fetch_one(&self.pool)
        .await?;
        Ok(row.get::<i64, _>("n"))
    }

    pub async fn insert_wallet_transaction(&self, t: &WalletTransaction) -> Result<()> {
        sqlx::query(
            "INSERT INTO wallet_transactions (id, organization_id, wallet_id, transaction_type, status, \
             amount, credit_amount, invoice_id, settled_at, created_at, updated_at) \
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11)",
        )
        .bind(t.id).bind(t.organization_id).bind(t.wallet_id).bind(t.transaction_type.as_i32())
        .bind(t.status.as_i32()).bind(t.amount).bind(t.credit_amount).bind(t.invoice_id)
        .bind(t.settled_at.map(|x| x.naive_utc())).bind(t.created_at.naive_utc()).bind(t.updated_at.naive_utc())
        .execute(&self.pool).await?;
        Ok(())
    }

    pub async fn find_invoice(&self, id: Uuid) -> Result<Option<Invoice>> {
        let row = sqlx::query("SELECT * FROM invoices WHERE id = $1")
            .bind(id)
            .fetch_optional(&self.pool)
            .await?;
        row.as_ref().map(mapping::invoice).transpose()
    }

    /// Find an invoice scoped to an organization — the tenant-safe lookup for
    /// API handlers. A cross-org id resolves to `None` (indistinguishable from
    /// a nonexistent invoice, so existence never leaks).
    pub async fn find_invoice_for_org(&self, organization_id: Uuid, id: Uuid) -> Result<Option<Invoice>> {
        let row = sqlx::query("SELECT * FROM invoices WHERE id = $1 AND organization_id = $2")
            .bind(id)
            .bind(organization_id)
            .fetch_optional(&self.pool)
            .await?;
        row.as_ref().map(mapping::invoice).transpose()
    }

    pub async fn fees_for_invoice(&self, invoice_id: Uuid) -> Result<Vec<Fee>> {
        let rows = sqlx::query("SELECT * FROM fees WHERE invoice_id = $1 ORDER BY created_at ASC")
            .bind(invoice_id)
            .fetch_all(&self.pool)
            .await?;
        rows.iter().map(mapping::fee).collect()
    }

    pub async fn invoices_for_customer(
        &self,
        customer_id: Uuid,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<Invoice>> {
        let rows = sqlx::query("SELECT * FROM invoices WHERE customer_id = $1 ORDER BY created_at DESC LIMIT $2 OFFSET $3")
            .bind(customer_id).bind(limit).bind(offset).fetch_all(&self.pool).await?;
        rows.iter().map(mapping::invoice).collect()
    }

    pub async fn insert_coupon(&self, c: &Coupon) -> Result<()> {
        sqlx::query(
            "INSERT INTO coupons (id, organization_id, name, code, status, coupon_type, amount_cents, \
             amount_currency, percentage_rate, frequency, frequency_duration, expiration, expiration_at, \
             reusable, limited_plans, limited_billable_metrics, created_at, updated_at) \
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17,$18)",
        )
        .bind(c.id).bind(c.organization_id).bind(&c.name).bind(&c.code).bind(c.status.as_i32())
        .bind(c.coupon_type.as_i32()).bind(c.amount_cents).bind(c.amount_currency.as_deref())
        .bind(c.percentage_rate).bind(c.frequency.as_i32()).bind(c.frequency_duration)
        .bind(c.expiration.as_i32()).bind(c.expiration_at.map(|t| t.naive_utc())).bind(c.reusable)
        .bind(c.limited_plans).bind(c.limited_billable_metrics)
        .bind(c.created_at.naive_utc()).bind(c.updated_at.naive_utc())
        .execute(&self.pool).await?;
        Ok(())
    }

    // ---- GraphQL list queries --------------------------------------------

    pub async fn find_customer(&self, id: Uuid) -> Result<Option<Customer>> {
        let row = sqlx::query("SELECT * FROM customers WHERE id = $1")
            .bind(id)
            .fetch_optional(&self.pool)
            .await?;
        row.as_ref().map(mapping::customer).transpose()
    }

    pub async fn list_plans(&self, org: Uuid, limit: i64, offset: i64) -> Result<Vec<Plan>> {
        let rows = sqlx::query("SELECT * FROM plans WHERE organization_id = $1 AND deleted_at IS NULL ORDER BY created_at DESC LIMIT $2 OFFSET $3")
            .bind(org).bind(limit).bind(offset).fetch_all(&self.pool).await?;
        rows.iter().map(mapping::plan).collect()
    }
    pub async fn count_plans(&self, org: Uuid) -> Result<i64> {
        let row = sqlx::query(
            "SELECT COUNT(*) AS n FROM plans WHERE organization_id = $1 AND deleted_at IS NULL",
        )
        .bind(org)
        .fetch_one(&self.pool)
        .await?;
        Ok(row.try_get("n")?)
    }

    pub async fn list_billable_metrics(
        &self,
        org: Uuid,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<BillableMetric>> {
        let rows = sqlx::query("SELECT * FROM billable_metrics WHERE organization_id = $1 AND deleted_at IS NULL ORDER BY created_at DESC LIMIT $2 OFFSET $3")
            .bind(org).bind(limit).bind(offset).fetch_all(&self.pool).await?;
        rows.iter().map(mapping::billable_metric).collect()
    }
    pub async fn count_billable_metrics(&self, org: Uuid) -> Result<i64> {
        let row = sqlx::query("SELECT COUNT(*) AS n FROM billable_metrics WHERE organization_id = $1 AND deleted_at IS NULL")
            .bind(org).fetch_one(&self.pool).await?;
        Ok(row.try_get("n")?)
    }

    pub async fn list_coupons(&self, org: Uuid, limit: i64, offset: i64) -> Result<Vec<Coupon>> {
        let rows = sqlx::query("SELECT * FROM coupons WHERE organization_id = $1 AND deleted_at IS NULL ORDER BY created_at DESC LIMIT $2 OFFSET $3")
            .bind(org).bind(limit).bind(offset).fetch_all(&self.pool).await?;
        rows.iter().map(mapping::coupon).collect()
    }
    pub async fn count_coupons(&self, org: Uuid) -> Result<i64> {
        let row = sqlx::query(
            "SELECT COUNT(*) AS n FROM coupons WHERE organization_id = $1 AND deleted_at IS NULL",
        )
        .bind(org)
        .fetch_one(&self.pool)
        .await?;
        Ok(row.try_get("n")?)
    }

    pub async fn subscriptions_for_customer(
        &self,
        customer_id: Uuid,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<Subscription>> {
        let rows = sqlx::query("SELECT * FROM subscriptions WHERE customer_id = $1 ORDER BY created_at DESC LIMIT $2 OFFSET $3")
            .bind(customer_id).bind(limit).bind(offset).fetch_all(&self.pool).await?;
        rows.iter().map(mapping::subscription).collect()
    }
    pub async fn count_subscriptions_for_customer(&self, customer_id: Uuid) -> Result<i64> {
        let row = sqlx::query("SELECT COUNT(*) AS n FROM subscriptions WHERE customer_id = $1")
            .bind(customer_id)
            .fetch_one(&self.pool)
            .await?;
        Ok(row.try_get("n")?)
    }
    pub async fn count_invoices_for_customer(&self, customer_id: Uuid) -> Result<i64> {
        let row = sqlx::query("SELECT COUNT(*) AS n FROM invoices WHERE customer_id = $1")
            .bind(customer_id)
            .fetch_one(&self.pool)
            .await?;
        Ok(row.try_get("n")?)
    }

    /// Persist a credit note (`CreditNotes::CreateService`).
    pub async fn insert_credit_note(&self, cn: &CreditNote) -> Result<()> {
        sqlx::query(
            "INSERT INTO credit_notes (id, organization_id, customer_id, invoice_id, sequential_id, \
             number, issuing_date, reason, credit_status, refund_status, status, credit_amount_cents, \
             credit_amount_currency, balance_amount_cents, refund_amount_cents, total_amount_cents, \
             total_amount_currency, taxes_amount_cents, taxes_rate, coupons_adjustment_amount_cents, \
             precise_coupons_adjustment_amount_cents, precise_taxes_amount_cents, created_at, updated_at) \
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17,$18,$19,$20,$21,$22, now(), now())",
        )
        .bind(cn.id).bind(cn.organization_id).bind(cn.customer_id).bind(cn.invoice_id).bind(cn.sequential_id)
        .bind(&cn.number).bind(cn.issuing_date).bind(cn.reason).bind(cn.credit_status).bind(cn.refund_status)
        .bind(cn.status).bind(cn.credit_amount_cents).bind(&cn.credit_amount_currency).bind(cn.balance_amount_cents)
        .bind(cn.refund_amount_cents).bind(cn.total_amount_cents).bind(&cn.total_amount_currency)
        .bind(cn.taxes_amount_cents).bind(cn.taxes_rate).bind(cn.coupons_adjustment_amount_cents)
        .bind(cn.precise_coupons_adjustment_amount_cents).bind(cn.precise_taxes_amount_cents)
        .execute(&self.pool).await?;
        Ok(())
    }

    /// Next credit-note sequential id for an invoice.
    pub async fn next_credit_note_sequential_id(&self, invoice_id: Uuid) -> Result<i32> {
        let row = sqlx::query("SELECT COALESCE(MAX(sequential_id), 0) + 1 AS n FROM credit_notes WHERE invoice_id = $1")
            .bind(invoice_id)
            .fetch_one(&self.pool)
            .await?;
        Ok(row.try_get("n")?)
    }

    pub async fn find_coupon_by_code(&self, org: Uuid, code: &str) -> Result<Option<Coupon>> {
        let row = sqlx::query(
            "SELECT * FROM coupons WHERE organization_id = $1 AND code = $2 AND deleted_at IS NULL",
        )
        .bind(org)
        .bind(code)
        .fetch_optional(&self.pool)
        .await?;
        row.as_ref().map(mapping::coupon).transpose()
    }

    pub async fn insert_applied_coupon(&self, a: &AppliedCoupon) -> Result<()> {
        sqlx::query(
            "INSERT INTO applied_coupons (id, organization_id, coupon_id, customer_id, status, \
             amount_cents, amount_currency, percentage_rate, frequency, frequency_duration, \
             frequency_duration_remaining, created_at, updated_at) \
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13)",
        )
        .bind(a.id)
        .bind(a.organization_id)
        .bind(a.coupon_id)
        .bind(a.customer_id)
        .bind(a.status.as_i32())
        .bind(a.amount_cents)
        .bind(a.amount_currency.as_deref())
        .bind(a.percentage_rate)
        .bind(a.frequency.as_i32())
        .bind(a.frequency_duration)
        .bind(a.frequency_duration_remaining)
        .bind(a.created_at.naive_utc())
        .bind(a.updated_at.naive_utc())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    // ---- payments / webhooks / scheduler ---------------------------------

    /// All active subscriptions (scheduler input).
    pub async fn active_subscriptions(&self) -> Result<Vec<Subscription>> {
        let rows =
            sqlx::query("SELECT * FROM subscriptions WHERE status = 1 ORDER BY created_at ASC")
                .fetch_all(&self.pool)
                .await?;
        rows.iter().map(mapping::subscription).collect()
    }

    /// The organization's webhook endpoint (first configured), if any.
    pub async fn webhook_endpoint(&self, organization_id: Uuid) -> Result<Option<String>> {
        let row = sqlx::query(
            "SELECT webhook_url FROM webhook_endpoints WHERE organization_id = $1 ORDER BY created_at ASC LIMIT 1",
        )
        .bind(organization_id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|r| r.get::<String, _>("webhook_url")))
    }

    pub async fn insert_webhook_endpoint(&self, organization_id: Uuid, url: &str) -> Result<()> {
        sqlx::query(
            "INSERT INTO webhook_endpoints (id, organization_id, webhook_url, signature_algo, created_at, updated_at) \
             VALUES (gen_random_uuid(), $1, $2, 'hmac', now(), now())",
        )
        .bind(organization_id)
        .bind(url)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// The organization's webhook HMAC signing key.
    pub async fn organization_hmac_key(&self, organization_id: Uuid) -> Result<Option<String>> {
        let row = sqlx::query("SELECT hmac_key FROM organizations WHERE id = $1")
            .bind(organization_id)
            .fetch_optional(&self.pool)
            .await?;
        Ok(row.and_then(|r| r.get::<Option<String>, _>("hmac_key")))
    }

    /// Insert/update a webhook delivery record (primitive fields, so this crate
    /// stays independent of rago-integrations).
    #[allow(clippy::too_many_arguments)]
    pub async fn upsert_webhook(
        &self,
        id: Uuid,
        organization_id: Uuid,
        webhook_type: &str,
        object_type: &str,
        endpoint: &str,
        payload: &serde_json::Value,
        status: &str,
        http_status: Option<i32>,
        retries: i32,
    ) -> Result<()> {
        sqlx::query(
            "INSERT INTO webhooks (id, organization_id, webhook_type, object_type, endpoint, \
             payload, status, http_status, retries, created_at, updated_at) \
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9, now(), now()) \
             ON CONFLICT (id) DO UPDATE SET status = EXCLUDED.status, \
             http_status = EXCLUDED.http_status, retries = EXCLUDED.retries, updated_at = now()",
        )
        .bind(id)
        .bind(organization_id)
        .bind(webhook_type)
        .bind(object_type)
        .bind(endpoint)
        .bind(payload)
        .bind(status)
        .bind(http_status)
        .bind(retries)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Recent webhooks for an organization (verification/inspection).
    pub async fn recent_webhooks(
        &self,
        organization_id: Uuid,
        limit: i64,
    ) -> Result<Vec<(String, String, Option<i32>)>> {
        let rows = sqlx::query(
            "SELECT webhook_type, status, http_status FROM webhooks WHERE organization_id = $1 \
             ORDER BY created_at DESC LIMIT $2",
        )
        .bind(organization_id)
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .iter()
            .map(|r| {
                (
                    r.get::<String, _>("webhook_type"),
                    r.get::<String, _>("status"),
                    r.get::<Option<i32>, _>("http_status"),
                )
            })
            .collect())
    }

    /// Record a payment.
    #[allow(clippy::too_many_arguments)]
    pub async fn insert_payment(
        &self,
        id: Uuid,
        organization_id: Uuid,
        invoice_id: Uuid,
        provider: &str,
        provider_payment_id: Option<&str>,
        provider_status: Option<&str>,
        status: &str,
        amount_cents: i64,
        amount_currency: &str,
    ) -> Result<()> {
        // `id` is derived deterministically from the invoice (see
        // `rago_jobs::payments::deterministic_payment_id`), so a scheduler/job
        // re-run for the same invoice reuses the same primary key: at most one
        // payments row per logical payment. `ON CONFLICT (id) DO UPDATE` keeps
        // the write idempotent (replaying an identical outcome is a no-op)
        // while letting a legitimate dunning retry of a FAILED charge record
        // its new provider payment id + outcome — with `DO NOTHING` the retry's
        // PaymentIntent id was never stored, so incoming provider webhooks
        // could not match the payment. Re-charging a non-failed payment is
        // prevented upstream by the guard in `rago_jobs::payments::charge_invoice`.
        sqlx::query(
            "INSERT INTO payments (id, organization_id, invoice_id, provider, provider_payment_id, \
             provider_status, status, amount_cents, amount_currency, created_at, updated_at) \
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9, now(), now()) \
             ON CONFLICT (id) DO UPDATE SET \
                 provider_payment_id = EXCLUDED.provider_payment_id, \
                 provider_status = EXCLUDED.provider_status, \
                 status = EXCLUDED.status, \
                 updated_at = now()",
        )
        .bind(id).bind(organization_id).bind(invoice_id).bind(provider)
        .bind(provider_payment_id).bind(provider_status).bind(status)
        .bind(amount_cents).bind(amount_currency)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Set an invoice's `payment_status` (from a payment outcome).
    pub async fn update_invoice_payment_status(
        &self,
        invoice_id: Uuid,
        payment_status: i32,
    ) -> Result<()> {
        sqlx::query("UPDATE invoices SET payment_status = $2, updated_at = now() WHERE id = $1")
            .bind(invoice_id)
            .bind(payment_status)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Void an invoice (`Invoices::Void` — status → voided).
    pub async fn void_invoice(&self, invoice_id: Uuid) -> Result<()> {
        sqlx::query("UPDATE invoices SET status = 2, updated_at = now() WHERE id = $1")
            .bind(invoice_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Update a fee's tax columns (after applying provider taxes).
    pub async fn update_fee_taxes(
        &self,
        fee_id: Uuid,
        taxes_amount_cents: i64,
        taxes_rate: f64,
        taxes_precise_amount_cents: rust_decimal::Decimal,
    ) -> Result<()> {
        sqlx::query(
            "UPDATE fees SET taxes_amount_cents = $2, taxes_rate = $3, taxes_precise_amount_cents = $4, updated_at = now() WHERE id = $1",
        )
        .bind(fee_id)
        .bind(taxes_amount_cents)
        .bind(taxes_rate)
        .bind(taxes_precise_amount_cents)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Update an invoice's tax/total columns.
    pub async fn update_invoice_totals(
        &self,
        invoice_id: Uuid,
        taxes_amount_cents: i64,
        sub_total_including_taxes_amount_cents: i64,
        total_amount_cents: i64,
    ) -> Result<()> {
        sqlx::query(
            "UPDATE invoices SET taxes_amount_cents = $2, sub_total_including_taxes_amount_cents = $3, \
             total_amount_cents = $4, updated_at = now() WHERE id = $1",
        )
        .bind(invoice_id)
        .bind(taxes_amount_cents)
        .bind(sub_total_including_taxes_amount_cents)
        .bind(total_amount_cents)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    // ---- durable job queue -----------------------------------------------

    /// Enqueue a job to run at or after `run_at`.
    pub async fn enqueue_job(
        &self,
        job_type: &str,
        payload: &serde_json::Value,
        run_at: NaiveDateTime,
        max_attempts: i32,
    ) -> Result<Uuid> {
        let id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO jobs (id, queue, job_type, payload, status, attempts, max_attempts, run_at, created_at, updated_at) \
             VALUES ($1, 'default', $2, $3, 'pending', 0, $4, $5, now(), now())",
        )
        .bind(id)
        .bind(job_type)
        .bind(payload)
        .bind(max_attempts)
        .bind(run_at)
        .execute(&self.pool)
        .await?;
        Ok(id)
    }

    /// Lease one due job atomically (`FOR UPDATE SKIP LOCKED`).
    pub async fn lease_job(&self, worker: &str) -> Result<Option<LeasedJob>> {
        let row = sqlx::query(
            "UPDATE jobs SET status = 'leased', leased_at = now(), leased_by = $1, updated_at = now() \
             WHERE id = ( \
               SELECT id FROM jobs WHERE status = 'pending' AND run_at <= now() \
               ORDER BY run_at ASC FOR UPDATE SKIP LOCKED LIMIT 1 \
             ) RETURNING id, job_type, payload, attempts, max_attempts",
        )
        .bind(worker)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|r| LeasedJob {
            id: r.get("id"),
            job_type: r.get("job_type"),
            payload: r.get("payload"),
            attempts: r.get("attempts"),
            max_attempts: r.get("max_attempts"),
        }))
    }

    /// Ack a job as succeeded.
    pub async fn complete_job(&self, id: Uuid) -> Result<()> {
        sqlx::query("UPDATE jobs SET status = 'succeeded', updated_at = now() WHERE id = $1")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Fail a job: retry with backoff (future `run_at`) or dead-letter when
    /// attempts are exhausted.
    pub async fn fail_job(
        &self,
        id: Uuid,
        error: &str,
        retry_in_secs: Option<i64>,
    ) -> Result<bool> {
        if let Some(secs) = retry_in_secs {
            sqlx::query(
                "UPDATE jobs SET status = 'pending', attempts = attempts + 1, last_error = $2, \
                 run_at = now() + ($3 || ' seconds')::interval, leased_by = NULL, updated_at = now() \
                 WHERE id = $1",
            )
            .bind(id)
            .bind(error)
            .bind(secs.to_string())
            .execute(&self.pool)
            .await?;
            Ok(true)
        } else {
            sqlx::query(
                "UPDATE jobs SET status = 'dead', attempts = attempts + 1, last_error = $2, updated_at = now() WHERE id = $1",
            )
            .bind(id)
            .bind(error)
            .execute(&self.pool)
            .await?;
            Ok(false)
        }
    }

    /// Job status (for tests/inspection).
    pub async fn job_status(&self, id: Uuid) -> Result<Option<(String, i32)>> {
        let row = sqlx::query("SELECT status, attempts FROM jobs WHERE id = $1")
            .bind(id)
            .fetch_optional(&self.pool)
            .await?;
        Ok(row.map(|r| (r.get("status"), r.get("attempts"))))
    }

    // ---- Clock crons -----------------------------------------------------

    /// `Clock::ActivateSubscriptionsJob` — pending subs whose start has passed.
    pub async fn activate_due_subscriptions(&self, now: NaiveDateTime) -> Result<u64> {
        let r = sqlx::query(
            "UPDATE subscriptions SET status = 1, updated_at = now() \
             WHERE status = 0 AND subscription_at IS NOT NULL AND subscription_at <= $1",
        )
        .bind(now)
        .execute(&self.pool)
        .await?;
        Ok(r.rows_affected())
    }

    /// `Clock::TerminateEndedSubscriptionsJob` — active subs past `ending_at`.
    pub async fn terminate_ended_subscriptions(&self, now: NaiveDateTime) -> Result<u64> {
        let r = sqlx::query(
            "UPDATE subscriptions SET status = 2, terminated_at = now(), updated_at = now() \
             WHERE status = 1 AND ending_at IS NOT NULL AND ending_at <= $1",
        )
        .bind(now)
        .execute(&self.pool)
        .await?;
        Ok(r.rows_affected())
    }

    /// `Clock::ExpireIncompleteSubscriptionsJob` — incomplete subs older than a
    /// cutoff → canceled.
    pub async fn expire_incomplete_subscriptions(&self, cutoff: NaiveDateTime) -> Result<u64> {
        let r = sqlx::query(
            "UPDATE subscriptions SET status = 3, canceled_at = now(), updated_at = now() \
             WHERE status = 4 AND created_at <= $1",
        )
        .bind(cutoff)
        .execute(&self.pool)
        .await?;
        Ok(r.rows_affected())
    }

    /// `Clock::FinalizeInvoicesJob` — every draft of the org → finalized, each
    /// numbered in its own transaction. Returns the count finalized.
    pub async fn finalize_draft_invoices(&self, organization_id: Uuid) -> Result<u64> {
        Ok(self.finalize_all_drafts(organization_id).await?.len() as u64)
    }

    /// `Clock::MarkInvoicesAsPaymentOverdueJob` — finalized, not-succeeded
    /// invoices past their due date (or, absent a due date, older than the
    /// cutoff) are flagged overdue for dunning.
    pub async fn mark_payment_overdue(&self, cutoff: NaiveDateTime) -> Result<u64> {
        let r = sqlx::query(
            "UPDATE invoices SET payment_overdue = true, updated_at = now() \
             WHERE status = 1 AND payment_status <> 1 AND payment_overdue = false \
             AND ((payment_due_date IS NOT NULL AND payment_due_date <= $1::date) \
                  OR (payment_due_date IS NULL AND created_at <= $1))",
        )
        .bind(cutoff)
        .execute(&self.pool)
        .await?;
        Ok(r.rows_affected())
    }

    /// Count overdue invoices (for dunning campaigns / verification).
    pub async fn count_overdue_invoices(&self, organization_id: Uuid) -> Result<i64> {
        let row = sqlx::query(
            "SELECT COUNT(*) AS n FROM invoices WHERE organization_id = $1 AND payment_overdue = true",
        )
        .bind(organization_id)
        .fetch_one(&self.pool)
        .await?;
        Ok(row.try_get("n")?)
    }

    /// Free-trial subs whose trial has ended and are still pending → active.
    pub async fn end_free_trials(&self, now: NaiveDateTime) -> Result<u64> {
        let r = sqlx::query(
            "UPDATE subscriptions SET status = 1, updated_at = now() \
             WHERE status = 0 AND trial_ended_at IS NOT NULL AND trial_ended_at <= $1",
        )
        .bind(now)
        .execute(&self.pool)
        .await?;
        Ok(r.rows_affected())
    }

    // ---- webhook re-delivery ---------------------------------------------

    /// Webhook work items due for delivery: freshly materialized `pending`
    /// rows and `retrying` rows whose backoff has elapsed.
    pub async fn due_retrying_webhooks(&self, now: NaiveDateTime) -> Result<Vec<RetryingWebhook>> {
        let rows = sqlx::query(
            "SELECT id, organization_id, webhook_type, object_type, endpoint, payload, retries \
             FROM webhooks WHERE status IN ('pending', 'retrying') \
               AND (next_retry_at IS NULL OR next_retry_at <= $1) \
             ORDER BY created_at ASC LIMIT 200",
        )
        .bind(now)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .iter()
            .map(|r| RetryingWebhook {
                id: r.get("id"),
                organization_id: r.get("organization_id"),
                webhook_type: r.get("webhook_type"),
                object_type: r.get("object_type"),
                endpoint: r.get("endpoint"),
                payload: r.get("payload"),
                retries: r.get("retries"),
            })
            .collect())
    }

    /// Update a webhook after a re-delivery attempt.
    #[allow(clippy::too_many_arguments)]
    pub async fn update_webhook_retry(
        &self,
        id: Uuid,
        status: &str,
        http_status: Option<i32>,
        retries: i32,
        next_retry_in_secs: Option<i64>,
    ) -> Result<()> {
        sqlx::query(
            "UPDATE webhooks SET status = $2, http_status = $3, retries = $4, \
             next_retry_at = CASE WHEN $5::bigint IS NULL THEN NULL ELSE now() + ($5 || ' seconds')::interval END, \
             last_retried_at = now(), updated_at = now() WHERE id = $1",
        )
        .bind(id)
        .bind(status)
        .bind(http_status)
        .bind(retries)
        .bind(next_retry_in_secs)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Seed a `retrying` webhook (used to drive the re-delivery worker).
    pub async fn insert_retrying_webhook(
        &self,
        organization_id: Uuid,
        webhook_type: &str,
        endpoint: &str,
        payload: &serde_json::Value,
    ) -> Result<Uuid> {
        let id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO webhooks (id, organization_id, webhook_type, object_type, endpoint, payload, \
             status, retries, next_retry_at, created_at, updated_at) \
             VALUES ($1, $2, $3, 'invoice', $4, $5, 'retrying', 1, now(), now(), now())",
        )
        .bind(id)
        .bind(organization_id)
        .bind(webhook_type)
        .bind(endpoint)
        .bind(payload)
        .execute(&self.pool)
        .await?;
        Ok(id)
    }

    // ---- analytics (real SQL aggregations) -------------------------------

    /// `Analytics::GrossRevenues` — finalized-invoice revenue by month+currency.
    pub async fn gross_revenues(&self, org: Uuid) -> Result<Vec<AnalyticsPoint>> {
        let rows = sqlx::query(
            "SELECT to_char(COALESCE(issuing_date, created_at::date), 'YYYY-MM-01') AS month, \
             COALESCE(currency, 'USD') AS currency, SUM(total_amount_cents)::bigint AS amount, COUNT(*)::bigint AS n \
             FROM invoices WHERE organization_id = $1 AND status = 1 \
             GROUP BY month, currency ORDER BY month",
        )
        .bind(org)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.iter().map(analytics_point).collect())
    }

    /// `Analytics::Mrr` — monthly recurring revenue = subscription-fee amounts
    /// by month+currency.
    pub async fn mrrs(&self, org: Uuid) -> Result<Vec<AnalyticsPoint>> {
        let rows = sqlx::query(
            "SELECT to_char(COALESCE(i.issuing_date, i.created_at::date), 'YYYY-MM-01') AS month, \
             COALESCE(i.currency, 'USD') AS currency, SUM(f.amount_cents)::bigint AS amount, COUNT(*)::bigint AS n \
             FROM fees f JOIN invoices i ON f.invoice_id = i.id \
             WHERE i.organization_id = $1 AND i.status = 1 AND f.fee_type = 2 \
             GROUP BY month, currency ORDER BY month",
        )
        .bind(org)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.iter().map(analytics_point).collect())
    }

    /// `Analytics::InvoiceCollections` — invoiced amount by month+payment status.
    pub async fn invoice_collections(&self, org: Uuid) -> Result<Vec<InvoiceCollectionPoint>> {
        let rows = sqlx::query(
            "SELECT to_char(COALESCE(issuing_date, created_at::date), 'YYYY-MM-01') AS month, \
             COALESCE(currency, 'USD') AS currency, payment_status, \
             COUNT(*)::bigint AS n, SUM(total_amount_cents)::bigint AS amount \
             FROM invoices WHERE organization_id = $1 AND status = 1 \
             GROUP BY month, currency, payment_status ORDER BY month",
        )
        .bind(org)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .iter()
            .map(|r| InvoiceCollectionPoint {
                month: r.get("month"),
                currency: r.get("currency"),
                payment_status: r.get::<i32, _>("payment_status"),
                invoices_count: r.get("n"),
                amount_cents: r.get("amount"),
            })
            .collect())
    }

    /// `Analytics::OverdueBalances` — finalized, not-succeeded invoice amount by
    /// currency (an outstanding-balance proxy).
    pub async fn overdue_balances(&self, org: Uuid) -> Result<Vec<AnalyticsPoint>> {
        let rows = sqlx::query(
            "SELECT to_char(COALESCE(issuing_date, created_at::date), 'YYYY-MM-01') AS month, \
             COALESCE(currency, 'USD') AS currency, SUM(total_amount_cents)::bigint AS amount, COUNT(*)::bigint AS n \
             FROM invoices WHERE organization_id = $1 AND status = 1 AND payment_status <> 1 \
             GROUP BY month, currency ORDER BY month",
        )
        .bind(org)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.iter().map(analytics_point).collect())
    }

    // ---- add-ons / quotes / data exports ---------------------------------

    #[allow(clippy::too_many_arguments)]
    pub async fn insert_add_on(
        &self,
        id: Uuid,
        org: Uuid,
        name: &str,
        code: &str,
        description: Option<&str>,
        amount_cents: i64,
        currency: &str,
    ) -> Result<()> {
        sqlx::query("INSERT INTO add_ons (id, organization_id, name, code, description, amount_cents, amount_currency, created_at, updated_at) VALUES ($1,$2,$3,$4,$5,$6,$7,now(),now())")
            .bind(id).bind(org).bind(name).bind(code).bind(description).bind(amount_cents).bind(currency)
            .execute(&self.pool).await?;
        Ok(())
    }

    /// A page of add-ons `(id, name, code, amount_cents, currency)` plus total count.
    pub async fn list_add_ons(
        &self,
        org: Uuid,
        limit: i64,
        offset: i64,
    ) -> Result<(Vec<(Uuid, String, String, i64, String)>, i64)> {
        let rows = sqlx::query("SELECT id, name, code, amount_cents, amount_currency FROM add_ons WHERE organization_id = $1 AND deleted_at IS NULL ORDER BY created_at DESC LIMIT $2 OFFSET $3")
            .bind(org).bind(limit).bind(offset).fetch_all(&self.pool).await?;
        let items = rows
            .iter()
            .map(|r| {
                (
                    r.get::<Uuid, _>("id"),
                    r.get::<String, _>("name"),
                    r.get::<String, _>("code"),
                    r.get::<i64, _>("amount_cents"),
                    r.get::<String, _>("amount_currency"),
                )
            })
            .collect();
        let total: i64 = sqlx::query(
            "SELECT COUNT(*) AS n FROM add_ons WHERE organization_id = $1 AND deleted_at IS NULL",
        )
        .bind(org)
        .fetch_one(&self.pool)
        .await?
        .get("n");
        Ok((items, total))
    }

    /// `(id, name, code, amount_cents, currency)` for an add-on by code.
    pub async fn find_add_on_by_code(
        &self,
        org: Uuid,
        code: &str,
    ) -> Result<Option<(Uuid, String, String, i64, String)>> {
        let row = sqlx::query("SELECT id, name, code, amount_cents, amount_currency FROM add_ons WHERE organization_id = $1 AND code = $2 AND deleted_at IS NULL")
            .bind(org).bind(code).fetch_optional(&self.pool).await?;
        Ok(row.map(|r| {
            (
                r.get::<Uuid, _>("id"),
                r.get::<String, _>("name"),
                r.get::<String, _>("code"),
                r.get::<i64, _>("amount_cents"),
                r.get::<String, _>("amount_currency"),
            )
        }))
    }

    #[allow(clippy::too_many_arguments)]
    #[allow(clippy::too_many_arguments)]
    pub async fn insert_quote(
        &self,
        id: Uuid,
        org: Uuid,
        customer_id: Uuid,
        number: &str,
        currency: &str,
        sub_total: i64,
        taxes: i64,
        total: i64,
        line_items: &serde_json::Value,
    ) -> Result<()> {
        sqlx::query("INSERT INTO quotes (id, organization_id, customer_id, number, status, currency, sub_total_amount_cents, taxes_amount_cents, total_amount_cents, line_items, created_at, updated_at) VALUES ($1,$2,$3,$4,'draft',$5,$6,$7,$8,$9,now(),now())")
            .bind(id).bind(org).bind(customer_id).bind(number).bind(currency).bind(sub_total).bind(taxes).bind(total).bind(line_items)
            .execute(&self.pool).await?;
        Ok(())
    }

    /// `(id, customer_id, status, currency, total, line_items)` for a quote.
    pub async fn find_quote(
        &self,
        id: Uuid,
    ) -> Result<Option<(Uuid, Uuid, String, String, i64, serde_json::Value)>> {
        let row = sqlx::query("SELECT id, customer_id, status, currency, total_amount_cents, line_items FROM quotes WHERE id = $1")
            .bind(id).fetch_optional(&self.pool).await?;
        Ok(row.map(|r| {
            (
                r.get::<Uuid, _>("id"),
                r.get::<Uuid, _>("customer_id"),
                r.get::<String, _>("status"),
                r.get::<String, _>("currency"),
                r.get::<i64, _>("total_amount_cents"),
                r.get::<serde_json::Value, _>("line_items"),
            )
        }))
    }

    pub async fn accept_quote(&self, id: Uuid, invoice_id: Uuid) -> Result<()> {
        sqlx::query("UPDATE quotes SET status = 'accepted', accepted_invoice_id = $2, updated_at = now() WHERE id = $1")
            .bind(id).bind(invoice_id).execute(&self.pool).await?;
        Ok(())
    }

    pub async fn insert_data_export(&self, id: Uuid, org: Uuid, resource_type: &str) -> Result<()> {
        sqlx::query("INSERT INTO data_exports (id, organization_id, resource_type, format, status, created_at, updated_at) VALUES ($1,$2,$3,'csv','pending',now(),now())")
            .bind(id).bind(org).bind(resource_type).execute(&self.pool).await?;
        Ok(())
    }

    pub async fn complete_data_export(
        &self,
        id: Uuid,
        content: &str,
        row_count: i32,
    ) -> Result<()> {
        sqlx::query("UPDATE data_exports SET status = 'completed', content = $2, row_count = $3, updated_at = now() WHERE id = $1")
            .bind(id).bind(content).bind(row_count).execute(&self.pool).await?;
        Ok(())
    }

    /// `(resource_type, status, content, row_count)` for a data export.
    pub async fn find_data_export(
        &self,
        id: Uuid,
    ) -> Result<Option<(String, String, Option<String>, i32)>> {
        let row = sqlx::query(
            "SELECT resource_type, status, content, row_count FROM data_exports WHERE id = $1",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|r| {
            (
                r.get::<String, _>("resource_type"),
                r.get::<String, _>("status"),
                r.get::<Option<String>, _>("content"),
                r.get::<i32, _>("row_count"),
            )
        }))
    }

    /// All invoices for an org (data export source).
    pub async fn all_invoices(&self, org: Uuid, limit: i64) -> Result<Vec<Invoice>> {
        let rows = sqlx::query(
            "SELECT * FROM invoices WHERE organization_id = $1 ORDER BY created_at DESC LIMIT $2",
        )
        .bind(org)
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(mapping::invoice).collect()
    }

    /// All events for an org (data export source).
    pub async fn all_events(&self, org: Uuid, limit: i64) -> Result<Vec<Event>> {
        let rows = sqlx::query("SELECT * FROM events WHERE organization_id = $1 AND deleted_at IS NULL ORDER BY created_at DESC LIMIT $2")
            .bind(org).bind(limit).fetch_all(&self.pool).await?;
        rows.iter().map(mapping::event).collect()
    }

    /// Insert a wallet recurring/threshold top-up rule.
    #[allow(clippy::too_many_arguments)]
    pub async fn insert_wallet_recurring_rule(
        &self,
        id: Uuid,
        org: Uuid,
        wallet_id: Uuid,
        rule_type: &str,
        interval: Option<&str>,
        threshold: Option<rust_decimal::Decimal>,
        paid: rust_decimal::Decimal,
        granted: rust_decimal::Decimal,
    ) -> Result<()> {
        sqlx::query("INSERT INTO wallet_recurring_rules (id, organization_id, wallet_id, rule_type, interval, threshold_credits, paid_credits, granted_credits, created_at, updated_at) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,now(),now())")
            .bind(id).bind(org).bind(wallet_id).bind(rule_type).bind(interval).bind(threshold).bind(paid).bind(granted)
            .execute(&self.pool).await?;
        Ok(())
    }

    // ---- index filters / search ------------------------------------------

    /// Subscriptions for an org, optionally filtered by `status` (int enum) and
    /// an `external_id` substring (`IndexSubscriptionsService`).
    pub async fn list_subscriptions(
        &self,
        org: Uuid,
        status: Option<i32>,
        external_id: Option<&str>,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<Subscription>> {
        let rows = sqlx::query(
            "SELECT * FROM subscriptions WHERE organization_id = $1 \
             AND ($2::int IS NULL OR status = $2) \
             AND ($3::text IS NULL OR external_id ILIKE '%' || $3 || '%') \
             ORDER BY created_at DESC LIMIT $4 OFFSET $5",
        )
        .bind(org)
        .bind(status)
        .bind(external_id)
        .bind(limit)
        .bind(offset)
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(mapping::subscription).collect()
    }

    pub async fn count_subscriptions(
        &self,
        org: Uuid,
        status: Option<i32>,
        external_id: Option<&str>,
    ) -> Result<i64> {
        let row = sqlx::query(
            "SELECT COUNT(*) AS n FROM subscriptions WHERE organization_id = $1 \
             AND ($2::int IS NULL OR status = $2) \
             AND ($3::text IS NULL OR external_id ILIKE '%' || $3 || '%')",
        )
        .bind(org)
        .bind(status)
        .bind(external_id)
        .fetch_one(&self.pool)
        .await?;
        Ok(row.try_get("n")?)
    }

    /// Invoices for an org, optionally filtered by `status` and `payment_status`
    /// (int enums) and an issuing-date range (`IndexInvoicesService`).
    #[allow(clippy::too_many_arguments)]
    pub async fn list_invoices(
        &self,
        org: Uuid,
        status: Option<i32>,
        payment_status: Option<i32>,
        from: Option<NaiveDate>,
        to: Option<NaiveDate>,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<Invoice>> {
        let rows = sqlx::query(
            "SELECT * FROM invoices WHERE organization_id = $1 \
             AND ($2::int IS NULL OR status = $2) \
             AND ($3::int IS NULL OR payment_status = $3) \
             AND ($4::date IS NULL OR issuing_date >= $4) \
             AND ($5::date IS NULL OR issuing_date <= $5) \
             ORDER BY created_at DESC LIMIT $6 OFFSET $7",
        )
        .bind(org)
        .bind(status)
        .bind(payment_status)
        .bind(from)
        .bind(to)
        .bind(limit)
        .bind(offset)
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(mapping::invoice).collect()
    }

    pub async fn count_invoices(
        &self,
        org: Uuid,
        status: Option<i32>,
        payment_status: Option<i32>,
        from: Option<NaiveDate>,
        to: Option<NaiveDate>,
    ) -> Result<i64> {
        let row = sqlx::query(
            "SELECT COUNT(*) AS n FROM invoices WHERE organization_id = $1 \
             AND ($2::int IS NULL OR status = $2) \
             AND ($3::int IS NULL OR payment_status = $3) \
             AND ($4::date IS NULL OR issuing_date >= $4) \
             AND ($5::date IS NULL OR issuing_date <= $5)",
        )
        .bind(org)
        .bind(status)
        .bind(payment_status)
        .bind(from)
        .bind(to)
        .fetch_one(&self.pool)
        .await?;
        Ok(row.try_get("n")?)
    }

    /// Events for an org, optionally filtered by `code` and `external_subscription_id`
    /// (`IndexEventsService`).
    pub async fn list_events(
        &self,
        org: Uuid,
        code: Option<&str>,
        external_subscription_id: Option<&str>,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<Event>> {
        let rows = sqlx::query(
            "SELECT * FROM events WHERE organization_id = $1 AND deleted_at IS NULL \
             AND ($2::text IS NULL OR code = $2) \
             AND ($3::text IS NULL OR external_subscription_id = $3) \
             ORDER BY created_at DESC LIMIT $4 OFFSET $5",
        )
        .bind(org)
        .bind(code)
        .bind(external_subscription_id)
        .bind(limit)
        .bind(offset)
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(mapping::event).collect()
    }

    pub async fn count_events(
        &self,
        org: Uuid,
        code: Option<&str>,
        external_subscription_id: Option<&str>,
    ) -> Result<i64> {
        let row = sqlx::query(
            "SELECT COUNT(*) AS n FROM events WHERE organization_id = $1 AND deleted_at IS NULL \
             AND ($2::text IS NULL OR code = $2) \
             AND ($3::text IS NULL OR external_subscription_id = $3)",
        )
        .bind(org)
        .bind(code)
        .bind(external_subscription_id)
        .fetch_one(&self.pool)
        .await?;
        Ok(row.try_get("n")?)
    }

    // ---- integration connections (OAuth lifecycle) + mappings ------------

    #[allow(clippy::too_many_arguments)]
    pub async fn insert_integration_connection(
        &self,
        id: Uuid,
        org: Uuid,
        integration_type: &str,
        provider_code: &str,
        name: Option<&str>,
        access_token: Option<&str>,
        refresh_token: Option<&str>,
        expires_at: Option<NaiveDateTime>,
        settings: &serde_json::Value,
    ) -> Result<()> {
        let access_token = access_token.map(seal_token);
        let refresh_token = refresh_token.map(seal_token);
        sqlx::query("INSERT INTO integration_connections (id, organization_id, integration_type, provider_code, name, access_token, refresh_token, token_expires_at, settings, created_at, updated_at) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,now(),now())")
            .bind(id).bind(org).bind(integration_type).bind(provider_code).bind(name)
            .bind(access_token).bind(refresh_token).bind(expires_at).bind(settings)
            .execute(&self.pool).await?;
        Ok(())
    }

    /// `(id, integration_type, provider_code, access_token, refresh_token, expires_at)`.
    pub async fn find_integration_connection(
        &self,
        id: Uuid,
    ) -> Result<
        Option<(
            Uuid,
            String,
            String,
            Option<String>,
            Option<String>,
            Option<NaiveDateTime>,
        )>,
    > {
        let row = sqlx::query("SELECT id, integration_type, provider_code, access_token, refresh_token, token_expires_at FROM integration_connections WHERE id = $1 AND deleted_at IS NULL")
            .bind(id).fetch_optional(&self.pool).await?;
        Ok(row.map(|r| {
            (
                r.get::<Uuid, _>("id"),
                r.get::<String, _>("integration_type"),
                r.get::<String, _>("provider_code"),
                r.get::<Option<String>, _>("access_token").map(|t| open_token(&t)),
                r.get::<Option<String>, _>("refresh_token").map(|t| open_token(&t)),
                r.get::<Option<NaiveDateTime>, _>("token_expires_at"),
            )
        }))
    }

    /// Latest connection for an org + provider code.
    pub async fn find_connection_by_provider(
        &self,
        org: Uuid,
        provider_code: &str,
    ) -> Result<Option<(Uuid, Option<String>)>> {
        let row = sqlx::query("SELECT id, access_token FROM integration_connections WHERE organization_id = $1 AND provider_code = $2 AND deleted_at IS NULL ORDER BY (access_token IS NOT NULL) DESC, created_at DESC LIMIT 1")
            .bind(org).bind(provider_code).fetch_optional(&self.pool).await?;
        Ok(row.map(|r| {
            (
                r.get::<Uuid, _>("id"),
                r.get::<Option<String>, _>("access_token").map(|t| open_token(&t)),
            )
        }))
    }

    pub async fn update_integration_tokens(
        &self,
        id: Uuid,
        access_token: &str,
        refresh_token: Option<&str>,
        expires_at: Option<NaiveDateTime>,
    ) -> Result<()> {
        let access_token = seal_token(access_token);
        let refresh_token = refresh_token.map(seal_token);
        sqlx::query("UPDATE integration_connections SET access_token = $2, refresh_token = COALESCE($3, refresh_token), token_expires_at = $4, updated_at = now() WHERE id = $1")
            .bind(id).bind(access_token).bind(refresh_token).bind(expires_at)
            .execute(&self.pool).await?;
        Ok(())
    }

    pub async fn insert_integration_mapping(
        &self,
        id: Uuid,
        org: Uuid,
        connection_id: Uuid,
        resource_type: &str,
        lago_id: Uuid,
        external_id: &str,
    ) -> Result<()> {
        sqlx::query("INSERT INTO integration_mappings (id, organization_id, integration_connection_id, resource_type, lago_id, external_id, created_at) VALUES ($1,$2,$3,$4,$5,$6,now())")
            .bind(id).bind(org).bind(connection_id).bind(resource_type).bind(lago_id).bind(external_id)
            .execute(&self.pool).await?;
        Ok(())
    }

    pub async fn find_integration_mapping(
        &self,
        org: Uuid,
        resource_type: &str,
        lago_id: Uuid,
    ) -> Result<Option<String>> {
        let row = sqlx::query("SELECT external_id FROM integration_mappings WHERE organization_id = $1 AND resource_type = $2 AND lago_id = $3 ORDER BY created_at DESC LIMIT 1")
            .bind(org).bind(resource_type).bind(lago_id).fetch_optional(&self.pool).await?;
        Ok(row.map(|r| r.get::<String, _>("external_id")))
    }

    // ---- usage-monitoring alerts -----------------------------------------

    #[allow(clippy::too_many_arguments)]
    pub async fn insert_usage_alert(
        &self,
        id: Uuid,
        org: Uuid,
        sub_ext: &str,
        alert_type: &str,
        metric_code: Option<&str>,
        code: Option<&str>,
        threshold_cents: i64,
        recurring: bool,
    ) -> Result<()> {
        sqlx::query("INSERT INTO usage_alerts (id, organization_id, subscription_external_id, alert_type, billable_metric_code, code, threshold_cents, recurring, previous_value_cents, created_at, updated_at) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,0,now(),now())")
            .bind(id).bind(org).bind(sub_ext).bind(alert_type).bind(metric_code).bind(code).bind(threshold_cents).bind(recurring)
            .execute(&self.pool).await?;
        Ok(())
    }

    /// `(id, alert_type, billable_metric_code, threshold_cents, recurring, previous_value_cents, last_triggered_at)`.
    pub async fn list_usage_alerts(
        &self,
        org: Uuid,
        sub_ext: &str,
    ) -> Result<
        Vec<(
            Uuid,
            String,
            Option<String>,
            i64,
            bool,
            i64,
            Option<NaiveDateTime>,
        )>,
    > {
        let rows = sqlx::query("SELECT id, alert_type, billable_metric_code, threshold_cents, recurring, previous_value_cents, last_triggered_at FROM usage_alerts WHERE organization_id = $1 AND subscription_external_id = $2 ORDER BY created_at")
            .bind(org).bind(sub_ext).fetch_all(&self.pool).await?;
        Ok(rows
            .iter()
            .map(|r| {
                (
                    r.get::<Uuid, _>("id"),
                    r.get::<String, _>("alert_type"),
                    r.get::<Option<String>, _>("billable_metric_code"),
                    r.get::<i64, _>("threshold_cents"),
                    r.get::<bool, _>("recurring"),
                    r.get::<i64, _>("previous_value_cents"),
                    r.get::<Option<NaiveDateTime>, _>("last_triggered_at"),
                )
            })
            .collect())
    }

    pub async fn mark_alert_triggered(&self, id: Uuid, current_value_cents: i64) -> Result<()> {
        sqlx::query("UPDATE usage_alerts SET last_triggered_at = now(), previous_value_cents = $2, updated_at = now() WHERE id = $1")
            .bind(id).bind(current_value_cents).execute(&self.pool).await?;
        Ok(())
    }

    pub async fn insert_alert_trigger(
        &self,
        id: Uuid,
        org: Uuid,
        alert_id: Uuid,
        sub_ext: &str,
        crossed_threshold_cents: i64,
        current_value_cents: i64,
    ) -> Result<()> {
        sqlx::query("INSERT INTO usage_alert_triggers (id, organization_id, usage_alert_id, subscription_external_id, crossed_threshold_cents, current_value_cents, triggered_at) VALUES ($1,$2,$3,$4,$5,$6,now())")
            .bind(id).bind(org).bind(alert_id).bind(sub_ext).bind(crossed_threshold_cents).bind(current_value_cents)
            .execute(&self.pool).await?;
        Ok(())
    }

    /// `(usage_alert_id, crossed_threshold_cents, current_value_cents, triggered_at)`.
    pub async fn list_alert_triggers(
        &self,
        org: Uuid,
        sub_ext: &str,
    ) -> Result<Vec<(Uuid, i64, i64, NaiveDateTime)>> {
        let rows = sqlx::query("SELECT usage_alert_id, crossed_threshold_cents, current_value_cents, triggered_at FROM usage_alert_triggers WHERE organization_id = $1 AND subscription_external_id = $2 ORDER BY triggered_at DESC")
            .bind(org).bind(sub_ext).fetch_all(&self.pool).await?;
        Ok(rows
            .iter()
            .map(|r| {
                (
                    r.get::<Uuid, _>("usage_alert_id"),
                    r.get::<i64, _>("crossed_threshold_cents"),
                    r.get::<i64, _>("current_value_cents"),
                    r.get::<NaiveDateTime, _>("triggered_at"),
                )
            })
            .collect())
    }

    // ---- dashboard admin (org / api keys / webhook endpoints / integrations)

    /// Update mutable organization settings; returns the refreshed row.
    pub async fn update_organization(
        &self,
        id: Uuid,
        name: Option<&str>,
        default_currency: Option<&str>,
        email: Option<&str>,
        country: Option<&str>,
        timezone: Option<&str>,
    ) -> Result<Organization> {
        sqlx::query(
            "UPDATE organizations SET \
             name = COALESCE($2, name), \
             default_currency = COALESCE($3, default_currency), \
             email = COALESCE($4, email), \
             country = COALESCE($5, country), \
             timezone = COALESCE($6, timezone), \
             updated_at = now() WHERE id = $1",
        )
        .bind(id)
        .bind(name)
        .bind(default_currency)
        .bind(email)
        .bind(country)
        .bind(timezone)
        .execute(&self.pool)
        .await?;
        let row = sqlx::query("SELECT * FROM organizations WHERE id = $1")
            .bind(id)
            .fetch_one(&self.pool)
            .await?;
        mapping::organization(&row)
    }

    /// API keys for an org `(id, name, value, created_at)`.
    pub async fn list_api_keys(
        &self,
        org: Uuid,
    ) -> Result<Vec<(Uuid, Option<String>, String, NaiveDateTime)>> {
        let rows = sqlx::query("SELECT id, name, value, created_at FROM api_keys WHERE organization_id = $1 ORDER BY created_at DESC")
            .bind(org).fetch_all(&self.pool).await?;
        Ok(rows
            .iter()
            .map(|r| {
                (
                    r.get::<Uuid, _>("id"),
                    r.get::<Option<String>, _>("name"),
                    r.get::<String, _>("value"),
                    r.get::<NaiveDateTime, _>("created_at"),
                )
            })
            .collect())
    }

    /// Insert a webhook endpoint, returning its id.
    pub async fn insert_webhook_endpoint_returning(&self, org: Uuid, url: &str) -> Result<Uuid> {
        let id = Uuid::new_v4();
        sqlx::query("INSERT INTO webhook_endpoints (id, organization_id, webhook_url, signature_algo, created_at, updated_at) VALUES ($1,$2,$3,'hmac',now(),now())")
            .bind(id).bind(org).bind(url).execute(&self.pool).await?;
        Ok(id)
    }

    /// Webhook endpoints for an org `(id, webhook_url, signature_algo)`.
    pub async fn list_webhook_endpoints(&self, org: Uuid) -> Result<Vec<(Uuid, String, String)>> {
        let rows = sqlx::query("SELECT id, webhook_url, signature_algo FROM webhook_endpoints WHERE organization_id = $1 ORDER BY created_at")
            .bind(org).fetch_all(&self.pool).await?;
        Ok(rows
            .iter()
            .map(|r| {
                (
                    r.get::<Uuid, _>("id"),
                    r.get::<String, _>("webhook_url"),
                    r.get::<String, _>("signature_algo"),
                )
            })
            .collect())
    }

    /// Integration connections for an org `(id, integration_type, provider_code, name)`.
    pub async fn list_integration_connections(
        &self,
        org: Uuid,
    ) -> Result<Vec<(Uuid, String, String, Option<String>)>> {
        let rows = sqlx::query("SELECT id, integration_type, provider_code, name FROM integration_connections WHERE organization_id = $1 AND deleted_at IS NULL ORDER BY created_at DESC")
            .bind(org).fetch_all(&self.pool).await?;
        Ok(rows
            .iter()
            .map(|r| {
                (
                    r.get::<Uuid, _>("id"),
                    r.get::<String, _>("integration_type"),
                    r.get::<String, _>("provider_code"),
                    r.get::<Option<String>, _>("name"),
                )
            })
            .collect())
    }

    pub async fn find_coupon(&self, id: Uuid) -> Result<Option<Coupon>> {
        let row = sqlx::query("SELECT * FROM coupons WHERE id = $1")
            .bind(id)
            .fetch_optional(&self.pool)
            .await?;
        row.as_ref().map(mapping::coupon).transpose()
    }

    pub async fn terminate_coupon(&self, id: Uuid) -> Result<()> {
        sqlx::query("UPDATE coupons SET status = 1, updated_at = now() WHERE id = $1")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Terminate a wallet (status → terminated).
    pub async fn terminate_wallet(&self, id: Uuid) -> Result<()> {
        sqlx::query("UPDATE wallets SET status = 1, terminated_at = now(), updated_at = now() WHERE id = $1").bind(id).execute(&self.pool).await?;
        Ok(())
    }

    // ---- dunning campaigns -----------------------------------------------

    #[allow(clippy::too_many_arguments)]
    pub async fn insert_dunning_campaign(
        &self,
        id: Uuid,
        org: Uuid,
        name: &str,
        code: &str,
        description: Option<&str>,
        max_attempts: i32,
        days_between: i32,
        bcc: &serde_json::Value,
        applied_to_org: bool,
    ) -> Result<()> {
        sqlx::query("INSERT INTO dunning_campaigns (id, organization_id, name, code, description, max_attempts, days_between_attempts, bcc_emails, applied_to_organization, created_at, updated_at) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,now(),now())")
            .bind(id).bind(org).bind(name).bind(code).bind(description).bind(max_attempts).bind(days_between).bind(bcc).bind(applied_to_org)
            .execute(&self.pool).await?;
        Ok(())
    }

    /// Dunning campaigns `(id, name, code, max_attempts, days_between, applied_to_org)`.
    pub async fn list_dunning_campaigns(
        &self,
        org: Uuid,
    ) -> Result<Vec<(Uuid, String, String, i32, i32, bool)>> {
        let rows = sqlx::query("SELECT id, name, code, max_attempts, days_between_attempts, applied_to_organization FROM dunning_campaigns WHERE organization_id = $1 ORDER BY created_at")
            .bind(org).fetch_all(&self.pool).await?;
        Ok(rows
            .iter()
            .map(|r| {
                (
                    r.get::<Uuid, _>("id"),
                    r.get::<String, _>("name"),
                    r.get::<String, _>("code"),
                    r.get::<i32, _>("max_attempts"),
                    r.get::<i32, _>("days_between_attempts"),
                    r.get::<bool, _>("applied_to_organization"),
                )
            })
            .collect())
    }

    // ---- lifetime usage --------------------------------------------------

    /// Total invoiced usage (charge fees) for a subscription across finalized invoices.
    pub async fn subscription_invoiced_usage(&self, org: Uuid, sub_ext: &str) -> Result<i64> {
        let row = sqlx::query(
            "SELECT COALESCE(SUM(f.amount_cents), 0)::bigint AS n FROM fees f \
             JOIN subscriptions s ON s.id = f.subscription_id \
             WHERE s.organization_id = $1 AND s.external_id = $2 AND f.fee_type = 0",
        )
        .bind(org)
        .bind(sub_ext)
        .fetch_one(&self.pool)
        .await?;
        Ok(row.try_get::<i64, _>("n")?)
    }

    /// Upsert the subscription's lifetime-usage accumulator.
    pub async fn upsert_lifetime_usage(
        &self,
        org: Uuid,
        sub_ext: &str,
        invoiced: i64,
        current: i64,
    ) -> Result<()> {
        sqlx::query(
            "INSERT INTO lifetime_usages (id, organization_id, subscription_external_id, invoiced_usage_amount_cents, current_usage_amount_cents, updated_at) \
             VALUES (gen_random_uuid(), $1, $2, $3, $4, now()) \
             ON CONFLICT (organization_id, subscription_external_id) DO UPDATE SET \
             invoiced_usage_amount_cents = EXCLUDED.invoiced_usage_amount_cents, \
             current_usage_amount_cents = EXCLUDED.current_usage_amount_cents, updated_at = now()")
            .bind(org).bind(sub_ext).bind(invoiced).bind(current).execute(&self.pool).await?;
        Ok(())
    }

    // ---- entitlements ----------------------------------------------------

    pub async fn insert_feature(
        &self,
        id: Uuid,
        org: Uuid,
        code: &str,
        name: Option<&str>,
        description: Option<&str>,
    ) -> Result<()> {
        sqlx::query("INSERT INTO entitlement_features (id, organization_id, code, name, description, created_at, updated_at) VALUES ($1,$2,$3,$4,$5,now(),now())")
            .bind(id).bind(org).bind(code).bind(name).bind(description)
            .execute(&self.pool).await?;
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn insert_privilege(
        &self,
        id: Uuid,
        org: Uuid,
        feature_id: Uuid,
        code: &str,
        name: Option<&str>,
        value_type: &str,
        config: &serde_json::Value,
    ) -> Result<()> {
        sqlx::query("INSERT INTO entitlement_privileges (id, organization_id, entitlement_feature_id, code, name, value_type, config, created_at, updated_at) VALUES ($1,$2,$3,$4,$5,$6,$7,now(),now())")
            .bind(id).bind(org).bind(feature_id).bind(code).bind(name).bind(value_type).bind(config)
            .execute(&self.pool).await?;
        Ok(())
    }

    pub async fn insert_entitlement(
        &self,
        id: Uuid,
        org: Uuid,
        feature_id: Uuid,
        plan_id: Uuid,
    ) -> Result<()> {
        sqlx::query("INSERT INTO entitlement_entitlements (id, organization_id, entitlement_feature_id, plan_id, created_at, updated_at) VALUES ($1,$2,$3,$4,now(),now())")
            .bind(id).bind(org).bind(feature_id).bind(plan_id)
            .execute(&self.pool).await?;
        Ok(())
    }

    pub async fn insert_entitlement_value(
        &self,
        id: Uuid,
        org: Uuid,
        privilege_id: Uuid,
        entitlement_id: Uuid,
        value: &str,
    ) -> Result<()> {
        sqlx::query("INSERT INTO entitlement_entitlement_values (id, organization_id, entitlement_privilege_id, entitlement_entitlement_id, value, created_at, updated_at) VALUES ($1,$2,$3,$4,$5,now(),now())")
            .bind(id).bind(org).bind(privilege_id).bind(entitlement_id).bind(value)
            .execute(&self.pool).await?;
        Ok(())
    }

    pub async fn find_feature_by_code(&self, org: Uuid, code: &str) -> Result<Option<Uuid>> {
        let row = sqlx::query("SELECT id FROM entitlement_features WHERE organization_id = $1 AND code = $2 AND deleted_at IS NULL")
            .bind(org).bind(code).fetch_optional(&self.pool).await?;
        Ok(row.map(|r| r.get::<Uuid, _>("id")))
    }

    /// Entitlements on a plan: `(feature_code, feature_name, privilege_code, value)`.
    pub async fn plan_entitlements(
        &self,
        plan_id: Uuid,
    ) -> Result<Vec<(String, Option<String>, Option<String>, Option<String>)>> {
        let rows = sqlx::query(
            "SELECT f.code AS feature_code, f.name AS feature_name, p.code AS privilege_code, v.value AS value \
             FROM entitlement_entitlements e \
             JOIN entitlement_features f ON f.id = e.entitlement_feature_id \
             LEFT JOIN entitlement_entitlement_values v ON v.entitlement_entitlement_id = e.id \
             LEFT JOIN entitlement_privileges p ON p.id = v.entitlement_privilege_id \
             WHERE e.plan_id = $1 AND e.deleted_at IS NULL ORDER BY f.code",
        )
        .bind(plan_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .iter()
            .map(|r| {
                (
                    r.get::<String, _>("feature_code"),
                    r.get::<Option<String>, _>("feature_name"),
                    r.get::<Option<String>, _>("privilege_code"),
                    r.get::<Option<String>, _>("value"),
                )
            })
            .collect())
    }

    /// Attach a feature entitlement to a subscription (override).
    pub async fn insert_subscription_entitlement(
        &self,
        id: Uuid,
        org: Uuid,
        feature_id: Uuid,
        subscription_id: Uuid,
    ) -> Result<()> {
        sqlx::query("INSERT INTO entitlement_entitlements (id, organization_id, entitlement_feature_id, subscription_id, created_at, updated_at) VALUES ($1,$2,$3,$4,now(),now())")
            .bind(id).bind(org).bind(feature_id).bind(subscription_id)
            .execute(&self.pool).await?;
        Ok(())
    }

    /// Entitlements on a subscription: `(feature_code, feature_name, privilege_code, value)`.
    pub async fn subscription_entitlements(
        &self,
        subscription_id: Uuid,
    ) -> Result<Vec<(String, Option<String>, Option<String>, Option<String>)>> {
        let rows = sqlx::query(
            "SELECT f.code AS feature_code, f.name AS feature_name, p.code AS privilege_code, v.value AS value \
             FROM entitlement_entitlements e \
             JOIN entitlement_features f ON f.id = e.entitlement_feature_id \
             LEFT JOIN entitlement_entitlement_values v ON v.entitlement_entitlement_id = e.id \
             LEFT JOIN entitlement_privileges p ON p.id = v.entitlement_privilege_id \
             WHERE e.subscription_id = $1 AND e.deleted_at IS NULL ORDER BY f.code",
        )
        .bind(subscription_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .iter()
            .map(|r| {
                (
                    r.get::<String, _>("feature_code"),
                    r.get::<Option<String>, _>("feature_name"),
                    r.get::<Option<String>, _>("privilege_code"),
                    r.get::<Option<String>, _>("value"),
                )
            })
            .collect())
    }

    /// The value-type of a feature's privilege (for validation).
    pub async fn privilege_value_type(
        &self,
        org: Uuid,
        feature_id: Uuid,
        privilege_code: &str,
    ) -> Result<Option<String>> {
        let row = sqlx::query("SELECT value_type FROM entitlement_privileges WHERE organization_id = $1 AND entitlement_feature_id = $2 AND code = $3")
            .bind(org).bind(feature_id).bind(privilege_code).fetch_optional(&self.pool).await?;
        Ok(row.map(|r| r.get::<String, _>("value_type")))
    }

    /// A feature privilege's id by code.
    pub async fn privilege_id(
        &self,
        org: Uuid,
        feature_id: Uuid,
        privilege_code: &str,
    ) -> Result<Option<Uuid>> {
        let row = sqlx::query("SELECT id FROM entitlement_privileges WHERE organization_id = $1 AND entitlement_feature_id = $2 AND code = $3")
            .bind(org).bind(feature_id).bind(privilege_code).fetch_optional(&self.pool).await?;
        Ok(row.map(|r| r.get::<Uuid, _>("id")))
    }

    // ---- audit / activity logs -------------------------------------------

    /// Write an activity-log entry (`ActivityLog` — Lago event stream).
    #[allow(clippy::too_many_arguments)]
    pub async fn insert_activity_log(
        &self,
        organization_id: Uuid,
        activity_type: &str,
        resource_type: &str,
        resource_id: Uuid,
        external_customer_id: Option<&str>,
        external_subscription_id: Option<&str>,
        payload: &serde_json::Value,
    ) -> Result<()> {
        sqlx::query(
            "INSERT INTO activity_logs (id, organization_id, activity_type, activity_source, resource_type, \
             resource_id, external_customer_id, external_subscription_id, payload, created_at) \
             VALUES (gen_random_uuid(), $1, $2, 'system', $3, $4, $5, $6, $7, now())",
        )
        .bind(organization_id).bind(activity_type).bind(resource_type).bind(resource_id)
        .bind(external_customer_id).bind(external_subscription_id).bind(payload)
        .execute(&self.pool).await?;
        Ok(())
    }

    /// Write an audit-log entry (resource change).
    pub async fn insert_audit_log(
        &self,
        organization_id: Uuid,
        resource_type: &str,
        resource_id: Uuid,
        action: &str,
        changes: &serde_json::Value,
    ) -> Result<()> {
        sqlx::query(
            "INSERT INTO audit_logs (id, organization_id, resource_type, resource_id, action, changes, created_at) \
             VALUES (gen_random_uuid(), $1, $2, $3, $4, $5, now())",
        )
        .bind(organization_id).bind(resource_type).bind(resource_id).bind(action).bind(changes)
        .execute(&self.pool).await?;
        Ok(())
    }

    /// Recent activity-log entries for an organization.
    pub async fn list_activity_logs(
        &self,
        organization_id: Uuid,
        limit: i64,
    ) -> Result<
        Vec<(
            String,
            String,
            Option<Uuid>,
            serde_json::Value,
            chrono::DateTime<chrono::Utc>,
        )>,
    > {
        let rows = sqlx::query(
            "SELECT activity_type, resource_type, resource_id, payload, created_at FROM activity_logs \
             WHERE organization_id = $1 ORDER BY created_at DESC LIMIT $2",
        )
        .bind(organization_id).bind(limit).fetch_all(&self.pool).await?;
        Ok(rows
            .iter()
            .map(|r| {
                (
                    r.get::<String, _>("activity_type"),
                    r.get::<Option<String>, _>("resource_type")
                        .unwrap_or_default(),
                    r.get::<Option<Uuid>, _>("resource_id"),
                    r.get::<serde_json::Value, _>("payload"),
                    r.get::<NaiveDateTime, _>("created_at").and_utc(),
                )
            })
            .collect())
    }

    pub async fn count_activity_logs(&self, organization_id: Uuid) -> Result<i64> {
        let row = sqlx::query("SELECT COUNT(*) AS n FROM activity_logs WHERE organization_id = $1")
            .bind(organization_id)
            .fetch_one(&self.pool)
            .await?;
        Ok(row.try_get("n")?)
    }

    // ---- refunds ---------------------------------------------------------

    /// Refund an invoice's payment: mark the payment refunded and set the
    /// invoice `payment_status` to refunded.
    pub async fn refund_invoice(&self, invoice_id: Uuid) -> Result<()> {
        sqlx::query(
            "UPDATE payments SET status = 'refunded', updated_at = now() WHERE invoice_id = $1",
        )
        .bind(invoice_id)
        .execute(&self.pool)
        .await?;
        sqlx::query("UPDATE invoices SET payment_status = 3, updated_at = now() WHERE id = $1")
            .bind(invoice_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    // ---- incoming webhooks / provider customers --------------------------

    /// Find a payment + its invoice id by provider payment id (Stripe `pi_…`).
    pub async fn find_payment_by_provider_id(
        &self,
        provider_payment_id: &str,
    ) -> Result<Option<(Uuid, Uuid, Uuid, String)>> {
        // returns (payment_id, invoice_id, organization_id, status)
        let row = sqlx::query(
            "SELECT id, invoice_id, organization_id, status FROM payments WHERE provider_payment_id = $1 LIMIT 1",
        )
        .bind(provider_payment_id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|r| {
            (
                r.get::<Uuid, _>("id"),
                r.get::<Uuid, _>("invoice_id"),
                r.get::<Uuid, _>("organization_id"),
                r.get::<String, _>("status"),
            )
        }))
    }

    /// Update a payment's status by provider payment id.
    pub async fn update_payment_status_by_provider_id(
        &self,
        provider_payment_id: &str,
        status: &str,
    ) -> Result<()> {
        sqlx::query(
            "UPDATE payments SET status = $2, updated_at = now() WHERE provider_payment_id = $1",
        )
        .bind(provider_payment_id)
        .bind(status)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Link (or update) a customer to a provider customer id.
    pub async fn upsert_provider_customer(
        &self,
        organization_id: Uuid,
        customer_id: Uuid,
        provider: &str,
        provider_customer_id: &str,
    ) -> Result<()> {
        // one row per (customer, provider)
        sqlx::query(
            "INSERT INTO payment_provider_customers (id, organization_id, customer_id, provider, provider_customer_id, created_at, updated_at) \
             SELECT gen_random_uuid(), $1, $2, $3, $4, now(), now() \
             WHERE NOT EXISTS (SELECT 1 FROM payment_provider_customers WHERE customer_id = $2 AND provider = $3)",
        )
        .bind(organization_id).bind(customer_id).bind(provider).bind(provider_customer_id)
        .execute(&self.pool).await?;
        sqlx::query(
            "UPDATE payment_provider_customers SET provider_customer_id = $3, updated_at = now() \
             WHERE customer_id = $1 AND provider = $2",
        )
        .bind(customer_id)
        .bind(provider)
        .bind(provider_customer_id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// The provider customer id for a Lago customer, if linked.
    pub async fn provider_customer_id(
        &self,
        customer_id: Uuid,
        provider: &str,
    ) -> Result<Option<String>> {
        let row = sqlx::query(
            "SELECT provider_customer_id FROM payment_provider_customers WHERE customer_id = $1 AND provider = $2 LIMIT 1",
        )
        .bind(customer_id)
        .bind(provider)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.and_then(|r| r.get::<Option<String>, _>("provider_customer_id")))
    }

    /// A customer's external id (for serialization).
    pub async fn customer_external_id(&self, customer_id: Uuid) -> Result<Option<String>> {
        let row = sqlx::query("SELECT external_id FROM customers WHERE id = $1")
            .bind(customer_id)
            .fetch_optional(&self.pool)
            .await?;
        Ok(row.map(|r| r.get::<String, _>("external_id")))
    }

    /// Whether an event already exists for `(organization, external_subscription,
    /// transaction)` — Lago's `Events::ValidateCreationService#valid_transaction_id?`
    /// dedup scope. This is only a fast-path validation hint; the authoritative
    /// guard is the unique index enforced in [`Self::insert_event`], so a race
    /// past this check still cannot double-insert.
    pub async fn event_exists(
        &self,
        organization_id: Uuid,
        external_subscription_id: &str,
        transaction_id: &str,
    ) -> Result<bool> {
        let row = sqlx::query(
            "SELECT 1 AS x FROM events \
             WHERE organization_id = $1 AND external_subscription_id = $2 AND transaction_id = $3 \
             LIMIT 1",
        )
        .bind(organization_id)
        .bind(external_subscription_id)
        .bind(transaction_id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.is_some())
    }

    pub async fn find_credit_note(&self, id: Uuid) -> Result<Option<CreditNote>> {
        let row = sqlx::query("SELECT * FROM credit_notes WHERE id = $1")
            .bind(id)
            .fetch_optional(&self.pool)
            .await?;
        match row {
            None => Ok(None),
            Some(r) => Ok(Some(CreditNote {
                id: r.try_get("id")?,
                organization_id: r.try_get("organization_id")?,
                customer_id: r.try_get("customer_id")?,
                invoice_id: r.try_get("invoice_id")?,
                sequential_id: r.try_get("sequential_id")?,
                number: r.try_get("number")?,
                issuing_date: r.try_get("issuing_date")?,
                reason: r.try_get("reason")?,
                credit_status: r.try_get("credit_status")?,
                refund_status: r.try_get("refund_status")?,
                status: r.try_get("status")?,
                credit_amount_cents: r.try_get("credit_amount_cents")?,
                credit_amount_currency: r.try_get("credit_amount_currency")?,
                balance_amount_cents: r.try_get("balance_amount_cents")?,
                refund_amount_cents: r.try_get("refund_amount_cents")?,
                total_amount_cents: r.try_get("total_amount_cents")?,
                total_amount_currency: r.try_get("total_amount_currency")?,
                taxes_amount_cents: r.try_get("taxes_amount_cents")?,
                taxes_rate: r.try_get("taxes_rate")?,
                coupons_adjustment_amount_cents: r.try_get("coupons_adjustment_amount_cents")?,
                precise_coupons_adjustment_amount_cents: r
                    .try_get("precise_coupons_adjustment_amount_cents")?,
                precise_taxes_amount_cents: r.try_get("precise_taxes_amount_cents")?,
                created_at: r.try_get::<NaiveDateTime, _>("created_at")?.and_utc(),
                updated_at: r.try_get::<NaiveDateTime, _>("updated_at")?.and_utc(),
                voided_at: r
                    .try_get::<Option<NaiveDateTime>, _>("voided_at")?
                    .map(|t| t.and_utc()),
                items: Vec::new(),
                applied_taxes: Vec::new(),
            })),
        }
    }
}

/// Prepaid wallet draw-down to persist atomically with a subscription invoice
/// ([`Db::insert_subscription_invoice_with_prepaid_guarded`]).
///
/// Borrows the artifacts produced by
/// `rago_billing::prepaid::apply_prepaid_credit`: the outbound
/// `transactions` (one per drawn-down wallet) and the `updated_wallets` with
/// their decreased balances. The two slices are parallel to a single
/// allocation, so persisting both — and nothing on a lost guard race —
/// preserves conservation.
pub struct PrepaidSettlement<'a> {
    pub transactions: &'a [WalletTransaction],
    pub updated_wallets: &'a [Wallet],
}

impl PrepaidSettlement<'_> {
    /// No prepaid credit applied (the plain guarded-insert path).
    pub const NONE: PrepaidSettlement<'static> = PrepaidSettlement {
        transactions: &[],
        updated_wallets: &[],
    };
}

/// Outcome of a guarded subscription-invoice insert
/// ([`Db::insert_subscription_invoice_guarded`]).
#[derive(Debug, Clone)]
pub enum BilledInvoice {
    /// A fresh invoice + fees were persisted for this `(subscription, period)`.
    Created,
    /// The period was already billed; carries the pre-existing invoice.
    AlreadyBilled(Invoice),
}

/// Insert an invoice against any executor (pool or transaction) — the single
/// source of the `invoices` column list, shared by [`Db::insert_invoice`] and
/// the guarded transactional path.
pub(crate) async fn insert_invoice_exec<'e, E>(exec: E, inv: &Invoice) -> Result<()>
where
    E: sqlx::Executor<'e, Database = sqlx::Postgres>,
{
    sqlx::query(
        "INSERT INTO invoices (\
            id, organization_id, billing_entity_id, customer_id, number, sequential_id, \
            invoice_type, status, payment_status, currency, issuing_date, \
            fees_amount_cents, coupons_amount_cents, credit_notes_amount_cents, \
            prepaid_credit_amount_cents, progressive_billing_credit_amount_cents, \
            sub_total_excluding_taxes_amount_cents, sub_total_including_taxes_amount_cents, \
            taxes_amount_cents, taxes_rate, total_amount_cents, version_number, \
            created_at, updated_at) \
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17,$18,$19,$20,$21,$22,$23,$24)",
    )
    .bind(inv.id)
    .bind(inv.organization_id)
    .bind(inv.billing_entity_id)
    .bind(inv.customer_id)
    .bind(&inv.number)
    .bind(inv.sequential_id)
    .bind(inv.invoice_type.as_i32())
    .bind(inv.status.as_i32())
    .bind(inv.payment_status.as_i32())
    .bind(inv.currency.as_deref())
    .bind(inv.issuing_date)
    .bind(inv.fees_amount_cents)
    .bind(inv.coupons_amount_cents)
    .bind(inv.credit_notes_amount_cents)
    .bind(inv.prepaid_credit_amount_cents)
    .bind(inv.progressive_billing_credit_amount_cents)
    .bind(inv.sub_total_excluding_taxes_amount_cents)
    .bind(inv.sub_total_including_taxes_amount_cents)
    .bind(inv.taxes_amount_cents)
    .bind(inv.taxes_rate)
    .bind(inv.total_amount_cents)
    .bind(inv.version_number)
    .bind(inv.created_at.naive_utc())
    .bind(inv.updated_at.naive_utc())
    .execute(exec)
    .await?;
    Ok(())
}

/// Insert a fee against any executor (pool or transaction) — shared by
/// [`Db::insert_fee`] and the guarded transactional path.
pub(crate) async fn insert_fee_exec<'e, E>(exec: E, fee: &Fee) -> Result<()>
where
    E: sqlx::Executor<'e, Database = sqlx::Postgres>,
{
    sqlx::query(
        "INSERT INTO fees (\
            id, organization_id, billing_entity_id, invoice_id, subscription_id, charge_id, \
            charge_filter_id, fee_type, invoiceable_type, invoiceable_id, amount_cents, \
            amount_currency, precise_amount_cents, unit_amount_cents, precise_unit_amount, \
            taxes_amount_cents, taxes_precise_amount_cents, taxes_rate, units, \
            total_aggregated_units, events_count, precise_coupons_amount_cents, \
            precise_credit_notes_amount_cents, payment_status, properties, amount_details, \
            grouped_by, created_at, updated_at) \
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17,$18,$19,$20,$21,$22,$23,$24,$25,$26,$27,$28,$29)",
    )
    .bind(fee.id)
    .bind(fee.organization_id)
    .bind(fee.billing_entity_id)
    .bind(fee.invoice_id)
    .bind(fee.subscription_id)
    .bind(fee.charge_id)
    .bind(fee.charge_filter_id)
    .bind(fee.fee_type.map(|t| t.as_i32()))
    .bind(fee.invoiceable_type.as_deref())
    .bind(fee.invoiceable_id)
    .bind(fee.amount_cents)
    .bind(&fee.amount_currency)
    .bind(fee.precise_amount_cents)
    .bind(fee.unit_amount_cents)
    .bind(fee.precise_unit_amount)
    .bind(fee.taxes_amount_cents)
    .bind(fee.taxes_precise_amount_cents)
    .bind(fee.taxes_rate)
    .bind(fee.units)
    .bind(fee.total_aggregated_units)
    .bind(fee.events_count)
    .bind(fee.precise_coupons_amount_cents)
    .bind(fee.precise_credit_notes_amount_cents)
    .bind(fee.payment_status.as_i32())
    .bind(&fee.properties)
    .bind(&fee.amount_details)
    .bind(&fee.grouped_by)
    .bind(fee.created_at.naive_utc())
    .bind(fee.updated_at.naive_utc())
    .execute(exec)
    .await?;
    Ok(())
}

/// Insert a wallet transaction against any executor (pool or transaction) —
/// shared by [`Db::insert_wallet_transaction`] and the guarded prepaid path so
/// the outbound draw-down is written in the same transaction as the invoice.
async fn insert_wallet_transaction_exec<'e, E>(exec: E, t: &WalletTransaction) -> Result<()>
where
    E: sqlx::Executor<'e, Database = sqlx::Postgres>,
{
    sqlx::query(
        "INSERT INTO wallet_transactions (id, organization_id, wallet_id, transaction_type, status, \
         amount, credit_amount, invoice_id, settled_at, created_at, updated_at) \
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11)",
    )
    .bind(t.id)
    .bind(t.organization_id)
    .bind(t.wallet_id)
    .bind(t.transaction_type.as_i32())
    .bind(t.status.as_i32())
    .bind(t.amount)
    .bind(t.credit_amount)
    .bind(t.invoice_id)
    .bind(t.settled_at.map(|x| x.naive_utc()))
    .bind(t.created_at.naive_utc())
    .bind(t.updated_at.naive_utc())
    .execute(exec)
    .await?;
    Ok(())
}

/// Update a wallet's balance columns against any executor (pool or transaction)
/// — shared by [`Db::update_wallet_balance`] and the guarded prepaid path.
async fn update_wallet_balance_exec<'e, E>(exec: E, w: &Wallet) -> Result<()>
where
    E: sqlx::Executor<'e, Database = sqlx::Postgres>,
{
    sqlx::query(
        "UPDATE wallets SET balance_cents = $2, credits_balance = $3, consumed_credits = $4, \
         consumed_amount_cents = $5, updated_at = $6 WHERE id = $1",
    )
    .bind(w.id)
    .bind(w.balance_cents)
    .bind(w.credits_balance)
    .bind(w.consumed_credits)
    .bind(w.consumed_amount_cents)
    .bind(w.updated_at.naive_utc())
    .execute(exec)
    .await?;
    Ok(())
}

/// Seal an OAuth token for `integration_connections` (AES-GCM under
/// `RAGO_ENCRYPTION_KEY`). The API refuses to connect integrations without the
/// key (see `rago_api::phase14::create_integration`), so the plaintext branch is
/// only reachable from internal callers on a key-less single-tenant setup and is
/// logged.
fn seal_token(token: &str) -> String {
    match crate::secrets::seal(token) {
        Ok(sealed) => sealed,
        Err(e) => {
            tracing::warn!(error = %e, "integration token stored unsealed (RAGO_ENCRYPTION_KEY missing)");
            token.to_string()
        }
    }
}

/// Open a stored OAuth token; legacy plaintext rows (no `v1:` prefix) pass
/// through unchanged.
fn open_token(stored: &str) -> String {
    if !stored.starts_with("v1:") {
        return stored.to_string();
    }
    match crate::secrets::open(stored) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(error = %e, "integration token could not be opened");
            String::new()
        }
    }
}
