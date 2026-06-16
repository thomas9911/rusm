# rusm-jsc

> Build-time QuickJS bytecode compiler for RUSM's TypeScript guests — precompile a JS bundle so the runner skips parsing at spawn.

`rusm-jsc` is a build-time helper used by the `rusm` CLI when building TS components: it
compiles a Bun-bundled `.js` to **QuickJS bytecode** (the `QJSB` format the
[`rusm-wasm`](https://crates.io/crates/rusm-wasm) js-runner loads directly), wrapping it in
the CommonJS scope the runner expects. Shipping bytecode means each spawned guest skips the
JS parse step and loads the module straight away.

It is **version-locked to the js-runner's embedded rquickjs** — the two must agree on the
bytecode format — so it lives in the RUSM workspace and is built/used together with the host.

Part of [RUSM](https://github.com/archan937/rusm). See the
[repo README](https://github.com/archan937/rusm#readme).
