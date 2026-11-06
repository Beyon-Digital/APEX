import React, { useEffect, useState } from 'react';
import { useSymbolOptions } from './useSymbolOptions';

interface SymbolSearchProps {
  /** Called with the chosen symbol; `known` is true when it came from a search result. */
  onPick: (symbol: string, known: boolean) => void;
  /** Symbols to offer instantly (watchlist, recents) before the backend answers. */
  localSymbols?: string[];
  placeholder?: string;
  autoFocus?: boolean;
  inputTestId?: string;
  /** Shows an Add button that submits whatever is typed (as a hand-typed ticker). */
  confirmTestId?: string;
  onCancel?: () => void;
  className?: string;
}

/**
 * Type-ahead symbol picker. Local matches appear immediately; backend results
 * (broker instruments + Yahoo search) are merged in as they arrive.
 */
export const SymbolSearch: React.FC<SymbolSearchProps> = ({
  onPick, localSymbols = [], placeholder = 'Search symbol or company…', autoFocus, inputTestId = 'symbol-search-input', confirmTestId, onCancel, className = '',
}) => {
  const [query, setQuery] = useState('');
  const [active, setActive] = useState(0);
  const [open, setOpen] = useState(true);
  const options = useSymbolOptions(query, localSymbols);

  useEffect(() => setActive(0), [options.length, query]);

  const commit = (symbol: string, known: boolean) => {
    onPick(symbol.trim().toUpperCase(), known);
    setQuery('');
  };

  const onKeyDown = (event: React.KeyboardEvent<HTMLInputElement>) => {
    if (event.key === 'ArrowDown') {
      event.preventDefault();
      setOpen(true);
      setActive((i) => Math.min(i + 1, Math.max(options.length - 1, 0)));
    } else if (event.key === 'ArrowUp') {
      event.preventDefault();
      setActive((i) => Math.max(i - 1, 0));
    } else if (event.key === 'Enter') {
      event.preventDefault();
      const typed = query.trim();
      if (!typed) return;
      const exact = options.find((o) => o.symbol.toUpperCase() === typed.toUpperCase());
      // A highlighted suggestion is only taken if it still matches what is typed.
      const highlighted = open ? options[active] : undefined;
      const stillMatches = highlighted
        && (highlighted.symbol.toUpperCase().includes(typed.toUpperCase())
          || highlighted.name.toUpperCase().includes(typed.toUpperCase()));
      const choice = exact ?? (stillMatches ? highlighted : undefined);
      if (choice) commit(choice.symbol, true);
      else commit(typed, false);
    } else if (event.key === 'Escape') {
      if (query) setQuery('');
      else onCancel?.();
    }
  };

  return (
    <div className={`relative flex gap-2 ${className}`}>
      <input
        type="text"
        value={query}
        autoFocus={autoFocus}
        onChange={(e) => { setQuery(e.target.value); setOpen(true); }}
        onKeyDown={onKeyDown}
        placeholder={placeholder}
        aria-label="Search symbols"
        aria-autocomplete="list"
        aria-expanded={options.length > 0}
        role="combobox"
        data-testid={inputTestId}
        className="min-w-0 flex-1 border border-[var(--border-color)] bg-surface-0 px-2 py-1 font-mono text-sm uppercase placeholder:normal-case focus:border-accent focus:outline-none"
      />
      {confirmTestId && (
        <button
          type="button"
          onClick={() => {
            const typed = query.trim();
            if (!typed) return;
            const exact = options.find((o) => o.symbol.toUpperCase() === typed.toUpperCase());
            commit(exact ? exact.symbol : typed, Boolean(exact));
          }}
          data-testid={confirmTestId}
          className="shrink-0 border border-accent bg-accent/10 px-3 py-1 text-xs font-semibold uppercase tracking-wider text-accent hover:bg-accent/20"
        >
          Add
        </button>
      )}
      {open && options.length > 0 && (
        <ul role="listbox" className="absolute left-0 top-full w-full z-30 mt-px max-h-64 overflow-auto border border-[var(--border-color)] bg-surface-1 shadow-lg" data-testid="symbol-search-results">
          {options.map((option, index) => (
            <li
              key={option.symbol}
              role="option"
              aria-selected={index === active}
              data-testid={`symbol-search-option-${option.symbol}`}
              onMouseDown={(e) => { e.preventDefault(); commit(option.symbol, true); }}
              onMouseEnter={() => setActive(index)}
              className={`flex cursor-pointer items-baseline justify-between gap-2 px-2 py-1 text-xs ${index === active ? 'bg-surface-3' : ''}`}
            >
              <span className="font-mono font-semibold text-text-primary">{option.symbol}</span>
              <span className="min-w-0 flex-1 truncate text-text-muted">{option.name}</span>
              <span className="shrink-0 text-[10px] uppercase text-text-muted">{option.exchange}</span>
            </li>
          ))}
        </ul>
      )}
    </div>
  );
};
