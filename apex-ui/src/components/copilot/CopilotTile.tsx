import React, { useState } from 'react';
import type { CopilotMode } from '../../lib/tauri';
import { useCopilotMode, useCopilotSessions } from '../../stores/copilotSessions';
import { CopilotPanel } from './CopilotPanel';
import { CopilotSessionList } from './CopilotSessionList';

const MODES: { id: CopilotMode; label: string; description: string }[] = [
  { id: 'ask', label: 'Ask', description: 'Read-only: answers from data, changes nothing' },
  { id: 'review', label: 'Review', description: 'Asks before each change' },
  { id: 'agent', label: 'Agent', description: 'Acts on its own; live-broker actions still ask you' },
];

const ModeSwitch: React.FC = () => {
  const mode = useCopilotMode((s) => s.mode);
  const setMode = useCopilotMode((s) => s.setMode);
  const active = MODES.find((m) => m.id === mode) ?? MODES[0];
  return (
    <div className="flex items-center gap-2 px-2 pb-1.5">
      <div className="flex shrink-0 overflow-hidden border border-[var(--border-color)]" role="group" aria-label="Copilot mode">
        {MODES.map((m) => (
          <button
            key={m.id}
            type="button"
            onClick={() => setMode(m.id)}
            aria-pressed={mode === m.id}
            title={m.description}
            className={`border-r border-[var(--border-color)] px-2 py-0.5 font-mono text-[10px] uppercase tracking-wide last:border-r-0 ${
              mode === m.id ? 'bg-accent/20 text-accent' : 'text-text-muted hover:bg-surface-3'
            }`}
            data-testid={`copilot-mode-${m.id}`}
          >
            {m.label}
          </button>
        ))}
      </div>
      <p className="min-w-0 truncate text-[10px] text-text-muted" title={active.description} data-testid="copilot-mode-desc">
        {active.description}
      </p>
    </div>
  );
};

/** The Copilot as a normal tile in the workspace grid (or the focused panel). */
export const CopilotTile: React.FC = () => {
  const [sessionsOpen, setSessionsOpen] = useState(false);
  const title = useCopilotSessions((s) => s.sessions.find((x) => x.id === s.activeId)?.title ?? 'New chat');
  const newChat = useCopilotSessions((s) => s.newChat);
  const btn = 'h-5 px-1.5 flex items-center rounded text-[10px] font-mono text-text-muted hover:text-text-primary hover:bg-surface-3';

  return (
    <div className="flex h-full min-h-0 flex-col bg-surface-1" data-testid="copilot-tile">
      <div className="shrink-0 border-b border-[var(--border-color)] bg-surface-1">
        <div className="flex items-center justify-between gap-2 px-2 py-1">
          <span className="min-w-0 truncate text-xs font-medium text-text-primary" title={title} data-testid="copilot-session-title">{title}</span>
          <div className="flex shrink-0 items-center gap-1">
            <button
              type="button"
              onClick={() => setSessionsOpen((v) => !v)}
              aria-expanded={sessionsOpen}
              className={`${btn} ${sessionsOpen ? 'text-accent' : ''}`}
              title="Chat history"
              data-testid="copilot-sessions-toggle"
            >
              History
            </button>
            <button
              type="button"
              onClick={() => { newChat(); setSessionsOpen(false); }}
              className={btn}
              title="New chat"
              data-testid="copilot-new-chat"
            >
              + New
            </button>
          </div>
        </div>
        <ModeSwitch />
      </div>
      <div className="relative min-h-0 flex-1">
        {sessionsOpen && <CopilotSessionList onClose={() => setSessionsOpen(false)} />}
        <CopilotPanel />
      </div>
    </div>
  );
};
