use std::collections::VecDeque;
use std::fmt;
use std::future::Future;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use dashmap::mapref::entry::Entry;
use dashmap::DashMap;
use futures_util::future::{AbortHandle, Abortable};
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender};
use tokio::task::JoinHandle;

use crate::exit::{ExitReason, MonitorRef};
use crate::message::{Message, Received};
use crate::pid::Pid;
use crate::stream::StreamHandle;

/// What a process body receives when it starts: its own [`Pid`] and its
/// **mailbox** — the receiving end of its message queue.
pub struct Context {
    pid: Pid,
    mailbox: UnboundedReceiver<Received>,
    /// Items pulled from the channel but skipped over by a selective
    /// [`recv_match`](Context::recv_match), kept in arrival order. A later
    /// receive sees them before anything still in the channel — the Erlang
    /// "save queue". Empty (and allocation-free) unless selective receive is used.
    saved: VecDeque<Received>,
    /// Optional mailbox-depth counter (decrement side). `None` unless the runtime
    /// was built with [`Runtime::with_mailbox_depth`] — so the default hot path
    /// pays no allocation and no atomic.
    depth: Option<Arc<AtomicUsize>>,
}

impl Context {
    pub fn pid(&self) -> Pid {
        self.pid
    }

    /// Records that one item left the mailbox (no-op unless depth is tracked).
    fn note_consumed(&self) {
        if let Some(depth) = &self.depth {
            depth.fetch_sub(1, Ordering::Relaxed);
        }
    }

    /// Receives the next item, suspending the process until one arrives (FIFO),
    /// exactly like an Erlang `receive`. The result is usually a user
    /// [`Received::Message`], but a process that monitors or trap-links others
    /// also gets [`Received::Down`]/[`Received::Exit`] here, in arrival order. A
    /// process blocked here parks with zero cost until something arrives or a
    /// [`kill`](Runtime::kill) wakes it.
    pub async fn recv(&mut self) -> Received {
        let item = match self.saved.pop_front() {
            Some(item) => item,
            None => self.next_from_mailbox().await,
        };
        self.note_consumed();
        item
    }

    /// Receives the next item for which `matches` is true, suspending until one
    /// arrives. Items that don't match are left queued in arrival order for a
    /// later receive — Erlang's selective `receive`. Already-saved items are
    /// considered first, so this never reorders the mailbox.
    pub async fn recv_match<F>(&mut self, mut matches: F) -> Received
    where
        F: FnMut(&Received) -> bool,
    {
        if let Some(pos) = self.saved.iter().position(&mut matches) {
            let item = self.saved.remove(pos).expect("position is in bounds");
            self.note_consumed();
            return item;
        }
        loop {
            let item = self.next_from_mailbox().await;
            if matches(&item) {
                self.note_consumed();
                return item;
            }
            self.saved.push_back(item);
        }
    }

    async fn next_from_mailbox(&mut self) -> Received {
        // The sole sender lives in the process table, which the running task
        // keeps alive through its own `Arc<Inner>`; it is removed only after this
        // body returns. So while we are awaiting here the channel cannot close —
        // a live process always has a message coming or is parked forever.
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

/// A point-in-time snapshot of a live process for observability — the analogue
/// of Erlang's `Process.info/1`. Cheap to produce (a single table lookup). Run
/// vs. suspended *status* is deliberately omitted: Tokio doesn't expose a task's
/// park state, and faking it would mislead.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessInfo {
    pub pid: Pid,
    /// Number of bidirectionally linked peers.
    pub links: usize,
    /// Number of processes monitoring this one.
    pub monitors: usize,
    /// Registry names this process holds.
    pub names: Vec<String>,
    /// The optional human-readable label (see [`Runtime::set_label`]).
    pub label: Option<String>,
    /// Items waiting in the mailbox (channel + save queue), not yet consumed.
    pub mailbox_depth: usize,
    /// Whether this process traps exits.
    pub trap_exit: bool,
}

/// One process this entry records is monitoring us; on our exit it gets a
/// [`Received::Down`] tagged with `reference`.
struct Monitor {
    watcher: Pid,
    reference: MonitorRef,
}

/// What the runtime keeps for each live process. A process needs **only one
/// channel** — the mailbox; exit signals ride it as [`Received`], and kill rides
/// a `futures` abort handle (which exists *before* the task is spawned, so the
/// whole entry is written in a single race-free insert). Erlang runtimes and
/// Lunatic keep a *second*, signal channel per process; we don't.
///
/// `links`, `monitors` and `exit_reason` are empty/false/`None` for an ordinary
/// process and cost no allocation — only fault-tolerant processes pay for them.
struct ProcessEntry {
    abort: AbortHandle,
    mailbox: UnboundedSender<Received>,
    /// When set, incoming exit signals arrive as [`Received::Exit`] messages
    /// instead of killing this process (Erlang's `process_flag(trap_exit, true)`).
    trap_exit: bool,
    /// Bidirectionally linked peers — each also lists us.
    links: Vec<Pid>,
    /// Processes monitoring us.
    monitors: Vec<Monitor>,
    /// Names this process holds in the registry, released on exit.
    names: Vec<String>,
    /// A reason staged by a link cascade, so this process exits with the
    /// *original* reason rather than the bare `Killed` an abort would imply.
    exit_reason: Option<ExitReason>,
    /// An optional human-readable label for observability (Elixir's
    /// `Process.set_label`), distinct from a registered name. `None` and
    /// allocation-free until set.
    label: Option<String>,
    /// Optional mailbox-depth counter (increment side). `None` unless the runtime
    /// tracks depth (see [`Runtime::with_mailbox_depth`]); shared with the
    /// [`Context`] receive side and read by [`ProcessInfo`].
    depth: Option<Arc<AtomicUsize>>,
}

impl ProcessEntry {
    /// Records that one item entered the mailbox (no-op unless depth is tracked).
    fn note_enqueued(&self) {
        if let Some(depth) = &self.depth {
            depth.fetch_add(1, Ordering::Relaxed);
        }
    }
}

#[derive(Default)]
struct Inner {
    /// Whether to track per-process mailbox depth (off by default). Off means a
    /// spawn allocates no counter and send/recv do no atomics — see
    /// [`Runtime::with_mailbox_depth`].
    track_depth: bool,
    next_id: AtomicU64,
    next_ref: AtomicU64,
    spawned: AtomicU64,
    finished: AtomicU64,
    // Sharded concurrent map: spawners and completers mostly touch different
    // shards, so the process table isn't a global-lock bottleneck under a storm.
    table: DashMap<u64, ProcessEntry>,
    // name -> pid. Sharded too, so name lookups never take a global lock the way
    // Lunatic's single `RwLock<HashMap>` registry does.
    registry: DashMap<String, u64>,
}

impl Inner {
    /// Enqueues `item` into `to`'s mailbox if it is still alive, keeping the
    /// mailbox-depth counter in step. The single place a mailbox grows — used by
    /// user sends, stream sends, and system deliveries alike. Returns whether it
    /// landed. (The exit cascade in [`propagate_exit`] enqueues inline because it
    /// already holds the entry lock.)
    fn enqueue(&self, to: Pid, item: Received) -> bool {
        match self.table.get(&to.0) {
            Some(entry) => {
                if entry.mailbox.send(item).is_ok() {
                    entry.note_enqueued();
                    true
                } else {
                    false
                }
            }
            None => false,
        }
    }

