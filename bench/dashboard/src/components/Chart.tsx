import { useEffect, useRef } from 'react';
import uPlot from 'uplot';
import 'uplot/dist/uPlot.min.css';

interface ChartProps {
  data: number[];
  label: string;
  color: string;
}

const HEIGHT = 200;
const AXIS = '#7d8aa0';
const GRID = 'rgba(255,255,255,0.05)';

export function Chart({ data, label, color }: ChartProps) {
  const container = useRef<HTMLDivElement>(null);
  const plot = useRef<uPlot | null>(null);

  useEffect(() => {
    const el = container.current;
    if (!el) return;
    const opts: uPlot.Options = {
      width: el.clientWidth,
      height: HEIGHT,
      cursor: { show: false },
      legend: { show: false },
      scales: { x: { time: false } },
      axes: [
        { stroke: AXIS, grid: { stroke: GRID }, ticks: { stroke: GRID } },
        { stroke: AXIS, grid: { stroke: GRID }, ticks: { stroke: GRID } },
      ],
      series: [{}, { label, stroke: color, width: 2, fill: `${color}1f` }],
    };
    const u = new uPlot(opts, [[], []], el);
    plot.current = u;
    const onResize = () => u.setSize({ width: el.clientWidth, height: HEIGHT });
    window.addEventListener('resize', onResize);
    return () => {
      window.removeEventListener('resize', onResize);
      u.destroy();
      plot.current = null;
    };
  }, [label, color]);

  useEffect(() => {
    const xs = data.map((_, i) => i);
    plot.current?.setData([xs, data]);
  }, [data]);

  return (
    <div className="chart">
      <span className="chart-label">{label}</span>
      <div ref={container} />
    </div>
  );
}
