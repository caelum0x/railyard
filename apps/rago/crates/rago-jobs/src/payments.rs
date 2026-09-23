//! `Invoices::Payments::CreateService` — charge an invoice through a provider
//! and reflect the outcome on the invoice `payment_status`.

use rago_core::models::Invoice;
use rago_db::Db;
use rago_integrations::payments::{PaymentProvider, PaymentRequest};
use rago_integrations::{HttpTransport, PayableStatus, StripeProvider};
use uuid::Uuid;

use crate::JobError;

/// Fixed namespace for deriving deterministic payment ids. Any stable UUID
/// works; it only needs to be constant across processes and re-runs.
const PAYMENT_ID_NAMESPACE: Uuid = Uuid::from_u128(0x7261_676f_5f70_6179_6d65_6e74_5f6e_7331);

/// Derive a **stable** payment id from the invoice's identity.
///
/// The Stripe idempotency key is `payment-<payment_id>`. Previously `payment_id`
/// was a fresh `Uuid::new_v4()` per attempt, so a scheduler/job re-run minted a
/// new key and Stripe charged the customer a second time. Deriving the id
/// deterministically from the (period-unique) invoice id means the same
/// invoice always yields the same key, letting Stripe dedupe the charge — no
/// double-charge on re-run. The matching `payments` row insert is
/// `ON CONFLICT (id) DO NOTHING`, so the DB write is idempotent too.
pub fn deterministic_payment_id(invoice_id: Uuid) -> Uuid {
    Uuid::new_v5(&PAYMENT_ID_NAMESPACE, invoice_id.as_bytes())
}

/// Derive a stable `i64` Postgres advisory-lock key from an invoice id (its
/// first 8 bytes). Only needs to be deterministic per invoice and collision-
/// resistant enough that unrelated invoices rarely share a key (a coincidental
/// collision only serializes two unrelated charges — safe, never incorrect).
fn advisory_lock_key(invoice_id: Uuid) -> i64 {
    let b = invoice_id.as_bytes();
    i64::from_be_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]])
}

/// Charge `invoice` via Stripe (over the injected `transport`), record the
/// payment, and update the invoice `payment_status`. Returns the mapped
/// [`PayableStatus`].
///
/// Concurrency: the "already charged?" check and the provider call are guarded
/// by a Postgres advisory lock keyed on the invoice id. Without it two workers
/// running the dunning sweep at once could both read "no prior payment" and
/// both call Stripe before either records a row — a double charge. The lock
/// serializes them per invoice; a worker that cannot take the lock (another is
/// mid-charge) skips with `Pending` rather than racing.
pub async fn charge_invoice<T: HttpTransport>(
    db: &Db,
    transport: &T,
    stripe_secret_key: &str,
    invoice: &Invoice,
    provider_customer_id: &str,
) -> Result<PayableStatus, JobError> {
    let key = advisory_lock_key(invoice.id);
    let lock = match db.try_advisory_lock(key).await? {
        Some(l) => l,
        // Another worker holds the charge lock for this invoice — do not race it.
        None => return Ok(PayableStatus::Pending),
    };
    let result =
        charge_invoice_locked(db, transport, stripe_secret_key, invoice, provider_customer_id)
            .await;
    // Always release, even on error, so the lock never leaks on the pooled conn.
    lock.release().await?;
    result
}

/// The charge critical section, run while holding the per-invoice advisory lock
/// (see [`charge_invoice`]).
async fn charge_invoice_locked<T: HttpTransport>(
    db: &Db,
    transport: &T,
    stripe_secret_key: &str,
    invoice: &Invoice,
    provider_customer_id: &str,
) -> Result<PayableStatus, JobError> {
    let payment_id = deterministic_payment_id(invoice.id);

    // App-level re-charge guard. The stable Stripe idempotency key only dedupes
    // within Stripe's ~24h idempotency window — a billing re-run later than
    // that would mint a brand-new PaymentIntent for an invoice that was already
    // charged. If a payment for this invoice is already recorded, never call
    // the provider again unless the recorded attempt FAILED (a failed charge is
    // legitimately retried by dunning; anything else — succeeded, pending,
    // refunded — must not be re-charged).
    if let Some((_, _, status, _, _)) = db
        .list_payments(invoice.id)
        .await?
        .into_iter()
        .find(|row| row.0 == payment_id)
    {
        match status.as_str() {
            "failed" => {} // dunning retry allowed
            "succeeded" => return Ok(PayableStatus::Succeeded),
            _ => return Ok(PayableStatus::Pending),
        }
    }

    let provider = StripeProvider::new(stripe_secret_key, transport);

    let req = PaymentRequest {
        payment_id,
        invoice_id: invoice.id,
        customer_id: invoice.customer_id.unwrap_or_default(),
        provider_customer_id: provider_customer_id.to_string(),
        amount_cents: invoice.total_amount_cents,
        currency: invoice.currency().code(),
        reference: invoice.number.clone(),
        provider_mandate_id: None,
    };

    let outcome = provider
        .create_payment(&req)
        .await
        .map_err(|e| JobError::Payment(e.to_string()))?;

    let status_str = match outcome.payable_status {
        PayableStatus::Succeeded => "succeeded",
        PayableStatus::Failed => "failed",
        PayableStatus::Pending => "pending",
    };

    db.insert_payment(
        req.payment_id,
        invoice.organization_id,
        invoice.id,
        provider.payment_type(),
        Some(&outcome.provider_payment_id),
        Some(&outcome.provider_status),
        status_str,
        invoice.total_amount_cents,
        &invoice.currency().code(),
    )
    .await?;

    if let Some(ps) = outcome.payable_status.invoice_payment_status() {
        db.update_invoice_payment_status(invoice.id, ps.as_i32()).await?;
    }

    Ok(outcome.payable_status)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Two capture attempts for the same invoice must derive the SAME payment id,
    /// so the Stripe idempotency key `payment-<id>` is identical and Stripe
    /// dedupes the charge on a scheduler/job re-run (the CRITICAL double-charge fix).
    #[test]
    fn deterministic_payment_id_is_stable_per_invoice() {
        let invoice = Uuid::new_v4();

        let first = deterministic_payment_id(invoice);
        let second = deterministic_payment_id(invoice);
        assert_eq!(first, second, "same invoice -> same payment id");

        // The Stripe idempotency key derived from it is therefore stable too.
        assert_eq!(format!("payment-{first}"), format!("payment-{second}"));

        // Distinct invoices must not collide onto the same key.
        let other = deterministic_payment_id(Uuid::new_v4());
        assert_ne!(first, other, "different invoices -> different keys");
    }
}
