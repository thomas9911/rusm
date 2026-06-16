use std::path::Path;
use std::time::{Duration, Instant};

use rusm_bench::{
    runner_config, serve, summarize_frame, ClientCommand, Node, NodeConfig, ResourceProfile,
    Runner, RunnerConfig, Scenario,
};

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        // The dashboard / observer node: serves the benchmark scenarios over
        // WebSocket (what `make dashboard` and `make node` launch).
        Some("start") => {
            let cfg = load_config(&args);
            let node = Node::new(runner_config(&cfg));
            // Apply the startup profile (sets the spawn tuning, reflected in frames).
            let _ = node.apply(ClientCommand::SetResourceProfile {
                profile: cfg.node.profile.id().to_string(),
            });
            println!(
                "rusm-bench node listening on ws://{} (profile: {}, {} Hz)",
                cfg.node.listen,
                cfg.node.profile.id(),
                cfg.node.ticks_per_second
            );
            if let Err(error) = serve(&cfg.node.listen, node).await {
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
            eprintln!(
                "usage: rusm-bench <start [--config <file>] [--listen <addr>] [--profile light|balanced|max] | run <scenario> [seconds] [profile]>"
            );
            std::process::exit(2);
        }
    }
}

/// Loads the node manifest (`rusm.toml` by default) and applies `--listen` /
/// `--profile` overrides — the same layering as the `rusm` CLI.
fn load_config(args: &[String]) -> NodeConfig {
    let explicit = flag(args, "--config");
    let path = explicit.clone().unwrap_or_else(|| "rusm.toml".to_string());
    let mut cfg = NodeConfig::load(Path::new(&path), explicit.is_some()).unwrap_or_else(|error| {
        eprintln!("{error}");
        std::process::exit(2);
    });
    if let Some(listen) = flag(args, "--listen") {
        cfg.node.listen = listen;
    }
    if let Some(profile) = flag(args, "--profile") {
        cfg.node.profile = ResourceProfile::from_id(&profile).unwrap_or_else(|| {
            eprintln!("unknown profile: {profile} (use light | balanced | max)");
            std::process::exit(2);
        });
    }
    cfg
}

/// The value following `name` in `args`, if present (e.g. `--listen <addr>`).
fn flag(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1))
        .cloned()
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
