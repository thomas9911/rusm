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
use std::sync::Arc;

use anyhow::{anyhow, Context, Result};
use rusm_node::{
    BundleSource, CapabilitySpec, ComponentSpec, Resolution, RouteTable, ServeMode, ServeProtocol,
    ServeSpec,
};
use rusm_otp::ProcessHandle;
use rusm_wasm::{
    Capabilities, CapabilityProfile, HttpServer, ResidentHttpServer, ResidentWsServer, Resolver,
    Routed, WasmRuntime, WsServer,
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
    if let Some(v) = spec.storage {
        caps = caps.allow_storage(v);
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

/// Resolve a component's optional `source` to JS bundle bytes — the dynamic-deploy
/// path: fetch from a URL (e.g. a presigned blob / artifact API) or read from the
/// node's durable `kv` store, instead of the local `./wasm/<name>` artifact. `None`
/// when no `source` is set (the caller falls back to the local file). A remote source
/// is always a **JS** bundle. Re-run on each `spawn`/reload, so updating the source
/// deploys new JS with no node rebuild.
async fn remote_bundle(source: Option<&str>, wasm: &WasmRuntime) -> Result<Option<Vec<u8>>> {
    let Some(spec) = source else {
        return Ok(None);
    };
    let bytes = match BundleSource::parse(spec).map_err(|e| anyhow!(e))? {
        BundleSource::Url(url) => fetch_url(&url).await?,
        BundleSource::Kv { bucket, key } => wasm
            .store()
            .ok_or_else(|| anyhow!("kv source `{spec}` needs a store (set `store` in rusm.toml)"))?
            .bucket(&bucket)
            .get(&key)
            .map_err(|e| anyhow!("reading kv {bucket}/{key}: {e}"))?
            .ok_or_else(|| anyhow!("kv source `{spec}` not found ({bucket}/{key})"))?,
    };
    Ok(Some(bytes))
}

/// GET `url` and return the body bytes (a one-shot bundle fetch). A non-2xx status
/// is an error, so a stale/forbidden link fails loudly rather than loading garbage.
async fn fetch_url(url: &str) -> Result<Vec<u8>> {
    let response = reqwest::get(url)
        .await
        .with_context(|| format!("fetching bundle from {url}"))?
        .error_for_status()
        .with_context(|| format!("fetching bundle from {url}"))?;
    Ok(response
        .bytes()
        .await
        .with_context(|| format!("reading bundle body from {url}"))?
        .to_vec())
}

/// Loads each manifest component from `<dir>/wasm/` and spawns it as a process
/// under its capability profile. A `<name>.js` artifact (a TypeScript bundle)
/// takes precedence and runs on the shared js-runner; otherwise `<name>.wasm` is
/// loaded as a component instance. Returns the live `(name, handle)` pairs (hold
/// them to keep the processes alive). Errors if no artifact exists or it won't
/// compile — a clear signal to run `rusm build` first.
pub async fn spawn_components(
    dir: &Path,
    wasm: &WasmRuntime,
    specs: &[ComponentSpec],
    profiles: &HashMap<String, CapabilitySpec>,
) -> Result<Vec<(String, ProcessHandle)>> {
    let wasm_dir = dir.join("wasm");
    let mut handles = Vec::with_capacity(specs.len());
    for spec in specs {
        let caps = capabilities_for(&spec.capability, profiles);
        // A configured `source` (url/kv) supplies a JS bundle directly — the
        // dynamic-deploy path, no local artifact needed.
        if let Some(bundle) = remote_bundle(spec.source.as_deref(), wasm).await? {
            wasm.register_js_component(spec.name.clone(), bundle.clone());
            handles.push((spec.name.clone(), wasm.spawn_js_with(bundle, caps)));
            continue;
        }
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
    routes: &RouteTable,
    components: &[ComponentSpec],
    profiles: &HashMap<String, CapabilitySpec>,
) -> Result<Vec<ServedEndpoint>> {
    // The unified serving model: a non-empty `[routes]` table makes every HTTP/SSE
    // listener route per request to `component#action` (process-per-request). Register
    // each HTTP serve's handler component for spawn-by-name and build the per-component
    // capability map the routed gateway spawns under (serve handlers + `[[components]]`).
    let routed = if routes.is_empty() {
        None
    } else {
        let mut caps_map: HashMap<String, Capabilities> = components
            .iter()
            .map(|c| (c.name.clone(), capabilities_for(&c.capability, profiles)))
            .collect();
        for spec in specs.iter().filter(|s| s.protocol.is_http()) {
            register_handler_component(dir, wasm, &spec.name)?;
            caps_map.insert(
                spec.name.clone(),
                capabilities_for(&spec.capability, profiles),
            );
        }
        Some((routed_resolver(routes.clone()), caps_map))
    };

    let mut endpoints = Vec::with_capacity(specs.len());
    for spec in specs {
        let caps = capabilities_for(&spec.capability, profiles);
        let listener = TcpListener::bind(&spec.listen)
            .await
            .with_context(|| format!("binding {} for `{}`", spec.listen, spec.name))?;
        let addr = listener
            .local_addr()
            .with_context(|| format!("local address of `{}`", spec.name))?;
        // Resolve a `source` (url/kv) bundle once, up front (a remote source is always
        // a JS handler); `None` → the builder falls back to the local artifact.
        let remote = remote_bundle(spec.source.as_deref(), wasm).await?;
        // Build the server up front so a load/compile error surfaces here (before we
        // claim the endpoint is up), then drive the accept loop on its own task.
        let task = match (&routed, spec.protocol.is_http(), spec.mode) {
            // Routed per-request HTTP/SSE: resolve the route, spawn the matched handler
            // fresh, dispatch the action. The shape RUSM standardizes serving on.
            (Some((resolve, caps_map)), true, _) => tokio::spawn(
                wasm.routed_http_server(resolve.clone(), caps_map.clone())
                    .serve(listener),
            ),
            (_, true, ServeMode::PerRequest) => tokio::spawn(
                build_http_server(dir, wasm, &spec.name, caps, remote)?.serve(listener),
            ),
            (_, true, ServeMode::Resident) => tokio::spawn(
                build_resident_http_server(dir, wasm, spec, caps, remote)?.serve(listener),
            ),
            (_, false, ServeMode::PerRequest) => {
                tokio::spawn(build_ws_server(dir, wasm, &spec.name, caps, remote)?.serve(listener))
            }
            (_, false, ServeMode::Resident) => tokio::spawn(
                build_resident_ws_server(dir, wasm, spec, caps, remote)?.serve(listener),
            ),
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

/// Bridge the manifest [`RouteTable`] into the engine's routing-agnostic [`Resolver`]
/// — the only place the config's `[routes]` shape meets the Wasm gateway.
fn routed_resolver(table: RouteTable) -> Resolver {
    Arc::new(
        move |method: &str, path: &str| match table.resolve(method, path) {
            Resolution::Found {
                component,
                action,
                params,
            } => Routed::Found {
                component,
                action,
                params,
            },
            Resolution::MethodNotAllowed => Routed::MethodNotAllowed,
            Resolution::NotFound => Routed::NotFound,
        },
    )
}

/// Register a routed HTTP handler component for spawn-by-name: a `.js` bundle on the
/// js-runner, else a `.wasm` actor component (the `run` export, a `#[rusm_rs::handlers]`
/// dispatcher). The routed gateway spawns it fresh per request.
fn register_handler_component(dir: &Path, wasm: &WasmRuntime, name: &str) -> Result<()> {
    let wasm_dir = dir.join("wasm");
    let js_path = wasm_dir.join(format!("{name}.js"));
    if js_path.is_file() {
        let bundle =
            std::fs::read(&js_path).with_context(|| format!("reading {}", js_path.display()))?;
        wasm.register_js_component(name.to_string(), bundle);
        return Ok(());
    }
    let prepared = prepare_resident_component(wasm, &wasm_dir, name)?;
    wasm.register_component(name.to_string(), prepared);
    Ok(())
}

/// Builds an HTTP/SSE server for `name`, resolving a `.js` bundle (on the
/// js-http-runner) before a `.wasm` component (instance-per-request `wasi:http`).
fn build_http_server(
    dir: &Path,
    wasm: &WasmRuntime,
    name: &str,
    caps: Capabilities,
    remote: Option<Vec<u8>>,
) -> Result<HttpServer> {
    if let Some(bundle) = remote {
        let bundle = String::from_utf8(bundle).context("URL/kv bundle is not valid UTF-8 JS")?;
        return Ok(wasm.http_server_js(bundle, caps));
    }
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
    remote: Option<Vec<u8>>,
) -> Result<WsServer> {
    if let Some(bundle) = remote {
        return Ok(wasm.ws_server_js(bundle, caps));
    }
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
    remote: Option<Vec<u8>>,
) -> Result<ResidentHttpServer> {
    let wasm_dir = dir.join("wasm");
    let name = &spec.name;
    let js_path = wasm_dir.join(format!("{name}.js"));
    let server = if let Some(bundle) = remote {
        wasm.resident_http_server_js(bundle, caps, spec.instances)
    } else if js_path.is_file() {
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
    remote: Option<Vec<u8>>,
) -> Result<ResidentWsServer> {
    let wasm_dir = dir.join("wasm");
    let name = &spec.name;
    let js_path = wasm_dir.join(format!("{name}.js"));
    let server = if let Some(bundle) = remote {
        wasm.resident_ws_server_js(bundle, caps, spec.instances)
    } else if js_path.is_file() {
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
        let cfg = rusm_node::NodeConfig::from_toml(
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
            storage: None,
            max_memory_mb: None,
            env: Vec::new(),
            preopen: Vec::new(),
        };
        assert!(
            !to_capabilities(&bare).can_spawn(),
            "default base is sandboxed"
        );
    }

    #[test]
    fn storage_grant_maps_through() {
        // `storage = true` on a profile turns the durable-KV grant on…
        let cfg = rusm_node::NodeConfig::from_toml(
            "[capabilities.stateful]\ninherits = \"trusted\"\nstorage = true\n",
        )
        .unwrap();
        assert!(to_capabilities(&cfg.capabilities["stateful"]).storage_allowed());
        // …and omitting it inherits the base (sandboxed → no storage).
        let cfg = rusm_node::NodeConfig::from_toml("[capabilities.x]\ninherits = \"sandboxed\"\n")
            .unwrap();
        assert!(!to_capabilities(&cfg.capabilities["x"]).storage_allowed());
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
                storage: None,
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
            source: None,
        }];
        let handles = spawn_components(dir.path(), &wasm, &specs, &HashMap::new())
            .await
            .unwrap();
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
            source: None,
        }];
        let handles = spawn_components(dir.path(), &wasm, &specs, &HashMap::new())
            .await
            .unwrap();
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
            source: None,
        }];
        let endpoints = serve_apps(
            dir.path(),
            &wasm,
            &specs,
            &RouteTable::default(),
            &[],
            &HashMap::new(),
        )
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
            source: None,
        }];
        let endpoints = serve_apps(
            dir.path(),
            &wasm,
            &specs,
            &RouteTable::default(),
            &[],
            &HashMap::new(),
        )
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
            source: None,
        }];
        let endpoints = serve_apps(
            dir.path(),
            &wasm,
            &specs,
            &RouteTable::default(),
            &[],
            &HashMap::new(),
        )
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

    // A `#[rusm_rs::handlers]` component: `fn hello(_, params)` + `fn echo(req, _)`.
    const RS_HANDLERS: &[u8] =
        include_bytes!("../../crates/rusm-wasm/tests/fixtures/rs_handlers_demo.wasm");

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_routes_table_dispatches_per_request_to_a_handler_component() {
        // The unified model end-to-end via `rusm serve`: a `[routes]` table fronts an
        // HTTP `[[serve]]` listener; each request spawns the matched handler fresh and
        // dispatches `component#action`. The handler is just `fn`s — no router code.
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("wasm")).unwrap();
        std::fs::write(dir.path().join("wasm/api.wasm"), RS_HANDLERS).unwrap();

        let rt = Runtime::new();
        let wasm = WasmRuntime::new(rt).unwrap();
        let specs = vec![ServeSpec {
            name: "api".to_string(),
            protocol: ServeProtocol::Http,
            listen: "127.0.0.1:0".to_string(),
            capability: "sandboxed".to_string(),
            mode: ServeMode::PerRequest,
            instances: 1,
            shard_by: None,
            max_inflight: None,
            source: None,
        }];
        let routes = RouteTable::from_map(&HashMap::from([
            ("GET /hello/:name".to_string(), "api#hello".to_string()),
            ("POST /echo".to_string(), "api#echo".to_string()),
        ]))
        .unwrap();
        let endpoints = serve_apps(dir.path(), &wasm, &specs, &routes, &[], &HashMap::new())
            .await
            .unwrap();
        let addr = endpoints[0].addr;

        let send = |method: &'static str, path: &'static str, body: &'static str| async move {
            let req = format!(
                "{method} {path} HTTP/1.1\r\nHost: rusm\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{body}",
                body.len()
            );
            let mut conn = TcpStream::connect(addr).await.unwrap();
            conn.write_all(req.as_bytes()).await.unwrap();
            let mut buf = Vec::new();
            conn.read_to_end(&mut buf).await.unwrap();
            String::from_utf8_lossy(&buf).into_owned()
        };

        let hello = send("GET", "/hello/ada", "").await;
        assert!(hello.starts_with("HTTP/1.1 200"), "got: {hello}");
        assert!(hello.contains("hi ada"), "param dispatched: {hello}");
        assert!(send("POST", "/echo", "pong").await.contains("pong"), "echo");
        assert!(
            send("GET", "/nope", "").await.starts_with("HTTP/1.1 404"),
            "unmatched path is 404"
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
            mode: ServeMode::PerRequest,
            instances: 1,
            shard_by: None,
            max_inflight: None,
            source: None,
        }];
        let err = serve_apps(
            dir.path(),
            &wasm,
            &specs,
            &RouteTable::default(),
            &[],
            &HashMap::new(),
        )
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
            source: None,
        }];
        let err = spawn_components(dir.path(), &wasm, &specs, &HashMap::new())
            .await
            .err()
            .expect("missing artifact must error");
        assert!(err.to_string().contains("absent.wasm"));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn spawns_a_component_whose_bundle_is_sourced_from_kv() {
        // The dynamic-deploy path: a component with no local artifact, its JS bundle
        // read from the node's durable store. Put a worker that registers a name, then
        // spawn it from `kv:` and confirm it actually ran.
        let dir = tempfile::tempdir().unwrap();
        let rt = Runtime::new();
        let wasm = WasmRuntime::with_store(rt.clone(), dir.path().join("kv.redb")).unwrap();
        let bundle =
            b"module.exports.default = async function(){ Process.register('from-kv'); for(;;) await Process.receive(); };";
        wasm.store()
            .unwrap()
            .bucket("bundles")
            .set("greeter", bundle)
            .unwrap();

        let specs = vec![ComponentSpec {
            name: "greeter".to_string(),
            capability: "sandboxed".to_string(),
            restart: false,
            source: Some("kv:bundles/greeter".to_string()),
        }];
        let handles = spawn_components(dir.path(), &wasm, &specs, &HashMap::new())
            .await
            .unwrap();
        assert_eq!(handles.len(), 1);
        // The kv-sourced bundle actually ran on the js-runner: it registered its name.
        for _ in 0..200 {
            if rt.whereis("from-kv").is_some() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        assert!(
            rt.whereis("from-kv").is_some(),
            "the kv-sourced JS component ran and registered its name"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn resolves_a_bundle_from_a_url() {
        use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
        // A minimal HTTP server returning a JS bundle body.
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let body = "console.log('hi');";
        tokio::spawn(async move {
            if let Ok((mut stream, _)) = listener.accept().await {
                let mut buf = [0u8; 1024];
                let _ = stream.read(&mut buf).await; // consume the request head
                let resp = format!(
                    "HTTP/1.1 200 OK\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(resp.as_bytes()).await;
            }
        });

        let wasm = WasmRuntime::new(Runtime::new()).unwrap();
        let bundle = remote_bundle(Some(&format!("url:http://{addr}/x.js")), &wasm)
            .await
            .unwrap();
        assert_eq!(bundle.as_deref(), Some(body.as_bytes()));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn bundle_source_errors_are_clear() {
        let rt = Runtime::new();
        // No source → None (fall back to the local artifact).
        let plain = WasmRuntime::new(rt.clone()).unwrap();
        assert!(remote_bundle(None, &plain).await.unwrap().is_none());
        // A kv source with no store configured is a clear error.
        assert!(remote_bundle(Some("kv:b/k"), &plain).await.is_err());
        // A store, but a missing key.
        let dir = tempfile::tempdir().unwrap();
        let stored = WasmRuntime::with_store(rt, dir.path().join("kv.redb")).unwrap();
        assert!(remote_bundle(Some("kv:b/absent"), &stored).await.is_err());
        // An unrecognised source shape.
        assert!(remote_bundle(Some("ftp://x"), &stored).await.is_err());
    }
}
