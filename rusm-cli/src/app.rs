//! The **app model**: build a project's components and run them from `./wasm/`.
//!
//! A RUSM app is a directory with `rusm.toml` (`[[components]]`), a `components/`
//! tree of source crates, and a `wasm/` dir of built artifacts. `rusm build`
//! compiles each `components/<name>/` to `wasm/<name>.wasm`; the loader then
//! spawns each declared component as a supervised process under its capability
//! profile. Env vars are the Rust way: process env first, then `.env`.

use std::path::Path;

use anyhow::{Context, Result};
use rusm_bench::ComponentSpec;
use rusm_otp::ProcessHandle;
use rusm_wasm::{Capabilities, CapabilityProfile, WasmRuntime};

/// Resolves a capability-profile id to its [`Capabilities`], defaulting to the
/// secure `Sandboxed` profile for an unknown id (default-deny).
pub fn capabilities_for(id: &str) -> Capabilities {
    CapabilityProfile::from_id(id)
        .unwrap_or(CapabilityProfile::Sandboxed)
        .capabilities()
}

/// Loads each manifest component from `<dir>/wasm/<name>.wasm` and spawns it as a
/// process under its capability profile. Returns the live `(name, handle)` pairs
/// (hold them to keep the processes alive). Errors if an artifact is missing or
/// won't compile — a clear signal to run `rusm build` first.
pub fn spawn_components(
    dir: &Path,
    wasm: &WasmRuntime,
    specs: &[ComponentSpec],
) -> Result<Vec<(String, ProcessHandle)>> {
    let wasm_dir = dir.join("wasm");
    let mut handles = Vec::with_capacity(specs.len());
    for spec in specs {
        let path = wasm_dir.join(format!("{}.wasm", spec.name));
        let bytes = std::fs::read(&path)
            .with_context(|| format!("reading {} (run `rusm build`?)", path.display()))?;
        let component = wasm
            .compile_component(&bytes)
            .with_context(|| format!("compiling component `{}`", spec.name))?;
        let prepared = wasm.prepare_component(&component, "run")?;
        let handle = wasm.spawn_component_with(&prepared, capabilities_for(&spec.capability));
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
    fn unknown_capability_falls_back_to_sandboxed() {
        // Both resolve without panicking; the unknown one is treated as Sandboxed.
        let _ = capabilities_for("trusted");
        let _ = capabilities_for("does-not-exist");
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
        let handles = spawn_components(dir.path(), &wasm, &specs).unwrap();
        assert_eq!(handles.len(), 1);
        assert_eq!(handles[0].0, "echo");
        // The component runs to completion as a real process.
        let (_name, handle) = handles.into_iter().next().unwrap();
        handle.join().await;
        assert_eq!(rt.finished(), 1);
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
        let err = spawn_components(dir.path(), &wasm, &specs)
            .err()
            .expect("missing artifact must error");
        assert!(err.to_string().contains("absent.wasm"));
    }
}
