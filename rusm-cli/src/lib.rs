//! Logic for the `rusm` CLI, kept separate from the I/O glue in `main.rs` so it
//! is unit-testable: REPL command parsing and live-message formatting.

mod app;
mod endpoint;
mod render;
mod repl;

pub use app::{capabilities_for, spawn_components};
pub use endpoint::{normalize_target, DEFAULT_HOST};
pub use render::render_message;
pub use repl::{parse, ReplInput, HELP};
