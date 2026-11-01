import React, { useEffect, useMemo } from 'react';
import { useMarketStore } from '../../stores/marketStore';
import { useNewsStore } from '../../stores/newsStore';
import { useWorkspaceStore } from '../../stores/workspaceStore';
import { getQuote, subscribeSymbols } from '../../lib/tauri';
import { formatPrice } from '../../lib/format';
import { PnlValue } from '../common/PnlValue';
import { quoteAge } from '../../lib/quoteAge';

const INDEX_SYMBOLS = ['^GSPC', '^IXIC', '^DJI', '^NSEI', 'BTC-USD', 'ETH-USD', 'GC=F', 'CL=F', 'EURUSD=X', 'USDINR=X'];

const INDEX_LABELS: Record<string, string> = {
  '^GSPC': 'S&P 500',
  '^IXIC': 'NASDAQ',
  '^DJI': 'DOW',
  '^NSEI': 'NIFTY 50',
  'BTC-USD': 'BTC / USD',
  'ETH-USD': 'ETH / USD',
  'GC=F': 'GOLD',
  'CL=F': 'WTI CRUDE',
  'EURUSD=X': 'EUR / USD',
  'USDINR=X': 'USD / INR',
};

export const MARKET_OVERVIEW_SYMBOLS = INDEX_SYMBOLS;

const REFRESH_MS = 30_000;

