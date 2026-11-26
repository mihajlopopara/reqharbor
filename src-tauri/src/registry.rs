//! Small in-memory registries used by the commands, kept free of Tauri types
//! so they can be unit-tested: the in-flight send registry (cancel), the
//! response body cache and the debounce/retry generation counter.

use reqharbor_core::CancellationToken;
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

/// executionId -> cancel token of an in-flight send (AC-REQ-8).
#[derive(Default)]
pub struct CancelRegistry {
    serial: AtomicU64,
    inflight: Mutex<HashMap<String, (u64, CancellationToken)>>,
}

/// Handle returned by [`CancelRegistry::register`].
pub struct InFlight {
    pub serial: u64,
    pub token: CancellationToken,
}

impl CancelRegistry {
    /// Register a send and get its token. Re-using an id cancels the
    /// previous send with that id.
    pub fn register(&self, id: &str) -> InFlight {
        let token = CancellationToken::new();
        let serial = self.serial.fetch_add(1, Ordering::SeqCst) + 1;
        if let Some((_, old)) = lock(&self.inflight).insert(id.to_string(), (serial, token.clone()))
        {
            old.cancel();
        }
        InFlight { serial, token }
    }

    /// Cancel a send. Returns false when nothing is in flight under `id`.
    pub fn cancel(&self, id: &str) -> bool {
        match lock(&self.inflight).get(id) {
            Some((_, t)) => {
                t.cancel();
                true
            }
            None => false,
        }
    }

    /// Forget a finished send (unless a newer send re-used the id).
    pub fn finish(&self, id: &str, handle: &InFlight) {
        let mut map = lock(&self.inflight);
        if map
            .get(id)
            .is_some_and(|(serial, _)| *serial == handle.serial)
        {
            map.remove(id);
        }
    }

    #[cfg(test)]
    fn in_flight_count(&self) -> usize {
        lock(&self.inflight).len()
    }
}

/// The last `capacity` response bodies, for `get_response_body`.
pub struct ResponseCache {
    capacity: usize,
    entries: Mutex<VecDeque<(String, Arc<Vec<u8>>)>>,
}

impl ResponseCache {
    pub fn new(capacity: usize) -> Self {
        ResponseCache {
            capacity: capacity.max(1),
            entries: Mutex::new(VecDeque::new()),
        }
    }

    pub fn put(&self, id: String, body: Vec<u8>) {
        let mut cache = lock(&self.entries);
        cache.retain(|(k, _)| *k != id);
        cache.push_back((id, Arc::new(body)));
        while cache.len() > self.capacity {
            cache.pop_front();
        }
    }

    pub fn get(&self, id: &str) -> Option<Arc<Vec<u8>>> {
        lock(&self.entries)
            .iter()
            .find(|(k, _)| k == id)
            .map(|(_, b)| b.clone())
    }

    /// The first `limit` bytes (all when `None`) — AC-RES-3.
    pub fn slice(&self, id: &str, limit: Option<usize>) -> Option<Vec<u8>> {
        let body = self.get(id)?;
        let end = limit.map(|l| l.min(body.len())).unwrap_or(body.len());
        Some(body[..end].to_vec())
    }
}

/// Generation counter for debounced / superseded background tasks: a task
/// armed with generation `g` only runs if nothing re-armed meanwhile.
#[derive(Default)]
pub struct Debouncer(AtomicU64);

impl Debouncer {
    pub fn arm(&self) -> u64 {
        self.0.fetch_add(1, Ordering::SeqCst) + 1
    }

    pub fn is_current(&self, generation: u64) -> bool {
        self.0.load(Ordering::SeqCst) == generation
    }

    /// Invalidate every armed task.
    pub fn cancel(&self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ac_req_8_cancel_registry() {
        let r = CancelRegistry::default();
        let h = r.register("e1");
        assert!(!h.token.is_cancelled());
        assert!(r.cancel("e1"));
        assert!(h.token.is_cancelled());
        assert!(!r.cancel("nope"));
        r.finish("e1", &h);
        assert_eq!(r.in_flight_count(), 0);
        assert!(!r.cancel("e1"), "finished sends cannot be cancelled");
    }

    #[test]
    fn reusing_an_execution_id_cancels_the_previous_send() {
        let r = CancelRegistry::default();
        let first = r.register("x");
        let second = r.register("x");
        assert!(first.token.is_cancelled());
        assert!(!second.token.is_cancelled());
        // the stale handle does not unregister the newer send
        r.finish("x", &first);
        assert_eq!(r.in_flight_count(), 1);
        r.finish("x", &second);
        assert_eq!(r.in_flight_count(), 0);
    }

    #[test]
    fn ac_res_3_response_cache_keeps_last_n_and_slices() {
        let c = ResponseCache::new(2);
        c.put("a".into(), vec![1, 2, 3, 4]);
        c.put("b".into(), vec![5]);
        assert_eq!(c.slice("a", Some(2)), Some(vec![1, 2]));
        assert_eq!(c.slice("a", Some(99)), Some(vec![1, 2, 3, 4]));
        assert_eq!(c.slice("a", None), Some(vec![1, 2, 3, 4]));
        c.put("c".into(), vec![6]);
        assert!(c.get("a").is_none(), "oldest evicted");
        assert!(c.get("b").is_some() && c.get("c").is_some());
        // re-putting refreshes recency
        c.put("b".into(), vec![7]);
        c.put("d".into(), vec![8]);
        assert_eq!(c.get("b").as_deref(), Some(&vec![7]));
        assert!(c.get("c").is_none());
    }

    #[test]
    fn ac_sync_2_debouncer_only_latest_generation_runs() {
        let d = Debouncer::default();
        let g1 = d.arm();
        let g2 = d.arm();
        assert!(!d.is_current(g1));
        assert!(d.is_current(g2));
        d.cancel();
        assert!(!d.is_current(g2));
    }
}
