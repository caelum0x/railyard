use http::Method;
use relay_cost_feed::CostEventListQuery;
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RelayEndpoint {
    Status,
    Health,
    Catalog,
    Quote,
    CostEvents(CostEventListQuery),
    CostEvent(Uuid),
    ReplayCostEvent(Uuid),
    OpenAiChatCompletions,
    OpenAiEmbeddings,
    NativeInference,
    NativeBatchInference,
    BatchInferenceStatus(Uuid),
    SettlementEvidence(Uuid),
    Custom { method: Method, path: String },
}

impl RelayEndpoint {
    pub fn method(&self) -> Method {
        match self {
            Self::Status
            | Self::Health
            | Self::Catalog
            | Self::CostEvents(_)
            | Self::CostEvent(_)
            | Self::BatchInferenceStatus(_)
            | Self::SettlementEvidence(_) => Method::GET,
            Self::Custom { method, .. } => method.clone(),
            _ => Method::POST,
        }
    }

    pub fn path(&self) -> String {
        match self {
            Self::Status => "status".into(),
            Self::Health => "health".into(),
            Self::Catalog => "v1/relay/catalog".into(),
            Self::Quote => "v1/relay/quotes".into(),
            Self::CostEvents(query) => {
                let mut path = format!("v1/billing/cost-events?limit={}", query.limit);
                if let Some(state) = query.state {
                    path.push_str("&state=");
                    path.push_str(state.as_str());
                }
                if let Some(before) = query.before {
                    path.push_str("&before=");
                    path.push_str(&before.to_string());
                }
                path
            }
            Self::CostEvent(event_id) => format!("v1/billing/cost-events/{event_id}"),
            Self::ReplayCostEvent(event_id) => {
                format!("v1/billing/cost-events/{event_id}/replay")
            }
            Self::OpenAiChatCompletions => "openai/v1/chat/completions".into(),
            Self::OpenAiEmbeddings => "openai/v1/embeddings".into(),
            Self::NativeInference => "inference".into(),
            Self::NativeBatchInference => "batch_inference".into(),
            Self::BatchInferenceStatus(batch_id) => format!("batch_inference/{batch_id}"),
            Self::SettlementEvidence(request_id) => {
                format!("v1/billing/settlements/{request_id}")
            }
            Self::Custom { path, .. } => path.trim_start_matches('/').into(),
        }
    }
}

#[derive(Clone, Default, PartialEq, Eq)]
pub struct RequestOptions {
    pub dry_run: bool,
    pub aitp_authorization: Option<String>,
    pub aitp_request_id: Option<Uuid>,
    pub idempotency_key: Option<String>,
}

impl std::fmt::Debug for RequestOptions {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RequestOptions")
            .field("dry_run", &self.dry_run)
            .field(
                "aitp_authorization",
                &self.aitp_authorization.as_ref().map(|_| "[REDACTED]"),
            )
            .field("aitp_request_id", &self.aitp_request_id)
            .field("idempotency_key", &self.idempotency_key)
            .finish()
    }
}

impl RequestOptions {
    pub fn paid_retry(authorization: impl Into<String>, request_id: Uuid) -> Self {
        Self {
            aitp_authorization: Some(authorization.into()),
            aitp_request_id: Some(request_id),
            ..Self::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_redacts_paid_authorization() {
        let options = RequestOptions::paid_retry("signed-secret", Uuid::now_v7());
        let debug = format!("{options:?}");
        assert!(debug.contains("[REDACTED]"));
        assert!(!debug.contains("signed-secret"));
    }

    #[test]
    fn cost_event_inspection_and_replay_have_fixed_methods_and_paths() {
        let event_id = Uuid::now_v7();
        let inspect = RelayEndpoint::CostEvent(event_id);
        assert_eq!(inspect.method(), Method::GET);
        assert_eq!(inspect.path(), format!("v1/billing/cost-events/{event_id}"));
        let replay = RelayEndpoint::ReplayCostEvent(event_id);
        assert_eq!(replay.method(), Method::POST);
        assert_eq!(
            replay.path(),
            format!("v1/billing/cost-events/{event_id}/replay")
        );
    }

    #[test]
    fn cost_event_listing_has_a_bounded_typed_query() {
        let before = Uuid::now_v7();
        let endpoint = RelayEndpoint::CostEvents(CostEventListQuery {
            state: Some(relay_cost_feed::CostEventState::DeadLettered),
            before: Some(before),
            limit: 25,
        });
        assert_eq!(endpoint.method(), Method::GET);
        assert_eq!(
            endpoint.path(),
            format!("v1/billing/cost-events?limit=25&state=dead_lettered&before={before}")
        );
    }
}
