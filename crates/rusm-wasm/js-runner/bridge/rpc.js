// rpc.js — the RPC / service layer over the raw Process actor API (process.js).
//
// Two halves of one protocol:
//   • service — a component that EXPORTS functions. The runner drives __rusm_entry,
//     which dispatches each request to the matching export and replies.
//   • client  — `spawn(name)` returns a typed proxy. `proxy.method(...)` is a call
//     you `await` for the reply, or `for await` to stream a generator handler's
//     chunks; `proxy.cast.method(...)` is fire-and-forget; `proxy.pid`/`.stop()`
//     manage the process. Function arguments become **callbacks** routed home.
//
// Wire protocol (JSON over the byte mailbox):
//   request   { op, args, from, ref }            — ref omitted ⇒ a cast
//   request   { op, args, from, ref, stream:true }— streaming: reply rides a stream
//   reply     { ref, ok } | { ref, err }
//   callback  { op:"__cb", cbref, args }          — invoke a caller-side function
// Streamed chunks are JSON-encoded values, one per stream write.

const __td = new TextDecoder();
let __ref = 0;
let __cbSeq = 0;
// Caller-side functions passed as args, by id, so a service's callback message can
// be routed back to the live closure (the function never leaves this instance).
const __callbacks = {};

// A JSON.stringify replacer that encodes a function argument as a {__cb:id} marker
// (registering it in `cbs`) in a single serialization pass — no pre-clone of args.
function __cbReplacer(cbs) {
  return (_key, value) => {
    if (typeof value === "function") {
      const id = ++__cbSeq;
      cbs[id] = value;
      return { __cb: id };
    }
    return value;
  };
}

// Encode + send a request, registering any callback args so their invocations can
// be routed home. Returns the ids registered (to clean up when the call is done).
function __sendRequest(pid, msg) {
  const cbs = {};
  const payload = JSON.stringify(msg, __cbReplacer(cbs));
  const ids = Object.keys(cbs);
  for (const id of ids) __callbacks[id] = cbs[id];
  Process.send(pid, payload);
  return ids;
}

// Service side: replace {__cb:id} markers with functions that message the caller.
// Only called when the request actually carries a callback (see __rusm_serve).
function __decodeArgs(args, from) {
  const dec = (v) => {
    if (Array.isArray(v)) return v.map(dec);
    if (v && typeof v === "object") {
      if (typeof v.__cb === "number" && Object.keys(v).length === 1) {
        const cbref = v.__cb;
        return (...a) =>
          Process.send(BigInt(from), JSON.stringify({ op: "__cb", cbref, args: a }));
      }
      const o = {};
      for (const k of Object.keys(v)) o[k] = dec(v[k]);
      return o;
    }
    return v;
  };
  return (args || []).map(dec);
}

// A call: send the request, then await the matching reply — servicing callback
// messages and stashing unrelated mail so the app's own receive still sees it.
async function __call(pid, op, args, expectReply) {
  const ref = expectReply ? ++__ref : undefined;
  const msg = { op, args, from: Process.self().toString() };
  if (expectReply) msg.ref = ref;
  const ids = __sendRequest(pid, msg);
  if (!expectReply) return undefined; // cast: fire-and-forget
  try {
    for (;;) {
      const raw = await Process.receive();
      let m;
      try { m = JSON.parse(__td.decode(raw)); } catch { __rusm_stash(raw); continue; }
      if (m && m.op === "__cb") { __callbacks[m.cbref]?.(...(m.args || [])); continue; }
      if (m && m.ref === ref) {
        if ("err" in m) throw new Error(m.err);
        return m.ok;
      }
      __rusm_stash(raw); // not our reply — leave it for the app
    }
  } finally {
    for (const id of ids) delete __callbacks[id];
  }
}

// A streaming call: the service opens a byte stream back; yield each JSON chunk.
async function* __streamCall(pid, op, args) {
  const ref = ++__ref;
  __sendRequest(pid, { op, args, from: Process.self().toString(), ref, stream: true });
  const s = Process.acceptStream();
  let chunk;
  while ((chunk = await s.read()) !== null) {
    yield JSON.parse(__td.decode(chunk));
  }
}

// A method result: awaitable (→ a call) and async-iterable (→ a streaming call).
// The caller chooses by `await proxy.m(...)` vs `for await (... of proxy.m(...))`.
function __invoke(pid, op, args) {
  let p;
  const call = () => (p = p || __call(pid, op, args, true));
  return {
    then: (res, rej) => call().then(res, rej),
    catch: (f) => call().catch(f),
    finally: (f) => call().finally(f),
    [Symbol.asyncIterator]: () => __streamCall(pid, op, args)[Symbol.asyncIterator](),
  };
}

