//! A native, Wasm-free supervisor — the OTP restart strategies over the core's
//! `monitor`/`Down` substrate. This is the single home for the restart logic: the
//! guest SDK supervisors (`rusm-rs`, the TS runner) delegate to it, and native
//! callers (the cluster, the resident-handler pool) use it directly.
//!
//! A supervisor is itself a process. It **monitors** (never links) its children, so
//! a child's death can't take it down; on a child's `Down` it restarts per the
//! strategy. Exceeding the restart-intensity budget makes the supervisor give up:
//! it kills its remaining children and exits `Crashed`, so a parent supervisor sees
//! the failure and can escalate.

use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::{Context, ExitReason, Pid, ProcessHandle, Received, Runtime};

/// How a supervisor reacts when one child dies.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Strategy {
    /// Restart only the child that died.
    OneForOne,
    /// Terminate and restart every child.
    OneForAll,
    /// Restart the dead child and every child started after it.
    RestForOne,
}

/// A factory that starts one child process. Called once per (re)start, so it must
/// be re-runnable — hence `Fn`, not `FnOnce`.
type ChildFn = Arc<dyn Fn(&Runtime) -> ProcessHandle + Send + Sync>;

/// Builder for a supervisor process. Create with [`Runtime::supervisor`], add
/// children, then [`start`](Supervisor::start).
pub struct Supervisor {
    rt: Runtime,
    strategy: Strategy,
    children: Vec<ChildFn>,
    max_restarts: u32,
    within: Option<Duration>,
}

impl Runtime {
    /// Begins building a supervisor under `strategy`. Defaults to a restart
    /// intensity of 3 restarts within 5 seconds — sane against crash loops; override
    /// with [`max_restarts`](Supervisor::max_restarts) / [`within`](Supervisor::within).
    pub fn supervisor(&self, strategy: Strategy) -> Supervisor {
        Supervisor {
            rt: self.clone(),
            strategy,
            children: Vec::new(),
            max_restarts: 3,
            within: Some(Duration::from_secs(5)),
        }
    }
}

impl Supervisor {
    /// Adds a child, described by a factory that starts (and returns a handle to) a
    /// fresh process. The factory runs again on each restart.
    pub fn child<F>(mut self, start: F) -> Self
    where
        F: Fn(&Runtime) -> ProcessHandle + Send + Sync + 'static,
    {
        self.children.push(Arc::new(start));
        self
    }

    /// Sets the maximum number of restarts tolerated (within the window, if any).
    /// `0` means unlimited.
    pub fn max_restarts(mut self, n: u32) -> Self {
        self.max_restarts = n;
        self
    }

    /// Sets the restart-intensity window: at most `max_restarts` restarts may occur
    /// within this span before the supervisor gives up. `None` counts over the whole
    /// lifetime instead.
    pub fn within(mut self, window: Duration) -> Self {
        self.within = Some(window);
        self
    }

    /// Counts restarts over the whole lifetime rather than a sliding window.
    pub fn over_lifetime(mut self) -> Self {
        self.within = None;
        self
    }

    /// Starts the supervisor as a process: it spawns and monitors each child, then
    /// restarts per the strategy as children die. Returns the supervisor's handle.
    pub fn start(self) -> ProcessHandle {
        let Supervisor {
            rt,
            strategy,
            children,
            max_restarts,
            within,
        } = self;
        let sup_rt = rt.clone();
        rt.spawn(move |mut ctx| async move {
            let me = ctx.pid();
            let mut pids: Vec<Pid> = (0..children.len())
                .map(|i| start_child(&sup_rt, me, &children, i))
                .collect();
            let mut lifetime = 0u32;
            let mut window: Vec<Instant> = Vec::new();

            loop {
                let dead = match ctx.recv().await {
                    Received::Down { pid, .. } => pid,
                    _ => continue,
                };
                // A `Down` for a pid we no longer track (e.g. a survivor we killed
                // during a previous restart) is stale — ignore it.
                let Some(index) = pids.iter().position(|&p| p == dead) else {
                    continue;
                };

                if over_budget(&mut window, &mut lifetime, within, max_restarts) {
                    for &p in &pids {
                        sup_rt.kill(p);
                    }
                    // Give up abnormally so a parent supervisor can escalate.
                    sup_rt.exit(me, ExitReason::Crashed);
                    return;
                }

                match strategy {
                    Strategy::OneForOne => {
                        pids[index] = start_child(&sup_rt, me, &children, index);
                    }
                    Strategy::OneForAll => {
                        // Terminate the survivors and wait for them to actually go
                        // down (which releases their names/resources) before
                        // restarting — OTP semantics, and it avoids racing a
                        // restart against the old child's teardown.
                        let survivors: Vec<Pid> = pids
                            .iter()
                            .enumerate()
                            .filter(|&(j, _)| j != index)
                            .map(|(_, &p)| p)
                            .collect();
                        terminate(&sup_rt, &mut ctx, survivors).await;
                        pids = (0..children.len())
                            .map(|i| start_child(&sup_rt, me, &children, i))
                            .collect();
                    }
                    Strategy::RestForOne => {
                        let later: Vec<Pid> = pids[index + 1..].to_vec();
                        terminate(&sup_rt, &mut ctx, later).await;
                        for j in index..pids.len() {
                            pids[j] = start_child(&sup_rt, me, &children, j);
                        }
                    }
                }
            }
        })
    }
}

