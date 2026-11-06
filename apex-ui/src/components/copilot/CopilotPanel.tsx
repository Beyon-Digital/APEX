import React, { useCallback, useEffect, useRef, useState } from 'react';
import { approveCopilotTools, copilotChat } from '../../lib/tauri';
import type { CopilotMessageDto, PendingApprovalDto, ToolCallTraceDto } from '../../lib/types';

interface ChatEntry extends CopilotMessageDto {
  id: number;
  model?: string;
  provider?: string;
  toolCalls?: ToolCallTraceDto[];
  pendingApprovals?: PendingApprovalDto[];
  approvalState?: 'pending' | 'approved' | 'denied';
  /** The user message that produced this pending turn (for resend-on-approve). */
  prompt?: string;
  /** Turn id of that message; re-sent on approve so the backend treats it as the same turn. */
  turnId?: string;
  error?: boolean;
}

const SUGGESTIONS = [
  'Summarize my open positions and session P&L',
  'Which watchlist symbols are moving most today?',
  'Backtest the default strategy on RELIANCE.NS and report the metrics',
  'Export RELIANCE.NS bars and train a model to predict next-day direction',
  'Place a paper order: buy 5 RELIANCE.NS at market',
  'Write a moving-average strategy for RELIANCE.NS, backtest it, then run it on paper',
  'What is my live trading status and what is running right now?',
  'Create an automation that trades my model on RELIANCE.NS every 5 minutes',
];

const TOOL_LABELS: Record<string, string> = {
  get_quote: 'Quote',
  get_ohlcv: 'OHLCV',
  get_quant_stats: 'Quant stats',
  get_regression: 'Regression',
  run_scan: 'Scan',
  get_news: 'News',
  list_strategies: 'Strategies',
  read_strategy: 'Read file',
  save_strategy: 'Write file',
  run_backtest: 'Backtest',
  export_bars_csv: 'CSV export',
  list_ml_models: 'Models',
  train_ml_model: 'Train model',
  get_model_signal: 'Signal',
  get_positions: 'Positions',
  get_open_orders: 'Open orders',
  list_orders: 'Orders',
  place_order: 'Place order',
  cancel_order: 'Cancel order',
  create_automation: 'New automation',
  list_automations: 'Automations',
  set_automation_enabled: 'Toggle rule',
  delete_automation: 'Delete rule',
  create_alert: 'New alert',
  list_alerts: 'Alerts',
  remove_alert: 'Remove alert',
  get_system_status: 'System status',
  list_brokers: 'Brokers',
  get_account_balance: 'Balance',
  get_holdings: 'Holdings',
  search_symbols: 'Symbol search',
  add_to_watchlist: 'Add to watchlist',
  remove_from_watchlist: 'Remove from watchlist',
  list_strategy_sessions: 'Sessions',
  get_strategy_session: 'Session',
  start_strategy: 'Run strategy',
  stop_strategy: 'Stop strategy',
  modify_order: 'Modify order',
  kill_switch: 'KILL SWITCH',
};

const HISTORY_KEY = 'apex.copilot.history';
const HISTORY_LIMIT = 100;

function loadHistory(): ChatEntry[] {
  try {
    const raw = localStorage.getItem(HISTORY_KEY);
    if (!raw) return [];
    const parsed = JSON.parse(raw) as ChatEntry[];
    return Array.isArray(parsed) ? parsed : [];
  } catch {
    return [];
  }
}

const ApprovalCard: React.FC<{
  entry: ChatEntry;
  onApprove: (entry: ChatEntry) => void;
  onDeny: (entry: ChatEntry) => void;
}> = ({ entry, onApprove, onDeny }) => {
  const approvals = entry.pendingApprovals ?? [];
  return (
    <div
      className="mt-1.5 rounded border border-warning/50 bg-warning/10 px-2 py-1.5 text-[11px]"
      data-testid="copilot-approval-card"
    >
      <div className="mb-1 font-medium text-warning">
        Approval required — the copilot wants to run:
      </div>
      {approvals.map((a) => (
        <div key={a.key} className="font-mono text-[10px] text-text-secondary truncate">
          {TOOL_LABELS[a.name] ?? a.name} · {a.detail}
        </div>
      ))}
      {entry.approvalState === 'pending' ? (
        <div className="mt-1.5 flex gap-2">
          <button
            onClick={() => onApprove(entry)}
            className="px-2 py-0.5 rounded bg-bull/20 text-bull border border-bull/40 text-[10px]"
            data-testid="copilot-approve"
          >
            Approve
          </button>
          <button
            onClick={() => onDeny(entry)}
            className="px-2 py-0.5 rounded bg-bear/20 text-bear border border-bear/40 text-[10px]"
            data-testid="copilot-deny"
          >
            Deny
          </button>
        </div>
      ) : (
        <div className="mt-1 text-[10px] text-text-muted">
          {entry.approvalState === 'approved' ? 'Approved — continuing…' : 'Denied.'}
        </div>
      )}
    </div>
  );
};

