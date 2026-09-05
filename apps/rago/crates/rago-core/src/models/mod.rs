//! Domain models — faithful Rust ports of Lago's `app/models/*` billing
//! entities. Each struct mirrors the Postgres columns in `db/structure.sql`
//! for the billing vertical; ActiveRecord associations become explicit IDs or
//! caller-supplied values (there is no lazy `belongs_to` magic in Rust).

pub mod api_key;
pub mod billable_metric;
pub mod charge;
pub mod coupon;
pub mod credit_note;
pub mod customer;
pub mod event;
pub mod fee;
pub mod invoice;
pub mod organization;
pub mod payment;
pub mod plan;
pub mod subscription;
pub mod tax;
pub mod wallet;

pub use api_key::ApiKey;
pub use billable_metric::BillableMetric;
pub use charge::{Charge, ChargeFilter};
pub use coupon::{AppliedCoupon, Coupon, Credit};
pub use credit_note::{CreditNote, CreditNoteAppliedTax, CreditNoteItem};
pub use customer::Customer;
pub use event::{Event, QuantifiedEvent};
pub use fee::{Fee, FeeAppliedTax};
pub use invoice::{Invoice, InvoiceAppliedTax, InvoiceSubscription};
pub use organization::Organization;
pub use payment::Payment;
pub use plan::Plan;
pub use subscription::Subscription;
pub use tax::Tax;
pub use wallet::{Wallet, WalletTransaction};
