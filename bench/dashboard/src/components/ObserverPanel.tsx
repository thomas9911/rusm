import { formatBytes, formatCount, formatPercent } from '../format';
import type { ObserverSnapshot } from '../types';

interface ObserverPanelProps {
  observer: ObserverSnapshot | null;
  detail: boolean;
  onToggleDetail: (enabled: boolean) => void;
  /** The node's Wasm pool capacity (reserved ceiling), shown next to live usage. */
  capacity?: number;
}

export function ObserverPanel({ observer, detail, onToggleDetail, capacity }: ObserverPanelProps) {
  const processes = observer?.processes ?? [];
  const schedulerLoad = observer?.scheduler_load ?? [];
  const hasSchedulerLoad = schedulerLoad.some((l) => l > 0);

  return (
    <section className="observer">
      <header className="observer-head">
        <h2>Observer</h2>
        <label className="toggle">
          <input
            type="checkbox"
            checked={detail}
            onChange={(e) => onToggleDetail(e.target.checked)}
          />
          per-instance detail
        </label>
      </header>
      <p className="observer-note">
        A process is a lightweight <strong>in-host</strong> actor — one isolated Wasm instance, not
        an OS process. Tens of thousands run over a handful of OS threads (the schedulers).
      </p>

      {/* Real aggregates from the runtime — the live signal for every scenario. */}
      <div className="observer-aggregates">
        <span>
          <strong>{formatCount(observer?.process_count ?? 0)}</strong> live processes
        </span>
        <span>
          <strong>{formatCount(observer?.messages_total ?? 0)}</strong> ops this run
        </span>
        <span>
          <strong>{schedulerLoad.length}</strong> schedulers
        </span>
        {capacity ? (
          <span title="Reserved Wasm-instance pool ceiling (live usage is shown left)">
            <strong>{formatCount(capacity)}</strong> Wasm pool cap
          </span>
        ) : null}
      </div>

      {hasSchedulerLoad && (
        <div className="schedulers">
          {schedulerLoad.map((load, i) => (
            <div key={i} className="sched" title={`scheduler ${i}: ${formatPercent(load)}`}>
              <div className="sched-fill" style={{ height: formatPercent(load) }} />
            </div>
          ))}
        </div>
      )}

      {!detail ? (
        <p className="observer-empty">Per-instance detail is off — aggregates only.</p>
      ) : processes.length > 0 ? (
        <table className="process-table">
          <thead>
            <tr>
              <th>pid</th>
              <th>status</th>
              <th>mailbox</th>
              <th>memory</th>
              <th>reductions</th>
            </tr>
          </thead>
          <tbody>
            {processes.map((p) => (
              <tr key={p.id}>
                <td>{p.id}</td>
                <td>
                  <span className={`status status--${p.status}`}>{p.status}</span>
                </td>
                <td>{p.mailbox_depth}</td>
                <td>{formatBytes(p.memory_bytes)}</td>
                <td>{formatCount(p.reductions)}</td>
              </tr>
            ))}
          </tbody>
        </table>
      ) : (
        // Real scenarios: RUSM intentionally doesn't track per-process memory /
        // reductions the way the BEAM does, so there's no per-instance table to
        // fabricate — the aggregates above (and the throughput) are the real signal.
        <p className="observer-empty">
          This is a <strong>real</strong> run — the aggregates above are live from the runtime. RUSM
          doesn't track per-process memory or reduction counts (it's not the BEAM), so the
          per-instance table is modelled only for synthetic scenarios like distributed fan-out.
        </p>
      )}
    </section>
  );
}
