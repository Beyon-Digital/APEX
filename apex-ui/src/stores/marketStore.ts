import { create } from 'zustand';
import { persist } from 'zustand/middleware';
import type { QuoteDto } from '../lib/types';

interface MarketState {
  quotes: Map<string, QuoteDto>;
  watchlist: string[];
  lastQuoteAt: number | null;
  updateQuote: (quote: QuoteDto) => void;
  setWatchlist: (symbols: string[]) => void;
  addToWatchlist: (symbol: string) => void;
  removeFromWatchlist: (symbol: string) => void;
  getQuote: (symbol: string) => QuoteDto | undefined;
}

export const useMarketStore = create<MarketState>()(
  persist(
    (set, get) => ({
      quotes: new Map(),
      watchlist: ['RELIANCE.NS', 'TCS.NS', 'HDFCBANK.NS', 'INFY.NS', 'AAPL', 'MSFT', 'GOOGL'],
      lastQuoteAt: null,

      updateQuote: (quote: QuoteDto) => {
        set((state) => {
          const time = Date.parse(quote.updated_at);
          const previous = state.quotes.get(quote.symbol);
          if (previous && Number.isFinite(Date.parse(previous.updated_at)) && (!Number.isFinite(time) || time < Date.parse(previous.updated_at))) return state;
          const newQuotes = new Map(state.quotes);
          newQuotes.set(quote.symbol, quote);
          const lastQuoteAt = Number.isFinite(time) && time <= Date.now() + 5_000
            ? Math.max(state.lastQuoteAt ?? 0, time)
            : state.lastQuoteAt;
          return { quotes: newQuotes, lastQuoteAt };
        });
      },

      setWatchlist: (symbols: string[]) => set({ watchlist: symbols }),

      addToWatchlist: (symbol: string) => {
        set((state) => {
          if (!state.watchlist.includes(symbol)) {
            return { watchlist: [...state.watchlist, symbol] };
          }
          return state;
        });
      },

      removeFromWatchlist: (symbol: string) => {
        set((state) => ({
          watchlist: state.watchlist.filter((s) => s !== symbol),
        }));
      },

      getQuote: (symbol: string) => get().quotes.get(symbol),
    }),
    {
      name: 'market-storage',
      partialize: (state) => ({ watchlist: state.watchlist }),
    }
  )
);
