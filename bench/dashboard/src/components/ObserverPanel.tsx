import { formatBytes, formatCount, formatPercent } from '../format';
import type { ObserverSnapshot } from '../types';

interface ObserverPanelProps {
  observer: ObserverSnapshot | null;
  detail: boolean;
  onToggleDetail: (enabled: boolean) => void;
}

export function ObserverPanel({ observer, detail, onToggleDetail }: ObserverPanelProps) {
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

      <div className="schedulers">
        {(observer?.scheduler_load ?? []).map((load, i) => (
          <div key={i} className="sched" title={`scheduler ${i}: ${formatPercent(load)}`}>
            <div className="sched-fill" style={{ height: formatPercent(load) }} />
          </div>
        ))}
      </div>

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
          {(observer?.processes ?? []).map((p) => (
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
          {detail && (observer?.processes.length ?? 0) === 0 && (
            <tr>
              <td colSpan={5} className="muted">
                no processes
              </td>
            </tr>
          )}
          {!detail && (
            <tr>
              <td colSpan={5} className="muted">
                detail off — aggregates only
              </td>
            </tr>
          )}
        </tbody>
      </table>
    </section>
  );
}
