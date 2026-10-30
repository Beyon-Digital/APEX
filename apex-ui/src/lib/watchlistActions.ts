import { getQuote, subscribeSymbols } from './tauri';
import { useMarketStore } from '../stores/marketStore';

export interface WatchlistResult {
  ok: boolean;
  message: string;
}

const QUOTE_ATTEMPTS = 4;
const QUOTE_RETRY_MS = 700;

/**
 * Add a symbol to the watchlist. A symbol chosen from search results is known
 * to exist; a hand-typed one must produce a quote first so typos don't turn
 * into rows that never update.
 */
export async function addSymbolToWatchlist(rawSymbol: string, known: boolean): Promise<WatchlistResult> {
  const symbol = rawSymbol.trim().toUpperCase();
  if (!symbol) return { ok: false, message: 'Enter a symbol.' };
  const store = useMarketStore.getState();
  if (store.watchlist.includes(symbol)) {
    return { ok: false, message: `${symbol} is already in the watchlist.` };
  }
  if (known) {
    store.addToWatchlist(symbol);
    return { ok: true, message: `Added ${symbol}.` };
  }
  try {
    await subscribeSymbols([symbol]);
    for (let attempt = 0; attempt < QUOTE_ATTEMPTS; attempt += 1) {
      try {
        const quote = await getQuote(symbol);
        if (quote && quote.symbol) {
          useMarketStore.getState().addToWatchlist(symbol);
          return { ok: true, message: `Added ${symbol}.` };
        }
      } catch {
        // Not streaming yet; retry briefly.
      }
      await new Promise((resolve) => setTimeout(resolve, QUOTE_RETRY_MS));
    }
  } catch {
    // fall through to the not-found message
  }
  return {
    ok: false,
    message: `Couldn't find a live quote for ${symbol}. Pick a result from the list or check the ticker.`,
  };
}

export function removeSymbolFromWatchlist(rawSymbol: string): WatchlistResult {
  const symbol = rawSymbol.trim().toUpperCase();
  const store = useMarketStore.getState();
  if (!store.watchlist.includes(symbol)) {
    return { ok: false, message: `${symbol} is not in the watchlist.` };
  }
  store.removeFromWatchlist(symbol);
  return { ok: true, message: `Removed ${symbol}.` };
}
