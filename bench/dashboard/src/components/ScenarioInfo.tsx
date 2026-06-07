import hljs from 'highlight.js/lib/core';
import rust from 'highlight.js/lib/languages/rust';
import type { ScenarioMeta } from '../types';

hljs.registerLanguage('rust', rust);

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
      {scenario.source && (
        <details className="scenario-code">
          <summary>
            How it's built — the engine code
            {scenario.source_file && <span className="code-file">{scenario.source_file}</span>}
          </summary>
          <pre>
            <code
              className="hljs language-rust"
              dangerouslySetInnerHTML={{
                __html: hljs.highlight(scenario.source, { language: 'rust' }).value,
              }}
            />
          </pre>
        </details>
      )}
    </section>
  );
}
