import React from 'react';
import type { CvFoldDto, ModelSignalPointDto } from '../../lib/types';

/** Small dependency-free charts (SVG and CSS) for the model views. */

interface BarItem {
  label: string;
  value: number;
  color?: string;
}

/** Labelled horizontal bars, scaled to the largest value. */
export const HorizontalBars: React.FC<{
  items: BarItem[];
  format?: (v: number) => string;
  testId: string;
  max?: number;
}> = ({ items, format = (v) => v.toFixed(3), testId, max }) => {
  const top = max ?? Math.max(...items.map((i) => i.value), 1e-9);
  return (
    <div className="space-y-1" data-testid={testId}>
      {items.map((item) => (
        <div key={item.label} className="flex items-center gap-2 text-[11px]" data-testid={`${testId}-row`}>
          <span className="w-36 shrink-0 truncate font-mono text-text-secondary" title={item.label}>{item.label}</span>
          <div className="h-3 flex-1 bg-surface-2">
            <div
              className="h-full"
              style={{ width: `${Math.max(1, (item.value / top) * 100)}%`, background: item.color ?? 'var(--color-accent)' }}
              data-testid={`${testId}-bar`}
            />
          </div>
          <span className="w-14 shrink-0 text-right font-mono text-text-primary">{format(item.value)}</span>
        </div>
      ))}
    </div>
  );
};

const W = 320;
const H = 130;
const PAD = { l: 34, r: 8, t: 8, b: 22 };

/** Per-fold accuracy against the majority-class baseline. */
export const FoldAccuracyChart: React.FC<{ folds: CvFoldDto[] }> = ({ folds }) => {
  const plotW = W - PAD.l - PAD.r;
  const plotH = H - PAD.t - PAD.b;
  const lo = Math.max(0, Math.floor((Math.min(...folds.map((f) => Math.min(f.accuracy, f.baseline_accuracy ?? f.accuracy))) - 0.05) * 10) / 10);
  const hi = Math.min(1, Math.ceil((Math.max(...folds.map((f) => Math.max(f.accuracy, f.baseline_accuracy ?? f.accuracy))) + 0.02) * 10) / 10);
  const y = (v: number) => PAD.t + plotH - ((v - lo) / Math.max(hi - lo, 1e-9)) * plotH;
  const group = plotW / folds.length;
  const bw = Math.min(14, group / 3);
  return (
    <svg viewBox={`0 0 ${W} ${H}`} className="w-full" role="img" aria-label="Accuracy per fold against baseline" data-testid="cv-accuracy-chart">
      {[lo, (lo + hi) / 2, hi].map((t) => (
        <g key={t}>
          <line x1={PAD.l} x2={W - PAD.r} y1={y(t)} y2={y(t)} stroke="var(--border-color)" strokeWidth="0.5" />
          <text x={PAD.l - 4} y={y(t) + 3} textAnchor="end" fontSize="8" fill="var(--text-muted)">{(t * 100).toFixed(0)}%</text>
        </g>
      ))}
      {folds.map((f, i) => {
        const cx = PAD.l + group * i + group / 2;
        return (
          <g key={f.fold}>
            <rect x={cx - bw - 1} y={y(f.accuracy)} width={bw} height={Math.max(0, PAD.t + plotH - y(f.accuracy))} fill="var(--color-accent)" data-testid={`cv-acc-bar-${f.fold}`}>
              <title>{`Fold ${f.fold} accuracy ${(f.accuracy * 100).toFixed(1)}%`}</title>
            </rect>
            {f.baseline_accuracy !== null && (
              <rect x={cx + 1} y={y(f.baseline_accuracy)} width={bw} height={Math.max(0, PAD.t + plotH - y(f.baseline_accuracy))} fill="var(--color-muted)" data-testid={`cv-base-bar-${f.fold}`}>
                <title>{`Fold ${f.fold} baseline ${(f.baseline_accuracy * 100).toFixed(1)}%`}</title>
              </rect>
            )}
            <text x={cx} y={H - 8} textAnchor="middle" fontSize="8" fill="var(--text-muted)">F{f.fold}</text>
          </g>
        );
      })}
    </svg>
  );
};

/** Per-fold return from trading the predicted signals (bars around a zero line). */
export const FoldReturnChart: React.FC<{ folds: CvFoldDto[] }> = ({ folds }) => {
  const values = folds.map((f) => f.trade_return_pct);
  const present = values.filter((v): v is number => typeof v === 'number');
  if (present.length === 0) {
    return <p className="text-[11px] text-text-muted" data-testid="cv-return-empty">No trade returns were recorded for these folds.</p>;
  }
  const plotW = W - PAD.l - PAD.r;
  const plotH = H - PAD.t - PAD.b;
  const hi = Math.max(0.5, ...present);
  const lo = Math.min(-0.5, ...present);
  const y = (v: number) => PAD.t + plotH - ((v - lo) / (hi - lo)) * plotH;
  const group = plotW / folds.length;
  const bw = Math.min(22, group * 0.6);
  return (
    <svg viewBox={`0 0 ${W} ${H}`} className="w-full" role="img" aria-label="Trade return per fold" data-testid="cv-return-chart">
      <line x1={PAD.l} x2={W - PAD.r} y1={y(0)} y2={y(0)} stroke="var(--text-muted)" strokeWidth="0.7" />
      <text x={PAD.l - 4} y={y(hi) + 3} textAnchor="end" fontSize="8" fill="var(--text-muted)">{hi.toFixed(1)}%</text>
      <text x={PAD.l - 4} y={y(lo) + 3} textAnchor="end" fontSize="8" fill="var(--text-muted)">{lo.toFixed(1)}%</text>
      {folds.map((f, i) => {
        const cx = PAD.l + group * i + group / 2;
        const v = f.trade_return_pct;
        return (
          <g key={f.fold}>
            {typeof v === 'number' && (
              <rect
                x={cx - bw / 2} y={Math.min(y(v), y(0))} width={bw} height={Math.max(1, Math.abs(y(v) - y(0)))}
                fill={v >= 0 ? 'var(--color-bull)' : 'var(--color-bear)'} data-testid={`cv-ret-bar-${f.fold}`}
              >
                <title>{`Fold ${f.fold} return ${v.toFixed(2)}%`}</title>
              </rect>
            )}
            <text x={cx} y={H - 8} textAnchor="middle" fontSize="8" fill="var(--text-muted)">F{f.fold}</text>
          </g>
        );
      })}
    </svg>
  );
};

