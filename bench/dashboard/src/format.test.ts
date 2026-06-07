import { expect, test } from 'bun:test';
import {
  formatByteRate,
  formatBytes,
  formatCount,
  formatDuration,
  formatPercent,
  formatRate,
  formatThroughput,
} from './format';

test('formatDuration picks a unit per magnitude', () => {
  expect(formatDuration(500)).toBe('500 ns');
  expect(formatDuration(1_500)).toBe('1.5 µs');
  expect(formatDuration(2_500_000)).toBe('2.5 ms');
  expect(formatDuration(3_000_000_000)).toBe('3.00 s');
});

test('formatCount compacts thousands, millions, billions', () => {
  expect(formatCount(42)).toBe('42');
  expect(formatCount(312_000)).toBe('312.0k');
  expect(formatCount(1_200_000)).toBe('1.20M');
  expect(formatCount(2_500_000_000)).toBe('2.50B');
});

test('formatRate appends per-second', () => {
  expect(formatRate(300_000)).toBe('300.0k/s');
});

test('formatByteRate uses data-rate units (so 17.5e9 reads as GB/s, not 17.5B/s)', () => {
  expect(formatByteRate(800)).toBe('800B/s');
  expect(formatByteRate(12_300)).toBe('12.3KB/s');
  expect(formatByteRate(812_000_000)).toBe('812.0MB/s');
  expect(formatByteRate(17_500_000_000)).toBe('17.50GB/s');
});

test('formatThroughput switches on unit', () => {
  expect(formatThroughput(300_000, 'count')).toBe('300.0k/s');
  expect(formatThroughput(17_500_000_000, 'bytes')).toBe('17.50GB/s');
});

test('formatBytes uses binary units', () => {
  expect(formatBytes(512)).toBe('512 B');
  expect(formatBytes(2048)).toBe('2.0 KiB');
  expect(formatBytes(5 * 1024 * 1024)).toBe('5.0 MiB');
  expect(formatBytes(3 * 1024 * 1024 * 1024)).toBe('3.00 GiB');
});

test('formatPercent rounds a fraction', () => {
  expect(formatPercent(0)).toBe('0%');
  expect(formatPercent(0.426)).toBe('43%');
  expect(formatPercent(1)).toBe('100%');
});
