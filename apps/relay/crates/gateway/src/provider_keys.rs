//! Provider API keys managed from the Relay UI.
//!
//! Operators add, rotate, test and remove provider credentials from the
//! Providers page instead of redeploying with new environment variables.
//!
//! * Storage: `relay_provider_credentials`, sealed with AES-256-GCM under the
//!   32-byte master key in `RELAY_CREDENTIALS_KEY` (base64 or hex). The
//!   credential name is bound as associated data so a row cannot be replayed
//!   under another slot. Secrets never leave the gateway: the API returns only
//!   a four-character hint.
//! * Resolution: `config/tensorzero.toml` points each provider at a
//!   `dynamic::<slot>` credential. [`provider_credentials_middleware`] fills the
//!   request's TensorZero `credentials` map with the stored key, or the slot's
//!   environment variable when no key is stored, so env-only deployments keep
//!   working unchanged. A client-supplied credential for the same slot wins.
//! * Consistency: every replica re-reads the table every
//!   [`CREDENTIAL_REFRESH_INTERVAL`], and the replica that handled a write
//!   refreshes immediately.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use std::time::Duration;

use axum::{
    Extension, Json,
    body::Body,
    extract::{Path, Request, State},
    http::StatusCode,
    middleware::Next,
    response::{IntoResponse, Response},
};
use base64::Engine as _;
use relay_contract::{ApiError, ApiErrorEnvelope, DataEnvelope};
use relay_storage_postgres::{PostgresProviderCredentialStore, ProviderCredentialTest};
use ring::aead::{AES_256_GCM, Aad, LessSafeKey, NONCE_LEN, Nonce, UnboundKey};
use ring::rand::{SecureRandom, SystemRandom};
use serde::{Deserialize, Serialize};
use tensorzero_auth::middleware::RequestApiKeyExtension;
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

use crate::billing::MAX_REQUEST_BODY_BYTES;
use crate::relay_access::{is_operator, require_operator};

/// Env var holding the 32-byte credential master key (base64 or 64 hex chars).
pub const CREDENTIALS_KEY_ENV: &str = "RELAY_CREDENTIALS_KEY";
/// How often each replica re-reads stored provider credentials.
pub const CREDENTIAL_REFRESH_INTERVAL: Duration = Duration::from_secs(30);
const MIN_KEY_CHARS: usize = 8;
const MAX_KEY_CHARS: usize = 1_024;
const TEST_TIMEOUT: Duration = Duration::from_secs(10);

