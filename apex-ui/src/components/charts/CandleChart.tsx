import React, { useEffect, useRef, useCallback, useState } from 'react';
import {
  createChart,
  type IChartApi,
  type ISeriesApi,
  type CandlestickData,
  type HistogramData,
  type LogicalRange,
  type Time,
  ColorType,
  CrosshairMode,
} from 'lightweight-charts';
import { useMarketStore } from '../../stores/marketStore';
import { getHistoricalData, computeIndicator } from '../../lib/tauri';
import type { OHLCVDto } from '../../lib/types';
import { formatPrice, formatVolume } from '../../lib/format';
import { quoteAge } from '../../lib/quoteAge';
import { chartTheme, useThemeTick, withAlpha } from '../../lib/chartTheme';
import { repaintChart } from '../../lib/chartPaint';

interface CandleChartProps {
  symbol: string;
  ohlcvData?: OHLCVDto[];
  height?: number;
}

type ChartTimeframe = '5m' | '15m' | '1h' | '1d';

const TIMEFRAMES: { id: ChartTimeframe; label: string; bucketSecs: number }[] = [
  { id: '5m', label: '5m', bucketSecs: 300 },
  { id: '15m', label: '15m', bucketSecs: 900 },
  { id: '1h', label: '1H', bucketSecs: 3600 },
  { id: '1d', label: '1D', bucketSecs: 86400 },
];

function toChartTime(iso: string): Time {
  return (new Date(iso).getTime() / 1000) as Time;
}

/** Chart colors resolved from the active theme's CSS variables. */
function chartColors() {
  const t = chartTheme();
  return {
    background: t.background,
    text: t.text,
    grid: t.grid,
    border: t.border,
    bull: t.bull,
    bear: t.bear,
    volumeUp: withAlpha(t.bull, 30),
    volumeDown: withAlpha(t.bear, 30),
    crosshair: t.textMuted,
  };
}
type ChartColors = ReturnType<typeof chartColors>;

// OpenBB-style technical extension surface: price overlays + oscillator pane.
type OverlayId = 'sma' | 'ema' | 'bbands' | 'vwap';
type OscillatorId = 'rsi' | 'macd' | 'stoch' | 'atr' | 'stddev' | 'roc';

const OVERLAY_INDICATORS: { id: OverlayId; label: string }[] = [
  { id: 'sma', label: 'SMA 20' },
  { id: 'ema', label: 'EMA 20' },
  { id: 'bbands', label: 'BB 20·2' },
  { id: 'vwap', label: 'VWAP' },
];

const OSCILLATORS: { id: OscillatorId; label: string }[] = [
  { id: 'rsi', label: 'RSI 14' },
  { id: 'macd', label: 'MACD 12·26·9' },
  { id: 'stoch', label: 'STOCH 14·3' },
  { id: 'atr', label: 'ATR 14' },
  { id: 'stddev', label: 'STDEV 20' },
  { id: 'roc', label: 'ROC 10' },
];

const INDICATOR_SERIES_COLORS: Record<string, string> = {
  sma: '#f59e0b',
  ema: '#22d3ee',
  upper: '#a78bfa',
  middle: '#a78bfa',
  lower: '#a78bfa',
  vwap: '#f472b6',
  rsi: '#22d3ee',
  macd: '#22d3ee',
  signal: '#f59e0b',
  histogram: '#7c7c96',
  k: '#22d3ee',
  d: '#f59e0b',
  atr: '#f59e0b',
  stddev: '#a78bfa',
  roc: '#22d3ee',
};

