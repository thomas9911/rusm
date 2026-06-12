use std::path::Path;
use std::process::Command;

use anyhow::{anyhow, Context};
use futures_util::{SinkExt, StreamExt};
use rusm_cli::{
    normalize_target, parse, parse_new_args, render_message, scaffold, serve_apps,
    spawn_components, Protocol, ReplInput, DEFAULT_HOST, HELP,
};
use rusm_node::{serve, ClientCommand, Node, NodeConfig, ServerMessage};
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
        // Host the app's components and expose a live attach/observe endpoint.
        if let Err(error) = start_node(&args).await {
            eprintln!("node start failed: {error}");
            std::process::exit(1);
        }
    } else if command == Some("attach") {
        // Target defaults to the local node and accepts host / host:port / ws-url.
        let target = normalize_target(args.get(2).map(String::as_str).unwrap_or(DEFAULT_HOST));
        if let Err(error) = attach(&target).await {
            eprintln!("attach failed: {error}");
            std::process::exit(1);
        }
    } else if command == Some("new") {
        // Scaffold a new RUSM app in ./<name> (language/protocol via flags).
        match parse_new_args(&args[2..]) {
            Ok(app) => match scaffold(Path::new("."), &app) {
                Ok(_) => {
                    let probe = match app.protocol {
                        Protocol::Http => "curl http://127.0.0.1:8080/",
                        Protocol::Sse => "curl -N http://127.0.0.1:8080/",
                        Protocol::Ws => "websocat ws://127.0.0.1:8080/",
                    };
                    println!("created {}/", app.name);
                    println!("\nnext:");
                    println!("  cd {}", app.name);
                    println!("  rusm build      # compile components/ -> wasm/");
                    println!("  rusm serve      # http://127.0.0.1:8080");
                    println!("  {probe}");
                }
                Err(error) => {
                    eprintln!("new failed: {error}");
                    std::process::exit(1);
                }
            },
            Err(error) => {
                eprintln!("{error}");
                std::process::exit(2);
            }
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
    } else if command == Some("run") {
        // Run the app's components from ./wasm per the rusm.toml manifest.
        if let Err(error) = run_app(&args).await {
            eprintln!("run failed: {error}");
            std::process::exit(1);
        }
    } else if command == Some("serve") {
        // Host the app's [[serve]] components as real HTTP/WS/SSE servers on their
        // own ports — what an out-of-process load driver (rusm-loadtest) hits.
        if let Err(error) = serve_app(&args).await {
            eprintln!("serve failed: {error}");
            std::process::exit(1);
        }
    } else if command == Some("dev") {
        // Build, run, and watch: edit a component and RUSM rebuilds + reloads it.
        if let Err(error) = dev(&args).await {
            eprintln!("dev failed: {error}");
            std::process::exit(1);
        }
    } else {
        eprintln!("usage:");
        eprintln!("  rusm new <name>            scaffold a new RUSM app in ./<name>");
        eprintln!(
            "  rusm node start [--config <file>] [--listen <addr>]   host the app + a live attach endpoint"
        );
        eprintln!("  rusm build                 compile ./components/* -> ./wasm/*.wasm");
        eprintln!(
            "  rusm run                   run ./wasm components per rusm.toml [[components]]"
        );
        eprintln!("  rusm dev                   build + run, then watch & reload on edits");
        eprintln!(
            "  rusm serve                 host ./wasm components as HTTP/WS/SSE servers per rusm.toml [[serve]]"
        );
        eprintln!("  rusm attach [<host | host:port | ws-url>]   (defaults to 127.0.0.1:4000)");
        std::process::exit(2);
    }
}

/// `rusm node start`: host the app's `[[components]]` (like `rusm run`) and expose
/// a live **attach** endpoint on `cfg.listen`, so `rusm attach` can observe the
/// node's processes. The served runtime + held handles keep everything alive for
/// the lifetime of the server (which runs until Ctrl-C or a bind error).
async fn start_node(args: &[String]) -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    let cfg = load_node_config(args);
    let rt = Runtime::new();
    // `wasm` + `handles` stay bound for the whole function: they own the hosted
    // components' runtime, so they must outlive the server below.
    let wasm = wasm_runtime(rt.clone(), &cfg)?;
    let handles =
        spawn_components(Path::new("."), &wasm, &cfg.components, &cfg.capabilities).await?;
    let node = Node::new(rt.clone(), node_name(), cfg.ticks_per_second);
    println!(
        "rusm node listening on ws://{} ({} component(s), {} Hz)",
        cfg.listen,
        handles.len(),
        cfg.ticks_per_second
    );
    println!("attach with:  rusm attach {}", cfg.listen);
    serve(&cfg.listen, node).await?;
    Ok(())
}

