//! Byte-bounded LRU of *sealed* chunks with single-flight loading.
//!
//! Ciphertext is cached, not plaintext, so a memory dump of the cache reveals
//! nothing without the data keys. Concurrent readers of the same chunk (a video
//! player opening several ranges at once) share one backend download.

use crate::backend::ChunkBackend;
use bytes::Bytes;
use futures::future::{BoxFuture, FutureExt, Shared};
use lru::LruCache;
use parking_lot::Mutex;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

type Loading = Shared<BoxFuture<'static, Result<Bytes, String>>>;

pub struct ChunkCache {
    backend: Arc<dyn ChunkBackend>,
    max_bytes: usize,
    inner: Mutex<(LruCache<String, Bytes>, usize)>,
    inflight: Mutex<HashMap<String, Loading>>,
    pub hits: AtomicU64,
    pub misses: AtomicU64,
}

impl ChunkCache {
    pub fn new(backend: Arc<dyn ChunkBackend>, max_bytes: usize) -> Arc<Self> {
        Arc::new(Self {
            backend,
            max_bytes,
            inner: Mutex::new((LruCache::unbounded(), 0)),
            inflight: Mutex::new(HashMap::new()),
            hits: AtomicU64::new(0),
            misses: AtomicU64::new(0),
        })
    }

    pub fn backend(&self) -> &Arc<dyn ChunkBackend> {
        &self.backend
    }

    fn lookup(&self, locator: &str) -> Option<Bytes> {
        self.inner.lock().0.get(locator).cloned()
    }

    fn insert(&self, locator: String, data: Bytes) {
        if data.len() > self.max_bytes / 2 {
            return; // one chunk must not flush the whole cache
        }
        let mut g = self.inner.lock();
        let (lru, used) = &mut *g;
        if let Some(old) = lru.put(locator, data.clone()) {
            *used -= old.len();
        }
        *used += data.len();
        while *used > self.max_bytes {
            match lru.pop_lru() {
                Some((_, v)) => *used -= v.len(),
                None => break,
            }
        }
    }

    pub fn forget(&self, locator: &str) {
        let mut g = self.inner.lock();
        if let Some(v) = g.0.pop(locator) {
            g.1 -= v.len();
        }
    }

    pub async fn get(self: &Arc<Self>, locator: &str) -> Result<Bytes, String> {
        if let Some(b) = self.lookup(locator) {
            self.hits.fetch_add(1, Ordering::Relaxed);
            return Ok(b);
        }
        self.misses.fetch_add(1, Ordering::Relaxed);
        let fut = {
            let mut inflight = self.inflight.lock();
            if let Some(f) = inflight.get(locator) {
                f.clone()
            } else {
                let this = self.clone();
                let key = locator.to_string();
                let f = async move {
                    let r = this.backend.get(&key).await.map_err(|e| format!("{e:#}"));
                    if let Ok(b) = &r {
                        this.insert(key.clone(), b.clone());
                    }
                    this.inflight.lock().remove(&key);
                    r
                }
                .boxed()
                .shared();
                inflight.insert(locator.to_string(), f.clone());
                f
            }
        };
        fut.await
    }

    /// Warm the cache for the chunk a sequential reader will need next.
    pub fn prefetch(self: &Arc<Self>, locator: String) {
        if self.lookup(&locator).is_some() || self.inflight.lock().contains_key(&locator) {
            return;
        }
        let this = self.clone();
        tokio::spawn(async move {
            let _ = this.get(&locator).await;
        });
    }

    pub fn stats(&self) -> serde_json::Value {
        let g = self.inner.lock();
        serde_json::json!({
            "entries": g.0.len(),
            "bytes": g.1,
            "max_bytes": self.max_bytes,
            "hits": self.hits.load(Ordering::Relaxed),
            "misses": self.misses.load(Ordering::Relaxed),
        })
    }
}
