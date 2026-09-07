use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use std::time::Duration;

use futures::{StreamExt as _, stream::FuturesUnordered};
use relay_contract::SettlementEvidence;
use thiserror::Error;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::{RelayClient, RelaySdkError};

/// Synchronous Drop seam used by streams. Implementations must enqueue work;
/// they must never block or attempt network I/O in `abandoned`.
pub trait AbandonedStreamHandler: Send + Sync {
    fn abandoned(&self, request_id: Uuid);
}

pub trait SettlementEvidenceValidator: Send + Sync {
    fn validate(&self, evidence: &SettlementEvidence) -> Result<(), EvidenceValidationError>;
}

#[derive(Debug, Clone, PartialEq)]
pub enum RecoveryState {
    Pending,
    Verified(Box<SettlementEvidence>),
    Failed(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RecoveryPolicy {
    pub max_attempts: u32,
    pub max_in_flight: usize,
    pub queue_capacity: usize,
    pub initial_delay: Duration,
    pub max_delay: Duration,
}

impl Default for RecoveryPolicy {
    fn default() -> Self {
        Self {
            max_attempts: 20,
            max_in_flight: 32,
            queue_capacity: 1_024,
            initial_delay: Duration::from_millis(250),
            max_delay: Duration::from_secs(10),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("settlement evidence validation failed: {0}")]
pub struct EvidenceValidationError(pub String);

pub struct RecoveryQueue {
    sender: mpsc::Sender<Uuid>,
    states: Arc<RwLock<HashMap<Uuid, RecoveryState>>>,
}

impl RecoveryQueue {
    pub fn new(
        client: RelayClient,
        validator: Arc<dyn SettlementEvidenceValidator>,
        policy: RecoveryPolicy,
    ) -> Result<(Arc<Self>, RecoveryWorker), RelaySdkError> {
        if policy.max_attempts == 0
            || policy.max_in_flight == 0
            || policy.queue_capacity == 0
            || policy.initial_delay.is_zero()
            || policy.max_delay < policy.initial_delay
        {
            return Err(RelaySdkError::InvalidConfig(
                "invalid abandoned-stream recovery policy".into(),
            ));
        }
        let (sender, receiver) = mpsc::channel(policy.queue_capacity);
        let states = Arc::new(RwLock::new(HashMap::new()));
        Ok((
            Arc::new(Self {
                sender,
                states: states.clone(),
            }),
            RecoveryWorker {
                receiver,
                client,
                validator,
                policy,
                states,
            },
        ))
    }

    pub fn state(&self, request_id: Uuid) -> Option<RecoveryState> {
        self.states
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .get(&request_id)
            .cloned()
    }

    /// Remove a terminal state after the application has reconciled it. A
    /// pending state is left in place so callers cannot accidentally orphan an
    /// in-flight lookup.
    pub fn take_terminal(&self, request_id: Uuid) -> Option<RecoveryState> {
        let mut states = self
            .states
            .write()
            .unwrap_or_else(|error| error.into_inner());
        if matches!(states.get(&request_id), Some(RecoveryState::Pending)) {
            None
        } else {
            states.remove(&request_id)
        }
    }
}

impl AbandonedStreamHandler for RecoveryQueue {
    fn abandoned(&self, request_id: Uuid) {
        let inserted = {
            let mut states = self
                .states
                .write()
                .unwrap_or_else(|error| error.into_inner());
            if let std::collections::hash_map::Entry::Vacant(entry) = states.entry(request_id) {
                entry.insert(RecoveryState::Pending);
                true
            } else {
                false
            }
        };
        if inserted {
            let failure = match self.sender.try_send(request_id) {
                Ok(()) => return,
                Err(mpsc::error::TrySendError::Full(_)) => {
                    "settlement recovery queue capacity exceeded"
                }
                Err(mpsc::error::TrySendError::Closed(_)) => {
                    "settlement recovery worker is not running"
                }
            };
            self.set_state(request_id, RecoveryState::Failed(failure.into()));
        }
    }
}

pub struct RecoveryWorker {
    receiver: mpsc::Receiver<Uuid>,
    client: RelayClient,
    validator: Arc<dyn SettlementEvidenceValidator>,
    policy: RecoveryPolicy,
    states: Arc<RwLock<HashMap<Uuid, RecoveryState>>>,
}

impl RecoveryWorker {
    pub async fn run(mut self, shutdown: CancellationToken) {
        let mut receiver_open = true;
        let mut recoveries = FuturesUnordered::new();
        loop {
            if !receiver_open && recoveries.is_empty() {
                break;
            }
            tokio::select! {
                () = shutdown.cancelled() => {
                    self.fail_pending("settlement recovery stopped before completion");
                    break;
                },
                request_id = self.receiver.recv(), if receiver_open && recoveries.len() < self.policy.max_in_flight => {
                    match request_id {
                        Some(request_id) => recoveries.push(Self::recover(
                            self.client.clone(),
                            self.validator.clone(),
                            self.policy,
                            request_id,
                            shutdown.clone(),
                        )),
                        None => receiver_open = false,
                    }
                },
                Some((request_id, state)) = recoveries.next(), if !recoveries.is_empty() => {
                    self.set_state(request_id, state);
                }
            }
        }
    }

    async fn recover(
        client: RelayClient,
        validator: Arc<dyn SettlementEvidenceValidator>,
        policy: RecoveryPolicy,
        request_id: Uuid,
        shutdown: CancellationToken,
    ) -> (Uuid, RecoveryState) {
        let mut delay = policy.initial_delay;
        for attempt in 1..=policy.max_attempts {
            let outcome = client.settlement_evidence_unverified(request_id).await;
            match outcome {
                Ok(response) => {
                    let state = match validator.validate(&response.data) {
                        Ok(()) => RecoveryState::Verified(Box::new(response.data)),
                        Err(error) => RecoveryState::Failed(error.to_string()),
                    };
                    return (request_id, state);
                }
                Err(RelaySdkError::Api(failure))
                    if failure.status == 404 && attempt < policy.max_attempts => {}
                Err(error) => {
                    return (request_id, RecoveryState::Failed(error.to_string()));
                }
            }
            tokio::select! {
                () = shutdown.cancelled() => return (
                    request_id,
                    RecoveryState::Failed("settlement recovery stopped before completion".into()),
                ),
                () = tokio::time::sleep(delay) => {}
            }
            delay = delay.saturating_mul(2).min(policy.max_delay);
        }
        (
            request_id,
            RecoveryState::Failed(
                "settlement evidence did not become available before retry exhaustion".into(),
            ),
        )
    }

    fn set_state(&self, request_id: Uuid, state: RecoveryState) {
        self.states
            .write()
            .unwrap_or_else(|error| error.into_inner())
            .insert(request_id, state);
    }

    fn fail_pending(&self, message: &str) {
        let mut states = self
            .states
            .write()
            .unwrap_or_else(|error| error.into_inner());
        for state in states.values_mut() {
            if matches!(state, RecoveryState::Pending) {
                *state = RecoveryState::Failed(message.into());
            }
        }
    }
}

impl RecoveryQueue {
    fn set_state(&self, request_id: Uuid, state: RecoveryState) {
        self.states
            .write()
            .unwrap_or_else(|error| error.into_inner())
            .insert(request_id, state);
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use async_trait::async_trait;
    use bytes::Bytes;
    use futures::stream;
    use http::HeaderMap;
    use relay_contract::{DataEnvelope, Microdollars, SettlementReceiptProof};

    use super::*;
    use crate::{HttpRequest, HttpResponse, HttpTransport, RelayClientConfig};

    struct QueueTransport {
        responses: Mutex<VecDeque<HttpResponse>>,
        requests: Mutex<Vec<HttpRequest>>,
    }

    struct RoutingTransport {
        slow_id: Uuid,
        fast: SettlementEvidence,
        slow_attempts: AtomicUsize,
    }

    struct TestEvidenceValidator;

    impl SettlementEvidenceValidator for TestEvidenceValidator {
        fn validate(&self, _evidence: &SettlementEvidence) -> Result<(), EvidenceValidationError> {
            Ok(())
        }
    }

    #[async_trait]
    impl HttpTransport for RoutingTransport {
        async fn send(&self, request: HttpRequest) -> crate::Result<HttpResponse> {
            if request.url.path().ends_with(&self.slow_id.to_string()) {
                self.slow_attempts.fetch_add(1, Ordering::SeqCst);
                return Ok(response(
                    404,
                    br#"{"error":{"message":"settlement pending"}}"#.to_vec(),
                ));
            }
            Ok(response(
                200,
                serde_json::to_vec(&DataEnvelope {
                    data: self.fast.clone(),
                })
                .unwrap(),
            ))
        }
    }

    #[async_trait]
    impl HttpTransport for QueueTransport {
        async fn send(&self, request: HttpRequest) -> crate::Result<HttpResponse> {
            self.requests.lock().unwrap().push(request);
            self.responses
                .lock()
                .unwrap()
                .pop_front()
                .ok_or_else(|| RelaySdkError::Transport("no queued recovery response".into()))
        }
    }

    fn response(status: u16, body: Vec<u8>) -> HttpResponse {
        HttpResponse {
            status,
            headers: HeaderMap::new(),
            body: Box::pin(stream::iter([Ok(Bytes::from(body))])),
        }
    }

    fn evidence(request_id: Uuid) -> SettlementEvidence {
        SettlementEvidence {
            request_id,
            principal: "acct_owner".into(),
            receipt: SettlementReceiptProof {
                sequence: 7,
                operation: "hold.capture".into(),
                operation_hash: "11".repeat(32),
                amount_micros: Microdollars::new(42),
                balance_after_micros: None,
                prev_hash: None,
                receipt_hash: "22".repeat(32),
                signature: "33".repeat(64),
            },
            attestation: None,
            verified_at_ms: None,
            settled_at_ms: 1_700_000_000_000,
        }
    }

    #[tokio::test]
    async fn worker_polls_pending_evidence_then_verifies_it() {
        let request_id = Uuid::now_v7();
        let expected = evidence(request_id);
        let transport = Arc::new(QueueTransport {
            responses: Mutex::new(VecDeque::from([
                response(
                    404,
                    br#"{"error":{"message":"settlement pending"}}"#.to_vec(),
                ),
                response(
                    200,
                    serde_json::to_vec(&DataEnvelope {
                        data: expected.clone(),
                    })
                    .unwrap(),
                ),
            ])),
            requests: Mutex::new(Vec::new()),
        });
        let config = RelayClientConfig::builder("https://relay.test")
            .unwrap()
            .build()
            .unwrap();
        let client = RelayClient::with_transport(config, transport.clone());
        let (queue, worker) = RecoveryQueue::new(
            client,
            Arc::new(TestEvidenceValidator),
            RecoveryPolicy {
                max_attempts: 3,
                max_in_flight: 2,
                queue_capacity: 8,
                initial_delay: Duration::from_millis(1),
                max_delay: Duration::from_millis(2),
            },
        )
        .unwrap();
        let shutdown = CancellationToken::new();
        let observe = async {
            queue.abandoned(request_id);
            tokio::time::timeout(Duration::from_millis(100), async {
                loop {
                    if matches!(queue.state(request_id), Some(RecoveryState::Verified(_))) {
                        break;
                    }
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
            assert_eq!(
                queue.state(request_id),
                Some(RecoveryState::Verified(Box::new(expected)))
            );
            {
                let requests = transport.requests.lock().unwrap();
                assert_eq!(requests.len(), 2);
                assert!(
                    requests[0]
                        .url
                        .path()
                        .ends_with(&format!("/v1/billing/settlements/{request_id}"))
                );
            }
            shutdown.cancel();
        };
        tokio::join!(worker.run(shutdown.clone()), observe);
    }

    #[tokio::test]
    async fn slow_recovery_does_not_head_of_line_block_later_streams() {
        let slow_id = Uuid::now_v7();
        let fast_id = Uuid::now_v7();
        let expected = evidence(fast_id);
        let transport = Arc::new(RoutingTransport {
            slow_id,
            fast: expected.clone(),
            slow_attempts: AtomicUsize::new(0),
        });
        let config = RelayClientConfig::builder("https://relay.test")
            .unwrap()
            .build()
            .unwrap();
        let client = RelayClient::with_transport(config, transport.clone());
        let (queue, worker) = RecoveryQueue::new(
            client,
            Arc::new(TestEvidenceValidator),
            RecoveryPolicy {
                max_attempts: 100,
                max_in_flight: 2,
                queue_capacity: 8,
                initial_delay: Duration::from_millis(25),
                max_delay: Duration::from_millis(25),
            },
        )
        .unwrap();
        let shutdown = CancellationToken::new();
        let observe = async {
            queue.abandoned(slow_id);
            queue.abandoned(fast_id);
            tokio::time::timeout(Duration::from_millis(100), async {
                loop {
                    if matches!(queue.state(fast_id), Some(RecoveryState::Verified(_))) {
                        break;
                    }
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
            assert_eq!(
                queue.state(fast_id),
                Some(RecoveryState::Verified(Box::new(expected)))
            );
            assert!(transport.slow_attempts.load(Ordering::SeqCst) >= 1);
            shutdown.cancel();
        };
        tokio::join!(worker.run(shutdown.clone()), observe);
        assert!(matches!(
            queue.state(slow_id),
            Some(RecoveryState::Failed(_))
        ));
    }

    #[test]
    fn drop_queue_is_bounded_and_terminal_states_can_be_drained() {
        let transport = Arc::new(QueueTransport {
            responses: Mutex::new(VecDeque::new()),
            requests: Mutex::new(Vec::new()),
        });
        let config = RelayClientConfig::builder("https://relay.test")
            .unwrap()
            .build()
            .unwrap();
        let (queue, _worker) = RecoveryQueue::new(
            RelayClient::with_transport(config, transport),
            Arc::new(TestEvidenceValidator),
            RecoveryPolicy {
                max_attempts: 1,
                max_in_flight: 1,
                queue_capacity: 1,
                initial_delay: Duration::from_millis(1),
                max_delay: Duration::from_millis(1),
            },
        )
        .unwrap();
        let pending = Uuid::now_v7();
        let overflow = Uuid::now_v7();
        queue.abandoned(pending);
        queue.abandoned(overflow);
        assert_eq!(queue.take_terminal(pending), None);
        assert!(matches!(
            queue.take_terminal(overflow),
            Some(RecoveryState::Failed(message))
                if message == "settlement recovery queue capacity exceeded"
        ));
        assert_eq!(queue.state(overflow), None);
    }
}
