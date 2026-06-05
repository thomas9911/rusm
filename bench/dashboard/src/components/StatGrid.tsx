import { formatBytes, formatCount, formatDuration, formatRate } from '../format';
import type { Frame } from '../types';

interface StatGridProps {
  frame: Frame | null;
}

type Hint = 'higher is better' | 'lower is better' | 'live count';

function Stat({
  label,
  value,
  hint,
  accent,
}: {
  label: string;
  value: string;
  hint: Hint;
  accent?: boolean;
}) {
  const tone =
    hint === 'lower is better' ? 'lower' : hint === 'higher is better' ? 'higher' : 'info';
  return (
    <div className={`stat ${accent ? 'stat--accent' : ''}`}>
      <span className="stat-value">{value}</span>
      <span className="stat-label">{label}</span>
      <span className={`stat-hint stat-hint--${tone}`}>{hint}</span>
    </div>
  );
}

export function StatGrid({ frame }: StatGridProps) {
  const o = frame?.observer;
  return (
    <div className="stat-grid">
      <Stat
        label="throughput"
        value={frame ? formatRate(frame.ops_per_sec) : '—'}
        hint="higher is better"
        accent
      />
      <Stat
        label="peak concurrent"
        value={frame ? formatCount(frame.peak_concurrent) : '—'}
        hint="higher is better"
      />
      <Stat
        label="p50 latency"
        value={frame ? formatDuration(frame.latency.p50_ns) : '—'}
        hint="lower is better"
      />
      <Stat
        label="p99 latency"
        value={frame ? formatDuration(frame.latency.p99_ns) : '—'}
        hint="lower is better"
      />
      <Stat label="processes" value={o ? formatCount(o.process_count) : '—'} hint="live count" />
      <Stat
        label="memory"
        value={o ? formatBytes(o.total_memory_bytes) : '—'}
        hint="lower is better"
      />
    </div>
  );
}
