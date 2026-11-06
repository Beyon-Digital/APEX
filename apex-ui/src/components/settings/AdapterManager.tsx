import React, { useCallback, useEffect, useMemo, useState } from 'react';
import {
  clearBrokerSession, deleteAdapterConfig, getAdapterConfigs, importEnvCredentials, listAdapterSchemas,
  listBrokerConnections, saveAdapterConfig, setAdapterEnabled, testAdapterConnection, vaultLock, vaultStatus,
  openExternalUrl, zerodhaLogin, zerodhaLoginUrl,
} from '../../lib/tauri';
import type { AdapterConfigDto, AdapterSchemaDto, AdapterTestResultDto, BrokerConnectionDto } from '../../lib/types';
import { useBrokerStore } from '../../stores/brokerStore';
import { useVaultStore } from '../../stores/vaultStore';

const errorText = (err: unknown) => (err instanceof Error ? err.message : String(err));

const STATUS_STYLE: Record<string, string> = {
  connected: 'text-bull border-bull',
  ready: 'text-bull border-bull',
  auth_required: 'text-warning border-warning',
  not_configured: 'text-text-muted border-[var(--border-color)]',
};

interface CardProps {
  schema: AdapterSchemaDto;
  config: AdapterConfigDto;
  connection?: BrokerConnectionDto;
  vaultUnlocked: boolean;
  onChanged: () => Promise<void>;
}

