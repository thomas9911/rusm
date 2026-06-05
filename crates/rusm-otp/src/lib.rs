//! `rusm-otp` — the Wasm-free Erlang/OTP core of RUSM.
//!
//! Lightweight processes (Tokio tasks) with a signal-driven lifecycle and a
//! process table. This crate must never depend on or reference Wasmtime — the
//! actor model stands alone; Wasm is a separate, optional backend (`rusm-wasm`).
//! See `docs/01-architecture.md`.

mod pid;
mod runtime;
mod signal;

pub use pid::Pid;
pub use runtime::{Context, ProcessHandle, Runtime};
pub use signal::Signal;
