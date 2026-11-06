import React, { useCallback, useEffect, useState } from 'react';
import {
  chatgptListModels, chatgptSetPreferences, chatgptSignIn, chatgptSignOut, chatgptStatus,
} from '../../lib/tauri';
import type { ChatGptModelDto, ChatGptStatusDto } from '../../lib/types';
import { useVaultStore } from '../../stores/vaultStore';

const errorText = (err: unknown) => (err instanceof Error ? err.message : String(err));

/**
 * "Continue with ChatGPT": lets the Copilot run on the user's ChatGPT plan allowance
 * instead of an API key. The browser handles the login; APEX only receives a token
 * scoped to API calls, never the user's conversations.
 */
export const ChatGptCard: React.FC = () => {
  const [status, setStatus] = useState<ChatGptStatusDto | null>(null);
  const [models, setModels] = useState<ChatGptModelDto[]>([]);
  const [url, setUrl] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const refresh = useCallback(async () => {
    try {
      setStatus(await chatgptStatus());
    } catch (err) {
      setError(errorText(err));
    }
  }, []);

  useEffect(() => { void refresh(); }, [refresh]);

  // Poll only while the browser sign-in is in flight.
  useEffect(() => {
    if (status?.state !== 'waiting') return;
    const timer = setInterval(() => void refresh(), 1500);
    return () => clearInterval(timer);
  }, [refresh, status?.state]);

  const loadModels = useCallback(async () => {
    try {
      setModels(await chatgptListModels());
      setError(null);
    } catch (err) {
      setError(errorText(err));
    }
  }, []);

  useEffect(() => {
    if (status?.state === 'connected' && status.plan_usage && models.length === 0) void loadModels();
  }, [status?.state, status?.plan_usage, models.length, loadModels]);

  // The copilot needs a concrete model; pick the account's first listed one until the user chooses.
  useEffect(() => {
    if (status?.state === 'connected' && status.plan_usage && !status.model && models.length > 0) {
      void chatgptSetPreferences(undefined, models[0].slug).then(() => refresh()).catch(() => undefined);
    }
  }, [status?.state, status?.plan_usage, status?.model, models, refresh]);

  const run = async (action: () => Promise<unknown>) => {
    setBusy(true);
    setError(null);
    try {
      await action();
      await refresh();
    } catch (err) {
      setError(errorText(err));
    } finally {
      setBusy(false);
    }
  };

  const signIn = () => run(async () => { setUrl(await chatgptSignIn()); });
  const signOut = () => run(async () => { await chatgptSignOut(); setUrl(null); setModels([]); });
  const setUse = (use: boolean) => run(() => chatgptSetPreferences(use, undefined));
  const setModel = (model: string) => run(() => chatgptSetPreferences(undefined, model));

  const state = status?.state ?? 'signed_out';
  const connected = state === 'connected';

  return (
    <div className="border border-[var(--border-color)] bg-surface-0 p-3" data-testid="chatgpt-card">
      <div className="flex items-start justify-between gap-3">
        <div className="min-w-0">
          <div className="flex items-center gap-2">
            <h5 className="text-xs font-semibold text-text-primary">ChatGPT plan</h5>
            <span
              className={`border px-1 text-[9px] uppercase tracking-wider ${connected && status?.plan_usage ? 'border-bull text-bull' : state === 'waiting' ? 'border-warning text-warning' : 'border-[var(--border-color)] text-text-muted'}`}
              data-testid="chatgpt-status"
            >
              {state.replace('_', ' ')}
            </span>
            {connected && status?.email && <span className="text-[11px] text-text-muted">{status.email}</span>}
          </div>
          <p className="mt-1 text-[11px] text-text-muted">
            Run the Copilot on your ChatGPT Plus, Pro or Go allowance instead of an API key. You sign in on OpenAI&apos;s own page; APEX only gets
            a token for API calls and never sees your chats. Usage counts against a weekly cap you control in ChatGPT Settings &gt; Usage.
          </p>
        </div>
        <div className="shrink-0">
          {connected ? (
            <button type="button" onClick={() => void signOut()} disabled={busy} data-testid="chatgpt-sign-out"
              className="border border-[var(--border-color)] px-3 py-1 text-[11px] text-text-secondary hover:text-bear disabled:opacity-40">Sign out</button>
          ) : state === 'locked' ? (
            <button type="button" onClick={() => useVaultStore.setState({ dismissed: false })} data-testid="chatgpt-unlock"
              className="border border-accent px-3 py-1 text-[11px] text-accent">Unlock vault…</button>
          ) : (
            <button type="button" onClick={() => void signIn()} disabled={busy || state === 'waiting'} data-testid="chatgpt-sign-in"
              className="border border-accent bg-accent/10 px-3 py-1 text-[11px] font-semibold text-accent hover:bg-accent/20 disabled:opacity-40">
              {state === 'waiting' ? 'Waiting for browser…' : 'Continue with ChatGPT'}
            </button>
          )}
        </div>
      </div>

      {state === 'waiting' && url && (
        <p className="mt-2 break-all text-[10px] text-text-muted" data-testid="chatgpt-waiting">
          Finish signing in in your browser. If it did not open,{' '}
          <button type="button" className="underline hover:text-text-primary" onClick={() => void navigator.clipboard?.writeText(url)}>copy the link</button>.
        </p>
      )}

      {connected && status && !status.plan_usage && (
        <p className="mt-2 text-[11px] text-warning" role="alert" data-testid="chatgpt-no-plan">
          {status.message ?? 'Plan usage was not granted. Sign out, then sign in again and allow plan usage.'}
        </p>
      )}

      {connected && status?.plan_usage && (
        <div className="mt-3 space-y-2">
          <label className="flex items-center gap-2 text-[11px] text-text-secondary">
            <input type="checkbox" checked={status.use_for_copilot} disabled={busy}
              onChange={(e) => void setUse(e.target.checked)} data-testid="chatgpt-use-plan" />
            Use my ChatGPT plan for the Copilot
          </label>
          <label className="flex items-center gap-2 text-[11px] text-text-secondary">
            Model
            <select value={status.model ?? ''} disabled={busy || models.length === 0} onChange={(e) => void setModel(e.target.value)}
              className="min-w-[180px] border border-[var(--border-color)] bg-surface-1 px-2 py-1 text-xs text-text-primary" data-testid="chatgpt-model">
              {models.length === 0 && <option value="">Loading…</option>}
              {models.map((m) => <option key={m.slug} value={m.slug}>{m.name}</option>)}
            </select>
          </label>
        </div>
      )}

      {(error || (state === 'error' && status?.message)) && (
        <p className="mt-2 text-[11px] text-bear" role="alert" data-testid="chatgpt-error">{error ?? status?.message}</p>
      )}
    </div>
  );
};
