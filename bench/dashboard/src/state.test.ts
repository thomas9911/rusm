import { expect, test } from 'bun:test';
import {
  applyMessage,
  averages,
  HISTORY_LIMIT,
  initialState,
  resetData,
  setConnected,
} from './state';
import type { Frame, ServerMessage } from './types';

function frame(overrides: Partial<Frame> = {}): Frame {
  return {
    scenario: 'connection-storm',
    running: true,
    uptime_ms: 0,
    ops_per_sec: 1000,
    peak_concurrent: 0,
    profile: 'balanced',
    latency: { count: 0, min_ns: 0, max_ns: 0, mean_ns: 0, p50_ns: 0, p95_ns: 0, p99_ns: 0 },
    throughput: { points: [], capacity: 120 },
    observer: {
      uptime_ms: 0,
      process_count: 0,
      running: 0,
      waiting: 0,
      scheduler_load: [],
      total_memory_bytes: 0,
      spawned_total: 0,
      finished_total: 0,
      messages_total: 0,
      processes: [],
    },
    ...overrides,
  };
}

const tick = (f: Frame): ServerMessage => ({ type: 'tick', frame: f });

test('initial state is empty and disconnected', () => {
  const s = initialState();
  expect(s.connected).toBe(false);
  expect(s.scenarios).toEqual([]);
  expect(s.frame).toBeNull();
  expect(s.history).toEqual([]);
});

test('setConnected toggles connection without touching the rest', () => {
  const s = setConnected(initialState(), true);
  expect(s.connected).toBe(true);
  expect(s.frame).toBeNull();
});

test('hello populates the scenario and profile menus', () => {
  const s = applyMessage(initialState(), {
    type: 'hello',
    scenarios: [
      {
        id: 'ping-pong',
        label: 'Ping',
        description: 'd',
        details: ['x'],
        real_after_phase: 5,
        real: true,
        unit: 'count',
      },
    ],
    profiles: [{ id: 'balanced', label: 'Balanced', description: 'd' }],
    instance_capacity: 1024,
  });
  expect(s.scenarios).toHaveLength(1);
  expect(s.profiles).toHaveLength(1);
  expect(s.instanceCapacity).toBe(1024);
});

test('error records the message', () => {
  const s = applyMessage(initialState(), { type: 'error', message: 'boom' });
  expect(s.error).toBe('boom');
});

test('a running tick appends throughput history', () => {
  let s = applyMessage(initialState(), tick(frame({ ops_per_sec: 100 })));
  s = applyMessage(s, tick(frame({ ops_per_sec: 200 })));
  expect(s.running).toBe(true);
  expect(s.history).toEqual([100, 200]);
  expect(s.frame?.ops_per_sec).toBe(200);
});

test('history is capped at the limit', () => {
  let s = initialState();
  for (let i = 0; i < HISTORY_LIMIT + 50; i++) {
    s = applyMessage(s, tick(frame({ ops_per_sec: i })));
  }
  expect(s.history).toHaveLength(HISTORY_LIMIT);
  expect(s.history[s.history.length - 1]).toBe(HISTORY_LIMIT + 49);
});

test('an idle tick keeps the last run on screen, only flipping running off', () => {
  let s = applyMessage(initialState(), tick(frame({ ops_per_sec: 100 })));
  const before = s.frame;
  s = applyMessage(s, tick(frame({ running: false, scenario: null, ops_per_sec: 0 })));
  expect(s.running).toBe(false);
  // Data is preserved (frozen) until an explicit reset.
  expect(s.history).toEqual([100]);
  expect(s.frame).toBe(before);
  expect(s.scenario).toBe('connection-storm');
});

test('a fresh run after stop + reset shows throughput again', () => {
  // The exact dashboard flow: run → stop → reset → run. The second run must
  // populate throughput, not stay at zero.
  let s = applyMessage(initialState(), tick(frame({ ops_per_sec: 100 })));
  s = applyMessage(s, tick(frame({ running: false, scenario: null, ops_per_sec: 0 }))); // stop
  s = resetData(s); // reset
  s = applyMessage(s, tick(frame({ ops_per_sec: 500 }))); // run again
  expect(s.running).toBe(true);
  expect(s.frame?.ops_per_sec).toBe(500);
  expect(s.history).toEqual([500]);
  expect(averages(s)!.opsPerSec).toBe(500);
});

test('resetData clears the displayed run and the averages', () => {
  let s = applyMessage(initialState(), tick(frame({ ops_per_sec: 100 })));
  s = resetData(s);
  expect(s.running).toBe(false);
  expect(s.frame).toBeNull();
  expect(s.history).toEqual([]);
  expect(s.scenario).toBeNull();
  expect(averages(s)).toBeNull();
});

test('averages is null before any sample', () => {
  expect(averages(initialState())).toBeNull();
});

test('averages are the mean of the run', () => {
  let s = applyMessage(initialState(), tick(frame({ ops_per_sec: 100 })));
  s = applyMessage(s, tick(frame({ ops_per_sec: 300 })));
  const avg = averages(s);
  expect(avg).not.toBeNull();
  expect(avg!.opsPerSec).toBe(200);
});

test('idle ticks do not contribute to averages', () => {
  let s = applyMessage(initialState(), tick(frame({ ops_per_sec: 100 })));
  s = applyMessage(s, tick(frame({ running: false, ops_per_sec: 0 })));
  // Still just the one running sample.
  expect(averages(s)!.opsPerSec).toBe(100);
});
