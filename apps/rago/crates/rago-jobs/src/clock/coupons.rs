//! Coupon-lifecycle clock.
//!
//! Ports `Clock::TerminateCouponsJob` → `Coupons::TerminateService.terminate_all_expired`:
//! active, time-limited coupons whose `expiration_at` has passed are terminated.

use chrono::Utc;
use rago_core::enums::{CouponExpiration, CouponStatus};
use rago_db::Db;

use crate::JobError;

/// Terminate active, time-limited coupons past their expiration. Returns the
/// count terminated. Idempotent: a terminated coupon is no longer `active`.
pub async fn terminate_coupons(db: &Db) -> Result<u64, JobError> {
    let now = Utc::now().naive_utc();
    let r = sqlx::query(
        "UPDATE coupons SET status = $1, updated_at = now() \
         WHERE status = $2 AND expiration = $3 \
           AND expiration_at IS NOT NULL AND expiration_at <= $4",
    )
    .bind(CouponStatus::Terminated.as_i32())
    .bind(CouponStatus::Active.as_i32())
    .bind(CouponExpiration::TimeLimit.as_i32())
    .bind(now)
    .execute(&db.pool)
    .await
    .map_err(rago_db::DbError::from)?;
    Ok(r.rows_affected())
}
