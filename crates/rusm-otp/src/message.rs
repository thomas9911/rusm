/// A message delivered to a process mailbox.
///
/// Opaque bytes, handed to the recipient **by value** — ownership moves into its
/// mailbox, so processes never share memory, exactly like Erlang. Mailboxes are
/// unbounded (also like Erlang). Phase 2 carries raw bytes; the Wasm backend
/// (Phase 6) will copy these across isolated guest memories unchanged.
pub type Message = Vec<u8>;
