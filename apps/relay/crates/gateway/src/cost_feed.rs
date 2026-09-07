//! Postgres cost-event outbox delivery for Relay.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use axum::{
    Extension, Json,
    extract::{Path, Query},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use relay_cost_feed::{
    CostEvent, CostEventListQuery, CostEventPublisher, CostEventStore, CostFeedError,
    CostFeedWorker, Result, SystemCostFeedClock, WorkerPolicy,
};
use reqwest::Url;
use sqlx::PgPool;
use tensorzero_auth::middleware::RequestApiKeyExtension;
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;
use uuid::Uuid;

pub use relay_storage_postgres::PostgresCostEventStore;

pub async fn inspect_cost_event_handler(
    Path(event_id): Path<Uuid>,
    Extension(store): Extension<Arc<dyn CostEventStore>>,
    principal: Option<Extension<RequestApiKeyExtension>>,
) -> Response {
    let Some(Extension(principal)) = principal else {
        return cost_event_error(StatusCode::UNAUTHORIZED, "authenticated principal required");
    };
    match store.get(&principal.key_info.public_id, event_id).await {
        Ok(Some(event)) => Json(relay_contract::DataEnvelope { data: event }).into_response(),
        Ok(None) | Err(CostFeedError::NotFound) => {
            cost_event_error(StatusCode::NOT_FOUND, "cost event not found")
        }
        Err(error) => {
            tracing::error!(error = %error, "Relay cost-event inspection failed");
            cost_event_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "cost event is unavailable",
            )
        }
    }
}

pub async fn list_cost_events_handler(
    Query(query): Query<CostEventListQuery>,
    Extension(store): Extension<Arc<dyn CostEventStore>>,
    principal: Option<Extension<RequestApiKeyExtension>>,
) -> Response {
    let Some(Extension(principal)) = principal else {
        return cost_event_error(StatusCode::UNAUTHORIZED, "authenticated principal required");
    };
    match store.list(&principal.key_info.public_id, query).await {
        Ok(page) => Json(relay_contract::DataEnvelope { data: page }).into_response(),
        Err(CostFeedError::InvalidQuery(message)) => {
            cost_event_error(StatusCode::BAD_REQUEST, &message)
        }
        Err(error) => {
            tracing::error!(error = %error, "Relay cost-event listing failed");
            cost_event_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "cost events are unavailable",
            )
        }
    }
}

pub async fn replay_cost_event_handler(
    Path(event_id): Path<Uuid>,
    Extension(store): Extension<Arc<dyn CostEventStore>>,
    principal: Option<Extension<RequestApiKeyExtension>>,
) -> Response {
    let Some(Extension(principal)) = principal else {
        return cost_event_error(StatusCode::UNAUTHORIZED, "authenticated principal required");
    };
    match store
        .replay_dead_letter(
            &principal.key_info.public_id,
            event_id,
            relay_billing::now_ms(),
        )
        .await
    {
        Ok(event) => Json(relay_contract::DataEnvelope { data: event }).into_response(),
        Err(CostFeedError::NotFound) => {
            cost_event_error(StatusCode::NOT_FOUND, "cost event not found")
        }
        Err(CostFeedError::NotReplayable) => cost_event_error(
            StatusCode::CONFLICT,
            "cost event is not a replayable dead letter",
        ),
        Err(error) => {
            tracing::error!(error = %error, "Relay cost-event replay failed");
            cost_event_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "cost event replay failed",
            )
        }
    }
}

fn cost_event_error(status: StatusCode, message: &str) -> Response {
    let code = match status {
        StatusCode::UNAUTHORIZED => "relay_cost_event_unauthorized",
        StatusCode::BAD_REQUEST => "relay_cost_event_invalid_query",
        StatusCode::NOT_FOUND => "relay_cost_event_not_found",
        StatusCode::CONFLICT => "relay_cost_event_not_replayable",
        _ => "relay_cost_event_unavailable",
    };
    (
        status,
        Json(relay_contract::ApiErrorEnvelope {
            error: relay_contract::ApiError {
                message: message.into(),
                kind: Some("relay_cost_event_error".into()),
                code: Some(code.into()),
                param: None,
            },
        }),
    )
        .into_response()
}

struct WebhookCostEventPublisher {
    client: reqwest::Client,
    url: Url,
    bearer: Option<String>,
}

impl WebhookCostEventPublisher {
    fn new(url: &str, bearer: Option<String>) -> std::result::Result<Self, String> {
        let url = Url::parse(url).map_err(|error| format!("invalid cost-feed URL: {error}"))?;
        if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
            return Err("cost-feed URL must use http or https and include a host".into());
        }
        if !url.username().is_empty() || url.password().is_some() {
            return Err("cost-feed URL must not contain credentials".into());
        }
        if bearer
            .as_ref()
            .is_some_and(|value| value.trim().is_empty() || value.contains(['\r', '\n']))
        {
            return Err("cost-feed bearer is empty or contains newlines".into());
        }
        let client = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(15))
            .redirect(reqwest::redirect::Policy::none())
            .user_agent(format!("relay-cost-feed/{}", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|error| format!("build cost-feed client: {error}"))?;
        Ok(Self {
            client,
            url,
            bearer,
        })
    }
}

#[async_trait]
impl CostEventPublisher for WebhookCostEventPublisher {
    async fn publish(&self, event: &CostEvent) -> Result<()> {
        let mut request = self
            .client
            .post(self.url.clone())
            .header("idempotency-key", event.event_id.to_string())
            .json(event);
        if let Some(bearer) = &self.bearer {
            request = request.bearer_auth(bearer);
        }
        let response = request
            .send()
            .await
            .map_err(|error| CostFeedError::Publish(error.to_string()))?;
        if !response.status().is_success() {
            return Err(CostFeedError::Publish(format!(
                "cost-feed webhook returned HTTP {}",
                response.status().as_u16()
            )));
        }
        Ok(())
    }
}

