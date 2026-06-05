import { formatBytes, formatCount, formatDuration, formatRate } from '../format';
import type { Averages } from '../state';
import type { Frame } from '../types';

export type MetricMode = 'current' | 'average';

interface StatGridProps {
  frame: Frame | null;
  averages: Averages | null;
  mode: MetricMode;
}

type Hint = 'higher is better' | 'lower is better' | 'in-host, not OS' | 'session peak';

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

export function StatGrid({ frame, averages, mode }: StatGridProps) {
  // Use averages only in 'average' mode and only once a sample exists.
  const avg = mode === 'average' ? averages : null;
  const o = frame?.observer;
  const prefix = avg ? 'avg ' : '';

  return (
    <div className="stat-grid">
      <Stat
        label={`${prefix}throughput`}
        value={avg ? formatRate(avg.opsPerSec) : frame ? formatRate(frame.ops_per_sec) : '—'}
        hint="higher is better"
        accent
      />
      <Stat
        label="peak concurrent"
        value={frame ? formatCount(frame.peak_concurrent) : '—'}
        hint="session peak"
      />
      <Stat
        label={`${prefix}p50 latency`}
        value={avg ? formatDuration(avg.p50Ns) : frame ? formatDuration(frame.latency.p50_ns) : '—'}
        hint="lower is better"
      />
      <Stat
        label={`${prefix}p99 latency`}
        value={avg ? formatDuration(avg.p99Ns) : frame ? formatDuration(frame.latency.p99_ns) : '—'}
        hint="lower is better"
      />
      <Stat
        label={`${prefix}processes`}
        value={avg ? formatCount(avg.processCount) : o ? formatCount(o.process_count) : '—'}
        hint="in-host, not OS"
      />
      <Stat
        label={`${prefix}memory`}
        value={avg ? formatBytes(avg.memoryBytes) : o ? formatBytes(o.total_memory_bytes) : '—'}
        hint="lower is better"
      />
    </div>
  );
}
