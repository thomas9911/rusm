use crate::protocol::Frame;

/// Formats a frame as a single-line terminal status, e.g. for `rusm-bench run`.
pub fn summarize_frame(frame: &Frame) -> String {
    let scenario = frame.scenario.as_deref().unwrap_or("idle");
    format!(
        "[{scenario}] {ops:>12.0} ops/s  peak {peak:>7}  p50 {p50:>7}µs  p99 {p99:>7}µs  procs {procs}",
        ops = frame.ops_per_sec,
        peak = frame.peak_concurrent,
        p50 = frame.latency.p50_ns / 1_000,
        p99 = frame.latency.p99_ns / 1_000,
        procs = frame.observer.process_count,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Runner, RunnerConfig, Scenario};

    #[test]
    fn summarizes_a_running_frame() {
        let mut runner = Runner::new(RunnerConfig::default());
        runner.start_synthetic(Scenario::DistributedFanout);
        let line = summarize_frame(&runner.tick(100));
        assert!(line.contains("distributed-fanout"));
        assert!(line.contains("ops/s"));
        assert!(line.contains("p99"));
    }

    #[test]
    fn summarizes_idle_as_idle() {
        let mut runner = Runner::new(RunnerConfig::default());
        assert!(summarize_frame(&runner.tick(0)).contains("idle"));
    }
}