/// The node's display name for `attach`: the app directory's name (e.g. `hello`),
/// falling back to `rusm`.
fn node_name() -> String {
    std::env::current_dir()
        .ok()
        .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
        .unwrap_or_else(|| "rusm".to_string())
}

/// Runs the app's components: load `.env` (process env wins), then spawn each
/// `[[components]]` entry from `./wasm` under its capability profile, and wait
/// for Ctrl-C. Held handles + runtime keep the processes alive.
async fn run_app(args: &[String]) -> anyhow::Result<()> {
    // Environment variables the Rust way: process env first, then ./.env.
    dotenvy::dotenv().ok();

    let cfg = load_node_config(args);
    let rt = Runtime::new();
    let wasm = wasm_runtime(rt.clone(), &cfg)?;
    let handles =
        spawn_components(Path::new("."), &wasm, &cfg.components, &cfg.capabilities).await?;
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

/// `rusm serve`: host each `[[serve]]` component as a real network server on its
/// own port (HTTP/SSE or WebSocket), then wait for Ctrl-C. The bound runtime + the
/// accept-loop tasks keep the servers up. This is the *server* side of a fair
/// benchmark: the node only serves; load is driven out-of-process (`rusm-loadtest`).
async fn serve_app(args: &[String]) -> anyhow::Result<()> {
    // Env the Rust way: process env first, then ./.env.
    dotenvy::dotenv().ok();

    let cfg = load_node_config(args);
    let rt = Runtime::new();
    let wasm = wasm_runtime(rt.clone(), &cfg)?;
    // Bring up the supporting `[[components]]` (e.g. a `meta-json` sink) on the **same**
    // node first, so a `[[serve]]` entry can reach them via `whereis` / `spawn` — an app
    // that serves HTTP *and* runs sibling services comes up with one `rusm serve`. Held
    // to keep them alive.
    let components =
        spawn_components(Path::new("."), &wasm, &cfg.components, &cfg.capabilities).await?;
    let routes = cfg
        .route_table()
        .map_err(|e| anyhow::anyhow!("invalid [routes] in rusm.toml: {e}"))?;
    let endpoints = serve_apps(
        Path::new("."),
        &wasm,
        &cfg.serve,
        &routes,
        &cfg.components,
        &cfg.capabilities,
    )
    .await?;
    if endpoints.is_empty() && components.is_empty() {
        println!("no [[serve]] entries or [[components]] in rusm.toml — nothing to do");
        return Ok(());
    }
    if !components.is_empty() {
        let names: Vec<&str> = components.iter().map(|(n, _)| n.as_str()).collect();
        println!(
            "running {} component(s): {}",
            components.len(),
            names.join(", ")
        );
    }
    println!("serving {} endpoint(s):", endpoints.len());
    for ep in &endpoints {
        let scheme = if ep.protocol.is_http() { "http" } else { "ws" };
        println!("  {:<16} {scheme}://{}", ep.name, ep.addr);
    }
    println!("press Ctrl-C to stop");
    tokio::signal::ctrl_c().await?;
    println!("\nstopping {} process(es)…", rt.shutdown());
    Ok(())
}

/// `rusm dev`: build, spawn, and **watch** `./components` — on any source change,
/// rebuild and reload the components (kill + respawn). Ctrl-C stops. Watching is a
/// dependency-free mtime poll (a ~400 ms scan, skipping build output).
async fn dev(args: &[String]) -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    let cfg = load_node_config(args);
    let rt = Runtime::new();
    let wasm = wasm_runtime(rt.clone(), &cfg)?;
    let root = Path::new(".");

    build_components(root)?;
    let mut handles = spawn_components(root, &wasm, &cfg.components, &cfg.capabilities).await?;
    if handles.is_empty() {
        println!("no [[components]] in rusm.toml — nothing to run");
        return Ok(());
    }
    println!(
        "running {} component(s); watching ./components — edit to reload, Ctrl-C to stop",
        handles.len()
    );

    let components = root.join("components");
    let mut fingerprint = source_fingerprint(&components);
    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => break,
            _ = tokio::time::sleep(std::time::Duration::from_millis(400)) => {
                let next = source_fingerprint(&components);
                if next == fingerprint {
                    continue;
                }
                fingerprint = next;
                println!("change detected — rebuilding…");
                for (_, handle) in &handles {
                    handle.kill();
                }
                if let Err(error) = build_components(root) {
                    eprintln!("build failed: {error}");
                    continue;
                }
                match spawn_components(root, &wasm, &cfg.components, &cfg.capabilities).await {
                    Ok(reloaded) => {
                        handles = reloaded;
                        println!("reloaded {} component(s)", handles.len());
                    }
                    Err(error) => eprintln!("reload failed: {error}"),
                }
            }
        }
    }
    println!("\nstopping {} component(s)…", rt.shutdown());
    Ok(())
}

