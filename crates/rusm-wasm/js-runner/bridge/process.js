// process.js — the RUSM actor API for JS guests.
//
// Separation of concerns: this file is *only* the `Process`/`Stream` bridge over
// the host primitives the runner installs (the `__*` globals). Web API polyfills
// live in webapi.js; lifecycle/host wiring lives in the runner (lib.rs).
//
// Pids cross the boundary as decimal strings (a u64 doesn't fit a JS number) and
// surface as BigInt; messages/chunks are Uint8Array, with text helpers.

class Stream {
  constructor(handle) { this.handle = handle; }
  // write accepts a string (UTF-8) or a Uint8Array.
  write(chunk) {
    return typeof chunk === "string"
      ? __stream_write_text(this.handle, chunk)
      : __stream_write(this.handle, chunk);
  }
  close() { __stream_close(this.handle); }
  // Uint8Array, or null at end-of-stream (host None → undefined → null).
  read() {
    const c = __stream_read(this.handle);
    return c === undefined ? null : c;
  }
}

globalThis.Process = {
  self() { return BigInt(__own_pid()); },
  list() { return __list().map(BigInt); },
  send(to, msg) {
    if (typeof msg === "string") __send_text(String(to), msg);
    else __send(String(to), msg);
  },
  receive() { return __receive(); }, // Uint8Array
  receiveText() { return __receive_text(); }, // string
  register(name) { return __register(name); },
  whereis(name) { const p = __whereis(name); return p === "" ? null : BigInt(p); },
  isAlive(pid) { return __is_alive(String(pid)); },
  kill(pid) { return __kill(String(pid)); },
  setLabel(label) { __set_label(label); },
  openStream(to) { const h = __stream_open(String(to)); return h < 0 ? null : new Stream(h); },
  acceptStream() { return new Stream(__stream_accept()); },
};
