use rusm_observer::ProcessInfo;

/// One tick's worth of signals the runner records, produced by either the
/// synthetic source or a real engine (e.g. the spawn-storm engine).
///
/// `process_count` (and `running`/`waiting`/`total_memory_bytes`) are full,
/// authoritative totals; `processes` is only a capped *sample* for the observer
/// detail table — intentionally different scales.
#[derive(Debug, Clone, PartialEq)]
pub struct Sample {
    pub ops_per_sec: f64,
    pub process_count: u64,
    pub running: u64,
    pub waiting: u64,
    pub total_memory_bytes: u64,
    pub latencies_ns: Vec<u64>,
    pub processes: Vec<ProcessInfo>,
    pub scheduler_load: Vec<f32>,
}
