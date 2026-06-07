//! The RUSM benchmark harness: scenarios, a deterministic synthetic data source,
//! a runner that aggregates ticks into transportable frames, and the WebSocket
//! server feeding the dashboard and the `rusm attach` REPL.

mod componentstorm;
mod config;
mod connectionscale;
mod connectionstorm;
mod distributedfanout;
mod fairness;
mod faultrecovery;
mod httpthroughput;
mod modulestorm;
mod pingpong;
mod profile;
mod protocol;
mod report;
mod runner;
mod sample;
mod scenario;
mod server;
mod spawnstorm;
mod ssefanout;
mod streampipe;
mod synthetic;
mod wsecho;

pub use config::{CapabilitySpec, ComponentSpec, NodeConfig, PreopenSpec};
pub use profile::{ResourceProfile, ResourceProfileMeta};
pub use protocol::{ClientCommand, Frame, ServerMessage};
pub use report::summarize_frame;
pub use runner::{Runner, RunnerConfig};
pub use sample::Sample;
pub use scenario::{MetricUnit, Scenario, ScenarioMeta};
pub use server::{serve, serve_on, Node};
pub use synthetic::SyntheticSource;
