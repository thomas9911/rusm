use std::future::Future;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use dashmap::DashMap;
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender};
use tokio::task::JoinHandle;

use crate::pid::Pid;
use crate::signal::Signal;

/// What a process body receives when it starts. Grows with later phases (it will
/// carry the message mailbox in Phase 2).
#[derive(Debug)]
pub struct Context {
    pid: Pid,
}

impl Context {
    pub fn pid(&self) -> Pid {
        self.pid
    }
}

/// A handle to a spawned process: address it ([`kill`](ProcessHandle::kill)) and
/// await it ([`join`](ProcessHandle::join)).
pub struct ProcessHandle {
    pid: Pid,
    signals: UnboundedSender<Signal>,
    join: JoinHandle<()>,
}

impl ProcessHandle {
    pub fn pid(&self) -> Pid {
        self.pid
    }

    /// Asks the process to stop, by delivering [`Signal::Shutdown`].
    pub fn kill(&self) {
        let _ = self.signals.send(Signal::Shutdown);
    }

    /// Waits for the process to terminate (ignores a body panic).
    pub async fn join(self) {
        let _ = self.join.await;
    }
}

#[derive(Default)]
struct Inner {
    next_id: AtomicU64,
    spawned: AtomicU64,
    finished: AtomicU64,
    // Sharded concurrent map: spawners and completers mostly touch different
    // shards, so the process table isn't a global-lock bottleneck under a storm.
    table: DashMap<u64, UnboundedSender<Signal>>,
}

/// Spawns and tracks lightweight processes. Cheap to clone — clones share the
/// same process table and counters.
#[derive(Clone, Default)]
pub struct Runtime {
    inner: Arc<Inner>,
}

impl Runtime {
    pub fn new() -> Self {
        Self::default()
    }

    /// Spawns a process running `body`, returning a handle to it. The body is a
    /// plain async closure today; in Phase 6 a Wasm instance becomes another
    /// kind of body behind the same API.
    pub fn spawn<F, Fut>(&self, body: F) -> ProcessHandle
    where
        F: FnOnce(Context) -> Fut,
        Fut: Future<Output = ()> + Send + 'static,
    {
        let pid = Pid(self.inner.next_id.fetch_add(1, Ordering::Relaxed));
        let (signals, rx) = unbounded_channel();
        self.inner.table.insert(pid.0, signals.clone());
        self.inner.spawned.fetch_add(1, Ordering::Relaxed);

        let body = body(Context { pid });
        let join = tokio::spawn(run(pid, body, rx, Arc::clone(&self.inner)));
        ProcessHandle { pid, signals, join }
    }

    /// Number of currently-live processes.
    pub fn process_count(&self) -> usize {
        self.inner.table.len()
    }

    /// Total processes ever spawned.
    pub fn spawned(&self) -> u64 {
        self.inner.spawned.load(Ordering::Relaxed)
    }

    /// Total processes that have terminated (for any reason).
    pub fn finished(&self) -> u64 {
        self.inner.finished.load(Ordering::Relaxed)
    }

    pub fn is_alive(&self, pid: Pid) -> bool {
        self.inner.table.contains_key(&pid.0)
    }

    /// Signals `pid` to stop. Returns `false` if there is no such live process.
    pub fn kill(&self, pid: Pid) -> bool {
        match self.inner.table.get(&pid.0) {
            Some(signals) => {
                let _ = signals.send(Signal::Shutdown);
                true
            }
            None => false,
        }
    }
}

/// Deregisters a process and counts it finished on the **Drop** path, so cleanup
/// runs whether the body completes, is shut down, or panics.
struct ProcessGuard {
    pid: Pid,
    inner: Arc<Inner>,
}

impl Drop for ProcessGuard {
    fn drop(&mut self) {
        self.inner.table.remove(&self.pid.0);
        self.inner.finished.fetch_add(1, Ordering::Relaxed);
    }
}

async fn run<Fut>(pid: Pid, body: Fut, mut signals: UnboundedReceiver<Signal>, inner: Arc<Inner>)
where
    Fut: Future<Output = ()> + Send + 'static,
{
    let _guard = ProcessGuard { pid, inner };
    tokio::pin!(body);
    tokio::select! {
        biased;
        // Any signal (Phase 1: only Shutdown) or a closed channel stops the
        // process; Phase 2 expands this into a message/link match.
        _ = signals.recv() => {}
        () = &mut body => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_process_runs_to_completion_and_is_cleaned_up() {
        let rt = Runtime::new();
        let handle = rt.spawn(|_| async {});
        let pid = handle.pid();
        handle.join().await;
        assert_eq!(rt.spawned(), 1);
        assert_eq!(rt.finished(), 1);
        assert_eq!(rt.process_count(), 0);
        assert!(!rt.is_alive(pid));
    }

    #[tokio::test]
    async fn body_receives_its_own_pid() {
        let rt = Runtime::new();
        let (tx, rx) = tokio::sync::oneshot::channel();
        let handle = rt.spawn(move |ctx| async move {
            assert_eq!(
                format!("{ctx:?}"),
                format!("Context {{ pid: {:?} }}", ctx.pid())
            );
            let _ = tx.send(ctx.pid());
        });
        let pid = handle.pid();
        assert_eq!(rx.await.unwrap(), pid);
        handle.join().await;
    }

    #[tokio::test]
    async fn pids_are_unique_and_increasing() {
        let rt = Runtime::new();
        let a = rt.spawn(|_| async {});
        let b = rt.spawn(|_| async {});
        assert_ne!(a.pid(), b.pid());
        assert!(b.pid().raw() > a.pid().raw());
        a.join().await;
        b.join().await;
    }

    #[tokio::test]
    async fn kill_terminates_a_running_process() {
        let rt = Runtime::new();
        // A body that never completes on its own, so termination can only come
        // from the kill — `finished == 1` afterwards proves the kill worked.
        let handle = rt.spawn(|_| std::future::pending::<()>());
        let pid = handle.pid();
        assert!(rt.is_alive(pid));
        handle.kill();
        handle.join().await;
        assert!(!rt.is_alive(pid));
        assert_eq!(rt.process_count(), 0);
        assert_eq!(rt.finished(), 1);
    }

    #[tokio::test]
    async fn runtime_kill_signals_a_live_process() {
        let rt = Runtime::new();
        let handle = rt.spawn(|_| std::future::pending::<()>());
        let pid = handle.pid();
        assert!(rt.kill(pid));
        handle.join().await;
        assert!(!rt.is_alive(pid));
    }

    #[tokio::test]
    async fn kill_unknown_pid_returns_false() {
        let rt = Runtime::new();
        assert!(!rt.kill(Pid(999)));
    }

    #[tokio::test]
    async fn a_panicking_body_is_still_cleaned_up() {
        let rt = Runtime::new();
        let handle = rt.spawn(|_| async { panic!("boom") });
        handle.join().await; // join swallows the JoinError
        assert_eq!(rt.process_count(), 0);
        assert_eq!(rt.finished(), 1);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn spawns_many_processes_concurrently() {
        let rt = Runtime::new();
        let handles: Vec<_> = (0..1000).map(|_| rt.spawn(|_| async {})).collect();
        for handle in handles {
            handle.join().await;
        }
        assert_eq!(rt.spawned(), 1000);
        assert_eq!(rt.finished(), 1000);
        assert_eq!(rt.process_count(), 0);
    }
}
