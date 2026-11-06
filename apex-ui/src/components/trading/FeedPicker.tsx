import React, { useEffect, useLayoutEffect, useRef, useState } from 'react';
import { createPortal } from 'react-dom';
import { setSymbolProvider } from '../../lib/tauri';
import type { SymbolFeedDto } from '../../lib/types';
import { useMarketStore } from '../../stores/marketStore';
import { feedShortName } from '../../lib/feeds';

const QUALITY_STYLE: Record<string, string> = {
  live: 'border-bull text-bull',
  exchange: 'border-bull text-bull',
  delayed: 'border-warning text-warning',
};

interface FeedPickerProps {
  symbol: string;
  feed: SymbolFeedDto | undefined;
  onChanged: () => void;
}

/**
 * Source badge for a ticker. Click to see every data feed that could serve it,
 * whether each is connected, and pin one (or go back to automatic).
 */
export const FeedPicker: React.FC<FeedPickerProps> = ({ symbol, feed, onChanged }) => {
  const [open, setOpen] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [pos, setPos] = useState<{ top: number; left: number } | null>(null);
  const buttonRef = useRef<HTMLButtonElement>(null);
  const setPref = useMarketStore((s) => s.setProviderPref);

  // The menu is portalled and fixed-positioned: tiles are CSS-transformed and
  // clip overflow, which would otherwise cut it off.
  useLayoutEffect(() => {
    if (!open || !buttonRef.current) return;
    const rect = buttonRef.current.getBoundingClientRect();
    setPos({ top: rect.bottom + 2, left: Math.max(8, Math.min(rect.left, window.innerWidth - 268)) });
  }, [open]);

  // Move focus into the menu so it is usable from the keyboard.
  useEffect(() => {
    if (!open || !pos) return;
    const first = document.querySelector<HTMLButtonElement>('[data-feed-menu] button:not([disabled])');
    first?.focus();
  }, [open, pos]);

  useEffect(() => {
    if (!open) return;
    const close = (event: MouseEvent | KeyboardEvent) => {
      if (event instanceof KeyboardEvent && event.key !== 'Escape') return;
      if (event instanceof KeyboardEvent) buttonRef.current?.focus();
      if (event instanceof MouseEvent) {
        const target = event.target as Node;
        if ((target as HTMLElement).closest?.('[data-feed-menu]')) return;
        // Clicking the badge again is the toggle's own job; closing here would let the
        // same click reopen it.
        if (buttonRef.current?.contains(target)) return;
      }
      setOpen(false);
    };
    document.addEventListener('mousedown', close);
    document.addEventListener('keydown', close);
    return () => {
      document.removeEventListener('mousedown', close);
      document.removeEventListener('keydown', close);
    };
  }, [open]);

  const choose = async (adapterId: string | null) => {
    setError(null);
    try {
      await setSymbolProvider(symbol, adapterId);
      setPref(symbol, adapterId);
      setOpen(false);
      onChanged();
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    }
  };

  const active = feed?.active ?? null;
  const pinned = feed?.preferred ?? null;
  const activeOption = feed?.options.find((o) => o.adapter_id === active);

  return (
    <>
      <button
        ref={buttonRef}
        type="button"
        onClick={(e) => { e.stopPropagation(); setOpen((o) => !o); }}
        title={active ? `Data from ${activeOption?.name ?? active}${pinned ? ' (pinned)' : ' (automatic)'}. Click to change.` : 'Choose a data feed'}
        aria-haspopup="menu"
        aria-expanded={open}
        data-testid={`feed-badge-${symbol}`}
        className={`border px-1 font-mono text-[9px] leading-4 ${pinned ? 'border-accent text-accent' : 'border-[var(--border-color)] text-text-muted'} hover:text-text-primary`}
      >
        {feedShortName(active)}
      </button>
      {open && pos && createPortal(
        <div
          data-feed-menu
          role="menu"
          className="fixed z-[80] w-64 border border-[var(--border-color)] bg-surface-1 p-2 text-xs shadow-lg"
          style={{ top: pos.top, left: pos.left }}
          data-testid="feed-menu"
        >
          <div className="mb-1 font-semibold uppercase tracking-wider text-text-secondary">Data for {symbol}</div>
          <button type="button" role="menuitemradio" aria-checked={!pinned} onClick={() => void choose(null)} data-testid="feed-auto"
            className={`mb-1 flex w-full items-center justify-between border px-2 py-1 text-left ${!pinned ? 'border-accent text-accent' : 'border-[var(--border-color)] text-text-secondary'} hover:bg-surface-3`}>
            <span>Automatic</span><span className="text-[10px] text-text-muted">best available</span>
          </button>
          {(feed?.options ?? []).map((option) => {
            const usable = option.supports && option.ready;
            return (
              <button
                key={option.adapter_id}
                type="button"
                role="menuitemradio"
                aria-checked={pinned === option.adapter_id}
                disabled={!usable}
                onClick={() => void choose(option.adapter_id)}
                data-testid={`feed-option-${option.adapter_id}`}
                title={option.unavailable_reason ?? undefined}
                className={`mb-1 block w-full border px-2 py-1 text-left ${pinned === option.adapter_id ? 'border-accent' : 'border-[var(--border-color)]'} ${usable ? 'text-text-primary hover:bg-surface-3' : 'cursor-not-allowed text-text-muted opacity-60'}`}
              >
                <span className="flex items-center justify-between gap-2">
                  <span>{option.name}</span>
                  <span className={`border px-1 text-[9px] uppercase ${QUALITY_STYLE[option.quality] ?? 'border-[var(--border-color)]'}`}>{option.quality}</span>
                </span>
                {!usable && <span className="block text-[10px]">{option.unavailable_reason}</span>}
                {usable && active === option.adapter_id && <span className="block text-[10px] text-bull">Serving this ticker now</span>}
              </button>
            );
          })}
          {error && <p className="mt-1 text-[10px] text-bear" role="alert">{error}</p>}
          <p className="mt-1 text-[10px] text-text-muted">Add more feeds under Settings &gt; Accounts.</p>
        </div>,
        document.body,
      )}
    </>
  );
};
