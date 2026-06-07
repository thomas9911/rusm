// rpc.js — the RPC / service layer over the raw Process actor API (process.js).
//
// Two halves of one protocol:
//   • service — a component that EXPORTS functions. The runner drives __rusm_entry,
//     which dispatches each incoming request to the matching export and replies.
//   • client  — `spawn(name)` returns a typed proxy whose method calls become
//     request/reply messages: the concealed function call (spawn + send + receive,
//     all hidden). `.cast.method()` is fire-and-forget; `.pid` / `.stop()` manage
//     the underlying process.
//
// Wire protocol (JSON over the byte mailbox):
//   request  { op, args, from, ref }   — ref omitted ⇒ a cast (no reply expected)
//   reply    { ref, ok }  |  { ref, err }

const __td = new TextDecoder();
let __ref = 0;

// One client call: send the request, then await the matching reply, stashing any
// unrelated mail so the app's own `Process.receive` still sees it.
async function __call(pid, op, args, expectReply) {
  const msg = { op, args, from: Process.self().toString() };
  const ref = expectReply ? ++__ref : undefined;
  if (expectReply) msg.ref = ref;
  Process.send(pid, JSON.stringify(msg));
  if (!expectReply) return undefined; // cast: fire-and-forget
  for (;;) {
    const raw = await Process.receive(); // Uint8Array
    let m;
    try { m = JSON.parse(__td.decode(raw)); } catch { __rusm_stash(raw); continue; }
    if (m && m.ref === ref) {
      if ("err" in m) throw new Error(m.err);
      return m.ok;
    }
    __rusm_stash(raw); // not our reply — leave it for the app
  }
}

function __clientFor(pid, expectReply) {
  return new Proxy(
    {},
    { get: (_t, op) => (...args) => __call(pid, String(op), args, expectReply) },
  );
}

// `spawn(name)` → a typed client over a freshly spawned component.
globalThis.spawn = function (name) {
  const pid = Process.spawn(name);
  const call = __clientFor(pid, true);
  return new Proxy(
    {},
    {
      get(_t, op) {
        if (op === "pid") return pid;
        if (op === "stop") return () => Process.kill(pid);
        if (op === "cast") return __clientFor(pid, false);
        return call[op];
      },
    },
  );
};

// Service dispatch: receive a request, call the matching exported handler (sync or
// async), and reply (unless it was a cast). Runs until the process is killed.
async function __rusm_serve(handlers) {
  for (;;) {
    let req;
    try { req = JSON.parse(await Process.receiveText()); } catch { continue; }
    const { op, args = [], from, ref } = req || {};
    const fn = handlers[op];
    let reply;
    if (typeof fn !== "function") {
      reply = { ref, err: "no such function: " + op };
    } else {
      try { reply = { ref, ok: await fn(...args) }; }
      catch (e) { reply = { ref, err: String((e && e.message) || e) }; }
    }
    if (ref != null && from != null) Process.send(BigInt(from), JSON.stringify(reply));
  }
}

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
