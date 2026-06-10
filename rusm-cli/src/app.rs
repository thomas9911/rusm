//! The **app model**: build a project's components and run them from `./wasm/`.
//!
//! A RUSM app is a directory with `rusm.toml` (`[[components]]`), a `components/`
//! tree of source crates, and a `wasm/` dir of built artifacts. `rusm build`
//! compiles each `components/<name>/` to either `wasm/<name>.wasm` (a Rust
//! component) or `wasm/<name>.js` (a TypeScript bundle, Bun-built); the loader
//! resolves whichever exists and spawns each declared component as a supervised
//! process under its capability profile. A `.js` artifact runs on the shared
//! rquickjs js-runner via [`WasmRuntime::spawn_js`]; a `.wasm` artifact is a
//! component instance. Env vars are the Rust way: process env first, then `.env`.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::Path;

use anyhow::{Context, Result};
use rusm_bench::{CapabilitySpec, ComponentSpec, ServeMode, ServeProtocol, ServeSpec};
use rusm_otp::ProcessHandle;
use rusm_wasm::{
    Capabilities, CapabilityProfile, HttpServer, ResidentHttpServer, ResidentWsServer, WasmRuntime,
    WsServer,
};
use tokio::net::TcpListener;
use tokio::task::JoinHandle;

/// Resolves a [`CapabilitySpec`] to concrete [`Capabilities`]: start from the
/// inherited built-in base (default `sandboxed`), then apply each set override.
/// Env keys are resolved from the process environment (process env, then `.env`).
///
/// This is the `rusm-wasm` conversion of the manifest's `[capabilities.<name>]`
/// table; it lives here, the only place that links the Wasm backend, so the
/// `rusm-node` manifest crate stays Wasm-free.
fn to_capabilities(spec: &CapabilitySpec) -> Capabilities {
    let mut caps = spec
        .inherits
        .as_deref()
        .and_then(CapabilityProfile::from_id)
        .unwrap_or(CapabilityProfile::Sandboxed)
        .capabilities();
    if let Some(v) = spec.network {
        caps = caps.allow_network(v);
    }
    if let Some(v) = spec.spawn {
        caps = caps.allow_spawn(v);
    }
    if let Some(v) = spec.process_control {
        caps = caps.allow_process_control(v);
    }
    if let Some(v) = spec.stdio {
        caps = caps.inherit_stdio(v);
    }
    if let Some(mb) = spec.max_memory_mb {
        caps = caps.max_memory(mb << 20);
    }
    for key in &spec.env {
        if let Ok(value) = std::env::var(key) {
            caps = caps.env(key, value);
        }
    }
    for p in &spec.preopen {
        caps = caps.preopen(&p.host, &p.guest, p.read_only);
    }
    caps
}

/// Resolves a capability id to its [`Capabilities`]: a custom `[capabilities.<id>]`
/// profile first, then a built-in (`sandboxed` / `network-client` / `trusted`),
/// falling back to the secure `Sandboxed` default (default-deny) for an unknown id.
pub fn capabilities_for(id: &str, profiles: &HashMap<String, CapabilitySpec>) -> Capabilities {
    if let Some(spec) = profiles.get(id) {
        return to_capabilities(spec);
    }
    CapabilityProfile::from_id(id)
        .unwrap_or(CapabilityProfile::Sandboxed)
        .capabilities()
}

