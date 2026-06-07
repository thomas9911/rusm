// Pure display formatters for the dashboard.

/** Formats a nanosecond duration with a sensible unit. */
export function formatDuration(ns: number): string {
  if (ns < 1_000) return `${Math.round(ns)} ns`;
  if (ns < 1_000_000) return `${(ns / 1_000).toFixed(1)} µs`;
  if (ns < 1_000_000_000) return `${(ns / 1_000_000).toFixed(1)} ms`;
  return `${(ns / 1_000_000_000).toFixed(2)} s`;
}

/** Formats a per-second rate compactly (e.g. `312.0k/s`, `1.20M/s`). */
export function formatRate(perSec: number): string {
  return `${formatCount(perSec)}/s`;
}

/**
 * Formats a per-second **byte** rate with data-rate units (e.g. `812.0 MB/s`,
 * `15.70 GB/s`) — decimal (1000-based) MB/GB so the number matches "bytes/sec",
 * and so a billions-of-bytes rate never reads as a bare `17.5B/s`.
 */
export function formatByteRate(bytesPerSec: number): string {
  if (bytesPerSec < 1_000) return `${Math.round(bytesPerSec)} B/s`;
  if (bytesPerSec < 1_000_000) return `${(bytesPerSec / 1_000).toFixed(1)} KB/s`;
  if (bytesPerSec < 1_000_000_000) return `${(bytesPerSec / 1_000_000).toFixed(1)} MB/s`;
  return `${(bytesPerSec / 1_000_000_000).toFixed(2)} GB/s`;
}

/** Formats a throughput value per its metric unit (count vs byte rate). */
export function formatThroughput(perSec: number, unit: 'count' | 'bytes'): string {
  return unit === 'bytes' ? formatByteRate(perSec) : formatRate(perSec);
}

/** Formats a large count compactly (e.g. `312.0k`, `1.20M`, `42`). */
export function formatCount(value: number): string {
  if (value < 1_000) return `${Math.round(value)}`;
  if (value < 1_000_000) return `${(value / 1_000).toFixed(1)}k`;
  if (value < 1_000_000_000) return `${(value / 1_000_000).toFixed(2)}M`;
  return `${(value / 1_000_000_000).toFixed(2)}B`;
}

/** Formats a byte count in binary units. */
export function formatBytes(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KiB`;
  if (bytes < 1024 * 1024 * 1024) return `${(bytes / (1024 * 1024)).toFixed(1)} MiB`;
  return `${(bytes / (1024 * 1024 * 1024)).toFixed(2)} GiB`;
}

/** Formats a 0..1 load fraction as a percentage. */
export function formatPercent(fraction: number): string {
  return `${Math.round(fraction * 100)}%`;
}
