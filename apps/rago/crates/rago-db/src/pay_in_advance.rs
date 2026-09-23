//! Pay-in-advance charge billing: the event lookup and the exactly-once
//! invoice write keyed on `(event, charge)`.

use rago_core::models::{Event, Fee, Invoice};
use uuid::Uuid;

use crate::error::Result;
use crate::mapping;
use crate::repo::{insert_fee_exec, insert_invoice_exec, Db};

impl Db {
    /// One event by id.
    pub async fn find_event_by_id(&self, id: Uuid) -> Result<Option<Event>> {
        let row = sqlx::query("SELECT * FROM events WHERE id = $1 AND deleted_at IS NULL")
            .bind(id)
            .fetch_optional(&self.pool)
            .await?;
        row.as_ref().map(mapping::event).transpose()
    }

    /// Persist the in-advance invoice for `event` covering `charge_ids`,
    /// numbered from the org sequence. Returns `false` (nothing written) when
    /// any of these charges was already billed for this event.
    pub async fn insert_pay_in_advance_invoice(
        &self,
        invoice: &mut Invoice,
        fees: &[Fee],
        event_id: Uuid,
        charge_ids: &[Uuid],
    ) -> Result<bool> {
        let mut tx = self.pool.begin().await?;
        for charge_id in charge_ids {
            let claimed = sqlx::query(
                "INSERT INTO pay_in_advance_billings (event_id, charge_id, organization_id, invoice_id, created_at) \
                 VALUES ($1, $2, $3, $4, now()) ON CONFLICT (event_id, charge_id) DO NOTHING",
            )
            .bind(event_id)
            .bind(charge_id)
            .bind(invoice.organization_id)
            .bind(invoice.id)
            .execute(&mut *tx)
            .await?;
            if claimed.rows_affected() == 0 {
                tx.rollback().await?;
                return Ok(false);
            }
        }
        crate::numbering::number_if_needed(&mut *tx, invoice).await?;
        insert_invoice_exec(&mut *tx, invoice).await?;
        for fee in fees {
            insert_fee_exec(&mut *tx, fee).await?;
        }
        tx.commit().await?;
        Ok(true)
    }
}
