import React, { useRef, useState } from 'react';
import { SymbolField } from '../common/SymbolField';
import { fetchSymbolHistory } from '../../lib/tauri';
import { MAX_FETCH_BARS, THIN_BARS, dataStatus, type DataStatus } from './datasetModel';
import { errorMessage, type SymbolDataState } from './dataHooks';

const inputCls =
  'px-2 py-1 text-xs bg-surface-0 border border-[var(--border-color)] text-text-primary focus:border-accent focus:outline-none disabled:opacity-50';
const btnCls =
  'px-2 py-1 text-xs font-mono border border-[var(--border-color)] bg-surface-2 text-text-secondary hover:text-text-primary hover:border-accent disabled:opacity-50 disabled:cursor-not-allowed';

function fmtDay(iso: string | null): string {
  return iso ? iso.slice(0, 10) : '?';
}

const STATUS_STYLE: Record<DataStatus, string> = {
  loading: 'border-[var(--border-color)] text-text-muted',
  none: 'border-bear/40 bg-bear/10 text-bear',
  thin: 'border-warning/40 bg-warning/10 text-warning',
  ok: 'border-bull/40 bg-bull/10 text-bull',
};

export interface SymbolDataPickerProps {
  symbols: string[];
  onChange: (symbols: string[]) => void;
  timeframe: string;
  max: number;
  /** Stored data for the symbols (from `useSymbolData`). */
  state: SymbolDataState;
  /** Bars to download when the user presses "Fetch history". */
  fetchBars: number;
  /** True when the dataset needs more bars than one download can return. */
  fetchCapped?: boolean;
  /** Prefix of every data-testid: `${prefix}-input`, `${prefix}-chip-AAPL`, ... */
  testPrefix: string;
  emptyText: string;
  placeholder: string;
  ariaLabel: string;
  /** Wrapper test id (`ml-ds-symbols` is used by the main list). */
  listTestId: string;
}

/**
 * A list of symbols the user picks freely (type anything the app can resolve, or choose a
 * suggestion). Each symbol shows what is stored for the chosen timeframe and, when there is little
 * or none, a button to download history.
 */
export const SymbolDataPicker: React.FC<SymbolDataPickerProps> = ({
  symbols, onChange, timeframe, max, state, fetchBars, fetchCapped = false, testPrefix, emptyText, placeholder, ariaLabel, listTestId,
}) => {
  const [input, setInput] = useState('');
  const [fetching, setFetching] = useState<Record<string, boolean>>({});
  const [fetchErrors, setFetchErrors] = useState<Record<string, string>>({});
  const refreshRef = useRef(state.refresh);
  refreshRef.current = state.refresh;

  const add = (raw: string) => {
    const symbol = raw.trim().toUpperCase();
    if (!symbol) return;
    if (!symbols.includes(symbol) && symbols.length < max) onChange([...symbols, symbol]);
    setInput('');
  };

  const fetchHistory = async (symbol: string) => {
    setFetching((f) => ({ ...f, [symbol]: true }));
    setFetchErrors((e) => { const { [symbol]: _gone, ...rest } = e; return rest; });
    try {
      await fetchSymbolHistory(symbol, timeframe, fetchBars);
      await refreshRef.current();
    } catch (e) {
      setFetchErrors((errs) => ({ ...errs, [symbol]: errorMessage(e) }));
    } finally {
      setFetching((f) => ({ ...f, [symbol]: false }));
    }
  };

  return (
    <div className="space-y-1.5">
      <div className="space-y-1" data-testid={listTestId}>
        {symbols.map((s) => {
          const row = state.data[s];
          const status = dataStatus(row);
          const busy = fetching[s] === true;
          const err = fetchErrors[s];
          return (
            <div key={s} className="flex flex-wrap items-center gap-1.5" data-testid={`${testPrefix}-row-${s}`}>
              <span className="inline-flex items-center gap-1 border border-accent/40 bg-accent/10 px-1.5 py-0.5 text-xs font-mono text-accent" data-testid={`${testPrefix}-chip-${s}`}>
                {s}
                <button type="button" onClick={() => onChange(symbols.filter((x) => x !== s))} aria-label={`Remove ${s}`} className="text-text-muted hover:text-bear" data-testid={`${testPrefix}-remove-${s}`}>×</button>
              </span>
              <span
                className={`border px-1.5 py-0.5 text-[11px] font-mono ${STATUS_STYLE[status]}`}
                data-testid={`${testPrefix}-data-${s}`}
                data-status={status}
                data-bars={row?.bars ?? ''}
              >
                {status === 'loading' && 'checking stored data...'}
                {status === 'none' && `no ${timeframe} data stored`}
                {(status === 'thin' || status === 'ok') && row && `${row.bars} bars, ${fmtDay(row.first_time)} to ${fmtDay(row.last_time)}${status === 'thin' ? ` (thin, under ${THIN_BARS})` : ''}`}
              </span>
              {(status === 'none' || status === 'thin') && (
                <button type="button" className={btnCls} onClick={() => void fetchHistory(s)} disabled={busy} data-testid={`${testPrefix}-fetch-${s}`}>
                  {busy ? 'Fetching...' : `Fetch history (${fetchBars} bars)`}
                </button>
              )}
              {(status === 'none' || status === 'thin') && fetchCapped && (
                <span className="text-[11px] text-warning" data-testid={`${testPrefix}-fetch-cap-${s}`}>
                  Only the most recent {MAX_FETCH_BARS} bars can be downloaded for this timeframe.
                </span>
              )}
              {busy && <span className="text-[11px] text-text-muted" role="status" data-testid={`${testPrefix}-fetching-${s}`}>Downloading {fetchBars} {timeframe} bars for {s}...</span>}
              {err && <span className="text-[11px] text-bear" role="alert" data-testid={`${testPrefix}-fetch-error-${s}`}>{err}</span>}
            </div>
          );
        })}
        {symbols.length === 0 && <span className="text-[11px] text-text-muted">{emptyText}</span>}
      </div>
      {state.error && <div className="text-[11px] text-warning" data-testid={`${testPrefix}-data-error`}>Could not read stored data: {state.error}</div>}
      <div
        className="flex items-center gap-1.5"
        onKeyDown={(e) => {
          // Enter on a typed symbol adds it; Enter that picked a suggestion was already handled (defaultPrevented).
          if (e.key === 'Enter' && !e.defaultPrevented) { e.preventDefault(); add(input); }
        }}
      >
        <SymbolField
          value={input}
          onChange={setInput}
          onPick={add}
          placeholder={symbols.length >= max ? `Up to ${max} symbols` : placeholder}
          disabled={symbols.length >= max}
          className={`${inputCls} w-48`}
          testId={`${testPrefix}-input`}
          ariaLabel={ariaLabel}
        />
        <button type="button" className={btnCls} onClick={() => add(input)} disabled={!input.trim() || symbols.length >= max} data-testid={`${testPrefix}-add`}>Add</button>
      </div>
    </div>
  );
};
