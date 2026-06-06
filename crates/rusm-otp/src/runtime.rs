use std::fmt;
use std::future::Future;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use dashmap::DashMap;
use futures_util::future::{AbortHandle, Abortable};
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender};
use tokio::task::JoinHandle;

use crate::message::Message;
use crate::pid::Pid;

/// What a process body receives when it starts: its own [`Pid`] and its
/// **mailbox** — the receiving end of its message queue.
pub struct Context {
    pid: Pid,
    mailbox: UnboundedReceiver<Message>,
}

impl Context {
    pub fn pid(&self) -> Pid {
        self.pid
    }

    /// Receives the next message, suspending the process until one arrives
    /// (FIFO), exactly like an Erlang `receive`. A process blocked here parks
    /// with zero cost until a message or a [`kill`](Runtime::kill) wakes it, so a
    /// server loop is simply `loop { let msg = ctx.recv().await; … }`.
    pub async fn recv(&mut self) -> Message {
        // The sole sender lives in the process table, which the running task
        // keeps alive through its own `Arc<Inner>`; it is removed only after this
        // body returns. So while we are awaiting here the channel cannot close —
        // `recv` always yields a message.
        self.mailbox
            .recv()
            .await
            .expect("a live process always holds its own mailbox sender")
    }
}

impl fmt::Debug for Context {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // The mailbox receiver isn't meaningfully printable; the pid identifies it.
        f.debug_struct("Context").field("pid", &self.pid).finish()
    }
}

/// A handle to a spawned process: address it ([`kill`](ProcessHandle::kill)) and
/// await it ([`join`](ProcessHandle::join)).
pub struct ProcessHandle {
    pid: Pid,
    abort: AbortHandle,
    join: JoinHandle<()>,
}

impl ProcessHandle {
    pub fn pid(&self) -> Pid {
        self.pid
    }

    /// Stops the process at its next suspension point. Cleanup still runs (the
    /// table entry is removed and the process counted finished), because that
    /// lives on the body's drop path.
    pub fn kill(&self) {
        self.abort.abort();
    }

    /// Waits for the process to terminate (ignores a body panic or a kill).
    pub async fn join(self) {
        let _ = self.join.await;
    }
}

/// What the runtime keeps for each live process: its message mailbox and a
/// handle to stop it. A process needs **only one channel** — the mailbox.
/// (Erlang runtimes and Lunatic keep a *second*, signal channel per process;
/// here exit signals will instead ride the mailbox itself in Phase 3, like
/// Erlang's `{'EXIT', …}` messages.) Cancellation rides a `futures` abort handle
/// rather than Tokio's `JoinHandle::abort`, because this one exists *before* the
/// task is spawned — so the whole entry is written in a single, race-free insert.
struct ProcessEntry {
    abort: AbortHandle,
    mailbox: UnboundedSender<Message>,
}

#[derive(Default)]
struct Inner {
    next_id: AtomicU64,
    spawned: AtomicU64,
    finished: AtomicU64,
    // Sharded concurrent map: spawners and completers mostly touch different
    // shards, so the process table isn't a global-lock bottleneck under a storm.
    table: DashMap<u64, ProcessEntry>,
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
        let (mailbox, mailbox_rx) = unbounded_channel();
        let (abort, abort_registration) = AbortHandle::new_pair();

        // One write registers the whole process *before* it is spawned: a message
        // sent the instant after can't be lost, the reaper's remove always
        // balances this insert, and `kill` can already reach it.
        self.inner.table.insert(
            pid.0,
            ProcessEntry {
                abort: abort.clone(),
                mailbox,
            },
        );
        self.inner.spawned.fetch_add(1, Ordering::Relaxed);

