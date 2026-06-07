// rusm.d.ts — ambient types for a RUSM TypeScript component.
//
// A TS component is plain TypeScript run on the shared rquickjs js-runner: it
// gets the `Process` actor API (this file) plus the standard Web APIs the runner
// polyfills (URL, TextEncoder/Decoder, Headers, ReadableStream — already typed by
// TS's built-in libs). Reference it from your entry file:
//
//     /// <reference path="./rusm.d.ts" />
//
// Pids are u64s, too large for a JS number, so they cross as `bigint`. Messages
// and stream chunks are `Uint8Array`, with `*Text` helpers for UTF-8 strings.

/** A back-pressured byte stream to or from another process. */
declare interface Stream {
  /** Write a chunk; a string is sent as UTF-8. Returns false if the peer is gone. */
  write(chunk: string | Uint8Array): boolean;
  /** Read the next chunk, or `null` at end-of-stream. Blocks until a chunk arrives. */
  read(): Uint8Array | null;
  /** Close the stream (signals end-of-stream to the reader). */
  close(): void;
}

/** The RUSM actor API: this process and its peers. Mirrors the Erlang `Process`. */
declare const Process: {
  /** This process's own pid. */
  self(): bigint;
  /** Every live pid on the node (subject to capability). */
  list(): bigint[];
  /** Send a message to a pid. A string is sent as UTF-8; bytes are sent as-is. */
  send(to: bigint | string, msg: string | Uint8Array): void;
  /** Block until a message arrives; returns its raw bytes. */
  receive(): Uint8Array;
  /** Block until a message arrives; returns it decoded as UTF-8. */
  receiveText(): string;
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
