//! `rusm-otp` — the Wasm-free Erlang/OTP core of RUSM.
//!
//! Lightweight processes (Tokio tasks), each with a message mailbox, over a
//! sharded process table. A process is killed by aborting its task — Tokio gives
//! us that handle for free — so a process carries just one channel. This crate
//! must never depend on or reference Wasmtime — the actor model stands alone;
//! Wasm is a separate, optional backend (`rusm-wasm`). See `docs/01-architecture.md`.

mod exit;
mod message;
mod pid;
mod runtime;

pub use exit::{ExitReason, MonitorRef};
pub use message::{Message, Received};
pub use pid::Pid;
pub use runtime::{Context, ProcessHandle, Runtime};
