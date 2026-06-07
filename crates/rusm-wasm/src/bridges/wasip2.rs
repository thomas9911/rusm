//! The **wasip2 bridge**: run a WASI **component** as a `rusm-otp` process.
//!
//! Mirrors the core-module path in [`crate`] (`compile`/`prepare`/`spawn`) but for
//! the component model — `Component`, a component [`Linker`](ComponentLinker) with
//! WASI p2 wired in, and a per-process [`WasiHost`]. Instance-per-process, epoch
//! preemption and the pooling allocator all carry over unchanged; a trap exits the
//! process [`Crashed`](ExitReason::Crashed). The shared efficiency levers live in
//! `lib.rs`; this file is only the component-specific glue.

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::Result;
use rusm_otp::{Context, ExitReason, ProcessHandle};
use wasmtime::component::{
    Component, ComponentExportIndex, InstancePre as ComponentInstancePre, Linker as ComponentLinker,
};
use wasmtime::{Engine, ResourceLimiter, Store};
use wasmtime_wasi::ResourceTable;

use super::WasiHost;
use crate::caps::{Capabilities, CapabilityProfile};
use crate::{Spawner, WasmRuntime};

/// A component whose imports are resolved **and** whose entry-export index is
/// precomputed — so a spawn skips both per-spawn import resolution *and* the
/// by-name export lookup. Opaque on purpose: it hides the internal WASI host type.
#[derive(Clone)]
pub struct PreparedComponent {
    pre: ComponentInstancePre<WasiHost>,
    /// The `entry` export resolved once at prepare time (index, not a string).
    entry: ComponentExportIndex,
}

/// Builds the component linker once, with WASI **p2 and p3** wired in plus the
/// `rusm:runtime` actor ABI — all sharing one [`WasiHost`]. A component importing
/// the `@0.2.0` or `@0.3.0` WASI interfaces resolves against the same host.
pub(crate) fn build_linker(engine: &Engine) -> Result<ComponentLinker<WasiHost>> {
    let mut linker = ComponentLinker::new(engine);
    wasmtime_wasi::p2::add_to_linker_async(&mut linker)?;
    super::wasip3::add_to_linker(&mut linker)?;
    crate::actor::add_to_linker(&mut linker)?;
    Ok(linker)
}

impl WasmRuntime {
    /// Compiles a component from Wasm bytes or component-model `.wat` text.
    pub fn compile_component(&self, wasm: impl AsRef<[u8]>) -> Result<Component> {
        Ok(Component::new(&self.spawner.engine, wasm)?)
    }

    /// Resolves a component's imports **once** against the WASI linker and
    /// precomputes its `entry` export index — the fast path for spawning the same
    /// component+entrypoint many times (no per-spawn import resolution or by-name
    /// export lookup). Errors if the component has no such export.
    pub fn prepare_component(
        &self,
        component: &Component,
        entry: &str,
    ) -> Result<PreparedComponent> {
        let pre = self.component_linker.instantiate_pre(component)?;
        let entry = component
            .get_export_index(None, entry)
            .ok_or_else(|| anyhow::anyhow!("component has no `{entry}` export"))?;
        Ok(PreparedComponent { pre, entry })
    }

    /// Spawns a prepared component as an isolated process under the **default-deny
    /// `Sandboxed`** profile (no fs/net/env, a bounded heap). Use
    /// [`spawn_component_with`](WasmRuntime::spawn_component_with) to grant more.
    pub fn spawn_component(&self, prepared: &PreparedComponent) -> ProcessHandle {
        self.spawn_component_with(prepared, CapabilityProfile::Sandboxed.capabilities())
    }

    /// Spawns a prepared component as an isolated process running its entry export,
    /// under the given [`Capabilities`]. A fresh instance + WASI context per
    /// process; a trap (or a denied capability the guest turns into a trap) exits
    /// the process [`Crashed`](ExitReason::Crashed).
    pub fn spawn_component_with(
        &self,
        prepared: &PreparedComponent,
        caps: Capabilities,
    ) -> ProcessHandle {
        self.spawner.spawn_component(prepared, caps)
    }
}

