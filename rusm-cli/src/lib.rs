//! Logic for the `rusm` CLI, kept separate from the I/O glue in `main.rs` so it
//! is unit-testable: REPL command parsing and live-message formatting.

mod app;
mod endpoint;
mod render;
mod repl;
mod scaffold;

pub use app::{capabilities_for, serve_apps, spawn_components, Hosted, ServedEndpoint};
pub use endpoint::{normalize_target, DEFAULT_HOST};
pub use render::render_message;
pub use repl::{parse, ReplInput, HELP};
pub use scaffold::{parse_new_args, scaffold, Lang, NewApp, Protocol};