/// Loads each manifest component from `<dir>/wasm/` and spawns it as a process
/// under its capability profile. A `<name>.js` artifact (a TypeScript bundle)
/// takes precedence and runs on the shared js-runner; otherwise `<name>.wasm` is
/// loaded as a component instance. Returns the live `(name, handle)` pairs (hold
/// them to keep the processes alive). Errors if no artifact exists or it won't
/// compile — a clear signal to run `rusm build` first.
pub fn spawn_components(
    dir: &Path,
    wasm: &WasmRuntime,
    specs: &[ComponentSpec],
    profiles: &HashMap<String, CapabilitySpec>,
) -> Result<Vec<(String, ProcessHandle)>> {
    let wasm_dir = dir.join("wasm");
    let mut handles = Vec::with_capacity(specs.len());
    for spec in specs {
        let caps = capabilities_for(&spec.capability, profiles);
        // TypeScript component: prefer the precompiled QuickJS bytecode
        // (`<name>.qjsbc`, no runtime parse) and fall back to the `.js` source. Both
        // run on the shared js-runner, which detects the form by its magic prefix.
        let bc_path = wasm_dir.join(format!("{}.qjsbc", spec.name));
        let js_path = wasm_dir.join(format!("{}.js", spec.name));
        let bundle_path = [bc_path, js_path].into_iter().find(|p| p.is_file());
        let handle = if let Some(path) = bundle_path {
            let bundle =
                std::fs::read(&path).with_context(|| format!("reading {}", path.display()))?;
            // Register by name so a running sibling may `spawn` it as a TS service.
            wasm.register_js_component(spec.name.clone(), bundle.clone());
            wasm.spawn_js_with(bundle, caps)
        } else {
            let path = wasm_dir.join(format!("{}.wasm", spec.name));
            let bytes = std::fs::read(&path)
                .with_context(|| format!("reading {} (run `rusm build`?)", path.display()))?;
            let component = wasm
                .compile_component(&bytes)
                .with_context(|| format!("compiling component `{}`", spec.name))?;
            // An actor component exports `run` (rusm:runtime); a stock component
            // exports `wasi:cli/run`. Prefer the actor path (registrable + spawnable
            // by siblings); otherwise run it unchanged as a standard command component.
            match wasm.prepare_component(&component, "run") {
                Ok(prepared) => {
                    wasm.register_component(spec.name.clone(), prepared.clone());
                    wasm.spawn_component_with(&prepared, caps)
                }
                Err(_) => wasm.spawn_command_with(&component, caps).with_context(|| {
                    format!(
                        "`{}` is neither a rusm actor component nor a wasi:cli command",
                        spec.name
                    )
                })?,
            }
        };
        handles.push((spec.name.clone(), handle));
    }
    Ok(handles)
}

/// A `[[serve]]` server now running: its name, protocol, the address it actually
/// bound (resolves `:0` to the real port), and the task driving it. Hold the task
/// (and the `WasmRuntime`) to keep the server up.
pub struct ServedEndpoint {
    pub name: String,
    pub protocol: ServeProtocol,
    pub addr: SocketAddr,
    pub task: JoinHandle<()>,
}

/// Hosts each `[[serve]]` entry on its own TCP port: load the component from
/// `<dir>/wasm/<name>.{wasm,js}`, build the matching server (HTTP/SSE via
/// `http_server`, WebSocket via `ws_server`), bind its `listen` address, and spawn
/// the accept loop. Returns the live endpoints (the bound address is captured
/// before serving, so a caller can log or connect to it immediately). Errors with a
/// clear message if an artifact is missing or an address won't bind.
pub async fn serve_apps(
    dir: &Path,
    wasm: &WasmRuntime,
    specs: &[ServeSpec],
    profiles: &HashMap<String, CapabilitySpec>,
) -> Result<Vec<ServedEndpoint>> {
    let mut endpoints = Vec::with_capacity(specs.len());
    for spec in specs {
        let caps = capabilities_for(&spec.capability, profiles);
        let listener = TcpListener::bind(&spec.listen)
            .await
            .with_context(|| format!("binding {} for `{}`", spec.listen, spec.name))?;
        let addr = listener
            .local_addr()
            .with_context(|| format!("local address of `{}`", spec.name))?;
        // Build the server up front so a load/compile error surfaces here (before we
        // claim the endpoint is up), then drive the accept loop on its own task. The
        // mode picks per-request (a fresh instance per unit) vs resident (a supervised
        // pool of stateful instances).
        let task = match (spec.protocol.is_http(), spec.mode) {
            (true, ServeMode::PerRequest) => {
                tokio::spawn(build_http_server(dir, wasm, &spec.name, caps)?.serve(listener))
            }
            (true, ServeMode::Resident) => {
                tokio::spawn(build_resident_http_server(dir, wasm, spec, caps)?.serve(listener))
            }
            (false, ServeMode::PerRequest) => {
                tokio::spawn(build_ws_server(dir, wasm, &spec.name, caps)?.serve(listener))
            }
            (false, ServeMode::Resident) => {
                tokio::spawn(build_resident_ws_server(dir, wasm, spec, caps)?.serve(listener))
            }
        };
        endpoints.push(ServedEndpoint {
            name: spec.name.clone(),
            protocol: spec.protocol,
            addr,
            task,
        });
    }
    Ok(endpoints)
}

