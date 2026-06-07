use std::path::Path;
use std::process::Command;

use anyhow::{anyhow, Context};
use futures_util::{SinkExt, StreamExt};
use rusm_bench::{serve, ClientCommand, Node, NodeConfig, ResourceProfile};
use rusm_cli::{
    normalize_target, parse, render_message, spawn_components, ReplInput, DEFAULT_HOST, HELP,
};
use rusm_otp::Runtime;
use rusm_wasm::WasmRuntime;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio_tungstenite::tungstenite::Message;

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().collect();
    let command = args.get(1).map(String::as_str);
    let subcommand = args.get(2).map(String::as_str);

    if command == Some("node") && subcommand == Some("start") {
        let cfg = load_node_config(&args);
        let node = Node::new(cfg.runner_config());
        // Apply the startup profile (sets the spawn tuning and is reflected in frames).
        let _ = node.apply(ClientCommand::SetResourceProfile {
            profile: cfg.profile.id().to_string(),
        });
        println!(
            "rusm node listening on ws://{} (profile: {}, {} Hz)",
            cfg.listen,
            cfg.profile.id(),
            cfg.ticks_per_second
        );
        if let Err(error) = serve(&cfg.listen, node).await {
            eprintln!("node error: {error}");
            std::process::exit(1);
        }
    } else if command == Some("attach") {
        // Target defaults to the local node and accepts host / host:port / ws-url.
        let target = normalize_target(args.get(2).map(String::as_str).unwrap_or(DEFAULT_HOST));
        if let Err(error) = attach(&target).await {
            eprintln!("attach failed: {error}");
            std::process::exit(1);
        }
    } else if command == Some("build") {
        // Compile components/<name>/ -> wasm/<name>.wasm (one toolchain, no jco).
        match build_components(Path::new(".")) {
            Ok(built) if built.is_empty() => {
                println!("no component crates found under ./components");
            }
            Ok(built) => println!(
                "built {} component(s) -> ./wasm: {}",
                built.len(),
                built.join(", ")
            ),
            Err(error) => {
                eprintln!("build failed: {error}");
                std::process::exit(1);
            }
        }
    } else if command == Some("run") || command == Some("dev") {
        // `dev` auto-builds first (write source, RUSM compiles); both then run the
        // app's components from ./wasm per the rusm.toml manifest.
        if command == Some("dev") {
            match build_components(Path::new(".")) {
                Ok(built) => println!("built: {}", built.join(", ")),
                Err(error) => {
                    eprintln!("build failed: {error}");
                    std::process::exit(1);
                }
            }
        }
        if let Err(error) = run_app(&args).await {
            eprintln!("run failed: {error}");
            std::process::exit(1);
        }
    } else {
        eprintln!("usage:");
        eprintln!(
            "  rusm node start [--config <file>] [--listen <addr>] [--profile light|balanced|max]"
        );
        eprintln!("  rusm build                 compile ./components/* -> ./wasm/*.wasm");
        eprintln!(
            "  rusm run                   run ./wasm components per rusm.toml [[components]]"
        );
        eprintln!("  rusm dev                   build, then run (write source, RUSM compiles)");
        eprintln!("  rusm attach [<host | host:port | ws-url>]   (defaults to 127.0.0.1:4000)");
        std::process::exit(2);
    }
}

/// Runs the app's components: load `.env` (process env wins), then spawn each
/// `[[components]]` entry from `./wasm` under its capability profile, and wait
/// for Ctrl-C. Held handles + runtime keep the processes alive.
async fn run_app(args: &[String]) -> anyhow::Result<()> {
    // Environment variables the Rust way: process env first, then ./.env.
    dotenvy::dotenv().ok();

    let cfg = load_node_config(args);
    let rt = Runtime::new();
    let wasm = WasmRuntime::new(rt.clone())?;
    let handles = spawn_components(Path::new("."), &wasm, &cfg.components)?;
    if handles.is_empty() {
        println!("no [[components]] in rusm.toml — nothing to run");
        return Ok(());
    }
    let names: Vec<&str> = handles.iter().map(|(n, _)| n.as_str()).collect();
    println!(
        "running {} component(s): {}",
        handles.len(),
        names.join(", ")
    );
    println!("press Ctrl-C to stop");
    tokio::signal::ctrl_c().await?;
    println!("\nstopping {} component(s)…", rt.shutdown());
    Ok(())
}