    /// Delivers a system item to `to`'s mailbox if it is still alive.
    fn deliver(&self, to: Pid, item: Received) {
        self.enqueue(to, item);
    }

    /// Removes `pid`, counts it finished, and fans its exit out to everyone who
    /// was watching: a [`Received::Down`] to each monitor and a propagated exit to
    /// each link. A staged cascade reason (see [`ProcessEntry::exit_reason`])
    /// overrides `reason`.
    fn deregister(&self, pid: Pid, reason: ExitReason) {
        let Some((_, entry)) = self.table.remove(&pid.0) else {
            return;
        };
        self.finished.fetch_add(1, Ordering::Relaxed);
        let reason = entry.exit_reason.unwrap_or(reason);

        for name in &entry.names {
            self.registry.remove(name);
        }
        for monitor in entry.monitors {
            self.deliver(
                monitor.watcher,
                Received::Down {
                    reference: monitor.reference,
                    pid,
                    reason,
                },
            );
        }
        for peer in entry.links {
            self.propagate_exit(peer, pid, reason);
        }
    }

    /// Applies `from`'s exit to a linked `peer`: a trapping peer gets a
    /// [`Received::Exit`] message; an ordinary peer is taken down too on an
    /// abnormal exit (the cascade), carrying the same reason.
    fn propagate_exit(&self, peer: Pid, from: Pid, reason: ExitReason) {
        let Some(mut entry) = self.table.get_mut(&peer.0) else {
            return;
        };
        entry.links.retain(|&linked| linked != from);
        if entry.trap_exit {
            if entry.mailbox.send(Received::Exit { from, reason }).is_ok() {
                entry.note_enqueued();
            }
        } else if reason.is_abnormal() {
            entry.exit_reason = Some(reason);
            entry.abort.abort();
        }
    }
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

    /// Like [`new`](Runtime::new) but **tracks per-process mailbox depth**, so
    /// [`info`](Runtime::info) reports it. This costs a per-spawn counter
    /// allocation and a relaxed atomic per send/receive, so it's opt-in: enable it
    /// for an observer/REPL node; leave it off (the default) for peak throughput.
    pub fn with_mailbox_depth() -> Self {
        Self {
            inner: Arc::new(Inner {
                track_depth: true,
                ..Default::default()
            }),
        }
    }

    /// Spawns a process running `body`, returning a handle to it. The body is a
    /// plain async closure today; in Phase 6 a Wasm instance becomes another
    /// kind of body behind the same API.
    pub fn spawn<F, Fut>(&self, body: F) -> ProcessHandle
    where
        F: FnOnce(Context) -> Fut,
        Fut: Future<Output = ()> + Send + 'static,
    {
        self.spawn_entry(Vec::new(), body).0
    }

    /// Like [`spawn`](Runtime::spawn), but the child is **linked** to `parent`
    /// before it runs — so the link is in place even if the child exits
    /// immediately, with no race (Erlang's `spawn_link`).
    pub fn spawn_link<F, Fut>(&self, parent: Pid, body: F) -> ProcessHandle
    where
        F: FnOnce(Context) -> Fut,
        Fut: Future<Output = ()> + Send + 'static,
    {
        let (handle, child) = self.spawn_entry(vec![parent], body);
        if let Some(mut entry) = self.inner.table.get_mut(&parent.0) {
            entry.links.push(child);
        }
        handle
    }