/// Builds an HTTP/SSE server for `name`, resolving a `.js` bundle (on the
/// js-http-runner) before a `.wasm` component (instance-per-request `wasi:http`).
fn build_http_server(
    dir: &Path,
    wasm: &WasmRuntime,
    name: &str,
    caps: Capabilities,
) -> Result<HttpServer> {
    let wasm_dir = dir.join("wasm");
    let js_path = wasm_dir.join(format!("{name}.js"));
    if js_path.is_file() {
        let bundle = std::fs::read_to_string(&js_path)
            .with_context(|| format!("reading {}", js_path.display()))?;
        return Ok(wasm.http_server_js(bundle, caps));
    }
    let path = wasm_dir.join(format!("{name}.wasm"));
    let bytes = std::fs::read(&path)
        .with_context(|| format!("reading {} (run `rusm build`?)", path.display()))?;
    let component = wasm
        .compile_component(&bytes)
        .with_context(|| format!("compiling component `{name}`"))?;
    let prepared = wasm
        .prepare_http(&component)
        .with_context(|| format!("`{name}` is not a `wasi:http` component"))?;
    Ok(wasm.http_server(&prepared, caps))
}

/// Builds a WebSocket server for `name`: a `.js` worker bundle (on the js-runner)
/// before a `.wasm` actor component (one process per connection).
fn build_ws_server(
    dir: &Path,
    wasm: &WasmRuntime,
    name: &str,
    caps: Capabilities,
) -> Result<WsServer> {
    let wasm_dir = dir.join("wasm");
    let js_path = wasm_dir.join(format!("{name}.js"));
    if js_path.is_file() {
        let bundle =
            std::fs::read(&js_path).with_context(|| format!("reading {}", js_path.display()))?;
        return Ok(wasm.ws_server_js(bundle, caps));
    }
    let path = wasm_dir.join(format!("{name}.wasm"));
    let bytes = std::fs::read(&path)
        .with_context(|| format!("reading {} (run `rusm build`?)", path.display()))?;
    let component = wasm
        .compile_component(&bytes)
        .with_context(|| format!("compiling component `{name}`"))?;
    let prepared = wasm.prepare_component(&component, "run")?;
    Ok(wasm.ws_server(&prepared, caps))
}

/// Builds a **resident** HTTP/SSE server for `spec`: a supervised pool of
/// `spec.instances` long-lived actor handlers (`.js` on the js-runner, else a
/// `.wasm` actor component driving `rusm_rs::http::serve`), with optional shard
/// affinity. Unlike the per-request path this is an actor component, not `wasi:http`.
fn build_resident_http_server(
    dir: &Path,
    wasm: &WasmRuntime,
    spec: &ServeSpec,
    caps: Capabilities,
) -> Result<ResidentHttpServer> {
    let wasm_dir = dir.join("wasm");
    let name = &spec.name;
    let js_path = wasm_dir.join(format!("{name}.js"));
    let server = if js_path.is_file() {
        let bundle =
            std::fs::read(&js_path).with_context(|| format!("reading {}", js_path.display()))?;
        wasm.resident_http_server_js(bundle, caps, spec.instances)
    } else {
        let prepared = prepare_resident_component(wasm, &wasm_dir, name)?;
        wasm.resident_http_server(&prepared, caps, spec.instances)
    };
    let server = server.shard_by(spec.shard_by.as_deref());
    Ok(match spec.max_inflight {
        Some(limit) => server.max_inflight(limit),
        None => server,
    })
}

