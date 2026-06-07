//! A guest **Supervisor** — the OTP pattern over the actor ABI: spawn children by
//! name, `monitor` them, and restart per a strategy when one dies. Event-driven
//! (a dead child arrives as a `__down` message — no polling).

use std::time::{Duration, Instant};

use crate::{kill, monitor, receive_bytes, spawn, Pid};

/// How a supervisor reacts when one child dies.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Strategy {
    /// Restart only the child that died.
    OneForOne,
    /// Restart **all** children (terminate the survivors first).
    OneForAll,
    /// Restart the dead child and every child started **after** it.
    RestForOne,
}

/// A supervisor of named child components. Build it, then [`run`](Supervisor::run)
/// it as the process body (a component's `run` calls this).
pub struct Supervisor {
    strategy: Strategy,
    children: Vec<String>,
    /// Max restarts before the supervisor gives up and exits (0 = unlimited).
    max_restarts: u32,
    /// Restart-intensity window. `None` counts `max_restarts` over the whole
    /// lifetime; `Some(w)` counts only restarts within the last `w` (Erlang's
    /// `{max_restarts, max_seconds}`).
    within: Option<Duration>,
}

impl Supervisor {
    /// A new supervisor with the given restart [`Strategy`].
    pub fn new(strategy: Strategy) -> Self {
        Self {
            strategy,
            children: Vec::new(),
            max_restarts: 0,
            within: None,
        }
    }

    /// Add a child by the component name it was registered under.
    pub fn child(mut self, component: &str) -> Self {
        self.children.push(component.to_string());
        self
    }

    /// Give up (return) after this many restarts — overload protection. By default
    /// this counts over the supervisor's whole lifetime; pair it with
    /// [`within`](Supervisor::within) for a sliding window. 0 (the default) means
    /// never give up.
    pub fn max_restarts(mut self, n: u32) -> Self {
        self.max_restarts = n;
        self
    }

    /// Bound [`max_restarts`](Supervisor::max_restarts) to a sliding **time
    /// window** — give up only if more than `max_restarts` happen within `window`
    /// (Erlang's restart *intensity*, `{max_restarts, max_seconds}`). Restarts
    /// spread out wider than the window never trip it, so an occasional crash over a
    /// long uptime won't eventually exhaust the budget. Without this, the budget is
    /// the supervisor's whole lifetime.
    pub fn within(mut self, window: Duration) -> Self {
        self.within = Some(window);
        self
    }

    /// Spawn + monitor every child, then supervise forever: on each child death,
    /// restart per the strategy. Returns only if `max_restarts` is exceeded.
    pub fn run(self) {
        let mut pids: Vec<Pid> = self.children.iter().map(|c| start(c)).collect();
        // Lifetime mode counts; windowed mode keeps the restart instants in-window.
        let mut lifetime = 0u32;
        let mut window: Vec<Instant> = Vec::new();
        loop {
            let raw = receive_bytes();
            let Some(dead) = parse_down(&raw) else {
                continue;
            };
            let Some(index) = pids.iter().position(|&p| p == dead) else {
                continue; // a Down for something we don't supervise (already restarted)
            };
            let over_budget = match self.within {
                Some(span) => {
                    let now = Instant::now();
                    window.push(now);
                    window.retain(|t| now.duration_since(*t) <= span);
                    self.max_restarts != 0 && window.len() as u32 > self.max_restarts
                }
                None => {
                    lifetime += 1;
                    self.max_restarts != 0 && lifetime > self.max_restarts
                }
            };
            if over_budget {
                return; // overload — let the supervisor itself crash/stop
            }
            match self.strategy {
                Strategy::OneForOne => {
                    pids[index] = start(&self.children[index]);
                }
                Strategy::OneForAll => {
                    for (j, &p) in pids.iter().enumerate() {
                        if j != index {
                            kill(p); // the dead one is already gone
                        }
                    }
                    pids = self.children.iter().map(|c| start(c)).collect();
                }
                Strategy::RestForOne => {
                    for &p in &pids[index + 1..] {
                        kill(p);
                    }
                    for j in index..pids.len() {
                        pids[j] = start(&self.children[j]);
                    }
                }
            }
        }
    }
}

/// Spawn a child and monitor it, so its death comes back as a `__down`.
fn start(component: &str) -> Pid {
    let pid = spawn(component).expect("supervisor child spawns");
    monitor(pid);
    pid
}

/// The pid in a `{"__down":"<pid>","reason":"…"}` message, if that's what `raw` is.
fn parse_down(raw: &[u8]) -> Option<Pid> {
    let v: serde_json::Value = serde_json::from_slice(raw).ok()?;
    v.get("__down")?.as_str()?.parse().ok().map(Pid)
}
