import type { ScenarioMeta } from '../types';

interface ScenarioInfoProps {
  scenario: ScenarioMeta | undefined;
}

export function ScenarioInfo({ scenario }: ScenarioInfoProps) {
  if (!scenario) {
    return (
      <section className="scenario-info muted">
        Connecting to a node — pick a scenario to see what it measures.
      </section>
    );
  }
  return (
    <section className="scenario-info">
      <div className="scenario-info-head">
        <h2>{scenario.label}</h2>
        <span
          className={`phase-badge ${scenario.real ? 'phase-badge--live' : ''}`}
          title={scenario.real ? 'Driven by the real runtime' : 'Synthetic until its phase lands'}
        >
          {scenario.real ? 'live data' : `synthetic · real from phase ${scenario.real_after_phase}`}
        </span>
      </div>
      <p className="scenario-info-desc">{scenario.description}</p>
      <ul className="scenario-info-details">
        {scenario.details.map((detail, i) => (
          <li key={i}>{detail}</li>
        ))}
      </ul>
    </section>
  );
}
