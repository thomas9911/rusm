---
layout: home
hero:
  name: RUSM
  text: An Erlang-inspired WebAssembly runtime in Rust
  tagline: Isolated lightweight processes, fault tolerance, per-actor sandboxing, and secure clusters you can hook into live — on WebAssembly.
  actions:
    - theme: brand
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
    details: Processes are Tokio tasks multiplexed over a few threads, with epoch interruption for BEAM-like fairness even under tight loops.
  - title: Fault tolerance
    details: Traps become process exits; links and supervisors restart what crashes. Let it crash.
  - title: Hook into a running node
    details: Attach a live REPL or observer to a running node — like iex --remsh — over a secure control channel.
  - title: Secure distributed clusters
    details: Nodes connect over QUIC + TLS; processes spawn and message across the cluster with a global registry.
---
