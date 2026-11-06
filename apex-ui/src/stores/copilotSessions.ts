import { create } from 'zustand';
import type { CopilotMode } from '../lib/tauri';
import type { CopilotMessageDto, PendingApprovalDto, ToolCallTraceDto } from '../lib/types';

export interface ChatEntry extends CopilotMessageDto {
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
  /** Mode the backend reports when it differs from the one the user selected. */
  usedMode?: string;
  error?: boolean;
}

export interface ChatSession {
  id: string;
  title: string;
  createdAt: number;
  updatedAt: number;
  entries: ChatEntry[];
}

export const SESSIONS_KEY = 'apex.copilot.sessions.v1';
export const LEGACY_HISTORY_KEY = 'apex.copilot.history';
export const MODE_KEY = 'apex.copilot.mode';
export const DEFAULT_TITLE = 'New chat';
export const MAX_SESSIONS = 30;
export const MAX_ENTRIES = 100;
const TITLE_LENGTH = 40;

export const DEFAULT_MODE: CopilotMode = 'agent';

function makeSession(entries: ChatEntry[] = [], title = DEFAULT_TITLE): ChatSession {
  const now = Date.now();
  return { id: crypto.randomUUID(), title, createdAt: now, updatedAt: now, entries };
}

export function titleFrom(text: string): string {
  const t = text.trim().replace(/\s+/g, ' ');
  if (!t) return DEFAULT_TITLE;
  return t.length > TITLE_LENGTH ? `${t.slice(0, TITLE_LENGTH - 1).trimEnd()}…` : t;
}

/** Keeps the MAX_SESSIONS most recently updated sessions (always including the active one) and MAX_ENTRIES entries each. */
export function capSessions(sessions: ChatSession[], activeId: string): ChatSession[] {
  let kept = sessions.map((s) => (s.entries.length > MAX_ENTRIES ? { ...s, entries: s.entries.slice(-MAX_ENTRIES) } : s));
  if (kept.length > MAX_SESSIONS) {
    const keep = new Set(
      [...kept]
        .sort((a, b) => (a.id === activeId ? -1 : b.id === activeId ? 1 : b.updatedAt - a.updatedAt))
        .slice(0, MAX_SESSIONS)
        .map((s) => s.id),
    );
    kept = kept.filter((s) => keep.has(s.id));
  }
  return kept;
}

function isEntry(e: unknown): e is ChatEntry {
  if (!e || typeof e !== 'object') return false;
  const o = e as Record<string, unknown>;
  return typeof o.id === 'number' && (o.role === 'user' || o.role === 'assistant') && typeof o.content === 'string';
}

function parseSessions(raw: string | null): { sessions: ChatSession[]; activeId: string } | null {
  if (!raw) return null;
  try {
    const parsed = JSON.parse(raw) as { sessions?: unknown; activeId?: unknown };
    if (!Array.isArray(parsed.sessions)) return null;
    const sessions: ChatSession[] = [];
    for (const s of parsed.sessions as Partial<ChatSession>[]) {
      if (!s || typeof s.id !== 'string' || !Array.isArray(s.entries)) continue;
      sessions.push({
        id: s.id,
        title: typeof s.title === 'string' && s.title ? s.title : DEFAULT_TITLE,
        createdAt: typeof s.createdAt === 'number' ? s.createdAt : Date.now(),
        updatedAt: typeof s.updatedAt === 'number' ? s.updatedAt : Date.now(),
        entries: s.entries.filter(isEntry),
      });
    }
    if (sessions.length === 0) return null;
    const activeId = sessions.some((s) => s.id === parsed.activeId) ? (parsed.activeId as string) : sessions[0].id;
    return { sessions, activeId };
  } catch {
    return null;
  }
}

/** Reads persisted sessions, migrating the legacy single-history key once. Pure apart from localStorage. */
export function loadSessions(): { sessions: ChatSession[]; activeId: string } {
  let stored: ReturnType<typeof parseSessions> = null;
  try {
    stored = parseSessions(localStorage.getItem(SESSIONS_KEY));
  } catch {
    /* storage unavailable */
  }
  if (stored) return stored;

  let legacy: ChatEntry[] = [];
  try {
    const raw = localStorage.getItem(LEGACY_HISTORY_KEY);
    if (raw) {
      const parsed = JSON.parse(raw) as unknown;
      if (Array.isArray(parsed)) legacy = parsed.filter(isEntry);
    }
  } catch {
    /* corrupt legacy history is dropped */
  }
  const first = legacy.find((e) => e.role === 'user');
  const session = makeSession(legacy.slice(-MAX_ENTRIES), first ? titleFrom(first.content) : DEFAULT_TITLE);
  if (legacy.length > 0) {
    // Persist before removing the legacy key so a quota failure cannot lose the history.
    if (persist([session], session.id)) {
      try {
        localStorage.removeItem(LEGACY_HISTORY_KEY);
      } catch {
        /* ignore */
      }
    }
  }
  return { sessions: [session], activeId: session.id };
}

