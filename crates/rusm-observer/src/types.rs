use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProcessStatus {
    Running,
    Waiting,
    Sleeping,
    Finished,
    Crashed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProcessInfo {
    pub id: u64,
    pub name: Option<String>,
    pub status: ProcessStatus,
    pub mailbox_depth: u32,
    pub memory_bytes: u64,
    pub reductions: u64,
}

/// A point-in-time view of a running node, sent to attached observers (the
/// dashboard, the `rusm attach` REPL). `processes` is the per-instance detail
/// table; it is empty when detail sampling is disabled (see [`super::Observer`]).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ObserverSnapshot {
    pub uptime_ms: u64,
    pub process_count: usize,
    pub running: usize,
    pub waiting: usize,
    pub scheduler_load: Vec<f32>,
    pub total_memory_bytes: u64,
    pub spawned_total: u64,
    pub finished_total: u64,
    pub messages_total: u64,
    pub processes: Vec<ProcessInfo>,
}
