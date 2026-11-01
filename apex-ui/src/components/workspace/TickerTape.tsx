import React, { useEffect, useMemo, useRef } from 'react';
import { useMarketStore } from '../../stores/marketStore';
import { getQuote, subscribeSymbols } from '../../lib/tauri';
import { formatPrice } from '../../lib/format';
import { quoteAge } from '../../lib/quoteAge';

const INDEX_SYMBOLS = ['^GSPC', '^IXIC', '^DJI', '^NSEI', 'BTC-USD', 'ETH-USD', 'GC=F', 'CL=F', 'EURUSD=X', 'USDINR=X'];

const INDEX_LABELS: Record<string, string> = {
  '^GSPC': 'S&P 500',
  '^IXIC': 'NASDAQ',
  '^DJI': 'DOW',
  '^NSEI': 'NIFTY 50',
  'BTC-USD': 'BTC',
  'ETH-USD': 'ETH',
  'GC=F': 'GOLD',
  'CL=F': 'WTI',
  'EURUSD=X': 'EURUSD',
  'USDINR=X': 'USDINR',
};

const REFRESH_MS = 30_000;

export const TickerTape: React.FC = () => {
  const quotes = useMarketStore((s) => s.quotes);
  const updateQuote = useMarketStore((s) => s.updateQuote);
  const mounted = useRef(false);

  useEffect(() => {
    if (mounted.current) return;
    mounted.current = true;

    subscribeSymbols(INDEX_SYMBOLS).catch(() => {});

    const refresh = async () => {
      await Promise.allSettled(
        INDEX_SYMBOLS.map(async (symbol) => {
          const quote = await getQuote(symbol);
          if (quote) updateQuote(quote);
        }),
      );
    };
    void refresh();
    const interval = setInterval(refresh, REFRESH_MS);
    return () => clearInterval(interval);
  }, [updateQuote]);

  const items = useMemo(() => {
    const entries = INDEX_SYMBOLS
      .map((symbol) => ({ symbol, quote: quotes.get(symbol) }))
      .filter((e) => e.quote !== undefined);
    // triplicate for a seamless loop
    return [...entries, ...entries, ...entries];
  }, [quotes]);

  return (
    <div
      className="h-6 bg-surface-1 border-b border-[var(--border-color)] overflow-hidden shrink-0 relative"
      data-testid="ticker-tape"
    >
      {items.length === 0 ? (
        <div className="h-full flex items-center px-3">
          <span className="text-[10px] font-mono text-text-muted uppercase tracking-widest animate-pulse">
            Awaiting market data…
          </span>
        </div>
      ) : (
        <div className="ticker-tape-track flex items-center h-full w-max whitespace-nowrap">
          {items.map(({ symbol, quote }, i) => {
            const changePct = quote!.change_pct;
            const tone = changePct > 0 ? 'text-bull' : changePct < 0 ? 'text-bear' : 'text-text-muted';
            const arrow = changePct > 0 ? '▲' : changePct < 0 ? '▼' : '—';
            return (
              <div
                key={`${symbol}-${i}`}
                className="inline-flex items-center gap-2 px-3 h-full border-r border-[var(--border-color)] text-[11px] font-mono uppercase tracking-wider"
                data-testid={`ticker-item-${symbol}`}
              >
                <span className="text-accent font-semibold">{INDEX_LABELS[symbol] ?? symbol}</span>
                <span className="text-text-primary" data-numeric>{formatPrice(quote!.last)}</span>
                {quoteAge(quote!.updated_at).stale && <span className="text-text-muted" title={quoteAge(quote!.updated_at).title}>{quoteAge(quote!.updated_at).label}</span>}
                <span className={`${tone} font-semibold`}>
                  {arrow} {Math.abs(changePct).toFixed(2)}%
                </span>
              </div>
            );
          })}
        </div>
      )}
    </div>
  );
};
