import React, { useCallback, useEffect, useState } from 'react';
import { vaultInit, vaultStatus, vaultUnlock } from '../../lib/tauri';
import { useVaultStore } from '../../stores/vaultStore';

const MIN_PASSPHRASE = 8;

/**
 * Full-screen gate shown when the credential vault is uninitialized or locked.
 * Paper trading and free market data work without it, so it can be dismissed.
 */
export const VaultGate: React.FC = () => {
  const status = useVaultStore((s) => s.status);
  const dismissed = useVaultStore((s) => s.dismissed);
  const setStatus = useVaultStore((s) => s.setStatus);
  const dismiss = useVaultStore((s) => s.dismiss);
  const [passphrase, setPassphrase] = useState('');
  const [confirm, setConfirm] = useState('');
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    vaultStatus()
      .then((result) => setStatus(result.status))
      .catch(() => setStatus('unlocked')); // no vault backend (web preview)
  }, [setStatus]);

  const creating = status === 'uninitialized';
  const mismatch = creating && confirm.length > 0 && confirm !== passphrase;
  const canSubmit = passphrase.length >= (creating ? MIN_PASSPHRASE : 1) && (!creating || confirm === passphrase) && !busy;

  const submit = useCallback(async (event: React.FormEvent) => {
    event.preventDefault();
    if (!canSubmit) return;
    setBusy(true);
    setError(null);
    try {
      const result = creating ? await vaultInit(passphrase) : await vaultUnlock(passphrase);
      setStatus(result.status);
      setPassphrase('');
      setConfirm('');
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setBusy(false);
    }
  }, [canSubmit, creating, passphrase, setStatus]);

  if (status === null || status === 'unlocked' || dismissed) return null;

  return (
    <div className="fixed inset-0 z-[100] flex items-center justify-center bg-black/80" data-testid="vault-gate">
      <form onSubmit={submit} className="w-[420px] max-w-[92vw] border border-[var(--border-color)] bg-surface-1 p-5">
        <h2 className="text-sm font-semibold uppercase tracking-wider text-accent">
          {creating ? 'Create credential vault' : 'Unlock credential vault'}
        </h2>
        <p className="mt-2 text-xs text-text-muted">
          {creating
            ? 'Broker and API keys you enter in the app are encrypted with this passphrase. It is never stored, so it cannot be recovered if lost.'
            : 'Enter your passphrase to bring your saved broker and API-key adapters online.'}
        </p>
        <input
          type="password"
          autoFocus
          value={passphrase}
          onChange={(e) => setPassphrase(e.target.value)}
          placeholder="Passphrase"
          aria-label="Passphrase"
          autoComplete={creating ? 'new-password' : 'current-password'}
          data-testid="vault-passphrase-input"
          className="mt-4 w-full border border-[var(--border-color)] bg-surface-0 px-2 py-2 font-mono text-sm text-text-primary focus:border-accent focus:outline-none"
        />
        {creating && (
          <>
            <input
              type="password"
              value={confirm}
              onChange={(e) => setConfirm(e.target.value)}
              placeholder="Confirm passphrase"
              aria-label="Confirm passphrase"
              autoComplete="new-password"
              data-testid="vault-confirm-input"
              className="mt-2 w-full border border-[var(--border-color)] bg-surface-0 px-2 py-2 font-mono text-sm text-text-primary focus:border-accent focus:outline-none"
            />
            <p className={`mt-1 text-[11px] ${mismatch ? 'text-bear' : 'text-text-muted'}`}>
              {mismatch ? 'Passphrases do not match.' : `At least ${MIN_PASSPHRASE} characters.`}
            </p>
          </>
        )}
        {error && <p role="alert" className="mt-2 text-xs text-bear" data-testid="vault-error">{error}</p>}
        <div className="mt-4 flex items-center justify-between">
          <button type="button" onClick={dismiss} data-testid="vault-skip"
            className="text-xs text-text-muted underline hover:text-text-primary">
            Continue with paper trading only
          </button>
          <button type="submit" disabled={!canSubmit} data-testid="vault-submit"
            className="border border-accent bg-accent/10 px-4 py-1.5 text-xs font-semibold uppercase tracking-wider text-accent hover:bg-accent/20 disabled:cursor-not-allowed disabled:opacity-40">
            {busy ? 'Working…' : creating ? 'Create vault' : 'Unlock'}
          </button>
        </div>
      </form>
    </div>
  );
};
