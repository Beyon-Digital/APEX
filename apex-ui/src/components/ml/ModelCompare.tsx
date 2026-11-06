import React, { useMemo, useState } from 'react';
import type { ModelDetailDto } from '../../lib/types';
import {
  SERIES_COLORS, algorithmLabel, baselineAccuracy, bestIndexes, displayName, fmtValue, labelText, meanOf, num, paramRows, pct, signedPct, tradeModeLabel,
} from './modelUtils';

export interface ModelCompareProps {
  models: ModelDetailDto[];
  onClose: () => void;
  onOpen: (modelId: string) => void;
}

interface MetricDef {
  id: string;
  label: string;
  higherIsBetter: boolean;
  value: (m: ModelDetailDto) => number | null;
  show: (v: number | null) => string;
}

const METRICS: MetricDef[] = [
  { id: 'accuracy', label: 'Accuracy', higherIsBetter: true, value: (m) => m.metrics.accuracy ?? null, show: (v) => pct(v) },
  { id: 'edge', label: 'Edge over baseline (pts)', higherIsBetter: true, value: (m) => { const b = baselineAccuracy(m.cv_folds); const a = m.metrics.accuracy; return b === null || typeof a !== 'number' ? null : (a - b) * 100; }, show: (v) => (v === null ? 'n/a' : `${v >= 0 ? '+' : ''}${v.toFixed(1)}`) },
  { id: 'f1', label: 'F1', higherIsBetter: true, value: (m) => m.metrics.f1 ?? null, show: (v) => num(v, 3) },
  { id: 'precision', label: 'Precision', higherIsBetter: true, value: (m) => m.metrics.precision ?? null, show: (v) => num(v, 3) },
  { id: 'recall', label: 'Recall', higherIsBetter: true, value: (m) => m.metrics.recall ?? null, show: (v) => num(v, 3) },
  { id: 'cv-accuracy', label: 'Mean fold accuracy', higherIsBetter: true, value: (m) => meanOf(m.cv_folds, (f) => f.accuracy), show: (v) => pct(v) },
  { id: 'trading-return', label: 'Trading return', higherIsBetter: true, value: (m) => m.trading?.strategy_return_pct ?? null, show: (v) => signedPct(v) },
  { id: 'excess', label: 'Return vs buy and hold (pts)', higherIsBetter: true, value: (m) => (m.trading ? m.trading.strategy_return_pct - m.trading.buy_hold_return_pct : null), show: (v) => (v === null ? 'n/a' : `${v >= 0 ? '+' : ''}${v.toFixed(1)}`) },
  { id: 'holdout-accuracy', label: 'Hold-out accuracy', higherIsBetter: true, value: (m) => m.holdout?.accuracy ?? null, show: (v) => pct(v) },
  { id: 'holdout-return', label: 'Hold-out trading return', higherIsBetter: true, value: (m) => m.holdout?.trading?.strategy_return_pct ?? null, show: (v) => signedPct(v) },
  { id: 'hit-rate', label: 'Hit rate', higherIsBetter: true, value: (m) => m.trading?.hit_rate ?? null, show: (v) => pct(v) },
  { id: 'max-drawdown', label: 'Max drawdown', higherIsBetter: true, value: (m) => m.trading?.max_drawdown_pct ?? null, show: (v) => signedPct(v) },
  { id: 'sharpe', label: 'Sharpe', higherIsBetter: true, value: (m) => m.trading?.sharpe ?? null, show: (v) => num(v, 2) },
];

