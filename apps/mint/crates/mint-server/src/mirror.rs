//! The portfolio mirror API and its outbound deliveries (Wave P PY1.5).
//!
//! Scribase, Menivor and Rally each record the paid events their payment
//! provider (Dodo today) sent them: one Mint record per event, beside the
//! ledger and never in it. This module has no handle on the ledger, so a
//! mirrored event cannot move a balance, hold funds or start a payout: Mint
//! never receives or forwards anyone's money here.
//!
//! - `POST /v1/mirror/events` `{provider, provider_event_id, kind, amount_micros,
//!   currency, customer_ref, occurred_at_ms}` with `x-api-key` = the product's
//!   mirror key (`MINT_MIRROR_KEYS_JSON`, `[{"product","key"}]`): 201 when
//!   recorded, 200 `{duplicate:true}` for a replay of the same provider event.
//! - `GET /v1/mirror/events?limit=` with the same key: that product's records only.
//! - Every record is delivered to each endpoint in `MINT_EVENT_WEBHOOKS_JSON`
//!   (`[{"name","url","secret":"whsec_...","products":[...]}]`) as a Standard
//!   Webhooks signed `mirror.recorded` event, retried with backoff until a 2xx.

use std::sync::Arc;
use std::time::Duration;

use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use mint_payments::HttpTransport;
use mint_store::{MirrorEvent, MirrorKind, MirrorStore};
use serde::Deserialize;
use serde_json::json;
use subtle::ConstantTimeEq;

/// One product's mirror key.
#[derive(Clone, Debug, Deserialize)]
pub struct MirrorKey {
    pub product: String,
    pub key: String,
}

/// One outbound endpoint.
#[derive(Clone, Debug, Deserialize)]
pub struct Endpoint {
    pub name: String,
    pub url: String,
    pub secret: String,
    #[serde(default)]
    pub products: Vec<String>,
}

/// Everything the mirror needs; deliberately no ledger.
#[derive(Clone)]
pub struct MirrorState {
    pub store: Arc<dyn MirrorStore>,
    pub keys: Arc<Vec<MirrorKey>>,
    pub endpoints: Arc<Vec<Endpoint>>,
    pub transport: Arc<dyn HttpTransport>,
    pub now_ms: fn() -> i64,
}

pub fn system_now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

