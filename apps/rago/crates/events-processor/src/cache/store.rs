//! Port of `cache/cache.go` — the BadgerDB-backed in-memory JSON KV store.
//!
//! Faithful-port decision: Badger → **redb** with an in-memory backend (pure
//! Rust embedded KV). Badger's native per-key TTL has no redb equivalent, so we
//! store a small envelope `[expires_at_millis: i64 LE][json bytes]`; reads and
//! prefix scans transparently skip entries whose TTL has elapsed, reproducing
//! Badger's lazy expiry (used for the 30-day terminated-subscription window).

use redb::{Database, ReadableDatabase, TableDefinition};
use serde::de::DeserializeOwned;
use serde::Serialize;

use crate::utils::result::Result;
use crate::utils::RagoError;

const TABLE: TableDefinition<&str, &[u8]> = TableDefinition::new("cache");

pub struct Cache {
    pub(crate) db: Database,
    // Consumed by the Debezium CDC consumers (feature `memcache`).
    #[cfg_attr(not(feature = "memcache"), allow(dead_code))]
    pub(crate) debezium_topic_prefix: String,
}

fn now_millis() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

fn encode(expires_at_millis: i64, data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(8 + data.len());
    out.extend_from_slice(&expires_at_millis.to_le_bytes());
    out.extend_from_slice(data);
    out
}

/// Returns the JSON payload if present and not expired.
fn decode_live(raw: &[u8]) -> Option<&[u8]> {
    if raw.len() < 8 {
        return None;
    }
    let expires_at = i64::from_le_bytes(raw[0..8].try_into().unwrap());
    if expires_at != 0 && now_millis() > expires_at {
        return None;
    }
    Some(&raw[8..])
}

impl Cache {
    /// `NewCache` — an in-memory redb database.
    pub fn new(debezium_topic_prefix: String) -> anyhow::Result<Cache> {
        let db = Database::builder().create_with_backend(redb::backends::InMemoryBackend::new())?;
        // Ensure the table exists.
        let wtxn = db.begin_write()?;
        {
            let _ = wtxn.open_table(TABLE)?;
        }
        wtxn.commit()?;
        Ok(Cache {
            db,
            debezium_topic_prefix,
        })
    }

    /// `setJSON`.
    pub fn set_json<T: Serialize>(&self, key: &str, value: &T) -> Result<bool> {
        let data = match serde_json::to_vec(value) {
            Ok(d) => d,
            Err(e) => return Result::failed(RagoError::from(e)),
        };
        self.put_raw(key, encode(0, &data))
    }

    /// `deleteWithTTL` — store with a delayed expiry.
    pub fn set_json_with_ttl<T: Serialize>(
        &self,
        key: &str,
        value: &T,
        ttl: chrono::Duration,
    ) -> Result<bool> {
        let data = match serde_json::to_vec(value) {
            Ok(d) => d,
            Err(e) => return Result::failed(RagoError::from(e)),
        };
        let expires_at = now_millis() + ttl.num_milliseconds();
        self.put_raw(key, encode(expires_at, &data))
    }

    fn put_raw(&self, key: &str, value: Vec<u8>) -> Result<bool> {
        let write = || -> anyhow::Result<()> {
            let wtxn = self.db.begin_write()?;
            {
                let mut table = wtxn.open_table(TABLE)?;
                table.insert(key, value.as_slice())?;
            }
            wtxn.commit()?;
            Ok(())
        };
        match write() {
            Ok(()) => Result::success(true),
            Err(e) => Result::failed(RagoError::Message(e.to_string())),
        }
    }

    /// `delete`.
    pub fn delete(&self, key: &str) -> Result<bool> {
        let del = || -> anyhow::Result<()> {
            let wtxn = self.db.begin_write()?;
            {
                let mut table = wtxn.open_table(TABLE)?;
                table.remove(key)?;
            }
            wtxn.commit()?;
            Ok(())
        };
        match del() {
            Ok(()) => Result::success(true),
            Err(e) => Result::failed(RagoError::Message(e.to_string())),
        }
    }

    /// `getJSON` — a missing (or expired) key is a non-capturable,
    /// non-retryable "record not found", matching `badger.ErrKeyNotFound`.
    pub fn get_json<T: DeserializeOwned>(&self, key: &str) -> Result<T> {
        let read = || -> anyhow::Result<Option<Vec<u8>>> {
            let rtxn = self.db.begin_read()?;
            let table = rtxn.open_table(TABLE)?;
            match table.get(key)? {
                Some(v) => Ok(decode_live(v.value()).map(|d| d.to_vec())),
                None => Ok(None),
            }
        };

        match read() {
            Ok(Some(bytes)) => match serde_json::from_slice::<T>(&bytes) {
                Ok(v) => Result::success(v),
                Err(e) => Result::failed(RagoError::from(e)),
            },
            Ok(None) => Result::failed(RagoError::RecordNotFound)
                .non_capturable()
                .non_retryable(),
            Err(e) => Result::failed(RagoError::Message(e.to_string())),
        }
    }

    /// `searchJSON` — collect all live values whose key starts with `prefix`.
    pub fn search_json<T: DeserializeOwned>(&self, prefix: &str) -> Result<Vec<T>> {
        let scan = || -> anyhow::Result<Vec<Vec<u8>>> {
            let rtxn = self.db.begin_read()?;
            let table = rtxn.open_table(TABLE)?;
            let mut out = Vec::new();
            for entry in table.range(prefix..)? {
                let (k, v) = entry?;
                if !k.value().starts_with(prefix) {
                    break;
                }
                if let Some(data) = decode_live(v.value()) {
                    out.push(data.to_vec());
                }
            }
            Ok(out)
        };

        match scan() {
            Ok(items) => {
                let mut results = Vec::with_capacity(items.len());
                for bytes in items {
                    match serde_json::from_slice::<T>(&bytes) {
                        Ok(v) => results.push(v),
                        Err(e) => return Result::failed(RagoError::from(e)),
                    }
                }
                Result::success(results)
            }
            Err(e) => Result::failed(RagoError::Message(e.to_string())),
        }
    }
}
