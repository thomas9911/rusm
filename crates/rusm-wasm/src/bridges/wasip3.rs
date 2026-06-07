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