/// How a slot's key is verified against its provider without spending tokens.
#[derive(Debug, Clone, Copy)]
enum ConnectionTest {
    /// `GET <url>` with `Authorization: Bearer <key>`.
    Bearer(&'static str),
    /// Anthropic's `x-api-key` + `anthropic-version` headers.
    Anthropic(&'static str),
}

/// One provider credential the gateway config resolves via `dynamic::<name>`.
#[derive(Debug, Clone, Copy)]
pub struct ProviderKeySlot {
    pub name: &'static str,
    pub provider: &'static str,
    pub label: &'static str,
    /// Environment variable used when no key is stored in Relay.
    pub env_var: &'static str,
    test: ConnectionTest,
}

/// Every slot referenced by `config/tensorzero.toml`. Keep in sync with the
/// `dynamic::relay_*_api_key` locations there.
pub const PROVIDER_KEY_SLOTS: &[ProviderKeySlot] = &[
    ProviderKeySlot {
        name: "relay_openai_api_key",
        provider: "openai",
        label: "OpenAI",
        env_var: "OPENAI_API_KEY",
        test: ConnectionTest::Bearer("https://api.openai.com/v1/models"),
    },
    ProviderKeySlot {
        name: "relay_anthropic_api_key",
        provider: "anthropic",
        label: "Anthropic",
        env_var: "ANTHROPIC_API_KEY",
        test: ConnectionTest::Anthropic("https://api.anthropic.com/v1/models"),
    },
    ProviderKeySlot {
        name: "relay_openrouter_api_key",
        provider: "openrouter",
        label: "OpenRouter (failover)",
        env_var: "OPENROUTER_API_KEY",
        test: ConnectionTest::Bearer("https://openrouter.ai/api/v1/key"),
    },
];

fn slot(name: &str) -> Option<&'static ProviderKeySlot> {
    PROVIDER_KEY_SLOTS.iter().find(|slot| slot.name == name)
}

fn env_value(slot: &ProviderKeySlot) -> Option<String> {
    std::env::var(slot.env_var)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

/// Parse the master key. `Ok(None)` when unset; an error when set but invalid,
/// which aborts startup rather than silently disabling key management.
pub fn master_key_from_env() -> Result<Option<LessSafeKey>, String> {
    let Some(raw) = std::env::var(CREDENTIALS_KEY_ENV)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
    else {
        return Ok(None);
    };
    let bytes = if raw.len() == 64 && raw.chars().all(|c| c.is_ascii_hexdigit()) {
        hex::decode(&raw).map_err(|error| format!("{CREDENTIALS_KEY_ENV} hex: {error}"))?
    } else {
        base64::engine::general_purpose::STANDARD
            .decode(&raw)
            .map_err(|error| format!("{CREDENTIALS_KEY_ENV} must be base64 or hex: {error}"))?
    };
    if bytes.len() != 32 {
        return Err(format!(
            "{CREDENTIALS_KEY_ENV} must decode to exactly 32 bytes (got {})",
            bytes.len()
        ));
    }
    let key = UnboundKey::new(&AES_256_GCM, &bytes)
        .map_err(|_| format!("{CREDENTIALS_KEY_ENV} is not a valid AES-256 key"))?;
    Ok(Some(LessSafeKey::new(key)))
}

fn seal(key: &LessSafeKey, name: &str, secret: &str) -> Result<(Vec<u8>, Vec<u8>), String> {
    let mut nonce = [0_u8; NONCE_LEN];
    SystemRandom::new()
        .fill(&mut nonce)
        .map_err(|_| "secure random nonce generation failed".to_string())?;
    let mut in_out = secret.as_bytes().to_vec();
    key.seal_in_place_append_tag(
        Nonce::assume_unique_for_key(nonce),
        Aad::from(name.as_bytes()),
        &mut in_out,
    )
    .map_err(|_| "credential encryption failed".to_string())?;
    Ok((nonce.to_vec(), in_out))
}

fn open(key: &LessSafeKey, name: &str, nonce: &[u8], ciphertext: &[u8]) -> Result<String, String> {
    let nonce = Nonce::try_assume_unique_for_key(nonce)
        .map_err(|_| "stored credential nonce is malformed".to_string())?;
    let mut buffer = ciphertext.to_vec();
    let plain = key
        .open_in_place(nonce, Aad::from(name.as_bytes()), &mut buffer)
        .map_err(|_| {
            "stored credential could not be decrypted (wrong RELAY_CREDENTIALS_KEY?)".to_string()
        })?;
    String::from_utf8(plain.to_vec()).map_err(|_| "stored credential is not UTF-8".to_string())
}

fn key_hint(secret: &str) -> String {
    let tail: String = secret
        .chars()
        .rev()
        .take(4)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    format!("...{tail}")
}

/// Metadata of a stored credential (never the secret).
#[derive(Debug, Clone)]
struct StoredMeta {
    key_hint: String,
    updated_by: String,
    updated_at_ms: i64,
    test: Option<ProviderCredentialTest>,
    decrypt_error: Option<String>,
}

#[derive(Default)]
struct CredentialCache {
    secrets: HashMap<String, String>,
    meta: HashMap<String, StoredMeta>,
}

/// Shared provider-key state (axum extension and middleware state).
#[derive(Clone)]
pub struct ProviderKeys {
    store: Option<PostgresProviderCredentialStore>,
    cipher: Option<Arc<LessSafeKey>>,
    cache: Arc<RwLock<CredentialCache>>,
    http: reqwest::Client,
}

impl ProviderKeys {
    pub fn new(
        store: Option<PostgresProviderCredentialStore>,
        cipher: Option<LessSafeKey>,
    ) -> Self {
        Self {
            store,
            cipher: cipher.map(Arc::new),
            cache: Arc::new(RwLock::new(CredentialCache::default())),
            http: reqwest::Client::builder()
                .timeout(TEST_TIMEOUT)
                .build()
                .unwrap_or_default(),
        }
    }

    /// Re-read and decrypt stored credentials into the in-memory cache.
    pub async fn reload(&self) -> Result<(), String> {
        let Some(store) = &self.store else {
            return Ok(());
        };
        let rows = store.list().await.map_err(|error| error.to_string())?;
        let mut next = CredentialCache::default();
        for row in rows {
            let decrypt_error = match &self.cipher {
                Some(cipher) => {
                    match open(cipher, &row.credential_name, &row.nonce, &row.ciphertext) {
                        Ok(secret) => {
                            next.secrets.insert(row.credential_name.clone(), secret);
                            None
                        }
                        Err(error) => {
                            tracing::error!(credential = %row.credential_name, error = %error, "Relay provider credential could not be opened");
                            Some(error)
                        }
                    }
                }
                None => Some(format!(
                    "{CREDENTIALS_KEY_ENV} is not set, so this stored key cannot be used"
                )),
            };
            next.meta.insert(
                row.credential_name.clone(),
                StoredMeta {
                    key_hint: row.key_hint,
                    updated_by: row.updated_by,
                    updated_at_ms: row.updated_at_ms,
                    test: row.test,
                    decrypt_error,
                },
            );
        }
        match self.cache.write() {
            Ok(mut cache) => *cache = next,
            Err(poisoned) => *poisoned.into_inner() = next,
        }
        Ok(())
    }

    pub fn spawn_refresh(&self, tasks: &TaskTracker, shutdown: CancellationToken) {
        if self.store.is_none() {
            return;
        }
        let keys = self.clone();
        tasks.spawn(async move {
            let mut interval = tokio::time::interval(CREDENTIAL_REFRESH_INTERVAL);
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            interval.tick().await;
            loop {
                tokio::select! {
                    () = shutdown.cancelled() => break,
                    _ = interval.tick() => {
                        if let Err(error) = keys.reload().await {
                            tracing::warn!(error = %error, "Relay provider credential refresh failed; keeping cached keys");
                        }
                    }
                }
            }
        });
    }

    fn stored_secret(&self, name: &str) -> Option<String> {
        let cache = match self.cache.read() {
            Ok(cache) => cache,
            Err(poisoned) => poisoned.into_inner(),
        };
        cache.secrets.get(name).cloned()
    }

    fn stored_meta(&self, name: &str) -> Option<StoredMeta> {
        let cache = match self.cache.read() {
            Ok(cache) => cache,
            Err(poisoned) => poisoned.into_inner(),
        };
        cache.meta.get(name).cloned()
    }

    /// The key a slot resolves to right now: stored first, then environment.
    fn effective(&self, slot: &ProviderKeySlot) -> Option<(String, KeySource)> {
        self.stored_secret(slot.name)
            .map(|secret| (secret, KeySource::Stored))
            .or_else(|| env_value(slot).map(|secret| (secret, KeySource::Environment)))
    }

    fn dynamic_credentials(&self) -> serde_json::Map<String, serde_json::Value> {
        PROVIDER_KEY_SLOTS
            .iter()
            .filter_map(|slot| {
                self.effective(slot)
                    .map(|(secret, _)| (slot.name.to_string(), serde_json::Value::String(secret)))
            })
            .collect()
    }

    fn unavailable_reason(&self) -> Option<String> {
        if self.store.is_none() {
            return Some(
                "Provider key management requires Postgres (TENSORZERO_POSTGRES_URL); keys come from environment variables only."
                    .into(),
            );
        }
        if self.cipher.is_none() {
            return Some(format!(
                "Set {CREDENTIALS_KEY_ENV} (32 random bytes, base64) on the gateway to store provider keys encrypted; until then keys come from environment variables only."
            ));
        }
        None
    }

    fn view(&self, slot: &ProviderKeySlot) -> ProviderKeyView {
        let meta = self.stored_meta(slot.name);
        let source = match (&meta, self.stored_secret(slot.name), env_value(slot)) {
            (_, Some(_), _) => KeySource::Stored,
            (_, None, Some(_)) => KeySource::Environment,
            _ => KeySource::Missing,
        };
        let env_hint = (source == KeySource::Environment)
            .then(|| env_value(slot).map(|value| key_hint(&value)))
            .flatten();
        ProviderKeyView {
            name: slot.name,
            provider: slot.provider,
            label: slot.label,
            env_var: slot.env_var,
            source,
            key_hint: meta.as_ref().map(|meta| meta.key_hint.clone()).or(env_hint),
            stored: meta.is_some(),
            updated_by: meta.as_ref().map(|meta| meta.updated_by.clone()),
            updated_at_ms: meta.as_ref().map(|meta| meta.updated_at_ms),
            last_test: meta.as_ref().and_then(|meta| meta.test.clone()),
            error: meta.and_then(|meta| meta.decrypt_error),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum KeySource {
    Stored,
    Environment,
    Missing,
}

/// One slot as shown on the Providers page. Never carries the secret.
#[derive(Debug, Clone, Serialize)]
pub struct ProviderKeyView {
    pub name: &'static str,
    pub provider: &'static str,
    pub label: &'static str,
    pub env_var: &'static str,
    pub source: KeySource,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub key_hint: Option<String>,
    /// A key row exists in Relay (it may be unusable, see `error`).
    pub stored: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub updated_by: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub updated_at_ms: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_test: Option<ProviderCredentialTest>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// `GET /v1/relay/provider_keys` response body.
#[derive(Debug, Clone, Serialize)]
pub struct ProviderKeysResponse {
    /// Whether keys can be stored in Relay (Postgres + master key present).
    pub management_available: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unavailable_reason: Option<String>,
    pub operator: bool,
    pub keys: Vec<ProviderKeyView>,
}

/// `GET /v1/relay/provider_keys`: every credential slot and where it resolves.
pub async fn list_provider_keys_handler(
    Extension(keys): Extension<ProviderKeys>,
    principal: Option<Extension<RequestApiKeyExtension>>,
) -> Response {
    let Some(Extension(principal)) = principal else {
        return keys_error(
            StatusCode::UNAUTHORIZED,
            "relay_billing_unauthorized",
            "authenticated principal required",
        );
    };
    let unavailable_reason = keys.unavailable_reason();
    Json(DataEnvelope {
        data: ProviderKeysResponse {
            management_available: unavailable_reason.is_none(),
            unavailable_reason,
            operator: is_operator(&principal.key_info.public_id),
            keys: PROVIDER_KEY_SLOTS
                .iter()
                .map(|slot| keys.view(slot))
                .collect(),
        },
    })
    .into_response()
}

/// Body of `PUT /v1/relay/provider_keys/{name}`.
#[derive(Deserialize)]
pub struct SetProviderKeyRequest {
    pub api_key: String,
}

/// `PUT /v1/relay/provider_keys/{name}`: add or rotate a key. Operator only.
pub async fn set_provider_key_handler(
    Extension(keys): Extension<ProviderKeys>,
    Path(name): Path<String>,
    principal: Option<Extension<RequestApiKeyExtension>>,
    Json(request): Json<SetProviderKeyRequest>,
) -> Response {
    let operator = match require_operator(principal.as_ref().map(|p| &p.0)) {
        Ok(operator) => operator,
        Err(response) => return response,
    };
    let Some(slot) = slot(&name) else {
        return unknown_slot();
    };
    let (Some(store), Some(cipher)) = (keys.store.clone(), keys.cipher.clone()) else {
        return keys_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "relay_provider_keys_unavailable",
            &keys.unavailable_reason().unwrap_or_default(),
        );
    };
    let secret = request.api_key.trim();
    let length = secret.chars().count();
    if !(MIN_KEY_CHARS..=MAX_KEY_CHARS).contains(&length)
        || secret.chars().any(|c| c.is_whitespace() || c.is_control())
    {
        return keys_error(
            StatusCode::BAD_REQUEST,
            "relay_provider_key_invalid",
            "paste the full API key: 8-1024 characters with no spaces",
        );
    }
    let (nonce, ciphertext) = match seal(&cipher, slot.name, secret) {
        Ok(sealed) => sealed,
        Err(error) => {
            tracing::error!(error = %error, "Relay provider credential sealing failed");
            return keys_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "relay_provider_key_unavailable",
                "the key could not be encrypted",
            );
        }
    };
    if let Err(error) = store
        .upsert(
            slot.name,
            slot.provider,
            &nonce,
            &ciphertext,
            &key_hint(secret),
            &operator,
        )
        .await
    {
        tracing::error!(error = %error, "Relay provider credential could not be saved");
        return keys_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "relay_provider_key_unavailable",
            "the key could not be saved",
        );
    }
    tracing::info!(credential = slot.name, operator = %operator, "Relay operator stored a provider key");
    refreshed_view(&keys, slot).await
}

/// `DELETE /v1/relay/provider_keys/{name}`: remove a stored key so the slot
/// falls back to its environment variable. Operator only.
pub async fn delete_provider_key_handler(
    Extension(keys): Extension<ProviderKeys>,
    Path(name): Path<String>,
    principal: Option<Extension<RequestApiKeyExtension>>,
) -> Response {
    let operator = match require_operator(principal.as_ref().map(|p| &p.0)) {
        Ok(operator) => operator,
        Err(response) => return response,
    };
    let Some(slot) = slot(&name) else {
        return unknown_slot();
    };
    let Some(store) = keys.store.clone() else {
        return keys_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "relay_provider_keys_unavailable",
            &keys.unavailable_reason().unwrap_or_default(),
        );
    };
    match store.delete(slot.name).await {
        Ok(true) => {}
        Ok(false) => {
            return keys_error(
                StatusCode::NOT_FOUND,
                "relay_provider_key_not_found",
                "no key is stored in Relay for this provider",
            );
        }
        Err(error) => {
            tracing::error!(error = %error, "Relay provider credential could not be deleted");
            return keys_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "relay_provider_key_unavailable",
                "the key could not be removed",
            );
        }
    }
    tracing::info!(credential = slot.name, operator = %operator, "Relay operator removed a provider key");
    refreshed_view(&keys, slot).await
}

/// `POST /v1/relay/provider_keys/{name}/test` response body.
#[derive(Debug, Clone, Serialize)]
pub struct ProviderKeyTestResult {
    pub name: &'static str,
    pub source: KeySource,
    pub ok: bool,
    pub message: String,
}

/// `POST /v1/relay/provider_keys/{name}/test`: verify the effective key with
/// a free provider call (model/key listing). Operator only.
pub async fn test_provider_key_handler(
    Extension(keys): Extension<ProviderKeys>,
    Path(name): Path<String>,
    principal: Option<Extension<RequestApiKeyExtension>>,
) -> Response {
    if let Err(response) = require_operator(principal.as_ref().map(|p| &p.0)) {
        return response;
    }
    let Some(slot) = slot(&name) else {
        return unknown_slot();
    };
    let Some((secret, source)) = keys.effective(slot) else {
        return keys_error(
            StatusCode::CONFLICT,
            "relay_provider_key_missing",
            &format!(
                "no key is configured for {}: store one here or set {}",
                slot.label, slot.env_var
            ),
        );
    };
    let (ok, message) = run_connection_test(&keys.http, slot.test, &secret).await;
    if source == KeySource::Stored
        && let Some(store) = &keys.store
    {
        if let Err(error) = store.record_test(slot.name, ok, &message).await {
            tracing::warn!(error = %error, "Relay could not persist a provider key test result");
        } else if let Err(error) = keys.reload().await {
            tracing::warn!(error = %error, "Relay provider credential refresh failed");
        }
    }
    Json(DataEnvelope {
        data: ProviderKeyTestResult {
            name: slot.name,
            source,
            ok,
            message,
        },
    })
    .into_response()
}

async fn run_connection_test(
    http: &reqwest::Client,
    test: ConnectionTest,
    secret: &str,
) -> (bool, String) {
    let request = match test {
        ConnectionTest::Bearer(url) => http.get(url).bearer_auth(secret),
        ConnectionTest::Anthropic(url) => http
            .get(url)
            .header("x-api-key", secret)
            .header("anthropic-version", "2023-06-01"),
    };
    match request.send().await {
        Ok(response) if response.status().is_success() => {
            (true, "The provider accepted this key.".to_string())
        }
        Ok(response) => {
            let status = response.status();
            let message = match status.as_u16() {
                401 | 403 => "The provider rejected this key (unauthorized).".to_string(),
                429 => "The key is valid but currently rate limited by the provider.".to_string(),
                _ => format!("The provider answered HTTP {}.", status.as_u16()),
            };
            (status.as_u16() == 429, message)
        }
        Err(error) if error.is_timeout() => (
            false,
            "The provider did not answer within 10 seconds.".to_string(),
        ),
        Err(error) => {
            tracing::warn!(error = %error, "Relay provider key test could not reach the provider");
            (
                false,
                "The provider could not be reached from the gateway.".to_string(),
            )
        }
    }
}

async fn refreshed_view(keys: &ProviderKeys, slot: &ProviderKeySlot) -> Response {
    if let Err(error) = keys.reload().await {
        tracing::error!(error = %error, "Relay provider credential refresh failed after a write");
        return keys_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "relay_provider_key_unavailable",
            "the change was saved but the gateway could not reload keys; it will retry automatically",
        );
    }
    Json(DataEnvelope {
        data: keys.view(slot),
    })
    .into_response()
}

