/** Hooks that talk to the backend for the Data tab: stored data per symbol and live formula checks. */
import { useCallback, useEffect, useRef, useState } from 'react';
import { checkExpression, listSymbolData } from '../../lib/tauri';
import type { ExpressionCheckDto, SymbolDataDto } from '../../lib/types';
import { sameTimeframe, type CustomRow, type RowCheck } from './datasetModel';

export function errorMessage(e: unknown): string {
  return e instanceof Error ? e.message : String(e);
}

export interface SymbolDataState {
  /** Stored data per symbol for the current timeframe. A missing key means the answer is still on its way. */
  data: Record<string, SymbolDataDto>;
  /** Why the stored-data lookup failed, if it did. */
  error: string | null;
  /** Looks the data up again (call it after storing more history). Counts as a data change. */
  refresh: () => Promise<void>;
  /** Goes up every time `refresh` finishes, so cached answers that depend on stored data can be dropped. */
  revision: number;
}

/** Looks up what is stored for each symbol at a timeframe, and again whenever either changes. */
export function useSymbolData(symbols: string[], timeframe: string): SymbolDataState {
  const [data, setData] = useState<Record<string, SymbolDataDto>>({});
  const [error, setError] = useState<string | null>(null);
  const [revision, setRevision] = useState(0);
  const seq = useRef(0);
  const sig = symbols.join(',');

  const load = useCallback(async () => {
    const list = sig ? sig.split(',') : [];
    const mine = ++seq.current;
    if (list.length === 0) { setData({}); setError(null); return; }
    try {
      const rows = await listSymbolData(list, timeframe);
      if (mine !== seq.current) return; // a newer request owns the state
      const next: Record<string, SymbolDataDto> = {};
      for (const row of rows) if (sameTimeframe(row.timeframe, timeframe)) next[row.symbol.toUpperCase()] = row;
      setData(next);
      setError(null);
    } catch (e) {
      if (mine === seq.current) setError(errorMessage(e));
    }
  }, [sig, timeframe]);

  const refresh = useCallback(async () => {
    await load();
    setRevision((r) => r + 1);
  }, [load]);

  useEffect(() => { void load(); }, [load]);

  return { data, error, refresh, revision };
}

const DEBOUNCE_MS = 400;

function checkKey(symbol: string, timeframe: string, expr: string, context: string, revision: number): string {
  return JSON.stringify([symbol, timeframe, expr, context, revision]);
}

/**
 * Checks every formula against the first selected symbol, a moment after typing stops. Results are
 * cached per (symbol, timeframe, context symbols, data revision, formula), so editing one row does
 * not re-check the others, while fetching history (a new `dataRevision`) checks every formula again.
 */
export function useExpressionChecks(
  rows: CustomRow[], symbol: string | null, timeframe: string, contextSymbols: string[] = [], dataRevision = 0,
  debounceMs = DEBOUNCE_MS,
): Record<string, RowCheck> {
  const [results, setResults] = useState<Record<string, ExpressionCheckDto>>({});
  const inflight = useRef(new Set<string>());

  const context = [...contextSymbols].sort().join(',');
  const wanted: { key: string; expr: string }[] = [];
  if (symbol) {
    for (const row of rows) {
      const expr = row.expr.trim();
      const key = checkKey(symbol, timeframe, expr, context, dataRevision);
      if (expr && !wanted.some((w) => w.key === key)) wanted.push({ key, expr });
    }
  }
  const missing = wanted.filter((w) => !(w.key in results));
  const missingSig = missing.map((w) => w.key).join('\n');

  useEffect(() => {
    if (!symbol || missing.length === 0) return;
    const timer = setTimeout(() => {
      for (const { key, expr } of missing) {
        if (inflight.current.has(key)) continue;
        inflight.current.add(key);
        checkExpression(expr, symbol, timeframe)
          .then((r) => setResults((prev) => ({ ...prev, [key]: r })))
          .catch((e) => setResults((prev) => ({
            ...prev,
            [key]: { ok: false, error: e instanceof Error ? e.message : String(e), error_pos: null, warmup_bars: 0, sample: [] },
          })))
          .finally(() => inflight.current.delete(key));
      }
    }, debounceMs);
    return () => clearTimeout(timer);
    // `missing` is derived from `missingSig`; the signature is the real dependency.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [missingSig, symbol, timeframe, debounceMs]);

  const out: Record<string, RowCheck> = {};
  for (const row of rows) {
    const expr = row.expr.trim();
    if (!expr) { out[row.key] = { status: 'empty' }; continue; }
    if (!symbol) { out[row.key] = { status: 'nosymbol' }; continue; }
    const result = results[checkKey(symbol, timeframe, expr, context, dataRevision)];
    out[row.key] = result ? { status: result.ok ? 'ok' : 'error', result } : { status: 'checking' };
  }
  return out;
}