export const ModelCompare: React.FC<ModelCompareProps> = ({ models, onClose, onOpen }) => {
  const [onlyDiffs, setOnlyDiffs] = useState(false);

  const features = useMemo(() => {
    const names = new Set<string>();
    models.forEach((m) => m.feature_importance.forEach((f) => names.add(f.feature)));
    const peak = (n: string) => Math.max(...models.map((m) => m.feature_importance.find((f) => f.feature === n)?.importance ?? 0));
    return [...names].sort((a, b) => peak(b) - peak(a));
  }, [models]);
  const maxImportance = Math.max(1e-9, ...models.flatMap((m) => m.feature_importance.map((f) => f.importance)));

  const configRows = useMemo(() => {
    const rows: { id: string; label: string; values: string[] }[] = [
      { id: 'algorithm', label: 'Algorithm', values: models.map((m) => algorithmLabel(m.algorithm)) },
      { id: 'dataset', label: 'Dataset', values: models.map((m) => m.dataset ?? m.data_path) },
      { id: 'symbols', label: 'Symbols', values: models.map((m) => (m.symbols.length ? m.symbols.join(', ') : 'n/a')) },
      { id: 'timeframe', label: 'Timeframe', values: models.map((m) => m.timeframe ?? 'n/a') },
      { id: 'label', label: 'Label', values: models.map((m) => labelText(m.label)) },
      { id: 'trade-mode', label: 'Trading rule', values: models.map((m) => tradeModeLabel(m.trade_mode)) },
      { id: 'holdout', label: 'Hold-out', values: models.map((m) => (m.holdout ? `${m.holdout.rows} rows` : 'none')) },
      { id: 'fee', label: 'Fee (round trip)', values: models.map((m) => (m.fee_pct ? `${m.fee_pct}%` : 'none')) },
      { id: 'estimator', label: 'Estimator file', values: models.map((m) => m.estimator_file ?? 'built-in') },
      { id: 'custom-features', label: 'Custom features', values: models.map((m) => (m.custom_features.length ? m.custom_features.map((c) => `${c.name} = ${c.expr}`).join('\n') : 'none')) },
      { id: 'context-symbols', label: 'Context symbols', values: models.map((m) => (m.context_symbols.length ? m.context_symbols.join(', ') : 'none')) },
      { id: 'rows', label: 'Rows', values: models.map((m) => String(m.rows ?? 'n/a')) },
      { id: 'n_splits', label: 'CV splits', values: models.map((m) => String(m.n_splits)) },
      { id: 'lags', label: 'Lag periods', values: models.map((m) => (m.lag_periods.length ? m.lag_periods.join(', ') : 'none')) },
      { id: 'features', label: 'Features', values: models.map((m) => m.feature_names.join(', ')) },
    ];
    const keys = new Set<string>();
    models.forEach((m) => paramRows(m.algorithm, m.hyperparams).forEach((p) => keys.add(p.key)));
    [...keys].sort().forEach((key) => {
      rows.push({
        id: `hp-${key}`, label: key,
        values: models.map((m) => {
          const row = paramRows(m.algorithm, m.hyperparams).find((p) => p.key === key);
          return row ? `${fmtValue(row.value)}${row.changed ? '' : ' (default)'}` : 'n/a';
        }),
      });
    });
    return rows.map((r) => ({ ...r, differs: new Set(r.values).size > 1 }));
  }, [models]);

  const colsStyle = { gridTemplateColumns: `11rem repeat(${models.length}, minmax(0, 1fr))` };

  return (
    <div className="h-full overflow-auto p-3" data-testid="model-compare">
      <div className="mb-2 flex items-center justify-between">
        <h3 className="font-mono text-sm text-text-primary">Compare {models.length} models</h3>
        <button type="button" className="border border-[var(--border-color)] bg-surface-2 px-2 py-1 text-[11px] font-mono text-text-secondary hover:text-text-primary" onClick={onClose} data-testid="compare-close">Back to list</button>
      </div>

      <div className="grid gap-px bg-[var(--border-color)] text-[11px]" style={colsStyle} data-testid="compare-header">
        <div className="bg-surface-1 p-1.5 text-text-muted">Model</div>
        {models.map((m, i) => (
          <div key={m.model_id} className="bg-surface-1 p-1.5" data-testid={`compare-head-${m.model_id}`}>
            <div className="flex items-center gap-1">
              <span className="inline-block h-2 w-2 shrink-0" style={{ background: SERIES_COLORS[i] }} />
              <button type="button" className="truncate font-mono text-text-primary hover:text-accent" title={m.model_id} onClick={() => onOpen(m.model_id)} data-testid={`compare-open-${m.model_id}`}>{displayName(m)}</button>
            </div>
            <div className="text-[10px] text-text-muted">{algorithmLabel(m.algorithm)}</div>
          </div>
        ))}
      </div>

      <div className="mt-3 text-[10px] uppercase text-text-muted">Metrics (best value highlighted)</div>
      <div className="grid gap-px bg-[var(--border-color)] text-[11px]" style={colsStyle} data-testid="compare-metrics">
        {METRICS.map((metric) => {
          const values = models.map(metric.value);
          const best = bestIndexes(values, metric.higherIsBetter);
          return (
            <React.Fragment key={metric.id}>
              <div className="bg-surface-1 p-1.5 text-text-muted">{metric.label}</div>
              {models.map((m, i) => (
                <div
                  key={m.model_id}
                  className={`p-1.5 font-mono ${best.includes(i) ? 'bg-bull/15 font-semibold text-bull' : 'bg-surface-1 text-text-primary'}`}
                  data-testid={`compare-cell-${metric.id}-${m.model_id}`} data-best={best.includes(i)}
                >
                  {metric.show(values[i])}
                </div>
              ))}
            </React.Fragment>
          );
        })}
      </div>

      <div className="mt-3 text-[10px] uppercase text-text-muted">Feature importance (shared features aligned)</div>
      {features.length === 0 ? (
        <p className="text-[11px] text-text-muted" data-testid="compare-features-empty">None of these models recorded feature importances.</p>
      ) : (
        <div className="space-y-1.5" data-testid="compare-features">
          {features.map((name) => (
            <div key={name} className="grid items-center gap-2 text-[11px]" style={{ gridTemplateColumns: '10rem 1fr' }} data-testid="compare-feature-row" data-feature={name}>
              <span className="truncate font-mono text-text-secondary" title={name}>{name}</span>
              <div className="space-y-px">
                {models.map((m, i) => {
                  const v = m.feature_importance.find((f) => f.feature === name)?.importance;
                  return (
                    <div key={m.model_id} className="flex items-center gap-2" data-testid={`compare-feature-${name}-${m.model_id}`} data-value={v ?? ''}>
                      <div className="h-2 flex-1 bg-surface-2">
                        {v !== undefined && <div className="h-full" style={{ width: `${Math.max(1, (v / maxImportance) * 100)}%`, background: SERIES_COLORS[i] }} />}
                      </div>
                      <span className="w-12 text-right font-mono text-text-muted">{v === undefined ? '-' : v.toFixed(3)}</span>
                    </div>
                  );
                })}
              </div>
            </div>
          ))}
        </div>
      )}

      <div className="mt-3 flex items-center justify-between">
        <div className="text-[10px] uppercase text-text-muted">Configuration differences</div>
        <label className="flex items-center gap-1 text-[10px] text-text-muted">
          <input type="checkbox" checked={onlyDiffs} onChange={(e) => setOnlyDiffs(e.target.checked)} data-testid="compare-only-diffs" />
          Only show differences
        </label>
      </div>
      <div className="grid gap-px bg-[var(--border-color)] text-[11px]" style={colsStyle} data-testid="compare-config">
        {configRows.filter((r) => !onlyDiffs || r.differs).map((r) => (
          <React.Fragment key={r.id}>
            <div className="bg-surface-1 p-1.5 text-text-muted">{r.label}</div>
            {r.values.map((v, i) => (
              <div key={models[i].model_id} className={`whitespace-pre-wrap break-words p-1.5 font-mono ${r.differs ? 'bg-warning/10 text-warning' : 'bg-surface-1 text-text-secondary'}`} data-testid={`compare-config-${r.id}-${models[i].model_id}`} data-differs={r.differs}>{v}</div>
            ))}
          </React.Fragment>
        ))}
      </div>
    </div>
  );
};
