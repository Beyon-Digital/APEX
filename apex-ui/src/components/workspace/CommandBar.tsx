import React, { useState, useCallback, useRef, useEffect } from 'react';
import { placeOrder, previewOrder } from '../../lib/tauri';
import type { NewOrderRequestDto, OrderPreviewDto } from '../../lib/types';
import { useBrokerStore } from '../../stores/brokerStore';
import { useMarketStore } from '../../stores/marketStore';
import { useSymbolOptions } from '../common/useSymbolOptions';
import { addSymbolToWatchlist, removeSymbolFromWatchlist } from '../../lib/watchlistActions';
import type { CenterTab } from './workspaceTabs';

const HISTORY_KEY = 'apex-command-history';
const HISTORY_LIMIT = 50;

function loadHistory(): string[] {
  try {
    const raw = localStorage.getItem(HISTORY_KEY);
    const parsed = raw ? JSON.parse(raw) : [];
    return Array.isArray(parsed) ? parsed.filter((x): x is string => typeof x === 'string') : [];
  } catch {
    return [];
  }
}

function saveHistory(history: string[]) {
  try {
    localStorage.setItem(HISTORY_KEY, JSON.stringify(history.slice(-HISTORY_LIMIT)));
  } catch {
    // storage unavailable — history is a convenience only
  }
}

interface PendingOrder {
  request: NewOrderRequestDto;
  preview: OrderPreviewDto;
  label: string;
}

interface Suggestion {
  /** Text the input becomes when the suggestion is accepted. */
  insert: string;
  label: string;
  detail: string;
}

/** Delay in ms before deactivating command bar on blur, to allow click events */
const BLUR_DELAY_MS = 200;

/** Map from command keywords to tab identifiers (COPILOT / AI add and highlight the Copilot tile) */
const PANEL_MAP: Record<string, CenterTab> = {
  CHART: 'chart',
  STRATEGY: 'strategy',
  ML: 'ml',
  DATA: 'data',
  NOTEBOOK: 'notebook',
  HEALTH: 'health',
  NEWS: 'news',
  BLOTTER: 'blotter',
  ORDERS: 'blotter',
  ORDER_BOOK: 'book',
  BOOK: 'book',
  SCANNER: 'scanner',
  SCAN: 'scanner',
  GRAPH: 'graph',
  VECTOR: 'graph',
  ANALYTICS: 'analytics',
  QUANT: 'analytics',
  STATS: 'analytics',
  BACKTEST: 'backtest',
  BT: 'backtest',
  OPTIMIZE: 'backtest',
  COPILOT: 'copilot',
  AI: 'copilot',
  MARKET: 'market',
  MOST: 'market',
  OVERVIEW: 'market',
  AUTOMATIONS: 'automations',
  AUTOMATION: 'automations',
  AUTO: 'automations',
  POSITIONS: 'chart',
};

interface ParsedCommand {
  type: 'SYMBOL_DEFAULT' | 'SYMBOL_PANEL' | 'SYSTEM_PANEL' | 'ORDER' | 'ADD_SYMBOLS' | 'REMOVE_SYMBOLS' | 'UNKNOWN';
  symbol?: string;
  symbols?: string[];
  panel?: string;
  side?: string;
  quantity?: number;
  price?: number;
  orderType?: string;
}

