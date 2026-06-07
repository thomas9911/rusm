# `http_bench` — HTTP serving stress benchmark

Measures the **real** throughput + latency of serving a WASM component as an HTTP
handler, against a **bare-hyper baseline** (the same server loop returning a static
response, no Wasm) so the sandbox's cost is explicit — the number is *earned*, not
asserted.

```sh
# release, for real numbers:  [seconds] [keep-alive clients]
cargo run --release -p rusm-bench --example http_bench -- 5 64
```

Representative output (loopback, everyday machine load):

```
HTTP stress: 64 keep-alive clients, 5s each

WASM component (instance-per-request):
  50422 req/sec   latency p50 1247.8µs  p99 2098.8µs

bare hyper (no Wasm, baseline):
  198170 req/sec   latency p50 319.1µs  p99 524.8µs

sandbox overhead: 3.9x fewer req/s, +928.7µs p50
```

## Reading it

- The WASM server runs **one fresh, sandboxed component instance per request** (a
  `wstd` `wasi:http` component — `tests/fixtures/http_hello`). Total isolation
  between requests; a trap fails just that request.
- The ~4× gap and the +~0.9ms p50 are **per-request instantiation** — exactly the
  cost a **warm-instance pool** would amortize. The design left that as a
  *measure-first* call; this benchmark is the measurement that justifies it.
- Each client holds one keep-alive connection and pipelines requests; the client
  reads full responses (Content-Length *or* chunked) so the connection stays in
  sync. `TCP_NODELAY` on both ends.

Numbers scale with free CPU and the client count. SSE and WS benchmarks
(`sse-fanout`, `ws-echo`) follow as those serving paths land.
