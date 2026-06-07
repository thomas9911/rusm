//! The **wasip3 bridge**: preview3 WASI on the component linker, **additive over
//! [`wasip2`](super::wasip2)**.
//!
//! wasip3 is the async/streams generation of WASI (the `@0.3.0` interfaces). It
//! ships in `wasmtime-wasi` today, so this is just wiring: [`add_to_linker`] adds
//! the p3 host implementations to the *same* component [`Linker`] the wasip2 bridge
//! builds, sharing the one [`WasiHost`]. A component that imports the `@0.2.0`
//! interfaces and one that imports `@0.3.0` both resolve against the same host —
//! no separate runtime, no separate store type.

use wasmtime::component::Linker;

use super::WasiHost;

/// Adds the preview3 WASI interfaces (`wasi:cli`/`clocks`/`filesystem`/`random`/
/// `sockets`@0.3.0) to the component linker, on top of the p2 interfaces.
pub(crate) fn add_to_linker(linker: &mut Linker<WasiHost>) -> wasmtime::Result<()> {
    wasmtime_wasi::p3::add_to_linker(linker)
}

#[cfg(test)]
mod tests {
    use crate::WasmRuntime;
    use rusm_otp::{ExitReason, Received, Runtime};

    // A real component (checked in) that imports `wasi:random/random@0.3.0` — a
    // **preview3** interface — and calls `get-random-u64` on `run`. If the wasip3
    // bridge didn't resolve and execute p3 imports, it would fail to instantiate
    // or trap; a Normal exit proves p3 works end to end.
    const P3_RANDOM: &[u8] = include_bytes!("../../tests/fixtures/p3_random.wasm");

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_component_using_a_wasip3_interface_runs() {
        let rt = Runtime::new();
        let wr = WasmRuntime::new(rt.clone()).unwrap();
        let pre = wr
            .prepare_component(&wr.compile_component(P3_RANDOM).unwrap(), "run")
            .unwrap();
        let guest = wr.spawn_component(&pre);

        // Monitor it and assert it ran to completion (p3 imports resolved + ran).
        let (tx, rx) = tokio::sync::oneshot::channel();
        let watcher = rt
            .spawn(move |mut ctx| async move {
                let _ = tx.send(ctx.recv().await);
            })
            .pid();
        rt.monitor(watcher, guest.pid());
        match rx.await.unwrap() {
            Received::Down { reason, .. } => assert_eq!(
                reason,
                ExitReason::Normal,
                "a component importing wasi:random@0.3.0 (p3) must run to completion"
            ),
            other => panic!("expected a Down, got {other:?}"),
        }
    }
}
