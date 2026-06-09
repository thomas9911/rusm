# Concept — guests: Rust & TypeScript

A RUSM process body can be written in **Rust** (`rusm-rs`) or **TypeScript**
(`rusm-ts`). Both compile/bundle to a sandboxed Wasm process with the same actor API
and the same JSON wire — so a Rust client and a TypeScript service interoperate
transparently.

## Rust guests (`rusm-rs`)

Ergonomic `Pid` / `send` / `receive` / `spawn` / registry / `Stream`, plus a
`#[rusm_rs::service]` macro that generates the receive → dispatch → reply loop **and**
a typed `Client` with call / cast / streaming / callbacks. `rusm build` compiles each
`components/<name>/` with `cargo build --target wasm32-wasip2` — one toolchain, no
cargo-component, no jco.

## TypeScript guests (`rusm-ts`)

Import the `rusm` package: a **service** is just exported functions, a **worker** is
`export default`. The *concealed typed client* makes `await svc.method(...)` read
like a local call — with `for await` streaming and callback arguments — while
`spawn` / `send` / `receive` stay hidden. `rusm build` bundles each component with
Bun into a small `.js`.

## The shared runner — tiny TS components (vs jco)

A TypeScript component is **just its bundle** running on **one shared ~700 KB
rquickjs runner**: the JS engine is compiled once and shared by *every* TS process.
Contrast jco / ComponentizeJS, which bakes a multi-megabyte JS engine (StarlingMonkey)
into **every** component. Ship 50 TS components and RUSM ships the engine **once**,
not fifty times — far smaller and saner.

It's also the only option that keeps a JS guest **inside the Wasm sandbox**: rquickjs
compiles to `wasm32-wasip2`, so a TS guest gets the same memory isolation,
[capabilities](./permissions-and-sandboxing.md) and [preemption](./epoch-preemption.md)
as a Rust one. (A native engine like V8/`deno_core` can't run inside a component.)

## Bytecode precompile

`rusm build` precompiles each bundle to **version-locked QuickJS bytecode**
(`wasm/<name>.qjsbc`); the runner loads it straight into the VM, skipping the parser
on cold start. Full JS + npm is kept — the engine is shared, not embedded per
component.

> Phase 8 (the `rusm-rs` / `rusm-ts` SDKs + the rquickjs runner); bytecode precompile
> is a later optimization on the same shared runner.
