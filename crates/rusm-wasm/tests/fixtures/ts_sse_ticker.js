var __defProp = Object.defineProperty;
var __getOwnPropNames = Object.getOwnPropertyNames;
var __getOwnPropDesc = Object.getOwnPropertyDescriptor;
var __hasOwnProp = Object.prototype.hasOwnProperty;
function __accessProp(key) {
  return this[key];
}
var __toCommonJS = (from) => {
  var entry = (__moduleCache ??= new WeakMap).get(from), desc;
  if (entry)
    return entry;
  entry = __defProp({}, "__esModule", { value: true });
  if (from && typeof from === "object" || typeof from === "function") {
    for (var key of __getOwnPropNames(from))
      if (!__hasOwnProp.call(entry, key))
        __defProp(entry, key, {
          get: __accessProp.bind(from, key),
          enumerable: !(desc = __getOwnPropDesc(from, key)) || desc.enumerable
        });
  }
  __moduleCache.set(from, entry);
  return entry;
};
var __moduleCache;
var __returnValue = (v) => v;
function __exportSetter(name, newValue) {
  this[name] = __returnValue.bind(null, newValue);
}
var __export = (target, all) => {
  for (var name in all)
    __defProp(target, name, {
      get: all[name],
      enumerable: true,
      configurable: true,
      set: __exportSetter.bind(all, name)
    });
};

// index.ts
var exports_ts_sse_ticker = {};
__export(exports_ts_sse_ticker, {
  default: () => ts_sse_ticker_default
});
module.exports = __toCommonJS(exports_ts_sse_ticker);
var ts_sse_ticker_default = {
  fetch() {
    let n = 0;
    const enc = new TextEncoder;
    const body = new ReadableStream({
      pull(controller) {
        if (n >= 5) {
          controller.close();
          return;
        }
        controller.enqueue(enc.encode(`data: tick ${n}

`));
        n++;
      }
    });
    return new Response(body, { headers: { "content-type": "text/event-stream" } });
  }
};
