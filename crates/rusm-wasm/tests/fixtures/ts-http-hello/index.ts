// A TypeScript HTTP **handler** (server-side): a request → response function, the
// twin of http-hello. The js-http-runner calls it for each incoming request and
// marshals the Response back. (This is a server handler, not a client `fetch`.)
//   bun build --target=browser --format=cjs --outfile ts_http_hello.js index.ts

// `Response` is the Web global the runner polyfills.
declare const Response: new (body?: string, init?: { headers?: Record<string, string> }) => unknown;
type Request = { method: string; url: string };

export default async function handle(request: Request): Promise<unknown> {
  return new Response(`hello from TS (${request.method})\n`, {
    headers: { "content-type": "text/plain" },
  });
}
