use serde::{Deserialize, Serialize};

pub const MICROS_PER_AIT: i64 = 1_000_000;

/// Integer microdollars. AITP money never passes through a binary float.
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(transparent)]
pub struct AmountMicros(pub i64);

impl AmountMicros {
    pub const ZERO: Self = Self(0);

    #[must_use]
    pub const fn new(value: i64) -> Self {
        Self(value)
    }

    #[must_use]
    pub const fn get(self) -> i64 {
        self.0
    }
}

impl From<i64> for AmountMicros {
    fn from(value: i64) -> Self {
        Self(value)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Scheme {
    #[serde(rename = "aitp-hold")]
    Hold,
    #[serde(rename = "aitp-transfer")]
    Transfer,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaymentChallenge {
    pub payee: String,
    pub amount_micros: AmountMicros,
    pub currency: String,
    pub resource: String,
    pub nonce: String,
    pub expiry: i64,
    pub scheme: Scheme,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaymentAuthorization {
    pub payer: String,
    pub payee: String,
    pub amount_micros: AmountMicros,
    pub nonce: String,
    pub resource: String,
    pub expiry: i64,
    pub scheme: Scheme,
    pub idempotency_key: String,
    pub pubkey_id: String,
    pub op_hash: String,
    pub sig: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Receipt {
    #[serde(alias = "sequence")]
    pub seq: u64,
    #[serde(alias = "operation")]
    pub op: String,
    #[serde(alias = "operation_hash")]
    pub op_hash: String,
    pub amount_micros: AmountMicros,
    #[serde(default)]
    pub balance_after_micros: AmountMicros,
    #[serde(default)]
    pub prev_hash: String,
    #[serde(alias = "receipt_hash")]
    pub hash: String,
    #[serde(alias = "signature")]
    pub sig: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HoldRef(pub String);
