# RUSM vs Lunatic — comparison & efficiency playbook

> **Living document.** RUSM is built in phases; Lunatic is a complete (but dormant,
> v0.13.0 / May 2023) runtime. We keep this doc current as each RUSM phase closes a
> gap. It exists to (a) honestly track where RUSM stands, (b) capture the smart
> Lunatic techniques worth being *inspired* by (never copied), and (c) record where
> RUSM deliberately aims to be **faster, leaner, and more stable**.
>
> Source studied: `~/Sources/lunatic` (Wasmtime 8). File references are to that tree.

## How to read this

This is not apples-to-apples. RUSM is at **Phase 1 of 10**: the Wasm-free OTP
core spawns real lightweight processes (spawn-storm shows real data), atop the
Phase-0 observability foundation — but there is still **no Wasm execution** (the
Wasmtime backend is Phase 6). Lunatic is the full runtime. So most runtime rows
show RUSM as *planned (Phase N)*. The value is in the **efficiency playbook** below.

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
> allocation, copy-on-write memory init, and epoch preemption (see Phase 6 in the
> playbook below).

## Snapshot

| | RUSM (today) | Lunatic |
| --- | --- | --- |
| Status | Active, Phase 1 of 10 complete | Dormant since 2023 (v0.13.0) |
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

# Efficiency playbook — phase by phase

Same order as the roadmap and the matrix above. For each phase: the smart Lunatic
techniques to **borrow** (with file evidence in `~/Sources/lunatic`), why they
help, and where RUSM **beats** them. (Borrow ≠ copy — understand, then write our
own.)

## Phase 1 — Process & scheduler core ✅

| Borrow from Lunatic | Why it helps | RUSM plan |
| --- | --- | --- |
| Biased `tokio::select!` loop, signals before the body — `lunatic-process/src/lib.rs` | deterministic signal priority, no starvation, cancellation-safe | **Adopted** (`rusm-otp`) |
| Single `Signal` enum over one mpsc channel — `lunatic-process/src/lib.rs` | one channel for messages *and* control — uniform | **Adopted** |
| `HashMapId<T>` id→resource table — `crates/hash-map-id` | one uniform O(1) resource table everywhere | Adopt as the resource-table primitive |
| Unbounded signal mailbox (`UnboundedSender`) — `lunatic-process/src/lib.rs` | Erlang-style, but unbounded → flood/memory risk | **Beat (stability):** bounded + observable mailbox depth |

## Phase 2 — Mailboxes & message passing

| Borrow from Lunatic | Why it helps | RUSM plan |
| --- | --- | --- |
| `DataMessage{buffer, resources: Vec<Arc<Resource>>}` — resources moved by `Arc`, only bytes copied — `message.rs:68-103` | zero-copy handoff of sockets/modules | Adopt |
| Resources by index; `push_*`/`take_*` re-register on receive — `lunatic-messaging-api/src/lib.rs` | no serializing live handles; O(1) transfer | Adopt |
| Cancellation-safe selective receive by tag (waker + found-on-cancel re-queue) — `mailbox.rs:39-169` | safe in `select!`, no lost messages | Adopt (own code + tests) |

## Phase 3 — Links, supervision, fault tolerance

| Borrow from Lunatic | Why it helps | RUSM plan |
| --- | --- | --- |
| `Signal::{Link,Monitor,LinkDied}` + `die_when_link_dies` — `lunatic-process/src/lib.rs` | unified, configurable supervision | Adopt |
| trap → `ResultValue::Failed` → `LinkDied` propagation — `runtimes/wasmtime.rs` | a crash notifies linked peers | Adopt — task-panic isolation now, trap-level at Phase 7 |

## Phase 4 — Process management

| Borrow from Lunatic | Why it helps | RUSM plan |
| --- | --- | --- |
| Named registry `Arc<RwLock<HashMap>>` — `lunatic-registry-api` | async-safe name → pid | Adopt |
| Timers: `BinaryHeap` + `HashMapId` (O(log n) cancel) — `lunatic-timer-api` | cheap cancellation of many timers | Adopt, built on `tokio::time` |

## Phase 5 — Connectivity (TCP/TLS)

| Borrow from Lunatic | Why it helps | RUSM plan |
| --- | --- | --- |
| TCP owned read/write halves + per-conn timeouts in `HashMapId` — `lunatic-networking-api/src/lib.rs:71` | concurrent reader+writer without locking the stream | Adopt; **beat:** `RwLock` for the read-heavy timeout fields (Lunatic uses `Mutex`) |
| TLS via `tokio-rustls` + `webpki-roots`, Arc'd `ServerConfig` — `tls_tcp.rs:392` | config reuse amortizes setup | Adopt; **beat:** TLS session resumption per node |

## Phase 6 — Wasmtime backend  ← the biggest efficiency win