/// Builds every component crate under `<dir>/components/<name>/` to a
/// `wasm32-wasip2` component and copies it to `<dir>/wasm/<name>.wasm`. One
/// toolchain — `cargo build --target wasm32-wasip2 --release` — so a TS component
/// is just a Rust crate whose `build.rs` bundles TS via Bun (no jco). Returns the
/// built component names. (Shell-orchestration glue, hence it lives in `main`.)
fn build_components(dir: &Path) -> anyhow::Result<Vec<String>> {
    let components_dir = dir.join("components");
    let wasm_dir = dir.join("wasm");
    std::fs::create_dir_all(&wasm_dir)?;

    let mut entries: Vec<_> = std::fs::read_dir(&components_dir)
        .with_context(|| format!("reading {}", components_dir.display()))?
        .filter_map(|e| e.ok())
        .filter(|e| e.path().join("Cargo.toml").is_file())
        .collect();
    entries.sort_by_key(|e| e.file_name());

    let mut built = Vec::new();
    for entry in entries {
        let crate_dir = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        let status = Command::new("cargo")
            .args(["build", "--target", "wasm32-wasip2", "--release"])
            .current_dir(&crate_dir)
            .status()
            .with_context(|| "running cargo (is the wasm32-wasip2 target installed?)")?;
        if !status.success() {
            return Err(anyhow!("`cargo build` failed for component `{name}`"));
        }
        // Cargo names the artifact after the crate (dashes become underscores).
        let artifact = crate_dir
            .join("target/wasm32-wasip2/release")
            .join(format!("{}.wasm", name.replace('-', "_")));
        let dest = wasm_dir.join(format!("{name}.wasm"));
        std::fs::copy(&artifact, &dest)
            .with_context(|| format!("copying {} -> {}", artifact.display(), dest.display()))?;
        built.push(name);
    }
    Ok(built)
}

fn flag(args: &[String], name: &str) -> Option<String> {
    let idx = args.iter().position(|a| a == name)?;
    args.get(idx + 1).cloned()
}

/// Loads node config: defaults → `rusm.toml` (or `--config <file>`) → CLI flags.
fn load_node_config(args: &[String]) -> NodeConfig {
    let explicit = flag(args, "--config");
    let path = explicit.clone().unwrap_or_else(|| "rusm.toml".to_string());
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => Some(text),
        // A missing default rusm.toml is fine; a missing explicit --config is not.
        Err(_) if explicit.is_none() => None,
        Err(error) => {
            eprintln!("cannot read {path}: {error}");
            std::process::exit(2);
        }
    };
    let mut cfg = match text {
        Some(text) => NodeConfig::from_toml(&text).unwrap_or_else(|error| {
            eprintln!("invalid {path}: {error}");
            std::process::exit(2);
        }),
        None => NodeConfig::default(),
    };
    if let Some(listen) = flag(args, "--listen") {
        cfg.listen = listen;
    }
    if let Some(profile) = flag(args, "--profile") {
        cfg.profile = ResourceProfile::from_id(&profile).unwrap_or_else(|| {
            eprintln!("unknown profile: {profile} (use light | balanced | max)");
            std::process::exit(2);
        });
    }
    cfg
}

async fn attach(url: &str) -> Result<(), Box<dyn std::error::Error>> {
    let (ws, _) = tokio_tungstenite::connect_async(url).await?;
    let (mut write, mut read) = ws.split();
    println!("attached to {url} — type `help` for commands");

    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    loop {
        tokio::select! {
            incoming = read.next() => match incoming {
                Some(Ok(Message::Text(text))) => {
                    if let Ok(message) = rusm_bench::ServerMessage::from_json(text.as_str()) {
                        println!("{}", render_message(&message));
                    }
                }
                Some(Ok(Message::Close(_))) | None => {
                    println!("node disconnected");
                    break;
                }
                _ => {}
            },
            line = lines.next_line() => match line {
                Ok(Some(line)) => match parse(&line) {
                    ReplInput::Command(cmd) => send(&mut write, &cmd).await?,
                    ReplInput::Help => println!("{HELP}"),
                    ReplInput::Quit => break,
                    ReplInput::Empty => {}
                    ReplInput::Unknown(msg) => println!("{msg}"),
                },
                _ => break,
            },
        }
    }
    Ok(())
}

async fn send<S>(write: &mut S, command: &ClientCommand) -> Result<(), Box<dyn std::error::Error>>
where
    S: SinkExt<Message> + Unpin,
    S::Error: std::error::Error + 'static,
{
    write.send(Message::Text(command.to_json().into())).await?;
    Ok(())
}
