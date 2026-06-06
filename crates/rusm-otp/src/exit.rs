/// Why a process terminated — carried to linked and monitoring processes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExitReason {
    /// Ran to completion, or was asked to stop cleanly. Does **not** propagate
    /// down links (linked peers survive unless they trap exits).
    Normal,
    /// Stopped by [`kill`](crate::Runtime::kill) or a link cascade.
    Killed,
    /// The body panicked — Erlang's "let it crash".
    Crashed,
    /// Reported by a monitor when its target was already gone (Erlang's `noproc`).
    NoProc,
}

impl ExitReason {
    /// Whether this reason propagates death down links — everything but
    /// [`Normal`](ExitReason::Normal), matching Erlang's exit-signal rules.
    pub fn is_abnormal(self) -> bool {
        !matches!(self, ExitReason::Normal)
    }
}

/// Identifies a monitor set up with [`monitor`](crate::Runtime::monitor); echoed
/// back in the resulting [`Received::Down`](crate::Received::Down) so a watcher
/// can tell its monitors apart.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct MonitorRef(pub(crate) u64);

impl MonitorRef {
    pub fn raw(self) -> u64 {
        self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_normal_is_not_abnormal() {
        assert!(!ExitReason::Normal.is_abnormal());
        assert!(ExitReason::Killed.is_abnormal());
        assert!(ExitReason::Crashed.is_abnormal());
        assert!(ExitReason::NoProc.is_abnormal());
    }

    #[test]
    fn monitor_ref_exposes_its_raw_id() {
        assert_eq!(MonitorRef(7).raw(), 7);
    }
}