/// Starts child `i` and monitors it from the supervisor `me`. The child runs
/// detached (the handle is dropped — dropping never kills); the supervisor reaches
/// it by pid (`kill`) and learns of its death by the monitor.
fn start_child(rt: &Runtime, me: Pid, children: &[ChildFn], i: usize) -> Pid {
    let pid = (children[i])(rt).pid();
    rt.monitor(me, pid);
    pid
}

/// Kills each child in `targets` and waits until every one has reported `Down` —
/// so their names and resources are released before any restart. The supervisor
/// monitors its children, so each kill yields a `Down` we drain here; `Down`s for
/// pids outside `targets` are ignored (a restart trigger arriving mid-termination
/// is rare and handled on the next loop).
async fn terminate(rt: &Runtime, ctx: &mut Context, mut targets: Vec<Pid>) {
    for &p in &targets {
        rt.kill(p);
    }
    while !targets.is_empty() {
        if let Received::Down { pid, .. } = ctx.recv().await {
            targets.retain(|&p| p != pid);
        }
    }
}

/// Records this restart and reports whether it blows the intensity budget.
fn over_budget(
    window: &mut Vec<Instant>,
    lifetime: &mut u32,
    within: Option<Duration>,
    max_restarts: u32,
) -> bool {
    match within {
        Some(span) => {
            let now = Instant::now();
            window.push(now);
            window.retain(|t| now.duration_since(*t) <= span);
            max_restarts != 0 && window.len() as u32 > max_restarts
        }
        None => {
            *lifetime += 1;
            max_restarts != 0 && *lifetime > max_restarts
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    /// A child that registers itself under `name` and then idles — so a test can
    /// find it by name and observe a restart as a *changed* pid.
    fn named(name: &'static str) -> impl Fn(&Runtime) -> ProcessHandle + Send + Sync + 'static {
        move |r: &Runtime| {
            let rt = r.clone();
            let name = name.to_string();
            r.spawn(move |ctx| async move {
                rt.register(name, ctx.pid());
                std::future::pending::<()>().await
            })
        }
    }

    async fn wait_for(rt: &Runtime, name: &str) -> Pid {
        loop {
            if let Some(pid) = rt.whereis(name) {
                return pid;
            }
            tokio::task::yield_now().await;
        }
    }

    async fn wait_for_change(rt: &Runtime, name: &str, old: Pid) -> Pid {
        loop {
            if let Some(pid) = rt.whereis(name) {
                if pid != old {
                    return pid;
                }
            }
            tokio::task::yield_now().await;
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn one_for_one_restarts_only_the_failed_child() {
        let rt = Runtime::new();
        let sup = rt
            .supervisor(Strategy::OneForOne)
            .child(named("c0"))
            .child(named("c1"))
            .start();
        let c0 = wait_for(&rt, "c0").await;
        let c1 = wait_for(&rt, "c1").await;

        rt.kill(c0);
        let c0b = wait_for_change(&rt, "c0", c0).await;

        assert_ne!(c0b, c0, "the failed child is restarted with a fresh pid");
        assert_eq!(rt.whereis("c1"), Some(c1), "a sibling is left untouched");
        sup.kill();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn one_for_all_restarts_every_child() {
        let rt = Runtime::new();
        let sup = rt
            .supervisor(Strategy::OneForAll)
            .child(named("a0"))
            .child(named("a1"))
            .start();
        let a0 = wait_for(&rt, "a0").await;
        let a1 = wait_for(&rt, "a1").await;

        rt.kill(a0);
        let a0b = wait_for_change(&rt, "a0", a0).await;
        let a1b = wait_for_change(&rt, "a1", a1).await;

        assert_ne!(a0b, a0);
        assert_ne!(
            a1b, a1,
            "every child is restarted, not just the one that died"
        );
        sup.kill();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn rest_for_one_restarts_the_failed_and_later_children() {
        let rt = Runtime::new();
        let sup = rt
            .supervisor(Strategy::RestForOne)
            .child(named("r0"))
            .child(named("r1"))
            .child(named("r2"))
            .start();
        let r0 = wait_for(&rt, "r0").await;
        let r1 = wait_for(&rt, "r1").await;
        let r2 = wait_for(&rt, "r2").await;

        rt.kill(r1);
        let r1b = wait_for_change(&rt, "r1", r1).await;
        let r2b = wait_for_change(&rt, "r2", r2).await;

        assert_eq!(
            rt.whereis("r0"),
            Some(r0),
            "children before the failure stay"
        );
        assert_ne!(r1b, r1, "the failed child restarts");
        assert_ne!(r2b, r2, "and every child after it restarts");
        sup.kill();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn restart_intensity_stops_the_supervisor() {
        let rt = Runtime::new();
        // A child that crashes the instant it starts — forces repeated restarts.
        let crashes = Arc::new(AtomicU32::new(0));
        let counter = crashes.clone();
        let crasher = move |r: &Runtime| {
            let counter = counter.clone();
            r.spawn(move |_ctx| async move {
                counter.fetch_add(1, Ordering::Relaxed);
                panic!("boom");
            })
        };
        let _sup = rt
            .supervisor(Strategy::OneForOne)
            .max_restarts(3)
            .within(Duration::from_secs(60))
            .child(crasher)
            .start();

        // Once the supervisor blows its budget it kills its children and exits, so
        // every process drains. With max_restarts=3 that's the initial start plus 3
        // restarts = 4 crashes, then the supervisor gives up.
        loop {
            if rt.process_count() == 0 {
                break;
            }
            tokio::task::yield_now().await;
        }
        assert_eq!(
            crashes.load(Ordering::Relaxed),
            4,
            "initial start + 3 restarts, then the supervisor gives up"
        );
    }
}