    fn spawn_entry<F, Fut>(&self, links: Vec<Pid>, body: F) -> (ProcessHandle, Pid)
    where
        F: FnOnce(Context) -> Fut,
        Fut: Future<Output = ()> + Send + 'static,
    {
        let pid = Pid(self.inner.next_id.fetch_add(1, Ordering::Relaxed));
        let (mailbox, mailbox_rx) = unbounded_channel();
        let (abort, abort_registration) = AbortHandle::new_pair();
        // No allocation unless depth tracking is on (default off — see
        // `with_mailbox_depth`), keeping the spawn hot path allocation-lean.
        let depth = self
            .inner
            .track_depth
            .then(|| Arc::new(AtomicUsize::new(0)));

        // One write registers the whole process *before* it is spawned: a message
        // sent the instant after can't be lost, the reaper's remove always
        // balances this insert, and `kill`/`link` can already reach it.
        self.inner.table.insert(
            pid.0,
            ProcessEntry {
                abort: abort.clone(),
                mailbox,
                trap_exit: false,
                links,
                monitors: Vec::new(),
                names: Vec::new(),
                exit_reason: None,
                label: None,
                depth: depth.clone(),
            },
        );
        self.inner.spawned.fetch_add(1, Ordering::Relaxed);

        let body = body(Context {
            pid,
            mailbox: mailbox_rx,
            saved: VecDeque::new(),
            depth,
        });
        // The guard is moved *into* the task, so the process is deregistered on
        // every teardown path: completion, panic (drop runs during unwind), or a
        // kill (which makes `Abortable` resolve, ending the task).
        let guard = ProcessGuard {
            pid,
            inner: Arc::clone(&self.inner),
            reason: ExitReason::Killed,
        };
        let join = tokio::spawn(run(guard, Abortable::new(body, abort_registration)));
        (ProcessHandle { pid, abort, join }, pid)
    }

    /// Delivers `message` to `pid`'s mailbox. Returns `false` if there is no such
    /// live process — sending to a dead process is a silent no-op, like Erlang.
    pub fn send(&self, pid: Pid, message: Message) -> bool {
        self.inner.enqueue(pid, Received::Message(message))
    }

