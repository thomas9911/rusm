// rusm.d.ts — ambient types for a RUSM TypeScript component.
//
// A TS component is plain TypeScript, Bun-bundled (`rusm build` → `bun build
// --format=cjs`) and run on the shared rquickjs js-runner. Two shapes:
//
//   • service — `export` functions; RUSM runs the receive→dispatch→reply loop:
//         export function add(a: number, b: number) { return a + b; }
//
//   • worker  — `export default async function () { … }`; RUSM runs it once:
//         export default async function () { const m = await Process.receive(); … }
//
// Reach a service from another component with the typed client:
//         import type * as Calc from "../calc/index";
//         const calc = spawn<typeof Calc>("calc");
//         const sum = await calc.add(2, 3);          // spawn + send + receive, hidden
//
// Reference this file from your entry: /// <reference path="./rusm.d.ts" />
//
// Pids are u64s (too big for a JS number), so they cross as `bigint`. Messages
// and stream chunks are `Uint8Array`, with `*Text` helpers for UTF-8. `receive`
// and `Stream.read` are async (`await`) — the host call suspends the instance's
// fiber, so it's cheap, and they compose with other Promises.

// The runner polyfills a subset of Web APIs (console, URL, TextEncoder/Decoder,
// Headers, ReadableStream); add `"DOM"` to your tsconfig `lib` to type them.
// (`fetch` is typed but rejects at runtime until RUSM hosts wasi:http — roadmap.)

/** A back-pressured byte stream to or from another process. */
declare interface Stream {
  /** Write a chunk; a string is sent as UTF-8. Returns false if the peer is gone. */
  write(chunk: string | Uint8Array): boolean;
  /** The next chunk, or `null` at end-of-stream. */
  read(): Promise<Uint8Array | null>;
  /** Close the stream (signals end-of-stream to the reader). */
  close(): void;
}

/** The RUSM actor API: this process and its peers. Mirrors the Erlang `Process`. */
declare const Process: {
  /** This process's own pid. */
  self(): bigint;
  /** Every live pid on the node (subject to capability). */
  list(): bigint[];
  /** Spawn a registered component by name → its pid (capability-gated `spawn`). */
  spawn(name: string): bigint;
  /** Send a message to a pid. A string is sent as UTF-8; bytes are sent as-is. */
  send(to: bigint | string, msg: string | Uint8Array): void;
  /** The next message, as raw bytes. */
  receive(): Promise<Uint8Array>;
  /** The next message, decoded as UTF-8. */
  receiveText(): Promise<string>;
  /** Register this process under a name in the node registry. */
  register(name: string): boolean;
  /** Look up a registered name, or `null` if unregistered. */
  whereis(name: string): bigint | null;
  /** Whether a pid is still alive (subject to capability). */
  isAlive(pid: bigint | string): boolean;
  /** Kill a pid (subject to capability). */
  kill(pid: bigint | string): boolean;
  /** Set this process's human-readable label (shown in introspection). */
  setLabel(label: string): void;
  /** Open a byte stream to a pid, or `null` if it can't be opened. */
  openStream(to: bigint | string): Stream | null;
  /** Accept an incoming byte stream sent to this process. */
  acceptStream(): Stream;
};

/** The result of a typed call. A regular handler → a `Promise` you `await`; a
 *  generator handler → an `AsyncIterable` you `for await`. Function arguments
 *  become callbacks that stay in the caller — the service's invocations travel
 *  back as messages. */
type RusmCall<R> = R extends AsyncIterable<infer T>
  ? AsyncIterable<T> & PromiseLike<void>
  : R extends Iterable<infer T>
    ? AsyncIterable<T> & PromiseLike<void>
    : Promise<Awaited<R>>;

/** A typed client over a spawned service: each exported function becomes a call
 *  (`await`) — or a stream (`for await`); `cast` is fire-and-forget; `pid`/`stop`
 *  manage it. */
type ServiceClient<T> = {
  [K in keyof T]: T[K] extends (...args: infer A) => infer R
    ? (...args: A) => RusmCall<R>
    : never;
} & {
  /** Fire-and-forget variants (no reply awaited). */
  readonly cast: {
    [K in keyof T]: T[K] extends (...args: infer A) => any ? (...args: A) => void : never;
  };
  /** The spawned service's pid. */
  readonly pid: bigint;
  /** Kill the spawned service. */
  stop(): void;
};

/** Spawn a registered component by name and get a typed client for it — the
 *  concealed function call (spawn + send + receive, all hidden). Type it with the
 *  service's own exports: `spawn<typeof Calc>("calc")`. */
declare function spawn<T = Record<string, (...args: any[]) => any>>(
  component: string,
): ServiceClient<T>;
