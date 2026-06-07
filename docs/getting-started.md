# Getting started

This page takes you from a clean machine to running real RUSM processes — first
the pure-Rust actor core, then hosting WebAssembly, then writing your own
components. Every command and snippet here is real and current; anything not yet
built is marked **Roadmap**.

## Install

RUSM isn't published to crates.io yet, so you **clone the repo**:

```sh
git clone https://github.com/archan937/rusm
cd rusm
```

Prerequisites:

- **Rust** 1.94+ via [`rustup`](https://rustup.rs). To build guest
  components/modules, add the Wasm targets:
  ```sh
  rustup target add wasm32-wasip2 wasm32-wasip1
  ```
  (`wasm32-wasip2` for components, `wasm32-wasip1` for core modules.)
- **Bun** 1.3+ ([bun.sh](https://bun.sh)) — the dashboard and docs site use Bun,
  never Node.js. Only needed for those.

Verify the build:

```sh
cargo test          # the whole workspace
```

## Quick start

The fastest way to *see* RUSM is the live dashboard:

```sh
make dashboard      # builds + starts a node, then the dashboard — open the printed URL
```

Pick a scenario (e.g. **spawn storm**, **stream pipe**), hit **Run**, and watch
real throughput, latency, and the live observer. Everything is driven by the real
runtime.

### Make commands

| Command | What it does |
| --- | --- |
| `make dashboard` | Build + start a node, then the dashboard (the headline demo). |
| `make node` | Start a node on `ws://127.0.0.1:4000` (release). |
| `make ui` | Start only the dashboard (expects a node already running). |
| `make attach` | Attach a live REPL to the local node (like `iex --remsh`). |
| `make run SCENARIO=… SECONDS=…` | Run a benchmark scenario in the terminal. |
| `make example EX=…` | Run a bundled example (`host_components`, …). |
| `make test` / `make cov` | All Rust + dashboard tests / coverage. |
| `make fmt` / `make fmt-check` | Format / check (Rust + dashboard). |
| `make docs` / `make docs-build` | Live-preview / build this docs site. |

Run `make` with no target for the full list.

## Two ways to use RUSM

1. **As a library you embed** — depend on the `rusm-otp` core (and optionally the
   `rusm-wasm` backend) and drive it from your own Rust binary. Best when RUSM is a
   piece of a larger app.
2. **As an app you run** — declare components in `rusm.toml` and let the `rusm` CLI
   build, load, and supervise them. Best for a RUSM-first project.

The next sections cover both.

## 1. Without a Wasm runtime — the OTP core

RUSM's heart is a **Wasm-free** Erlang/OTP actor library, `rusm-otp`. You can use
it on its own — real lightweight processes, message passing, links, monitors,
supervision, a registry, and timers — with **no WebAssembly at all**. This is the
model RUSM is built on, and it stands alone (the dependency graph guarantees it).

```rust
use rusm_otp::{ExitReason, Received, Runtime};

#[tokio::main]
async fn main() {
    let rt = Runtime::new();

    // A worker: receive one message, then exit.
    let worker = rt.spawn(|mut ctx| async move {
        if let Received::Message(bytes) = ctx.recv().await {
            println!("worker got {} bytes", bytes.len());
        }
    });

    // Supervise it: monitor delivers a `Down` with the exit reason.
    let (tx, rx) = tokio::sync::oneshot::channel();
    let watcher = rt
        .spawn(move |mut ctx| async move {
            if let Received::Down { reason, .. } = ctx.recv().await {
                let _ = tx.send(reason);
            }
        })
        .pid();
    rt.monitor(watcher, worker.pid());

    rt.send(worker.pid(), b"hello".to_vec()); // messages are bytes (Vec<u8>)
    assert_eq!(rx.await.unwrap(), ExitReason::Normal);
}
```

You also get `spawn_link` (crash propagation), `trap_exit`, a named **registry**
(`register`/`whereis`), **timers** (`send_after`/`cancel`), graceful `shutdown`,
and **TCP** (`listen`/`connect`, one process per connection) — all in `rusm-otp`,
all without touching Wasm. See [links & supervision](./concepts/links-and-supervision.md).

## 2. With an already-compiled `.wasm` — embedding

Add the `rusm-wasm` backend and host a prebuilt component as a process. A
`WasmRuntime` wraps an `rusm-otp` `Runtime`; **construct it inside a Tokio runtime**
(it starts the epoch ticker).

```rust
use rusm_otp::Runtime;
use rusm_wasm::{Capabilities, WasmRuntime};

#[tokio::main]
async fn main() {
    let rt = Runtime::new();
    let wasm = WasmRuntime::new(rt.clone()).unwrap();

    // compile once → prepare once (imports + entry export resolved) → spawn many.
    let bytes = std::fs::read("wasm/worker.wasm").unwrap();
    let prepared = wasm
        .prepare_component(&wasm.compile_component(&bytes).unwrap(), "run")
        .unwrap();

    // Default-deny Sandboxed profile…
    wasm.spawn_component(&prepared).join().await;

    // …or grant capabilities explicitly (here: an 8 MiB heap cap):
    let caps = Capabilities::nothing().max_memory(8 << 20);
    wasm.spawn_component_with(&prepared, caps).join().await;
}
```

A trap (or a denied capability the guest turns into a trap) exits the process
`Crashed`, so links and supervisors react exactly as for a native process. The
runnable [`host_components`](https://github.com/archan937/rusm/tree/main/examples/host_components)
example (`make example EX=host_components`) shows this end to end, including a
memory-cap denial.

> **Core modules.** A `wasm32-wasip1` core module works the same way with
> `compile` / `prepare(module, "run")` / `spawn` (see the wasip1 bridge).

## 3. With an already-compiled `.wasm` — the app model

For a RUSM-first project, declare components in `rusm.toml` and let the CLI load
and supervise them from `./wasm/`:

```toml
# rusm.toml
listen = "127.0.0.1:4000"
profile = "balanced"

[[components]]
name = "worker"          # loaded from ./wasm/worker.wasm
capability = "sandboxed" # sandboxed | network-client | trusted
restart = true           # supervise: restart if it exits
```

```sh
rusm run          # load every [[components]] from ./wasm/, spawn under its profile
```

## 4. A Rust WASM component (source only)

Write the source, let RUSM build it. A component lives under `components/<name>/`:

```
my-app/
├── rusm.toml
├── components/
│   └── worker/
│       ├── Cargo.toml      # crate-type = ["cdylib"], wit-bindgen
│       ├── wit/            # the rusm:runtime world (vendored from crates/rusm-wasm/wit)
│       └── src/lib.rs
└── wasm/                   # rusm build writes worker.wasm here
```

`src/lib.rs` binds the `rusm:runtime` actor world with `wit-bindgen` and exports
`run`:

```rust
wit_bindgen::generate!({ world: "process", path: "wit" });

use rusm::runtime::actor;

struct Component;

impl Guest for Component {
    fn run() {
        actor::set_label("worker");
        let msg = actor::receive();              // block for a message (bytes)
        actor::send(actor::own_pid(), &msg);     // echo to self, etc.
    }
}

export!(Component);
```

Build and run the whole app:

```sh
rusm build        # cargo build --target wasm32-wasip2 per components/* → ./wasm/
rusm run          # spawn them per rusm.toml
rusm dev          # build, then run (one step)
```

One toolchain, no jco, no cargo-component — `cargo build --target wasm32-wasip2`
componentizes directly.

> **Roadmap.** `rusm dev` builds then runs today; **automatic rebuild + reload on
> file change** (true watch mode) is a follow-on. For now, re-run `rusm dev` after
> editing a component.

## 5. A TS / JS WASM component (source only)

TypeScript guests are **first-class, sandboxed RUSM processes** — the
`genius-wasmcloud` model, **no jco**. RUSM ships one **js-runner** component: it
embeds [rquickjs](https://github.com/DelSkayn/rquickjs) (QuickJS, compiled to
`wasm32-wasip2`, ~658 KB) and runs your JS, exposing a `Process` global bridged to
the actor world. You write TS, **Bun** bundles it to one `.js`, and the runner
executes it inside the same sandbox (capabilities, memory cap, epoch preemption)
as a Rust component:

```js
// worker.ts → bundled by Bun → run by the js-runner
const replyTo = Process.receive();          // block for a message
Process.setLabel("ts-worker");
Process.send(replyTo, "pong from " + Process.self());
```

Blocking JS "just works": `Process.receive()` suspends the whole instance's fiber
until a message arrives — no async/await needed, exactly like a Rust guest. The
full `Process` API (`self`/`list`/`send`/`receive`/`register`/`whereis`/`isAlive`/
`kill`/`setLabel`) is bridged and proven by a test that drives it entirely from JS.

> **Being formalised (Phase 8 polish).** The runtime is real; the ergonomic
> packaging is in progress: `rusm build` auto-running **Bun** on TS components,
> the `rusm.toml` app-model wiring a TS source to the runner, TS type
> definitions, binary (non-string) message marshalling, and exposing the
> [stream ops](#streaming-from-a-component) to JS. Also the **`rusm-rs`** Rust
> guest crate (ergonomic spawn/Mailbox/Supervisor) — the other half of Phase 8.

## Process management from inside a component (Rust)

A component imports the `rusm:runtime/actor` interface and calls the Erlang
`Process` API directly — the same operations the host has:

```rust
use rusm::runtime::actor;

let me = actor::own_pid();                 // self()
actor::register("worker");                 // name yourself in the registry
let who = actor::whereis("worker");        // look a name up → Option<pid>
let all = actor::list_processes();         // every live pid (find all)
let info = actor::info(me);                // Option<process-info>: links, label, mailbox depth…
let alive = actor::is_alive(some_pid);
actor::send(some_pid, &bytes);             // message-pass (bytes)
let incoming = actor::receive();           // block for the next message
actor::kill(some_pid);                     // terminate another process
actor::unregister("worker");
actor::set_label("worker#1");              // a human label for the observer
```

The runnable proof is the `actor-echo` test fixture, which drives **every** op
from inside a real component.

> **Composition is message passing, not spawn-from-guest.** A component doesn't
> spawn other processes from within — by design, the way Erlang components compose
> is: the host (or `rusm.toml`) spawns instances, and they find each other with
> `register`/`whereis` and talk with `send`/`receive`. A request/reply "callback"
> between two components is just a message and a reply. See
> [components & the actor world](./concepts/components-and-the-actor-world.md).

> **From TS/JS.** The same operations are bridged to the `Process` global in the
> [js-runner](#_5-a-ts-js-wasm-component-source-only): `Process.self()`,
> `Process.list()`, `Process.send(to, msg)`, `Process.receive()`,
> `Process.register/whereis/isAlive/kill/setLabel`.

## Streaming (from a component)

Cross-process **byte streams** are Tokio-backpressured and ride the mailbox as
`Received::Stream` — see [byte streams](./concepts/byte-streams.md). A component
opens a stream to another process, writes chunks (the write parks under
back-pressure when the reader is slow), and closes it; the other side accepts and
reads to end-of-stream:

```rust
use rusm::runtime::actor;

// Producer: open a stream to `peer`, write chunks, then close.
if let Some(id) = actor::stream_open(peer) {
    actor::stream_write(id, b"hello!");   // false if the reader is gone
    actor::stream_close(id);              // signals end-of-stream
}

// Consumer: accept the next incoming stream, read to EOF.
let id = actor::stream_accept();          // blocks until a stream arrives
while let Some(chunk) = actor::stream_read(id) {
    // …handle chunk (Vec<u8>)…           // None == end-of-stream
}
```

The same ops are available to **wasip1 core modules** through the raw `rusm::*`
ABI, and the **stream-pipe** benchmark drives the underlying `StreamHandle` at
multiple GB/s.

> **Roadmap.** A native p3-typed `stream<u8>` in the WIT signature (instead of the
> handle-based ops above) is a future ergonomic layer; the handle-based API is the
> real, working one today.

## Capabilities & sandboxing

Every process is default-deny. Named profiles set the baseline; the `Capabilities`
builder overrides per spawn:

```rust
use rusm_wasm::{Capabilities, CapabilityProfile};

CapabilityProfile::Sandboxed.capabilities();          // CPU + bounded heap only
Capabilities::nothing()                               // start from nothing…
    .max_memory(16 << 20)                             // …a 16 MiB ceiling
    .allow_network(true)                              // …outbound sockets
    .preopen("/srv/data", "/data", /* read_only */ true) // …a mounted dir
    .env("LOG", "info");                              // …an env var
```

Grants map onto standard WASI plus a `StoreLimiter` memory cap. A breach traps
*only that process*. See [permissions & sandboxing](./concepts/permissions-and-sandboxing.md).

## Observe a running node

```sh
make node                 # start a node
make attach               # a live REPL: run <scenario>, detail off, stop, quit
make ui                   # or the visual dashboard against the same node
```

The dashboard's **Observer** shows the live process count and per-tick activity;
each scenario panel also unfolds its real engine source so you can see exactly how
it's built.
