//! HTTP handlers — one module per resource group. Each handler (1) validates
//! the boundary body, (2) builds the domain request, (3) calls EXACTLY ONE
//! domain method, (4) maps `Ok → JSON envelope` / `Err → ApiError`. No business
//! logic lives here — it is all in the ledger/protocol/stream/reserve facades.

pub mod accounts;
pub mod addresses;
pub mod aitp;
pub mod allowances;
pub mod checkouts;
pub mod deposits;
pub mod health;
pub mod holds;
pub mod mint_redeem;
pub mod receipts;
pub mod reserve;
pub mod streams;
pub mod transfers;
pub mod verification_keys;