const AdapterCard: React.FC<CardProps> = ({ schema, config, connection, vaultUnlocked, onChanged }) => {
  const id = schema.adapter_id;
  const [settings, setSettings] = useState<Record<string, string>>(config.settings);
  const [secrets, setSecrets] = useState<Record<string, string>>({});
  const [cleared, setCleared] = useState<string[]>([]);
  const [busy, setBusy] = useState(false);
  const [notice, setNotice] = useState<{ ok: boolean; text: string } | null>(null);
  const [testResult, setTestResult] = useState<AdapterTestResultDto | null>(null);
  // Rows start collapsed so the Accounts tab is a short list, not a wall of fields.
  const [expanded, setExpanded] = useState(false);

  // Reset only when the saved values really change, not whenever another card triggers a reload.
  const savedSettingsKey = JSON.stringify(config.settings);
  useEffect(() => setSettings(config.settings), [savedSettingsKey]); // eslint-disable-line react-hooks/exhaustive-deps
  // Typed-but-unsaved secrets must not outlive the vault being locked.
  useEffect(() => {
    if (!vaultUnlocked) {
      setSecrets({});
      setCleared([]);
    }
  }, [vaultUnlocked]);

  const dirty = Object.values(secrets).some((v) => v.trim() !== '')
    || cleared.length > 0
    || JSON.stringify(settings) !== JSON.stringify(config.settings);

  const run = useCallback(async (action: () => Promise<void>, okText?: string) => {
    setBusy(true);
    setNotice(null);
    try {
      await action();
      if (okText) setNotice({ ok: true, text: okText });
    } catch (err) {
      setNotice({ ok: false, text: errorText(err) });
    } finally {
      setBusy(false);
    }
  }, []);

  const save = () => run(async () => {
    await saveAdapterConfig({ adapter_id: id, enabled: config.enabled, settings, secrets, clear_secrets: cleared });
    setSecrets({});
    setCleared([]);
    await onChanged();
  }, 'Saved and applied.');

  const test = () => run(async () => {
    setTestResult(await testAdapterConnection(id));
  });

  const remove = () => run(async () => {
    await deleteAdapterConfig(id);
    setSecrets({});
    setCleared([]);
    await onChanged();
  }, 'Credentials removed.');

  const toggleEnabled = () => run(async () => {
    await setAdapterEnabled(id, !config.enabled);
    await onChanged();
  });

  const status = connection?.status;
  const hasAnySecret = config.secrets_configured.length > 0;

  return (
    <div className="border border-[var(--border-color)] bg-surface-0 p-3" data-testid={`adapter-card-${id}`}>
      <div className="flex items-start justify-between gap-3">
        <button
          type="button"
          onClick={() => setExpanded((open) => !open)}
          aria-expanded={expanded}
          aria-controls={`adapter-body-${id}`}
          data-testid={`adapter-expand-${id}`}
          className="mt-0.5 w-4 shrink-0 text-left text-xs text-text-muted hover:text-accent"
          title={expanded ? 'Collapse' : 'Set up'}
        >
          {expanded ? '▾' : '▸'}
        </button>
        <div className="min-w-0 flex-1">
          <div className="flex items-center gap-2">
            <h5 className="text-xs font-semibold text-text-primary">{schema.display_name}</h5>
            <span className="border border-[var(--border-color)] px-1 text-[9px] uppercase tracking-wider text-text-muted">{schema.category}</span>
            {status && (
              <span className={`border px-1 text-[9px] uppercase tracking-wider ${STATUS_STYLE[status] ?? 'text-bear border-bear'}`} data-testid={`adapter-status-${id}`}>
                {status.replace('_', ' ')}
              </span>
            )}
          </div>
          <p className="mt-1 text-[11px] text-text-muted">
            {expanded ? schema.description : (hasAnySecret ? 'Keys saved.' : 'Not set up.')}{' '}
            {!expanded && <button type="button" onClick={() => setExpanded(true)} className="underline hover:text-text-primary">{hasAnySecret ? 'Edit' : 'Set up'}</button>}
          </p>
        </div>
        <label className="flex shrink-0 items-center gap-1.5 text-[11px] text-text-secondary">
          <input type="checkbox" checked={config.enabled} disabled={busy || !vaultUnlocked} onChange={() => void toggleEnabled()} data-testid={`adapter-enabled-${id}`} />
          Enabled
        </label>
      </div>

      {expanded && (<div id={`adapter-body-${id}`}>
      {!vaultUnlocked && (
        <div className="mt-3 flex items-center justify-between gap-3 border border-warning/50 bg-surface-1 px-2 py-1.5 text-[11px] text-warning" data-testid={`adapter-locked-${id}`}>
          <span>The credential vault is locked, so these fields are read-only.</span>
          <button type="button" onClick={() => useVaultStore.setState({ dismissed: false })} data-testid={`adapter-unlock-${id}`}
            className="shrink-0 border border-accent px-2 py-0.5 text-accent hover:bg-accent/10">Unlock vault…</button>
        </div>
      )}
      <div className="mt-3 grid grid-cols-2 gap-x-3 gap-y-2">
        {schema.fields.map((field) => {
          const testId = `adapter-field-${id}-${field.key}`;
          if (field.kind === 'toggle') {
            return (
              <label key={field.key} className="col-span-2 flex items-start gap-2 text-[11px] text-text-secondary" title={field.help}>
                <input type="checkbox" className="mt-0.5" checked={settings[field.key] === 'true'} data-testid={testId}
                  onChange={(e) => setSettings({ ...settings, [field.key]: e.target.checked ? 'true' : 'false' })} />
                <span>{field.label}{field.help && <span className="block text-text-muted">{field.help}</span>}</span>
              </label>
            );
          }
          const isSecret = field.kind === 'secret';
          const stored = isSecret && config.secrets_configured.includes(field.key) && !cleared.includes(field.key);
          return (
            <label key={field.key} className="min-w-0 text-[11px] text-text-secondary">
              {field.label}{field.required && <span className="text-bear"> *</span>}
              <input
                type={isSecret ? 'password' : 'text'}
                autoComplete="off"
                spellCheck={false}
                value={isSecret ? secrets[field.key] ?? '' : settings[field.key] ?? ''}
                placeholder={stored ? '•••••••• saved' : field.help || field.label}
                data-testid={testId}
                // readOnly, not disabled: a disabled input cannot be focused, so a locked vault
                // looked like a broken form. Read-only fields still take focus and show why.
                readOnly={!vaultUnlocked}
                aria-readonly={!vaultUnlocked}
                title={vaultUnlocked ? undefined : 'Unlock the vault to edit credentials'}
                onChange={(e) => {
                  if (!vaultUnlocked) return;
                  if (isSecret) setSecrets({ ...secrets, [field.key]: e.target.value });
                  else setSettings({ ...settings, [field.key]: e.target.value });
                }}
                className="mt-1 w-full min-w-0 border border-[var(--border-color)] bg-surface-1 px-2 py-1 font-mono text-xs text-text-primary focus:border-accent focus:outline-none read-only:opacity-60"
              />
              {stored && (
                <button type="button" className="mt-0.5 text-[10px] text-text-muted underline hover:text-bear"
                  onClick={() => setCleared([...cleared, field.key])} data-testid={`adapter-clear-${id}-${field.key}`}>
                  Remove saved value
                </button>
              )}
            </label>
          );
        })}
      </div>

      {id === 'zerodha' && <ZerodhaLogin ready={config.secrets_configured.includes('api_key') && config.secrets_configured.includes('api_secret')} connection={connection} onChanged={onChanged} />}

      <div className="mt-3 flex flex-wrap items-center gap-2">
        <button type="button" onClick={() => void save()} disabled={busy || !dirty || !vaultUnlocked} data-testid={`adapter-save-${id}`}
          className="border border-accent bg-accent/10 px-3 py-1 text-[11px] font-semibold uppercase tracking-wider text-accent hover:bg-accent/20 disabled:cursor-not-allowed disabled:opacity-40">
          Save
        </button>
        <button type="button" onClick={() => void test()} disabled={busy || !hasAnySecret} data-testid={`adapter-test-${id}`}
          className="border border-[var(--border-color)] px-3 py-1 text-[11px] text-text-secondary hover:text-text-primary disabled:opacity-40">
          Test
        </button>
        <button type="button" onClick={() => void remove()} disabled={busy || !vaultUnlocked || (!hasAnySecret && Object.keys(config.settings).length === 0)} data-testid={`adapter-remove-${id}`}
          className="border border-[var(--border-color)] px-3 py-1 text-[11px] text-text-muted hover:text-bear disabled:opacity-40">
          Remove
        </button>
        {notice && <span className={`text-[11px] ${notice.ok ? 'text-bull' : 'text-bear'}`} role="status" data-testid={`adapter-notice-${id}`}>{notice.text}</span>}
        {testResult && <span className={`text-[11px] ${testResult.ok ? 'text-bull' : 'text-warning'}`} data-testid={`adapter-test-result-${id}`}>{testResult.message}</span>}
      </div>
      </div>)}
    </div>
  );
};

