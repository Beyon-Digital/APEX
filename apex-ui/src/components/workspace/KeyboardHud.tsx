import React, { useEffect, useState } from 'react';

const SHORTCUTS: { keys: string; action: string }[] = [
  { keys: 'Space / Ctrl+K', action: 'Activate command bar (Ctrl+K works while typing)' },
  { keys: '↑ / ↓ / Tab', action: 'Command history / suggestions / complete' },
  { keys: 'Ctrl+Shift+K', action: 'Kill switch: halt trading, cancel live orders' },
  { keys: 'Esc', action: 'Close dialog / deactivate command bar' },
  { keys: '?', action: 'Toggle this help overlay' },
  { keys: 'Enter', action: 'Submit command / confirm dialog' },
];

const COMMANDS: { syntax: string; result: string }[] = [
  { syntax: 'AAPL', result: 'View symbol on chart' },
  { syntax: 'AAPL:BOOK', result: 'Symbol on a specific panel' },
  { syntax: 'ADD AAPL MSFT', result: 'Add tickers to the watchlist' },
  { syntax: 'RM AAPL', result: 'Remove a ticker from the watchlist' },
  { syntax: 'BUY AAPL 10', result: 'Market order' },
  { syntax: 'SELL AAPL 5 LIMIT 350', result: 'Limit order' },
  { syntax: ':NEWS / :SCANNER / :GRAPH', result: 'Jump to a panel' },
  { syntax: ':BLOTTER / :COPILOT / :HEALTH', result: 'More panels' },
];

export const KeyboardHud: React.FC = () => {
  const [open, setOpen] = useState(false);

  useEffect(() => {
    const handler = (e: KeyboardEvent) => {
      const target = e.target as HTMLElement;
      const typing = target instanceof HTMLInputElement || target instanceof HTMLTextAreaElement || target.isContentEditable;
      if (e.key === '?' && !typing) {
        e.preventDefault();
        setOpen((o) => !o);
      } else if (e.key === 'Escape' && open) {
        setOpen(false);
      }
    };
    document.addEventListener('keydown', handler);
    return () => document.removeEventListener('keydown', handler);
  }, [open]);

  if (!open) return null;

  return (
    <div
      className="absolute inset-0 bg-black/60 flex items-center justify-center z-[60]"
      onClick={() => setOpen(false)}
      data-testid="keyboard-hud"
    >
      <div
        className="bg-surface-1 border border-[var(--border-color)] rounded-lg p-5 w-[480px] max-h-[80vh] overflow-auto"
        onClick={(e) => e.stopPropagation()}
      >
        <div className="flex items-center justify-between mb-4">
          <h3 className="text-sm font-semibold text-text-primary uppercase tracking-widest">Keyboard & Commands</h3>
          <button
            className="text-text-muted hover:text-text-primary text-xs font-mono"
            onClick={() => setOpen(false)}
            data-testid="keyboard-hud-close"
          >
            ESC
          </button>
        </div>

        <h4 className="text-[10px] font-mono uppercase tracking-widest text-text-muted mb-2">Shortcuts</h4>
        <div className="space-y-1.5 mb-4">
          {SHORTCUTS.map((s) => (
            <div key={s.keys} className="flex items-center justify-between text-xs">
              <kbd className="px-1.5 py-0.5 bg-surface-2 border border-[var(--border-color)] rounded font-mono text-accent">{s.keys}</kbd>
              <span className="text-text-secondary">{s.action}</span>
            </div>
          ))}
        </div>

        <h4 className="text-[10px] font-mono uppercase tracking-widest text-text-muted mb-2">Command bar syntax</h4>
        <div className="space-y-1.5">
          {COMMANDS.map((c) => (
            <div key={c.syntax} className="flex items-center justify-between gap-4 text-xs">
              <code className="font-mono text-accent whitespace-nowrap">{c.syntax}</code>
              <span className="text-text-secondary text-right">{c.result}</span>
            </div>
          ))}
        </div>
      </div>
    </div>
  );
};
