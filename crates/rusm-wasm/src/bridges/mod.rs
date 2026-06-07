//! WASI bridges: per-version glue from a Wasm artifact to a `rusm-otp` process,
//! over the shared core in [`crate`] (engine, epoch ticker, pooling allocator).
//!
//! A bridge differs only in *artifact kind* (core module vs component) and which
//! WASI version it wires; the engine, preemption and pooling are shared. Keeping
//! each version in its own file keeps `lib.rs` lean (the project's file-splitting
//! convention) and makes "add a WASI version" a local change.

pub(crate) mod wasip1;
pub(crate) mod wasip2;
pub(crate) mod wasip3;

use rusm_otp::{Context, Runtime};
use wasmtime::ResourceLimiter;
use wasmtime_wasi::{ResourceTable, WasiCtx, WasiCtxView, WasiView};

/// Store data for **component** guests (wasip2 today, wasip3 later): the WASI
/// context + resource table the component model needs, a per-process memory
/// ceiling enforced as a `ResourceLimiter`, and the actor handles (pid, runtime,
/// mailbox) that back the `rusm:runtime` host ABI. One host type serves both WASI
/// versions, since both `add_to_linker` entry points only require [`WasiView`].
pub(crate) struct WasiHost {
    pub(crate) wasi: WasiCtx,
    pub(crate) table: ResourceTable,
    /// Logical linear-memory cap (bytes) from the process's capabilities.
    pub(crate) max_memory: usize,
    /// The owning process's pid (for `own-pid`, `register`, `set-label`).
    pub(crate) pid: u64,
    /// Handle to the actor runtime, backing the actor host functions.
    pub(crate) rt: Runtime,
    /// The process's mailbox, for `receive`. `None` only for a bare host built
    /// outside a spawned process (e.g. direct inspection in a test); a running
    /// guest always has one.
    pub(crate) ctx: Option<Context>,
}

impl WasiView for WasiHost {
    fn ctx(&mut self) -> WasiCtxView<'_> {
        WasiCtxView {
            ctx: &mut self.wasi,
            table: &mut self.table,
        }
    }
}

impl ResourceLimiter for WasiHost {
    /// Denies growth past the capability's memory ceiling — `memory.grow` then
    /// returns -1 to the guest (no host trap), the standard sandbox signal.
    fn memory_growing(
        &mut self,
        _current: usize,
        desired: usize,
        _maximum: Option<usize>,
    ) -> wasmtime::Result<bool> {
        Ok(desired <= self.max_memory)
    }

    fn table_growing(
        &mut self,
        _current: usize,
        _desired: usize,
        _maximum: Option<usize>,
    ) -> wasmtime::Result<bool> {
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wasmtime::component::Resource;
    use wasmtime_wasi::WasiCtxBuilder;

    #[test]
    fn wasi_view_exposes_a_live_table() {
        let mut host = WasiHost {
            wasi: WasiCtxBuilder::new().build(),
            table: ResourceTable::new(),
            max_memory: 1 << 20,
            pid: 0,
            rt: Runtime::new(),
            ctx: None,
        };
        // The table reached through the view is the real one: a pushed resource
        // round-trips through it.
        let view = host.ctx();
        let handle: Resource<u32> = view.table.push(7u32).unwrap();
        assert_eq!(*view.table.get(&handle).unwrap(), 7);
    }
}
