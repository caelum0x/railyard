//! Focused streaming tests: bytes must arrive before Relay completes and every
//! incomplete/error terminal must release the bounded Mint hold.

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use futures::{stream, StreamExt};
use tower::ServiceExt;

use vend_core::error::VendError;
use vend_core::transport::{RelayRequest, RelayResponse, RelayStreamResponse, RelayTransport};
use vend_server::pricing::{ModelRate, PricingCatalog};
use vend_server::{build_router, AppState};
use vend_settlement::PayoutConfig;
use vend_testkit::fixtures::{two_tenants, TenantFixture};
use vend_testkit::{FakeMint, InMemoryStore};

#[derive(Clone)]
enum StreamScript {
    Chunks(Vec<(Duration, Result<Vec<u8>, VendError>)>),
    NeverAfterFirst(Vec<u8>),
}

struct StreamingRelay {
    status: u16,
    headers: Vec<(String, Vec<u8>)>,
    script: StreamScript,
}

#[async_trait]
impl RelayTransport for StreamingRelay {
    async fn chat_completion(&self, _req: RelayRequest) -> Result<RelayResponse, VendError> {
        unreachable!("streaming tests use chat_completion_stream")
    }

    async fn chat_completion_stream(
        &self,
        _req: RelayRequest,
    ) -> Result<RelayStreamResponse, VendError> {
        let stream: vend_core::transport::RelayByteStream = match self.script.clone() {
            StreamScript::Chunks(chunks) => {
                let chunks = VecDeque::from(chunks);
                Box::pin(stream::unfold(chunks, |mut chunks| async move {
                    let (delay, item) = chunks.pop_front()?;
                    tokio::time::sleep(delay).await;
                    Some((item, chunks))
                }))
            }
            StreamScript::NeverAfterFirst(first) => Box::pin(
                stream::once(async move { Ok(first) })
                    .chain(stream::pending::<Result<Vec<u8>, VendError>>()),
            ),
        };
        Ok(RelayStreamResponse {
            status: self.status,
            headers: self.headers.clone(),
            stream,
        })
    }
}

fn state(
    store: Arc<InMemoryStore>,
    mint: Arc<FakeMint>,
    relay: Arc<dyn RelayTransport>,
) -> AppState {
    AppState {
        metrics: Arc::new(vend_server::observability::Metrics::default()),
        mint,
        relay,
        tenants: store.clone(),
        keys: store.clone(),
        usage: store.clone(),
        payouts: store.clone(),
        readiness: store.clone(),
        idempotency: store,
        pricing: Arc::new(
            PricingCatalog::new([(
                "gpt-4o-mini".to_string(),
                ModelRate {
                    input_micros_per_million_tokens: 10_000_000,
                    output_micros_per_million_tokens: 10_000_000,
                    max_input_tokens: 100_000,
                    max_output_tokens: 1_000_000,
                },
            )])
            .unwrap(),
        ),
        relay_base_path: "/openai/v1".to_string(),
        relay_key: "relay-test-key".to_string(),
        settlement_account: "acct_vend_settlement".to_string(),
        payout_config: PayoutConfig::new(
            "acct_vend_settlement",
            "acct_provider_payout",
            "acct_platform_payout",
        )
        .unwrap(),
        security_ops: None,
    }
}

fn request(fixture: &TenantFixture, key: &str) -> Request<Body> {
    let body = serde_json::json!({
        "model": "gpt-4o-mini",
        "messages": [{"role": "user", "content": "hi"}],
        "max_tokens": 100,
        "stream": true
    });
    Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .header("content-type", "application/json")
        .header("authorization", format!("Bearer {}", fixture.key_secret))
        .header("idempotency-key", key)
        .body(Body::from(serde_json::to_vec(&body).unwrap()))
        .unwrap()
}

fn account(fixture: &TenantFixture) -> &str {
    &fixture.app.mint_account_id
}

