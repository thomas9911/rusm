//! A guest **Supervisor** — the OTP pattern over the actor ABI: spawn children by
//! name, `monitor` them, and restart per a strategy when one dies. Event-driven
//! (a dead child arrives as a `__down` message — no polling).

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
}

impl Supervisor {
    /// A new supervisor with the given restart [`Strategy`].
    pub fn new(strategy: Strategy) -> Self {
        Self {
            strategy,
            children: Vec::new(),
            max_restarts: 0,
        }
    }

    /// Add a child by the component name it was registered under.
    pub fn child(mut self, component: &str) -> Self {
        self.children.push(component.to_string());
        self
    }

    /// Give up (return) after this many restarts — overload protection (Erlang's
    /// restart intensity). 0 (the default) means never give up.
    pub fn max_restarts(mut self, n: u32) -> Self {
        self.max_restarts = n;
        self
    }

    /// Spawn + monitor every child, then supervise forever: on each child death,
    /// restart per the strategy. Returns only if `max_restarts` is exceeded.
    pub fn run(self) {
        let mut pids: Vec<Pid> = self.children.iter().map(|c| start(c)).collect();
        let mut restarts = 0u32;
        loop {
            let raw = receive_bytes();
            let Some(dead) = parse_down(&raw) else {
                continue;
            };
            let Some(index) = pids.iter().position(|&p| p == dead) else {
                continue; // a Down for something we don't supervise (already restarted)
            };
            restarts += 1;
            if self.max_restarts != 0 && restarts > self.max_restarts {
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
