//! # switchyard-cache
//!
//! - [`ResponseCache`]: an in-process TTL cache with a byte budget
//!   (`SWITCHYARD_CACHE_MB`, default 32). Entries are keyed on the normalised
//!   request hash; the oldest entries are evicted first when over budget.
//! - [`SingleFlight`]: identical concurrent calls share one upstream execution.

use std::collections::{HashMap, VecDeque};
use std::future::Future;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::Value;
use tokio::sync::OnceCell;

#[derive(Debug, Clone, PartialEq)]
pub struct CachedResponse {
    pub tool_id: String,
    pub provider: String,
    pub data: Value,
}

struct Entry {
    value: Arc<CachedResponse>,
    expires: Instant,
    size: usize,
}

struct Inner {
    map: HashMap<String, Entry>,
    order: VecDeque<String>,
    bytes: usize,
}

pub struct ResponseCache {
    inner: Mutex<Inner>,
    budget: usize,
}

impl ResponseCache {
    pub fn new(budget_bytes: usize) -> Self {
        ResponseCache { inner: Mutex::new(Inner { map: HashMap::new(), order: VecDeque::new(), bytes: 0 }), budget: budget_bytes }
    }

    pub fn get(&self, key: &str) -> Option<Arc<CachedResponse>> {
        let mut inner = self.inner.lock().expect("cache poisoned");
        let now = Instant::now();
        match inner.map.get(key) {
            Some(e) if e.expires > now => Some(e.value.clone()),
            Some(_) => {
                if let Some(e) = inner.map.remove(key) {
                    inner.bytes -= e.size;
                }
                None
            }
            None => None,
        }
    }

    pub fn put(&self, key: &str, value: CachedResponse, ttl: Duration) {
        if ttl.is_zero() {
            return;
        }
        let size = value.data.to_string().len() + key.len() + value.tool_id.len() + value.provider.len() + 64;
        if size > self.budget {
            return;
        }
        let mut inner = self.inner.lock().expect("cache poisoned");
        if let Some(old) = inner.map.remove(key) {
            inner.bytes -= old.size;
        }
        while inner.bytes + size > self.budget {
            let Some(oldest) = inner.order.pop_front() else { break };
            if let Some(e) = inner.map.remove(&oldest) {
                inner.bytes -= e.size;
            }
        }
        inner.order.retain(|k| k != key);
        inner.order.push_back(key.to_string());
        inner.bytes += size;
        inner.map.insert(key.to_string(), Entry { value: Arc::new(value), expires: Instant::now() + ttl, size });
    }

    pub fn bytes(&self) -> usize {
        self.inner.lock().expect("cache poisoned").bytes
    }

    pub fn len(&self) -> usize {
        self.inner.lock().expect("cache poisoned").map.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Collapses identical concurrent work onto one execution.
pub struct SingleFlight<T: Clone + Send + Sync + 'static> {
    inflight: Mutex<HashMap<String, Arc<OnceCell<T>>>>,
}

impl<T: Clone + Send + Sync + 'static> Default for SingleFlight<T> {
    fn default() -> Self {
        SingleFlight { inflight: Mutex::new(HashMap::new()) }
    }
}

impl<T: Clone + Send + Sync + 'static> SingleFlight<T> {
    /// Run `work` unless an identical `key` is already running, in which case
    /// wait for and share its result. Returns `(value, shared)`; `shared` is
    /// true when this caller did not execute the work itself.
    pub async fn run<F, Fut>(&self, key: &str, work: F) -> (T, bool)
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = T>,
    {
        let cell = {
            let mut map = self.inflight.lock().expect("single-flight poisoned");
            map.entry(key.to_string()).or_insert_with(|| Arc::new(OnceCell::new())).clone()
        };
        let ran = AtomicBool::new(false);
        let value = cell
            .get_or_init(|| async {
                ran.store(true, Ordering::SeqCst);
                work().await
            })
            .await
            .clone();
        {
            let mut map = self.inflight.lock().expect("single-flight poisoned");
            if map.get(key).map(|c| Arc::ptr_eq(c, &cell)).unwrap_or(false) {
                map.remove(key);
            }
        }
        (value, !ran.load(Ordering::SeqCst))
    }

    pub fn in_flight(&self) -> usize {
        self.inflight.lock().expect("single-flight poisoned").len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::atomic::AtomicUsize;

    fn resp(v: Value) -> CachedResponse {
        CachedResponse { tool_id: "t".into(), provider: "p".into(), data: v }
    }

    #[test]
    fn get_put_and_expiry() {
        let c = ResponseCache::new(1 << 20);
        c.put("k", resp(json!({"a": 1})), Duration::from_secs(60));
        assert_eq!(c.get("k").unwrap().data, json!({"a": 1}));
        c.put("short", resp(json!(1)), Duration::from_millis(1));
        std::thread::sleep(Duration::from_millis(5));
        assert!(c.get("short").is_none());
        c.put("zero", resp(json!(1)), Duration::ZERO);
        assert!(c.get("zero").is_none(), "ttl 0 means uncached");
    }

    #[test]
    fn budget_evicts_oldest() {
        let c = ResponseCache::new(600);
        for i in 0..10 {
            c.put(&format!("k{i}"), resp(json!("x".repeat(100))), Duration::from_secs(60));
        }
        assert!(c.bytes() <= 600);
        assert!(c.get("k0").is_none(), "oldest evicted");
        assert!(c.get("k9").is_some(), "newest kept");
        c.put("huge", resp(json!("y".repeat(10_000))), Duration::from_secs(60));
        assert!(c.get("huge").is_none(), "over-budget values are never stored");
    }

    #[tokio::test]
    async fn single_flight_runs_once_for_concurrent_callers() {
        let sf: Arc<SingleFlight<u32>> = Arc::new(SingleFlight::default());
        let hits = Arc::new(AtomicUsize::new(0));
        let mut handles = Vec::new();
        for _ in 0..20 {
            let sf = sf.clone();
            let hits = hits.clone();
            handles.push(tokio::spawn(async move {
                sf.run("same", || async move {
                    hits.fetch_add(1, Ordering::SeqCst);
                    tokio::time::sleep(Duration::from_millis(50)).await;
                    7
                })
                .await
            }));
        }
        let mut shared = 0;
        for h in handles {
            let (v, s) = h.await.unwrap();
            assert_eq!(v, 7);
            shared += s as usize;
        }
        assert_eq!(hits.load(Ordering::SeqCst), 1);
        assert_eq!(shared, 19);
        assert_eq!(sf.in_flight(), 0, "entry removed after completion");
        // A later call runs fresh work.
        let (v, s) = sf.run("same", || async { 8 }).await;
        assert_eq!((v, s), (8, false));
    }
}
