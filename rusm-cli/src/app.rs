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
use rusm_bench::{CapabilitySpec, ComponentSpec, ServeProtocol, ServeSpec};
use rusm_otp::ProcessHandle;
use rusm_wasm::{Capabilities, CapabilityProfile, HttpServer, WasmRuntime, WsServer};
use tokio::net::TcpListener;
use tokio::task::JoinHandle;

/// Resolves a capability id to its [`Capabilities`]: a custom `[capabilities.<id>]`
/// profile first, then a built-in (`sandboxed` / `network-client` / `trusted`),
/// falling back to the secure `Sandboxed` default (default-deny) for an unknown id.
pub fn capabilities_for(id: &str, profiles: &HashMap<String, CapabilitySpec>) -> Capabilities {
    if let Some(spec) = profiles.get(id) {
        return spec.to_capabilities();
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
        let js_path = wasm_dir.join(format!("{}.js", spec.name));
        let handle = if js_path.is_file() {
            // TypeScript component: a Bun-built bundle run on the shared js-runner.
            let bundle = std::fs::read(&js_path)
                .with_context(|| format!("reading {}", js_path.display()))?;
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
            let prepared = wasm.prepare_component(&component, "run")?;
            // Register by name so a running sibling may `spawn` it (capability-gated).
            wasm.register_component(spec.name.clone(), prepared.clone());
            wasm.spawn_component_with(&prepared, caps)
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
        // claim the endpoint is up), then drive the accept loop on its own task.
        let task = if spec.protocol.is_http() {
            tokio::spawn(build_http_server(dir, wasm, &spec.name, caps)?.serve(listener))
        } else {
            tokio::spawn(build_ws_server(dir, wasm, &spec.name, caps)?.serve(listener))
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

#[cfg(test)]
mod tests {
    use super::*;
    use rusm_otp::Runtime;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpStream;

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
