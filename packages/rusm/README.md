# rusm — the TypeScript guest API

The guest API for **RUSM** TypeScript components: the `Process` actor API, the
`spawn` typed-client, and the types. The Rust twin is the [`rusm-rs`](../../crates/rusm-rs)
crate.

```ts
import { Process, spawn } from "rusm";
import type * as Calc from "../calc/index";

const calc = spawn<typeof Calc>("calc");   // spawn a service by name, typed client
const sum = await calc.add(2, 3);          // a call
for await (const n of calc.countTo(3)) {}  // a stream
await calc.work((pct) => updateBar(pct));  // a callback (stays in the caller)
```

The js-runner injects `Process`/`spawn` as globals and polyfills the Web APIs
(`URL`, `TextEncoder`, `Headers`, `ReadableStream`, `console`); this package
re-exports them as a normal module and ships the types. `receive`/`Stream.read`
are async (`await`); pids are `bigint`.

## Use it

It isn't published yet, so depend on it by relative path in your app's
`package.json`:

```json
{ "dependencies": { "rusm": "file:../../packages/rusm" } }
```

`rusm build` runs `bun install` (if needed) and bundles each `components/<name>/index.ts`
with Bun → `wasm/<name>.js`. Add the standard `DOM` lib to your `tsconfig.json`
(`"lib": ["ES2022", "DOM"]`) so the polyfilled Web APIs are typed. See
`examples/ts-app`.
