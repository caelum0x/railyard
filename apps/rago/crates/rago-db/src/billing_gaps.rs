//! Persistence for customer billing addresses, scheduled plan downgrades and
//! the reversal of real-time wallet captures on void.

use chrono::{NaiveDateTime, Utc};
use rago_core::models::Subscription;
use sqlx::Row;
use uuid::Uuid;

use crate::error::Result;
use crate::mapping;
use crate::repo::Db;
use crate::wallet_realtime::RealtimeHold;

/// A customer's billing address and personal identity number.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CustomerAddress {
    pub address_line1: Option<String>,
    pub address_line2: Option<String>,
    pub city: Option<String>,
    pub state: Option<String>,
    pub zipcode: Option<String>,
    pub phone: Option<String>,
    pub identity_number: Option<String>,
}

impl CustomerAddress {
    /// Field names accepted by the API (Lago's customer address fields plus
    /// `identity_number`).
    pub const FIELDS: [&'static str; 7] =
        ["address_line1", "address_line2", "city", "state", "zipcode", "phone", "identity_number"];

    fn slot(&mut self, field: &str) -> Option<&mut Option<String>> {
        Some(match field {
            "address_line1" => &mut self.address_line1,
            "address_line2" => &mut self.address_line2,
            "city" => &mut self.city,
            "state" => &mut self.state,
            "zipcode" => &mut self.zipcode,
            "phone" => &mut self.phone,
            "identity_number" => &mut self.identity_number,
            _ => return None,
        })
    }

    /// A copy with `field` set (`None` or blank clears it).
    pub fn with(&self, field: &str, value: Option<&str>) -> Self {
        let mut next = self.clone();
        if let Some(slot) = next.slot(field) {
            *slot = value.map(str::trim).filter(|v| !v.is_empty()).map(str::to_string);
        }
        next
    }

    /// `(field, value)` pairs for serialization.
    pub fn pairs(&self) -> [(&'static str, Option<&str>); 7] {
        [
            ("address_line1", self.address_line1.as_deref()),
            ("address_line2", self.address_line2.as_deref()),
            ("city", self.city.as_deref()),
            ("state", self.state.as_deref()),
            ("zipcode", self.zipcode.as_deref()),
            ("phone", self.phone.as_deref()),
            ("identity_number", self.identity_number.as_deref()),
        ]
    }
}

/// A captured real-time hold with the customer's Mint account, ready to be
/// reversed.
#[derive(Debug, Clone)]
pub struct ReversibleCapture {
    pub hold: RealtimeHold,
    pub mint_account_id: String,
}

/// One row of the real-time wallet console.
#[derive(Debug, Clone)]
pub struct HoldListRow {
    pub hold: RealtimeHold,
    pub reversed_micros: i64,
    pub last_error: Option<String>,
    pub invoice_number: Option<String>,
    pub created_at: NaiveDateTime,
    pub updated_at: NaiveDateTime,
}

/// Totals of a real-time wallet's holds, by status.
#[derive(Debug, Clone, Default)]
pub struct HoldTotals {
    pub open_cents: i64,
    pub open_count: i64,
    pub captured_micros: i64,
    pub reversed_micros: i64,
    pub flagged_cents: i64,
    pub flagged_count: i64,
    pub failed_reversals: i64,
}

fn hold_from(r: &sqlx::postgres::PgRow) -> Result<RealtimeHold> {
    Ok(RealtimeHold {
        id: r.try_get("id")?,
        organization_id: r.try_get("organization_id")?,
        wallet_id: r.try_get("wallet_id")?,
        customer_id: r.try_get("customer_id")?,
        subscription_id: r.try_get("subscription_id")?,
        event_transaction_id: r.try_get("event_transaction_id")?,
        event_timestamp: r.try_get("event_timestamp")?,
        mint_hold_id: r.try_get("mint_hold_id")?,
        amount_cents: r.try_get("amount_cents")?,
        amount_micros: r.try_get("amount_micros")?,
        status: r.try_get("status")?,
        invoice_id: r.try_get("invoice_id")?,
        captured_micros: r.try_get("captured_micros")?,
    })
}

const H: &str = "h.id, h.organization_id, h.wallet_id, h.customer_id, h.subscription_id, \
    h.event_transaction_id, h.event_timestamp, h.mint_hold_id, h.amount_cents, h.amount_micros, \
    h.status, h.invoice_id, h.captured_micros";

impl Db {
    // ---- customer address -------------------------------------------------------

    /// The billing address of a customer (all `None` when unknown).
    pub async fn customer_address(&self, customer_id: Uuid) -> Result<CustomerAddress> {
        let row = sqlx::query(
            "SELECT address_line1, address_line2, city, state, zipcode, phone, identity_number \
             FROM customers WHERE id = $1",
        )
        .bind(customer_id)
        .fetch_optional(&self.pool)
        .await?;
        let Some(r) = row else { return Ok(CustomerAddress::default()) };
        Ok(CustomerAddress {
            address_line1: r.try_get("address_line1")?,
            address_line2: r.try_get("address_line2")?,
            city: r.try_get("city")?,
            state: r.try_get("state")?,
            zipcode: r.try_get("zipcode")?,
            phone: r.try_get("phone")?,
            identity_number: r.try_get("identity_number")?,
        })
    }

    /// Replace the billing address of a customer.
    pub async fn set_customer_address(&self, customer_id: Uuid, a: &CustomerAddress) -> Result<()> {
        sqlx::query(
            "UPDATE customers SET address_line1 = $2, address_line2 = $3, city = $4, state = $5, \
                zipcode = $6, phone = $7, identity_number = $8, updated_at = $9 WHERE id = $1",
        )
        .bind(customer_id)
        .bind(&a.address_line1)
        .bind(&a.address_line2)
        .bind(&a.city)
        .bind(&a.state)
        .bind(&a.zipcode)
        .bind(&a.phone)
        .bind(&a.identity_number)
        .bind(Utc::now().naive_utc())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    // ---- scheduled downgrades ---------------------------------------------------------

    /// The pending downgrade scheduled to replace `subscription_id`, if any.
    pub async fn scheduled_downgrade_for(&self, subscription_id: Uuid) -> Result<Option<Subscription>> {
        let row = sqlx::query(
            "SELECT * FROM subscriptions WHERE previous_subscription_id = $1 AND status = 0 \
             ORDER BY created_at DESC LIMIT 1",
        )
        .bind(subscription_id)
        .fetch_optional(&self.pool)
        .await?;
        row.as_ref().map(mapping::subscription).transpose()
    }

    /// Cancel every pending downgrade that would replace `subscription_id`.
    /// Returns the canceled subscriptions.
    pub async fn cancel_scheduled_downgrades(&self, subscription_id: Uuid) -> Result<Vec<Subscription>> {
        let rows = sqlx::query(
            "UPDATE subscriptions SET status = 3, canceled_at = now(), updated_at = now() \
             WHERE previous_subscription_id = $1 AND status = 0 RETURNING *",
        )
        .bind(subscription_id)
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(mapping::subscription).collect()
    }

    /// Pending downgrades whose start (the outgoing plan's billing boundary)
    /// has arrived: `(outgoing, incoming)`.
    pub async fn due_downgrades(&self, now: NaiveDateTime) -> Result<Vec<(Subscription, Subscription)>> {
        let rows = sqlx::query(
            "SELECT * FROM subscriptions WHERE status = 0 AND previous_subscription_id IS NOT NULL \
               AND subscription_at IS NOT NULL AND subscription_at <= $1 \
             ORDER BY subscription_at LIMIT 500",
        )
        .bind(now)
        .fetch_all(&self.pool)
        .await?;
        let mut out = Vec::with_capacity(rows.len());
        for r in &rows {
            let incoming = mapping::subscription(r)?;
            let Some(prev) = incoming.previous_subscription_id else { continue };
            let outgoing = self.find_subscription(prev).await?;
            out.push((outgoing, incoming));
        }
        Ok(out)
    }

    /// Atomically end `outgoing` (active → terminated) and start `incoming`
    /// (pending → active) at `boundary`. `false` when either already moved
    /// (a concurrent tick won); nothing changes then.
    pub async fn swap_downgrade(&self, outgoing: Uuid, incoming: Uuid, boundary: NaiveDateTime) -> Result<bool> {
        let mut tx = self.pool.begin().await?;
        let ended = sqlx::query(
            "UPDATE subscriptions SET status = 2, terminated_at = $2, updated_at = now() \
             WHERE id = $1 AND status = 1",
        )
        .bind(outgoing)
        .bind(boundary)
        .execute(&mut *tx)
        .await?
        .rows_affected();
        let started = sqlx::query(
            "UPDATE subscriptions SET status = 1, started_at = $2, updated_at = now() \
             WHERE id = $1 AND status = 0",
        )
        .bind(incoming)
        .bind(boundary)
        .execute(&mut *tx)
        .await?
        .rows_affected();
        if ended == 1 && started == 1 {
            tx.commit().await?;
            Ok(true)
        } else {
            tx.rollback().await?;
            Ok(false)
        }
    }

    // ---- real-time wallet reversals -------------------------------------------------

    /// Captured holds of a voided invoice not yet moved back to the customer.
    pub async fn reversible_captures_for_invoice(&self, invoice_id: Uuid) -> Result<Vec<ReversibleCapture>> {
        let rows = sqlx::query(&format!(
            "SELECT {H}, w.mint_account_id FROM wallet_realtime_holds h \
             JOIN wallets w ON w.id = h.wallet_id \
             JOIN invoices i ON i.id = h.invoice_id \
             WHERE h.invoice_id = $1 AND h.status = 'captured' AND h.captured_micros > 0 \
               AND i.status = 2 \
             ORDER BY h.created_at"
        ))
        .bind(invoice_id)
        .fetch_all(&self.pool)
        .await?;
        rows.iter()
            .map(|r| {
                Ok(ReversibleCapture {
                    hold: hold_from(r)?,
                    mint_account_id: r.try_get::<Option<String>, _>("mint_account_id")?.unwrap_or_default(),
                })
            })
            .collect()
    }

    /// Voided invoices of a wallet that still have captured, unreversed holds.
    pub async fn voided_invoices_with_captures(&self, wallet_id: Uuid) -> Result<Vec<Uuid>> {
        let rows = sqlx::query(
            "SELECT DISTINCT h.invoice_id FROM wallet_realtime_holds h JOIN invoices i ON i.id = h.invoice_id \
             WHERE h.wallet_id = $1 AND h.status = 'captured' AND h.captured_micros > 0 AND i.status = 2",
        )
        .bind(wallet_id)
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(|r| Ok(r.try_get("invoice_id")?)).collect()
    }

    /// Record the outcome of a reversal attempt: `reversed` with the amount, or
    /// the error on a still-captured hold.
    pub async fn mark_hold_reversal(&self, hold_id: Uuid, reversed_micros: Option<i64>, error: Option<&str>) -> Result<()> {
        match reversed_micros {
            Some(micros) => {
                sqlx::query(
                    "UPDATE wallet_realtime_holds SET status = 'reversed', reversed_micros = $2, \
                        reversed_at = $3, last_error = NULL, updated_at = $3 \
                     WHERE id = $1 AND status = 'captured'",
                )
                .bind(hold_id)
                .bind(micros)
                .bind(Utc::now().naive_utc())
                .execute(&self.pool)
                .await?;
            }
            None => {
                sqlx::query("UPDATE wallet_realtime_holds SET last_error = $2, updated_at = $3 WHERE id = $1")
                    .bind(hold_id)
                    .bind(error)
                    .bind(Utc::now().naive_utc())
                    .execute(&self.pool)
                    .await?;
            }
        }
        Ok(())
    }

    /// A page of a wallet's holds, newest first, optionally filtered by status.
    pub async fn wallet_holds_page(
        &self,
        wallet_id: Uuid,
        status: Option<&str>,
        limit: i64,
        offset: i64,
    ) -> Result<(Vec<HoldListRow>, i64)> {
        let rows = sqlx::query(&format!(
            "SELECT {H}, h.reversed_micros, h.last_error, h.created_at, h.updated_at, i.number AS invoice_number \
             FROM wallet_realtime_holds h LEFT JOIN invoices i ON i.id = h.invoice_id \
             WHERE h.wallet_id = $1 AND ($2::varchar IS NULL OR h.status = $2) \
             ORDER BY h.created_at DESC, h.id DESC LIMIT $3 OFFSET $4"
        ))
        .bind(wallet_id)
        .bind(status)
        .bind(limit)
        .bind(offset)
        .fetch_all(&self.pool)
        .await?;
        let total: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM wallet_realtime_holds WHERE wallet_id = $1 AND ($2::varchar IS NULL OR status = $2)",
        )
        .bind(wallet_id)
        .bind(status)
        .fetch_one(&self.pool)
        .await?;
        let list = rows
            .iter()
            .map(|r| {
                Ok(HoldListRow {
                    hold: hold_from(r)?,
                    reversed_micros: r.try_get("reversed_micros")?,
                    last_error: r.try_get("last_error")?,
                    invoice_number: r.try_get("invoice_number")?,
                    created_at: r.try_get("created_at")?,
                    updated_at: r.try_get("updated_at")?,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        Ok((list, total))
    }

    /// Hold totals of a wallet by status.
    pub async fn wallet_hold_totals(&self, wallet_id: Uuid) -> Result<HoldTotals> {
        let r = sqlx::query(
            "SELECT \
               COALESCE(SUM(amount_cents) FILTER (WHERE status = 'open'), 0)::bigint AS open_cents, \
               COUNT(*) FILTER (WHERE status = 'open') AS open_count, \
               COALESCE(SUM(captured_micros) FILTER (WHERE status IN ('captured', 'reversed')), 0)::bigint AS captured_micros, \
               COALESCE(SUM(reversed_micros), 0)::bigint AS reversed_micros, \
               COALESCE(SUM(amount_cents) FILTER (WHERE status = 'flagged'), 0)::bigint AS flagged_cents, \
               COUNT(*) FILTER (WHERE status = 'flagged') AS flagged_count, \
               COUNT(*) FILTER (WHERE status = 'captured' AND last_error IS NOT NULL \
                   AND invoice_id IN (SELECT id FROM invoices WHERE status = 2)) AS failed_reversals \
             FROM wallet_realtime_holds WHERE wallet_id = $1",
        )
        .bind(wallet_id)
        .fetch_one(&self.pool)
        .await?;
        Ok(HoldTotals {
            open_cents: r.try_get("open_cents")?,
            open_count: r.try_get("open_count")?,
            captured_micros: r.try_get("captured_micros")?,
            reversed_micros: r.try_get("reversed_micros")?,
            flagged_cents: r.try_get("flagged_cents")?,
            flagged_count: r.try_get("flagged_count")?,
            failed_reversals: r.try_get("failed_reversals")?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn address_updates_are_copies_and_trim_blanks() {
        let a = CustomerAddress::default();
        let b = a.with("city", Some("  Istanbul ")).with("zipcode", Some("   "));
        assert_eq!(a.city, None);
        assert_eq!(b.city.as_deref(), Some("Istanbul"));
        assert_eq!(b.zipcode, None);
        assert_eq!(b.with("unknown", Some("x")), b);
    }
}
