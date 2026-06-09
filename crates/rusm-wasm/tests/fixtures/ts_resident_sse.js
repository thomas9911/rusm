// A resident TS SSE handler: the same `export default { fetch }` shape, returning a
// Response whose body is a ReadableStream of `text/event-stream` events. The host
// flushes each chunk as it's produced (true streaming). Lowered to CJS for the
// js-runner; served on the resident HTTP path (RUSM_SERVE_ROLE=http).
const enc = new TextEncoder();
module.exports.default = {
  fetch(_request) {
    let n = 0;
    return new Response(
      new ReadableStream({
        pull(controller) {
          if (n < 5) {
            controller.enqueue(enc.encode(`data: tick ${n}\n\n`));
            n++;
          } else {
            controller.close();
          }
        },
      }),
      { headers: { "content-type": "text/event-stream" } },
    );
  },
};
