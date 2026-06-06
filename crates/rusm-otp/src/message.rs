use crate::exit::{ExitReason, MonitorRef};
use crate::pid::Pid;

/// A user message delivered to a process mailbox.
///
/// Opaque bytes, handed to the recipient **by value** — ownership moves into its
/// mailbox, so processes never share memory, exactly like Erlang. Mailboxes are
/// unbounded (also like Erlang). Phase 2 carries raw bytes; the Wasm backend
/// (Phase 6) will copy these across isolated guest memories unchanged.
pub type Message = Vec<u8>;

/// What a process pulls from its mailbox with [`recv`](crate::Context::recv): an
/// ordinary message, or a system notification the runtime injected. Exit and
/// down signals share the one mailbox (and FIFO order) with user messages —
/// exactly as Erlang delivers `{'EXIT', …}` / `{'DOWN', …}`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Received {
    /// A user message sent with [`send`](crate::Runtime::send).
    Message(Message),
    /// A monitored process terminated (set up with [`monitor`](crate::Runtime::monitor)).
    Down {
        reference: MonitorRef,
        pid: Pid,
        reason: ExitReason,
    },
    /// A linked process exited while this one traps exits (see
    /// [`link`](crate::Runtime::link) and [`set_trap_exit`](crate::Runtime::set_trap_exit)).
    Exit { from: Pid, reason: ExitReason },
}

impl Received {
    /// The user-message bytes, or `None` for a system notification — convenient
    /// when a process only cares about ordinary messages.
    pub fn message(self) -> Option<Message> {
        match self {
            Received::Message(bytes) => Some(bytes),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exit::{ExitReason, MonitorRef};

    #[test]
    fn message_extracts_user_bytes_only() {
        assert_eq!(
            Received::Message(b"hi".to_vec()).message(),
            Some(b"hi".to_vec())
        );
        let down = Received::Down {
            reference: MonitorRef(1),
            pid: Pid::from_raw(2),
            reason: ExitReason::Crashed,
        };
        assert_eq!(down.message(), None);
        let exit = Received::Exit {
            from: Pid::from_raw(3),
            reason: ExitReason::Normal,
        };
        assert_eq!(exit.message(), None);
    }
}