const ToolTrace: React.FC<{ calls: ToolCallTraceDto[] }> = ({ calls }) => (
  <div className="mt-1.5 space-y-0.5" data-testid="copilot-tool-trace">
    {calls.map((c, i) => (
      <div
        key={i}
        className={`flex items-center gap-1.5 rounded border px-1.5 py-0.5 text-[10px] font-mono ${
          c.ok ? 'border-[var(--border-color)] text-text-muted' : 'border-bear/40 text-bear'
        }`}
      >
        <span className={c.ok ? 'text-accent' : 'text-bear'}>{c.ok ? '▸' : '✕'}</span>
        <span className="font-medium">{TOOL_LABELS[c.name] ?? c.name}</span>
        {c.detail && <span className="truncate opacity-80">{c.detail}</span>}
      </div>
    ))}
  </div>
);

export const CopilotPanel: React.FC = () => {
  const [entries, setEntries] = useState<ChatEntry[]>(loadHistory);
  const [input, setInput] = useState('');
  const [busy, setBusy] = useState(false);
  const nextId = useRef(0);
  const scrollRef = useRef<HTMLDivElement>(null);

  // nextId stays ahead of the largest persisted id after reload.
  useEffect(() => {
    nextId.current = entries.reduce((m, e) => Math.max(m, e.id), 0) + 1;
  }, [entries]);

  useEffect(() => {
    try {
      localStorage.setItem(HISTORY_KEY, JSON.stringify(entries.slice(-HISTORY_LIMIT)));
    } catch {
      /* storage full or unavailable — chat still works in-memory */
    }
    scrollRef.current?.scrollTo({ top: scrollRef.current.scrollHeight });
  }, [entries, busy]);

  const send = useCallback(async (text: string, replay: boolean, replayTurnId?: string) => {
    const message = text.trim();
    if (!message || busy) return;

    // History is prior turns only: the backend appends `message` itself. When
    // replaying after an approval, the message and its "Paused" reply are already
    // in the list, so cut them off or the model sees the request twice.
    let prior = entries;
    if (replay) {
      let lastUser = -1;
      entries.forEach((e, i) => { if (e.role === 'user') lastUser = i; });
      prior = lastUser >= 0 ? entries.slice(0, lastUser) : entries;
    }
    const history: CopilotMessageDto[] = prior
      .filter((e) => !e.error)
      .map((e) => ({ role: e.role, content: e.content }));
    // One id per user message. Approvals and cached tool results are scoped to it,
    // so asking for the same order again later is a new turn, not a replay.
    const turnId = replay && replayTurnId ? replayTurnId : crypto.randomUUID();

    if (!replay) {
      const userEntry: ChatEntry = { id: nextId.current++, role: 'user', content: message };
      setEntries((current) => [...current, userEntry]);
      setInput('');
    }
    setBusy(true);
    try {
      const reply = await copilotChat(message, history, turnId);
      setEntries((current) => [
        ...current,
        {
          id: nextId.current++,
          role: 'assistant',
          content: reply.reply,
          model: reply.model,
          provider: reply.provider,
          toolCalls: reply.tool_calls,
          pendingApprovals: reply.pending_approvals,
          approvalState:
            reply.pending_approvals.length > 0 ? 'pending' : undefined,
          prompt: message,
          turnId,
        },
      ]);
    } catch (err) {
      setEntries((current) => [
        ...current,
        {
          id: nextId.current++,
          role: 'assistant',
          content: typeof err === 'string' ? err : err instanceof Error ? err.message : 'Copilot request failed',
          error: true,
        },
      ]);
    } finally {
      setBusy(false);
    }
  }, [busy, entries]);

  const handleApprove = useCallback(
    (entry: ChatEntry) => {
      const keys = (entry.pendingApprovals ?? []).map((a) => a.key);
      const prompt = entry.prompt ?? '';
      const turnId = entry.turnId;
      setEntries((current) =>
        current.map((e) => (e.id === entry.id ? { ...e, approvalState: 'approved' as const } : e)),
      );
      void approveCopilotTools(keys)
        .then(() => {
          if (prompt) return send(prompt, true, turnId);
        })
        .catch(() => undefined);
    },
    [send],
  );

  const handleDeny = useCallback((entry: ChatEntry) => {
    setEntries((current) =>
      current.map((e) => (e.id === entry.id ? { ...e, approvalState: 'denied' as const } : e)),
    );
  }, []);

  return (
    <div className="flex flex-col h-full" data-testid="copilot-panel">
      <div ref={scrollRef} className="flex-1 overflow-auto px-3 py-2 space-y-3">
        {entries.length === 0 ? (
          <div className="space-y-2" data-testid="copilot-empty">
            <p className="text-xs text-text-muted">
              Ask about positions, quotes, scans — or have it place paper orders, set alerts,
              write strategy files, run backtests, train models, and create automations that
              trade a model's signal on a schedule.
            </p>
            {SUGGESTIONS.map((s) => (
              <button
                key={s}
                onClick={() => void send(s, false)}
                className="block w-full text-left px-2 py-1.5 text-xs bg-surface-2 hover:bg-surface-3 border border-[var(--border-color)] rounded text-text-secondary"
                data-testid="copilot-suggestion"
              >
                {s}
              </button>
            ))}
          </div>
        ) : (
          entries.map((entry) => (
            <div
              key={entry.id}
              className={entry.role === 'user' ? 'flex justify-end' : 'flex justify-start'}
              data-testid={entry.role === 'user' ? 'copilot-user-msg' : 'copilot-assistant-msg'}
            >
              <div
                className={`max-w-[85%] rounded px-3 py-2 text-sm whitespace-pre-wrap ${
                  entry.role === 'user'
                    ? 'bg-primary-500/20 text-text-primary'
                    : entry.error
                      ? 'bg-bear/10 text-bear border border-bear/30'
                      : 'bg-surface-2 text-text-primary'
                }`}
              >
                {entry.toolCalls && entry.toolCalls.length > 0 && (
                  <ToolTrace calls={entry.toolCalls} />
                )}
                {entry.pendingApprovals && entry.pendingApprovals.length > 0 && (
                  <ApprovalCard entry={entry} onApprove={handleApprove} onDeny={handleDeny} />
                )}
                {entry.content}
                {(entry.model || entry.provider) && entry.model !== 'mock' && (
                  <div className="mt-1 text-[10px] text-text-muted font-mono">
                    {entry.provider && entry.provider !== 'copilot' ? `${entry.provider} · ` : ''}
                    {entry.model}
                  </div>
                )}
              </div>
            </div>
          ))
        )}
        {busy && (
          <div className="flex justify-start">
            <div className="bg-surface-2 rounded px-3 py-2 text-sm text-text-muted" data-testid="copilot-thinking">
              Working… (may call tools)
            </div>
          </div>
        )}
      </div>

      <form
        onSubmit={(e) => {
          e.preventDefault();
          void send(input, false);
        }}
        className="px-3 py-2 border-t border-[var(--border-color)] flex items-center gap-2"
      >
        <input
          type="text"
          value={input}
          onChange={(e) => setInput(e.target.value)}
          placeholder="Ask — or ask it to build & test a strategy…"
          className="flex-1 min-w-0 px-3 py-2 text-sm bg-surface-0 border border-[var(--border-color)] rounded focus:border-accent focus:outline-none"
          data-testid="copilot-input"
        />
        <button
          type="submit"
          disabled={busy || !input.trim()}
          className="px-3 py-2 text-xs bg-primary-500 hover:bg-primary-600 text-white rounded disabled:opacity-50"
          data-testid="copilot-send"
        >
          Send
        </button>
      </form>
    </div>
  );
};
