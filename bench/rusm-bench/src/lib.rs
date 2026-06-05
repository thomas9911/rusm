//! The RUSM benchmark harness: scenarios, a deterministic synthetic data source,
//! a runner that aggregates ticks into transportable frames, and the WebSocket
//! server feeding the dashboard and the `rusm attach` REPL.

mod protocol;
mod report;
mod runner;
mod scenario;
mod server;
mod synthetic;

pub use protocol::{ClientCommand, Frame, ServerMessage};
pub use report::summarize_frame;
pub use runner::{Runner, RunnerConfig};
pub use scenario::{Scenario, ScenarioMeta};
pub use server::{serve, serve_on, Node};
pub use synthetic::{SyntheticSource, SyntheticTick};
