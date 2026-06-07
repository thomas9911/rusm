---
layout: home
hero:
  name: RUSM
  text: An Erlang-inspired WebAssembly runtime in Rust
  tagline: Isolated lightweight processes, fault tolerance, per-actor sandboxing, and secure clusters you can hook into live — on WebAssembly.
  actions:
    - theme: brand
      text: Get started
      link: /getting-started
    - theme: alt
      text: Why RUSM
      link: /00-vision
    - theme: alt
      text: Architecture
      link: /01-architecture
    - theme: alt
      text: Roadmap
      link: /02-roadmap
features:
  - title: Processes as Wasm instances
    details: Each process is an isolated Wasm instance — own stack, heap, syscalls, and permissions. One crash can never corrupt another.
  - title: Write blocking code, get async
    details: Wasmtime fibers suspend a guest's “blocking” call while the host awaits. Guests never write async; millions can wait for almost nothing.
  - title: Massive, fair concurrency
    details: Processes are Tokio tasks multiplexed over a few threads, with epoch interruption for BEAM-like fairness even under tight loops. ~2.4M spawns/sec.
  - title: Fault tolerance
    details: Traps become process exits; links and supervisors restart what crashes. Let it crash.
  - title: The OTP core stands alone
    details: The Erlang/OTP model — processes, mailboxes, supervision, registry, timers, TCP — is pure Rust with zero Wasmtime dependency. Use it with or without Wasm.
  - title: Default-deny capabilities
    details: Every process gets nothing unless granted — fs, network, env, and a memory ceiling, per process. Run untrusted code safely.
  - title: Components, any language
    details: Host real WASI components (p2/p3) that call the Erlang Process API through a WIT actor world — or wasip1 core modules through a raw ABI. Standards-first; the actor world is opt-in.
  - title: Hook into a running node
    details: Attach a live REPL or observer to a running node — like iex --remsh. Eight live benchmarks; nothing synthetic.
  - title: Secure distributed clusters
    details: Nodes connect over QUIC + TLS; processes spawn and message across the cluster with a global registry. (Phase 9.)
---