const CandleChartInner: React.FC<CandleChartProps> = ({ symbol, ohlcvData, height }) => {
  const containerRef = useRef<HTMLDivElement>(null);
  const chartRef = useRef<IChartApi | null>(null);
  const candleSeriesRef = useRef<ISeriesApi<'Candlestick'> | null>(null);
  const volumeSeriesRef = useRef<ISeriesApi<'Histogram'> | null>(null);
  const lastBarTimeRef = useRef<number>(0);
  const getQuote = useMarketStore((s) => s.getQuote);
  const quote = useMarketStore((s) => s.quotes.get(symbol));
  const [timeframe, setTimeframe] = useState<ChartTimeframe>('1d');
  const [loading, setLoading] = useState(false);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [noBars, setNoBars] = useState(false);
  const [barCount, setBarCount] = useState(0);
  const [activeOverlays, setActiveOverlays] = useState<Set<OverlayId>>(new Set());
  const [activeOscillator, setActiveOscillator] = useState<OscillatorId | null>(null);
  const [indMenuOpen, setIndMenuOpen] = useState(false);
  const [indError, setIndError] = useState<string | null>(null);
  const overlaySeriesRef = useRef<Map<string, ISeriesApi<'Line'>[]>>(new Map());
  const oscContainerRef = useRef<HTMLDivElement>(null);
  const bucketSecs = TIMEFRAMES.find((t) => t.id === timeframe)?.bucketSecs ?? 86400;
  // Live-bucket state: each new bucket opens at the first tick's price with
  // volume counted from the day-volume delta — not the day-wide OHLC the
  // quote carries.
  const bucketRef = useRef<{ time: number; open: number; high: number; low: number; volume: number } | null>(null);
  const prevVolumeRef = useRef<number | null>(null);
  // Theme-resolved palette; refreshed each render so theme flips repaint.
  const colorsRef = useRef<ChartColors>(chartColors());
  colorsRef.current = chartColors();
  const themeTick = useThemeTick();

  const toSeriesData = useCallback(
    (points: { time: string; value: number }[]): { time: Time; value: number }[] => {
      const seen = new Set<number>();
      return points
        .filter((p) => {
          const t = new Date(p.time).getTime();
          if (!Number.isFinite(t) || seen.has(t)) return false;
          seen.add(t);
          return Number.isFinite(p.value);
        })
        .map((p) => ({ time: toChartTime(p.time), value: p.value }));
    },
    [],
  );

  const initChart = useCallback(() => {
    const container = containerRef.current;
    if (!container) return;

    // Dispose previous chart
    if (chartRef.current) {
      chartRef.current.remove();
      chartRef.current = null;
    }

    const C = colorsRef.current;
    const chart = createChart(container, {
      layout: {
        background: { type: ColorType.Solid, color: C.background },
        textColor: C.text,
        fontFamily: "'JetBrains Mono', monospace",
        fontSize: 11,
      },
      grid: {
        vertLines: { color: C.grid },
        horzLines: { color: C.grid },
      },
      crosshair: {
        mode: CrosshairMode.Normal,
        vertLine: { color: C.crosshair, labelBackgroundColor: C.border },
        horzLine: { color: C.crosshair, labelBackgroundColor: C.border },
      },
      rightPriceScale: {
        borderColor: C.border,
      },
      localization: {
        // System locale can be 'C' (no ICU data in some webviews) — pin a real
        // locale so price/time formatting never throws.
        locale: 'en-US',
      },
      timeScale: {
        borderColor: C.border,
        timeVisible: true,
        secondsVisible: false,
      },
      width: container.clientWidth,
      height: height ?? container.clientHeight,
    });

    const candleSeries = chart.addCandlestickSeries({
      upColor: C.bull,
      downColor: C.bear,
      borderDownColor: C.bear,
      borderUpColor: C.bull,
      wickDownColor: C.bear,
      wickUpColor: C.bull,
    });

    const volumeSeries = chart.addHistogramSeries({
      priceFormat: { type: 'volume' },
      priceScaleId: 'volume',
    });

    chart.priceScale('volume').applyOptions({
      scaleMargins: { top: 0.8, bottom: 0 },
    });

    chartRef.current = chart;
    candleSeriesRef.current = candleSeries;
    volumeSeriesRef.current = volumeSeries;

    return chart;
    // eslint-disable-next-line react-hooks/exhaustive-deps -- themeTick forces rebuild on theme change
  }, [height, themeTick]);

  const lastBarsRef = useRef<OHLCVDto[]>([]);
  const needsInitialFitRef = useRef(false);

  const setBars = useCallback((bars: OHLCVDto[]) => {
    lastBarsRef.current = bars;
    const candleSeries = candleSeriesRef.current;
    const volumeSeries = volumeSeriesRef.current;
    const chart = chartRef.current;
    if (!candleSeries || !volumeSeries || !chart) return;
    if (bars.length === 0) {
      // Clear both series — a symbol with no history must not keep the
      // previous symbol's candles on screen.
      candleSeries.setData([]);
      volumeSeries.setData([]);
      lastBarTimeRef.current = 0;
      needsInitialFitRef.current = false;
      return;
    }

    // Collapse duplicate bar timestamps — a charting series requires strictly
    // ascending unique times.
    const seen = new Set<number>();
    const uniqueBars = bars.filter((bar) => {
      const t = new Date(bar.time).getTime();
      if (seen.has(t)) return false;
      seen.add(t);
      return true;
    });

    const candles: CandlestickData<Time>[] = uniqueBars.map((bar) => ({
      time: toChartTime(bar.time),
      open: bar.open,
      high: bar.high,
      low: bar.low,
      close: bar.close,
    }));

    const volumes: HistogramData<Time>[] = uniqueBars.map((bar) => ({
      time: toChartTime(bar.time),
      value: bar.volume,
      color: bar.close >= bar.open ? colorsRef.current.volumeUp : colorsRef.current.volumeDown,
    }));

    candleSeries.setData(candles);
    volumeSeries.setData(volumes);
    lastBarTimeRef.current = Number(candles[candles.length - 1].time);
    const container = containerRef.current;
    needsInitialFitRef.current = !container || container.clientWidth <= 0 || (height ?? container.clientHeight) <= 0;
    if (!needsInitialFitRef.current) {
      chart.timeScale().fitContent();
      repaintChart(chart, container, height);
    }
  }, [height]);

  // Initialize chart once (and repaint data after a theme-driven rebuild)
  useEffect(() => {
    const chart = initChart();
    if (!chart) return;
    if (lastBarsRef.current.length > 0) setBars(lastBarsRef.current);

    const handleResize = () => {
      const container = containerRef.current;
      if (container && chartRef.current) {
        const width = container.clientWidth;
        const chartHeight = height ?? container.clientHeight;
        if (width <= 0 || chartHeight <= 0) return;
        chartRef.current.applyOptions({
          width,
          height: chartHeight,
        });
        if (needsInitialFitRef.current && lastBarsRef.current.length > 0) {
          chartRef.current.timeScale().fitContent();
          needsInitialFitRef.current = false;
          repaintChart(chartRef.current, container, height);
        }
      }
    };

    const resizeObserver = new ResizeObserver(handleResize);
    if (containerRef.current) {
      resizeObserver.observe(containerRef.current);
    }

    const repaintWhenVisible = () => {
      if (document.visibilityState === 'visible' && chartRef.current === chart) {
        repaintChart(chart, containerRef.current, height);
      }
    };
    window.addEventListener('focus', repaintWhenVisible);
    document.addEventListener('visibilitychange', repaintWhenVisible);

    return () => {
      resizeObserver.disconnect();
      window.removeEventListener('focus', repaintWhenVisible);
      document.removeEventListener('visibilitychange', repaintWhenVisible);
      if (chartRef.current) {
        chartRef.current.remove();
        chartRef.current = null;
      }
    };
     
  }, [height, initChart, setBars]);

  // Instrument/timeframe change → drop the live tick bucket. Declared BEFORE
  // the loader so it runs first in the same commit (setBars stays the final
  // writer to lastBarTimeRef); deps exclude ohlcvData so a same-series refresh
  // doesn't wipe the live bucket mid-session.
  useEffect(() => {
    bucketRef.current = null;
    prevVolumeRef.current = null;
    lastBarTimeRef.current = 0;
  }, [symbol, timeframe]);

  // Load OHLCV bars when symbol or timeframe changes
  useEffect(() => {
    let cancelled = false;
    setLoading(true);
    setLoadError(null);
    setNoBars(false);
    setBarCount(0);
    // Never display the previous symbol's candles while the next request is
    // loading or has failed. A dated quote alone is not historical data.
    setBars([]);

    const load = async () => {
      try {
        const bars = ohlcvData && ohlcvData.length > 0
          ? ohlcvData
          : await getHistoricalData(symbol, timeframe, 500);
        if (cancelled) return;
        setBars(bars);
        setNoBars(bars.length === 0);
        setBarCount(bars.length);
      } catch (err) {
        if (!cancelled) {
          setLoadError(typeof err === 'string' ? err : err instanceof Error ? err.message : 'Failed to load chart data');
        }
      } finally {
        if (!cancelled) setLoading(false);
      }
    };

    void load();
    return () => {
      cancelled = true;
    };
  }, [symbol, timeframe, ohlcvData, setBars]);

  // Overlay indicators (SMA/EMA/BBANDS/VWAP) — line series on the price pane
  useEffect(() => {
    const chart = chartRef.current;
    if (!chart) return;
    let cancelled = false;

    const existing = overlaySeriesRef.current;
    for (const [id, series] of existing) {
      if (!activeOverlays.has(id as OverlayId)) {
        series.forEach((s) => chart.removeSeries(s));
        existing.delete(id);
      }
    }

    for (const id of activeOverlays) {
      void computeIndicator(symbol, id, timeframe)
        .then((res) => {
          const c = chartRef.current;
          if (cancelled || !c) return;
          overlaySeriesRef.current.get(id)?.forEach((s) => c.removeSeries(s));
          const list: ISeriesApi<'Line'>[] = res.series.map((s) => {
            const line = c.addLineSeries({
              color: INDICATOR_SERIES_COLORS[s.name] ?? '#a78bfa',
              lineWidth: s.name === 'middle' ? 1 : 2,
              priceLineVisible: false,
              lastValueVisible: false,
              crosshairMarkerVisible: false,
            });
            line.setData(toSeriesData(s.points));
            return line;
          });
          overlaySeriesRef.current.set(id, list);
        })
        .catch((e: unknown) => {
          if (!cancelled) setIndError(typeof e === 'string' ? e : 'indicator failed');
        });
    }

    return () => {
      cancelled = true;
      // Symbol/timeframe changed → drop stale overlay lines so they get recomputed.
      const c = chartRef.current;
      if (c) {
        for (const [, series] of existing) {
          series.forEach((s) => c.removeSeries(s));
        }
        existing.clear();
      }
    };
  }, [activeOverlays, symbol, timeframe, toSeriesData]);

  // Oscillator pane — a second synced chart below the candles
  useEffect(() => {
    const container = oscContainerRef.current;
    if (!activeOscillator || !container) return;
    let cancelled = false;

    const C = colorsRef.current;
    const chart = createChart(container, {
      layout: {
        background: { type: ColorType.Solid, color: C.background },
        textColor: C.text,
        fontFamily: "'JetBrains Mono', monospace",
        fontSize: 10,
      },
      grid: {
        vertLines: { color: C.grid },
        horzLines: { color: C.grid },
      },
      rightPriceScale: { borderColor: C.border },
      localization: { locale: 'en-US' },
      timeScale: { borderColor: C.border, timeVisible: true, secondsVisible: false },
      width: container.clientWidth,
      height: container.clientHeight,
    });

    let rangeSync: { source: ReturnType<IChartApi['timeScale']>; handler: (range: LogicalRange | null) => void } | null = null;
    void computeIndicator(symbol, activeOscillator, timeframe)
      .then((res) => {
        if (cancelled) return;
        for (const s of res.series) {
          if (s.name === 'histogram') {
            const hist = chart.addHistogramSeries({ color: INDICATOR_SERIES_COLORS.histogram });
            hist.setData(
              toSeriesData(s.points).map((p) => ({
                ...p,
                color: p.value >= 0 ? C.volumeUp : C.volumeDown,
              })),
            );
          } else {
            const line = chart.addLineSeries({
              color: INDICATOR_SERIES_COLORS[s.name] ?? '#22d3ee',
              lineWidth: 1,
              priceLineVisible: false,
            });
            line.setData(toSeriesData(s.points));
          }
        }
        chart.timeScale().fitContent();
        // Follow the main chart's viewport — unsubscribed in cleanup so the
        // main chart never calls into this chart after it's removed.
        const main = chartRef.current;
        if (main) {
          const source = main.timeScale();
          const handler = (range: LogicalRange | null) => {
            if (range) chart.timeScale().setVisibleLogicalRange(range);
          };
          source.subscribeVisibleLogicalRangeChange(handler);
          rangeSync = { source, handler };
        }
      })
      .catch((e: unknown) => {
        if (!cancelled) setIndError(typeof e === 'string' ? e : 'indicator failed');
      });

    const resizeObserver = new ResizeObserver(() => {
      chart.applyOptions({ width: container.clientWidth, height: container.clientHeight });
    });
    resizeObserver.observe(container);

    return () => {
      cancelled = true;
      resizeObserver.disconnect();
      if (rangeSync) {
        rangeSync.source.unsubscribeVisibleLogicalRangeChange(rangeSync.handler);
      }
      chart.remove();
    };
     
  }, [activeOscillator, symbol, timeframe, toSeriesData, themeTick]);

  const toggleOverlay = (id: OverlayId) => {
    setActiveOverlays((prev) => {
      const next = new Set(prev);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });
  };

  // Reset indicator error when the underlying series changes
  useEffect(() => {
    setIndError(null);
  }, [symbol, timeframe, activeOverlays, activeOscillator]);

  // Listen for real-time quote updates — bucket ticks into the active timeframe
  useEffect(() => {
    if (!symbol) return;

    const intervalId = setInterval(() => {
      const q = getQuote(symbol);
      if (!q || !candleSeriesRef.current || !volumeSeriesRef.current) return;
      if (quoteAge(q.updated_at).stale) return;
      if (![q.open, q.high, q.low, q.last, q.volume].every((v) => Number.isFinite(v))) return;

      // Ticks must never update at a time earlier than the last stored bar
      // (e.g. today's daily bar is timestamped at market open, after the
      // midnight bucket boundary) — lwc rejects that as 'oldest data'.
      const bucketStart = Math.max(
        Math.floor(Date.now() / 1000 / bucketSecs) * bucketSecs,
        lastBarTimeRef.current,
      ) as number;

      // New bucket → open it from this tick; existing bucket → extend h/l/c.
      // Quote OHLC/volume are day-wide, so buckets accumulate their own.
      // Bucket volume = sum of day-volume deltas; a session reset counts the
      // quote's volume directly.
      const volDelta =
        prevVolumeRef.current !== null && q.volume >= prevVolumeRef.current
          ? q.volume - prevVolumeRef.current
          : q.volume;
      prevVolumeRef.current = q.volume;

      let bucket = bucketRef.current;
      if (!bucket || bucket.time !== bucketStart) {
        bucket = { time: bucketStart, open: q.last, high: q.last, low: q.last, volume: volDelta };
        bucketRef.current = bucket;
      } else {
        bucket.high = Math.max(bucket.high, q.last);
        bucket.low = Math.min(bucket.low, q.last);
        bucket.volume += volDelta;
      }

      candleSeriesRef.current.update({
        time: bucketStart as Time,
        open: bucket.open,
        high: bucket.high,
        low: bucket.low,
        close: q.last,
      });

      volumeSeriesRef.current.update({
        time: bucketStart as Time,
        value: bucket.volume,
        color: q.last >= bucket.open ? colorsRef.current.volumeUp : colorsRef.current.volumeDown,
      });
    }, 1000);

    return () => clearInterval(intervalId);
  }, [symbol, getQuote, bucketSecs]);

  return (
    <div className="flex flex-col h-full" data-testid="candle-chart">
      <div className="px-3 py-1.5 border-b border-[var(--border-color)] flex items-center justify-between">
        <div className="flex items-center gap-2">
          <span className="text-sm font-mono font-medium text-text-primary">{symbol}</span>
          {barCount > 0 && <span className="text-xs text-text-muted" data-testid="chart-bar-count">{barCount} bars</span>}
          {quote && (
            <>
              <span className="text-sm font-mono text-text-primary">{formatPrice(quote.last)}</span>
              {quoteAge(quote.updated_at).stale && <span className="text-xs text-text-muted" title={quoteAge(quote.updated_at).title}>Last update {quoteAge(quote.updated_at).label}</span>}
              <span className={`text-xs font-mono ${quote.change_pct >= 0 ? 'text-bull' : 'text-bear'}`}>
                {quote.change_pct >= 0 ? '+' : ''}{quote.change_pct.toFixed(2)}%
              </span>
              <span className="text-xs font-mono text-text-muted">Vol {formatVolume(quote.volume)}</span>
            </>
          )}
        </div>
        <div className="flex items-center gap-1" data-testid="chart-timeframes">
          <div className="relative">
            <button
              onClick={() => setIndMenuOpen((v) => !v)}
              className={`px-2 py-0.5 text-[10px] font-mono uppercase tracking-wider rounded transition-colors ${
                activeOverlays.size > 0 || activeOscillator
                  ? 'bg-accent/15 text-accent'
                  : 'text-text-muted hover:text-text-primary'
              }`}
              data-testid="chart-indicators-btn"
            >
              IND
            </button>
            {indMenuOpen && (
              <div
                className="absolute right-0 top-full mt-1 z-20 w-40 rounded border border-[var(--border-color)] bg-surface-1 shadow-lg py-1"
                data-testid="chart-indicators-menu"
              >
                <div className="px-2 py-1 text-[9px] font-mono uppercase tracking-wider text-text-muted">Overlay</div>
                {OVERLAY_INDICATORS.map((ind) => (
                  <button
                    key={ind.id}
                    onClick={() => toggleOverlay(ind.id)}
                    className={`block w-full text-left px-3 py-1 text-[11px] font-mono ${
                      activeOverlays.has(ind.id) ? 'text-accent' : 'text-text-primary hover:bg-surface-2'
                    }`}
                    data-testid={`ind-overlay-${ind.id}`}
                  >
                    {activeOverlays.has(ind.id) ? '● ' : '○ '}{ind.label}
                  </button>
                ))}
                <div className="px-2 py-1 text-[9px] font-mono uppercase tracking-wider text-text-muted border-t border-[var(--border-color)] mt-1 pt-1.5">Pane</div>
                {OSCILLATORS.map((ind) => (
                  <button
                    key={ind.id}
                    onClick={() => setActiveOscillator((cur) => (cur === ind.id ? null : ind.id))}
                    className={`block w-full text-left px-3 py-1 text-[11px] font-mono ${
                      activeOscillator === ind.id ? 'text-accent' : 'text-text-primary hover:bg-surface-2'
                    }`}
                    data-testid={`ind-osc-${ind.id}`}
                  >
                    {activeOscillator === ind.id ? '● ' : '○ '}{ind.label}
                  </button>
                ))}
                {(activeOverlays.size > 0 || activeOscillator) && (
                  <button
                    onClick={() => { setActiveOverlays(new Set()); setActiveOscillator(null); }}
                    className="block w-full text-left px-3 py-1 text-[11px] font-mono text-bear hover:bg-surface-2 border-t border-[var(--border-color)] mt-1 pt-1.5"
                    data-testid="ind-clear"
                  >
                    Clear all
                  </button>
                )}
              </div>
            )}
          </div>
          {TIMEFRAMES.map((tf) => (
            <button
              key={tf.id}
              onClick={() => setTimeframe(tf.id)}
              className={`px-2 py-0.5 text-[10px] font-mono uppercase tracking-wider rounded transition-colors ${
                timeframe === tf.id
                  ? 'bg-accent/15 text-accent'
                  : 'text-text-muted hover:text-text-primary'
              }`}
              data-testid={`chart-tf-${tf.id}`}
            >
              {tf.label}
            </button>
          ))}
        </div>
      </div>
      <div className="relative flex-1 min-h-0">
        <div ref={containerRef} className="absolute inset-0" />
        {loading && (
          <div className="absolute inset-0 flex items-center justify-center pointer-events-none">
            <span className="text-xs font-mono text-text-muted animate-pulse">Loading bars…</span>
          </div>
        )}
        {loadError && !loading && (
          <div className="absolute inset-0 flex items-center justify-center pointer-events-none">
            <span className="text-xs font-mono text-bear" data-testid="chart-load-error">{loadError}</span>
          </div>
        )}
        {noBars && !loading && !loadError && (
          <div className="absolute inset-0 flex items-center justify-center pointer-events-none">
            <span className="text-xs text-text-muted" data-testid="chart-empty">No historical bars for {symbol} on this timeframe.</span>
          </div>
        )}
        {indError && (
          <div className="absolute bottom-1 left-2 pointer-events-none">
            <span className="text-[10px] font-mono text-bear" data-testid="indicator-error">{indError}</span>
          </div>
        )}
      </div>
      {activeOscillator && (
        <div className="h-28 shrink-0 border-t border-[var(--border-color)] relative" data-testid="oscillator-pane">
          <div className="absolute top-0.5 left-2 z-10 text-[9px] font-mono uppercase tracking-wider text-text-muted">
            {OSCILLATORS.find((o) => o.id === activeOscillator)?.label}
          </div>
          <div ref={oscContainerRef} className="absolute inset-0" />
        </div>
      )}
    </div>
  );
};

export const CandleChart = React.memo(CandleChartInner);
CandleChart.displayName = 'CandleChart';
