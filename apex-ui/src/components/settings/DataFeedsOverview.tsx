import React, { useEffect, useState } from 'react';
import { getSymbolFeeds } from '../../lib/tauri';
import type { FeedOptionDto } from '../../lib/types';
import { useMarketStore } from '../../stores/marketStore';

interface FeedRow extends FeedOptionDto {
  covers: number;
}

/** Which data feeds exist, whether each is connected, and how many watchlist tickers it can serve. */
export const DataFeedsOverview: React.FC = () => {
  const watchlist = useMarketStore((s) => s.watchlist);
  const [rows, setRows] = useState<FeedRow[]>([]);
  const [loaded, setLoaded] = useState(false);

  useEffect(() => {
    let disposed = false;
    void (async () => {
      try {
        const feeds = await getSymbolFeeds(watchlist.slice(0, 100));
        if (disposed || !Array.isArray(feeds)) return;
        const byId = new Map<string, FeedRow>();
        for (const feed of feeds) {
          for (const option of feed.options) {
            const row = byId.get(option.adapter_id) ?? { ...option, covers: 0 };
            if (option.supports) row.covers += 1;
            byId.set(option.adapter_id, row);
          }
        }
        setRows([...byId.values()]);
      } catch {
        // Overview is informational.
      } finally {
        if (!disposed) setLoaded(true);
      }
    })();
    return () => { disposed = true; };
  }, [watchlist]);

  return (
    <div data-testid="data-feeds-overview">
      <h4 className="mb-2 text-xs font-medium text-text-secondary">Data feeds</h4>
      {loaded && rows.length === 0 && <p className="text-xs text-text-muted">No data feeds reported yet.</p>}
      <div className="space-y-1">
        {rows.map((row) => (
          <div key={row.adapter_id} className="flex items-center justify-between gap-3 border border-[var(--border-color)] bg-surface-0 px-3 py-2 text-xs" data-testid={`data-feed-${row.adapter_id}`}>
            <div className="min-w-0">
              <span className="font-medium text-text-primary">{row.name}</span>
              <span className={`ml-2 border px-1 text-[9px] uppercase ${row.quality === 'delayed' ? 'border-warning text-warning' : 'border-bull text-bull'}`}>{row.quality}</span>
            </div>
            <div className="shrink-0 text-right text-[11px] text-text-muted">
              <span className={row.ready ? 'text-bull' : 'text-warning'}>{row.ready ? 'Connected' : 'Not connected'}</span>
              {' · '}covers {row.covers} of {Math.min(watchlist.length, 100)} tickers
            </div>
          </div>
        ))}
      </div>
      <p className="mt-2 text-[11px] text-text-muted">
        Connect more feeds under Accounts. To use a specific feed for one ticker, click its <strong>SRC</strong> badge in the Watchlist.
      </p>
    </div>
  );
};