function parseCommand(input: string): ParsedCommand {
  const trimmed = input.trim().toUpperCase();

  // "ADD AAPL MSFT" / "WL+ AAPL" → add to watchlist; "RM AAPL" / "WL- AAPL" → remove
  const listMatch = trimmed.match(/^(ADD|WL\+|RM|DEL|WL-)\s+(.+)$/);
  if (listMatch) {
    const symbols = listMatch[2].split(/[\s,]+/).filter(Boolean);
    return { type: listMatch[1] === 'ADD' || listMatch[1] === 'WL+' ? 'ADD_SYMBOLS' : 'REMOVE_SYMBOLS', symbols };
  }

  // ":ORDERS" → system panel
  if (trimmed.startsWith(':')) {
    return { type: 'SYSTEM_PANEL', panel: trimmed.slice(1) };
  }

  // "BUY RELIANCE 10" or "SELL HDFCBANK 5 LIMIT 1600"
  const orderMatch = trimmed.match(/^(BUY|SELL)\s+(\S+)\s+(\d+(?:\.\d+)?)(?:\s+(LIMIT|STOP)\s+(\d+(?:\.\d+)?))?$/);
  if (orderMatch) {
    return {
      type: 'ORDER',
      side: orderMatch[1],
      symbol: orderMatch[2],
      quantity: parseFloat(orderMatch[3]),
      orderType: orderMatch[4] || 'MARKET',
      price: orderMatch[5] ? parseFloat(orderMatch[5]) : undefined,
    };
  }

  // "RELIANCE:CHART" → symbol + panel
  if (trimmed.includes(':')) {
    const [symbol, panel] = trimmed.split(':');
    return { type: 'SYMBOL_PANEL', symbol, panel };
  }

  // "RELIANCE" → symbol default (open chart)
  if (trimmed.length > 0) {
    return { type: 'SYMBOL_DEFAULT', symbol: trimmed };
  }

  return { type: 'UNKNOWN' };
}

interface CommandBarProps {
  onSelectSymbol?: (symbol: string) => void;
  onSwitchTab?: (tab: string) => void;
}