/// Builds a **resident** WebSocket server for `spec`: a supervised pool serving all
/// connections from shared state (`.js` worker / `.wasm` actor driving
/// `rusm_rs::ws::serve`), with optional shard affinity.
fn build_resident_ws_server(
    dir: &Path,
    wasm: &WasmRuntime,
    spec: &ServeSpec,
    caps: Capabilities,
) -> Result<ResidentWsServer> {
    let wasm_dir = dir.join("wasm");
    let name = &spec.name;
    let js_path = wasm_dir.join(format!("{name}.js"));
    let server = if js_path.is_file() {
        let bundle =
            std::fs::read(&js_path).with_context(|| format!("reading {}", js_path.display()))?;
        wasm.resident_ws_server_js(bundle, caps, spec.instances)
    } else {
        let prepared = prepare_resident_component(wasm, &wasm_dir, name)?;
        wasm.resident_ws_server(&prepared, caps, spec.instances)
    };
    let server = server.shard_by(spec.shard_by.as_deref());
    Ok(match spec.max_inflight {
        Some(limit) => server.max_inflight(limit),
        None => server,
    })
}

/// Compile + prepare a `.wasm` actor component (the `run` export) for resident
/// serving — shared by the resident HTTP and WS builders.
fn prepare_resident_component(
    wasm: &WasmRuntime,
    wasm_dir: &Path,
    name: &str,
) -> Result<rusm_wasm::PreparedComponent> {
    let path = wasm_dir.join(format!("{name}.wasm"));
    let bytes = std::fs::read(&path)
        .with_context(|| format!("reading {} (run `rusm build`?)", path.display()))?;
    let component = wasm
        .compile_component(&bytes)
        .with_context(|| format!("compiling component `{name}`"))?;
    wasm.prepare_component(&component, "run")
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusm_otp::Runtime;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpStream;

    #[test]
    fn a_custom_profile_inherits_then_overrides() {
        // Starts from network-client (network on, spawn off), then turns spawn on
        // and tightens memory — only the set fields override the inherited base.
        let cfg = rusm_bench::NodeConfig::from_toml(
            "[capabilities.worker]\ninherits = \"network-client\"\nspawn = true\nmax-memory-mb = 32\n",
        )
        .unwrap();
        let caps = to_capabilities(&cfg.capabilities["worker"]);
        assert!(caps.can_spawn(), "override turned spawn on");
        assert_eq!(caps.memory_limit(), 32 << 20, "override tightened memory");
        // An omitted base → the most restrictive default (sandboxed): no spawn.
        let bare = CapabilitySpec {
            inherits: None,
            network: None,
            spawn: None,
            process_control: None,
            stdio: None,
            max_memory_mb: None,
            env: Vec::new(),
            preopen: Vec::new(),
        };
        assert!(
            !to_capabilities(&bare).can_spawn(),
            "default base is sandboxed"
        );
    }

    // A minimal component (WAT text — accepted by compile_component) standing in
    // for a built `wasm/<name>.wasm`; it just runs and returns.
    const COMPONENT: &str = r#"(component
        (core module $m (memory (export "mem") 1) (func (export "run")))
        (core instance $i (instantiate $m))
        (func (export "run") (canon lift (core func $i "run"))))"#;

    #[test]
    fn capability_resolution_prefers_custom_then_builtin_then_sandboxed() {
        let mut profiles = HashMap::new();
        profiles.insert(
            "agent".to_string(),
            CapabilitySpec {
                inherits: Some("network-client".to_string()),
                network: None,
                spawn: Some(true),
                process_control: None,
                stdio: None,
                max_memory_mb: Some(16),
                env: Vec::new(),
                preopen: Vec::new(),
            },
        );
        // A custom profile resolves to its grants.
        let agent = capabilities_for("agent", &profiles);
        assert!(agent.can_spawn(), "custom profile grants spawn");
        assert_eq!(agent.memory_limit(), 16 << 20);
        // A built-in still resolves; an unknown id falls back to sandboxed.
        assert!(
            capabilities_for("trusted", &profiles).can_spawn(),
            "built-in trusted resolves and grants spawn"
        );
        assert!(
            !capabilities_for("does-not-exist", &profiles).can_spawn(),
            "unknown id falls back to default-deny sandboxed"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn loads_and_spawns_manifest_components_from_wasm_dir() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("wasm")).unwrap();
        std::fs::write(dir.path().join("wasm/echo.wasm"), COMPONENT).unwrap();

        let rt = Runtime::new();
        let wasm = WasmRuntime::new(rt.clone()).unwrap();
        let specs = vec![ComponentSpec {
            name: "echo".to_string(),
            capability: "sandboxed".to_string(),
            restart: false,
        }];
        let handles = spawn_components(dir.path(), &wasm, &specs, &HashMap::new()).unwrap();
        assert_eq!(handles.len(), 1);
        assert_eq!(handles[0].0, "echo");
        // The component runs to completion as a real process.
        let (_name, handle) = handles.into_iter().next().unwrap();
        handle.join().await;
        assert_eq!(rt.finished(), 1);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_typescript_js_bundle_runs_on_the_js_runner() {
        // A `wasm/<name>.js` artifact is a TS component: it runs on the shared
        // js-runner via spawn_js, not the component path. The bundle drives the
        // Process API and exits, finishing as a real process.
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("wasm")).unwrap();
        std::fs::write(
            dir.path().join("wasm/greeter.js"),
            "Process.setLabel('ts-greeter');",
        )
        .unwrap();

        let rt = Runtime::new();
        let wasm = WasmRuntime::new(rt.clone()).unwrap();
        let specs = vec![ComponentSpec {
            name: "greeter".to_string(),
            capability: "sandboxed".to_string(),
            restart: false,
        }];
        let handles = spawn_components(dir.path(), &wasm, &specs, &HashMap::new()).unwrap();
        assert_eq!(handles.len(), 1);
        let (_name, handle) = handles.into_iter().next().unwrap();
        handle.join().await;
        assert_eq!(rt.finished(), 1, "the TS bundle ran to completion");
    }

    // A lean `wasi:http` component fixture (the same one rusm-wasm/http_bench uses),
    // standing in for a built `wasm/<name>.wasm` HTTP server.
    const HTTP_LEAN: &[u8] = include_bytes!("../../crates/rusm-wasm/tests/fixtures/http_lean.wasm");

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn serves_an_http_component_on_a_real_port() {
        // The exact `rusm serve` path: drop a built `.wasm` into ./wasm, host it on a
        // real TCP port via `[[serve]]`, then hit it with a genuine HTTP GET. This is
        // the fair, out-of-process shape a load driver uses — no in-process generator.
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("wasm")).unwrap();
        std::fs::write(dir.path().join("wasm/api.wasm"), HTTP_LEAN).unwrap();

        let rt = Runtime::new();
        let wasm = WasmRuntime::new(rt).unwrap();
        let specs = vec![ServeSpec {
            name: "api".to_string(),
            protocol: ServeProtocol::Http,
            listen: "127.0.0.1:0".to_string(), // ephemeral; we read back the real port
            capability: "trusted".to_string(),
            mode: ServeMode::PerRequest,
            instances: 1,
            shard_by: None,
            max_inflight: None,
        }];
        let endpoints = serve_apps(dir.path(), &wasm, &specs, &HashMap::new())
            .await
            .unwrap();
        assert_eq!(endpoints.len(), 1);
        let addr = endpoints[0].addr;
        assert_ne!(addr.port(), 0, "an ephemeral port was bound and reported");

        // A real client connection — exactly what an external load tool does.
        let mut conn = TcpStream::connect(addr).await.unwrap();
        conn.write_all(b"GET / HTTP/1.1\r\nHost: rusm\r\nConnection: close\r\n\r\n")
            .await
            .unwrap();
        let mut response = Vec::new();
        conn.read_to_end(&mut response).await.unwrap();
        let text = String::from_utf8_lossy(&response);
        assert!(
            text.starts_with("HTTP/1.1 200"),
            "the hosted component answered 200 (got: {:?})",
            text.lines().next()
        );
    }

    // A Rust WS-handler component fixture (echoes each frame from the sandbox).
    const RS_WS_ECHO: &[u8] =
        include_bytes!("../../crates/rusm-wasm/tests/fixtures/rs_ws_echo.wasm");

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn serves_a_websocket_component_on_a_real_port() {
        use futures_util::{SinkExt, StreamExt};
        use tokio_tungstenite::tungstenite::Message;

        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("wasm")).unwrap();
        std::fs::write(dir.path().join("wasm/echo.wasm"), RS_WS_ECHO).unwrap();

        let rt = Runtime::new();
        let wasm = WasmRuntime::new(rt).unwrap();
        let specs = vec![ServeSpec {
            name: "echo".to_string(),
            protocol: ServeProtocol::Ws,
            listen: "127.0.0.1:0".to_string(),
            capability: "trusted".to_string(),
            mode: ServeMode::PerRequest,
            instances: 1,
            shard_by: None,
            max_inflight: None,
        }];
        let endpoints = serve_apps(dir.path(), &wasm, &specs, &HashMap::new())
            .await
            .unwrap();
        let addr = endpoints[0].addr;

        let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://{addr}/"))
            .await
            .unwrap();
        ws.send(Message::binary(b"ping".as_slice())).await.unwrap();
        let reply = ws.next().await.unwrap().unwrap();
        assert_eq!(
            reply.into_data().as_ref(),
            b"ping",
            "the WS component echoed"
        );
    }

    // A resident (stateful) actor HTTP handler fixture — counts requests.
    const RS_RESIDENT_COUNT: &[u8] =
        include_bytes!("../../crates/rusm-wasm/tests/fixtures/rs_resident_count.wasm");

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn serves_a_resident_stateful_component_on_a_real_port() {
        use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

        // `[[serve]] mode = "resident"` end-to-end: one long-lived instance holds
        // state, so two GETs over real sockets see the counter advance (hit #1, #2).
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("wasm")).unwrap();
        std::fs::write(dir.path().join("wasm/counter.wasm"), RS_RESIDENT_COUNT).unwrap();

        let rt = Runtime::new();
        let wasm = WasmRuntime::new(rt).unwrap();
        let specs = vec![ServeSpec {
            name: "counter".to_string(),
            protocol: ServeProtocol::Http,
            listen: "127.0.0.1:0".to_string(),
            capability: "sandboxed".to_string(),
            mode: ServeMode::Resident,
            instances: 1,
            shard_by: None,
            max_inflight: None,
        }];
        let endpoints = serve_apps(dir.path(), &wasm, &specs, &HashMap::new())
            .await
            .unwrap();
        let addr = endpoints[0].addr;

        let get = || async move {
            let mut conn = TcpStream::connect(addr).await.unwrap();
            conn.write_all(b"GET / HTTP/1.1\r\nHost: rusm\r\nConnection: close\r\n\r\n")
                .await
                .unwrap();
            let mut buf = Vec::new();
            conn.read_to_end(&mut buf).await.unwrap();
            String::from_utf8_lossy(&buf).into_owned()
        };
        assert!(get().await.contains("hit #1"), "first request");
        assert!(get().await.contains("hit #2"), "state persisted (resident)");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn serving_a_missing_artifact_is_a_clear_error() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("wasm")).unwrap();
        let rt = Runtime::new();
        let wasm = WasmRuntime::new(rt).unwrap();
        let specs = vec![ServeSpec {
            name: "ghost".to_string(),
            protocol: ServeProtocol::Http,
            listen: "127.0.0.1:0".to_string(),
            capability: "trusted".to_string(),
            mode: ServeMode::PerRequest,
            instances: 1,
            shard_by: None,
            max_inflight: None,
        }];
        let err = serve_apps(dir.path(), &wasm, &specs, &HashMap::new())
            .await
            .err()
            .expect("missing artifact must error");
        assert!(err.to_string().contains("ghost.wasm"));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_missing_artifact_is_a_clear_error() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("wasm")).unwrap();
        let rt = Runtime::new();
        let wasm = WasmRuntime::new(rt).unwrap();
        let specs = vec![ComponentSpec {
            name: "absent".to_string(),
            capability: "sandboxed".to_string(),
            restart: false,
        }];
        let err = spawn_components(dir.path(), &wasm, &specs, &HashMap::new())
            .err()
            .expect("missing artifact must error");
        assert!(err.to_string().contains("absent.wasm"));
    }
}
