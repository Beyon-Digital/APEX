import React, { useCallback, useLayoutEffect, useRef, useState } from 'react';
import { approveCopilotTools, copilotChat } from '../../lib/tauri';
import type { CopilotMessageDto } from '../../lib/types';
import { useCopilotMode, useCopilotSessions, type ChatEntry } from '../../stores/copilotSessions';
import { CopyButton, Markdown } from './Markdown';
import { ToolGroup } from './ToolGroup';
import { TOOL_LABELS } from './toolLabels';

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

/** Composer grows to about six lines, then scrolls. */
const COMPOSER_MAX_PX = 136;
/** Closer than this to the bottom counts as "following the conversation". */
const FOLLOW_THRESHOLD_PX = 48;

const EMPTY: ChatEntry[] = [];

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

const MessageRow: React.FC<{
  entry: ChatEntry;
  canRetry: boolean;
  onApprove: (entry: ChatEntry) => void;
  onDeny: (entry: ChatEntry) => void;
  onRetry: (entry: ChatEntry) => void;
}> = ({ entry, canRetry, onApprove, onDeny, onRetry }) => {
  if (entry.role === 'user') {
    return (
      <div className="flex justify-end">
        <div
          className="max-w-[88%] whitespace-pre-wrap break-words border border-accent/30 bg-accent/10 px-3 py-1.5 text-[13px] text-text-primary"
          data-testid="copilot-user-msg"
        >
          {entry.content}
        </div>
      </div>
    );
  }
  return (
    <div className="group flex flex-col items-start gap-0.5">
      <div className="font-mono text-[10px] uppercase tracking-widest text-accent">Copilot</div>
      <div
        className={`w-full min-w-0 border-l-2 pl-3 ${entry.error ? 'border-bear text-bear' : 'border-[var(--border-color)]'}`}
        data-testid="copilot-assistant-msg"
      >
        {entry.toolCalls && entry.toolCalls.length > 0 && <ToolGroup calls={entry.toolCalls} />}
        {entry.pendingApprovals && entry.pendingApprovals.length > 0 && (
          <ApprovalCard entry={entry} onApprove={onApprove} onDeny={onDeny} />
        )}
        {entry.error ? (
          <p className="whitespace-pre-wrap break-words text-[13px]">{entry.content}</p>
        ) : (
          <Markdown text={entry.content} />
        )}
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
      <div className="flex items-center gap-1.5 pl-3">
        {!entry.error && <CopyButton text={entry.content} testId="copilot-copy-msg" />}
        {entry.error && canRetry && (
          <button
            type="button"
            onClick={() => onRetry(entry)}
            className="rounded border border-bear/50 px-1.5 py-px font-mono text-[10px] text-bear hover:bg-bear/10"
            data-testid="copilot-retry"
          >
            Retry
          </button>
        )}
      </div>
    </div>
  );
};