/// Start delivery only when an endpoint is configured. Without one, the
/// transactionally-created outbox rows remain pending for a future consumer.
pub fn start_if_configured(
    pool: PgPool,
    tasks: TaskTracker,
    shutdown: CancellationToken,
) -> std::result::Result<(), String> {
    let Some(url) = std::env::var("RELAY_COST_FEED_URL")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
    else {
        tracing::info!("RELAY_COST_FEED_URL is unset; durable cost events will remain pending");
        return Ok(());
    };
    let bearer = std::env::var("RELAY_COST_FEED_BEARER")
        .ok()
        .filter(|value| !value.is_empty());
    let publisher = Arc::new(WebhookCostEventPublisher::new(&url, bearer)?);
    let worker = Arc::new(
        CostFeedWorker::new(
            format!("relay-{}", Uuid::now_v7()),
            Arc::new(PostgresCostEventStore::new(pool)),
            publisher,
            Arc::new(SystemCostFeedClock),
            WorkerPolicy::default(),
        )
        .map_err(|error| error.to_string())?,
    );
    tasks.spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(1));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                () = shutdown.cancelled() => {
                    if let Err(error) = worker.drain_once().await {
                        tracing::error!(error = %error, "final Relay cost-feed drain failed");
                    }
                    break;
                }
                _ = interval.tick() => {
                    match worker.drain_once().await {
                        Ok(report) if report.claimed > 0 => tracing::info!(
                            claimed = report.claimed,
                            published = report.published,
                            rescheduled = report.rescheduled,
                            dead_lettered = report.dead_lettered,
                            "Relay cost-feed drain completed"
                        ),
                        Ok(_) => {}
                        Err(error) => tracing::error!(error = %error, "Relay cost-feed drain failed"),
                    }
                }
            }
        }
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use http_body_util::BodyExt as _;
    use relay_contract::{Microdollars, Usage};
    use relay_cost_feed::{
        CostEventPage, InMemoryCostEventStore, InferenceRoute, PendingCostEvent,
    };
    use tensorzero_auth::key::TensorZeroApiKey;
    use tensorzero_auth::postgres::KeyInfo;

    use super::*;

    const OWNER: &str = "abcdefghijkl";

    fn principal() -> RequestApiKeyExtension {
        RequestApiKeyExtension {
            api_key: Arc::new(
                TensorZeroApiKey::parse(
                    "sk-t0-abcdefghijkl-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                )
                .unwrap(),
            ),
            key_info: KeyInfo {
                public_id: OWNER.into(),
                organization: "default".into(),
                workspace: "default".into(),
                description: None,
                created_at: Utc::now(),
                disabled_at: None,
                expires_at: None,
            },
        }
    }

    fn event(event_id: Uuid) -> CostEvent {
        CostEvent::captured(
            event_id,
            OWNER,
            "openai/gpt-4o",
            InferenceRoute::OpenAiChatCompletions,
            Usage {
                input_tokens: 10,
                output_tokens: 5,
                total_tokens: 15,
                provider_cost_micros: Some(Microdollars::new(40)),
            },
            Microdollars::new(50),
            Microdollars::new(40),
            "receipt",
            1_700_000_000_000,
        )
    }

    #[tokio::test]
    async fn authenticated_owner_can_inspect_its_cost_event() {
        let event_id = Uuid::now_v7();
        let store = Arc::new(InMemoryCostEventStore::default());
        store.append(event(event_id)).await.unwrap();
        let response = inspect_cost_event_handler(
            Path(event_id),
            Extension(store as Arc<dyn CostEventStore>),
            Some(Extension(principal())),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let envelope: relay_contract::DataEnvelope<PendingCostEvent> =
            serde_json::from_slice(&body).unwrap();
        assert_eq!(envelope.data.event.event_id, event_id);
    }

    #[tokio::test]
    async fn authenticated_owner_can_list_filtered_cost_events() {
        let event_id = Uuid::now_v7();
        let store = Arc::new(InMemoryCostEventStore::default());
        store.append(event(event_id)).await.unwrap();
        let response = list_cost_events_handler(
            Query(CostEventListQuery {
                state: Some(relay_cost_feed::CostEventState::Pending),
                before: None,
                limit: 10,
            }),
            Extension(store as Arc<dyn CostEventStore>),
            Some(Extension(principal())),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let envelope: relay_contract::DataEnvelope<CostEventPage> =
            serde_json::from_slice(&body).unwrap();
        assert_eq!(envelope.data.items.len(), 1);
        assert_eq!(envelope.data.items[0].event.event_id, event_id);
    }

    #[tokio::test]
    async fn owner_can_replay_dead_letter_but_unauthenticated_lookup_fails() {
        let event_id = Uuid::now_v7();
        let store = Arc::new(InMemoryCostEventStore::default());
        store.append(event(event_id)).await.unwrap();
        store
            .claim("worker", 1_700_000_000_000, 1_000, 1)
            .await
            .unwrap();
        store
            .dead_letter(event_id, "worker", 1_700_000_000_001, "failed")
            .await
            .unwrap();
        let erased = store.clone() as Arc<dyn CostEventStore>;
        let response = replay_cost_event_handler(
            Path(event_id),
            Extension(erased.clone()),
            Some(Extension(principal())),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        assert!(
            store
                .get(OWNER, event_id)
                .await
                .unwrap()
                .unwrap()
                .dead_lettered_at_ms
                .is_none()
        );

        let response = inspect_cost_event_handler(Path(event_id), Extension(erased), None).await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }
}
