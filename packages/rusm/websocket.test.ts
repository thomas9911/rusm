import { test, expect } from "bun:test";

// The runner injects `Process` as a global before the bundle runs; the helper
// captures it at import, so the mock must be installed before the dynamic import.
const sent: Array<[bigint, string | Uint8Array]> = [];
(globalThis as unknown as { Process: unknown }).Process = {
  send: (to: bigint, msg: string | Uint8Array) => sent.push([to, msg]),
};

const { websocket } = await import("./index");

test("websocket() exposes the { websocket: { open, message, close } } shape the runner drives", () => {
  const handler = websocket({ message: () => {} });
  expect(typeof handler.websocket.open).toBe("function");
  expect(typeof handler.websocket.message).toBe("function");
  expect(typeof handler.websocket.close).toBe("function");
});

test("a connection event becomes a Socket; socket.send routes to Process.send(conn, …)", () => {
  sent.length = 0;
  const opened: bigint[] = [];
  const closed: bigint[] = [];
  const handler = websocket({
    open: (s) => opened.push(s.id),
    message: (s, data) => s.send(data), // echo
    close: (s) => closed.push(s.id),
  });

  handler.websocket.open(7n);
  expect(opened).toEqual([7n]);

  const frame = new Uint8Array([1, 2, 3]);
  handler.websocket.message(7n, frame);
  expect(sent).toEqual([[7n, frame]]);

  handler.websocket.close(7n);
  expect(closed).toEqual([7n]);
});

test("open and close are optional — a message-only handler never throws on them", () => {
  const handler = websocket({ message: () => {} });
  expect(() => handler.websocket.open(1n)).not.toThrow();
  expect(() => handler.websocket.close(1n)).not.toThrow();
});