/// A fingerprint of the source files under `dir` (sorted path + mtime pairs),
/// skipping build output (`target/`, `node_modules/`). Any source edit changes it.
fn source_fingerprint(dir: &Path) -> Vec<(std::path::PathBuf, std::time::SystemTime)> {
    fn walk(dir: &Path, out: &mut Vec<(std::path::PathBuf, std::time::SystemTime)>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                let name = entry.file_name();
                if name != "target" && name != "node_modules" {
                    walk(&path, out);
                }
            } else if matches!(
                path.extension().and_then(|e| e.to_str()),
                Some("ts" | "rs" | "toml" | "js" | "json" | "wit")
            ) {
                if let Ok(modified) = entry.metadata().and_then(|m| m.modified()) {
                    out.push((path, modified));
                }
            }
        }
    }
    let mut out = Vec::new();
    walk(dir, &mut out);
    out.sort();
    out
}

/// Builds every component under `<dir>/components/<name>/` into `<dir>/wasm/`.
/// Two kinds, auto-detected, one toolchain each (no jco, no cargo-component):
/// a **Rust** component (has `Cargo.toml`) builds with `cargo build --target
/// wasm32-wasip2 --release` → `wasm/<name>.wasm`; a **TypeScript** component
/// (has `index.ts`/`src/index.ts`) bundles with `bun build` → `wasm/<name>.js`,
/// run on the shared rquickjs js-runner. Returns the built component names.
/// (Shell-orchestration glue, hence it lives in `main`.)
fn build_components(dir: &Path) -> anyhow::Result<Vec<String>> {
    let components_dir = dir.join("components");
    let wasm_dir = dir.join("wasm");
    std::fs::create_dir_all(&wasm_dir)?;

    // If the app declares JS dependencies (e.g. the `rusm-ts` package), make sure
    // they're installed so a TS component's `import` resolves during bundling.
    if dir.join("package.json").is_file() && !dir.join("node_modules").is_dir() {
        let status = Command::new("bun")
            .arg("install")
            .current_dir(dir)
            .status()
            .with_context(|| "running bun install (is Bun installed? https://bun.sh)")?;
        if !status.success() {
            return Err(anyhow!("`bun install` failed"));
        }
    }

    let mut entries: Vec<_> = std::fs::read_dir(&components_dir)
        .with_context(|| format!("reading {}", components_dir.display()))?
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_dir())
        .collect();
    entries.sort_by_key(|e| e.file_name());

    let mut built = Vec::new();
    for entry in entries {
        let crate_dir = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        if crate_dir.join("Cargo.toml").is_file() {
            build_rust_component(&crate_dir, &name, &wasm_dir)?;
            built.push(name);
        } else if let Some(ts_entry) = ts_entrypoint(&crate_dir) {
            build_ts_component(&ts_entry, &name, &wasm_dir)?;
            built.push(name);
        }
        // A dir that is neither a Rust crate nor a TS component is skipped.
    }
    Ok(built)
}

