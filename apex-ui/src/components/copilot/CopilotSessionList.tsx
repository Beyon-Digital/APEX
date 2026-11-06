import React, { useState } from 'react';
import { useCopilotSessions, type ChatSession } from '../../stores/copilotSessions';

const SessionRow: React.FC<{ session: ChatSession; active: boolean; onPick: () => void }> = ({ session, active, onPick }) => {
  const rename = useCopilotSessions((s) => s.rename);
  const remove = useCopilotSessions((s) => s.remove);
  const [editing, setEditing] = useState(false);
  const [draft, setDraft] = useState(session.title);
  const [confirming, setConfirming] = useState(false);

  const commit = () => {
    rename(session.id, draft);
    setEditing(false);
  };

  const iconBtn = 'px-1.5 py-0.5 text-[10px] font-mono text-text-muted hover:text-text-primary hover:bg-surface-3 rounded';

  return (
    <div
      className={`flex items-center gap-1 px-2 py-1.5 border-b border-[var(--border-color)] ${active ? 'bg-accent/10' : 'hover:bg-surface-2'}`}
      aria-current={active ? 'true' : undefined}
      data-testid="copilot-session-item"
    >
      {editing ? (
        <input
          autoFocus
          value={draft}
          onChange={(e) => setDraft(e.target.value)}
          onBlur={commit}
          onKeyDown={(e) => {
            if (e.key === 'Enter') commit();
            if (e.key === 'Escape') setEditing(false);
          }}
          className="flex-1 min-w-0 px-1.5 py-0.5 text-xs bg-surface-0 border border-accent rounded focus:outline-none"
          data-testid="copilot-session-rename-input"
        />
      ) : (
        <button
          type="button"
          onClick={onPick}
          className={`flex-1 min-w-0 text-left text-xs truncate ${active ? 'text-accent' : 'text-text-primary'}`}
          title={session.title}
          data-testid="copilot-session-open"
        >
          {session.title}
        </button>
      )}
      {!editing && !confirming && (
        <>
          <button
            type="button"
            className={iconBtn}
            onClick={() => { setDraft(session.title); setEditing(true); }}
            data-testid="copilot-session-rename"
          >
            Rename
          </button>
          <button type="button" className={iconBtn} onClick={() => setConfirming(true)} data-testid="copilot-session-delete">
            Delete
          </button>
        </>
      )}
      {confirming && (
        <>
          <span className="text-[10px] text-bear">Delete?</span>
          <button
            type="button"
            className="px-1.5 py-0.5 text-[10px] font-mono text-bear border border-bear/40 rounded hover:bg-bear/10"
            onClick={() => remove(session.id)}
            data-testid="copilot-session-delete-confirm"
          >
            Yes
          </button>
          <button type="button" className={iconBtn} onClick={() => setConfirming(false)} data-testid="copilot-session-delete-cancel">
            No
          </button>
        </>
      )}
    </div>
  );
};

/** Overlay listing all chats, most recently updated first. */
export const CopilotSessionList: React.FC<{ onClose: () => void }> = ({ onClose }) => {
  const sessions = useCopilotSessions((s) => s.sessions);
  const activeId = useCopilotSessions((s) => s.activeId);
  const switchTo = useCopilotSessions((s) => s.switchTo);
  const sorted = [...sessions].sort((a, b) => b.updatedAt - a.updatedAt);

  return (
    <div
      className="absolute inset-x-0 top-0 z-10 max-h-[70%] overflow-auto bg-surface-1 border-b border-[var(--border-color)] shadow-xl"
      data-testid="copilot-sessions"
    >
      {sorted.map((s) => (
        <SessionRow
          key={s.id}
          session={s}
          active={s.id === activeId}
          onPick={() => {
            switchTo(s.id);
            onClose();
          }}
        />
      ))}
    </div>
  );
};
