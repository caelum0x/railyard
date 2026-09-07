use std::time::{SystemTime, UNIX_EPOCH};

pub trait Clock: Send + Sync {
    fn now_ms(&self) -> i64;
}

#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now_ms(&self) -> i64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |duration| {
                i64::try_from(duration.as_millis()).unwrap_or(i64::MAX)
            })
    }
}

pub trait NonceSource: Send + Sync {
    fn nonce(&self) -> String;
}

#[derive(Debug, Clone, Copy, Default)]
pub struct UuidNonceSource;

impl NonceSource for UuidNonceSource {
    fn nonce(&self) -> String {
        uuid::Uuid::now_v7().to_string()
    }
}
