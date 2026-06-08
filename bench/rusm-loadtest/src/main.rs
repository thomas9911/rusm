//! `rusm-loadtest` — an **out-of-process** load driver for a `rusm serve` node.
//!
//! This is the *fair* half of the benchmark: the node only serves (a real TCP
//! port, hosting a WASM component); load is generated here, in a separate process,
//! so the generator never steals CPU from the server and the numbers are the
//! server's. It hits the server over the real network with battle-proven clients.
//!
//! Two measurement models, each matched to the workload:
//!
//! * **HTTP** (`http`) — request/response *throughput*. Driven by [`balter`], which
//!   ramps TPS until a target error-rate / tail-latency SLA is reached and reports
//!   the **max sustainable req/s**. The number is *earned* (found by saturation),
//!   not asserted. A fixed `--tps` overrides saturation.
//! * **WS / SSE** (`ws` / `sse`) — connection *capacity*: many long-lived
//!   connections held at once, each sustaining echo round-trips (WS) or draining
//!   events (SSE). Reported as concurrency + sustained ops/sec + p50/p99 — the
//!   honest metric for held connections, which a request-rate model misrepresents.
//!
//! Usage:
//! ```text
//! rusm-loadtest http <url> [--duration S] [--tps N | --error-rate F --latency-ms M --quantile Q]
//! rusm-loadtest ws   <url> [--duration S] [--connections N]
//! rusm-loadtest sse  <url> [--duration S] [--connections N]
//! ```

mod capacity;
mod http;

use std::time::Duration;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let mode = args.get(1).map(String::as_str);
    let url = args.get(2).cloned();

    match (mode, url) {
        (Some("http"), Some(url)) => http::run(Opts::parse(&args), url),
        (Some("ws"), Some(url)) => capacity::run(capacity::Protocol::Ws, Opts::parse(&args), url),
        (Some("sse"), Some(url)) => capacity::run(capacity::Protocol::Sse, Opts::parse(&args), url),
        _ => {
            eprintln!("{USAGE}");
            std::process::exit(2);
        }
    }
}

const USAGE: &str = "\
rusm-loadtest — out-of-process load driver for a `rusm serve` node

USAGE:
  rusm-loadtest http <url> [options]   saturate HTTP to find max sustainable req/s
  rusm-loadtest ws   <url> [options]   hold N WS connections, sustained echo round-trips/s
  rusm-loadtest sse  <url> [options]   hold N SSE streams, sustained events/s

OPTIONS:
  --duration <secs>      test length (default 15)
  --connections <n>      held connections for ws/sse (default 512)
  --tps <n>              HTTP: drive a fixed TPS instead of saturating
  --error-rate <0..1>    HTTP saturation: ramp until this error rate (default 0.02)
  --latency-ms <ms>      HTTP saturation: also cap tail latency (default 250)
  --quantile <0..1>      HTTP latency quantile to cap (default 0.99)
  --start-tps <n>        HTTP saturation: seed the search here so it converges fast
                         instead of crawling up from balter's 512 default (default 10000)";

/// Common knobs, parsed from `--flag value` pairs (dependency-light, matching the
/// rest of the repo's CLI style).
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
    fn parse(args: &[String]) -> Self {
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
