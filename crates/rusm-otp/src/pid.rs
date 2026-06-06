use std::fmt;

/// A process identifier, unique within a [`Runtime`](crate::Runtime).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Pid(pub(crate) u64);

impl Pid {
    pub fn raw(self) -> u64 {
        self.0
    }

    /// Rebuilds a pid from its [`raw`](Pid::raw) id — e.g. after carrying it in a
    /// message or across a node boundary. The id need not be live; sending to a
    /// stale pid is simply a no-op.
    pub fn from_raw(raw: u64) -> Pid {
        Pid(raw)
    }
}

impl fmt::Display for Pid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "#{}", self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn raw_display_and_ordering() {
        let p = Pid(7);
        assert_eq!(p.raw(), 7);
        assert_eq!(p.to_string(), "#7");
        assert_eq!(format!("{p:?}"), "Pid(7)");
        assert!(Pid(1) < Pid(2));
        assert_eq!(Pid(3), Pid(3));
    }

    #[test]
    fn from_raw_round_trips() {
        assert_eq!(Pid::from_raw(42).raw(), 42);
        assert_eq!(Pid::from_raw(9), Pid(9));
    }
}