fn terminal_chunks() -> Vec<(Duration, Result<Vec<u8>, VendError>)> {
    vec![
        (
            Duration::ZERO,
            Ok(b"data: {\"choices\":[{\"delta\":{\"content\":\"hi\"}}],\"usage\":null}\n\n"
                .to_vec()),
        ),
        (
            Duration::from_millis(250),
            Ok(b"data: {\"choices\":[],\"usage\":{\"input_tokens\":3,\"output_tokens\":2,\"tensorzero_cost\":0.000050}}\n\n".to_vec()),
        ),
        (Duration::ZERO, Ok(b"data: [DONE]\n\n".to_vec())),
    ]
}

async fn wait_for_balance(mint: &FakeMint, account: &str, expected: i64) {
    tokio::time::timeout(Duration::from_secs(2), async {
        while mint.balance(account) != expected {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("settlement did not finish");
}

#[tokio::test]
async fn first_sse_chunk_arrives_before_terminal_and_done_is_preserved() {
    let fixtures = two_tenants();
    let store = Arc::new(fixtures.store);
    let account = account(&fixtures.a).to_string();
    let mint = Arc::new(FakeMint::new().with_balance(&account, 100_000_000));
    let relay = Arc::new(StreamingRelay {
        status: 200,
        headers: vec![
            ("content-type".into(), b"text/event-stream".to_vec()),
            ("x-request-id".into(), b"relay-123".to_vec()),
        ],
        script: StreamScript::Chunks(terminal_chunks()),
    });
    let app = build_router(state(store.clone(), mint.clone(), relay));
    let response = app
        .clone()
        .oneshot(request(&fixtures.a, "incremental"))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["content-type"], "text/event-stream");
    assert_eq!(response.headers()["x-request-id"], "relay-123");
    let charge_id = response.headers()["vend-charge-id"]
        .to_str()
        .unwrap()
        .to_string();

    let mut body = response.into_body().into_data_stream();
    let first = tokio::time::timeout(Duration::from_millis(100), body.next())
        .await
        .expect("first byte was buffered until terminal completion")
        .unwrap()
        .unwrap();
    assert!(first.starts_with(b"data: "));
    assert!(
        store.payout_by_charge_id(&charge_id).is_none(),
        "payout/capture planning must wait for terminal usage and [DONE]"
    );
    let duplicate = app
        .oneshot(request(&fixtures.a, "incremental"))
        .await
        .unwrap();
    assert_eq!(duplicate.status(), StatusCode::CONFLICT);
    let duplicate_body = axum::body::to_bytes(duplicate.into_body(), usize::MAX)
        .await
        .unwrap();
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&duplicate_body).unwrap()["error"]["code"],
        "idempotency_in_progress"
    );

    let mut received = first.to_vec();
    while let Some(chunk) = body.next().await {
        received.extend_from_slice(&chunk.unwrap());
    }
    assert!(received.ends_with(b"data: [DONE]\n\n"));
    wait_for_balance(&mint, &account, 99_998_950).await;
    wait_for_balance(&mint, "acct_provider_payout", 50).await;
    assert_eq!(mint.balance("acct_platform_payout"), 100);
    assert_eq!(mint.balance(&fixtures.a.tenant.mint_account_id), 900);
}

#[tokio::test]
async fn missing_terminal_cost_forwards_bytes_but_releases() {
    let fixtures = two_tenants();
    let store = Arc::new(fixtures.store);
    let account = account(&fixtures.a).to_string();
    let mint = Arc::new(FakeMint::new().with_balance(&account, 100_000_000));
    let relay = Arc::new(StreamingRelay {
        status: 200,
        headers: vec![("content-type".into(), b"text/event-stream".to_vec())],
        script: StreamScript::Chunks(vec![
            (
                Duration::ZERO,
                Ok(b"data: {\"choices\":[],\"usage\":{\"input_tokens\":1}}\n\n".to_vec()),
            ),
            (Duration::ZERO, Ok(b"data: [DONE]\n\n".to_vec())),
        ]),
    });
    let response = build_router(state(store, mint.clone(), relay))
        .oneshot(request(&fixtures.a, "missing-cost"))
        .await
        .unwrap();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    assert!(bytes.ends_with(b"data: [DONE]\n\n"));
    wait_for_balance(&mint, &account, 100_000_000).await;
}

