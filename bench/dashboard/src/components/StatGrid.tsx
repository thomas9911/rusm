import { formatBytes, formatCount, formatDuration, formatThroughput } from '../format';
import type { Averages } from '../state';
import type { Frame } from '../types';

export type MetricMode = 'current' | 'average';

interface StatGridProps {
  frame: Frame | null;
  averages: Averages | null;
  mode: MetricMode;
  /** The active scenario's throughput unit (count vs byte rate). */
  unit?: 'count' | 'bytes';
  /** Exactly what the throughput counts (shown as the throughput stat's tooltip). */
  opsLabel?: string;
  /** What the latency measures (e.g. "round-trip"); null → the scenario has no
   *  meaningful per-op latency, so the latency stats read "n/a". */
  latencyLabel?: string | null;
}

type Hint = 'higher is better' | 'lower is better' | 'in-host, not OS' | 'session peak';

function Stat({
  label,
  value,
  hint,
  accent,
  title,
}: {
  label: string;
  value: string;
  hint: Hint;
  accent?: boolean;
  /** Optional hover text — e.g. the precise definition of what the number counts. */
  title?: string;
}) {
  const tone =
    hint === 'lower is better' ? 'lower' : hint === 'higher is better' ? 'higher' : 'info';
  return (
    <div className={`stat ${accent ? 'stat--accent' : ''}`} title={title}>
      <span className="stat-value">{value}</span>
      <span className="stat-label">{label}</span>
      <span className={`stat-hint stat-hint--${tone}`}>{hint}</span>
    </div>
  );
}

export function StatGrid({
  frame,
  averages,
  mode,
  unit = 'count',
  opsLabel,
  latencyLabel,
}: StatGridProps) {
  // Use averages only in 'average' mode and only once a sample exists.
  const avg = mode === 'average' ? averages : null;
  const o = frame?.observer;
  const prefix = avg ? 'avg ' : '';
  // Name what the latency measures ("p50 round-trip"); "n/a" when the scenario has none.
  const hasLatency = latencyLabel != null;
  const noun = hasLatency ? latencyLabel : 'latency';
  const p50Value = !hasLatency
    ? 'n/a'
    : avg
      ? formatDuration(avg.p50Ns)
      : frame
        ? formatDuration(frame.latency.p50_ns)
        : '—';
  const p99Value = !hasLatency
    ? 'n/a'
    : avg
      ? formatDuration(avg.p99Ns)
      : frame
        ? formatDuration(frame.latency.p99_ns)
        : '—';

  return (
    <div className="stat-grid">
      <Stat
        label={`${prefix}throughput`}
        value={
          avg
            ? formatThroughput(avg.opsPerSec, unit)
            : frame
              ? formatThroughput(frame.ops_per_sec, unit)
              : '—'
        }
        hint="higher is better"
        accent
        title={opsLabel}
      />
      <Stat
        label="peak concurrent"
        value={frame ? formatCount(frame.peak_concurrent) : '—'}
        hint="session peak"
      />
      <Stat
        label={`${prefix}p50 ${noun}`}
        value={p50Value}
        hint={hasLatency ? 'lower is better' : 'session peak'}
        title={hasLatency ? `steady-state ${latencyLabel} latency (warm-up excluded)` : undefined}
      />
      <Stat
        label={`${prefix}p99 ${noun}`}
        value={p99Value}
        hint={hasLatency ? 'lower is better' : 'session peak'}
        title={hasLatency ? `steady-state ${latencyLabel} latency (warm-up excluded)` : undefined}
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
