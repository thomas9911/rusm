// A wasi:http handler written in **TypeScript** — the TS twin of http-hello. The
// Workers/Deno `export default { fetch }` shape: the js-http-runner builds a Request
// from the wasi:http request, runs this, and marshals the Response back. Built with:
//   bun build --target=browser --format=cjs --outfile ts_http_hello.js index.ts

// `Request`/`Response` are the Web globals the runner polyfills.
declare const Response: new (body?: string, init?: { headers?: Record<string, string> }) => unknown;
type Req = { method: string; url: string };

export default {
  fetch(req: Req): unknown {
    return new Response(`hello from TS (${req.method})\n`, {
      headers: { "content-type": "text/plain" },
    });
  },
};