export const CopilotPanel: React.FC = () => {
  const activeId = useCopilotSessions((s) => s.activeId);
  const entries = useCopilotSessions((s) => s.sessions.find((x) => x.id === s.activeId)?.entries ?? EMPTY);
  const busy = useCopilotSessions((s) => !!s.busy[s.activeId]);
  const [input, setInput] = useState('');
  const scrollRef = useRef<HTMLDivElement>(null);
  const composerRef = useRef<HTMLTextAreaElement>(null);
  /** True while the user is at (or near) the bottom: new messages then scroll into view. */
  const followRef = useRef(true);
  const lastSessionRef = useRef(activeId);
  const [showJump, setShowJump] = useState(false);

  const scrollToEnd = useCallback((smooth = false) => {
    const el = scrollRef.current;
    if (!el) return;
    el.scrollTo({ top: el.scrollHeight, behavior: smooth ? 'smooth' : 'auto' });
    followRef.current = true;
    setShowJump(false);
  }, []);

  const onScroll = useCallback(() => {
    const el = scrollRef.current;
    if (!el) return;
    const atEnd = el.scrollHeight - el.scrollTop - el.clientHeight < FOLLOW_THRESHOLD_PX;
    followRef.current = atEnd;
    setShowJump(!atEnd);
  }, []);

  // New content scrolls into view only if the user was following; otherwise offer "jump to latest".
  useLayoutEffect(() => {
    const switched = lastSessionRef.current !== activeId;
    lastSessionRef.current = activeId;
    if (switched || followRef.current) scrollToEnd();
    else setShowJump(true);
  }, [entries.length, busy, activeId, scrollToEnd]);

  // Grow the composer with its content, up to about six lines.
  useLayoutEffect(() => {
    const el = composerRef.current;
    if (!el) return;
    el.style.height = 'auto';
    el.style.height = `${Math.min(el.scrollHeight, COMPOSER_MAX_PX)}px`;
    el.style.overflowY = el.scrollHeight > COMPOSER_MAX_PX ? 'auto' : 'hidden';
  }, [input]);

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
      followRef.current = true;
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
        prompt: message,
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

  // Retry drops the error reply and replays the user message it answered.
  const handleRetry = useCallback((entry: ChatEntry) => {
    const store = useCopilotSessions.getState();
    const sessionId = store.activeId;
    const session = store.sessions.find((s) => s.id === sessionId);
    if (!session || store.busy[sessionId]) return;
    const idx = session.entries.findIndex((e) => e.id === entry.id);
    const prompt = entry.prompt
      ?? [...session.entries.slice(0, Math.max(idx, 0))].reverse().find((e) => e.role === 'user')?.content;
    if (!prompt) return;
    store.removeEntry(sessionId, entry.id);
    void send(sessionId, prompt, true);
  }, [send]);

  const lastEntry = entries[entries.length - 1];

  return (
    <div className="relative flex h-full min-h-0 flex-col" data-testid="copilot-panel">
      <div ref={scrollRef} onScroll={onScroll} className="min-h-0 flex-1 space-y-4 overflow-y-auto px-3 py-3" data-testid="copilot-scroll">
        {entries.length === 0 ? (
          <div className="space-y-3" data-testid="copilot-empty">
            <p className="text-xs leading-relaxed text-text-muted">
              Ask about positions, quotes, scans, or have it place paper orders, set alerts,
              write strategy files, run backtests, train models, and create automations that
              trade a model's signal on a schedule.
            </p>
            <div className="flex flex-wrap gap-1.5">
              {SUGGESTIONS.map((s) => (
                <button
                  key={s}
                  onClick={() => void send(activeId, s, false)}
                  className="border border-[var(--border-color)] bg-surface-2 px-2 py-1 text-left text-[11px] text-text-secondary hover:border-accent hover:text-accent"
                  data-testid="copilot-suggestion"
                >
                  {s}
                </button>
              ))}
            </div>
          </div>
        ) : (
          entries.map((entry) => (
            <MessageRow
              key={entry.id}
              entry={entry}
              canRetry={entry === lastEntry && !busy}
              onApprove={handleApprove}
              onDeny={handleDeny}
              onRetry={handleRetry}
            />
          ))
        )}
        {busy && (
          <div className="font-mono text-xs text-text-muted" data-testid="copilot-thinking">
            <span className="mr-1.5 inline-block h-1.5 w-1.5 animate-pulse rounded-full bg-accent" />
            Working… (may call tools)
          </div>
        )}
      </div>

      {showJump && (
        <button
          type="button"
          onClick={() => scrollToEnd(true)}
          className="absolute bottom-[4.5rem] right-4 z-10 border border-accent/60 bg-surface-2 px-2 py-1 font-mono text-[10px] text-accent shadow-lg hover:bg-surface-3"
          data-testid="copilot-jump-latest"
        >
          Jump to latest ↓
        </button>
      )}

      <form
        onSubmit={(e) => {
          e.preventDefault();
          void send(activeId, input, false);
        }}
        className="flex shrink-0 items-end gap-2 border-t border-[var(--border-color)] bg-surface-1 px-3 py-2"
      >
        <textarea
          ref={composerRef}
          rows={1}
          value={input}
          onChange={(e) => setInput(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === 'Enter' && !e.shiftKey && !e.nativeEvent.isComposing) {
              e.preventDefault();
              void send(activeId, input, false);
            }
          }}
          placeholder="Ask, or build and test a strategy…"
          title="Enter sends. Shift+Enter adds a line."
          className="min-w-0 flex-1 resize-none border border-[var(--border-color)] bg-surface-0 px-2.5 py-1.5 text-[13px] leading-5 focus:border-accent focus:outline-none"
          style={{ maxHeight: COMPOSER_MAX_PX }}
          data-testid="copilot-input"
        />
        <button
          type="submit"
          disabled={busy || !input.trim()}
          title={busy ? 'Wait for the current reply' : 'Send (Enter)'}
          className="h-8 shrink-0 border border-accent bg-accent/20 px-3 text-xs font-medium text-accent hover:bg-accent/30 disabled:opacity-40"
          data-testid="copilot-send"
        >
          {busy ? 'Working…' : 'Send'}
        </button>
      </form>
    </div>
  );
};