/// Builds one Rust component crate to `wasm/<name>.wasm` via `cargo build
/// --target wasm32-wasip2 --release` (which componentizes).
fn build_rust_component(crate_dir: &Path, name: &str, wasm_dir: &Path) -> anyhow::Result<()> {
    let status = Command::new("cargo")
        .args(["build", "--target", "wasm32-wasip2", "--release"])
        .current_dir(crate_dir)
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
    Ok(())
}

/// The TS entrypoint of a component dir, if any: `index.ts` or `src/index.ts`.
fn ts_entrypoint(crate_dir: &Path) -> Option<std::path::PathBuf> {
    [crate_dir.join("index.ts"), crate_dir.join("src/index.ts")]
        .into_iter()
        .find(|p| p.is_file())
}

/// Bundles one TS component to `wasm/<name>.js` with `bun build`, in **CommonJS**
/// form (`--format=cjs`) so the runner sees its `export`s on `module.exports` — a
/// service component's functions, or a worker's `export default`. Targets `browser`
/// (no node/bun globals assumed); a bare script with no exports just runs.
///
/// Then **precompiles** the bundle to QuickJS bytecode → `wasm/<name>.qjsbc`
/// (version-locked to the js-runner via `rusm-jsc`), so the runner skips parsing at
/// load. The loader prefers the `.qjsbc`; the `.js` stays for debugging.
fn build_ts_component(entry: &Path, name: &str, wasm_dir: &Path) -> anyhow::Result<()> {
    let dest = wasm_dir.join(format!("{name}.js"));
    let status = Command::new("bun")
        .args([
            "build",
            "--target=browser",
            "--format=cjs",
            "--minify",
            "--outfile",
        ])
        .arg(&dest)
        .arg(entry)
        .status()
        .with_context(|| "running bun (is Bun installed? https://bun.sh)")?;
    if !status.success() {
        return Err(anyhow!("`bun build` failed for component `{name}`"));
    }
    // Precompile to QuickJS bytecode (skip the parser at runtime). A compile error
    // here is non-fatal: drop the stale .qjsbc so the loader falls back to source.
    let source = std::fs::read_to_string(&dest)
        .with_context(|| format!("reading bundled {}", dest.display()))?;
    let bc_path = wasm_dir.join(format!("{name}.qjsbc"));
    match rusm_jsc::compile(&source) {
        Ok(bytecode) => std::fs::write(&bc_path, bytecode)
            .with_context(|| format!("writing {}", bc_path.display()))?,
        Err(error) => {
            eprintln!("warning: bytecode precompile failed for `{name}` ({error}); using source");
            let _ = std::fs::remove_file(&bc_path);
        }
    }
    Ok(())
}

fn flag(args: &[String], name: &str) -> Option<String> {
    let idx = args.iter().position(|a| a == name)?;
    args.get(idx + 1).cloned()
}

/// Loads node config: defaults → `rusm.toml` (or `--config <file>`) → CLI flags.
fn load_node_config(args: &[String]) -> NodeConfig {
    let explicit = flag(args, "--config");
    let path = explicit.clone().unwrap_or_else(|| "rusm.toml".to_string());
    let mut cfg = NodeConfig::load(Path::new(&path), explicit.is_some()).unwrap_or_else(|error| {
        eprintln!("{error}");
        std::process::exit(2);
    });
    if let Some(listen) = flag(args, "--listen") {
        cfg.listen = listen;
    }
    cfg
}

/// Build the Wasm runtime for an app, opening the configured durable key-value
/// store (`store = "..."` in rusm.toml, relative to the app dir) when set — so
/// components granted `storage` can persist; otherwise a store-less runtime. The
/// store's parent dir is created so a fresh app's first run doesn't trip on it.
fn wasm_runtime(rt: Runtime, cfg: &NodeConfig) -> anyhow::Result<WasmRuntime> {
    match &cfg.store {
        Some(rel) => {
            let path = Path::new(".").join(rel);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).ok();
            }
            Ok(WasmRuntime::with_store(rt, &path)?)
        }
        None => Ok(WasmRuntime::new(rt)?),
    }
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
                    if let Ok(message) = ServerMessage::from_json(text.as_str()) {
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
