import React, { useCallback, useEffect, useRef, useState } from 'react';
import { approveCopilotTools, copilotChat, type CopilotMode } from '../../lib/tauri';
import type { CopilotMessageDto, ToolCallTraceDto } from '../../lib/types';
import { useCopilotMode, useCopilotSessions, type ChatEntry } from '../../stores/copilotSessions';

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
  web_fetch: 'Web page',
  compute_indicator: 'Indicator',
  get_order_book: 'Order book',
  get_correlations: 'Correlations',
  search_news: 'News search',
  get_system_health: 'System health',
  get_symbol_feeds: 'Data feeds',
  set_symbol_provider: 'Pin data feed',
  list_notebooks: 'Notebooks',
  read_notebook: 'Read notebook',
  write_notebook: 'Write notebook',
  run_notebook_cell: 'Run cell',
};

const MODES: { id: CopilotMode; label: string; description: string }[] = [
  { id: 'ask', label: 'Ask', description: 'Read-only: answers from data, changes nothing' },
  { id: 'review', label: 'Review', description: 'Asks before each change' },
  { id: 'agent', label: 'Agent', description: 'Acts on its own; live-broker actions still ask you' },
];

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
        <div key={a.key} className="whitespace-pre-wrap break-words font-mono text-[10px] text-text-secondary" data-testid="copilot-approval-line">
          <span className={a.detail.startsWith('[LIVE') ? 'font-bold text-bear' : ''}>{TOOL_LABELS[a.name] ?? a.name}</span> · {a.detail}
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

const EMPTY: ChatEntry[] = [];

const ModeSwitch: React.FC = () => {
  const mode = useCopilotMode((s) => s.mode);
  const setMode = useCopilotMode((s) => s.setMode);
  const active = MODES.find((m) => m.id === mode) ?? MODES[0];
  return (
    <div className="px-3 pt-2 border-t border-[var(--border-color)]">
      <div className="flex border border-[var(--border-color)] rounded overflow-hidden" role="group" aria-label="Copilot mode">
        {MODES.map((m) => (
          <button
            key={m.id}
            type="button"
            onClick={() => setMode(m.id)}
            aria-pressed={mode === m.id}
            title={m.description}
            className={`flex-1 px-2 py-1 text-[11px] font-mono uppercase tracking-wide border-r last:border-r-0 border-[var(--border-color)] ${
              mode === m.id ? 'bg-accent/20 text-accent' : 'text-text-muted hover:bg-surface-3'
            }`}
            data-testid={`copilot-mode-${m.id}`}
          >
            {m.label}
          </button>
        ))}
      </div>
      <p className="mt-1 text-[10px] text-text-muted" data-testid="copilot-mode-desc">{active.description}</p>
    </div>
  );
};

export const CopilotPanel: React.FC = () => {
  const activeId = useCopilotSessions((s) => s.activeId);
  const entries = useCopilotSessions((s) => s.sessions.find((x) => x.id === s.activeId)?.entries ?? EMPTY);
  const busy = useCopilotSessions((s) => !!s.busy[s.activeId]);
  const [input, setInput] = useState('');
  const scrollRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    scrollRef.current?.scrollTo({ top: scrollRef.current.scrollHeight });
  }, [entries, busy, activeId]);

  // Always targets an explicit session: the reply is appended to the session that
  // asked, even if the user switches or starts another chat while it is in flight.
  const send = useCallback(async (sessionId: string, text: string, replay: boolean, replayTurnId?: string) => {
    const message = text.trim();
    const store = useCopilotSessions.getState();
    const session = store.sessions.find((s) => s.id === sessionId);
    if (!message || !session || store.busy[sessionId]) return;
    const mode = useCopilotMode.getState().mode;

    // History is prior turns only: the backend appends `message` itself. When
    // replaying after an approval, the message and its "Paused" reply are already
    // in the list, so cut them off or the model sees the request twice.
    let prior = session.entries;
    if (replay) {
      let lastUser = -1;
      prior.forEach((e, i) => { if (e.role === 'user') lastUser = i; });
      prior = lastUser >= 0 ? prior.slice(0, lastUser) : prior;
    }
    const history: CopilotMessageDto[] = prior
      .filter((e) => !e.error)
      .map((e) => ({ role: e.role, content: e.content }));
    // One id per user message. Approvals and cached tool results are scoped to it,
    // so asking for the same order again later is a new turn, not a replay.
    const turnId = replay && replayTurnId ? replayTurnId : crypto.randomUUID();

    if (!replay) {
      store.append(sessionId, { role: 'user', content: message });
      setInput('');
    }
    store.setBusy(sessionId, true);
    try {
      const reply = await copilotChat(message, history, turnId, mode);
      store.append(sessionId, {
        role: 'assistant',
        content: reply.reply,
        model: reply.model,
        provider: reply.provider,
        toolCalls: reply.tool_calls,
        pendingApprovals: reply.pending_approvals,
        approvalState: reply.pending_approvals.length > 0 ? 'pending' : undefined,
        usedMode: reply.mode && reply.mode !== mode ? reply.mode : undefined,
        prompt: message,
        turnId,
      });
    } catch (err) {
      store.append(sessionId, {
        role: 'assistant',
        content: typeof err === 'string' ? err : err instanceof Error ? err.message : 'Copilot request failed',
        error: true,
      });
    } finally {
      store.setBusy(sessionId, false);
    }
  }, []);

  const handleApprove = useCallback(
    (entry: ChatEntry) => {
      const sessionId = useCopilotSessions.getState().activeId;
      const keys = (entry.pendingApprovals ?? []).map((a) => a.key);
      const prompt = entry.prompt ?? '';
      const turnId = entry.turnId;
      useCopilotSessions.getState().updateEntry(sessionId, entry.id, { approvalState: 'approved' });
      void approveCopilotTools(keys)
        .then(() => {
          if (prompt) return send(sessionId, prompt, true, turnId);
        })
        .catch(() => undefined);
    },
    [send],
  );

  const handleDeny = useCallback((entry: ChatEntry) => {
    const store = useCopilotSessions.getState();
    store.updateEntry(store.activeId, entry.id, { approvalState: 'denied' });
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
                onClick={() => void send(activeId, s, false)}
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
                {entry.usedMode && (
                  <div className="mt-1 text-[10px] text-warning font-mono" data-testid="copilot-used-mode">
                    ran in {entry.usedMode} mode
                  </div>
                )}
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

      <ModeSwitch />
      <form
        onSubmit={(e) => {
          e.preventDefault();
          void send(activeId, input, false);
        }}
        className="px-3 py-2 flex items-center gap-2"
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