const SIGNAL_COLOR: Record<string, string> = { '1': 'var(--color-bull)', '-1': 'var(--color-bear)', '0': 'var(--color-muted)' };

/** Close price as a line, with a coloured strip and markers where the model's signal changes. */
export const SignalChart: React.FC<{ points: ModelSignalPointDto[] }> = ({ points }) => {
  if (points.length < 2) return <p className="text-[11px] text-text-muted">Not enough history to chart.</p>;
  const w = 480;
  const h = 150;
  const stripH = 8;
  const padL = 40;
  const padR = 8;
  const padT = 8;
  const plotH = h - padT - stripH - 18;
  const closes = points.map((p) => p.close);
  const lo = Math.min(...closes);
  const hi = Math.max(...closes);
  const span = Math.max(hi - lo, 1e-9);
  const x = (i: number) => padL + (i / (points.length - 1)) * (w - padL - padR);
  const y = (v: number) => padT + plotH - ((v - lo) / span) * plotH;
  const line = points.map((p, i) => `${i === 0 ? 'M' : 'L'}${x(i).toFixed(1)},${y(p.close).toFixed(1)}`).join(' ');
  const stepW = (w - padL - padR) / (points.length - 1);
  const stripY = padT + plotH + 6;
  const markers = points.map((p, i) => ({ p, i })).filter(({ p, i }) => i === 0 || p.signal !== points[i - 1].signal);
  return (
    <svg viewBox={`0 0 ${w} ${h}`} className="w-full" role="img" aria-label="Price with model signals" data-testid="signal-chart">
      {[lo, (lo + hi) / 2, hi].map((t) => (
        <g key={t}>
          <line x1={padL} x2={w - padR} y1={y(t)} y2={y(t)} stroke="var(--border-color)" strokeWidth="0.5" />
          <text x={padL - 4} y={y(t) + 3} textAnchor="end" fontSize="8" fill="var(--text-muted)">{t.toFixed(1)}</text>
        </g>
      ))}
      <path d={line} fill="none" stroke="var(--text-primary)" strokeWidth="1.2" data-testid="signal-price-line" />
      {points.map((p, i) => (
        <rect key={p.time} x={x(i) - stepW / 2} y={stripY} width={stepW + 0.3} height={stripH} fill={SIGNAL_COLOR[String(p.signal)] ?? SIGNAL_COLOR['0']} opacity="0.7" />
      ))}
      {markers.map(({ p, i }) => {
        const cx = x(i);
        const cy = y(p.close);
        const kind = p.signal > 0 ? 'long' : p.signal < 0 ? 'short' : 'flat';
        const color = SIGNAL_COLOR[String(p.signal)] ?? SIGNAL_COLOR['0'];
        const shape = kind === 'long'
          ? <path d={`M${cx},${cy - 6} L${cx - 4},${cy + 1} L${cx + 4},${cy + 1} Z`} fill={color} />
          : kind === 'short'
            ? <path d={`M${cx},${cy + 6} L${cx - 4},${cy - 1} L${cx + 4},${cy - 1} Z`} fill={color} />
            : <circle cx={cx} cy={cy} r="2.5" fill={color} />;
        return (
          <g key={p.time} data-testid={`signal-marker-${kind}`}>
            {shape}
            <title>{`${new Date(p.time).toLocaleDateString()} ${kind} at ${p.close.toFixed(2)}${p.probability !== null ? ` (p ${(p.probability * 100).toFixed(0)}%)` : ''}`}</title>
          </g>
        );
      })}
      <text x={padL} y={h - 4} fontSize="8" fill="var(--text-muted)">{new Date(points[0].time).toLocaleDateString()}</text>
      <text x={w - padR} y={h - 4} textAnchor="end" fontSize="8" fill="var(--text-muted)">{new Date(points[points.length - 1].time).toLocaleDateString()}</text>
    </svg>
  );
};

export const ChartLegend: React.FC<{ items: { label: string; color: string }[] }> = ({ items }) => (
  <div className="flex flex-wrap gap-3 text-[10px] text-text-muted">
    {items.map((i) => (
      <span key={i.label} className="flex items-center gap-1"><span className="inline-block h-2 w-2" style={{ background: i.color }} />{i.label}</span>
    ))}
  </div>
);
