// rusm — the guest API for RUSM TypeScript components.
//
// The js-runner injects the `Process` actor API and the `spawn` typed-client
// factory as globals (and polyfills the Web APIs); this package re-exports them as
// a normal module and ships the types, so a component writes:
//
//     import { Process, spawn } from "rusm";
//
// Pids are u64s, too large for a JS number, so they cross as `bigint`. Messages
// and stream chunks are `Uint8Array`. `receive` / `Stream.read` are async.

/** A back-pressured byte stream to or from another process. */
export interface Stream {
  /** Write a chunk; a string is sent as UTF-8. Returns false if the peer is gone. */
  write(chunk: string | Uint8Array): boolean;
  /** The next chunk, or `null` at end-of-stream. */
  read(): Promise<Uint8Array | null>;
  /** Close the stream (signals end-of-stream to the reader). */
  close(): void;
}

/** The RUSM actor API: this process and its peers. Mirrors the Erlang `Process`. */
export interface ProcessApi {
  self(): bigint;
  list(): bigint[];
  /** Spawn a registered component by name → its pid (capability-gated). */
  spawn(name: string): bigint;
  send(to: bigint | string, msg: string | Uint8Array): void;
  receive(): Promise<Uint8Array>;
  receiveText(): Promise<string>;
  register(name: string): boolean;
  whereis(name: string): bigint | null;
  isAlive(pid: bigint | string): boolean;
  kill(pid: bigint | string): boolean;
  setLabel(label: string): void;
  openStream(to: bigint | string): Stream | null;
  acceptStream(): Stream;
}

/** The result of a typed call: `await` it for the reply, or `for await` it to
 *  stream a generator handler's chunks. Function arguments become callbacks that
 *  stay in the caller — the service's invocations travel back as messages. */
export type RusmCall<R> = R extends AsyncIterable<infer T>
  ? AsyncIterable<T> & PromiseLike<void>
  : R extends Iterable<infer T>
    ? AsyncIterable<T> & PromiseLike<void>
    : Promise<Awaited<R>>;

/** A typed client over a spawned service: each exported function becomes a call
 *  (`await`) — or a stream (`for await`); `cast` is fire-and-forget. */
export type ServiceClient<T> = {
  [K in keyof T]: T[K] extends (...args: infer A) => infer R
    ? (...args: A) => RusmCall<R>
    : never;
} & {
  readonly cast: {
    [K in keyof T]: T[K] extends (...args: infer A) => any ? (...args: A) => void : never;
  };
  readonly pid: bigint;
  stop(): void;
};

// The runner installs these globals before the bundle runs (and wraps the bundle
// in a CommonJS scope, so this module's bindings never clobber them).
const g = globalThis as unknown as {
  Process: ProcessApi;
  spawn: <T>(component: string) => ServiceClient<T>;
};

/** The actor API for this process. */
export const Process: ProcessApi = g.Process;

/** Spawn a registered component and get a typed client — the concealed function
 *  call (spawn + send + receive, hidden). Type it with the service's exports:
 *  `spawn<typeof Calc>("calc")`. */
export const spawn = <T = Record<string, (...args: any[]) => any>>(
  component: string,
): ServiceClient<T> => g.spawn<T>(component);
