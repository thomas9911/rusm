// An SSE handler in **TypeScript** — the TS twin of sse-ticker. Returns a Response
// whose body is a ReadableStream that enqueues `data:` events; the js-http-runner
// pulls each chunk and flushes it incrementally (true streaming). Built with:
//   bun build --target=browser --format=cjs --outfile ts_sse_ticker.js index.ts

declare const Response: new (body?: unknown, init?: { headers?: Record<string, string> }) => unknown;
declare const ReadableStream: new (source: {
  pull(controller: { enqueue(chunk: Uint8Array): void; close(): void }): void | Promise<void>;
}) => unknown;
declare const TextEncoder: new () => { encode(input: string): Uint8Array };

export default {
  fetch(): unknown {
    let n = 0;
    const enc = new TextEncoder();
    const body = new ReadableStream({
      pull(controller) {
        if (n >= 5) {
          controller.close();
          return;
        }
        controller.enqueue(enc.encode(`data: tick ${n}\n\n`));
        n++;
      },
    });
    return new Response(body, { headers: { "content-type": "text/event-stream" } });
  },
};
