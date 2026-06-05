import type { Frame, ScenarioMeta, ServerMessage } from './types';

/** Running sums over a run, for computing averages. */
export interface Totals {
  samples: number;
  opsPerSec: number;
  p50Ns: number;
  p99Ns: number;
  processCount: number;
  memoryBytes: number;
}

/** Averaged metrics over the current run, or null before any sample. */
export interface Averages {
  opsPerSec: number;
  p50Ns: number;
  p99Ns: number;
  processCount: number;
  memoryBytes: number;
}

/** The dashboard's view of the node, derived purely from server messages. */
export interface DashboardState {
  connected: boolean;
  scenarios: ScenarioMeta[];
  running: boolean;
  scenario: string | null;
  frame: Frame | null;
  /** Rolling throughput history for the live chart (ops/sec per tick). */
  history: number[];
  totals: Totals;
  error: string | null;
}

export const HISTORY_LIMIT = 240;

const ZERO_TOTALS: Totals = {
  samples: 0,
  opsPerSec: 0,
  p50Ns: 0,
  p99Ns: 0,
  processCount: 0,
  memoryBytes: 0,
};

export function initialState(): DashboardState {
  return {
    connected: false,
    scenarios: [],
    running: false,
    scenario: null,
    frame: null,
    history: [],
    totals: ZERO_TOTALS,
    error: null,
  };
}

export function setConnected(state: DashboardState, connected: boolean): DashboardState {
  return { ...state, connected };
}

/** Folds a server message into the state. Pure — easy to test and reason about. */
export function applyMessage(state: DashboardState, message: ServerMessage): DashboardState {
  switch (message.type) {
    case 'hello':
      return { ...state, scenarios: message.scenarios };
    case 'error':
      return { ...state, error: message.message };
    case 'tick':
      return applyTick(state, message.frame);
  }
}

function applyTick(state: DashboardState, frame: Frame): DashboardState {
  // After Stop the node keeps emitting idle frames. Keep the last run's data on
  // screen — only flip the running flag — until the user explicitly resets.
  if (!frame.running) {
    return { ...state, running: false };
  }
  const o = frame.observer;
  return {
    ...state,
    running: true,
    scenario: frame.scenario,
    frame,
    history: [...state.history, frame.ops_per_sec].slice(-HISTORY_LIMIT),
    totals: {
      samples: state.totals.samples + 1,
      opsPerSec: state.totals.opsPerSec + frame.ops_per_sec,
      p50Ns: state.totals.p50Ns + frame.latency.p50_ns,
      p99Ns: state.totals.p99Ns + frame.latency.p99_ns,
      processCount: state.totals.processCount + o.process_count,
      memoryBytes: state.totals.memoryBytes + o.total_memory_bytes,
    },
  };
}

/** Mean of each accumulated metric over the run, or `null` before any sample. */
export function averages(state: DashboardState): Averages | null {
  const { samples } = state.totals;
  if (samples === 0) return null;
  return {
    opsPerSec: state.totals.opsPerSec / samples,
    p50Ns: state.totals.p50Ns / samples,
    p99Ns: state.totals.p99Ns / samples,
    processCount: state.totals.processCount / samples,
    memoryBytes: state.totals.memoryBytes / samples,
  };
}

/** Clears the displayed run data — the "Reset" action. */
export function resetData(state: DashboardState): DashboardState {
  return {
    ...state,
    running: false,
    scenario: null,
    frame: null,
    history: [],
    totals: ZERO_TOTALS,
  };
}