/** Writes sessions, dropping the oldest ones if the quota is exceeded. Returns false if nothing could be stored. */
export function persist(sessions: ChatSession[], activeId: string): boolean {
  let toWrite = capSessions(sessions, activeId);
  for (;;) {
    try {
      localStorage.setItem(SESSIONS_KEY, JSON.stringify({ sessions: toWrite, activeId }));
      return true;
    } catch {
      if (toWrite.length <= 1) return false;
      const oldest = [...toWrite]
        .filter((s) => s.id !== activeId)
        .sort((a, b) => a.updatedAt - b.updatedAt)[0];
      if (!oldest) return false;
      toWrite = toWrite.filter((s) => s.id !== oldest.id);
    }
  }
}

interface SessionsState {
  sessions: ChatSession[];
  activeId: string;
  /** Session ids with a request in flight (not persisted). */
  busy: Record<string, boolean>;
  /** Starts a fresh session unless the active one is already empty. */
  newChat: () => void;
  switchTo: (id: string) => void;
  rename: (id: string, title: string) => void;
  remove: (id: string) => void;
  /** Appends to the given session, never "the active one", so late replies land where they were asked. */
  append: (sessionId: string, entry: Omit<ChatEntry, 'id'>) => void;
  updateEntry: (sessionId: string, entryId: number, patch: Partial<ChatEntry>) => void;
  removeEntry: (sessionId: string, entryId: number) => void;
  setBusy: (sessionId: string, busy: boolean) => void;
}

const initial = loadSessions();

export const useCopilotSessions = create<SessionsState>((set, get) => ({
  sessions: initial.sessions,
  activeId: initial.activeId,
  busy: {},

  newChat: () => {
    const { sessions, activeId } = get();
    const active = sessions.find((s) => s.id === activeId);
    if (active && active.entries.length === 0) return;
    const fresh = makeSession();
    set({ sessions: capSessions([...sessions, fresh], fresh.id), activeId: fresh.id });
  },

  switchTo: (id) => {
    if (get().sessions.some((s) => s.id === id)) set({ activeId: id });
  },

  rename: (id, title) => {
    const t = title.trim().replace(/\s+/g, ' ').slice(0, 80);
    if (!t) return;
    set((st) => ({ sessions: st.sessions.map((s) => (s.id === id ? { ...s, title: t } : s)) }));
  },

  remove: (id) => {
    const { sessions, activeId } = get();
    const rest = sessions.filter((s) => s.id !== id);
    if (rest.length === sessions.length) return;
    if (rest.length === 0) {
      const fresh = makeSession();
      set({ sessions: [fresh], activeId: fresh.id });
      return;
    }
    if (id !== activeId) {
      set({ sessions: rest });
      return;
    }
    const next = [...rest].sort((a, b) => b.updatedAt - a.updatedAt)[0];
    set({ sessions: rest, activeId: next.id });
  },

  append: (sessionId, entry) => {
    set((st) => ({
      sessions: st.sessions.map((s) => {
        if (s.id !== sessionId) return s;
        const id = s.entries.reduce((m, e) => Math.max(m, e.id), 0) + 1;
        const firstUser = entry.role === 'user' && !s.entries.some((e) => e.role === 'user');
        return {
          ...s,
          title: firstUser && s.title === DEFAULT_TITLE ? titleFrom(entry.content) : s.title,
          updatedAt: Date.now(),
          entries: [...s.entries, { ...entry, id }].slice(-MAX_ENTRIES),
        };
      }),
    }));
  },

  updateEntry: (sessionId, entryId, patch) => {
    set((st) => ({
      sessions: st.sessions.map((s) =>
        s.id === sessionId ? { ...s, entries: s.entries.map((e) => (e.id === entryId ? { ...e, ...patch } : e)) } : s,
      ),
    }));
  },

  removeEntry: (sessionId, entryId) => {
    set((st) => ({
      sessions: st.sessions.map((s) =>
        s.id === sessionId ? { ...s, entries: s.entries.filter((e) => e.id !== entryId) } : s,
      ),
    }));
  },

  setBusy: (sessionId, busy) =>
    set((st) => {
      const next = { ...st.busy };
      if (busy) next[sessionId] = true;
      else delete next[sessionId];
      return { busy: next };
    }),
}));

// Persist only when conversations change, not on transient state such as `busy`.
useCopilotSessions.subscribe((state, prev) => {
  if (state.sessions !== prev.sessions || state.activeId !== prev.activeId) {
    persist(state.sessions, state.activeId);
  }
});

function loadMode(): CopilotMode {
  try {
    const v = localStorage.getItem(MODE_KEY);
    if (v === 'ask' || v === 'review' || v === 'agent') return v;
  } catch {
    /* storage unavailable */
  }
  return DEFAULT_MODE;
}

interface ModeState {
  mode: CopilotMode;
  setMode: (mode: CopilotMode) => void;
}

export const useCopilotMode = create<ModeState>((set) => ({
  mode: loadMode(),
  setMode: (mode) => {
    try {
      localStorage.setItem(MODE_KEY, mode);
    } catch {
      /* choice still applies for this session */
    }
    set({ mode });
  },
}));
