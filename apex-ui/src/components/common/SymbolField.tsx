import React, { useLayoutEffect, useRef, useState } from 'react';
import { createPortal } from 'react-dom';
import { useMarketStore } from '../../stores/marketStore';
import { useSymbolOptions } from './useSymbolOptions';

interface SymbolFieldProps {
  value: string;
  onChange: (value: string) => void;
  className?: string;
  placeholder?: string;
  testId?: string;
  ariaLabel?: string;
  /** Called when a suggestion is chosen (in addition to onChange). */
  onPick?: (symbol: string) => void;
  disabled?: boolean;
  required?: boolean;
}

/**
 * A ticker input that suggests matches as you type (broker instruments, Yahoo and
 * your watchlist) and offers your watchlist as quick picks when empty. It is still a
 * plain text input, so typing any symbol and pressing Tab or Enter keeps working.
 */
export const SymbolField: React.FC<SymbolFieldProps> = ({
  value, onChange, className = '', placeholder = 'Symbol', testId, ariaLabel = 'Symbol', onPick, disabled, required,
}) => {
  const watchlist = useMarketStore((s) => s.watchlist);
  const [open, setOpen] = useState(false);
  const [active, setActive] = useState(-1);
  const [pos, setPos] = useState<{ top: number; left: number; width: number } | null>(null);
  const inputRef = useRef<HTMLInputElement>(null);

  const query = value.trim();
  const found = useSymbolOptions(query, watchlist, 8);
  const quick = React.useMemo(
    () => watchlist.slice(0, 8).map((symbol) => ({ symbol, name: 'Watchlist', exchange: '', source: 'local' })),
    [watchlist],
  );
  const options = query ? found.filter((o) => o.symbol.toUpperCase() !== query.toUpperCase()) : quick;

  useLayoutEffect(() => {
    if (!open || !inputRef.current) return;
    const rect = inputRef.current.getBoundingClientRect();
    setPos({ top: rect.bottom + 1, left: rect.left, width: Math.max(rect.width, 220) });
  }, [open, value, options.length]);

  const choose = (symbol: string) => {
    onChange(symbol);
    onPick?.(symbol);
    setOpen(false);
    setActive(-1);
  };

  const onKeyDown = (event: React.KeyboardEvent<HTMLInputElement>) => {
    if (!open || options.length === 0) return;
    if (event.key === 'ArrowDown') {
      event.preventDefault();
      setActive((i) => Math.min(i + 1, options.length - 1));
    } else if (event.key === 'ArrowUp') {
      event.preventDefault();
      setActive((i) => Math.max(i - 1, 0));
    } else if (event.key === 'Enter' && active >= 0) {
      event.preventDefault(); // choose the suggestion instead of submitting the form
      choose(options[active].symbol);
    } else if (event.key === 'Escape') {
      setOpen(false);
    }
  };

  return (
    <>
      <input
        ref={inputRef}
        type="text"
        value={value}
        disabled={disabled}
        required={required}
        onChange={(e) => { onChange(e.target.value); setOpen(true); setActive(-1); }}
        onFocus={() => setOpen(true)}
        onBlur={() => setTimeout(() => setOpen(false), 120)}
        onKeyDown={onKeyDown}
        placeholder={placeholder}
        aria-label={ariaLabel}
        aria-autocomplete="list"
        aria-expanded={open && options.length > 0}
        autoComplete="off"
        spellCheck={false}
        data-testid={testId}
        className={className}
      />
      {open && pos && options.length > 0 && createPortal(
        <ul
          role="listbox"
          className="fixed z-[85] max-h-56 overflow-auto border border-[var(--border-color)] bg-surface-1 shadow-lg"
          style={{ top: pos.top, left: pos.left, width: pos.width }}
          data-testid={testId ? `${testId}-options` : undefined}
        >
          {!query && <li className="px-2 py-1 text-[10px] uppercase tracking-wider text-text-muted">Your watchlist</li>}
          {options.map((option, index) => (
            <li
              key={option.symbol}
              role="option"
              aria-selected={index === active}
              onMouseDown={(e) => { e.preventDefault(); choose(option.symbol); }}
              onMouseEnter={() => setActive(index)}
              className={`flex cursor-pointer items-baseline justify-between gap-2 px-2 py-1 text-xs ${index === active ? 'bg-surface-3' : ''}`}
            >
              <span className="font-mono font-semibold text-text-primary">{option.symbol}</span>
              <span className="min-w-0 flex-1 truncate text-text-muted">{option.name}</span>
              <span className="shrink-0 text-[10px] uppercase text-text-muted">{option.exchange}</span>
            </li>
          ))}
        </ul>,
        document.body,
      )}
    </>
  );
};
