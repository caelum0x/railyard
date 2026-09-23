//! Per-organization invoice numbering (Lago `per_organization` document
//! numbering): `<prefix>-<YYYYMM>-<sequence>`, where the sequence is a
//! strictly increasing per-organization counter.
//!
//! The counter lives in `invoice_number_sequences` and is bumped with an upsert
//! *inside the same transaction as the invoice insert*: the row lock serializes
//! concurrent billers of one organization, and a rolled-back insert (lost
//! re-bill race, constraint failure) rolls the counter back with it, so issued
//! numbers never skip or repeat.

use chrono::{Datelike, NaiveDate};
use rago_core::models::{Fee, Invoice};
use sqlx::{PgConnection, Row};
use uuid::Uuid;

use crate::error::Result;
use crate::repo::{insert_fee_exec, insert_invoice_exec, Db};

/// Longest accepted custom prefix.
pub const MAX_PREFIX_LEN: usize = 20;

/// Lago's default document prefix: the first three letters of the
/// organization name (upper-cased) plus the last four characters of its id.
pub fn default_prefix(org_name: &str, org_id: Uuid) -> String {
    let letters: String = org_name
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .take(3)
        .collect::<String>()
        .to_ascii_uppercase();
    let letters = if letters.is_empty() { "INV".to_string() } else { letters };
    let id = org_id.simple().to_string();
    format!("{}-{}", letters, id[id.len() - 4..].to_ascii_uppercase())
}

/// Validate an operator-supplied prefix: 1..=20 chars of `A-Z a-z 0-9 - _`.
pub fn validate_prefix(prefix: &str) -> std::result::Result<(), &'static str> {
    let p = prefix.trim();
    if p.is_empty() || p.len() > MAX_PREFIX_LEN {
        return Err("value_is_invalid");
    }
    if !p.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
        return Err("value_is_invalid");
    }
    Ok(())
}

/// Format an invoice number.
pub fn format_invoice_number(prefix: &str, issuing_date: NaiveDate, sequence: i64) -> String {
    format!(
        "{}-{:04}{:02}-{:03}",
        prefix,
        issuing_date.year(),
        issuing_date.month(),
        sequence
    )
}

/// Allocate the next number for `org` on an open transaction. Returns
/// `(number, sequential_id)`.
pub(crate) async fn allocate_invoice_number(
    conn: &mut PgConnection,
    org: Uuid,
    issuing_date: NaiveDate,
) -> Result<(String, i32)> {
    let seq: i64 = sqlx::query_scalar(
        "INSERT INTO invoice_number_sequences (organization_id, last_value, updated_at) \
         VALUES ($1, 1, now()) \
         ON CONFLICT (organization_id) DO UPDATE \
            SET last_value = invoice_number_sequences.last_value + 1, updated_at = now() \
         RETURNING last_value",
    )
    .bind(org)
    .fetch_one(&mut *conn)
    .await?;

    let row = sqlx::query("SELECT name, document_number_prefix FROM organizations WHERE id = $1")
        .bind(org)
        .fetch_optional(&mut *conn)
        .await?;
    let prefix = match row {
        Some(r) => {
            let custom: Option<String> = r.try_get("document_number_prefix").ok().flatten();
            match custom.filter(|p| !p.trim().is_empty()) {
                Some(p) => p.trim().to_string(),
                None => default_prefix(&r.try_get::<String, _>("name").unwrap_or_default(), org),
            }
        }
        None => default_prefix("", org),
    };
    let sequential_id = i32::try_from(seq).unwrap_or(i32::MAX);
    Ok((format_invoice_number(&prefix, issuing_date, seq), sequential_id))
}

/// Number `invoice` in place when it has no number yet.
pub(crate) async fn number_if_needed(conn: &mut PgConnection, invoice: &mut Invoice) -> Result<()> {
    if invoice.number.trim().is_empty() {
        let (number, seq) =
            allocate_invoice_number(
                conn,
                invoice.organization_id,
                invoice
                    .issuing_date
                    .unwrap_or_else(|| chrono::Utc::now().date_naive()),
            )
            .await?;
        invoice.number = number;
        invoice.sequential_id = Some(seq);
    }
    Ok(())
}

impl Db {
    /// Persist a new invoice, assigning its per-organization number (when
    /// `invoice.number` is empty) in the same transaction. `invoice` is
    /// updated with the assigned `number` / `sequential_id`.
    pub async fn insert_numbered_invoice(&self, invoice: &mut Invoice) -> Result<()> {
        self.insert_numbered_invoice_with_fees(invoice, &[]).await
    }

    /// [`Self::insert_numbered_invoice`] plus its fees, atomically.
    pub async fn insert_numbered_invoice_with_fees(
        &self,
        invoice: &mut Invoice,
        fees: &[Fee],
    ) -> Result<()> {
        let mut tx = self.pool.begin().await?;
        number_if_needed(&mut *tx, invoice).await?;
        insert_invoice_exec(&mut *tx, invoice).await?;
        for fee in fees {
            insert_fee_exec(&mut *tx, fee).await?;
        }
        tx.commit().await?;
        Ok(())
    }

    /// The organization's custom invoice-number prefix, if one is set.
    pub async fn document_number_prefix(&self, org: Uuid) -> Result<Option<String>> {
        let v: Option<Option<String>> = sqlx::query_scalar(
            "SELECT document_number_prefix FROM organizations WHERE id = $1",
        )
        .bind(org)
        .fetch_optional(&self.pool)
        .await?;
        Ok(v.flatten())
    }

    /// The prefix new invoices of `org` will carry (custom or default).
    pub async fn effective_document_number_prefix(&self, org: Uuid, org_name: &str) -> Result<String> {
        Ok(self
            .document_number_prefix(org)
            .await?
            .filter(|p| !p.trim().is_empty())
            .unwrap_or_else(|| default_prefix(org_name, org)))
    }

    /// Set (or clear, with `None`) the organization's invoice-number prefix.
    /// Only affects invoices numbered from now on.
    pub async fn set_document_number_prefix(&self, org: Uuid, prefix: Option<&str>) -> Result<()> {
        sqlx::query(
            "UPDATE organizations SET document_number_prefix = $2, updated_at = now() WHERE id = $1",
        )
        .bind(org)
        .bind(prefix.map(str::trim).filter(|p| !p.is_empty()))
        .execute(&self.pool)
        .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_prefix_uses_name_and_id_tail() {
        let id = Uuid::parse_str("00000000-0000-0000-0000-00000000abcd").unwrap();
        assert_eq!(default_prefix("acme corp", id), "ACM-ABCD");
        assert_eq!(default_prefix("--", id), "INV-ABCD");
    }

    #[test]
    fn formats_lago_style_number() {
        let d = NaiveDate::from_ymd_opt(2026, 9, 1).unwrap();
        assert_eq!(format_invoice_number("ACM-ABCD", d, 7), "ACM-ABCD-202609-007");
        assert_eq!(format_invoice_number("X", d, 1234), "X-202609-1234");
    }

    #[test]
    fn validates_prefix() {
        assert!(validate_prefix("ACME-01").is_ok());
        assert!(validate_prefix("").is_err());
        assert!(validate_prefix("has space").is_err());
        assert!(validate_prefix(&"A".repeat(21)).is_err());
    }
}
