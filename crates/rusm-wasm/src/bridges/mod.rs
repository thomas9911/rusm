//! WASI bridges: per-version glue from a Wasm artifact to a `rusm-otp` process,
//! over the shared core in [`crate`] (engine, epoch ticker, pooling allocator).
//!
//! A bridge differs only in *artifact kind* (core module vs component) and which
//! WASI version it wires; the engine, preemption and pooling are shared. Keeping
//! each version in its own file keeps `lib.rs` lean (the project's file-splitting
//! convention) and makes "add a WASI version" a local change.

pub(crate) mod http;
pub(crate) mod resident;
pub(crate) mod wasip1;
pub(crate) mod wasip2;
pub(crate) mod wasip3;
pub(crate) mod ws;

use std::collections::HashMap;
use std::sync::Arc;

use rusm_otp::{Context, Runtime, StreamHandle, StreamWriter};
use wasmtime::ResourceLimiter;
use wasmtime_wasi::{ResourceTable, WasiCtx, WasiCtxView, WasiView};
use wasmtime_wasi_http::p2::{WasiHttpCtxView, WasiHttpView};
use wasmtime_wasi_http::WasiHttpCtx;

use crate::caps::Capabilities;
use crate::Spawner;

/// Store data for **component** guests (wasip2 today, wasip3 later): the WASI
/// context + resource table the component model needs, a per-process memory
/// ceiling enforced as a `ResourceLimiter`, and the actor handles (pid, runtime,
/// mailbox) that back the `rusm:runtime` host ABI. One host type serves both WASI
/// versions, since both `add_to_linker` entry points only require [`WasiView`].
pub(crate) struct WasiHost {
    pub(crate) wasi: WasiCtx,
    pub(crate) table: ResourceTable,
    /// `wasi:http` host context, for serving a component as an HTTP handler
    /// (Phase 11). Idle for non-HTTP guests.
    pub(crate) http: WasiHttpCtx,
    /// The owning process's pid (for `own-pid`, `register`, `set-label`).
    pub(crate) pid: u64,
    /// This process's capabilities: the source of truth for its memory ceiling,
    /// whether it may control other processes, whether it may spawn, and the
    /// ceiling any child it spawns inherits (a child is never broader).
    pub(crate) caps: Capabilities,
    /// Handle to the actor runtime, backing the actor host functions.
    pub(crate) rt: Runtime,
    /// The process's mailbox, for `receive`. `None` only for a bare host built
    /// outside a spawned process (e.g. direct inspection in a test); a running
    /// guest always has one.
    pub(crate) ctx: Option<Context>,
    /// The shared spawn core, so this process may `spawn` registered components.
    /// `None` only for a bare host built outside the runtime (a test).
    pub(crate) spawner: Option<Arc<Spawner>>,
    /// Byte streams this process is **writing** to others, keyed by the handle
    /// returned to the guest by `stream-open`.
    pub(crate) out_streams: HashMap<u64, StreamWriter>,
    /// Byte streams this process has **accepted** and is reading, keyed by the
    /// handle returned by `stream-accept`.
    pub(crate) in_streams: HashMap<u64, StreamHandle>,
    /// Monotonic handle source for this process's streams.
    pub(crate) next_stream: u64,
}

impl WasiView for WasiHost {
    fn ctx(&mut self) -> WasiCtxView<'_> {
        WasiCtxView {
            ctx: &mut self.wasi,
            table: &mut self.table,
        }
    }
}

impl WasiHttpView for WasiHost {
    fn http(&mut self) -> WasiHttpCtxView<'_> {
        WasiHttpCtxView {
            ctx: &mut self.http,
            table: &mut self.table,
            hooks: Default::default(),
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
        Ok(desired <= self.caps.memory_limit())
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
            http: WasiHttpCtx::new(),
            pid: 0,
            caps: Capabilities::nothing(),
            rt: Runtime::new(),
            ctx: None,
            spawner: None,
            out_streams: HashMap::new(),
            in_streams: HashMap::new(),
            next_stream: 0,
        };
        // The table reached through the view is the real one: a pushed resource
        // round-trips through it.
        let view = host.ctx();
        let handle: Resource<u32> = view.table.push(7u32).unwrap();
        assert_eq!(*view.table.get(&handle).unwrap(), 7);
    }
}
