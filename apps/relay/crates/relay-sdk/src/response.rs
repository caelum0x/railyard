use std::collections::VecDeque;

use futures::StreamExt;
use http::HeaderMap;
use relay_contract::Usage;
use serde::de::DeserializeOwned;
use uuid::Uuid;

use crate::error::{RelaySdkError, Result};
use crate::http::HttpByteStream;
use crate::recovery::AbandonedStreamHandler;
use crate::sse::{SseDecoder, SseEvent};

#[derive(Debug, Clone, Default)]
pub struct RelayResponseMetadata {
    pub status: u16,
    pub request_id: Option<Uuid>,
    pub aitp_receipt: Option<String>,
    pub aitp_version: Option<String>,
    pub billing_overage: Option<String>,
    pub headers: HeaderMap,
}

#[derive(Debug, Clone)]
pub struct RelayResponse<T> {
    pub data: T,
    pub usage: Option<Usage>,
    pub metadata: RelayResponseMetadata,
}

pub struct RelayStream {
    metadata: RelayResponseMetadata,
    body: HttpByteStream,
    decoder: SseDecoder,
    pending: VecDeque<SseEvent>,
    finished: bool,
    terminal_usage: Option<Usage>,
    saw_done: bool,
    abandoned_streams: Option<std::sync::Arc<dyn AbandonedStreamHandler>>,
}

impl RelayStream {
    pub(crate) fn new(
        metadata: RelayResponseMetadata,
        body: HttpByteStream,
        max_event_bytes: usize,
        abandoned_streams: Option<std::sync::Arc<dyn AbandonedStreamHandler>>,
    ) -> Self {
        Self {
            metadata,
            body,
            decoder: SseDecoder::new(max_event_bytes),
            pending: VecDeque::new(),
            finished: false,
            terminal_usage: None,
            saw_done: false,
            abandoned_streams,
        }
    }

    pub fn metadata(&self) -> &RelayResponseMetadata {
        &self.metadata
    }

    pub fn terminal_usage(&self) -> Option<Usage> {
        self.terminal_usage
    }

    pub async fn next_event(&mut self) -> Result<Option<SseEvent>> {
        loop {
            if let Some(event) = self.pending.pop_front() {
                if let Some(data) = event.data.as_deref() {
                    if data == "[DONE]" {
                        self.saw_done = true;
                        self.finished = true;
                    } else if let Ok(value) = serde_json::from_str::<serde_json::Value>(data)
                        && let Some(usage) = Usage::from_response(&value)
                            .map_err(|error| RelaySdkError::InvalidJson(error.to_string()))?
                    {
                        self.terminal_usage = Some(usage);
                    }
                }
                return Ok(Some(event));
            }
            if self.finished {
                return Ok(None);
            }
            match self.body.next().await {
                Some(chunk) => {
                    let events = self.decoder.push(&chunk?)?;
                    self.pending.extend(events);
                }
                None => {
                    let events = self.decoder.finish()?;
                    self.pending.extend(events);
                    self.finished = self.pending.is_empty();
                    if self.finished {
                        return Ok(None);
                    }
                }
            }
        }
    }

    pub async fn next_json<T: DeserializeOwned>(&mut self) -> Result<Option<T>> {
        while let Some(event) = self.next_event().await? {
            let Some(data) = event.data else {
                continue;
            };
            if data == "[DONE]" {
                return Ok(None);
            }
            return serde_json::from_str(&data)
                .map(Some)
                .map_err(|error| RelaySdkError::InvalidJson(error.to_string()));
        }
        Ok(None)
    }
}

impl Drop for RelayStream {
    fn drop(&mut self) {
        if self.saw_done {
            return;
        }
        if let Some(request_id) = self.metadata.request_id
            && let Some(handler) = &self.abandoned_streams
        {
            handler.abandoned(request_id);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use bytes::Bytes;
    use futures::stream;

    use super::*;

    #[derive(Default)]
    struct RecordingHandler(Mutex<Vec<Uuid>>);

    impl AbandonedStreamHandler for RecordingHandler {
        fn abandoned(&self, request_id: Uuid) {
            self.0.lock().unwrap().push(request_id);
        }
    }

    fn stream_with(
        request_id: Uuid,
        body: &'static [u8],
        handler: Arc<RecordingHandler>,
    ) -> RelayStream {
        RelayStream::new(
            RelayResponseMetadata {
                request_id: Some(request_id),
                ..RelayResponseMetadata::default()
            },
            Box::pin(stream::once(async move { Ok(Bytes::from_static(body)) })),
            64 * 1024,
            Some(handler),
        )
    }

    #[test]
    fn dropping_before_done_enqueues_recovery_without_network_io() {
        let request_id = Uuid::now_v7();
        let handler = Arc::new(RecordingHandler::default());
        drop(stream_with(
            request_id,
            b"data: {\"delta\":1}\n\n",
            handler.clone(),
        ));
        assert_eq!(*handler.0.lock().unwrap(), vec![request_id]);
    }

    #[tokio::test]
    async fn observing_done_suppresses_recovery() {
        let request_id = Uuid::now_v7();
        let handler = Arc::new(RecordingHandler::default());
        let mut response = stream_with(request_id, b"data: [DONE]\n\n", handler.clone());
        assert!(response.next_event().await.unwrap().is_some());
        drop(response);
        assert!(handler.0.lock().unwrap().is_empty());
    }
}