#[tokio::test]
async fn upstream_non_2xx_preserves_status_and_body_then_releases() {
    let fixtures = two_tenants();
    let store = Arc::new(fixtures.store);
    let account = account(&fixtures.a).to_string();
    let mint = Arc::new(FakeMint::new().with_balance(&account, 100_000_000));
    let relay = Arc::new(StreamingRelay {
        status: 429,
        headers: vec![("retry-after".into(), b"7".to_vec())],
        script: StreamScript::Chunks(vec![(
            Duration::ZERO,
            Ok(b"{\"error\":\"limited\"}".to_vec()),
        )]),
    });
    let response = build_router(state(store, mint.clone(), relay))
        .oneshot(request(&fixtures.a, "non-2xx"))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(response.headers()["retry-after"], "7");
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    assert_eq!(&bytes[..], b"{\"error\":\"limited\"}");
    wait_for_balance(&mint, &account, 100_000_000).await;
}

#[tokio::test]
async fn relay_body_transport_error_terminates_stream_and_releases() {
    let fixtures = two_tenants();
    let store = Arc::new(fixtures.store);
    let account = account(&fixtures.a).to_string();
    let mint = Arc::new(FakeMint::new().with_balance(&account, 100_000_000));
    let relay = Arc::new(StreamingRelay {
        status: 200,
        headers: vec![("content-type".into(), b"text/event-stream".to_vec())],
        script: StreamScript::Chunks(vec![
            (
                Duration::ZERO,
                Ok(b"data: {\"choices\":[{}],\"usage\":null}\n\n".to_vec()),
            ),
            (
                Duration::ZERO,
                Err(VendError::Transport("relay reset".into())),
            ),
        ]),
    });
    let response = build_router(state(store, mint.clone(), relay))
        .oneshot(request(&fixtures.a, "body-error"))
        .await
        .unwrap();
    assert!(
        axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .is_err(),
        "a mid-stream Relay error must terminate the downstream body"
    );
    wait_for_balance(&mint, &account, 100_000_000).await;
}

#[tokio::test]
async fn dropping_client_body_releases_without_waiting_for_another_relay_chunk() {
    let fixtures = two_tenants();
    let store = Arc::new(fixtures.store);
    let account = account(&fixtures.a).to_string();
    let mint = Arc::new(FakeMint::new().with_balance(&account, 100_000_000));
    let relay = Arc::new(StreamingRelay {
        status: 200,
        headers: vec![("content-type".into(), b"text/event-stream".to_vec())],
        script: StreamScript::NeverAfterFirst(
            b"data: {\"choices\":[{}],\"usage\":null}\n\n".to_vec(),
        ),
    });
    let response = build_router(state(store, mint.clone(), relay))
        .oneshot(request(&fixtures.a, "disconnect"))
        .await
        .unwrap();
    let mut body = response.into_body().into_data_stream();
    assert!(body.next().await.unwrap().is_ok());
    drop(body);
    wait_for_balance(&mint, &account, 100_000_000).await;
}

#[tokio::test]
async fn completed_streaming_key_is_consumed_and_never_replayed() {
    let fixtures = two_tenants();
    let store = Arc::new(fixtures.store);
    let account = account(&fixtures.a).to_string();
    let mint = Arc::new(FakeMint::new().with_balance(&account, 100_000_000));
    let relay = Arc::new(StreamingRelay {
        status: 200,
        headers: vec![("content-type".into(), b"text/event-stream".to_vec())],
        script: StreamScript::Chunks(terminal_chunks()),
    });
    let app = build_router(state(store, mint.clone(), relay));
    let first = app
        .clone()
        .oneshot(request(&fixtures.a, "single-use"))
        .await
        .unwrap();
    axum::body::to_bytes(first.into_body(), usize::MAX)
        .await
        .unwrap();
    wait_for_balance(&mint, &account, 99_998_950).await;

    let replay = app
        .oneshot(request(&fixtures.a, "single-use"))
        .await
        .unwrap();
    assert_eq!(replay.status(), StatusCode::CONFLICT);
    let body = axum::body::to_bytes(replay.into_body(), usize::MAX)
        .await
        .unwrap();
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&body).unwrap()["error"]["code"],
        "streaming_idempotency_consumed"
    );
    assert_eq!(mint.balance(&account), 99_998_950);
}
