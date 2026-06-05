import { formatBytes, formatCount, formatDuration, formatRate } from '../format';
import type { Frame } from '../types';

interface StatGridProps {
  frame: Frame | null;
}

function Stat({ label, value, accent }: { label: string; value: string; accent?: boolean }) {
  return (
    <div className={`stat ${accent ? 'stat--accent' : ''}`}>
      <span className="stat-value">{value}</span>
      <span className="stat-label">{label}</span>
    </div>
  );
}

export function StatGrid({ frame }: StatGridProps) {
  const o = frame?.observer;
  return (
    <div className="stat-grid">
      <Stat label="throughput" value={frame ? formatRate(frame.ops_per_sec) : '—'} accent />
      <Stat label="peak concurrent" value={frame ? formatCount(frame.peak_concurrent) : '—'} />
      <Stat label="p50 latency" value={frame ? formatDuration(frame.latency.p50_ns) : '—'} />
      <Stat label="p99 latency" value={frame ? formatDuration(frame.latency.p99_ns) : '—'} />
      <Stat label="processes" value={o ? formatCount(o.process_count) : '—'} />
      <Stat label="memory" value={o ? formatBytes(o.total_memory_bytes) : '—'} />
    </div>
  );
}