    /// Delivers a byte `stream` to `pid` as a [`Received::Stream`]. Like
    /// [`send`](Runtime::send), returns `false` if there's no such live process.
    /// The recipient reads chunks at its own pace; back-pressure flows to the
    /// writer (the channel is bounded). The stream itself is the Wasm-free
    /// substrate the p3 component bridge maps `stream<u8>` onto.
    pub fn send_stream(&self, pid: Pid, stream: StreamHandle) -> bool {
        self.inner.enqueue(pid, Received::Stream(stream))
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

    /// A snapshot of every live process's pid — Erlang's `Process.list/0`. Walks
    /// the sharded table without a global lock; a best-effort view (processes may
    /// spawn/exit during the walk).
    pub fn list(&self) -> Vec<Pid> {
        self.inner
            .table
            .iter()
            .map(|entry| Pid(*entry.key()))
            .collect()
    }

    /// A [`ProcessInfo`] snapshot for `pid`, or `None` if it isn't live —
    /// Erlang's `Process.info/1`. One table lookup; off the messaging hot path.
    pub fn info(&self, pid: Pid) -> Option<ProcessInfo> {
        self.inner.table.get(&pid.0).map(|entry| ProcessInfo {
            pid,
            links: entry.links.len(),
            monitors: entry.monitors.len(),
            names: entry.names.clone(),
            label: entry.label.clone(),
            mailbox_depth: entry
                .depth
                .as_ref()
                .map_or(0, |d| d.load(Ordering::Relaxed)),
            trap_exit: entry.trap_exit,
        })
    }

    /// Attaches a human-readable `label` to `pid` for observability (like
    /// Elixir's `Process.set_label/1`) — distinct from a registered name and
    /// need not be unique. Returns `false` if `pid` isn't live. One allocation,
    /// only when called; never touched on the send/receive path.
    pub fn set_label(&self, pid: Pid, label: impl Into<String>) -> bool {
        match self.inner.table.get_mut(&pid.0) {
            Some(mut entry) => {
                entry.label = Some(label.into());
                true
            }
            None => false,
        }
    }

    /// Stops `pid` at its next suspension point. Returns `false` if there is no
    /// such live process. Equivalent to `exit(pid, ExitReason::Killed)`.
    pub fn kill(&self, pid: Pid) -> bool {
        match self.inner.table.get(&pid.0) {
            Some(entry) => {
                entry.abort.abort();
                true
            }
            None => false,
        }
    }

    /// Terminates `pid` with an explicit `reason` (Erlang's `exit/2`) — the
    /// reason links and monitors will observe. Lets a process "crash" without a
    /// Rust panic. Returns `false` if there is no such live process.
    pub fn exit(&self, pid: Pid, reason: ExitReason) -> bool {
        match self.inner.table.get_mut(&pid.0) {
            Some(mut entry) => {
                entry.exit_reason = Some(reason);
                entry.abort.abort();
                true
            }
            None => false,
        }
    }

    /// Sets whether `pid` traps exits. A trapping process receives a linked
    /// peer's exit as a [`Received::Exit`] message instead of dying with it — how
    /// a supervisor survives its children. No-op if `pid` is not alive.
    pub fn set_trap_exit(&self, pid: Pid, trap: bool) {
        if let Some(mut entry) = self.inner.table.get_mut(&pid.0) {
            entry.trap_exit = trap;
        }
    }

    /// Bidirectionally links two live processes: when either exits abnormally the
    /// other is taken down too (or, if it traps exits, gets a [`Received::Exit`]).
    /// A no-op if either is already dead or they are the same process.
    pub fn link(&self, a: Pid, b: Pid) {
        if a == b {
            return;
        }
        // Only link if both are live; record on each side. If one vanished
        // between the checks, undo so we never leave a half-link dangling.
        if self.add_link(a, b) {
            if self.add_link(b, a) {
                return;
            }
            self.remove_link(a, b);
        }
    }

    /// Removes the link between `a` and `b` in both directions.
    pub fn unlink(&self, a: Pid, b: Pid) {
        self.remove_link(a, b);
        self.remove_link(b, a);
    }

    fn add_link(&self, owner: Pid, peer: Pid) -> bool {
        match self.inner.table.get_mut(&owner.0) {
            Some(mut entry) => {
                if !entry.links.contains(&peer) {
                    entry.links.push(peer);
                }
                true
            }
            None => false,
        }
    }

    fn remove_link(&self, owner: Pid, peer: Pid) {
        if let Some(mut entry) = self.inner.table.get_mut(&owner.0) {
            entry.links.retain(|&linked| linked != peer);
        }
    }

    /// `watcher` starts monitoring `target`: when `target` exits, `watcher`
    /// receives a [`Received::Down`] carrying the returned reference and the exit
    /// reason. Monitoring is one-way and never propagates death. If `target` is
    /// already gone, the `Down` (reason [`ExitReason::NoProc`]) is delivered at
    /// once, like Erlang.
    pub fn monitor(&self, watcher: Pid, target: Pid) -> MonitorRef {
        let reference = MonitorRef(self.inner.next_ref.fetch_add(1, Ordering::Relaxed));
        match self.inner.table.get_mut(&target.0) {
            Some(mut entry) => entry.monitors.push(Monitor { watcher, reference }),
            None => self.inner.deliver(
                watcher,
                Received::Down {
                    reference,
                    pid: target,
                    reason: ExitReason::NoProc,
                },
            ),
        }
        reference
    }

    /// Registers `name` for `pid`, so it can be reached by name. Returns `false`
    /// if the name is already taken or `pid` is not alive. A pid may hold several
    /// names; a name maps to exactly one pid. Names are released automatically
    /// when the process exits (or via [`unregister`](Runtime::unregister)).
    pub fn register(&self, name: impl Into<String>, pid: Pid) -> bool {
        let name = name.into();
        // Hold the process entry first, then the registry slot — one consistent
        // lock order, so register can never deadlock against teardown.
        let Some(mut entry) = self.inner.table.get_mut(&pid.0) else {
            return false;
        };
        match self.inner.registry.entry(name.clone()) {
            Entry::Occupied(_) => false,
            Entry::Vacant(slot) => {
                slot.insert(pid.0);
                entry.names.push(name);
                true
            }
        }
    }

    /// Resolves a registered `name` to its (live) pid.
    pub fn whereis(&self, name: &str) -> Option<Pid> {
        self.inner.registry.get(name).map(|pid| Pid(*pid))
    }

    /// Releases `name`. Returns `false` if it wasn't registered.
    pub fn unregister(&self, name: &str) -> bool {
        match self.inner.registry.remove(name) {
            Some((_, pid)) => {
                if let Some(mut entry) = self.inner.table.get_mut(&pid) {
                    entry.names.retain(|held| held != name);
                }
                true
            }
            None => false,
        }
    }

    /// Sends to a registered `name`. Returns `false` if the name is unknown (or
    /// its process just died).
    pub fn send_named(&self, name: &str, message: Message) -> bool {
        match self.whereis(name) {
            Some(pid) => self.send(pid, message),
            None => false,
        }
    }

    /// Delivers `message` to `pid` after `delay`, returning a handle that can
    /// [`cancel`](TimerRef::cancel) it before it fires. Built on Tokio's timer
    /// wheel — many pending timers cost little, and cancellation is a free abort.
    pub fn send_after(&self, pid: Pid, delay: Duration, message: Message) -> TimerRef {
        let runtime = self.clone();
        let task = tokio::spawn(async move {
            tokio::time::sleep(delay).await;
            runtime.send(pid, message);
        });
        TimerRef {
            abort: task.abort_handle(),
        }
    }

    /// Stops every live process (each still runs its normal teardown — links and
    /// monitors are notified, names released). Returns how many were signalled.
    /// Teardown is asynchronous; poll [`process_count`](Runtime::process_count)
    /// to wait for the drain.
    pub fn shutdown(&self) -> usize {
        // `abort()` only flips an atomic flag (it never touches the table), so it
        // is safe — and allocation-free — to signal each process during iteration.
        let mut stopped = 0;
        for entry in self.inner.table.iter() {
            entry.abort.abort();
            stopped += 1;
        }
        stopped
    }
}

/// A handle to a pending timer from [`send_after`](Runtime::send_after).
pub struct TimerRef {
    abort: tokio::task::AbortHandle,
}

impl TimerRef {
    /// Cancels the timer if it hasn't fired yet; a no-op once it has.
    pub fn cancel(&self) {
        self.abort.abort();
    }
}

/// Deregisters a process — and fans its exit out to links and monitors — on the
/// **Drop** path, so it runs however the body ends: completion, panic, or kill.
/// The guard lives inside the task (see [`Runtime::spawn_entry`]).
struct ProcessGuard {
    pid: Pid,
    inner: Arc<Inner>,
    reason: ExitReason,
}

impl Drop for ProcessGuard {
    fn drop(&mut self) {
        // A panic unwinding through the task means the body crashed; otherwise
        // `run` has set the reason (Normal on completion, Killed on abort).
        let reason = if std::thread::panicking() {
            ExitReason::Crashed
        } else {
            self.reason
        };
        self.inner.deregister(self.pid, reason);
    }
}

async fn run<Fut>(mut guard: ProcessGuard, body: Abortable<Fut>)
where
    Fut: Future<Output = ()> + Send + 'static,
{
    // The guard lives in the task and deregisters the process on every exit path.
    // We only need to distinguish completion from a kill here; a panic is caught
    // by the guard via `std::thread::panicking()`. No select loop is needed — the
    // abort handle is the stop signal, and it drops the inner body future.
    guard.reason = match body.await {
        Ok(()) => ExitReason::Normal,
        Err(_aborted) => ExitReason::Killed,
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_process_receives_a_message_sent_to_its_pid() {
        let rt = Runtime::new();
        let (tx, rx) = tokio::sync::oneshot::channel();
        let handle = rt.spawn(|mut ctx| async move {
            let msg = ctx.recv().await.message().unwrap();
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
                got.push(ctx.recv().await.message().unwrap());
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
    async fn recv_match_takes_a_match_and_leaves_the_rest_in_order() {
        let rt = Runtime::new();
        let (tx, rx) = tokio::sync::oneshot::channel();
        let handle = rt.spawn(|mut ctx| async move {
            // Want the "B" message; "A" arrives first and must be left queued.
            let matched = ctx
                .recv_match(|m| matches!(m, Received::Message(b) if b.first() == Some(&b'B')))
                .await
                .message()
                .unwrap();
            let then = ctx.recv().await.message().unwrap(); // the deferred "A"
            let last = ctx.recv().await.message().unwrap(); // then "C"
            let _ = tx.send((matched, then, last));
        });
        for m in [b"A".to_vec(), b"B".to_vec(), b"C".to_vec()] {
            assert!(rt.send(handle.pid(), m));
        }
        let (matched, then, last) = rx.await.unwrap();
        assert_eq!(matched, b"B".to_vec());
        assert_eq!(then, b"A".to_vec());
        assert_eq!(last, b"C".to_vec());
        handle.join().await;
    }

    #[tokio::test]
    async fn recv_match_finds_a_previously_deferred_message() {
        let rt = Runtime::new();
        let (tx, rx) = tokio::sync::oneshot::channel();
        let handle = rt.spawn(|mut ctx| async move {
            // Match "C" first, deferring A and B; then selectively pull B out of
            // the save queue, leaving A for an ordinary recv.
            let is = |byte: u8| move |m: &Received| matches!(m, Received::Message(b) if b.first() == Some(&byte));
            let c = ctx.recv_match(is(b'C')).await.message().unwrap();
            let b = ctx.recv_match(is(b'B')).await.message().unwrap();
            let a = ctx.recv().await.message().unwrap();
            let _ = tx.send((a, b, c));
        });
        for m in [b"A".to_vec(), b"B".to_vec(), b"C".to_vec()] {
            assert!(rt.send(handle.pid(), m));
        }
        let (a, b, c) = rx.await.unwrap();
        assert_eq!((a, b, c), (b"A".to_vec(), b"B".to_vec(), b"C".to_vec()));
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
            let ball = ctx.recv().await.message().unwrap();
            let reply_to = Pid::from_raw(u64::from_le_bytes(ball[..8].try_into().unwrap()));
            ponger_rt.send(reply_to, b"pong".to_vec());
        });
        let ponger_pid = ponger.pid();

        let pinger_rt = rt.clone();
        let pinger = rt.spawn(move |mut ctx| async move {
            let mut ball = ctx.pid().raw().to_le_bytes().to_vec();
            ball.extend_from_slice(b"ping");
            pinger_rt.send(ponger_pid, ball);
            let _ = done_tx.send(ctx.recv().await.message().unwrap());
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

    // --- Phase 3: links, monitors, supervision -------------------------------

    /// A watcher process that forwards the first thing it receives to the test,
    /// then parks (staying alive so it can't race its own teardown). Returns its
    /// pid and the receiving end.
    fn watch(rt: &Runtime) -> (Pid, tokio::sync::oneshot::Receiver<Received>) {
        let (tx, rx) = tokio::sync::oneshot::channel();
        let pid = rt
            .spawn(move |mut ctx| async move {
                let item = ctx.recv().await;
                let _ = tx.send(item);
                std::future::pending::<()>().await;
            })
            .pid();
        (pid, rx)
    }

    /// A process that parks until `go` fires, then ends the given way. The gate
    /// lets the test wire up links/monitors *before* the exit, with no sleeps.
    fn gated<F>(rt: &Runtime, ending: F) -> (Pid, tokio::sync::oneshot::Sender<()>)
    where
        F: FnOnce() + Send + 'static,
    {
        let (go_tx, go_rx) = tokio::sync::oneshot::channel::<()>();
        let pid = rt
            .spawn(move |_| async move {
                let _ = go_rx.await;
                ending();
            })
            .pid();
        (pid, go_tx)
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn monitor_reports_each_kind_of_exit() {
        let rt = Runtime::new();

        // Normal completion.
        let (w1, d1) = watch(&rt);
        let (t1, go1) = gated(&rt, || {});
        let r1 = rt.monitor(w1, t1);
        let _ = go1.send(());
        assert_eq!(
            d1.await.unwrap(),
            Received::Down {
                reference: r1,
                pid: t1,
                reason: ExitReason::Normal
            }
        );

        // Panic -> Crashed.
        let (w2, d2) = watch(&rt);
        let (t2, go2) = gated(&rt, || panic!("boom"));
        let r2 = rt.monitor(w2, t2);
        let _ = go2.send(());
        assert_eq!(
            d2.await.unwrap(),
            Received::Down {
                reference: r2,
                pid: t2,
                reason: ExitReason::Crashed
            }
        );

        // Kill -> Killed.
        let (w3, d3) = watch(&rt);
        let t3 = rt.spawn(|_| std::future::pending::<()>()).pid();
        let r3 = rt.monitor(w3, t3);
        assert!(rt.kill(t3));
        assert_eq!(
            d3.await.unwrap(),
            Received::Down {
                reference: r3,
                pid: t3,
                reason: ExitReason::Killed
            }
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn monitoring_a_dead_process_reports_noproc_at_once() {
        let rt = Runtime::new();
        let dead = rt.spawn(|_| async {});
        let dead_pid = dead.pid();
        dead.join().await;

        let (watcher, down) = watch(&rt);
        let reference = rt.monitor(watcher, dead_pid);
        assert_eq!(
            down.await.unwrap(),
            Received::Down {
                reference,
                pid: dead_pid,
                reason: ExitReason::NoProc
            }
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_abnormal_exit_cascades_down_links_with_its_reason() {
        let rt = Runtime::new();
        let peer = rt.spawn(|_| std::future::pending::<()>()).pid();
        let (crasher, go) = gated(&rt, || panic!("boom"));
        rt.link(peer, crasher);

        // Watch the peer: it must go down too, carrying the *crash* reason, not
        // the bare Killed an abort would otherwise imply.
        let (watcher, down) = watch(&rt);
        let reference = rt.monitor(watcher, peer);

        let _ = go.send(());
        assert_eq!(
            down.await.unwrap(),
            Received::Down {
                reference,
                pid: peer,
                reason: ExitReason::Crashed
            }
        );
        assert!(!rt.is_alive(peer));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_normal_exit_does_not_cascade() {
        let rt = Runtime::new();
        let survivor = rt.spawn(|_| std::future::pending::<()>());
        let (quitter, go) = gated(&rt, || {});
        rt.link(survivor.pid(), quitter);

        let _ = go.send(());
        // Drain the quitter to completion; its teardown (and any propagation) has
        // run by the time the table no longer lists it.
        while rt.is_alive(quitter) {
            tokio::task::yield_now().await;
        }
        assert!(
            rt.is_alive(survivor.pid()),
            "a normal exit must not kill links"
        );
        survivor.kill();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_trapping_process_gets_an_exit_message_instead_of_dying() {
        let rt = Runtime::new();
        let (tx, rx) = tokio::sync::oneshot::channel();
        let trapper = rt.spawn(move |mut ctx| async move {
            let item = ctx.recv().await;
            let _ = tx.send(item);
            std::future::pending::<()>().await; // stay alive to prove we trapped
        });
        rt.set_trap_exit(trapper.pid(), true);

        let (child, go) = gated(&rt, || panic!("boom"));
        rt.link(trapper.pid(), child);
        let _ = go.send(());

        assert_eq!(
            rx.await.unwrap(),
            Received::Exit {
                from: child,
                reason: ExitReason::Crashed
            }
        );
        assert!(
            rt.is_alive(trapper.pid()),
            "a trapping process must survive"
        );
        trapper.kill();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn spawn_link_links_the_child_to_its_parent() {
        let rt = Runtime::new();
        let (tx, rx) = tokio::sync::oneshot::channel();
        let parent = rt.spawn(move |mut ctx| async move {
            let item = ctx.recv().await;
            let _ = tx.send(item);
            std::future::pending::<()>().await;
        });
        rt.set_trap_exit(parent.pid(), true);

        let (go_tx, go_rx) = tokio::sync::oneshot::channel::<()>();
        let child = rt
            .spawn_link(parent.pid(), move |_| async move {
                let _ = go_rx.await;
                panic!("boom");
            })
            .pid();
        let _ = go_tx.send(());

        assert_eq!(
            rx.await.unwrap(),
            Received::Exit {
                from: child,
                reason: ExitReason::Crashed
            }
        );
        parent.kill();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn unlinking_stops_propagation() {
        let rt = Runtime::new();
        let survivor = rt.spawn(|_| std::future::pending::<()>());
        let (crasher, go) = gated(&rt, || panic!("boom"));
        rt.link(survivor.pid(), crasher);
        rt.unlink(survivor.pid(), crasher);

        let _ = go.send(());
        while rt.is_alive(crasher) {
            tokio::task::yield_now().await;
        }
        assert!(
            rt.is_alive(survivor.pid()),
            "an unlinked peer must not be taken down"
        );
        survivor.kill();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn linking_a_dead_peer_leaves_no_half_link() {
        let rt = Runtime::new();
        let alive = rt.spawn(|_| std::future::pending::<()>());
        let dead = rt.spawn(|_| async {});
        let dead_pid = dead.pid();
        dead.join().await;

        // The dead side can't be recorded; the half-link on `alive` is undone.
        rt.link(alive.pid(), dead_pid);

        // Prove `alive`'s link set is intact: a fresh linked crasher still
        // cascades to it. (If the undo had corrupted the list this would hang.)
        let (crasher, go) = gated(&rt, || panic!("boom"));
        rt.link(alive.pid(), crasher);
        let _ = go.send(());
        while rt.is_alive(alive.pid()) {
            tokio::task::yield_now().await;
        }
        assert!(!rt.is_alive(crasher));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn exit_terminates_with_the_chosen_reason() {
        let rt = Runtime::new();
        let (watcher, down) = watch(&rt);
        let target = rt.spawn(|_| std::future::pending::<()>()).pid();
        let reference = rt.monitor(watcher, target);

        // exit/2 with a custom reason — no panic, yet observed as Crashed.
        assert!(rt.exit(target, ExitReason::Crashed));
        assert_eq!(
            down.await.unwrap(),
            Received::Down {
                reference,
                pid: target,
                reason: ExitReason::Crashed
            }
        );
        assert!(!rt.exit(Pid::from_raw(987_654), ExitReason::Normal)); // unknown pid
    }

    #[tokio::test]
    async fn link_and_trap_on_missing_processes_are_no_ops() {
        let rt = Runtime::new();
        let p = rt.spawn(|_| std::future::pending::<()>());
        let dead = Pid::from_raw(999_999);
        rt.link(p.pid(), p.pid()); // self-link: ignored
        rt.link(dead, p.pid()); // dead first arg: nothing recorded
        rt.link(p.pid(), dead); // dead second arg: half-link undone
        rt.unlink(dead, p.pid()); // unlink with a dead owner: no-op
        rt.set_trap_exit(dead, true); // dead pid: no-op, no panic
        assert!(rt.is_alive(p.pid()));
        p.kill();
    }

    // --- Phase 4: registry, timers, shutdown ---------------------------------

    #[tokio::test]
    async fn register_whereis_send_named_then_auto_release_on_exit() {
        let rt = Runtime::new();
        let (tx, rx) = tokio::sync::oneshot::channel();
        let worker = rt.spawn(move |mut ctx| async move {
            let job = ctx.recv().await.message().unwrap();
            let _ = tx.send(job);
        });
        assert!(rt.register("worker", worker.pid()));
        assert_eq!(rt.whereis("worker"), Some(worker.pid()));
        assert!(!rt.register("worker", worker.pid())); // already taken
        assert!(rt.send_named("worker", b"job".to_vec()));
        assert_eq!(rx.await.unwrap(), b"job".to_vec());

        worker.join().await; // exiting auto-releases the name
        assert_eq!(rt.whereis("worker"), None);
        assert!(!rt.send_named("worker", b"late".to_vec()));
    }

    #[tokio::test]
    async fn names_are_released_by_unregister_and_reusable_after_death() {
        let rt = Runtime::new();
        let a = rt.spawn(|_| std::future::pending::<()>());
        assert!(rt.register("svc", a.pid()));
        assert!(rt.unregister("svc"));
        assert_eq!(rt.whereis("svc"), None);
        assert!(!rt.unregister("svc")); // already gone

        assert!(rt.register("svc", a.pid()));
        a.kill();
        a.join().await;
        assert_eq!(rt.whereis("svc"), None);
        let b = rt.spawn(|_| std::future::pending::<()>());
        assert!(rt.register("svc", b.pid())); // a dead process's name is reusable
        b.kill();
    }

    #[tokio::test]
    async fn register_to_a_dead_pid_fails_and_a_pid_can_hold_several_names() {
        let rt = Runtime::new();
        let dead = rt.spawn(|_| async {});
        let dead_pid = dead.pid();
        dead.join().await;
        assert!(!rt.register("ghost", dead_pid));

        let p = rt.spawn(|_| std::future::pending::<()>());
        assert!(rt.register("one", p.pid()));
        assert!(rt.register("two", p.pid()));
        assert_eq!(rt.whereis("one"), Some(p.pid()));
        assert_eq!(rt.whereis("two"), Some(p.pid()));
        p.kill();
        p.join().await;
        assert_eq!(rt.whereis("one"), None); // all of a pid's names go on exit
        assert_eq!(rt.whereis("two"), None);
    }

    #[tokio::test(start_paused = true)]
    async fn send_after_delivers_when_the_timer_fires() {
        let rt = Runtime::new();
        let (tx, rx) = tokio::sync::oneshot::channel();
        let target = rt.spawn(move |mut ctx| async move {
            let msg = ctx.recv().await.message().unwrap();
            let _ = tx.send(msg);
        });
        rt.send_after(target.pid(), Duration::from_secs(60), b"ding".to_vec());
        // Paused time auto-advances to the timer once everything else is idle.
        assert_eq!(rx.await.unwrap(), b"ding".to_vec());
    }

    #[tokio::test(start_paused = true)]
    async fn a_cancelled_timer_never_fires() {
        let rt = Runtime::new();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let target = rt.spawn(move |mut ctx| async move {
            loop {
                let _ = tx.send(ctx.recv().await);
            }
        });
        let timer = rt.send_after(target.pid(), Duration::from_secs(60), b"x".to_vec());
        timer.cancel();
        tokio::time::advance(Duration::from_secs(120)).await;
        tokio::task::yield_now().await; // let any (erroneous) delivery land before we check
        assert!(
            rx.try_recv().is_err(),
            "a cancelled timer must deliver nothing"
        );
        target.kill();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn shutdown_stops_every_process() {
        let rt = Runtime::new();
        let procs: Vec<_> = (0..5)
            .map(|_| rt.spawn(|_| std::future::pending::<()>()))
            .collect();
        assert_eq!(rt.process_count(), 5);
        assert_eq!(rt.shutdown(), 5);
        for p in procs {
            p.join().await;
        }
        assert_eq!(rt.process_count(), 0);
        assert_eq!(rt.shutdown(), 0); // nothing left to stop
    }

    // --- Phase 7: introspection & labels -------------------------------------

    #[tokio::test]
    async fn list_reflects_live_processes() {
        use std::collections::HashSet;
        let rt = Runtime::new();
        assert!(rt.list().is_empty());
        let a = rt.spawn(|_| std::future::pending::<()>());
        let b = rt.spawn(|_| std::future::pending::<()>());
        let live: HashSet<u64> = rt.list().iter().map(|p| p.raw()).collect();
        assert_eq!(live, HashSet::from([a.pid().raw(), b.pid().raw()]));
        a.kill();
        a.join().await;
        assert_eq!(rt.list(), vec![b.pid()]);
        b.kill();
    }

    #[tokio::test]
    async fn info_reports_links_names_label_and_trap() {
        let rt = Runtime::new();
        let p = rt.spawn(|_| std::future::pending::<()>());
        let peer = rt.spawn(|_| std::future::pending::<()>());
        rt.link(p.pid(), peer.pid());
        assert!(rt.register("svc", p.pid()));
        rt.set_trap_exit(p.pid(), true);
        assert!(rt.set_label(p.pid(), "worker #1"));

        let info = rt.info(p.pid()).unwrap();
        assert_eq!(info.pid, p.pid());
        assert_eq!(info.links, 1);
        assert_eq!(info.monitors, 0);
        assert_eq!(info.names, vec!["svc".to_string()]);
        assert_eq!(info.label.as_deref(), Some("worker #1"));
        assert!(info.trap_exit);
        assert_eq!(info.mailbox_depth, 0);
        p.kill();
        peer.kill();
    }

    #[tokio::test]
    async fn info_and_set_label_on_a_dead_pid() {
        let rt = Runtime::new();
        let d = rt.spawn(|_| async {});
        let pid = d.pid();
        d.join().await;
        assert!(rt.info(pid).is_none());
        assert!(!rt.set_label(pid, "ghost"));
    }

    #[tokio::test]
    async fn mailbox_depth_tracks_unconsumed_messages() {
        let rt = Runtime::with_mailbox_depth();
        let (go_tx, go_rx) = tokio::sync::oneshot::channel::<()>();
        let (done_tx, done_rx) = tokio::sync::oneshot::channel();
        let p = rt.spawn(move |mut ctx| async move {
            let _ = go_rx.await; // hold off consuming until the test has filled the box
            for _ in 0..3 {
                ctx.recv().await;
            }
            let _ = done_tx.send(());
            std::future::pending::<()>().await;
        });
        // `send` increments depth synchronously, so this is race-free even though
        // the process hasn't been polled past its gate yet.
        for m in [b"a".to_vec(), b"b".to_vec(), b"c".to_vec()] {
            assert!(rt.send(p.pid(), m));
        }
        assert_eq!(rt.info(p.pid()).unwrap().mailbox_depth, 3);

        let _ = go_tx.send(());
        let _ = done_rx.await; // all three consumed by now
        assert_eq!(rt.info(p.pid()).unwrap().mailbox_depth, 0);
        p.kill();
    }

    #[tokio::test]
    async fn mailbox_depth_counts_messages_deferred_by_selective_receive() {
        let rt = Runtime::with_mailbox_depth();
        let (go_tx, go_rx) = tokio::sync::oneshot::channel::<()>();
        let (done_tx, done_rx) = tokio::sync::oneshot::channel();
        let p = rt.spawn(move |mut ctx| async move {
            let _ = go_rx.await;
            // Consume only "B"; "A" and "C" stay deferred — still unconsumed.
            let _ = ctx
                .recv_match(|m| matches!(m, Received::Message(b) if b.first() == Some(&b'B')))
                .await;
            let _ = done_tx.send(());
            std::future::pending::<()>().await;
        });
        for m in [b"A".to_vec(), b"B".to_vec(), b"C".to_vec()] {
            assert!(rt.send(p.pid(), m));
        }
        assert_eq!(rt.info(p.pid()).unwrap().mailbox_depth, 3);

        let _ = go_tx.send(());
        let _ = done_rx.await;
        // One consumed (B); A and C remain deferred but counted unconsumed.
        while rt.info(p.pid()).map_or(false, |i| i.mailbox_depth != 2) {
            tokio::task::yield_now().await;
        }
        assert_eq!(rt.info(p.pid()).unwrap().mailbox_depth, 2);
        p.kill();
    }

    // --- Phase 7: stream-carrying messages -----------------------------------

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_stream_is_delivered_and_read_in_order_after_a_message() {
        use crate::stream::stream;
        let rt = Runtime::new();
        let (out_tx, out_rx) = tokio::sync::oneshot::channel();
        let p = rt.spawn(move |mut ctx| async move {
            // A normal message, then a stream — FIFO across both kinds.
            let first = ctx.recv().await.message();
            let mut handle = ctx.recv().await.stream().expect("a stream");
            let mut chunks = Vec::new();
            while let Some(chunk) = handle.read().await {
                chunks.push(chunk);
            }
            let _ = out_tx.send((first, chunks));
        });

        assert!(rt.send(p.pid(), b"hello".to_vec()));
        let (writer, handle) = stream();
        assert!(rt.send_stream(p.pid(), handle));
        tokio::spawn(async move {
            for chunk in [b"a".to_vec(), b"b".to_vec(), b"c".to_vec()] {
                writer.write(chunk).await.unwrap();
            }
            // Dropping the writer here closes the stream so the reader sees the end.
        });

        let (first, chunks) = out_rx.await.unwrap();
        assert_eq!(first, Some(b"hello".to_vec()));
        assert_eq!(chunks, vec![b"a".to_vec(), b"b".to_vec(), b"c".to_vec()]);
        p.join().await;
    }

    #[tokio::test]
    async fn send_stream_to_a_dead_pid_returns_false() {
        use crate::stream::stream;
        let rt = Runtime::new();
        let (_writer, handle) = stream();
        assert!(!rt.send_stream(Pid::from_raw(123_456), handle));
    }

    #[tokio::test]
    async fn a_stream_counts_toward_mailbox_depth_until_consumed() {
        use crate::stream::stream;
        let rt = Runtime::with_mailbox_depth();
        let (go_tx, go_rx) = tokio::sync::oneshot::channel::<()>();
        let (done_tx, done_rx) = tokio::sync::oneshot::channel();
        let p = rt.spawn(move |mut ctx| async move {
            let _ = go_rx.await;
            let _ = ctx.recv().await; // consume the stream message
            let _ = done_tx.send(());
            std::future::pending::<()>().await;
        });
        let (_writer, handle) = stream();
        assert!(rt.send_stream(p.pid(), handle));
        assert_eq!(rt.info(p.pid()).unwrap().mailbox_depth, 1);
        let _ = go_tx.send(());
        let _ = done_rx.await;
        assert_eq!(rt.info(p.pid()).unwrap().mailbox_depth, 0);
        p.kill();
    }
}
