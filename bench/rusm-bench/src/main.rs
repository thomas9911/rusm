use std::time::{Duration, Instant};

use rusm_bench::{serve, summarize_frame, Node, ResourceProfile, Runner, RunnerConfig, Scenario};

const DEFAULT_ADDR: &str = "127.0.0.1:4000";

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("serve") => {
            let addr = args
                .get(2)
                .cloned()
                .unwrap_or_else(|| DEFAULT_ADDR.to_string());
            let node = Node::new(RunnerConfig::default());
            println!("rusm-bench serving on ws://{addr}");
            if let Err(error) = serve(&addr, node).await {
                eprintln!("server error: {error}");
                std::process::exit(1);
            }
        }
        Some("run") => match args.get(2).and_then(|id| Scenario::from_id(id)) {
            Some(scenario) => {
                let seconds = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(5);
                let profile = args
                    .get(4)
                    .and_then(|id| ResourceProfile::from_id(id))
                    .unwrap_or_default();
                run_terminal(scenario, seconds, profile).await;
            }
            None => {
                eprintln!("usage: rusm-bench run <scenario> [seconds] [light|balanced|max]");
                eprintln!("scenarios: {}", Scenario::ALL.map(|s| s.id()).join(", "));
                std::process::exit(2);
            }
        },
        _ => {
            eprintln!("usage: rusm-bench <serve [addr] | run <scenario> [seconds] [profile]>");
            std::process::exit(2);
        }
    }
}

async fn run_terminal(scenario: Scenario, seconds: u64, profile: ResourceProfile) {
    let config = RunnerConfig::default();
    let rate = u64::from(config.ticks_per_second.max(1));
    let mut runner = Runner::new(config);
    runner.set_resource_profile(profile);
    runner.start(scenario);

    let mut interval = tokio::time::interval(Duration::from_millis(1_000 / rate));
    let start = Instant::now();
    let print_every = (rate / 4).max(1);
    for tick in 0..seconds * rate {
        interval.tick().await;
        let frame = runner.tick(start.elapsed().as_millis() as u64);
        if tick % print_every == 0 {
            println!("{}", summarize_frame(&frame));
        }
    }
}
