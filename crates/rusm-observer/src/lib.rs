//! Live observer for a running RUSM node: aggregate counters plus a sampled
//! per-instance table, captured as [`ObserverSnapshot`]s for the dashboard and
//! the `rusm attach` REPL. Designed to be cheap enough to leave on under load.

mod observer;
mod types;

pub use observer::Observer;
pub use types::{ObserverSnapshot, ProcessInfo, ProcessStatus};
