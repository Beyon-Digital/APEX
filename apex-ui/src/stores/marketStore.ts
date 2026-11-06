import { create } from 'zustand';
import { persist } from 'zustand/middleware';
import type { QuoteDto } from '../lib/types';

interface MarketState {
  quotes: Map<string, QuoteDto>;
  watchlist: string[];
  lastQuoteAt: number | null;
  /** Data feed pinned per symbol (adapter id). Absent = automatic. */
  providerPrefs: Record<string, string>;
  setProviderPref: (symbol: string, adapterId: string | null) => void;
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
      providerPrefs: {},

      setProviderPref: (symbol, adapterId) => {
        set((state) => {
          const next = { ...state.providerPrefs };
          if (adapterId) next[symbol] = adapterId;
          else delete next[symbol];
          return { providerPrefs: next };
        });
      },

      updateQuote: (quote: QuoteDto) => {
        set((state) => {
          const time = Date.parse(quote.updated_at);
          const previous = state.quotes.get(quote.symbol);
          // A pinned feed is the user's explicit choice. A delayed feed's timestamps are older
          // than the live quote it replaced, and ignoring them would freeze the row.
          const pinned = Boolean(state.providerPrefs[quote.symbol]);
          if (!pinned && previous && Number.isFinite(Date.parse(previous.updated_at)) && (!Number.isFinite(time) || time < Date.parse(previous.updated_at))) return state;
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
        set((state) => {
          const providerPrefs = { ...state.providerPrefs };
          delete providerPrefs[symbol];
          return { watchlist: state.watchlist.filter((s) => s !== symbol), providerPrefs };
        });
      },

      getQuote: (symbol: string) => get().quotes.get(symbol),
    }),
    {
      name: 'market-storage',
      partialize: (state) => ({ watchlist: state.watchlist, providerPrefs: state.providerPrefs }),
    }
  )
);
