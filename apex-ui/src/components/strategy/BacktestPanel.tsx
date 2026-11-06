import { SymbolField } from '../common/SymbolField';
import { formatProfitFactor } from '../../lib/format';
import React, { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { createChart } from 'lightweight-charts';
import type { IChartApi, UTCTimestamp } from 'lightweight-charts';
import { listStrategyFiles, runStrategyBacktest } from '../../lib/tauri';
import type { StrategyBacktestResultDto, StrategyFileDto } from '../../lib/types';
import { useMarketStore } from '../../stores/marketStore';
import { chartTheme, useThemeTick, withAlpha } from '../../lib/chartTheme';

const TF_OPTIONS = ['d1', '1h', '15m', '5m', '1m', 'w1'];

function fmtDate(d: Date): string {
  return d.toISOString().slice(0, 10);
}

function toTs(time: string): UTCTimestamp {
  return Math.floor(new Date(time).getTime() / 1000) as UTCTimestamp;
}

/** Chart chrome resolved from the active theme at mount. */
function chartOpts() {
  const t = chartTheme();
  return {
    height: 160,
    layout: {
      background: { color: 'transparent' },
      textColor: t.textMuted,
      fontSize: 10,
    },
    grid: {
      vertLines: { color: withAlpha(t.textMuted, 10) },
      horzLines: { color: withAlpha(t.textMuted, 10) },
    },
    timeScale: { borderColor: t.border },
    rightPriceScale: { borderColor: t.border },
    localization: { locale: 'en-US' },
  } as const;
}

const EquityChart: React.FC<{ result: StrategyBacktestResultDto }> = ({ result }) => {
  const eqRef = useRef<HTMLDivElement>(null);
  const ddRef = useRef<HTMLDivElement>(null);
  const themeTick = useThemeTick();

  useEffect(() => {
    if (!eqRef.current || !ddRef.current || result.equity_curve.length === 0) return;
    const t = chartTheme();
    const equityContainer = eqRef.current;
    const drawdownContainer = ddRef.current;
    const eqChart: IChartApi = createChart(equityContainer, { ...chartOpts(), width: equityContainer.clientWidth, height: 170 });
    const ddChart: IChartApi = createChart(drawdownContainer, { ...chartOpts(), width: drawdownContainer.clientWidth, height: 90 });
    const resize = () => {
      if (equityContainer.clientWidth > 0) eqChart.applyOptions({ width: equityContainer.clientWidth });
      if (drawdownContainer.clientWidth > 0) ddChart.applyOptions({ width: drawdownContainer.clientWidth });
    };
    const observer = new ResizeObserver(resize);
    observer.observe(equityContainer);
    observer.observe(drawdownContainer);
    const eq = eqChart.addLineSeries({ color: t.accent, lineWidth: 2 });
    const dd = ddChart.addAreaSeries({
      lineColor: t.bear,
      topColor: withAlpha(t.bear, 15),
      bottomColor: withAlpha(t.bear, 45),
      lineWidth: 1,
    });
    eq.setData(result.equity_curve.map((p) => ({ time: toTs(p.time), value: p.equity })));
    dd.setData(result.equity_curve.map((p) => ({ time: toTs(p.time), value: -Math.abs(p.drawdown) * 100 })));
    eqChart.timeScale().fitContent();
    ddChart.timeScale().fitContent();
    return () => {
      observer.disconnect();
      eqChart.remove();
      ddChart.remove();
    };
     
  }, [result, themeTick]);

  return (
    <div className="space-y-1">
      <div ref={eqRef} data-testid="backtest-equity-chart" />
      <div className="text-[10px] uppercase tracking-wide text-text-muted px-1">Drawdown %</div>
      <div ref={ddRef} data-testid="backtest-drawdown-chart" />
    </div>
  );
};

const MetricCell: React.FC<{ label: string; value: string; tone?: 'bull' | 'bear' }> = ({ label, value, tone }) => (
  <div className="rounded border border-[var(--border-color)] bg-surface-1 px-2.5 py-1.5">
    <div className="text-[10px] uppercase tracking-wide text-text-muted">{label}</div>
    <div className={`font-mono text-sm ${tone === 'bull' ? 'text-bull' : tone === 'bear' ? 'text-bear' : 'text-text-primary'}`}>
      {value}
    </div>
  </div>
);

export const BacktestPanel: React.FC<{ defaultSymbol?: string }> = ({ defaultSymbol }) => {
  const watchlist = useMarketStore((s) => s.watchlist);
  const [strategies, setStrategies] = useState<StrategyFileDto[]>([]);
  const [path, setPath] = useState('');
  const [loadError, setLoadError] = useState<string | null>(null);
  const [loadingStrategies, setLoadingStrategies] = useState(true);
  const [symbol, setSymbol] = useState(defaultSymbol ?? watchlist[0] ?? 'RELIANCE.NS');
  const [timeframe, setTimeframe] = useState('d1');
  const [from, setFrom] = useState(() => fmtDate(new Date(Date.now() - 730 * 86400_000)));
  const [to, setTo] = useState(() => fmtDate(new Date()));
  const [quantity, setQuantity] = useState(10);
  const [capital, setCapital] = useState(100_000);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [result, setResult] = useState<StrategyBacktestResultDto | null>(null);
  const [resultConfig, setResultConfig] = useState('');

  const configKey = JSON.stringify([path, symbol.trim().toUpperCase(), timeframe, from, to, quantity, capital]);
  const validation = loadingStrategies ? 'Loading strategies…' : loadError ?? (!path ? 'Create a strategy in the Strategy IDE first.'
    : !symbol.trim() ? 'Enter a symbol.'
      : !from || !to ? 'Choose both dates.'
        : from >= to ? 'Start date must be earlier than end date.'
          : !Number.isFinite(quantity) || quantity <= 0 ? 'Trade size must be greater than zero.'
            : !Number.isFinite(capital) || capital <= 0 ? 'Initial capital must be greater than zero.'
              : null);

  useEffect(() => {
    listStrategyFiles()
      .then((files) => {
        setStrategies(files);
        if (!path && files.length > 0) setPath(files[0].path);
      })
      .catch((err) => { setStrategies([]); setLoadError(err instanceof Error ? err.message : 'Could not load strategy files.'); })
      .finally(() => setLoadingStrategies(false));
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const run = useCallback(async () => {
    if (validation || busy) return;
    setBusy(true);
    setError(null);
    try {
      const res = await runStrategyBacktest({
        path,
        symbol: symbol.trim().toUpperCase(),
        timeframe,
        from,
        to,
        quantity,
        initial_capital: capital,
      });
      setResult(res);
      setResultConfig(configKey);
    } catch (err) {
      setResult(null);
      setError(typeof err === 'string' ? err : err instanceof Error ? err.message : 'Backtest failed');
    } finally {
      setBusy(false);
    }
  }, [path, symbol, timeframe, from, to, quantity, capital, busy, validation, configKey]);

  const m = result?.metrics;
  const metricCells = useMemo(() => {
    if (!result || !m) return [];
    const retTone = m.total_return_pct >= 0 ? 'bull' as const : 'bear' as const;
    return [
      { label: 'Return', value: `${m.total_return_pct.toFixed(2)}%`, tone: retTone },
      { label: 'Ann. Return', value: `${m.annualized_return_pct.toFixed(2)}%`, tone: m.annualized_return_pct >= 0 ? 'bull' as const : 'bear' as const },
      { label: 'Sharpe', value: m.sharpe_ratio.toFixed(2) },
      { label: 'Max DD', value: `${m.max_drawdown_pct.toFixed(2)}%`, tone: 'bear' as const },
      { label: 'Closed trades', value: `${m.total_trades}` },
      { label: 'Win Rate', value: `${m.win_rate.toFixed(1)}%` },
      { label: 'Profit Factor', value: formatProfitFactor(m.profit_factor, m.winning_trades, m.losing_trades) },
      { label: 'Avg Trade', value: m.avg_trade_pnl.toFixed(2), tone: m.avg_trade_pnl >= 0 ? 'bull' as const : 'bear' as const },
      { label: 'Final Equity', value: m.final_equity.toFixed(0) },
      { label: 'Bars', value: `${result.bars_analyzed}` },
    ];
  }, [result, m]);

  const closedTrades = result?.trades.filter((trade) => trade.exit_time !== null) ?? [];

  const inputCls = 'px-2 py-1 text-sm bg-surface-0 border border-[var(--border-color)] rounded min-w-0';

  return (
    <div className="flex flex-col h-full overflow-auto" data-testid="backtest-panel">
      <div className="border-b border-[var(--border-color)] bg-surface-1 px-3 py-3 space-y-3">
        <div>
          <h2 className="text-sm font-medium text-text-primary">Backtest a strategy</h2>
          <p className="mt-0.5 text-xs text-text-muted">Replay your saved strategy on historical data. This does not place orders.</p>
        </div>
        <div className="grid grid-cols-2 gap-2 md:grid-cols-4 xl:grid-cols-7">
          <label className="text-xs text-text-secondary">Strategy
            <select value={path} onChange={(e) => setPath(e.target.value)} className={`${inputCls} mt-1 w-full`} data-testid="backtest-strategy-select">
              {strategies.map((f) => <option key={f.path} value={f.path}>{f.name}</option>)}
              {strategies.length === 0 && <option value="">{loadingStrategies ? 'Loading strategies…' : loadError ? 'Unable to load strategies' : 'No strategy files'}</option>}
            </select>
          </label>
          <label className="text-xs text-text-secondary">Symbol
            <SymbolField value={symbol} onChange={setSymbol} placeholder="e.g. BTC-USD" className={`${inputCls} mt-1 w-full font-mono`} testId="backtest-symbol" />
          </label>
          <label className="text-xs text-text-secondary">Timeframe
            <select value={timeframe} onChange={(e) => setTimeframe(e.target.value)} className={`${inputCls} mt-1 w-full`} data-testid="backtest-timeframe">
              {TF_OPTIONS.map((t) => <option key={t} value={t}>{t}</option>)}
            </select>
          </label>
          <label className="text-xs text-text-secondary">From
            <input type="date" value={from} onChange={(e) => setFrom(e.target.value)} className={`${inputCls} mt-1 w-full`} data-testid="backtest-from" />
          </label>
          <label className="text-xs text-text-secondary">To
            <input type="date" value={to} onChange={(e) => setTo(e.target.value)} className={`${inputCls} mt-1 w-full`} data-testid="backtest-to" />
          </label>
          <label className="text-xs text-text-secondary">Trade size
            <input type="number" value={quantity} min={0} step="any" onChange={(e) => setQuantity(Number(e.target.value))} className={`${inputCls} mt-1 w-full`} data-testid="backtest-quantity" />
          </label>
          <label className="text-xs text-text-secondary">Initial capital
            <input type="number" value={capital} min={0} step="any" onChange={(e) => setCapital(Number(e.target.value))} className={`${inputCls} mt-1 w-full`} data-testid="backtest-capital" />
          </label>
        </div>
        <div className="flex flex-wrap items-center gap-3">
          <button onClick={() => void run()} disabled={busy || !!validation} className="px-4 py-2 text-xs font-medium bg-primary-500 hover:bg-primary-600 text-white rounded disabled:opacity-50" data-testid="backtest-run">
            {busy ? 'Running backtest…' : 'Run backtest'}
          </button>
          {validation && <p className="text-xs text-text-secondary" data-testid="backtest-validation">{validation}</p>}
          {!validation && <p className="text-xs text-text-muted">Uses the saved file and fixed trade size.</p>}
        </div>
      </div>

      <div className="flex-1 p-3 space-y-3">
        {error && <p className="text-xs text-bear" data-testid="backtest-error">{error}</p>}

        {!result && !error && (
          <p className="text-xs text-text-muted" data-testid="backtest-empty">
            Pick a strategy file and symbol, then run a backtest. Results show the equity
            curve, drawdown profile, headline metrics, and the full trade log.
          </p>
        )}

        {result && (
          <>
            {resultConfig !== configKey && <p className="rounded border border-accent/30 bg-accent/10 px-3 py-2 text-xs text-text-secondary" data-testid="backtest-stale">Inputs changed. These results are from the previous run; run the backtest again to update them.</p>}
            <div className="flex flex-wrap items-center gap-2 text-xs text-text-muted" data-testid="backtest-meta">
              <span className="text-text-primary font-medium">{result.strategy_name}</span>
              <span className="rounded bg-surface-2 px-2 py-0.5">{result.inferred_strategy}</span>
              <span className="font-mono">{result.symbol} · {result.timeframe}</span>
            </div>

            <div className="grid grid-cols-2 gap-2 md:grid-cols-5" data-testid="backtest-metrics">
              {metricCells.map((c) => <MetricCell key={c.label} {...c} />)}
            </div>

            <EquityChart result={result} />

            <div data-testid="backtest-trades">
              <div className="text-[10px] uppercase tracking-wide text-text-muted mb-1">Closed trades ({closedTrades.length})</div>
              <div className="rounded border border-[var(--border-color)] overflow-auto max-h-56">
                <table className="w-full text-xs font-mono">
                  <thead className="bg-surface-1 text-text-muted sticky top-0">
                    <tr>
                      <th className="text-left px-2 py-1">Exit side</th>
                      <th className="text-left px-2 py-1">Entry</th>
                      <th className="text-right px-2 py-1">Px</th>
                      <th className="text-left px-2 py-1">Exit</th>
                      <th className="text-right px-2 py-1">Px</th>
                      <th className="text-right px-2 py-1">Qty</th>
                      <th className="text-right px-2 py-1">P&L</th>
                    </tr>
                  </thead>
                  <tbody>
                    {closedTrades.map((t, i) => (
                      <tr key={i} className="border-t border-[var(--border-color)]">
                        <td className={`px-2 py-1 ${t.side.toLowerCase() === 'buy' ? 'text-bull' : 'text-bear'}`}>{t.side}</td>
                        <td className="px-2 py-1">{t.entry_time.slice(0, 10)}</td>
                        <td className="px-2 py-1 text-right">{t.entry_price.toFixed(2)}</td>
                        <td className="px-2 py-1">{t.exit_time?.slice(0, 10) ?? '—'}</td>
                        <td className="px-2 py-1 text-right">{t.exit_price?.toFixed(2) ?? '—'}</td>
                        <td className="px-2 py-1 text-right">{t.quantity}</td>
                        <td className={`px-2 py-1 text-right ${t.pnl >= 0 ? 'text-bull' : 'text-bear'}`}>{t.pnl.toFixed(2)}</td>
                      </tr>
                    ))}
                    {closedTrades.length === 0 && (
                      <tr><td colSpan={7} className="px-2 py-3 text-center text-text-muted">No closed trades generated</td></tr>
                    )}
                  </tbody>
                </table>
              </div>
            </div>

            {result.notes.length > 0 && (
              <div className="rounded border border-[var(--border-color)] bg-surface-1 px-3 py-2 text-xs text-text-muted space-y-0.5" data-testid="backtest-notes">
                {result.notes.map((n, i) => <p key={i}>· {n}</p>)}
              </div>
            )}
          </>
        )}
      </div>
    </div>
  );
};