function __castClient(pid) {
  return new Proxy({}, { get: (_t, op) => (...args) => __call(pid, String(op), args, false) });
}

// `spawn(name)` → a typed client over a freshly spawned component.
globalThis.spawn = function (name) {
  const pid = Process.spawn(name);
  return new Proxy(
    {},
    {
      get(_t, op) {
        if (op === "pid") return pid;
        if (op === "stop") return () => Process.kill(pid);
        if (op === "cast") return __castClient(pid);
        return (...args) => __invoke(pid, String(op), args);
      },
    },
  );
};

// Service dispatch: receive a request, call the matching exported handler, and
// either reply with its value or stream a generator handler's chunks back.
async function __rusm_serve(handlers) {
  for (;;) {
    let text;
    try { text = await Process.receiveText(); } catch { continue; }
    let req;
    try { req = JSON.parse(text); } catch { continue; }
    const { op, args, from, ref, stream } = req || {};
    const fn = handlers[op];
    // Only walk the args to rebuild callbacks when one is actually present.
    const decoded = text.includes('"__cb"') ? __decodeArgs(args, from) : args || [];
    if (typeof fn !== "function") {
      if (ref != null && from != null) {
        Process.send(BigInt(from), JSON.stringify({ ref, err: "no such function: " + op }));
      }
      continue;
    }
    if (stream) {
      // Streaming handler (a generator / async-iterable): pump chunks down a stream.
      const out = Process.openStream(from);
      if (out) {
        try {
          for await (const chunk of await fn(...decoded)) out.write(JSON.stringify(chunk));
        } catch (_e) {
          // a handler error just ends the stream early (close below)
        }
        out.close();
      }
      continue;
    }
    let reply;
    try { reply = { ref, ok: await fn(...decoded) }; }
    catch (e) { reply = { ref, err: String((e && e.message) || e) }; }
    if (ref != null && from != null) Process.send(BigInt(from), JSON.stringify(reply));
  }
}

// A guest **supervisor**: spawn + monitor named children and restart per a
// strategy ("one_for_one" | "one_for_all" | "rest_for_one") when one dies. A dead
// child arrives as a `{ __down }` message (no polling). `await supervise({...})`.
globalThis.supervise = async function ({ strategy = "one_for_one", children = [], maxRestarts = 0, maxSeconds = 0 }) {
  const start = (name) => {
    const pid = Process.spawn(name);
    Process.monitor(pid);
    return pid;
  };
  let pids = children.map(start);
  // Lifetime mode counts; windowed mode (maxSeconds) keeps restart times in-window
  // — Erlang's restart intensity. System signals are unaffected.
  let lifetime = 0;
  let window = [];
  for (;;) {
    let m;
    try { m = JSON.parse(await Process.receiveText()); } catch { continue; }
    if (!m || m.__down == null) continue;
    const dead = BigInt(m.__down);
    const i = pids.findIndex((p) => p === dead);
    if (i < 0) continue;
    let overBudget;
    if (maxSeconds) {
      const now = Date.now();
      window.push(now);
      const cutoff = now - maxSeconds * 1000;
      window = window.filter((t) => t >= cutoff);
      overBudget = maxRestarts && window.length > maxRestarts;
    } else {
      overBudget = maxRestarts && ++lifetime > maxRestarts;
    }
    if (overBudget) return;
    if (strategy === "one_for_all") {
      pids.forEach((p, j) => { if (j !== i) Process.kill(p); });
      pids = children.map(start);
    } else if (strategy === "rest_for_one") {
      for (let j = i + 1; j < pids.length; j++) Process.kill(pids[j]);
      for (let j = i; j < pids.length; j++) pids[j] = start(children[j]);
    } else {
      pids[i] = start(children[i]);
    }
  }
};

// Called by the runner after evaluating the bundle; returns the promise the runner
// drives to completion. A component is a SERVICE if it exports named functions (run
// the dispatch loop), a WORKER if it exports `default` (run it), or a bare script
// (already ran during eval — nothing left to drive).
globalThis.__rusm_entry = function () {
  const h = (globalThis.module && globalThis.module.exports) || {};
  const named = Object.keys(h).filter((k) => k !== "default" && typeof h[k] === "function");
  if (named.length) {
    const handlers = {};
    for (const k of named) handlers[k] = h[k];
    return __rusm_serve(handlers);
  }
  if (typeof h.default === "function") return h.default();
  return Promise.resolve();
};