| Borrow from Lunatic | Why it helps | RUSM plan |
| --- | --- | --- |
| `InstancePre` (imports type-checked once) + Arc'd `Module` + `async_support` — `runtimes/wasmtime.rs:34,63,163` | fast instantiation, compile-once | Adopt |
| `InstanceAllocationStrategy::OnDemand` — `runtimes/wasmtime.rs:173` | fresh memory per instance → slower, heavier spawn | **Beat: pooling allocator** — pre-allocated slots → near-allocation-free spawns + bounded RSS |
| `static_memory_forced(true)` on v8 (no CoW) — `runtimes/wasmtime.rs:175` | static memories, but no copy-on-write | **Beat: copy-on-write init** (`memory_init_cow`) → spawn ≈ a few syscalls, shared pages, tiny incremental memory |
| Preemption via **fuel** (`consume_fuel`, `out_of_fuel_async_yield`) — `runtimes/wasmtime.rs:166,56` | works, but per-unit accounting overhead | **Beat: epoch interruption** — a periodic atomic check ≈ near-zero steady-state |

**Why this phase matters most:** pooling + CoW + epoch are exactly the levers for
"300k spawns/s with a small footprint." Lunatic (Wasmtime 8, OnDemand, fuel)
predates easy access to them — the dashboard is built to *prove* the delta.

## Phase 7 — WASI + per-process sandbox

| Borrow from Lunatic | Why it helps | RUSM plan |
| --- | --- | --- |
| WASI preopens (scoped fs), isolated per-process stdio — `lunatic-wasi-api` | fine-grained filesystem sandbox | Adopt; **beat:** finer memory/fuel limits per process |
| stdout capture as a `WasiFile` — `lunatic-stdout-capture` | isolated, inspectable output | Adopt — feeds the observer/attach |

## Phase 8 — Guest crate

| Borrow from Lunatic | Why it helps | RUSM plan |
| --- | --- | --- |
| `lunatic-rs` API shape — spawn / `Mailbox` / `AbstractProcess` / `Supervisor` (separate repo) | a familiar, ergonomic guest API | Mirror the shape in `rusm-rs` |

## Phase 9 — Distributed clusters + live attach

| Borrow from Lunatic | Why it helps | RUSM plan |
| --- | --- | --- |
| One persistent QUIC conn/node + N-stream pool (`NodeConnectionManager`) — `congestion/mod.rs:174` | 1 TLS handshake/node, multiplexed | Adopt |
| Deterministic stream routing `((src ^ dest) % streams)` — `congestion/mod.rs:244` | in-order per process-pair, no head-of-line block | Adopt |
| Message chunking + `try_send` backpressure — `congestion/mod.rs:69,99` | streams large messages; bounded memory | Adopt; **beat:** adaptive chunk size (RTT/bandwidth-aware) |
| Atomic message IDs + `DashMap` response cache + `AsyncCell` — `distributed/client.rs:85,93` | lock-free RPC hot path; sharded reads | Adopt |
| `rmp-serde` (MessagePack) wire format — `distributed/Cargo.toml` | compact + fast vs JSON | Adopt (or `bincode`/`postcard`; benchmark) |
| Cert-embedded authz (X.509 ext, OID 2.5.29.9) + 100 ms keep-alive — `quic/quin.rs:61,145` | auth at handshake, NAT traversal | Adopt |
| Node discovery via 5 s HTTP polling — `control/client.rs:287` | simple, but laggy for fast scaling | **Beat:** push-based discovery + pre-warmed connections |

## Phase 10 — Performance & hardening

Roll up the "beat" levers and prove them: pooling + CoW + epoch toward 300k+
spawns/sec, quinn 0.11+ with adaptive chunking, and the superiority scorecard
below — each as a measured number on the dashboard.

---

## Superiority scorecard (recap)

A one-glance summary of the **beat** items above — all **targets to validate on
the dashboard**, not yet achieved.

| Dimension | Lunatic | RUSM target | Lever (phase) |
| --- | --- | --- | --- |
| Spawn throughput | OnDemand alloc | **higher** | pooling + CoW (6) |
| Memory / process | fresh memory per instance | **lower** | CoW-shared pages, pooled slots (6) |
| Scheduling overhead | fuel accounting | **lower** | epoch interruption (6) |
| Engine | Wasmtime 8 (2023) | **current** | modern Cranelift/async/CoW (6) |
| Connectivity | quinn 0.10, fixed chunks, 5 s poll | **lower latency** | quinn 0.11+, adaptive chunks, push discovery (9) |
| Stability under flood | unbounded mailbox | **bounded + observable** | depth limits + live observer (1) |
| Observability | metrics facade | **first-class** | HdrHistogram + observer + REPL (Phase 0, shipped) |

## Pitfalls in Lunatic to avoid

Dated or suboptimal choices — don't inherit these:

- **Wasmtime 8** and **quinn 0.10** — well behind current; upgrading buys CoW, pooling, flow-control.
- **OnDemand allocation** + **fuel** — leave spawn/memory/scheduling wins on the table.
- **`Mutex` on read-heavy timeout fields** — should be `RwLock`.
- **Fixed 1 KiB chunk size** and **5 s HTTP polling** discovery — should adapt / push.
- **No TLS session resumption** — full handshake on reconnect.
- **Unbounded signal mailbox** — memory-growth risk under flood.

# Maintaining this document

Update at the end of each phase: flip the matrix cells RUSM now implements, record
the **measured** spawn/sec, memory/process, and latency vs the targets above
(screenshot or numbers from the dashboard), and note any technique we adopted,
modernized, or rejected — with the reason.
