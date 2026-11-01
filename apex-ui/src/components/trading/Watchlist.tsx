import React, { useEffect, useMemo, useRef, useState } from 'react';
import { useMarketStore } from '../../stores/marketStore';
import { PnlValue } from '../common/PnlValue';
import { formatPrice } from '../../lib/format';
import { quoteAge } from '../../lib/quoteAge';
import type { QuoteDto } from '../../lib/types';
import { SymbolSearch } from '../common/SymbolSearch';
import { addSymbolToWatchlist } from '../../lib/watchlistActions';

type SortKey = 'symbol' | 'last' | 'chg';

interface WatchlistRowProps {
  symbol: string;
  selected: boolean;
  onSelect: (symbol: string) => void;
  onRemove: (symbol: string) => void;
}

const WatchlistRow: React.FC<WatchlistRowProps> = React.memo(({ symbol, selected, onSelect, onRemove }) => {
  const quote = useMarketStore((s) => s.quotes.get(symbol));
  const prevLast = useRef<number | null>(null);
  const [flash, setFlash] = useState<'up' | 'down' | null>(null);

  useEffect(() => {
    if (!quote) return;
    const prev = prevLast.current;
    prevLast.current = quote.last;
    if (prev !== null && quote.last !== prev) {
      setFlash(quote.last > prev ? 'up' : 'down');
      const t = setTimeout(() => setFlash(null), 700);
      return () => clearTimeout(t);
    }
  }, [quote]);

  const flashClass = flash === 'up' ? 'price-flash-up' : flash === 'down' ? 'price-flash-down' : '';

  if (!quote) {
    return (
      <tr
        className={`border-b border-[var(--border-color)] cursor-pointer hover:bg-surface-2 transition-colors duration-100 ${selected ? 'bg-surface-2' : ''}`}
        data-testid={`watchlist-item-${symbol}`}
      >
        <td className="px-1 py-1.5 font-mono text-xs font-medium whitespace-nowrap" onClick={() => onSelect(symbol)}>{symbol}</td>
        <td className="px-1 py-1.5 text-text-muted font-mono text-xs" colSpan={2} onClick={() => onSelect(symbol)}>—</td>
        <td className="px-1 py-1.5 text-right">
          <button
            onClick={(e) => {
              e.stopPropagation();
              onRemove(symbol);
            }}
            className="text-red-500 hover:text-red-400 text-xs px-1 py-0.5"
            data-testid="watchlist-remove-button"
          >
            ✕
          </button>
        </td>
      </tr>
    );
  }

  return (
    <tr
      className={`border-b border-[var(--border-color)] hover:bg-surface-2 cursor-pointer transition-colors duration-100 ${selected ? 'bg-surface-2' : ''}`}
      data-testid={`watchlist-item-${symbol}`}
    >
      <td className="px-1 py-1.5 font-mono text-xs font-medium whitespace-nowrap" onClick={() => onSelect(symbol)}>{symbol}</td>
      <td className={`px-1 py-1.5 font-mono text-xs text-right whitespace-nowrap ${flashClass}`} data-testid={`watchlist-price-${symbol}`} data-numeric onClick={() => onSelect(symbol)} title={quoteAge(quote.updated_at).title}>
        {formatPrice(quote.last)}
        {quoteAge(quote.updated_at).stale && <span className="block text-[10px] text-text-muted">{quoteAge(quote.updated_at).label}</span>}
      </td>
      <td className="px-1 py-1.5 text-right whitespace-nowrap" onClick={() => onSelect(symbol)}>
        <PnlValue value={quote.change_pct} type="percent" className="text-xs" />
      </td>
      <td className="px-1 py-1.5 text-right">
        <button
          onClick={(e) => {
            e.stopPropagation();
            onRemove(symbol);
          }}
          className="text-red-500 hover:text-red-400 text-xs px-1 py-0.5"
          data-testid="watchlist-remove-button"
        >
          ✕
        </button>
      </td>
    </tr>
  );
});

WatchlistRow.displayName = 'WatchlistRow';

interface WatchlistProps {
  onSelectSymbol?: (symbol: string) => void;
  selectedSymbol?: string;
}

