//! `rusm-loadtest` — out-of-process load driver for a `rusm serve` node.
//!
//! The node only serves (a real TCP port hosting a WASM component); load is
//! generated here, in a separate process, so the generator never steals CPU from
//! the server and the numbers are the server's. See the [library](rusm_loadtest)
//! for the reusable load path (also driven live by the dashboard).
//!
//! Usage:
//! ```text
//! rusm-loadtest http <url> [--duration S] [--tps N | --start-tps N --error-rate F --latency-ms M --quantile Q]
//! rusm-loadtest ws   <url> [--duration S] [--connections N]
//! rusm-loadtest sse  <url> [--duration S] [--connections N]
//! ```

use rusm_loadtest::{capacity, http, Opts};

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
  --tps <n>              HTTP: drive a fixed TPS instead of sweeping
  --error-rate <0..1>    HTTP sweep: SLA error-rate budget (default 0.02)
  --latency-ms <ms>      HTTP sweep: SLA tail-latency budget (default 250)
  --quantile <0..1>      HTTP sweep: latency quantile to bound (default 0.99)
  --start-tps <n>        HTTP sweep: first offered rate, doubled each step (default 10000)";
