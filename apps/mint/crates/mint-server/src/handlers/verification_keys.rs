//! Public Ed25519 verification-key discovery.

use axum::extract::State;
use axum::Json;
use mint_store::LedgerStore;

use crate::attestation::{verification_keys_document, VerificationKeysDocument};
use crate::state::AppState;

/// `GET /.well-known/mint-verification-keys.json`.
pub async fn get<S: LedgerStore + 'static>(
    State(st): State<AppState<S>>,
) -> Json<VerificationKeysDocument> {
    Json(verification_keys_document(st.receipt_signer.as_ref()))
}
