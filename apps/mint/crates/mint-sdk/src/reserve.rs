//! Proof-of-reserves and deposit-gated mint / KYC-gated redemption.

use reqwest::Method;
use serde::{Deserialize, Serialize};

use crate::{Balance, Error, MintClient};

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct ReserveProof {
    pub supply_micros: i64,
    pub backing_micros: i64,
    pub cushion: i64,
    pub ok: bool,
}

#[derive(Debug, Serialize)]
pub struct MintRequest<'a> {
    pub to: &'a str,
    pub amount_micros: i64,
    pub deposit_ref: &'a str,
    pub idem_key: &'a str,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct MintResponse {
    pub first_seq: u64,
    pub to_balance: Balance,
    pub reserve_after_micros: i64,
    pub deposit_ref: String,
    pub idempotent_replay: bool,
}

#[derive(Debug, Serialize)]
pub struct RedeemRequest<'a> {
    pub from: &'a str,
    pub amount_micros: i64,
    pub payout_ref: &'a str,
    pub kyc_tier: &'a str,
    pub payout_dest: PayoutDestination<'a>,
    pub idem_key: &'a str,
}

#[derive(Debug, Serialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum PayoutDestination<'a> {
    Stripe { account: &'a str },
    Crypto { chain: &'a str, address: &'a str },
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct Payout {
    pub payout_ref: String,
    pub amount_micros: i64,
    pub status: String,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct RedeemResponse {
    pub first_seq: u64,
    pub from_balance: Balance,
    pub reserve_after_micros: i64,
    pub payout_ref: String,
    pub payout: Payout,
    pub idempotent_replay: bool,
}

impl MintClient {
    pub async fn reserve(&self) -> Result<ReserveProof, Error> {
        self.get("v1/reserve").await
    }

    pub async fn mint(&self, request: &MintRequest<'_>) -> Result<MintResponse, Error> {
        self.send(Method::POST, "v1/mint", Some(request)).await
    }

    pub async fn redeem(&self, request: &RedeemRequest<'_>) -> Result<RedeemResponse, Error> {
        self.send(Method::POST, "v1/redeem", Some(request)).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn payout_destination_is_server_tagged_shape() {
        let value = serde_json::to_value(PayoutDestination::Crypto {
            chain: "solana",
            address: "wallet",
        })
        .unwrap();
        assert_eq!(value["kind"], "crypto");
        assert_eq!(value["chain"], "solana");
    }
}
