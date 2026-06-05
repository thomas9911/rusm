import type { Frame, ScenarioMeta, ServerMessage } from './types';

/** The dashboard's view of the node, derived purely from server messages. */
export interface DashboardState {
  connected: boolean;
  scenarios: ScenarioMeta[];
  running: boolean;
  scenario: string | null;
  frame: Frame | null;
  /** Rolling throughput history for the live chart (ops/sec per tick). */
  history: number[];
  error: string | null;
}

export const HISTORY_LIMIT = 240;

export function initialState(): DashboardState {
  return {
    connected: false,
    scenarios: [],
    running: false,
    scenario: null,
    frame: null,
    history: [],
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
  const history = frame.running ? [...state.history, frame.ops_per_sec].slice(-HISTORY_LIMIT) : [];
  return { ...state, running: frame.running, scenario: frame.scenario, frame, history };
}