fn valid_name(value: &str) -> bool {
    (2..=32).contains(&value.len())
        && value.as_bytes()[0].is_ascii_lowercase()
        && value.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// Parses `MINT_MIRROR_KEYS_JSON` (empty or unset: no product can record).
pub fn parse_keys(raw: &str) -> Result<Vec<MirrorKey>, String> {
    let keys: Vec<MirrorKey> = serde_json::from_str(raw).map_err(|e| format!("MINT_MIRROR_KEYS_JSON: {e}"))?;
    for k in &keys {
        if !valid_name(&k.product) || k.key.len() < 24 {
            return Err(format!("MINT_MIRROR_KEYS_JSON: product {:?} needs a lower-case name and a key of 24+ characters", k.product));
        }
    }
    Ok(keys)
}

/// Parses `MINT_EVENT_WEBHOOKS_JSON` (https URLs and whsec_ secrets only; http for localhost).
pub fn parse_endpoints(raw: &str) -> Result<Vec<Endpoint>, String> {
    let endpoints: Vec<Endpoint> = serde_json::from_str(raw).map_err(|e| format!("MINT_EVENT_WEBHOOKS_JSON: {e}"))?;
    for e in &endpoints {
        let local = e.url.starts_with("http://127.0.0.1") || e.url.starts_with("http://localhost");
        if !(e.url.starts_with("https://") || local) || !e.secret.starts_with("whsec_") {
            return Err(format!("MINT_EVENT_WEBHOOKS_JSON: endpoint {:?} needs an https URL and a whsec_ secret", e.name));
        }
    }
    Ok(endpoints)
}

fn product_for(state: &MirrorState, headers: &HeaderMap) -> Option<String> {
    let given = headers.get("x-api-key")?.to_str().ok()?;
    state
        .keys
        .iter()
        .find(|k| k.key.len() == given.len() && bool::from(k.key.as_bytes().ct_eq(given.as_bytes())))
        .map(|k| k.product.clone())
}

fn problem(status: StatusCode, code: &str, detail: &str) -> Response {
    (status, Json(json!({ "error": { "code": code, "message": detail } }))).into_response()
}

#[derive(Deserialize)]
struct RecordBody {
    provider: String,
    provider_event_id: String,
    kind: String,
    amount_micros: i64,
    currency: String,
    customer_ref: String,
    occurred_at_ms: i64,
}

fn event_json(e: &MirrorEvent) -> serde_json::Value {
    json!({
        "id": e.id, "product": e.product, "provider": e.provider, "provider_event_id": e.provider_event_id,
        "kind": e.kind.as_str(), "amount_micros": e.amount_micros, "currency": e.currency,
        "customer_ref": e.customer_ref, "occurred_at_ms": e.occurred_at_ms, "recorded_at_ms": e.recorded_at_ms,
    })
}

async fn record(State(state): State<MirrorState>, headers: HeaderMap, Json(body): Json<RecordBody>) -> Response {
    let Some(product) = product_for(&state, &headers) else {
        return problem(StatusCode::UNAUTHORIZED, "unauthorized", "a product mirror key is required");
    };
    let Some(kind) = MirrorKind::parse(&body.kind) else {
        return problem(StatusCode::BAD_REQUEST, "invalid_kind", "kind is payment, refund or dispute");
    };
    let ok_id = |v: &str| (1..=200).contains(&v.len()) && v.bytes().all(|b| b.is_ascii_graphic());
    if !valid_name(&body.provider)
        || !ok_id(&body.provider_event_id)
        || !ok_id(&body.customer_ref)
        || body.currency.len() != 3
        || !body.currency.bytes().all(|b| b.is_ascii_uppercase())
        || body.occurred_at_ms <= 0
    {
        return problem(StatusCode::BAD_REQUEST, "invalid_event", "provider, provider_event_id, currency (ISO), customer_ref and occurred_at_ms are required");
    }
    let now = (state.now_ms)();
    let event = MirrorEvent {
        id: format!("mev_{}", ulid::Ulid::new().to_string().to_lowercase()),
        product: product.clone(),
        provider: body.provider,
        provider_event_id: body.provider_event_id,
        kind,
        amount_micros: body.amount_micros,
        currency: body.currency,
        customer_ref: body.customer_ref,
        occurred_at_ms: body.occurred_at_ms,
        recorded_at_ms: now,
    };
    let endpoints: Vec<String> = state
        .endpoints
        .iter()
        .filter(|e| e.products.is_empty() || e.products.contains(&product))
        .map(|e| e.name.clone())
        .collect();
    match state.store.record(event.clone(), &endpoints).await {
        Ok(true) => (StatusCode::CREATED, Json(json!({ "recorded": true, "event": event_json(&event) }))).into_response(),
        Ok(false) => (StatusCode::OK, Json(json!({ "recorded": false, "duplicate": true }))).into_response(),
        Err(_) => problem(StatusCode::SERVICE_UNAVAILABLE, "storage", "the mirror could not be written; retry"),
    }
}

#[derive(Deserialize)]
struct ListQuery {
    limit: Option<usize>,
}

async fn list(State(state): State<MirrorState>, headers: HeaderMap, Query(q): Query<ListQuery>) -> Response {
    let Some(product) = product_for(&state, &headers) else {
        return problem(StatusCode::UNAUTHORIZED, "unauthorized", "a product mirror key is required");
    };
    match state.store.list(&product, q.limit.unwrap_or(100).min(500)).await {
        Ok(events) => Json(json!({ "events": events.iter().map(event_json).collect::<Vec<_>>() })).into_response(),
        Err(_) => problem(StatusCode::SERVICE_UNAVAILABLE, "storage", "the mirror could not be read; retry"),
    }
}

/// The mirror routes (own auth: product mirror keys, not the ledger keys).
pub fn router(state: MirrorState) -> Router {
    Router::new().route("/v1/mirror/events", post(record).get(list)).with_state(state)
}

/// Backoff after `attempts` failures: 30 s doubling, at most 6 h.
pub fn backoff_ms(attempts: i32) -> i64 {
    let shift = u32::try_from(attempts.clamp(0, 10)).unwrap_or(10);
    (30_000_i64 << shift).min(6 * 3_600_000)
}

/// Delivers every due record once; returns (delivered, failed).
pub async fn deliver_due(state: &MirrorState) -> (usize, usize) {
    let now = (state.now_ms)();
    let Ok(due) = state.store.due(now, 100).await else {
        tracing::error!("mirror: could not read the delivery outbox");
        return (0, 0);
    };
    let (mut ok, mut failed) = (0, 0);
    for d in due {
        let Some(endpoint) = state.endpoints.iter().find(|e| e.name == d.endpoint) else {
            let _ = state.store.failed(&d.event.id, &d.endpoint, now + backoff_ms(d.attempts), "endpoint no longer configured").await;
            failed += 1;
            continue;
        };
        let body = serde_json::to_vec(&json!({ "type": "mirror.recorded", "data": event_json(&d.event) })).unwrap_or_default();
        let id = format!("{}:{}", d.event.id, d.endpoint);
        let ts = (now / 1000).to_string();
        match mint_payments::outbound::deliver(state.transport.as_ref(), &endpoint.url, &endpoint.secret, &id, &ts, &body).await {
            Ok(status) if (200..300).contains(&status) => {
                let _ = state.store.delivered(&d.event.id, &d.endpoint, now).await;
                ok += 1;
            }
            Ok(status) => {
                let _ = state.store.failed(&d.event.id, &d.endpoint, now + backoff_ms(d.attempts), &format!("HTTP {status}")).await;
                failed += 1;
            }
            Err(e) => {
                let _ = state.store.failed(&d.event.id, &d.endpoint, now + backoff_ms(d.attempts), &e.0).await;
                failed += 1;
            }
        }
    }
    if failed > 0 {
        tracing::warn!(delivered = ok, failed, "mirror: some deliveries will be retried");
    }
    (ok, failed)
}

/// Delivers the outbox every 15 s in the background.
pub fn spawn_delivery(state: MirrorState) {
    if state.endpoints.is_empty() {
        return;
    }
    tokio::spawn(async move {
        loop {
            deliver_due(&state).await;
            tokio::time::sleep(Duration::from_secs(15)).await;
        }
    });
}
