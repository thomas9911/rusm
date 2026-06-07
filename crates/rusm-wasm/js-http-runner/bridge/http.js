// http.js — the fetch-handler bridge for the js-http-runner. Loaded after the
// shared webapi.js: it upgrades the stub `Response` to a real one, gives `Request`
// a body + text()/json(), and exposes `__rusm_http(...)` for the Rust host to call
// per request. The host marshals the wasi:http request in and the result back out.
(() => {
  const G = globalThis;

  G.Response = class Response {
    constructor(body, init) {
      this.body = body ?? null;
      this.status = (init && init.status) || 200;
      this.statusText = (init && init.statusText) || "";
      this.headers = new G.Headers(init && init.headers);
    }
  };

  // Augment the base Request (which already stores `url`/`method`/`headers`/`body`)
  // with text()/json(). No `body` accessor — the base assigns `this.body`, and a
  // getter-only override would make that assignment throw ("no setter").
  const RequestBase = G.Request;
  G.Request = class Request extends RequestBase {
    async text() {
      if (this.body == null) return "";
      return typeof this.body === "string"
        ? this.body
        : new G.TextDecoder().decode(this.body);
    }
    async json() { return JSON.parse(await this.text()); }
  };

  // Resolve the handler from the bundle's exports: `export default { fetch }`
  // (Workers/Deno shape), `export default (req) => Response`, or `export fetch`.
  const resolveHandler = () => {
    const h = (G.module && G.module.exports) || {};
    if (h.default && typeof h.default.fetch === "function") return h.default.fetch.bind(h.default);
    if (typeof h.default === "function") return h.default;
    if (typeof h.fetch === "function") return h.fetch;
    return null;
  };

  // Drain a Response body (string / Uint8Array / ReadableStream) to bytes.
  const bodyToBytes = async (body) => {
    if (body == null) return new Uint8Array();
    if (typeof body === "string") return new G.TextEncoder().encode(body);
    if (body instanceof Uint8Array) return body;
    if (body && typeof body.getReader === "function") {
      const reader = body.getReader();
      const chunks = [];
      let total = 0;
      for (;;) {
        const { done, value } = await reader.read();
        if (done) break;
        const v = typeof value === "string" ? new G.TextEncoder().encode(value) : value;
        chunks.push(v);
        total += v.length;
      }
      const out = new Uint8Array(total);
      let off = 0;
      for (const c of chunks) { out.set(c, off); off += c.length; }
      return out;
    }
    return new G.TextEncoder().encode(String(body));
  };

  // Called by the Rust host with the request; returns the normalized response.
  G.__rusm_http = async function (method, url, headerPairs, bodyBytes) {
    const handler = resolveHandler();
    if (!handler) {
      throw new Error(
        "component has no fetch handler (export default { fetch } or export default (req) => Response)",
      );
    }
    const req = new G.Request(url, { method, headers: headerPairs, body: bodyBytes });
    const res = await handler(req);
    if (!res) throw new Error("fetch handler returned no Response");
    const headers = [];
    if (res.headers && typeof res.headers.forEach === "function") {
      res.headers.forEach((v, k) => headers.push([String(k), String(v)]));
    }
    return { status: res.status || 200, headers, body: await bodyToBytes(res.body) };
  };
})();