impl Spawner {
    /// Spawns a prepared component as an isolated process under `caps` — the single
    /// spawn path shared by the public [`spawn_component_with`] and a guest's
    /// capability-gated `spawn`. The child carries an `Arc` to this spawner, so it
    /// too can spawn siblings by name.
    ///
    /// [`spawn_component_with`]: WasmRuntime::spawn_component_with
    pub(crate) fn spawn_component(
        self: &Arc<Self>,
        prepared: &PreparedComponent,
        caps: Capabilities,
    ) -> ProcessHandle {
        let spawner = Arc::clone(self);
        let pre = prepared.pre.clone();
        let entry = prepared.entry;
        self.rt
            .spawn(move |ctx| run(spawner, pre, entry, caps, ctx))
    }
}

/// The process body for a component: build its WASI context, instantiate it in a
/// fresh store, and run its entry export — exiting [`Crashed`](ExitReason::Crashed)
/// on any failure. The runtime handle is cloned exactly once (into the host) and
/// the crash-exit reads it back through the store; the engine is borrowed from the
/// `Arc<Spawner>` the host carries. Yields to the scheduler on each epoch tick.
async fn run(
    spawner: Arc<Spawner>,
    pre: ComponentInstancePre<WasiHost>,
    entry: ComponentExportIndex,
    caps: Capabilities,
    ctx: Context,
) {
    let pid = ctx.pid();
    let wasi = match caps.build_wasi() {
        Ok(wasi) => wasi,
        Err(_) => {
            spawner.rt.exit(pid, ExitReason::Crashed);
            return;
        }
    };
    let engine = spawner.engine.clone();
    let rt = spawner.rt.clone();
    let host = WasiHost {
        wasi,
        table: ResourceTable::new(),
        pid: pid.raw(),
        caps,
        rt,
        ctx: Some(ctx),
        spawner: Some(spawner),
        out_streams: HashMap::new(),
        in_streams: HashMap::new(),
        next_stream: 0,
    };
    let mut store = Store::new(&engine, host);
    // Enforce the per-process memory ceiling (WasiHost is the ResourceLimiter).
    store.limiter(|host| host as &mut dyn ResourceLimiter);
    store.set_epoch_deadline(1);
    store.epoch_deadline_async_yield_and_update(1);

    let outcome = async {
        let instance = pre.instantiate_async(&mut store).await?;
        // Precomputed index — no per-spawn by-name export lookup.
        let func = instance.get_typed_func::<(), ()>(&mut store, entry)?;
        func.call_async(&mut store, ()).await
    }
    .await;
    if outcome.is_err() {
        // The host (and its runtime handle) is still in the store — no extra clone.
        store.data().rt.exit(pid, ExitReason::Crashed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusm_otp::Runtime;

    // A minimal component exporting `run: func()` — no imports, no WASI use.
    const COMP_RUN: &str = r#"(component
        (core module $m (func (export "run")))
        (core instance $i (instantiate $m))
        (func (export "run") (canon lift (core func $i "run"))))"#;

    const COMP_TRAP: &str = r#"(component
        (core module $m (func (export "run") unreachable))
        (core instance $i (instantiate $m))
        (func (export "run") (canon lift (core func $i "run"))))"#;

    const COMP_SPIN: &str = r#"(component
        (core module $m (func (export "run") (loop (br 0))))
        (core instance $i (instantiate $m))
        (func (export "run") (canon lift (core func $i "run"))))"#;

    // Starts with one page (64 KiB) and tries to grow by two more (to 192 KiB);
    // if growth is denied (memory.grow returns -1) it traps. So a memory cap below
    // 192 KiB makes it crash, a cap at/above it lets it finish normally.
    const COMP_GROW: &str = r#"(component
        (core module $m
            (memory (export "mem") 1)
            (func (export "run")
                (if (i32.eq (memory.grow (i32.const 2)) (i32.const -1))
                    (then unreachable))))
        (core instance $i (instantiate $m))
        (func (export "run") (canon lift (core func $i "run"))))"#;

    /// Monitors a freshly spawned guest and returns the exit reason it observes.
    async fn exit_reason_of(rt: &Runtime, guest: &ProcessHandle) -> ExitReason {
        let (tx, rx) = tokio::sync::oneshot::channel();
        let watcher = rt
            .spawn(move |mut ctx| async move {
                let _ = tx.send(ctx.recv().await);
            })
            .pid();
        rt.monitor(watcher, guest.pid());
        match rx.await.unwrap() {
            rusm_otp::Received::Down { reason, .. } => reason,
            other => panic!("expected a Down, got {other:?}"),
        }
    }

    /// Monitors a process *by pid* (a guest-spawned child returns only its pid).
    async fn exit_reason_of_pid(rt: &Runtime, pid: rusm_otp::Pid) -> ExitReason {
        let (tx, rx) = tokio::sync::oneshot::channel();
        let watcher = rt
            .spawn(move |mut ctx| async move {
                let _ = tx.send(ctx.recv().await);
            })
            .pid();
        rt.monitor(watcher, pid);
        match rx.await.unwrap() {
            rusm_otp::Received::Down { reason, .. } => reason,
            other => panic!("expected a Down, got {other:?}"),
        }
    }

    /// A bare [`WasiHost`] wired to the runtime's spawner — stands in for a running
    /// guest so the capability-gated `spawn` host fn can be driven directly.
    fn test_host(wr: &WasmRuntime, rt: &Runtime, caps: Capabilities) -> WasiHost {
        WasiHost {
            wasi: caps.build_wasi().unwrap(),
            table: ResourceTable::new(),
            pid: 0,
            caps,
            rt: rt.clone(),
            ctx: None,
            spawner: Some(Arc::clone(&wr.spawner)),
            out_streams: HashMap::new(),
            in_streams: HashMap::new(),
            next_stream: 0,
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_guest_can_spawn_a_registered_component() {
        use crate::actor::rusm::runtime::actor::Host;
        let rt = Runtime::new();
        let wr = WasmRuntime::new(rt.clone()).unwrap();
        let child = wr
            .prepare_component(&wr.compile_component(COMP_RUN).unwrap(), "run")
            .unwrap();
        wr.register_component("child", child);

        // Trusted grants the spawn capability.
        let mut host = test_host(&wr, &rt, CapabilityProfile::Trusted.capabilities());
        host.spawn("child".to_string())
            .await
            .expect("spawn of a registered component succeeds");

        // The child runs to completion as a real, reaped process.
        for _ in 0..200 {
            if rt.finished() == 1 {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        assert_eq!(rt.finished(), 1, "the spawned child ran and was reaped");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn spawn_is_denied_without_the_capability() {
        use crate::actor::rusm::runtime::actor::Host;
        let rt = Runtime::new();
        let wr = WasmRuntime::new(rt.clone()).unwrap();
        let child = wr
            .prepare_component(&wr.compile_component(COMP_RUN).unwrap(), "run")
            .unwrap();
        wr.register_component("child", child);

        // Sandboxed (default-deny): no spawn capability.
        let mut host = test_host(&wr, &rt, CapabilityProfile::Sandboxed.capabilities());
        let err = host.spawn("child".to_string()).await.unwrap_err();
        assert!(
            err.contains("denied"),
            "sandboxed spawn must be denied: {err}"
        );
        assert_eq!(rt.process_count(), 0, "no child was created");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn spawn_of_an_unknown_component_errors() {
        use crate::actor::rusm::runtime::actor::Host;
        let rt = Runtime::new();
        let wr = WasmRuntime::new(rt.clone()).unwrap();
        let mut host = test_host(&wr, &rt, CapabilityProfile::Trusted.capabilities());
        let err = host.spawn("ghost".to_string()).await.unwrap_err();
        assert!(err.contains("unknown component"), "{err}");
        assert_eq!(rt.process_count(), 0, "nothing spawned");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_spawned_child_inherits_the_parents_capabilities() {
        use crate::actor::rusm::runtime::actor::Host;
        // The child is non-escalating: it gets the spawner's caps. A parent with a
        // tight memory cap yields a child that crashes growing; a roomy parent's
        // child finishes — same component, capability inherited from the spawner.
        let rt = Runtime::new();
        let wr = WasmRuntime::new(rt.clone()).unwrap();
        let grow = wr
            .prepare_component(&wr.compile_component(COMP_GROW).unwrap(), "run")
            .unwrap();
        wr.register_component("grow", grow);

        let tight = Capabilities::nothing()
            .allow_spawn(true)
            .max_memory(64 << 10);
        let mut parent = test_host(&wr, &rt, tight);
        let crashed = parent.spawn("grow".to_string()).await.unwrap();
        assert_eq!(
            exit_reason_of_pid(&rt, rusm_otp::Pid::from_raw(crashed)).await,
            ExitReason::Crashed,
            "child inherited the tight cap and crashed growing"
        );

        let roomy = Capabilities::nothing()
            .allow_spawn(true)
            .max_memory(8 << 20);
        let mut parent = test_host(&wr, &rt, roomy);
        let finished = parent.spawn("grow".to_string()).await.unwrap();
        assert_eq!(
            exit_reason_of_pid(&rt, rusm_otp::Pid::from_raw(finished)).await,
            ExitReason::Normal,
            "child inherited the roomy cap and finished"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn components_call_each_other_via_the_actor_abi() {
        // Two instances of one component: the first registers "responder" and
        // serves request/reply; the second finds it via whereis and calls it,
        // forwarding the reply to a native collector — component-to-component
        // "callbacks" with no new runtime API, just the actor ABI.
        const CALLBACK: &[u8] = include_bytes!("../../tests/fixtures/callback.wasm");
        let rt = Runtime::new();
        let wr = WasmRuntime::new(rt.clone()).unwrap();
        let pre = wr
            .prepare_component(&wr.compile_component(CALLBACK).unwrap(), "run")
            .unwrap();

        // Native collector receives the caller's final result.
        let (tx, rx) = tokio::sync::oneshot::channel();
        let collector = rt.spawn(move |mut ctx| async move {
            let _ = tx.send(ctx.recv().await.message().unwrap());
        });
        assert!(rt.register("collector", collector.pid()));

        // Instance 1 → responder; wait until it has registered the name.
        let _responder = wr.spawn_component(&pre);
        for _ in 0..200 {
            if rt.whereis("responder").is_some() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        assert!(rt.whereis("responder").is_some(), "responder must register");

        // Instance 2 → caller: calls the responder (21 -> doubled -> 42).
        let _caller = wr.spawn_component(&pre);
        assert_eq!(rx.await.unwrap(), vec![42]);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn one_component_streams_bytes_to_another() {
        // Two instances of one component: a producer opens a byte stream to a
        // consumer and writes 3x "hello!" (18 bytes); the consumer accepts it,
        // reads to EOF, and reports the total — cross-process streaming through the
        // actor world, Tokio-backpressured.
        const PIPE: &[u8] = include_bytes!("../../tests/fixtures/stream_pipe.wasm");
        let rt = Runtime::new();
        let wr = WasmRuntime::new(rt.clone()).unwrap();
        let pre = wr
            .prepare_component(&wr.compile_component(PIPE).unwrap(), "run")
            .unwrap();

        // Native collector receives the consumer's byte total.
        let (tx, rx) = tokio::sync::oneshot::channel();
        let collector = rt.spawn(move |mut ctx| async move {
            let _ = tx.send(ctx.recv().await.message().unwrap());
        });

        let consumer = wr.spawn_component(&pre);
        let producer = wr.spawn_component(&pre);
        // consumer: [role 1][collector pid] — accept, read, report to collector.
        let mut cmsg = vec![1u8];
        cmsg.extend_from_slice(&collector.pid().raw().to_le_bytes());
        rt.send(consumer.pid(), cmsg);
        // producer: [role 0][consumer pid] — open, write, close.
        let mut pmsg = vec![0u8];
        pmsg.extend_from_slice(&consumer.pid().raw().to_le_bytes());
        rt.send(producer.pid(), pmsg);

        let total = rx.await.unwrap();
        assert_eq!(
            u32::from_le_bytes(total[..4].try_into().unwrap()),
            18,
            "consumer should read all 3x6 = 18 streamed bytes through to EOF"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_javascript_bundle_drives_the_actor_api() {
        // The rusm-ts js-runner: a component embedding rquickjs that runs a JS
        // bundle bridged to the actor world. The bundle uses `Process.receive`,
        // `Process.setLabel`, `Process.self`, and `Process.send` — proving a TS/JS
        // guest is a first-class, sandboxed RUSM process.
        const BUNDLE: &str = r#"
            module.exports.default = async function () {
                const replyTo = await Process.receiveText();   // msg: who to answer
                Process.setLabel("ts-worker");
                Process.send(replyTo, "pong from " + Process.self());
            };
        "#;
        let rt = Runtime::new();
        let wr = WasmRuntime::new(rt.clone()).unwrap();

        let (tx, rx) = tokio::sync::oneshot::channel();
        let collector = rt.spawn(move |mut ctx| async move {
            let _ = tx.send(ctx.recv().await.message().unwrap());
        });

        // spawn_js feeds the bundle as the first message; then the reply-to pid.
        let guest = wr.spawn_js(BUNDLE.as_bytes());
        rt.send(guest.pid(), collector.pid().raw().to_string().into_bytes());

        let reply = String::from_utf8(rx.await.unwrap()).unwrap();
        assert_eq!(
            reply,
            format!("pong from {}", guest.pid().raw()),
            "JS ran inside the component and drove the actor API"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn console_log_tolerates_bigint_pids() {
        // Pids surface as bigint; `console.log(Process.self())` must not throw
        // (JSON.stringify can't serialise bigint). If `fmt` threw, the bundle would
        // trap before replying — so a reply proves console handled the pid.
        const BUNDLE: &str = r#"
            module.exports.default = async function () {
                const replyTo = await Process.receiveText();
                console.log("my pid is", Process.self(), undefined);
                Process.send(replyTo, "logged ok");
            };
        "#;
        let rt = Runtime::new();
        let wr = WasmRuntime::new(rt.clone()).unwrap();
        let (tx, rx) = tokio::sync::oneshot::channel();
        let collector = rt.spawn(move |mut ctx| async move {
            let _ = tx.send(ctx.recv().await.message().unwrap());
        });
        let guest = wr.spawn_js(BUNDLE.as_bytes());
        rt.send(guest.pid(), collector.pid().raw().to_string().into_bytes());
        assert_eq!(String::from_utf8(rx.await.unwrap()).unwrap(), "logged ok");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_javascript_bundle_has_web_api_polyfills() {
        // The runner installs Web API polyfills (webapi.js) before the bundle, so a
        // TS guest gets URL/TextEncoder/etc. transparently — no host support needed.
        const BUNDLE: &str = r#"
            module.exports.default = async function () {
                const replyTo = await Process.receiveText();
                const u = new URL("https://example.io:8080/a?x=1");
                const n = new TextEncoder().encode("héllo").length;   // é = 2 bytes → 6
                Process.send(replyTo, u.hostname + "|" + u.port + "|" + n);
            };
        "#;
        let rt = Runtime::new();
        let wr = WasmRuntime::new(rt.clone()).unwrap();
        let (tx, rx) = tokio::sync::oneshot::channel();
        let collector = rt.spawn(move |mut ctx| async move {
            let _ = tx.send(ctx.recv().await.message().unwrap());
        });
        let guest = wr.spawn_js(BUNDLE.as_bytes());
        rt.send(guest.pid(), collector.pid().raw().to_string().into_bytes());
        assert_eq!(
            String::from_utf8(rx.await.unwrap()).unwrap(),
            "example.io|8080|6",
            "URL + TextEncoder polyfills work inside the guest"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_javascript_bundle_handles_binary_messages() {
        // JS receives a reply-to (text) and a binary message (Uint8Array), then
        // echoes the bytes back — proving binary marshalling both ways.
        const BUNDLE: &str = r#"
            module.exports.default = async function () {
                const replyTo = await Process.receiveText();
                const bytes = await Process.receive();   // Uint8Array
                Process.send(replyTo, bytes);            // send it back (binary path)
            };
        "#;
        let rt = Runtime::new();
        let wr = WasmRuntime::new(rt.clone()).unwrap();
        let (tx, rx) = tokio::sync::oneshot::channel();
        let collector = rt.spawn(move |mut ctx| async move {
            let _ = tx.send(ctx.recv().await.message().unwrap());
        });
        let guest = wr.spawn_js(BUNDLE.as_bytes());
        rt.send(guest.pid(), collector.pid().raw().to_string().into_bytes());
        rt.send(guest.pid(), vec![7, 8, 9]);
        assert_eq!(rx.await.unwrap(), vec![7, 8, 9]);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_javascript_bundle_consumes_a_byte_stream() {
        // JS accepts a stream, reads Uint8Array chunks to EOF, reports the total.
        const BUNDLE: &str = r#"
            module.exports.default = async function () {
                const collector = await Process.receiveText();
                const s = Process.acceptStream();
                let total = 0, chunk;
                while ((chunk = await s.read()) !== null) { total += chunk.length; }
                Process.send(collector, "total:" + total);
            };
        "#;
        let rt = Runtime::new();
        let wr = WasmRuntime::new(rt.clone()).unwrap();
        let (tx, rx) = tokio::sync::oneshot::channel();
        let collector = rt.spawn(move |mut ctx| async move {
            let _ = tx.send(ctx.recv().await.message().unwrap());
        });
        let guest = wr.spawn_js(BUNDLE.as_bytes());
        rt.send(guest.pid(), collector.pid().raw().to_string().into_bytes());

        // Stream 3x "hello!" (18 bytes) into the JS consumer, then EOF.
        let (writer, reader) = rusm_otp::stream();
        rt.send_stream(guest.pid(), reader);
        for _ in 0..3 {
            writer.write(b"hello!".to_vec()).await.unwrap();
        }
        drop(writer); // end of stream

        let reply = String::from_utf8(rx.await.unwrap()).unwrap();
        assert_eq!(reply, "total:18", "JS read all streamed bytes to EOF");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_javascript_service_dispatches_exported_functions() {
        // A service component EXPORTS functions (no Process plumbing); the runner
        // runs the request→dispatch→reply loop. A Rust "client" drives it: send a
        // JSON request, get a JSON reply. Proves the service half of the typed RPC.
        const SERVICE: &str = r#"
            module.exports.add = (a, b) => a + b;
            module.exports.greet = async ({ name }) => "hi " + name;   // async handler too
        "#;
        let rt = Runtime::new();
        let wr = WasmRuntime::new(rt.clone()).unwrap();
        let (tx, rx) = tokio::sync::oneshot::channel();
        let collector = rt.spawn(move |mut ctx| async move {
            let _ = tx.send(ctx.recv().await.message().unwrap());
        });
        let svc = wr.spawn_js(SERVICE.as_bytes());
        // request: call `add(2, 3)`, asking `collector` to be answered with ref 1.
        let req = format!(
            r#"{{"op":"add","args":[2,3],"from":"{}","ref":1}}"#,
            collector.pid().raw()
        );
        rt.send(svc.pid(), req.into_bytes());

        let reply = String::from_utf8(rx.await.unwrap()).unwrap();
        assert!(
            reply.contains("\"ref\":1") && reply.contains("\"ok\":5"),
            "service should reply {{ref:1, ok:5}}, got {reply}"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_typescript_commander_calls_a_service_via_the_typed_client() {
        // The whole concealed-function-call story end to end, in JS: a `calc`
        // service exports `add`; the commander spawns it BY NAME and `await`s a
        // typed call — spawn + send + receive all hidden by the client proxy.
        const CALC: &str = r#"module.exports.add = (a, b) => a + b;"#;
        const COMMANDER: &str = r#"
            module.exports.default = async function () {
                const collector = await Process.receiveText();
                const calc = spawn("calc");          // spawn-from-guest by name
                const sum = await calc.add(2, 3);    // concealed call: send + await reply
                Process.send(collector, "sum=" + sum);
            };
        "#;
        let rt = Runtime::new();
        let wr = WasmRuntime::new(rt.clone()).unwrap();
        // Register the service by name so the commander can spawn it.
        wr.register_js_component("calc", CALC.as_bytes());

        let (tx, rx) = tokio::sync::oneshot::channel();
        let collector = rt.spawn(move |mut ctx| async move {
            let _ = tx.send(ctx.recv().await.message().unwrap());
        });
        // The commander needs the spawn capability (Trusted grants it).
        let commander = wr.spawn_js_with(
            COMMANDER.as_bytes(),
            CapabilityProfile::Trusted.capabilities(),
        );
        rt.send(
            commander.pid(),
            collector.pid().raw().to_string().into_bytes(),
        );

        let reply = String::from_utf8(rx.await.unwrap()).unwrap();
        assert_eq!(
            reply, "sum=5",
            "typed client called the service and got 2+3"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn component_stream_errors_are_reported_not_fatal() {
        // role 2: open to a dead pid, write/read bogus handles — each must return
        // none/false cleanly (flags 0b111), never trap.
        const PIPE: &[u8] = include_bytes!("../../tests/fixtures/stream_pipe.wasm");
        let rt = Runtime::new();
        let wr = WasmRuntime::new(rt.clone()).unwrap();
        let pre = wr
            .prepare_component(&wr.compile_component(PIPE).unwrap(), "run")
            .unwrap();

        let (tx, rx) = tokio::sync::oneshot::channel();
        let collector = rt.spawn(move |mut ctx| async move {
            let _ = tx.send(ctx.recv().await.message().unwrap());
        });
        let guest = wr.spawn_component(&pre);
        let mut msg = vec![2u8];
        msg.extend_from_slice(&collector.pid().raw().to_le_bytes());
        rt.send(guest.pid(), msg);

        let flags = rx.await.unwrap();
        assert_eq!(
            u32::from_le_bytes(flags[..4].try_into().unwrap()),
            0b111,
            "open-to-dead/write-bogus/read-bogus should each fail gracefully"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_memory_cap_crashes_a_component_that_grows_past_it() {
        let rt = Runtime::new();
        let wr = WasmRuntime::new(rt.clone()).unwrap();
        let pre = wr
            .prepare_component(&wr.compile_component(COMP_GROW).unwrap(), "run")
            .unwrap();
        // Cap at one page: the two-page growth is denied → the guest traps.
        let caps = Capabilities::nothing().max_memory(64 << 10);
        let guest = wr.spawn_component_with(&pre, caps);
        assert_eq!(exit_reason_of(&rt, &guest).await, ExitReason::Crashed);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_component_drives_the_whole_actor_abi() {
        // A real Rust component (built for wasm32-wasip2, checked in) that calls
        // every rusm:runtime actor op and reports which succeeded.
        const ECHO: &[u8] = include_bytes!("../../tests/fixtures/actor_echo.wasm");
        let rt = Runtime::new();
        let wr = WasmRuntime::new(rt.clone()).unwrap();
        let pre = wr
            .prepare_component(&wr.compile_component(ECHO).unwrap(), "run")
            .unwrap();

        // A victim the guest will kill via the ABI.
        let victim = rt.spawn(|_| std::future::pending::<()>());
        let victim_pid = victim.pid();

        // Controlling *other* processes (kill/list/info/is-alive) needs the
        // process-control capability — Trusted grants it.
        let guest = wr.spawn_component_with(&pre, CapabilityProfile::Trusted.capabilities());
        let guest_pid = guest.pid();

        // A native process pings the guest with [its pid][victim pid], then awaits
        // the guest's reply: [guest pid][flags].
        let (tx, rx) = tokio::sync::oneshot::channel();
        let ping_rt = rt.clone();
        rt.spawn(move |mut ctx| async move {
            let mut msg = ctx.pid().raw().to_le_bytes().to_vec();
            msg.extend_from_slice(&victim_pid.raw().to_le_bytes());
            ping_rt.send(guest_pid, msg);
            let _ = tx.send(ctx.recv().await.message().unwrap());
        });

        let reply = rx.await.unwrap();
        let reported_pid = u64::from_le_bytes(reply[0..8].try_into().unwrap());
        let flags = reply[8];
        assert_eq!(reported_pid, guest_pid.raw(), "own-pid via the ABI");
        // All seven ops (register, whereis, info, list, is-alive, kill, unregister)
        // succeeded from inside the component.
        assert_eq!(flags, 0b0111_1111, "every actor op should succeed");

        // Observable effects: the guest killed the victim and released the name.
        for _ in 0..200 {
            if !rt.is_alive(victim_pid) {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        assert!(
            !rt.is_alive(victim_pid),
            "the guest killed the victim via kill"
        );
        assert_eq!(rt.whereis("echo"), None, "the guest released its name");
        guest.join().await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_sandboxed_component_cannot_control_other_processes() {
        // The SAME echo fixture, but spawned Sandboxed (default-deny): it may
        // manage itself (register/whereis/info-self/list-self/unregister) but NOT
        // inspect or kill its neighbours — so is-alive(victim) and kill(victim) are
        // denied, and the victim survives.
        const ECHO: &[u8] = include_bytes!("../../tests/fixtures/actor_echo.wasm");
        let rt = Runtime::new();
        let wr = WasmRuntime::new(rt.clone()).unwrap();
        let pre = wr
            .prepare_component(&wr.compile_component(ECHO).unwrap(), "run")
            .unwrap();

        let victim = rt.spawn(|_| std::future::pending::<()>());
        let victim_pid = victim.pid();
        let guest = wr.spawn_component(&pre); // Sandboxed (no process-control)
        let guest_pid = guest.pid();

        let (tx, rx) = tokio::sync::oneshot::channel();
        let ping_rt = rt.clone();
        rt.spawn(move |mut ctx| async move {
            let mut msg = ctx.pid().raw().to_le_bytes().to_vec();
            msg.extend_from_slice(&victim_pid.raw().to_le_bytes());
            ping_rt.send(guest_pid, msg);
            let _ = tx.send(ctx.recv().await.message().unwrap());
        });

        let flags = rx.await.unwrap()[8];
        // Self-ops succeed (register, whereis, info-self, list-contains-self,
        // unregister = bits 0,1,2,3,6); control-of-others denied (is-alive bit 4,
        // kill bit 5 = 0).
        assert_eq!(
            flags, 0b0100_1111,
            "self-ops allowed, control-of-others denied"
        );
        assert!(
            rt.is_alive(victim_pid),
            "the sandboxed guest must NOT be able to kill a neighbour"
        );
        guest.kill();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_generous_memory_cap_lets_a_component_grow() {
        let rt = Runtime::new();
        let wr = WasmRuntime::new(rt.clone()).unwrap();
        let pre = wr
            .prepare_component(&wr.compile_component(COMP_GROW).unwrap(), "run")
            .unwrap();
        // Cap above the 192 KiB it grows to: growth succeeds → normal exit.
        let caps = Capabilities::nothing().max_memory(256 << 10);
        let guest = wr.spawn_component_with(&pre, caps);
        assert_eq!(exit_reason_of(&rt, &guest).await, ExitReason::Normal);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_component_runs_as_a_process_and_is_reaped() {
        let rt = Runtime::new();
        let wr = WasmRuntime::new(rt.clone()).unwrap();
        let component = wr.compile_component(COMP_RUN).unwrap();
        let pre = wr.prepare_component(&component, "run").unwrap();
        wr.spawn_component(&pre).join().await;
        assert_eq!(rt.finished(), 1);
        assert_eq!(rt.process_count(), 0);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_trapping_component_crashes_the_process() {
        let rt = Runtime::new();
        let wr = WasmRuntime::new(rt.clone()).unwrap();
        let component = wr.compile_component(COMP_TRAP).unwrap();

        let (tx, rx) = tokio::sync::oneshot::channel();
        let watcher = rt
            .spawn(move |mut ctx| async move {
                let _ = tx.send(ctx.recv().await);
            })
            .pid();
        let pre = wr.prepare_component(&component, "run").unwrap();
        let guest = wr.spawn_component(&pre);
        let reference = rt.monitor(watcher, guest.pid());
        let guest_pid = guest.pid();

        assert_eq!(
            rx.await.unwrap(),
            rusm_otp::Received::Down {
                reference,
                pid: guest_pid,
                reason: ExitReason::Crashed,
            }
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_spinning_component_yields_and_stays_killable() {
        let rt = Runtime::new();
        let wr = WasmRuntime::new(rt.clone()).unwrap();
        let component = wr.compile_component(COMP_SPIN).unwrap();

        // A bystander must still run alongside the spinning component — proof the
        // epoch preempts the component just as it does a core module.
        let (tx, rx) = tokio::sync::oneshot::channel();
        let bystander = rt.spawn(move |_| async move {
            let _ = tx.send(());
        });

        let pre = wr.prepare_component(&component, "run").unwrap();
        let spinner = wr.spawn_component(&pre);
        let spinner_pid = spinner.pid();
        rx.await.unwrap();
        bystander.join().await;

        assert!(rt.is_alive(spinner_pid));
        spinner.kill();
        spinner.join().await;
        assert!(!rt.is_alive(spinner_pid));
    }
}
