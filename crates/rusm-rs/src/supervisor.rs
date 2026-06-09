//! A guest **Supervisor** — a thin, ergonomic facade over the host's single native
//! supervisor (the `supervise` actor ABI). Build it (strategy, children, restart
//! intensity), then [`run`](Supervisor::run) it as the component body: the host
//! spawns and supervises the named children under one restart implementation, links
//! the supervisor to this process, and tears the whole tree down on give-up or our
//! death. (Previously this loop lived here *and* in the TS runner; now there is one.)

use std::time::Duration;

use crate::rusm::runtime::actor;

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
    /// Max restarts before the supervisor gives up (0 = unlimited).
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

    /// Give up after this many restarts — overload protection. By default this counts
    /// over the supervisor's whole lifetime; pair it with [`within`](Supervisor::within)
    /// for a sliding window. 0 (the default) means never give up.
    pub fn max_restarts(mut self, n: u32) -> Self {
        self.max_restarts = n;
        self
    }

    /// Bound [`max_restarts`](Supervisor::max_restarts) to a sliding **time window** —
    /// give up only if more than `max_restarts` happen within `window` (Erlang's
    /// restart *intensity*). Without this, the budget is the whole lifetime.
    pub fn within(mut self, window: Duration) -> Self {
        self.within = Some(window);
        self
    }

    /// Hand the children to the host's native supervisor and run as its owner: the
    /// host spawns + monitors + restarts them under one implementation, and links the
    /// supervisor to us. Returns only if the host call is rejected (e.g. the spawn
    /// capability is missing); otherwise it parks — the link tears us down when the
    /// supervisor gives up, and tears the children down if we're killed.
    pub fn run(self) {
        let strategy = match self.strategy {
            Strategy::OneForOne => actor::SuperviseStrategy::OneForOne,
            Strategy::OneForAll => actor::SuperviseStrategy::OneForAll,
            Strategy::RestForOne => actor::SuperviseStrategy::RestForOne,
        };
        let within_ms = self.within.map_or(0, |d| d.as_millis() as u32);
        if actor::supervise(strategy, &self.children, self.max_restarts, within_ms).is_err() {
            return; // supervision denied (no spawn capability) — nothing to own
        }
        // Park: stay alive as the supervisor's owner until the link takes us.
        loop {
            let _ = crate::receive_bytes();
        }
    }
}
