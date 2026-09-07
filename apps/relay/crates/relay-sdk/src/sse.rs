use crate::error::{RelaySdkError, Result};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SseEvent {
    pub event: Option<String>,
    pub data: Option<String>,
    pub id: Option<String>,
    pub retry_ms: Option<u64>,
}

pub struct SseDecoder {
    buffer: Vec<u8>,
    max_event_bytes: usize,
}

impl SseDecoder {
    pub fn new(max_event_bytes: usize) -> Self {
        Self {
            buffer: Vec::new(),
            max_event_bytes,
        }
    }

    pub fn push(&mut self, bytes: &[u8]) -> Result<Vec<SseEvent>> {
        self.buffer.extend_from_slice(bytes);
        if self.buffer.len() > self.max_event_bytes && find_delimiter(&self.buffer).is_none() {
            return Err(RelaySdkError::SseEventTooLarge {
                limit: self.max_event_bytes,
            });
        }
        self.drain_complete_events()
    }

    pub fn finish(&mut self) -> Result<Vec<SseEvent>> {
        let mut events = self.drain_complete_events()?;
        if self.buffer.iter().all(u8::is_ascii_whitespace) {
            self.buffer.clear();
            return Ok(events);
        }
        if self.buffer.len() > self.max_event_bytes {
            return Err(RelaySdkError::SseEventTooLarge {
                limit: self.max_event_bytes,
            });
        }
        let remaining = std::mem::take(&mut self.buffer);
        events.push(parse_event(&remaining)?);
        Ok(events)
    }

    fn drain_complete_events(&mut self) -> Result<Vec<SseEvent>> {
        let mut events = Vec::new();
        while let Some((index, delimiter_len)) = find_delimiter(&self.buffer) {
            if index > self.max_event_bytes {
                return Err(RelaySdkError::SseEventTooLarge {
                    limit: self.max_event_bytes,
                });
            }
            let frame = self.buffer[..index].to_vec();
            self.buffer.drain(..index + delimiter_len);
            if !frame.is_empty() {
                events.push(parse_event(&frame)?);
            }
        }
        Ok(events)
    }
}

fn find_delimiter(bytes: &[u8]) -> Option<(usize, usize)> {
    let lf = bytes.windows(2).position(|window| window == b"\n\n");
    let crlf = bytes.windows(4).position(|window| window == b"\r\n\r\n");
    match (lf, crlf) {
        (Some(left), Some(right)) if left <= right => Some((left, 2)),
        (Some(_), Some(right)) => Some((right, 4)),
        (Some(left), None) => Some((left, 2)),
        (None, Some(right)) => Some((right, 4)),
        (None, None) => None,
    }
}

fn parse_event(bytes: &[u8]) -> Result<SseEvent> {
    let text = std::str::from_utf8(bytes)
        .map_err(|error| RelaySdkError::InvalidJson(format!("SSE is not UTF-8: {error}")))?;
    let mut event = SseEvent::default();
    let mut data = Vec::new();
    for line in text.lines() {
        let line = line.strip_suffix('\r').unwrap_or(line);
        if line.starts_with(':') {
            continue;
        }
        let (field, value) = line.split_once(':').map_or((line, ""), |(field, value)| {
            (field, value.strip_prefix(' ').unwrap_or(value))
        });
        match field {
            "event" => event.event = Some(value.into()),
            "data" => data.push(value),
            "id" if !value.contains('\0') => event.id = Some(value.into()),
            "retry" => event.retry_ms = value.parse().ok(),
            _ => {}
        }
    }
    if !data.is_empty() {
        event.data = Some(data.join("\n"));
    }
    Ok(event)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decoder_handles_arbitrary_chunks_crlf_and_multiline_data() {
        let mut decoder = SseDecoder::new(1024);
        assert!(decoder.push(b"id: 7\r\ndata: hel").unwrap().is_empty());
        let events = decoder
            .push(b"lo\r\ndata: world\r\nevent: message\r\n\r\n")
            .unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].id.as_deref(), Some("7"));
        assert_eq!(events[0].data.as_deref(), Some("hello\nworld"));
        assert_eq!(events[0].event.as_deref(), Some("message"));
    }

    #[test]
    fn decoder_enforces_per_event_limit() {
        let mut decoder = SseDecoder::new(4);
        assert!(matches!(
            decoder.push(b"data: too long"),
            Err(RelaySdkError::SseEventTooLarge { limit: 4 })
        ));
    }
}