const ZerodhaLogin: React.FC<{ ready: boolean; connection?: BrokerConnectionDto; onChanged: () => Promise<void> }> = ({ ready, connection, onChanged }) => {
  const [url, setUrl] = useState<string | null>(null);
  const [requestToken, setRequestToken] = useState('');
  const [busy, setBusy] = useState(false);
  const [notice, setNotice] = useState<{ ok: boolean; text: string } | null>(null);

  const fetchUrl = async () => {
    try {
      const loginUrl = await zerodhaLoginUrl();
      setUrl(loginUrl);
      await openExternalUrl(loginUrl);
    } catch (err) {
      setNotice({ ok: false, text: errorText(err) });
    }
  };

  const connect = async () => {
    setBusy(true);
    setNotice(null);
    try {
      await zerodhaLogin(requestToken);
      setRequestToken('');
      setNotice({ ok: true, text: 'Logged in to Zerodha. The token is valid until about 06:00 IST tomorrow.' });
      await onChanged();
    } catch (err) {
      setNotice({ ok: false, text: errorText(err) });
    } finally {
      setBusy(false);
    }
  };

  const disconnect = async () => {
    try {
      await clearBrokerSession('zerodha');
      await onChanged();
    } catch (err) {
      setNotice({ ok: false, text: errorText(err) });
    }
  };

  return (
    <div className="mt-3 border-t border-[var(--border-color)] pt-3" data-testid="zerodha-login">
      <h6 className="text-[11px] font-semibold uppercase tracking-wider text-text-secondary">Daily login</h6>
      {!ready ? (
        <p className="mt-1 text-[11px] text-text-muted">Save the API key and secret first, then log in here.</p>
      ) : (
        <>
          <ol className="mt-1 list-decimal pl-4 text-[11px] text-text-muted">
            <li>Open the Kite login page and sign in.</li>
            <li>After redirect, copy the full address from the browser bar (it contains <code>request_token=…</code>).</li>
            <li>Paste it below and connect.</li>
          </ol>
          <div className="mt-2 flex gap-2">
            <button type="button" onClick={() => void fetchUrl()} data-testid="zerodha-open-login"
              className="border border-[var(--border-color)] px-3 py-1 text-[11px] text-text-secondary hover:text-text-primary">Open Kite login</button>
            {url && <button type="button" onClick={() => void navigator.clipboard?.writeText(url)} className="border border-[var(--border-color)] px-3 py-1 text-[11px] text-text-muted hover:text-text-primary">Copy URL</button>}
            {connection?.authenticated && <button type="button" onClick={() => void disconnect()} data-testid="zerodha-disconnect" className="border border-[var(--border-color)] px-3 py-1 text-[11px] text-text-muted hover:text-bear">Disconnect</button>}
          </div>
          {url && <p className="mt-1 break-all font-mono text-[10px] text-text-muted" data-testid="zerodha-login-url">{url}</p>}
          <div className="mt-2 flex gap-2">
            <input type="text" value={requestToken} onChange={(e) => setRequestToken(e.target.value)} placeholder="Paste redirect URL or request token"
              aria-label="Zerodha request token" data-testid="zerodha-request-token" spellCheck={false}
              className="min-w-0 flex-1 border border-[var(--border-color)] bg-surface-1 px-2 py-1 font-mono text-xs focus:border-accent focus:outline-none" />
            <button type="button" onClick={() => void connect()} disabled={busy || !requestToken.trim()} data-testid="zerodha-connect"
              className="border border-accent bg-accent/10 px-3 py-1 text-[11px] font-semibold uppercase tracking-wider text-accent hover:bg-accent/20 disabled:opacity-40">
              Connect
            </button>
          </div>
        </>
      )}
      {notice && <p className={`mt-1 text-[11px] ${notice.ok ? 'text-bull' : 'text-bear'}`} role="status" data-testid="zerodha-login-notice">{notice.text}</p>}
    </div>
  );
};

