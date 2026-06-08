use std::time::Instant;

use rusm_otp::{ProcessHandle, Runtime};
use rusm_wasm::WasmRuntime;

use crate::sample::Sample;

/// Bytes per streamed chunk. Each chunk the consumer reads is one `notify`, so
/// throughput = chunks/sec x this. 4 KiB fits comfortably in the guest's single
/// 64 KiB memory page (producer source + consumer sink don't overlap).
const CHUNK_SIZE: u64 = 4096;

/// A producer core module: receives the consumer's pid, opens a byte stream to it,
/// then writes 4 KiB chunks forever (the write parks under back-pressure when the
/// consumer is slow, so it can never outrun the reader). Stops if the stream closes.
const PRODUCER: &str = r#"(module
    (import "rusm" "stream_open" (func $open (param i64) (result i64)))
    (import "rusm" "stream_write" (func $write (param i64 i32 i32) (result i32)))
    (import "rusm" "receive" (func $receive (param i32 i32) (result i32)))
    (memory (export "memory") 1)
    (func (export "run")
        (local $consumer i64) (local $sid i64)
        (drop (call $receive (i32.const 8) (i32.const 8)))
        (local.set $consumer (i64.load (i32.const 8)))
        (local.set $sid (call $open (local.get $consumer)))
        (block $done (loop $more
            (br_if $done
                (i32.lt_s (call $write (local.get $sid) (i32.const 4096) (i32.const 4096))
                          (i32.const 0)))
            (br $more)))))"#;

/// A consumer core module: accepts the stream, then reads chunks to end-of-stream,
/// calling `notify` once per chunk so the harness can count throughput.
const CONSUMER: &str = r#"(module
    (import "rusm" "stream_accept" (func $accept (result i64)))
    (import "rusm" "stream_read" (func $read (param i64 i32 i32) (result i32)))
    (import "rusm" "notify" (func $notify))
    (memory (export "memory") 1)
    (func (export "run")
        (local $sid i64)
        (local.set $sid (call $accept))
        (block $done (loop $more
            (br_if $done
                (i32.lt_s (call $read (local.get $sid) (i32.const 0) (i32.const 4096))
                          (i32.const 0)))
            (call $notify)
            (br $more)))))"#;

/// A **real cross-process byte-streaming throughput** workload over `rusm-wasm`:
/// one or more producer→consumer pairs of WASM processes pipe 4 KiB chunks through
/// the runtime's Tokio-backpressured streams. [`tick`](Self::tick) reports
/// **sustained throughput in bytes/sec** (chunks consumed x [`CHUNK_SIZE`]) — how
/// fast one process can hand a byte stream to another, the metric that underpins
/// HTTP/WS/SSE bodies. Must be constructed inside a Tokio runtime.
pub struct StreamPipeEngine {
    runtime: Runtime,
    // Owns the engine + epoch ticker + the guest-progress counter.
    wasm: WasmRuntime,
    processes: Vec<ProcessHandle>,
    last_chunks: u64,
    last_at: Instant,
    scheduler_count: usize,
}

impl StreamPipeEngine {
    pub fn new(workers: usize, scheduler_count: usize) -> Self {
        let runtime = Runtime::new();
        let wasm = WasmRuntime::new(runtime.clone()).expect("wasm engine");
        let producer = wasm
            .prepare(&wasm.compile(PRODUCER).expect("compile producer"), "run")
            .expect("prepare producer");
        let consumer = wasm
            .prepare(&wasm.compile(CONSUMER).expect("compile consumer"), "run")
            .expect("prepare consumer");

        // One producer→consumer pair per worker — independent streams in parallel,
        // so aggregate throughput scales across cores.
        let pairs = workers.max(1);
        let mut processes = Vec::with_capacity(pairs * 2);
        for _ in 0..pairs {
            let consumer = wasm.spawn(&consumer);
            let producer = wasm.spawn(&producer);
            // Hand the producer its consumer's pid; it opens the stream and floods.
            runtime.send(producer.pid(), consumer.pid().raw().to_le_bytes().to_vec());
            processes.push(consumer);
            processes.push(producer);
        }

        Self {
            runtime,
            wasm,
            processes,
            last_chunks: 0,
            last_at: Instant::now(),
            scheduler_count,
        }
    }

    pub fn tick(&mut self) -> Sample {
        let now = Instant::now();
        let chunks = self.wasm.notifications();
        let dt = now
            .duration_since(self.last_at)
            .as_secs_f64()
            .max(f64::MIN_POSITIVE);
        // Throughput in bytes/sec: each consumed chunk is one notify of CHUNK_SIZE.
        let ops_per_sec = chunks.saturating_sub(self.last_chunks) as f64 * CHUNK_SIZE as f64 / dt;
        self.last_chunks = chunks;
        self.last_at = now;

        let process_count = self.runtime.process_count() as u64;
        Sample {
            ops_per_sec,
            process_count,
            running: process_count,
            waiting: 0,
            total_memory_bytes: 0,
            latencies_ns: Vec::new(),
            processes: Vec::new(),
            scheduler_load: vec![0.0; self.scheduler_count],
        }
    }
}

impl Drop for StreamPipeEngine {
    fn drop(&mut self) {
        for process in &self.processes {
            process.kill();
        }
        // Catch-all: abort every process still on the runtime (e.g. stream peers a
        // tracked pair spawned), so none outlive the engine into the next run.
        self.runtime.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn bytes_flow_from_producers_to_consumers() {
        let mut engine = StreamPipeEngine::new(2, 4);
        // Poll until a non-zero throughput is observed (robust to scheduling), but
        // bounded so a genuine stall fails instead of hanging.
        let mut sample = engine.tick();
        let mut flowed = false;
        for _ in 0..200 {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            sample = engine.tick();
            if sample.ops_per_sec > 0.0 {
                flowed = true;
                break;
            }
        }
        assert!(flowed, "bytes must stream from producers to consumers");
        assert_eq!(sample.scheduler_load.len(), 4);
        assert!(sample.process_count >= 2);
    }
}
