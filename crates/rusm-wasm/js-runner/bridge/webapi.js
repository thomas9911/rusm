// webapi.js — generic Web API polyfills for QuickJS (rquickjs).
//
// Separation of concerns: this file is *pure standards* — zero RUSM/actor
// knowledge, zero host imports except optional `__print` for console. The actor
// bridge lives in process.js. A TS developer never imports or sees this; it's
// installed by the js-runner before their bundle runs, so `TextEncoder`, `URL`,
// `ReadableStream`, etc. are simply present.
//
// `fetch` is intentionally a clear-erroring stub: real HTTP needs RUSM to host
// `wasi:http` (roadmap). Everything else here is self-contained.

(() => {
  const G = globalThis;
  const def = (name, value) => {
    if (!G[name]) G[name] = value;
  };

  // console → optional host __print (set by the runner; no-op if absent).
  if (!G.console) {
    const print = G.__print ?? (() => {});
    // bigint (pids!) and undefined have no JSON form — String() them; JSON the rest.
    const show = (x) =>
      typeof x === "string" ? x
      : typeof x === "bigint" || x === undefined ? String(x)
      : JSON.stringify(x);
    const fmt = (...a) => a.map(show).join(" ");
    G.console = {
      log: (...a) => print(fmt(...a)),
      info: (...a) => print(fmt(...a)),
      warn: (...a) => print("[warn] " + fmt(...a)),
      error: (...a) => print("[error] " + fmt(...a)),
      debug: (...a) => print(fmt(...a)),
    };
  }

  class TextEncoderPF {
    encode(str) {
      const out = [];
      for (let i = 0; i < str.length; i++) {
        const c = str.charCodeAt(i);
        if (c < 0x80) out.push(c);
        else if (c < 0x800) out.push((c >> 6) | 0xc0, (c & 0x3f) | 0x80);
        else out.push((c >> 12) | 0xe0, ((c >> 6) & 0x3f) | 0x80, (c & 0x3f) | 0x80);
      }
      return new Uint8Array(out);
    }
  }
  class TextDecoderPF {
    constructor(_enc = "utf-8") {}
    decode(input) {
      if (!input) return "";
      const b = input instanceof ArrayBuffer ? new Uint8Array(input) : input;
      let s = "";
      for (let i = 0; i < b.length; ) {
        const x = b[i++];
        if (x < 0x80) s += String.fromCharCode(x);
        else if ((x & 0xe0) === 0xc0) s += String.fromCharCode(((x & 0x1f) << 6) | (b[i++] & 0x3f));
        else if ((x & 0xf0) === 0xe0)
          s += String.fromCharCode(((x & 0x0f) << 12) | ((b[i++] & 0x3f) << 6) | (b[i++] & 0x3f));
        else {
          const cp =
            (((x & 0x07) << 18) | ((b[i++] & 0x3f) << 12) | ((b[i++] & 0x3f) << 6) | (b[i++] & 0x3f)) -
            0x10000;
          s += String.fromCharCode(0xd800 + (cp >> 10), 0xdc00 + (cp & 0x3ff));
        }
      }
      return s;
    }
  }

  class HeadersPF {
    constructor(init) {
      this._d = {};
      const add = (k, v) => (this._d[String(k).toLowerCase()] = v);
      if (Array.isArray(init)) init.forEach(([k, v]) => add(k, v));
      else if (init) Object.entries(init).forEach(([k, v]) => add(k, v));
    }
    get(k) { return this._d[k.toLowerCase()] ?? null; }
    set(k, v) { this._d[k.toLowerCase()] = v; }
    has(k) { return k.toLowerCase() in this._d; }
    delete(k) { delete this._d[k.toLowerCase()]; }
    forEach(fn) { Object.entries(this._d).forEach(([k, v]) => fn(v, k)); }
    entries() { return Object.entries(this._d)[Symbol.iterator](); }
  }

  class URLSearchParamsPF {
    constructor(init) {
      this._d = [];
      if (typeof init === "string") {
        (init[0] === "?" ? init.slice(1) : init).split("&").forEach((p) => {
          if (!p) return;
          const [k, v = ""] = p.split("=");
          this._d.push([decodeURIComponent(k), decodeURIComponent(v)]);
        });
      } else if (Array.isArray(init)) this._d = [...init];
      else if (init) Object.entries(init).forEach(([k, v]) => this._d.push([k, v]));
    }
    get(k) { return this._d.find(([key]) => key === k)?.[1] ?? null; }
    getAll(k) { return this._d.filter(([key]) => key === k).map(([, v]) => v); }
    set(k, v) {
      const i = this._d.findIndex(([key]) => key === k);
      if (i >= 0) this._d[i][1] = v; else this._d.push([k, v]);
    }
    append(k, v) { this._d.push([k, v]); }
    has(k) { return this._d.some(([key]) => key === k); }
    delete(k) { this._d = this._d.filter(([key]) => key !== k); }
    forEach(fn) { this._d.forEach(([k, v]) => fn(v, k)); }
    entries() { return this._d[Symbol.iterator](); }
    toString() {
      return this._d.map(([k, v]) => `${encodeURIComponent(k)}=${encodeURIComponent(v)}`).join("&");
    }
  }

  class URLPF {
    constructor(url, base) {
      let full = String(url);
      if (base && !full.match(/^[a-z][a-z+\-.]*:/i))
        full = String(base).replace(/\/$/, "") + "/" + full.replace(/^\//, "");
      this.href = full;
      const m = full.match(/^(([a-z][a-z+\-.]*):\/\/([^/:?#]*)(?::(\d+))?)(\/[^?#]*)?(\?[^#]*)?(#.*)?$/i);
      if (m) {
        this.protocol = m[2] + ":"; this.hostname = m[3]; this.port = m[4] ?? "";
        this.host = this.hostname + (this.port ? ":" + this.port : ""); this.origin = m[1];
        this.pathname = m[5] ?? "/"; this.search = m[6] ?? ""; this.hash = m[7] ?? "";
      } else {
        this.protocol = this.hostname = this.port = this.host = this.origin = this.search = this.hash = "";
        this.pathname = full;
      }
      this.searchParams = new URLSearchParamsPF(this.search ? this.search.slice(1) : "");
    }
    toString() { return this.href; }
  }

  // Supports both `new ReadableStream(uint8Array)` (legacy one-shot) and
  // `new ReadableStream({ start, pull, cancel })` with a controller — the shape an
  // SSE handler uses. (The per-request js-http-runner installs an identical class;
  // sharing it here lets the resident runner stream too.)
  class ReadableStreamPF {
    constructor(source) {
      if (source instanceof Uint8Array) {
        this._fixed = source;
        return;
      }
      this._source = source || {};
      this._queue = [];
      this._closed = false;
      this._error = null;
      this._started = false;
      const self = this;
      this._controller = {
        enqueue(chunk) { self._queue.push(chunk); },
        close() { self._closed = true; },
        error(e) { self._error = e; self._closed = true; },
      };
    }
    getReader() {
      const self = this;
      if (self._fixed !== undefined) {
        let done = false;
        return {
          read() {
            if (done) return Promise.resolve({ done: true });
            done = true;
            return Promise.resolve({ done: false, value: self._fixed });
          },
          cancel() { done = true; },
          releaseLock() {},
        };
      }
      return {
        async read() {
          if (!self._started) {
            self._started = true;
            if (self._source.start) await self._source.start(self._controller);
          }
          // Pull until a chunk is queued or the stream closes. A bounded guard keeps
          // a buggy guest (a pull that never enqueues or closes) from hanging.
          let spins = 0;
          while (self._queue.length === 0 && !self._closed) {
            if (!self._source.pull) break;
            await self._source.pull(self._controller);
            if (++spins > 1_000_000) break;
          }
          if (self._error) throw self._error;
          if (self._queue.length) return { done: false, value: self._queue.shift() };
          return { done: true };
        },
        cancel(reason) {
          self._closed = true;
          return self._source.cancel ? self._source.cancel(reason) : undefined;
        },
        releaseLock() {},
      };
    }
  }

  class BlobPF {
    constructor(parts) { this._s = (parts ?? []).map(String).join(""); }
    text() { return Promise.resolve(this._s); }
    get size() { return this._s.length; }
  }
  class FilePF extends BlobPF {
    constructor(parts, name, opts) { super(parts, opts); this.name = name; }
  }
  class FormDataPF {
    constructor() { this._d = []; }
    append(k, v) { this._d.push([k, v]); }
    get(k) { return this._d.find(([key]) => key === k)?.[1] ?? null; }
    has(k) { return this._d.some(([key]) => key === k); }
    entries() { return this._d[Symbol.iterator](); }
  }
  class AbortControllerPF {
    constructor() {
      this.signal = { aborted: false, onabort: null, addEventListener() {}, removeEventListener() {} };
    }
    abort() { this.signal.aborted = true; }
  }
  class RequestPF {
    constructor(url, init) {
      this.url = String(url); this.method = init?.method ?? "GET";
      this.headers = new HeadersPF(init?.headers); this.body = init?.body ?? null;
    }
  }
  // A real Response (the per-request js-http-runner augments this with a streaming
  // body; the resident server reads .status/.headers/.body directly).
  class ResponsePF {
    constructor(body, init) {
      this.body = body ?? null;
      this.status = (init && init.status) || 200;
      this.statusText = (init && init.statusText) || "";
      this.headers = new HeadersPF(init && init.headers);
    }
  }

  def("TextEncoder", TextEncoderPF);
  def("TextDecoder", TextDecoderPF);
  def("Headers", HeadersPF);
  def("URL", URLPF);
  def("URLSearchParams", URLSearchParamsPF);
  def("ReadableStream", ReadableStreamPF);
  def("Blob", BlobPF);
  def("File", FilePF);
  def("FormData", FormDataPF);
  def("AbortController", AbortControllerPF);
  def("Request", RequestPF);
  def("Response", ResponsePF);

  // setTimeout/clearTimeout via microtasks (QuickJS has no event loop timers).
  if (!G.setTimeout) {
    let id = 0;
    const cancelled = new Set();
    G.setTimeout = (fn) => {
      const t = ++id;
      Promise.resolve().then(() => Promise.resolve().then(() => {
        if (!cancelled.has(t)) fn();
        cancelled.delete(t);
      }));
      return t;
    };
    G.clearTimeout = (t) => cancelled.add(t);
  }

  // fetch needs RUSM to host wasi:http (roadmap). Fail clearly until then.
  def("fetch", () =>
    Promise.reject(new Error("fetch() is unavailable: RUSM does not host wasi:http yet (roadmap)")));
})();