export const AdapterManager: React.FC = () => {
  const [schemas, setSchemas] = useState<AdapterSchemaDto[]>([]);
  const [configs, setConfigs] = useState<AdapterConfigDto[]>([]);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [info, setInfo] = useState<string | null>(null);
  const connections = useBrokerStore((s) => s.connections);
  const setConnections = useBrokerStore((s) => s.setConnections);
  const vault = useVaultStore((s) => s.status);
  const setVaultStatus = useVaultStore((s) => s.setStatus);
  const unlocked = vault === 'unlocked';

  const reload = useCallback(async () => {
    try {
      const [loadedSchemas, loadedConfigs, loadedConnections, status] = await Promise.all([
        listAdapterSchemas(), getAdapterConfigs(), listBrokerConnections(), vaultStatus(),
      ]);
      setSchemas(loadedSchemas);
      setConfigs(loadedConfigs);
      if (Array.isArray(loadedConnections)) setConnections(loadedConnections);
      setVaultStatus(status.status);
      setLoadError(null);
    } catch (err) {
      setLoadError(errorText(err));
    }
  }, [setConnections, setVaultStatus]);

  useEffect(() => {
    void reload();
  }, [reload]);

  const configById = useMemo(() => new Map(configs.map((c) => [c.adapter_id, c])), [configs]);

  const lock = async () => {
    try {
      setVaultStatus((await vaultLock()).status);
      await reload();
    } catch (err) {
      setLoadError(errorText(err));
    }
  };

  const importEnv = async () => {
    try {
      const result = await importEnvCredentials();
      setInfo(result.imported.length > 0 ? `Imported ${result.imported.length} value(s) from ${result.source}.` : 'Nothing to import.');
      await reload();
    } catch (err) {
      setInfo(errorText(err));
    }
  };

  const grouped = (category: string[]) => schemas.filter((s) => category.includes(s.category));

  return (
    <div data-testid="adapter-manager">
      <div className="mb-2 flex items-center justify-between">
        <h4 className="text-xs font-medium text-text-secondary">Adapters &amp; API keys</h4>
        <div className="flex items-center gap-2 text-[11px]">
          <span className={`border px-1 uppercase tracking-wider ${unlocked ? 'border-bull text-bull' : 'border-warning text-warning'}`} data-testid="vault-state">
            Vault {vault ?? '…'}
          </span>
          {unlocked ? (
            <>
              <button type="button" onClick={() => void importEnv()} data-testid="vault-import-env"
                className="border border-[var(--border-color)] px-2 py-1 text-text-secondary hover:text-text-primary">Import .env</button>
              <button type="button" onClick={() => void lock()} data-testid="vault-lock"
                className="border border-[var(--border-color)] px-2 py-1 text-text-secondary hover:text-text-primary">Lock</button>
            </>
          ) : (
            <button type="button" onClick={() => useVaultStore.setState({ dismissed: false })} data-testid="vault-open-gate"
              className="border border-accent px-2 py-1 text-accent">Unlock…</button>
          )}
        </div>
      </div>
      <p className="mb-3 text-[11px] text-text-muted">
        Keys are encrypted with your vault passphrase and applied immediately, with no restart or environment variables needed.
      </p>
      {loadError && <p role="alert" className="mb-2 text-xs text-bear">{loadError}</p>}
      {info && <p className="mb-2 text-xs text-text-secondary" role="status">{info}</p>}
      {!unlocked && <p className="mb-2 text-xs text-warning">Unlock the vault to add or change credentials.</p>}

      {(['Brokers & data feeds', 'AI providers'] as const).map((title, index) => {
        const items = index === 0 ? grouped(['broker', 'data']) : grouped(['llm']);
        if (items.length === 0) return null;
        return (
          <div key={title} className="mb-4">
            <h5 className="mb-2 text-[10px] font-semibold uppercase tracking-wider text-text-muted">{title}</h5>
            <div className="space-y-2">
              {items.map((schema) => (
                <AdapterCard
                  key={schema.adapter_id}
                  schema={schema}
                  config={configById.get(schema.adapter_id) ?? { adapter_id: schema.adapter_id, enabled: true, settings: {}, secrets_configured: [] }}
                  connection={connections.find((c) => c.broker_id === schema.adapter_id)}
                  vaultUnlocked={unlocked}
                  onChanged={reload}
                />
              ))}
            </div>
          </div>
        );
      })}
    </div>
  );
};
