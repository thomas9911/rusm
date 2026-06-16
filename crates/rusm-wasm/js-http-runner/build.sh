#!/usr/bin/env bash
# Build the wizer-pre-initialized js-http-runner wasi:http component.
#
# Pipeline (see Cargo.toml header for the why):
#   1. cargo  → a wasm32-wasip1 **core module** (wasi-sdk compiles QuickJS's C)
#   2. wizer  → run `wizer_initialize` (boot QuickJS + bridge) and snapshot the warm
#               engine into the image, so each per-request instance starts warm
#   3. wasm-tools component new → re-wrap as a wasi:http component (preview1 adapter
#               pinned to wasmtime 45.0.1, the version rusm-wasm links — checked in)
# Output: ../runtimes/js_http_runner.wasm (embedded by rusm-wasm via include_bytes!).
set -euo pipefail
cd "$(dirname "$0")"

export CC_wasm32_wasip1="$HOME/.wasi-sdk/bin/clang"
export AR_wasm32_wasip1="$HOME/.wasi-sdk/bin/llvm-ar"
export CFLAGS_wasm32_wasip1="--sysroot=$HOME/.wasi-sdk/share/wasi-sysroot"

cargo build --release --target wasm32-wasip1
core="target/wasm32-wasip1/release/js_http_runner.wasm"

wizer "$core" -o target/js_http_runner.wizer.wasm \
  --init-func wizer_initialize --allow-wasi

wasm-tools component new target/js_http_runner.wizer.wasm \
  --adapt "wasi_snapshot_preview1=wasi_snapshot_preview1.reactor.wasm" \
  -o ../runtimes/js_http_runner.wasm

echo "built ../runtimes/js_http_runner.wasm ($(wc -c < ../runtimes/js_http_runner.wasm) bytes)"
