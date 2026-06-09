//! Build-time **QuickJS bytecode** compiler for RUSM TypeScript bundles.
//!
//! A TS component is a Bun-built `.js` bundle the shared js-runner evaluates. Parsing
//! that source is pure startup cost (paid per cold instance / per request on the
//! instance-per-request HTTP path). This crate compiles the bundle to QuickJS
//! **bytecode** at `rusm build` time so the runner skips straight to the VM.
//!
//! **Version lock (critical):** QuickJS bytecode is tied to the exact engine. This
//! crate pins `rquickjs = "=0.9.0"` — byte-identical to the js-runner's engine
//! (`crates/rusm-wasm/js-runner`). Default features → the same bundled quickjs-ng.
//! The format is little-endian and target-independent, so host-compiled bytecode
//! loads in the wasm runner. If the runner's rquickjs ever changes, bump this in
//! lock-step (the round-trip test guards validity, not cross-version drift).

use anyhow::{Context as _, Result};
use rquickjs::{Context, Module, Runtime};

/// Magic prefix marking a bundle payload as bytecode (vs raw JS source), so the
/// runner can accept either: `QJSB` + little-endian QuickJS module bytecode.
pub const MAGIC: &[u8; 4] = b"QJSB";

/// The CommonJS wrapper the js-runner applies to a bundle, so its top-level
/// declarations don't leak into globals and `module`/`exports` are in scope. Must
/// match the runner's runtime wrapper exactly, so source-mode and bytecode-mode are
/// semantically identical.
fn wrap_cjs(bundle: &str) -> String {
    format!(
        "(function(module,exports){{\n{bundle}\n}})(globalThis.module,globalThis.module.exports);"
    )
}

/// Compiles a JS/TS bundle (Bun-built CJS source) to a `MAGIC`-prefixed QuickJS
/// bytecode payload. Compile-only (`Module::declare`) — never executed here — so the
/// runtime globals it references need not exist at build time.
pub fn compile(bundle: &str) -> Result<Vec<u8>> {
    let wrapped = wrap_cjs(bundle);
    let rt = Runtime::new().context("quickjs runtime")?;
    let ctx = Context::full(&rt).context("quickjs context")?;
    let bytecode = ctx.with(|ctx| -> Result<Vec<u8>> {
        let module = Module::declare(ctx.clone(), "bundle", wrapped).context("compile bundle")?;
        module.write_le().context("write bytecode")
    })?;
    let mut out = Vec::with_capacity(MAGIC.len() + bytecode.len());
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&bytecode);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bytecode_round_trips_in_the_same_engine() {
        // Compile a tiny CJS bundle to bytecode, then load+run it in a *fresh* context
        // (the same rquickjs 0.9.0 the runner embeds). This proves the bytecode is
        // valid for the runner's engine — the whole correctness premise.
        let payload = compile("module.exports.default = 6 * 7;").unwrap();
        assert_eq!(&payload[..MAGIC.len()], &MAGIC[..], "MAGIC-prefixed");

        let rt = Runtime::new().unwrap();
        let ctx = Context::full(&rt).unwrap();
        ctx.with(|ctx| {
            // The runner sets up the CJS shim before loading the bundle module.
            ctx.eval::<(), _>("globalThis.module={exports:{}};globalThis.exports=module.exports;")
                .unwrap();
            let module = unsafe { Module::load(ctx.clone(), &payload[MAGIC.len()..]) }.unwrap();
            let (_m, promise) = module.eval().unwrap();
            promise.finish::<()>().unwrap();
            let answer: i32 = ctx.eval("globalThis.module.exports.default").unwrap();
            assert_eq!(answer, 42, "the bytecode ran and populated module.exports");
        });
    }
}
