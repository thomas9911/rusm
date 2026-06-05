# RUSM vs Lunatic — comparison & efficiency playbook

> **Living document.** RUSM is built in phases; Lunatic is a complete (but dormant,
> v0.13.0 / May 2023) runtime. We keep this doc current as each RUSM phase closes a
> gap. It exists to (a) honestly track where RUSM stands, (b) capture the smart
> Lunatic techniques worth being *inspired* by (never copied), and (c) record where
> RUSM deliberately aims to be **faster, leaner, and more stable**.
>
> Source studied: `~/Sources/lunatic` (Wasmtime 8). File references are to that tree.

## How to read this

This is not apples-to-apples. RUSM today is **Phase 0** — an observability
foundation (metrics, live observer, benchmark dashboard) on synthetic data, with
**no Wasm execution yet**. Lunatic is the full runtime. So most runtime rows show
RUSM as *planned (Phase N)*. The value is in the **efficiency playbook** below.

> ### Does RUSM handle lightweight processes as efficiently as Lunatic — today?
>
> **No. Today RUSM does not run lightweight processes at all.** There is no
> Wasmtime, no instance, no spawn, no scheduler for guests. The processes shown in
> the dashboard are **synthetic placeholders** that exercise the observability/UI
> before the engine exists. Lunatic genuinely runs them; RUSM does not yet.
>
> The "superiority" targets below are **intent, to be proven on the dashboard** —
> not achievements. The actor model is built first (Phases 1–5, native bodies);
> the lightweight-process *efficiency* race begins when Wasmtime becomes the
> backend at **Phase 6**, where the levers meant to beat Lunatic kick in: pooling
> allocation, copy-on-write memory init, and epoch preemption (see §2 and §1).

## Snapshot

| | RUSM (today) | Lunatic |
| --- | --- | --- |
| Status | Active, Phase 0 of 10 | Dormant since 2023 (v0.13.0) |
| Rust LOC | ~2,560 (4 crates) + 790 TS | ~15,150 (20 crates) |
| Tests | 87 Rust + 18 TS, ~99.5% cov | ~26 test annotations |
| Wasmtime | none yet (target: modern) | v8 (2023) |
| Guest target | planned `wasm32-wasip1` | `wasm32-wasi` (preview1) |
| License | MIT | MIT + Apache-2.0 |

## Capability matrix

✅ implemented · ⚠️ partial/synthetic · ❌ not yet · 🅛 Lunatic-only · 🅡 RUSM-only

This mirrors the [roadmap](./02-roadmap.md) **phase-for-phase** — one row per
phase, same themes, same order.

| Phase | Capability | RUSM | Lunatic |
| --- | --- | :---: | --- |
| 1 ✅ | Process & scheduler core | ✅ done (`rusm-otp`) | ✅ `WasmProcess` |
| 2 | Mailboxes & message passing | ❌ | ✅ selective-receive |
| 3 | Links, monitors, supervision, fault tolerance | ❌ | ✅ `Signal` enum |
| 4 | Process management (registry, timers, lifecycle) | ❌ | ✅ |
| 5 | Connectivity — TCP/TLS | ⚠️ WS (dashboard) | ✅ TCP/UDP/DNS/TLS |
| 6 | Wasmtime backend (instance-per-process, preemption) | ❌ → epoch | ✅ (fuel) |
| 7 | WASI + per-process sandbox | ❌ | ✅ |
| 8 | Guest crate | ❌ → `rusm-rs` | 🅛 `lunatic-rs` (separate repo) |
| 9 | Distributed clusters + live attach | ❌ | ✅ (Axum + Submillisecond) |
| 10 | Performance (pooling + CoW + epoch) | ❌ | ⚠️ OnDemand + fuel |
| — | SQLite host API | ❌ | 🅛 |

Already shipped in Phase 0 — where RUSM already leads Lunatic:

| Capability | RUSM | Lunatic |
| --- | :---: | :---: |
| HdrHistogram latency metrics | ✅✅ | ⚠️ passthrough facade |
| Live observer + REPL attach | ✅✅ | ❌ |
| Web dashboard | ✅✅ | ❌ |
| Enforced ≥98% coverage + docs site | ✅✅ | ❌ |

---

# Efficiency playbook — borrow vs beat

For each dimension: **what Lunatic does** (with evidence), the **efficiency note**,
and **RUSM's plan** — adopt the good ideas, modernize the dated ones.

## 1. Concurrency & scheduling

