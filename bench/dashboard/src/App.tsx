import { useEffect, useState } from 'react';
import { Chart } from './components/Chart';
import { ObserverPanel } from './components/ObserverPanel';
import { ScenarioInfo } from './components/ScenarioInfo';
import { ScenarioMenu } from './components/ScenarioMenu';
import { StatGrid, type MetricMode } from './components/StatGrid';
import {
  runCommand,
  setObserverDetailCommand,
  setResourceProfileCommand,
  stopCommand,
} from './protocol';
import { averages } from './state';
import { useNode } from './useNode';

const DEFAULT_URL = 'ws://127.0.0.1:4000';
const ACCENT = '#34d399';

export function App() {
  const { state, send, reset } = useNode(DEFAULT_URL);
  const [selected, setSelected] = useState<string | null>(null);
  const [detail, setDetail] = useState(true);
  const [mode, setMode] = useState<MetricMode>('current');
  const [profile, setProfile] = useState('balanced');

  useEffect(() => {
    if (!selected && state.scenarios.length > 0) setSelected(state.scenarios[0].id);
  }, [state.scenarios, selected]);

  const toggleDetail = (enabled: boolean) => {
    setDetail(enabled);
    send(setObserverDetailCommand(enabled));
  };

  const startRun = () => {
    if (!selected) return;
    reset(); // clear the previous run's frozen data before starting fresh
    send(runCommand(selected));
  };

  // A profile switch starts a new measurement regime: clear the previous run's
  // (possibly frozen) data so a stale peak/average from the old profile can't
  // linger on screen. The highlight follows the click even while stopped, when
  // there is no frame to read the active profile from.
  const switchProfile = (id: string) => {
    setProfile(id);
    reset();
    send(setResourceProfileCommand(id));
  };

  const hasData = state.frame !== null || state.history.length > 0;
  const selectedMeta = state.scenarios.find((s) => s.id === selected);
  const activeProfile = state.running ? (state.frame?.profile ?? profile) : profile;

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
            <button className="run" disabled={!selected || !state.connected} onClick={startRun}>
              Run
            </button>
            {state.running ? (
              <button className="stop" onClick={() => send(stopCommand())}>
                Stop
              </button>
            ) : (
              <button className="reset" disabled={!hasData} onClick={reset}>
                Reset
              </button>
            )}
          </div>
        </aside>

        <main className="main">
          <ScenarioInfo scenario={selectedMeta} />
          <div className="metrics-bar">
            <div className="seg-group">
              <span className="seg-label">resources</span>
              <div className="seg" role="group" aria-label="resource profile">
                {state.profiles.map((p) => (
                  <button
                    key={p.id}
                    className={activeProfile === p.id ? 'seg-on' : ''}
                    title={p.description}
                    onClick={() => switchProfile(p.id)}
                  >
                    {p.label}
                  </button>
                ))}
              </div>
            </div>
            <div className="seg-group">
              <span className="seg-label">metrics</span>
              <div className="seg" role="group" aria-label="metric mode">
                <button
                  className={mode === 'current' ? 'seg-on' : ''}
                  onClick={() => setMode('current')}
                >
                  Current
                </button>
                <button
                  className={mode === 'average' ? 'seg-on' : ''}
                  onClick={() => setMode('average')}
                >
                  Average
                </button>
              </div>
            </div>
          </div>
          <StatGrid
            frame={state.frame}
            averages={averages(state)}
            mode={mode}
            unit={selectedMeta?.unit}
          />
          <Chart
            data={state.history}
            label={
              selectedMeta?.unit === 'bytes' ? 'throughput (bytes/sec)' : 'throughput (ops/sec)'
            }
            color={ACCENT}
          />
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
