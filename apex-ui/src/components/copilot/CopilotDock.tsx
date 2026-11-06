import React, { useState } from 'react';
import { useWorkspaceStore } from '../../stores/workspaceStore';
import { useCopilotSessions } from '../../stores/copilotSessions';
import { CopilotPanel } from './CopilotPanel';
import { CopilotSessionList } from './CopilotSessionList';

/**
 * LinkedIn-chats style docked chat: collapsed shows a pill in the bottom-right
 * corner; expanded shows a card anchored to the bottom-right. The panel stays
 * mounted while hidden so conversation history survives collapse.
 */
export const CopilotDock: React.FC = () => {
  const open = useWorkspaceStore((s) => s.copilotOpen);
  const setOpen = useWorkspaceStore((s) => s.setCopilotOpen);
  const [sessionsOpen, setSessionsOpen] = useState(false);
  const title = useCopilotSessions((s) => s.sessions.find((x) => x.id === s.activeId)?.title ?? 'New chat');
  const newChat = useCopilotSessions((s) => s.newChat);
  const btn = 'h-6 px-1.5 flex items-center justify-center rounded text-[11px] font-mono text-text-muted hover:text-text-primary hover:bg-surface-3';

  return (
    <div className="fixed bottom-0 right-4 z-50 flex flex-col items-end pointer-events-none" data-testid="copilot-dock">
      {/* Expanded chat card */}
      <div
        className={`pointer-events-auto flex flex-col w-[400px] h-[560px] max-h-[calc(100vh-120px)] bg-surface-1 border border-[var(--border-color)] border-b-0 rounded-t-lg shadow-2xl overflow-hidden ${open ? '' : 'hidden'}`}
        data-testid="copilot-dock-panel"
        aria-hidden={!open}
      >
        <div className="flex items-center justify-between px-3 py-2 bg-surface-2 border-b border-[var(--border-color)] select-none">
          <div className="flex items-center gap-2 min-w-0">
            <span className="w-2 h-2 rounded-full bg-accent shrink-0" />
            <span className="text-sm font-semibold text-text-primary truncate">Copilot</span>
            <span className="text-[10px] text-text-muted font-mono truncate" title={title} data-testid="copilot-session-title">{title}</span>
          </div>
          <div className="flex items-center gap-1 shrink-0">
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
            <button
              type="button"
              onClick={() => setOpen(false)}
              className="w-6 h-6 flex items-center justify-center rounded text-text-muted hover:text-text-primary hover:bg-surface-3"
              title="Minimize"
              data-testid="copilot-dock-minimize"
            >
              <svg width="12" height="12" viewBox="0 0 12 12" fill="none" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round">
                <path d="M2 8.5 6 4.5l4 4" />
              </svg>
            </button>
            <button
              type="button"
              onClick={() => setOpen(false)}
              className="w-6 h-6 flex items-center justify-center rounded text-text-muted hover:text-text-primary hover:bg-surface-3"
              title="Close"
              data-testid="copilot-dock-close"
            >
              <svg width="12" height="12" viewBox="0 0 12 12" fill="none" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round">
                <path d="M3 3l6 6M9 3l-6 6" />
              </svg>
            </button>
          </div>
        </div>
        <div className="relative flex-1 min-h-0">
          {sessionsOpen && <CopilotSessionList onClose={() => setSessionsOpen(false)} />}
          <CopilotPanel />
        </div>
      </div>

      {/* Collapsed pill — hidden while the card is open */}
      {!open && (
        <button
          type="button"
          onClick={() => setOpen(true)}
          className="pointer-events-auto mb-3 flex items-center gap-2 px-3.5 py-2 rounded-full bg-surface-2 border border-[var(--border-color)] shadow-lg hover:bg-surface-3 hover:border-accent/60 transition-colors"
          data-testid="copilot-dock-toggle"
        >
          <span className="w-2 h-2 rounded-full bg-accent" />
          <span className="text-xs font-semibold text-text-primary">Copilot</span>
          <svg width="10" height="10" viewBox="0 0 12 12" fill="none" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round" className="text-text-muted">
            <path d="M2 7.5 6 3.5l4 4" />
          </svg>
        </button>
      )}
    </div>
  );
};