export const MarketOverview: React.FC = () => {
  const quotes = useMarketStore((s) => s.quotes);
  const watchlist = useMarketStore((s) => s.watchlist);
  const updateQuote = useMarketStore((s) => s.updateQuote);
  const newsItems = useNewsStore((s) => s.items);
  const setCommandSymbol = useWorkspaceStore((s) => s.setCommandSymbol);
  const setCommandTab = useWorkspaceStore((s) => s.setCommandTab);

  useEffect(() => {
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

  const movers = useMemo(() => {
    const rows = watchlist
      .map((symbol) => ({ symbol, quote: quotes.get(symbol) }))
      .filter((r): r is { symbol: string; quote: NonNullable<typeof r.quote> } => r.quote !== undefined && !quoteAge(r.quote.updated_at).stale);
    const sorted = [...rows].sort((a, b) => b.quote.change_pct - a.quote.change_pct);
    return {
      gainers: sorted.filter((r) => r.quote.change_pct > 0).slice(0, 5),
      losers: [...sorted].reverse().filter((r) => r.quote.change_pct < 0).slice(0, 5),
      advancing: rows.filter((r) => r.quote.change_pct > 0).length,
      declining: rows.filter((r) => r.quote.change_pct < 0).length,
      unchanged: rows.filter((r) => r.quote.change_pct === 0).length,
    };
  }, [watchlist, quotes]);

  const latestNews = newsItems.slice(0, 5);

  const jumpToChart = (symbol: string) => {
    setCommandSymbol(symbol);
    setCommandTab('chart');
  };

  return (
    <div className="h-full overflow-auto p-3 space-y-3" data-testid="market-overview">
      {/* Index cards */}
      <div className="grid grid-cols-5 gap-2">
        {INDEX_SYMBOLS.map((symbol) => {
          const quote = quotes.get(symbol);
          return (
            <button
              key={symbol}
              onClick={() => jumpToChart(symbol)}
              className="bg-surface-1 border border-[var(--border-color)] rounded p-2.5 text-left hover:border-accent/50 transition-colors"
              data-testid={`index-card-${symbol}`}
            >
              <div className="text-[10px] font-mono uppercase tracking-widest text-text-muted">
                {INDEX_LABELS[symbol] ?? symbol}
              </div>
              <div className="font-mono text-sm text-text-primary mt-1" data-numeric>
                {quote ? formatPrice(quote.last) : '—'}
              </div>
              {quote && quoteAge(quote.updated_at).stale && <div className="text-[10px] text-text-muted" title={quoteAge(quote.updated_at).title}>{quoteAge(quote.updated_at).label}</div>}
              {quote && (
                <div className="mt-0.5">
                  <PnlValue value={quote.change_pct} type="percent" className="text-[11px] font-mono" />
                </div>
              )}
            </button>
          );
        })}
      </div>

      <div className="grid grid-cols-3 gap-3">
        {/* Breadth */}
        <div className="bg-surface-1 border border-[var(--border-color)] rounded p-3">
          <div className="text-[10px] font-mono uppercase tracking-widest text-text-muted mb-2">Watchlist Breadth · recent quotes</div>
          <div className="flex items-end gap-4">
            <div>
              <div className="text-2xl font-mono text-bull" data-testid="breadth-advancing">{movers.advancing}</div>
              <div className="text-[10px] text-text-muted uppercase">Advancing</div>
            </div>
            <div>
              <div className="text-2xl font-mono text-bear" data-testid="breadth-declining">{movers.declining}</div>
              <div className="text-[10px] text-text-muted uppercase">Declining</div>
            </div>
            <div>
              <div className="text-2xl font-mono text-text-secondary" data-testid="breadth-unchanged">{movers.unchanged}</div>
              <div className="text-[10px] text-text-muted uppercase">Flat</div>
            </div>
          </div>
          <div className="mt-3 h-1.5 bg-surface-2 rounded overflow-hidden flex">
            {movers.advancing + movers.declining + movers.unchanged > 0 && (
              <>
                <div className="bg-bull" style={{ width: `${(movers.advancing / (movers.advancing + movers.declining + movers.unchanged)) * 100}%` }} />
                <div className="bg-text-muted" style={{ width: `${(movers.unchanged / (movers.advancing + movers.declining + movers.unchanged)) * 100}%` }} />
                <div className="bg-bear" style={{ width: `${(movers.declining / (movers.advancing + movers.declining + movers.unchanged)) * 100}%` }} />
              </>
            )}
          </div>
        </div>

        {/* Gainers */}
        <div className="bg-surface-1 border border-[var(--border-color)] rounded p-3">
          <div className="text-[10px] font-mono uppercase tracking-widest text-text-muted mb-2">Top Gainers</div>
          {movers.gainers.length === 0 ? (
            <div className="text-xs text-text-muted">No advancing symbols</div>
          ) : (
            movers.gainers.map(({ symbol, quote }) => (
              <button
                key={symbol}
                onClick={() => jumpToChart(symbol)}
                className="w-full flex items-center justify-between text-xs py-0.5 hover:bg-surface-2 px-1 rounded"
                data-testid={`gainer-${symbol}`}
              >
                <span className="font-mono font-medium">{symbol}</span>
                <span className="font-mono text-text-secondary">{formatPrice(quote.last)}</span>
                <PnlValue value={quote.change_pct} type="percent" className="font-mono" />
              </button>
            ))
          )}
        </div>

        {/* Losers */}
        <div className="bg-surface-1 border border-[var(--border-color)] rounded p-3">
          <div className="text-[10px] font-mono uppercase tracking-widest text-text-muted mb-2">Top Losers</div>
          {movers.losers.length === 0 ? (
            <div className="text-xs text-text-muted">No declining symbols</div>
          ) : (
            movers.losers.map(({ symbol, quote }) => (
              <button
                key={symbol}
                onClick={() => jumpToChart(symbol)}
                className="w-full flex items-center justify-between text-xs py-0.5 hover:bg-surface-2 px-1 rounded"
                data-testid={`loser-${symbol}`}
              >
                <span className="font-mono font-medium">{symbol}</span>
                <span className="font-mono text-text-secondary">{formatPrice(quote.last)}</span>
                <PnlValue value={quote.change_pct} type="percent" className="font-mono" />
              </button>
            ))
          )}
        </div>
      </div>

      {/* Latest headlines */}
      <div className="bg-surface-1 border border-[var(--border-color)] rounded p-3">
        <div className="text-[10px] font-mono uppercase tracking-widest text-text-muted mb-2">Latest Headlines</div>
        {latestNews.length === 0 ? (
          <div className="text-xs text-text-muted">Waiting for news feeds…</div>
        ) : (
          latestNews.map((item) => (
            <button
              key={item.id}
              onClick={() => setCommandTab('news')}
              className="w-full flex items-center gap-3 text-xs py-1 hover:bg-surface-2 px-1 rounded text-left"
              data-testid={`overview-news-${item.id}`}
            >
              <span className="text-text-muted font-mono whitespace-nowrap">{item.source}</span>
              <span className="text-text-primary truncate">{item.headline}</span>
            </button>
          ))
        )}
      </div>
    </div>
  );
};
