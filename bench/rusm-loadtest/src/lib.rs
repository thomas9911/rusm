//! `rusm-loadtest` — an out-of-process load driver for a `rusm serve` node, also
//! reused (as a library) by the benchmark dashboard's serving engines so both share
//! one battle-tested load path.
//!
//! * [`http`] — request/response throughput via [`balter`] (a fixed-rate sweep that
//!   finds the max sustained req/s at an SLA; or a single bounded window the
//!   dashboard polls live).
//! * [`capacity`] — connection-capacity load for WS/SSE (hold N connections,
//!   sustain echo round-trips / drain events), exposed as a live [`capacity::CapacityLoad`]
//!   handle.
//!
//! The binary ([`main`](../main/index.html)) wraps these for the CLI; the dashboard
//! drives them live.

pub mod capacity;
pub mod http;

use std::time::Duration;

/// Common knobs for the CLI, parsed from `--flag value` pairs (dependency-light,
/// matching the rest of the repo's CLI style).
pub struct Opts {
    pub duration: Duration,
    pub connections: usize,
    pub tps: Option<u32>,
    pub error_rate: f64,
    pub latency: Duration,
    pub quantile: f64,
    pub start_tps: u32,
}

impl Opts {
    pub fn parse(args: &[String]) -> Self {
        let flag = |name: &str| -> Option<String> {
            args.iter()
                .position(|a| a == name)
                .and_then(|i| args.get(i + 1))
                .cloned()
        };
        let parse = |name: &str| flag(name).and_then(|v| v.parse::<f64>().ok());
        Self {
            duration: Duration::from_secs(parse("--duration").map_or(15, |v| v as u64)),
            connections: parse("--connections").map_or(512, |v| v as usize),
            tps: parse("--tps").map(|v| v as u32),
            error_rate: parse("--error-rate").unwrap_or(0.02),
            latency: Duration::from_millis(parse("--latency-ms").map_or(250, |v| v as u64)),
            quantile: parse("--quantile").unwrap_or(0.99),
            start_tps: parse("--start-tps").map_or(10_000, |v| v as u32),
        }
    }
}
