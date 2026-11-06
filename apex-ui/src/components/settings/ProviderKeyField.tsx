import React, { useState } from 'react';
import { saveAdapterConfig } from '../../lib/tauri';
import { useVaultStore } from '../../stores/vaultStore';

const errorText = (err: unknown) => (err instanceof Error ? err.message : String(err));

interface Props {
  providerId: string;
  /** The vault already holds a key for this provider. */
  stored: boolean;
  /** Called after the key is saved or removed so the parent can refresh its "key set" badge. */
  onChanged: () => void | Promise<void>;
}

/**
 * API key entry for one AI provider, shown inside the provider's own card so there is
 * no second trip to another tab. The key goes straight to the encrypted vault and is
 * never part of the settings file or the draft.
 */
export const ProviderKeyField: React.FC<Props> = ({ providerId, stored: storedInSettings, onChanged }) => {
  const vault = useVaultStore((s) => s.status);
  const unlocked = vault === 'unlocked';
  const [value, setValue] = useState('');
  // A provider added in this session is not in the saved settings yet, so remember our own save.
  const [savedHere, setSavedHere] = useState<boolean | null>(null);
  const stored = savedHere ?? storedInSettings;
  const [busy, setBusy] = useState(false);
  const [notice, setNotice] = useState<{ ok: boolean; text: string } | null>(null);
  const testId = (name: string) => `llm-key-${name}-${providerId}`;

  const run = async (request: { secrets: Record<string, string>; clear: string[] }, okText: string, nowStored: boolean) => {
    setBusy(true);
    setNotice(null);
    try {
      await saveAdapterConfig({
        adapter_id: `llm:${providerId}`, enabled: true, settings: {},
        secrets: request.secrets, clear_secrets: request.clear,
      });
      setValue('');
      setSavedHere(nowStored);
      setNotice({ ok: true, text: okText });
      await onChanged();
    } catch (err) {
      setNotice({ ok: false, text: errorText(err) });
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="col-span-2 space-y-1" data-testid={testId('field')}>
      <label className="block text-[11px] text-text-secondary">
        API key
        <div className="mt-1 flex items-center gap-2">
          <input
            type="password"
            autoComplete="off"
            spellCheck={false}
            value={value}
            // readOnly rather than disabled, so the field can still be focused and explains itself.
            readOnly={!unlocked}
            aria-readonly={!unlocked}
            onChange={(e) => { if (unlocked) setValue(e.target.value); }}
            placeholder={stored ? '•••••••• saved (type to replace)' : 'Paste API key'}
            data-testid={testId('input')}
            className="min-w-0 flex-1 border border-[var(--border-color)] bg-surface-1 px-2 py-1 font-mono text-xs text-text-primary focus:border-accent focus:outline-none read-only:opacity-60"
          />
          <button type="button" disabled={busy || !unlocked || value.trim() === ''}
            onClick={() => void run({ secrets: { api_key: value }, clear: [] }, 'Key saved.', true)}
            data-testid={testId('save')}
            className="shrink-0 border border-accent bg-accent/10 px-3 py-1 text-[11px] font-semibold uppercase tracking-wider text-accent hover:bg-accent/20 disabled:cursor-not-allowed disabled:opacity-40">
            Save key
          </button>
          {stored && (
            <button type="button" disabled={busy || !unlocked}
              onClick={() => void run({ secrets: {}, clear: ['api_key'] }, 'Key removed.', false)}
              data-testid={testId('remove')}
              className="shrink-0 border border-[var(--border-color)] px-2 py-1 text-[11px] text-text-muted hover:text-bear disabled:opacity-40">
              Remove
            </button>
          )}
        </div>
      </label>
      {!unlocked && (
        <p className="flex items-center gap-2 text-[11px] text-warning" data-testid={testId('locked')}>
          The credential vault is locked, so the key can't be saved yet.
          <button type="button" onClick={() => useVaultStore.setState({ dismissed: false })}
            data-testid={testId('unlock')} className="border border-accent px-2 py-0.5 text-accent hover:bg-accent/10">
            Unlock vault…
          </button>
        </p>
      )}
      {notice && <p role="status" data-testid={testId('notice')} className={`text-[11px] ${notice.ok ? 'text-bull' : 'text-bear'}`}>{notice.text}</p>}
    </div>
  );
};
