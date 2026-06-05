import { memo } from 'react';
import type { ScenarioMeta } from '../types';

interface ScenarioMenuProps {
  scenarios: ScenarioMeta[];
  active: string | null;
  onPick: (id: string) => void;
}

// Memoized: the menu is static during a run, so it skips the per-tick re-render.
export const ScenarioMenu = memo(function ScenarioMenu({
  scenarios,
  active,
  onPick,
}: ScenarioMenuProps) {
  return (
    <nav className="scenario-menu">
      {scenarios.map((s) => (
        <button
          key={s.id}
          className={`scenario ${active === s.id ? 'scenario--active' : ''}`}
          onClick={() => onPick(s.id)}
          title={s.description}
        >
          <span className="scenario-label">{s.label}</span>
          <span className="scenario-phase">phase {s.real_after_phase}</span>
        </button>
      ))}
    </nav>
  );
});