export const CommandBar: React.FC<CommandBarProps> = ({ onSelectSymbol, onSwitchTab }) => {
  const [input, setInput] = useState('');
  const [isActive, setIsActive] = useState(false);
  const [feedback, setFeedback] = useState<{ message: string; type: 'success' | 'error' } | null>(null);
  const [pending, setPending] = useState<PendingOrder | null>(null);
  const [history, setHistory] = useState<string[]>(loadHistory);
  const [historyIndex, setHistoryIndex] = useState<number | null>(null);
  const [suggestionIndex, setSuggestionIndex] = useState(-1);
  const inputRef = useRef<HTMLInputElement>(null);
  const watchlist = useMarketStore((s) => s.watchlist);
  const activeBrokerId = useBrokerStore((s) => s.activeBrokerId);
  const brokerConnections = useBrokerStore((s) => s.connections);
  const activeBroker = React.useMemo(
    () => brokerConnections.find((broker) => broker.broker_id === activeBrokerId),
    [activeBrokerId, brokerConnections],
  );

  const brokerBadgeLabel = activeBroker?.mode === 'paper'
    ? 'Paper Trading'
    : activeBroker?.display_name ?? activeBrokerId;

  const brokerDotClass = activeBroker?.status === 'connected' || activeBroker?.status === 'ready'
    ? 'bg-bull'
    : activeBroker?.status === 'auth_required' || activeBroker?.status === 'not_configured'
      ? 'bg-warning'
      : 'bg-bear';

  const showFeedback = useCallback((message: string, type: 'success' | 'error') => {
    setFeedback({ message, type });
    setTimeout(() => setFeedback(null), 3000);
  }, []);

  // Symbol suggestions for the token being typed, depending on the command shape.
  const symbolToken = React.useMemo(() => {
    const upper = input.trim().toUpperCase();
    if (!upper || upper.startsWith(':')) return '';
    const withVerb = upper.match(/^(?:ADD|WL\+|BUY|SELL)\s+([^\s,]+)$/);
    if (withVerb) return withVerb[1];
    return /^[^\s:]{2,}$/.test(upper) ? upper : '';
  }, [input]);
  const symbolOptions = useSymbolOptions(symbolToken, watchlist, 8);

  const suggestions = React.useMemo<Suggestion[]>(() => {
    const upper = input.trim().toUpperCase();
    if (!upper) return [];
    if (upper.startsWith(':')) {
      return Object.keys(PANEL_MAP)
        .filter((key) => key.startsWith(upper.slice(1)))
        .slice(0, 8)
        .map((key) => ({ insert: `:${key}`, label: `:${key}`, detail: 'Panel' }));
    }
    const removing = upper.match(/^(?:RM|DEL|WL-)\s+([^\s,]*)$/);
    if (removing) {
      return watchlist
        .filter((symbol) => symbol.startsWith(removing[1]))
        .slice(0, 8)
        .map((symbol) => ({ insert: `RM ${symbol}`, label: symbol, detail: 'Remove from watchlist' }));
    }
    const verb = upper.match(/^(ADD|WL\+|BUY|SELL)\s+/)?.[1];
    return symbolOptions.map((option) => ({
      insert: verb ? `${verb} ${option.symbol}${verb === 'ADD' || verb === 'WL+' ? '' : ' '}` : option.symbol,
      label: option.symbol,
      detail: [option.name, option.exchange].filter(Boolean).join(' · '),
    }));
  }, [input, symbolOptions, watchlist]);

  const confirmPending = useCallback(async () => {
    if (!pending) return;
    const { request, preview, label } = pending;
    setPending(null);
    try {
      const orderId = await placeOrder(request, preview.confirmation_id);
      showFeedback(`Order placed on ${label}: ${request.side} ${request.quantity} ${request.symbol} → ${orderId}`, 'success');
    } catch (err) {
      showFeedback(`Order failed: ${err instanceof Error ? err.message : 'Unknown error'}`, 'error');
    }
  }, [pending, showFeedback]);

  // A confirmation expires server-side; drop the banner when it does.
  useEffect(() => {
    if (!pending) return;
    const ttl = Math.max(pending.preview.expires_in_secs, 5) * 1000;
    const timer = setTimeout(() => {
      setPending(null);
      showFeedback('Order confirmation expired', 'error');
    }, ttl);
    return () => clearTimeout(timer);
  }, [pending, showFeedback]);

  const executeCommand = useCallback(async (cmd: ParsedCommand) => {
    switch (cmd.type) {
      case 'ORDER': {
        if (!cmd.symbol || !cmd.side || !cmd.quantity) {
          showFeedback('Invalid order: missing symbol, side, or quantity', 'error');
          return;
        }
        if (activeBroker && !activeBroker.execution_available) {
          showFeedback(`${activeBroker.display_name} is not available for order routing yet`, 'error');
          return;
        }
        if (activeBroker?.mode === 'live' && !activeBroker.authenticated) {
          showFeedback(`${activeBroker.display_name} needs a session token before live orders are enabled`, 'error');
          return;
        }
        const request: NewOrderRequestDto = {
          symbol: cmd.symbol,
          side: cmd.side === 'BUY' ? 'Buy' : 'Sell',
          order_type: cmd.orderType === 'LIMIT' ? 'Limit' : cmd.orderType === 'STOP' ? 'Stop' : 'Market',
          quantity: cmd.quantity,
          // A STOP order's number is its trigger, not a limit price.
          price: cmd.orderType === 'STOP' ? null : cmd.price ?? null,
          stop_price: cmd.orderType === 'STOP' ? cmd.price ?? null : null,
          broker_id: activeBrokerId,
          tag: 'command-bar',
        };
        try {
          if (activeBroker?.mode === 'live') {
            // Live orders are previewed first and need an explicit confirmation.
            const preview = await previewOrder(request);
            if (!preview.confirmation_id) {
              showFeedback(preview.warnings.join(' ') || 'Order cannot be confirmed right now', 'error');
              return;
            }
            setPending({ request, preview, label: brokerBadgeLabel });
            return;
          }
          const orderId = await placeOrder(request);
          showFeedback(`Order placed on ${brokerBadgeLabel}: ${cmd.side} ${cmd.quantity} ${cmd.symbol} → ${orderId}`, 'success');
        } catch (err) {
          showFeedback(`Order failed: ${err instanceof Error ? err.message : 'Unknown error'}`, 'error');
        }
        break;
      }
      case 'ADD_SYMBOLS': {
        for (const symbol of cmd.symbols ?? []) {
          const result = await addSymbolToWatchlist(symbol, false);
          showFeedback(result.message, result.ok ? 'success' : 'error');
        }
        break;
      }
      case 'REMOVE_SYMBOLS': {
        for (const symbol of cmd.symbols ?? []) {
          const result = removeSymbolFromWatchlist(symbol);
          showFeedback(result.message, result.ok ? 'success' : 'error');
        }
        break;
      }
      case 'SYMBOL_DEFAULT': {
        if (cmd.symbol && onSelectSymbol) {
          onSelectSymbol(cmd.symbol);
          onSwitchTab?.('chart');
          showFeedback(`Viewing ${cmd.symbol}`, 'success');
        }
        break;
      }
      case 'SYMBOL_PANEL': {
        if (cmd.symbol && onSelectSymbol) {
          onSelectSymbol(cmd.symbol);
        }
        if (cmd.panel) {
          const tab = PANEL_MAP[cmd.panel];
          if (tab) onSwitchTab?.(tab);
          else showFeedback(`Unknown panel: ${cmd.panel}`, 'error');
        }
        break;
      }
      case 'SYSTEM_PANEL': {
        if (cmd.panel) {
          const tab = PANEL_MAP[cmd.panel];
          if (tab) {
            onSwitchTab?.(tab);
            showFeedback(`Switched to ${cmd.panel}`, 'success');
          } else {
            showFeedback(`Unknown panel: ${cmd.panel}`, 'error');
          }
        }
        break;
      }
      default:
        break;
    }
  }, [activeBroker, activeBrokerId, brokerBadgeLabel, onSelectSymbol, onSwitchTab, showFeedback]);

  const handleSubmit = useCallback((e: React.FormEvent) => {
    e.preventDefault();
    if (!input.trim()) {
      if (pending) void confirmPending();
      return;
    }
    const entry = input.trim();
    setPending(null);
    const nextHistory = [...history.filter((h) => h !== entry), entry].slice(-HISTORY_LIMIT);
    setHistory(nextHistory);
    saveHistory(nextHistory);
    setHistoryIndex(null);
    const cmd = parseCommand(input);
    void executeCommand(cmd);
    setInput('');
    setIsActive(false);
  }, [input, pending, history, confirmPending, executeCommand]);

  const acceptSuggestion = useCallback((suggestion: Suggestion) => {
    setInput(suggestion.insert);
    setSuggestionIndex(-1);
  }, []);

  const handleInputKeyDown = useCallback((e: React.KeyboardEvent<HTMLInputElement>) => {
    if (e.key === 'ArrowDown' || e.key === 'ArrowUp') {
      e.preventDefault();
      if (suggestions.length > 0) {
        setSuggestionIndex((i) => {
          const next = e.key === 'ArrowDown' ? i + 1 : i - 1;
          return Math.max(-1, Math.min(next, suggestions.length - 1));
        });
        return;
      }
      if (history.length === 0) return;
      const next = e.key === 'ArrowUp'
        ? Math.max(0, (historyIndex ?? history.length) - 1)
        : (historyIndex ?? history.length) + 1;
      if (next >= history.length) {
        setHistoryIndex(null);
        setInput('');
      } else {
        setHistoryIndex(next);
        setInput(history[next]);
      }
    } else if (e.key === 'Tab' && suggestions.length > 0) {
      e.preventDefault();
      acceptSuggestion(suggestions[Math.max(suggestionIndex, 0)]);
    } else if (e.key === 'Enter' && suggestionIndex >= 0 && suggestions[suggestionIndex]) {
      e.preventDefault();
      acceptSuggestion(suggestions[suggestionIndex]);
    }
  }, [suggestions, suggestionIndex, history, historyIndex, acceptSuggestion]);

  const handleKeyDown = useCallback((e: KeyboardEvent) => {
    const el = e.target as HTMLElement | null;
    const tag = el?.tagName;
    const isEditable =
      el instanceof HTMLInputElement ||
      el instanceof HTMLTextAreaElement ||
      tag === 'BUTTON' ||
      tag === 'SELECT' ||
      tag === 'A' ||
      Boolean(el?.closest('.monaco-editor')) ||
      el?.isContentEditable === true;
    if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === 'k') {
      e.preventDefault();
      setIsActive(true);
      setTimeout(() => inputRef.current?.focus(), 0);
      return;
    }
    if (e.key === ' ' && !isActive && !isEditable) {
      e.preventDefault();
      setIsActive(true);
      setTimeout(() => inputRef.current?.focus(), 0);
    }
    if (e.key === 'Escape') {
      setIsActive(false);
      setInput('');
      setPending(null);
      setSuggestionIndex(-1);
    }
  }, [isActive]);

  useEffect(() => {
    document.addEventListener('keydown', handleKeyDown);
    return () => document.removeEventListener('keydown', handleKeyDown);
  }, [handleKeyDown]);

  return (
    <div className="h-10 bg-surface-1 border-b border-[var(--border-color)] flex items-center px-4 gap-3">
      <span className="text-accent font-mono font-semibold text-sm">APEX</span>
      <form onSubmit={handleSubmit} className="relative flex-1 max-w-xl" data-testid="command-bar-form">
        <input
          ref={inputRef}
          type="text"
          value={input}
          onKeyDown={handleInputKeyDown}
          onChange={(e) => { setInput(e.target.value); setSuggestionIndex(-1); setHistoryIndex(null); }}
          onFocus={() => setIsActive(true)}
          onBlur={() => setTimeout(() => setIsActive(false), BLUR_DELAY_MS)}
          placeholder="Symbol, ADD/RM ticker, BUY/SELL order, or :panel  (Space or Ctrl+K)"
          className="w-full bg-surface-2 text-text-primary font-mono text-sm px-3 py-1.5 rounded-md border border-[var(--border-color)] focus:border-accent focus:outline-none placeholder:text-text-muted"
          data-testid="command-bar-input"
        />
        {isActive && suggestions.length > 0 && (
          <ul role="listbox" className="absolute left-0 right-0 top-full z-40 mt-px max-h-72 overflow-auto border border-[var(--border-color)] bg-surface-1 shadow-lg" data-testid="command-bar-suggestions">
            {suggestions.map((suggestion, index) => (
              <li
                key={suggestion.insert}
                role="option"
                aria-selected={index === suggestionIndex}
                onMouseDown={(e) => { e.preventDefault(); acceptSuggestion(suggestion); inputRef.current?.focus(); }}
                className={`flex cursor-pointer items-baseline justify-between gap-3 px-3 py-1 font-mono text-xs ${index === suggestionIndex ? 'bg-surface-3' : ''}`}
              >
                <span className="font-semibold text-text-primary">{suggestion.label}</span>
                <span className="truncate text-text-muted">{suggestion.detail}</span>
              </li>
            ))}
          </ul>
        )}
      </form>
      {pending && (
        <div className="flex items-center gap-2 border border-warning bg-warning/10 px-2 py-1 text-xs font-mono text-warning" data-testid="command-bar-confirm">
          <span>
            LIVE {pending.request.side.toUpperCase()} {pending.request.quantity} {pending.request.symbol} on {pending.label}
            {pending.preview.estimated_value ? ` ≈ ${pending.preview.estimated_value.toFixed(2)}` : ''}
          </span>
          <button type="button" onClick={() => void confirmPending()} data-testid="command-bar-confirm-yes"
            className="border border-warning px-2 py-0.5 font-semibold hover:bg-warning/20">CONFIRM</button>
          <button type="button" onClick={() => setPending(null)} className="px-1 text-text-muted hover:text-text-primary">Esc</button>
        </div>
      )}
      {feedback && (
        <span
          className={`text-xs font-mono ${feedback.type === 'success' ? 'text-bull' : 'text-bear'}`}
          data-testid="command-bar-feedback"
        >
          {feedback.message}
        </span>
      )}
      <div className="hidden md:flex items-center gap-1.5 text-text-muted">
        <kbd className="px-1.5 py-0.5 bg-surface-2 border border-[var(--border-color)] rounded font-mono text-[9px]">SPACE</kbd>
        <kbd className="px-1.5 py-0.5 bg-surface-2 border border-[var(--border-color)] rounded font-mono text-[9px]">CTRL+K</kbd>
        <kbd className="px-1.5 py-0.5 bg-surface-2 border border-[var(--border-color)] rounded font-mono text-[9px]" title="Keyboard shortcuts">?</kbd>
      </div>
      <div className="flex items-center gap-2 text-xs text-text-muted font-mono">
        <span>{brokerBadgeLabel}</span>
        <span className={`w-2 h-2 rounded-full ${brokerDotClass}`}></span>
      </div>
    </div>
  );
};
