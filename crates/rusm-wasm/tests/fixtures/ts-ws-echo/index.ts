// A WebSocket-handler component written in **TypeScript** — the TS twin of
// rs-ws-echo. Pure sandboxed actor logic, no IO: the host owns the socket and
// delivers each inbound frame to this worker's mailbox. Its first message is the
// connection's *writer* pid (decimal); it echoes every later message back.
//
// A worker is `export default` an async function the js-runner drives to
// completion (each `receive` suspends the fiber). Built with:
//   bun build --target=browser --format=cjs --outfile ts_ws_echo.js index.ts

// `Process` is the global actor API the js-runner injects (see packages/rusm-ts).
declare const Process: {
  receive(): Promise<Uint8Array>;
  receiveText(): Promise<string>;
  send(to: bigint, message: Uint8Array | string): void;
};

export default async function () {
  // Message 1 (to the app): the writer pid to answer through, as a decimal string.
  const writer = BigInt(await Process.receiveText());
  // Every later message is one inbound WS frame — echo it straight back.
  for (;;) {
    const frame = await Process.receive();
    Process.send(writer, frame);
  }
}
