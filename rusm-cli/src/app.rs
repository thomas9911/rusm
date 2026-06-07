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
use std::path::Path;

use anyhow::{Context, Result};
use rusm_bench::{CapabilitySpec, ComponentSpec};
use rusm_otp::ProcessHandle;
use rusm_wasm::{Capabilities, CapabilityProfile, WasmRuntime};

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

#[cfg(test)]
mod tests {
    use super::*;
    use rusm_otp::Runtime;

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
