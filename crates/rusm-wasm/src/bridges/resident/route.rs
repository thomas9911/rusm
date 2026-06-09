//! Routing a request/connection to a resident instance (see [`super`]): the shard
//! policy + an optional per-instance in-flight permit, the single decision shared by
//! the resident HTTP and WS servers.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use rusm_otp::{Pid, Runtime};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

use super::pool::ResidentPool;

/// How a resident server spreads requests/connections across its pool.
#[derive(Clone)]
enum Shard {
    /// Round-robin across the pool (the default).
    RoundRobin,
    /// Pin by a request header value: the same value always maps to the same
    /// instance (session affinity), so per-key state lives on one instance.
    Header(String),
}

impl Shard {
    /// Parse a `shard_by` config spec: `"header:<name>"` → header affinity; `None`
    /// (or an unrecognised spec) → round-robin.
    fn parse(spec: Option<&str>) -> Self {
        match spec.and_then(|s| s.strip_prefix("header:")) {
            Some(name) => Shard::Header(name.trim().to_ascii_lowercase()),
            None => Shard::RoundRobin,
        }
    }
}

/// The single routing decision over a [`ResidentPool`], shared by the HTTP and WS
/// resident servers: pick an instance (round-robin or header affinity) and, if a
/// `max_inflight` limit is set, take a per-instance permit — held by the returned
/// [`Lease`] for the request/connection's lifetime, so an overloaded instance sheds.
#[derive(Clone)]
pub(crate) struct ResidentRoute {
    pool: ResidentPool,
    shard: Shard,
    next: Arc<AtomicUsize>,
    /// Per-instance in-flight permits; `None` = unbounded. A permit is held by the
    /// [`Lease`] until the request/connection completes, so this bounds concurrent
    /// in-flight work per instance (always-on — no runtime opt-in needed).
    inflight: Option<Arc<Vec<Arc<Semaphore>>>>,
}

/// Holds the routed instance and (when `max_inflight` is set) its in-flight permit;
/// dropping it releases the permit. Keep it for the request/connection's lifetime.
pub(crate) struct Lease {
    pub(crate) pid: Pid,
    _permit: Option<OwnedSemaphorePermit>,
}

impl ResidentRoute {
    pub(crate) fn new(pool: ResidentPool) -> Self {
        Self {
            pool,
            shard: Shard::RoundRobin,
            next: Arc::new(AtomicUsize::new(0)),
            inflight: None,
        }
    }

    /// Set the shard policy from a `shard_by` spec.
    pub(crate) fn shard_by(&mut self, spec: Option<&str>) {
        self.shard = Shard::parse(spec);
    }

    /// Bound concurrent in-flight requests/connections per instance to `limit`;
    /// excess sheds to 503 / a refused upgrade.
    pub(crate) fn max_inflight(&mut self, limit: usize) {
        let sems = (0..self.pool.len())
            .map(|_| Arc::new(Semaphore::new(limit)))
            .collect();
        self.inflight = Some(Arc::new(sems));
    }

    /// The slot a request/connection routes to (round-robin or header affinity).
    fn slot_index(&self, headers: &hyper::HeaderMap) -> usize {
        let n = self.pool.len();
        match &self.shard {
            Shard::RoundRobin => self.next.fetch_add(1, Ordering::Relaxed) % n,
            Shard::Header(name) => {
                let key = headers
                    .get(name)
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or("");
                let mut hasher = DefaultHasher::new();
                key.hash(&mut hasher);
                (hasher.finish() % n as u64) as usize
            }
        }
    }

    /// Route to an instance and take its in-flight permit; `None` if the instance is
    /// absent (mid-restart) or saturated — the caller turns that into a 503.
    pub(crate) fn route(&self, headers: &hyper::HeaderMap) -> Option<Lease> {
        let i = self.slot_index(headers);
        let pid = self.pool.whereis(i)?;
        let permit = match &self.inflight {
            Some(sems) => match Arc::clone(&sems[i]).try_acquire_owned() {
                Ok(permit) => Some(permit),
                Err(_) => return None, // saturated — shed
            },
            None => None,
        };
        Some(Lease {
            pid,
            _permit: permit,
        })
    }

    pub(crate) fn runtime(&self) -> &Runtime {
        self.pool.runtime()
    }

    pub(crate) async fn ready(&self) {
        self.pool.ready().await
    }

    pub(crate) fn pids(&self) -> Vec<Pid> {
        self.pool.pids()
    }

    /// The live pid in slot `i` (introspection / tests).
    pub(crate) fn slot_pid(&self, i: usize) -> Option<Pid> {
        self.pool.whereis(i)
    }
}
