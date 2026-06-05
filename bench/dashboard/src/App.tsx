import { useEffect, useState } from 'react';
import { Chart } from './components/Chart';
import { ObserverPanel } from './components/ObserverPanel';
import { ScenarioMenu } from './components/ScenarioMenu';
import { StatGrid } from './components/StatGrid';
import { runCommand, setObserverDetailCommand, stopCommand } from './protocol';
import { useNode } from './useNode';

const DEFAULT_URL = 'ws://127.0.0.1:4000';
const ACCENT = '#34d399';

export function App() {
  const { state, send } = useNode(DEFAULT_URL);
  const [selected, setSelected] = useState<string | null>(null);
  const [detail, setDetail] = useState(true);

  useEffect(() => {
    if (!selected && state.scenarios.length > 0) setSelected(state.scenarios[0].id);
  }, [state.scenarios, selected]);

  const toggleDetail = (enabled: boolean) => {
    setDetail(enabled);
    send(setObserverDetailCommand(enabled));
  };

  return (
    <div className="app">
      <header className="topbar">
        <div className="brand">
          <span className="brand-mark">◇</span> RUSM <span className="brand-sub">live</span>
        </div>
        <div className="status">
          <span className={`dot ${state.connected ? 'dot--up' : 'dot--down'}`} />
          {state.connected ? 'connected' : 'reconnecting…'}
          {state.running && state.scenario && (
            <span className="running-pill">running · {state.scenario}</span>
          )}
        </div>
      </header>

      <div className="layout">
        <aside className="sidebar">
          <h2>Scenarios</h2>
          <ScenarioMenu scenarios={state.scenarios} active={selected} onPick={setSelected} />
          <div className="controls">
            <button
              className="run"
              disabled={!selected || !state.connected}
              onClick={() => selected && send(runCommand(selected))}
            >
              Run
            </button>
            <button className="stop" disabled={!state.running} onClick={() => send(stopCommand())}>
              Stop
            </button>
          </div>
        </aside>

        <main className="main">
          <StatGrid frame={state.frame} />
          <Chart data={state.history} label="throughput (ops/sec)" color={ACCENT} />
          <ObserverPanel
            observer={state.frame?.observer ?? null}
            detail={detail}
            onToggleDetail={toggleDetail}
          />
        </main>
      </div>
    </div>
  );
}
