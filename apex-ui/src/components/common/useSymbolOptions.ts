import { useEffect, useMemo, useRef, useState } from 'react';
import Fuse from 'fuse.js';
import { searchSymbols } from '../../lib/tauri';
import type { SymbolSearchResultDto } from '../../lib/types';

const DEBOUNCE_MS = 150;

/**
 * Symbol suggestions for `query`: instant local matches (fuzzy) merged with
 * debounced backend results (broker instruments + Yahoo search).
 */
export function useSymbolOptions(query: string, localSymbols: string[], limit = 12): SymbolSearchResultDto[] {
  const [remote, setRemote] = useState<{ query: string; results: SymbolSearchResultDto[] }>({ query: '', results: [] });
  const requestId = useRef(0);
  const q = query.trim();

  const localFuse = useMemo(
    () => new Fuse(localSymbols.map((symbol) => ({ symbol })), { keys: ['symbol'], threshold: 0.35 }),
    [localSymbols],
  );

  useEffect(() => {
    if (!q) {
      requestId.current += 1;
      setRemote({ query: '', results: [] });
      return;
    }
    const id = ++requestId.current;
    const timer = setTimeout(() => {
      searchSymbols(q)
        .then((results) => {
          if (id === requestId.current && Array.isArray(results)) setRemote({ query: q, results });
        })
        .catch(() => {
          if (id === requestId.current) setRemote({ query: q, results: [] });
        });
    }, DEBOUNCE_MS);
    return () => clearTimeout(timer);
  }, [q]);

  return useMemo(() => {
    if (!q) return [];
    const seen = new Set<string>();
    const merged: SymbolSearchResultDto[] = [];
    const push = (item: SymbolSearchResultDto) => {
      const key = item.symbol.toUpperCase();
      if (!seen.has(key)) {
        seen.add(key);
        merged.push(item);
      }
    };
    // Results for an older query are dropped: they would be suggestions for something the user is no longer typing.
    if (remote.query === q) remote.results.forEach(push);
    localFuse.search(q, { limit: 5 }).forEach(({ item }) =>
      push({ symbol: item.symbol, name: 'In watchlist', exchange: '', source: 'local' }),
    );
    return merged.slice(0, limit);
  }, [q, remote, localFuse, limit]);
}
