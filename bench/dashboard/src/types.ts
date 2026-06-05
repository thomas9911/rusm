// Wire types — mirror the Rust `rusm-bench` protocol (serde tagged enums).

export interface LatencySnapshot {
  count: number;
  min_ns: number;
  max_ns: number;
  mean_ns: number;
  p50_ns: number;
  p95_ns: number;
  p99_ns: number;
}

export interface TimeSeriesSnapshot {
  points: number[];
  capacity: number;
}

export type ProcessStatus = 'running' | 'waiting' | 'sleeping' | 'finished' | 'crashed';

export interface ProcessInfo {
  id: number;
  name: string | null;
  status: ProcessStatus;
  mailbox_depth: number;
  memory_bytes: number;
  reductions: number;
}

export interface ObserverSnapshot {
  uptime_ms: number;
  process_count: number;
  running: number;
  waiting: number;
  scheduler_load: number[];
  total_memory_bytes: number;
  spawned_total: number;
  finished_total: number;
  messages_total: number;
  processes: ProcessInfo[];
}

export interface Frame {
  scenario: string | null;
  running: boolean;
  uptime_ms: number;
  ops_per_sec: number;
  peak_concurrent: number;
  latency: LatencySnapshot;
  throughput: TimeSeriesSnapshot;
  observer: ObserverSnapshot;
}

export interface ScenarioMeta {
  id: string;
  label: string;
  description: string;
  details: string[];
  real_after_phase: number;
}

export type ServerMessage =
  | { type: 'hello'; scenarios: ScenarioMeta[] }
  | { type: 'tick'; frame: Frame }
  | { type: 'error'; message: string };

export type ClientCommand =
  | { type: 'run'; scenario: string }
  | { type: 'stop' }
  | { type: 'set_observer_detail'; enabled: boolean };