| Lunatic technique | Evidence | Efficiency note | RUSM plan |
| --- | --- | --- | --- |
| One `tokio::select!` loop per process, **`biased`** — signals checked before advancing the Wasm future | `lunatic-process/src/lib.rs` (exec loop) | Deterministic signal priority; no starvation; cancellation-safe | **Adopt.** Same biased signal-vs-run loop. |
| Process interaction only via a single **`Signal`** enum (Message/Link/Monitor/Kill/…) over an mpsc `signal_mailbox` | `lunatic-process/src/lib.rs` | One channel handles messages *and* control — simple, uniform | **Adopt** the unified signal channel. |
| Preemption via **Wasmtime fuel** (`consume_fuel`, `out_of_fuel_async_yield`, 100k-instruction units) | `runtimes/wasmtime.rs:166,56` | Works, but fuel **injects per-unit accounting into compiled code** — steady-state overhead | **Beat: use epoch interruption.** A periodically-bumped atomic checked at backedges ≈ near-zero overhead. Validate fuel-vs-epoch on the dashboard (Phase 6). |
| Unbounded signal mailbox (`UnboundedSender<Signal>`) | `lunatic-process/src/lib.rs` | Erlang-style, but unbounded → memory-growth/stability risk under flood | **Beat (stability): observable + optionally bounded** mailboxes, with depth surfaced in the observer (already modeled). |

## 2. Memory footprint & spawn cost  ← biggest superiority opportunity

| Lunatic technique | Evidence | Efficiency note | RUSM plan |
| --- | --- | --- | --- |
| **`InstancePre`** — imports type-checked once at compile, reused for every spawn | `runtimes/wasmtime.rs:34,63` | Avoids re-resolving the linker per spawn → fast instantiation | **Adopt** (essential for spawn throughput). |
| Compiled `Module` shared via `Arc` across all instances | `runtimes/wasmtime.rs:72-94` | Compile once, instantiate many | **Adopt.** |
| **`InstanceAllocationStrategy::OnDemand`** (explicitly chosen: "can't predict how many processes") | `runtimes/wasmtime.rs:173` | Allocates each instance's memory fresh → slower spawn, higher per-process cost | **Beat: pooling allocator.** Modern Wasmtime `PoolingAllocationConfig` pre-allocates instance/memory slots → near-allocation-free spawns + bounded RSS. Size a generous pool; fall back to on-demand past it. |
| `static_memory_forced(true)` on Wasmtime 8 | `runtimes/wasmtime.rs:175` | Static memories + guard pages; no CoW guarantees on v8 | **Beat: copy-on-write memory init** (`memory_init_cow`, default-on in modern Wasmtime). New instances map the module's initial image CoW → spawn cost ≈ a few syscalls, shared read-only pages → tiny incremental memory. |
| `cranelift_opt_level(SpeedAndSize)` | `runtimes/wasmtime.rs:171` | Good default | **Adopt.** |

**Why this is the win:** pooling + CoW is exactly what makes "300k spawns/s with a
small footprint" realistic. Lunatic predates easy access to both. The benchmark
dashboard is built to *prove* the delta (spawn storm scenario, memory/process).

## 3. Messaging

| Lunatic technique | Evidence | Efficiency note | RUSM plan |
| --- | --- | --- | --- |
| **Resources moved by `Arc`, only the byte buffer is copied** (`DataMessage{buffer, resources: Vec<Option<Arc<Resource>>>}`) | `lunatic-process/src/message.rs:68-103` | Zero-copy handoff of sockets/modules between processes | **Adopt.** |
| Resources referenced by **index in the buffer**; `push_*`/`take_*` re-register on receive | `lunatic-messaging-api/src/lib.rs` | No serialization of live handles; O(1) transfer | **Adopt.** |
| `DataMessage` impls `Read`/`Write`; guest fills the buffer in place | `message.rs:64-95` | Avoids intermediate copies | **Adopt.** |
| **Cancellation-safe selective receive** by tag (waker + "found-on-cancel re-queue") | `lunatic-process/src/mailbox.rs:39-169` | Safe inside `tokio::select!`; selective receive without losing messages | **Adopt** — the mailbox design is excellent; mirror its semantics, with our own code + tests. |

## 4. Connectivity (networking + distributed)

| Lunatic technique | Evidence | Efficiency note | RUSM plan |
| --- | --- | --- | --- |
| **One persistent QUIC connection per node** + pool of N unidirectional streams (`NodeConnectionManager`) | `lunatic-distributed/src/congestion/mod.rs:174` | 1 TLS handshake/node, not per message | **Adopt.** |
| **Deterministic stream routing** `((src ^ dest) % streams)` | `congestion/mod.rs:244` | In-order per process-pair, multiplexed across streams → no head-of-line block | **Adopt.** |
| **Message chunking** (1 KiB) + `try_send` backpressure | `congestion/mod.rs:69,99` | Streams large messages; bounded memory under slow receivers | **Adopt; beat: adaptive chunk size** (RTT/bandwidth-aware) instead of fixed 1 KiB. |
| Lock-free **atomic message IDs** + `DashMap` response cache + `AsyncCell` | `distributed/client.rs:85,93` | No mutex on the RPC hot path; sharded reads | **Adopt.** |
| **MessagePack** (`rmp-serde`) wire format | `distributed/Cargo.toml` | Compact + fast vs JSON | **Adopt** (or `bincode`/`postcard`; benchmark). |
| TCP **owned read/write halves** + per-half locks + per-connection timeouts, stored in `HashMapId` | `lunatic-networking-api/src/lib.rs:71` | Concurrent reader+writer without locking the stream | **Adopt; beat: `RwLock` for the read-heavy timeout fields** (Lunatic uses `Mutex`). |
| TLS via `tokio-rustls` + `webpki-roots`, `Arc`-shared `ServerConfig` | `tls_tcp.rs:392` | Config reuse amortizes setup | **Adopt; beat: TLS session resumption** caching per node. |
| QUIC keep-alive 100 ms; **authz embedded in X.509 cert extension** (OID 2.5.29.9) | `quic/quin.rs:145,61` | NAT traversal; auth at handshake, not RPC time | **Adopt** the cert-carried permissions idea. |
| Node discovery via **5 s HTTP polling** refresh task | `control/client.rs:287` | Simple, but laggy for fast scaling | **Beat: push-based** (stream/gossip) discovery; pre-warm connections on join. |