        let body = body(Context {
            pid,
            mailbox: mailbox_rx,
        });
        // The guard is moved *into* the task, so the process is deregistered on
        // every teardown path: completion, panic (drop runs during unwind), or a
        // kill (which makes `Abortable` resolve, ending the task).
        let guard = ProcessGuard {
            pid,
            inner: Arc::clone(&self.inner),
        };
        let join = tokio::spawn(run(guard, Abortable::new(body, abort_registration)));
        ProcessHandle { pid, abort, join }
    }

    /// Delivers `message` to `pid`'s mailbox. Returns `false` if there is no such
    /// live process — sending to a dead process is a silent no-op, like Erlang.
    pub fn send(&self, pid: Pid, message: Message) -> bool {
        match self.inner.table.get(&pid.0) {
            Some(entry) => entry.mailbox.send(message).is_ok(),
            None => false,
        }
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

    /// Stops `pid` at its next suspension point. Returns `false` if there is no
    /// such live process.
    pub fn kill(&self, pid: Pid) -> bool {
        match self.inner.table.get(&pid.0) {
            Some(entry) => {
                entry.abort.abort();
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

async fn run<Fut>(_guard: ProcessGuard, body: Abortable<Fut>)
where
    Fut: Future<Output = ()> + Send + 'static,
{
    // `_guard` lives in the task and deregisters the process on every exit path:
    // normal completion, a panic (drop runs during unwind), or a kill (which
    // makes `body` resolve to `Err(Aborted)`). No select loop is needed — the
    // abort handle is the stop signal, and it drops the inner body future.
    let _ = body.await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_process_receives_a_message_sent_to_its_pid() {
        let rt = Runtime::new();
        let (tx, rx) = tokio::sync::oneshot::channel();
        let handle = rt.spawn(|mut ctx| async move {
            let msg = ctx.recv().await;
            let _ = tx.send(msg);
        });
        assert!(rt.send(handle.pid(), b"hello".to_vec()));
        assert_eq!(rx.await.unwrap(), b"hello".to_vec());
        handle.join().await;
    }

    #[tokio::test]
    async fn messages_arrive_in_fifo_order() {
        let rt = Runtime::new();
        let (tx, rx) = tokio::sync::oneshot::channel();
        let handle = rt.spawn(|mut ctx| async move {
            let mut got = Vec::new();
            for _ in 0..3 {
                got.push(ctx.recv().await);
            }
            let _ = tx.send(got);
        });
        for byte in [b"a".to_vec(), b"b".to_vec(), b"c".to_vec()] {
            assert!(rt.send(handle.pid(), byte));
        }
        assert_eq!(
            rx.await.unwrap(),
            vec![b"a".to_vec(), b"b".to_vec(), b"c".to_vec()]
        );
        handle.join().await;
    }

    #[tokio::test]
    async fn send_to_unknown_pid_returns_false() {
        let rt = Runtime::new();
        assert!(!rt.send(Pid(424242), b"hi".to_vec()));
    }

    #[tokio::test]
    async fn send_to_a_finished_process_returns_false() {
        let rt = Runtime::new();
        let handle = rt.spawn(|_| async {});
        let pid = handle.pid();
        handle.join().await; // finished and reaped — mailbox is gone
        assert!(!rt.send(pid, b"too late".to_vec()));
    }

    #[tokio::test]
    async fn killing_a_parked_receiver_stops_it_and_cleans_up() {
        // A process blocked in recv (no message will ever come) must still be
        // killable — abort wakes it at the suspension point and the guard reaps it.
        let rt = Runtime::new();
        let handle = rt.spawn(|mut ctx| async move {
            let _forever = ctx.recv().await;
        });
        let pid = handle.pid();
        assert!(rt.is_alive(pid));
        handle.kill();
        handle.join().await;
        assert!(!rt.is_alive(pid));
        assert_eq!(rt.finished(), 1);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn two_processes_play_ping_pong() {
        // A message carries its sender's pid (its first 8 bytes), so the ponger
        // knows whom to reply to — the byte-level analogue of Erlang's
        // `send(peer, {self(), :ping})`.
        let rt = Runtime::new();
        let (done_tx, done_rx) = tokio::sync::oneshot::channel();

        let ponger_rt = rt.clone();
        let ponger = rt.spawn(move |mut ctx| async move {
            let ball = ctx.recv().await;
            let reply_to = Pid(u64::from_le_bytes(ball[..8].try_into().unwrap()));
            ponger_rt.send(reply_to, b"pong".to_vec());
        });
        let ponger_pid = ponger.pid();

        let pinger_rt = rt.clone();
        let pinger = rt.spawn(move |mut ctx| async move {
            let mut ball = ctx.pid().raw().to_le_bytes().to_vec();
            ball.extend_from_slice(b"ping");
            pinger_rt.send(ponger_pid, ball);
            let _ = done_tx.send(ctx.recv().await);
        });

        assert_eq!(done_rx.await.unwrap(), b"pong".to_vec());
        pinger.join().await;
        ponger.join().await;
    }

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
