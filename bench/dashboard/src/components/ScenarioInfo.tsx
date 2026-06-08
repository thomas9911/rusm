import hljs from 'highlight.js/lib/core';
import rust from 'highlight.js/lib/languages/rust';
import typescript from 'highlight.js/lib/languages/typescript';
import type { ScenarioMeta } from '../types';

hljs.registerLanguage('rust', rust);
hljs.registerLanguage('typescript', typescript);

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
      {scenario.source &&
        (() => {
          // Serving scenarios ship the guest handler (TypeScript or Rust); the rest
          // ship the Rust engine. Highlight by the file's extension.
          const isTs = scenario.source_file?.endsWith('.ts') ?? false;
          const lang = isTs ? 'typescript' : 'rust';
          return (
            <details className="scenario-code">
              <summary>
                How it's built — {isTs ? 'the TypeScript handler' : 'the code'}
                {scenario.source_file && <span className="code-file">{scenario.source_file}</span>}
              </summary>
              <pre>
                <code
                  className={`hljs language-${lang}`}
                  dangerouslySetInnerHTML={{
                    __html: hljs.highlight(scenario.source, { language: lang }).value,
                  }}
                />
              </pre>
            </details>
          );
        })()}
    </section>
  );
}