## 5. Management & resource model

| Lunatic technique | Evidence | Efficiency note | RUSM plan |
| --- | --- | --- | --- |
| **`HashMapId<T>`** — `HashMap<u64, T>` + incrementing seed for every resource (sockets, timers, errors, …) | `crates/hash-map-id/src/lib.rs` | One simple, uniform id→resource table everywhere | **Adopt** as the core resource-table primitive. |
| `DashMap` for concurrent cluster/connection state | `distributed/client.rs:87-93` | Sharded locking scales with cores | **Adopt** where contention is real. |
| Named **registry** via `Arc<RwLock<HashMap>>`; **timers** via `BinaryHeap` + `HashMapId` (O(log n) cancel) | `lunatic-registry-api`, `lunatic-timer-api` | Async-safe, cheap cancellation | **Adopt.** |
| Per-process **stdout capture** as a `WasiFile` | `lunatic-stdout-capture` | Isolated, inspectable output | **Adopt** (feeds naturally into our observer/attach). |
| Live introspection | — | ❌ none in Lunatic | **RUSM already wins:** live observer + `rusm attach` REPL + dashboard. Keep extending. |

---

# Where RUSM intends to be *superior*

> Intent, to be **validated on the benchmark dashboard** — not yet achieved.

| Dimension | Lunatic | RUSM target | Lever |
| --- | --- | --- | --- |
| Spawn throughput | OnDemand alloc | **higher** | pooling allocator + CoW init |
| Memory / process | fresh memory per instance | **lower** | CoW-shared pages, pooled slots |
| Scheduling overhead | fuel accounting | **lower** | epoch interruption |
| Engine | Wasmtime 8 (2023) | **current** | modern Cranelift/async/CoW |
| Connectivity | quinn 0.10, fixed 1 KiB chunks, 5 s poll | **lower latency** | quinn 0.11+, adaptive chunks, push discovery, session resumption |
| Stability under flood | unbounded mailbox | **bounded + observable** | depth limits + live observer |
| Observability | metrics facade | **first-class** | HdrHistogram + live observer + REPL (already shipped) |

# Dated / suboptimal in Lunatic (avoid or improve)

- **Wasmtime 8** and **quinn 0.10** — both well behind current; upgrade buys CoW, pooling, flow-control.
- **OnDemand allocation** — leaves spawn/memory wins on the table.
- **Fuel** preemption — per-unit cost vs epoch.
- **`Mutex` on read-heavy timeout fields** — should be `RwLock`.
- **Fixed 1 KiB chunk size** — should adapt to RTT/bandwidth.
- **5 s HTTP polling** discovery — laggy; prefer push.
- **No TLS session resumption** — full handshake on reconnect.
- **Unbounded signal mailbox** — memory-growth risk under flood.

# Borrow-from-Lunatic, per RUSM phase

| Phase | Borrow (inspired by) | Modernize / beat |
| --- | --- | --- |
| 1 Process core | biased `select!` signal loop, `Signal` channel, `HashMapId` | native body first (wasm-ready) |
| 2 Messaging | `DataMessage` Arc-resources, selective-receive mailbox | — |
| 3 Supervision | `Signal::{Link,Monitor,LinkDied}`, `die_when_link_dies` | task-panic isolation now, trap-level at Phase 7 |
| 4 Management | registry `RwLock`, timer `BinaryHeap` + `HashMapId` | — |
| 5 Connectivity | owned half split, `HashMapId` resources, TLS root setup | `RwLock` timeouts, TLS session resumption |
| 6 Wasm backend | `InstancePre`, Arc'd `Module`, `async_support` | **pooling + CoW**; **epoch** not fuel |
| 7 Sandbox | WASI preopens, stdout capture | finer per-process limits |
| 8 Guest crate | `lunatic-rs` API shape | — |
| 9 Distributed | QUIC 1-conn/node + stream pool, chunking, cert-authz, `DashMap` | quinn 0.11+, adaptive chunks, push discovery |

# Maintaining this document

Update at the end of each phase: flip the matrix cells RUSM now implements, record
the **measured** spawn/sec, memory/process, and latency vs the targets above
(screenshot or numbers from the dashboard), and note any technique we adopted,
modernized, or rejected — with the reason.