fn unknown_slot() -> Response {
    keys_error(
        StatusCode::NOT_FOUND,
        "relay_provider_key_unknown",
        "unknown provider credential",
    )
}

fn keys_error(status: StatusCode, code: &str, message: &str) -> Response {
    (
        status,
        Json(ApiErrorEnvelope {
            error: ApiError {
                message: message.into(),
                kind: Some("relay_provider_key_error".into()),
                code: Some(code.into()),
                param: None,
            },
        }),
    )
        .into_response()
}

/// Which request field carries TensorZero dynamic credentials, if any.
fn credentials_field(method: &axum::http::Method, path: &str) -> Option<&'static str> {
    if method != axum::http::Method::POST {
        return None;
    }
    if path.ends_with("/openai/v1/chat/completions") || path.ends_with("/openai/v1/embeddings") {
        Some("tensorzero::credentials")
    } else if path.ends_with("/inference")
        || path.ends_with("/batch_inference")
        || path.ends_with("/v1/passport/generate")
    {
        Some("credentials")
    } else {
        None
    }
}

/// Supply stored/env provider keys as TensorZero dynamic credentials.
pub async fn provider_credentials_middleware(
    State(keys): State<ProviderKeys>,
    request: Request,
    next: Next,
) -> Response {
    let Some(field) = credentials_field(request.method(), request.uri().path()) else {
        return next.run(request).await;
    };
    let supplied = keys.dynamic_credentials();
    if supplied.is_empty() {
        return next.run(request).await;
    }
    let (mut parts, body) = request.into_parts();
    let bytes = match axum::body::to_bytes(body, MAX_REQUEST_BODY_BYTES).await {
        Ok(bytes) => bytes,
        Err(_) => {
            return keys_error(
                StatusCode::PAYLOAD_TOO_LARGE,
                "request_body_too_large",
                "request body exceeds the gateway limit",
            );
        }
    };
    let rebuilt = match serde_json::from_slice::<serde_json::Value>(&bytes) {
        Ok(serde_json::Value::Object(mut object)) => {
            let credentials = object
                .entry(field.to_string())
                .or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()));
            if let Some(existing) = credentials.as_object_mut() {
                for (name, value) in supplied {
                    // A caller-supplied credential for the same slot wins.
                    existing.entry(name).or_insert(value);
                }
            }
            serde_json::to_vec(&serde_json::Value::Object(object)).ok()
        }
        // Not a JSON object: leave it for the handler to reject.
        _ => None,
    };
    let body = match rebuilt {
        Some(rebuilt) => {
            if parts
                .headers
                .contains_key(axum::http::header::CONTENT_LENGTH)
                && let Ok(length) = axum::http::HeaderValue::from_str(&rebuilt.len().to_string())
            {
                parts
                    .headers
                    .insert(axum::http::header::CONTENT_LENGTH, length);
            }
            Body::from(rebuilt)
        }
        None => Body::from(bytes),
    };
    next.run(Request::from_parts(parts, body)).await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key() -> LessSafeKey {
        LessSafeKey::new(UnboundKey::new(&AES_256_GCM, &[7_u8; 32]).unwrap())
    }

    #[test]
    fn sealed_keys_round_trip_and_bind_the_slot_name() {
        let key = key();
        let (nonce, ciphertext) = seal(&key, "relay_openai_api_key", "sk-test-123456").unwrap();
        assert_ne!(ciphertext, b"sk-test-123456".to_vec());
        assert_eq!(
            open(&key, "relay_openai_api_key", &nonce, &ciphertext).unwrap(),
            "sk-test-123456"
        );
        assert!(open(&key, "relay_anthropic_api_key", &nonce, &ciphertext).is_err());
    }

    #[test]
    fn hints_reveal_only_the_last_four_characters() {
        assert_eq!(key_hint("sk-abcdefgh1234"), "...1234");
    }

    #[test]
    fn credentials_are_injected_only_into_inference_posts() {
        let post = axum::http::Method::POST;
        assert_eq!(credentials_field(&post, "/inference"), Some("credentials"));
        assert_eq!(
            credentials_field(&post, "/batch_inference"),
            Some("credentials")
        );
        assert_eq!(
            credentials_field(&post, "/openai/v1/chat/completions"),
            Some("tensorzero::credentials")
        );
        assert_eq!(
            credentials_field(&axum::http::Method::GET, "/inference"),
            None
        );
        assert_eq!(
            credentials_field(&post, "/relay/billing/wallet/top_up"),
            None
        );
    }

    #[test]
    fn slots_match_the_gateway_config() {
        let config = include_str!("../../../config/tensorzero.toml");
        for slot in PROVIDER_KEY_SLOTS {
            assert!(
                config.contains(&format!("dynamic::{}", slot.name)),
                "config/tensorzero.toml does not reference {}",
                slot.name
            );
        }
    }
}
