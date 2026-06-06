import { expect, test } from 'bun:test';
import {
  encodeCommand,
  parseServerMessage,
  runCommand,
  setObserverDetailCommand,
  setResourceProfileCommand,
  stopCommand,
} from './protocol';

test('parses known message types', () => {
  expect(parseServerMessage('{"type":"hello","scenarios":[]}')?.type).toBe('hello');
  expect(parseServerMessage('{"type":"error","message":"x"}')?.type).toBe('error');
});

test('rejects malformed json', () => {
  expect(parseServerMessage('{not json')).toBeNull();
});

test('rejects non-objects and unknown types', () => {
  expect(parseServerMessage('42')).toBeNull();
  expect(parseServerMessage('null')).toBeNull();
  expect(parseServerMessage('{"type":"nope"}')).toBeNull();
});

test('command builders produce tagged commands', () => {
  expect(runCommand('ping-pong')).toEqual({ type: 'run', scenario: 'ping-pong' });
  expect(stopCommand()).toEqual({ type: 'stop' });
  expect(setObserverDetailCommand(false)).toEqual({
    type: 'set_observer_detail',
    enabled: false,
  });
  expect(setResourceProfileCommand('max')).toEqual({
    type: 'set_resource_profile',
    profile: 'max',
  });
});

test('encodeCommand round-trips through the parser shape', () => {
  const json = encodeCommand(runCommand('connection-storm'));
  expect(JSON.parse(json)).toEqual({ type: 'run', scenario: 'connection-storm' });
});
