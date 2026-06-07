# host_ts_component — a TypeScript guest as a RUSM process

Shows **rusm-ts**: a TypeScript guest runs as a first-class, sandboxed RUSM
process, message-passing with the rest of the node — no per-component Wasm build.

```sh
cargo run -p rusm-bench --example host_ts_component
```

## What it shows

A TS component is **plain TypeScript** bundled by Bun and run on the shared
rquickjs **js-runner**. `WasmRuntime::spawn_js(bundle)` hands the bundle to a
fresh, isolated, default-deny process that gets the `Process` actor API (and the
Web API polyfills the runner installs). The worker here receives a reply-to pid,
labels itself `ts-worker`, and answers — exactly like an Erlang process.

## How it maps to a real app

In an app you don't embed the bundle; you write source and let RUSM build it:

```
my-app/
├── rusm.toml                 # [[components]] name = "worker", capability = "sandboxed"
└── components/worker/index.ts
```

```ts
/// <reference path="./rusm.d.ts" />
const replyTo = Process.receiveText();
Process.setLabel("ts-worker");
Process.send(replyTo, `pong from ${Process.self()}`);
```

`rusm build` detects the `index.ts`, runs `bun build --format=iife` → `wasm/worker.js`,
and `rusm run` loads `.js` artifacts on the js-runner under the declared capability
profile. (A Rust component builds to `wasm/<name>.wasm` instead — same manifest,
same loader.) The `Process` global is typed by `rusm.d.ts` (shipped with the runner).
