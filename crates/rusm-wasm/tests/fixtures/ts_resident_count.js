// A resident TS HTTP handler proving statefulness: a module-scope counter that
// persists across requests because the instance is long-lived (a per-request
// instance would always say "hit #1"). Authored as the CJS the js-runner
// evaluates — the same `export default` shape, lowered to `module.exports.default`.
let hits = 0;
function handle(_request) {
  hits++;
  return new Response(`hit #${hits}\n`, {
    headers: { "content-type": "text/plain" },
  });
}
module.exports.default = handle;
