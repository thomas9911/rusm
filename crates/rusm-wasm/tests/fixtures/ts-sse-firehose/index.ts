// An endless SSE **handler** in TypeScript (server-side) — the twin of sse-firehose,
// for the sse-fanout (TS) stress scenario. Each pull enqueues one event; the
// js-http-runner flushes it and pulls again, as fast as the client drains. A server
// handler, not a client `fetch`.
//   bun build --target=browser --format=cjs --outfile ts_sse_firehose.js index.ts

declare const Response: new (body?: unknown, init?: { headers?: Record<string, string> }) => unknown;
declare const ReadableStream: new (source: {
  pull(controller: { enqueue(chunk: Uint8Array): void }): void;
}) => unknown;
declare const TextEncoder: new () => { encode(input: string): Uint8Array };

export default async function handle(): Promise<unknown> {
  let n = 0;
  const enc = new TextEncoder();
  const body = new ReadableStream({
    pull(controller) {
      controller.enqueue(enc.encode(`data: ${n++}\n\n`));
    },
  });
  return new Response(body, { headers: { "content-type": "text/event-stream" } });
}