export const Watchlist: React.FC<WatchlistProps> = ({ onSelectSymbol, selectedSymbol }) => {
  const watchlist = useMarketStore((s) => s.watchlist);
  const quotes = useMarketStore((s) => s.quotes);
  const removeFromWatchlist = useMarketStore((s) => s.removeFromWatchlist);
  const [isAdding, setIsAdding] = useState(false);
  const [addStatus, setAddStatus] = useState<{ kind: 'idle' | 'checking' | 'error'; message?: string }>({ kind: 'idle' });
  const [sortKey, setSortKey] = useState<SortKey | null>(null);
  const [sortAsc, setSortAsc] = useState(true);

  // A symbol picked from search results is known-good. A hand-typed one must
  // produce a quote before it is saved, so typos don't become dead rows.
  const handlePick = async (symbol: string, known: boolean) => {
    if (!known) setAddStatus({ kind: 'checking', message: `Checking ${symbol}…` });
    const result = await addSymbolToWatchlist(symbol, known);
    setAddStatus(result.ok ? { kind: 'idle' } : { kind: 'error', message: result.message });
  };

  const toggleSort = (key: SortKey) => {
    if (sortKey === key) {
      if (sortAsc) {
        setSortAsc(false);
      } else {
        setSortKey(null);
      }
    } else {
      setSortKey(key);
      setSortAsc(key === 'symbol');
    }
  };

  const sortedWatchlist = useMemo(() => {
    if (!sortKey) return watchlist;
    const dir = sortAsc ? 1 : -1;
    const value = (symbol: string): [number, number | string] => {
      const quote: QuoteDto | undefined = quotes.get(symbol);
      if (!quote) return [1, 0];
      if (sortKey === 'symbol') return [0, symbol];
      if (sortKey === 'last') return [0, quote.last];
      return [0, quote.change_pct];
    };
    return [...watchlist].sort((a, b) => {
      const [aMissing, av] = value(a);
      const [bMissing, bv] = value(b);
      if (aMissing !== bMissing) return aMissing - bMissing;
      if (typeof av === 'string' || typeof bv === 'string') {
        return String(av).localeCompare(String(bv)) * dir;
      }
      return (av - bv) * dir;
    });
  }, [watchlist, quotes, sortKey, sortAsc]);

  const sortIndicator = (key: SortKey) =>
    sortKey === key ? (sortAsc ? ' ▲' : ' ▼') : '';

  const headerClass = 'px-1 py-1.5 font-normal cursor-pointer select-none hover:text-text-secondary uppercase tracking-wider';

  return (
    <div className="flex flex-col h-full" data-testid="watchlist-panel">
      <div className="px-3 py-2 border-b border-[var(--border-color)] flex items-center justify-between">
        <span className="text-sm font-medium text-text-secondary">Watchlist</span>
        <div className="flex items-center gap-2">
          <span className="text-xs text-text-muted font-mono">{watchlist.length} symbols</span>
          <button
            onClick={() => setIsAdding(!isAdding)}
            className="text-primary-500 hover:text-primary-400 text-sm px-2 py-0.5 rounded border border-primary-500/50"
            data-testid="watchlist-add-button"
          >
            +
          </button>
        </div>
      </div>

      {isAdding && (
        <div className="border-b border-[var(--border-color)] px-3 py-2" data-testid="watchlist-add-form">
          <SymbolSearch
            autoFocus
            inputTestId="watchlist-symbol-input"
            confirmTestId="watchlist-confirm-add"
            localSymbols={watchlist}
            onPick={handlePick}
            onCancel={() => { setIsAdding(false); setAddStatus({ kind: 'idle' }); }}
          />
          {addStatus.kind !== 'idle' && (
            <p
              role={addStatus.kind === 'error' ? 'alert' : 'status'}
              className={`mt-1 text-[11px] ${addStatus.kind === 'error' ? 'text-bear' : 'text-text-muted'}`}
              data-testid="watchlist-add-status"
            >
              {addStatus.message}
            </p>
          )}
          <p className="mt-1 text-[10px] text-text-muted">Type a ticker or company, ↑↓ to choose, Enter to add, Esc to close.</p>
        </div>
      )}

      <div className="flex-1 overflow-auto">
        <table className="w-full">
          <thead>
            <tr className="text-xs text-text-muted border-b border-[var(--border-color)]">
              <th className={`${headerClass} text-left`} onClick={() => toggleSort('symbol')} data-testid="watchlist-sort-symbol">
                Symbol{sortIndicator('symbol')}
              </th>
              <th className={`${headerClass} text-right`} onClick={() => toggleSort('last')} data-testid="watchlist-sort-last">
                Last{sortIndicator('last')}
              </th>
              <th className={`${headerClass} text-right`} onClick={() => toggleSort('chg')} data-testid="watchlist-sort-chg">
                Chg%{sortIndicator('chg')}
              </th>
              <th className="px-1 py-1.5 text-right font-normal"></th>
            </tr>
          </thead>
          <tbody>
            {sortedWatchlist.map((symbol) => (
              <WatchlistRow
                key={symbol}
                symbol={symbol}
                selected={symbol === selectedSymbol}
                onSelect={onSelectSymbol ?? (() => {})}
                onRemove={removeFromWatchlist}
              />
            ))}
          </tbody>
        </table>
      </div>
    </div>
  );
};
