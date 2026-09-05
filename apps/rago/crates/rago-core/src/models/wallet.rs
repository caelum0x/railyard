use crate::enums::{WalletStatus, WalletTransactionStatus, WalletTransactionType};
use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// `wallets` — a customer's prepaid credit balance. (Lago
/// `app/models/wallet.rb`.)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Wallet {
    pub id: Uuid,
    pub organization_id: Uuid,
    pub customer_id: Uuid,
    pub status: WalletStatus,
    pub name: Option<String>,
    /// Fiat value of one credit (`numeric(30,5)`).
    pub rate_amount: Decimal,
    pub credits_balance: Decimal,
    pub consumed_credits: Decimal,
    pub balance_cents: i64,
    pub balance_currency: String,
    pub consumed_amount_cents: i64,
    pub ongoing_balance_cents: i64,
    pub ongoing_usage_balance_cents: i64,
    pub credits_ongoing_balance: Decimal,
    pub credits_ongoing_usage_balance: Decimal,
    pub expiration_at: Option<DateTime<Utc>>,
    pub terminated_at: Option<DateTime<Utc>>,
    pub lock_version: i32,
    pub priority: i32,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// `wallet_transactions` — a credit/debit against a wallet. (Lago
/// `app/models/wallet_transaction.rb`.)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WalletTransaction {
    pub id: Uuid,
    pub organization_id: Uuid,
    pub wallet_id: Uuid,
    pub transaction_type: WalletTransactionType,
    pub status: WalletTransactionStatus,
    pub amount: Decimal,
    pub credit_amount: Decimal,
    pub invoice_id: Option<Uuid>,
    pub settled_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}
