//! The supervised instance pool behind a resident server (see [`super`]).

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use rusm_otp::{Pid, ProcessHandle, Runtime, Strategy};

use crate::bridges::wasip2::PreparedComponent;
use crate::caps::Capabilities;
use crate::Spawner;

/// Namespaces slot names so independent pools never collide.
static POOL_SEQ: AtomicU64 = AtomicU64::new(0);

/// A pool of long-lived resident component instances, each under its **own**
/// one-for-one supervisor and addressed by a registry slot name. Per-instance
/// supervision means one crash-looping instance is isolated — it never trips a
/// shared budget and takes down healthy siblings. Cheap to clone (`Arc`/`Runtime`).
#[derive(Clone)]
pub(crate) struct ResidentPool {
    rt: Runtime,
    /// One registry name per instance; routing resolves a slot to its live pid, so a
    /// restarted instance (new pid) is found without bookkeeping.
    slots: Arc<Vec<String>>,
    /// One supervisor per instance (held so they aren't dropped), each with its own
    /// independent restart budget.
    _supervisors: Arc<Vec<ProcessHandle>>,
}

impl ResidentPool {
    /// Spawn `instances` (≥1) instances of `prepared`, each under its own one-for-one
    /// supervisor that restarts only that instance; each registers a slot name (and,
    /// for a JS runner, is fed `bundle` as its first message).
    pub(crate) fn spawn(
        spawner: &Arc<Spawner>,
        prepared: PreparedComponent,
        caps: Capabilities,
        bundle: Option<Arc<Vec<u8>>>,
        instances: usize,
    ) -> Self {
        let rt = spawner.rt.clone();
        let n = instances.max(1);
        let uid = POOL_SEQ.fetch_add(1, Ordering::Relaxed);
        let slots: Vec<String> = (0..n).map(|i| format!("__resident.{uid}.{i}")).collect();

        let supervisors: Vec<ProcessHandle> = slots
            .iter()
            .map(|slot| {
                let spawner = Arc::clone(spawner);
                let prepared = prepared.clone();
                let caps = caps.clone();
                let bundle = bundle.clone();
                let slot = slot.clone();
                // Each instance is its own supervised child — an isolated restart budget.
                rt.supervisor(Strategy::OneForOne)
                    .child(move |rt: &Runtime| {
                        let handle = spawner.spawn_component(&prepared, caps.clone());
                        if let Some(bundle) = &bundle {
                            rt.send(handle.pid(), (**bundle).clone()); // js-runner: bundle first
                        }
                        // Register the slot so routing always finds the *current*
                        // instance, even after a restart gives it a new pid. The dead
                        // instance released this name before its `Down` reached the
                        // supervisor, so a restart can't clash on it.
                        rt.register(slot.clone(), handle.pid());
                        handle
                    })
                    .start()
            })
            .collect();

        ResidentPool {
            rt,
            slots: Arc::new(slots),
            _supervisors: Arc::new(supervisors),
        }
    }

    /// Number of instance slots.
    pub(crate) fn len(&self) -> usize {
        self.slots.len()
    }

    /// The live pid in slot `i`, or `None` if it's absent (mid-restart / gave up).
    pub(crate) fn whereis(&self, i: usize) -> Option<Pid> {
        self.rt.whereis(&self.slots[i])
    }

    /// The pool's runtime handle (for sending into instances from a connection task).
    pub(crate) fn runtime(&self) -> &Runtime {
        &self.rt
    }

    /// Wait (bounded) until every instance has registered, so accepting traffic never
    /// races a request ahead of a ready instance.
    pub(crate) async fn ready(&self) {
        let _ = tokio::time::timeout(Duration::from_secs(5), async {
            for slot in self.slots.iter() {
                while self.rt.whereis(slot).is_none() {
                    tokio::time::sleep(Duration::from_millis(1)).await;
                }
            }
        })
        .await;
    }

    /// The current live instance pids (introspection / tests).
    pub(crate) fn pids(&self) -> Vec<Pid> {
        self.slots
            .iter()
            .filter_map(|slot| self.rt.whereis(slot))
            .collect()
    }
}
