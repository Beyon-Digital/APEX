import { SymbolField } from '../common/SymbolField';
import React, { useCallback, useEffect, useState } from 'react';
import {
  createAutomation,
  deleteAutomation,
  listAutomations,
  listMLModels,
  setAutomationEnabled,
} from '../../lib/tauri';
import type { AutomationDto, MLModelDto } from '../../lib/types';

const POLL_MS = 5000;

function fmtInterval(secs: number): string {
  if (secs % 3600 === 0) return `${secs / 3600}h`;
  if (secs % 60 === 0) return `${secs / 60}m`;
  return `${secs}s`;
}

function fmtTime(iso: string | null): string {
  if (!iso) return 'never';
  try {
    return new Date(iso).toLocaleTimeString('en-US', {
      hour: '2-digit',
      minute: '2-digit',
      second: '2-digit',
    });
  } catch {
    return iso;
  }
}

export const AutomationsPanel: React.FC = () => {
  const [rules, setRules] = useState<AutomationDto[]>([]);
  const [models, setModels] = useState<MLModelDto[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  // form state
  const [name, setName] = useState('');
  const [symbol, setSymbol] = useState('');
  const [modelId, setModelId] = useState('');
  const [intervalSecs, setIntervalSecs] = useState('300');
  const [quantity, setQuantity] = useState('1');
  const [threshold, setThreshold] = useState('0.5');
  const [brokerId, setBrokerId] = useState('paper');

  const refresh = useCallback(async () => {
    try {
      const [r, m] = await Promise.all([listAutomations(), listMLModels()]);
      setRules(r);
      setModels(m.filter((x) => x.status === 'completed'));
      setError(null);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    }
  }, []);

  useEffect(() => {
    void refresh();
    const id = setInterval(() => void refresh(), POLL_MS);
    return () => clearInterval(id);
  }, [refresh]);

  useEffect(() => {
    if (!modelId && models.length > 0) setModelId(models[0].model_id);
  }, [models, modelId]);

  const submit = useCallback(
    async (e: React.FormEvent) => {
      e.preventDefault();
      if (busy) return;
      setBusy(true);
      setError(null);
      try {
        await createAutomation({
          name: name.trim() || `${modelId} on ${symbol}`,
          kind: 'model_signal',
          symbol: symbol.trim().toUpperCase(),
          model_id: modelId,
          interval_secs: Math.max(30, parseInt(intervalSecs, 10) || 300),
          quantity: parseFloat(quantity) || 1,
          threshold: Math.min(1, Math.max(0, parseFloat(threshold) || 0.5)),
          broker_id: brokerId.trim() || 'paper',
        });
        setName('');
        setSymbol('');
        await refresh();
      } catch (err) {
        setError(err instanceof Error ? err.message : String(err));
      } finally {
        setBusy(false);
      }
    },
    [busy, name, symbol, modelId, intervalSecs, quantity, threshold, brokerId, refresh],
  );

  const toggle = useCallback(
    async (id: string, enabled: boolean) => {
      try {
        await setAutomationEnabled(id, !enabled);
        await refresh();
      } catch (e) {
        setError(e instanceof Error ? e.message : String(e));
      }
    },
    [refresh],
  );

  const remove = useCallback(
    async (id: string) => {
      try {
        await deleteAutomation(id);
        await refresh();
      } catch (e) {
        setError(e instanceof Error ? e.message : String(e));
      }
    },
    [refresh],
  );

  const inputCls =
    'px-2 py-1 text-xs bg-surface-0 border border-[var(--border-color)] rounded text-text-primary focus:border-accent focus:outline-none';

  return (
    <div className="h-full flex flex-col overflow-auto" data-testid="automations-panel">
      <div className="px-3 py-2 border-b border-[var(--border-color)]">
        <div className="text-xs font-semibold text-text-primary uppercase tracking-wider">
          Automations
        </div>
        <div className="text-[11px] text-text-muted mt-0.5">
          Scheduled rules run a model's signal and route orders through the risk engine.
          Live brokers require <span className="font-mono">[automations] allow_live_trading</span> in config.
        </div>
      </div>

      {error && (
        <div className="mx-3 mt-2 px-3 py-2 text-xs text-bear bg-bear/10 border border-bear/30 rounded" data-testid="automations-error">
          {error}
        </div>
      )}

      <form onSubmit={(e) => void submit(e)} className="px-3 py-2 border-b border-[var(--border-color)] grid grid-cols-2 gap-2" data-testid="automation-form">
        <input className={inputCls} placeholder="Name (optional)" value={name} onChange={(e) => setName(e.target.value)} data-testid="automation-name" />
        <SymbolField className={inputCls} placeholder="Symbol *" value={symbol} onChange={setSymbol} required testId="automation-symbol" />
        <select className={inputCls} value={modelId} onChange={(e) => setModelId(e.target.value)} required data-testid="automation-model">
          {models.length === 0 && <option value="">— train a model first (ML Workbench) —</option>}
          {models.map((m) => (
            <option key={m.model_id} value={m.model_id}>
              {m.model_id} ({m.algorithm})
            </option>
          ))}
        </select>
        <input className={inputCls} placeholder="Interval secs" value={intervalSecs} onChange={(e) => setIntervalSecs(e.target.value)} title="Interval between runs (min 30s)" data-testid="automation-interval" />
        <input className={inputCls} placeholder="Qty" value={quantity} onChange={(e) => setQuantity(e.target.value)} title="Order quantity" data-testid="automation-qty" />
        <input className={inputCls} placeholder="Prob threshold 0–1" value={threshold} onChange={(e) => setThreshold(e.target.value)} title="Minimum model confidence to place the order" data-testid="automation-threshold" />
        <input className={inputCls} placeholder="Broker (default paper)" value={brokerId} onChange={(e) => setBrokerId(e.target.value)} data-testid="automation-broker" />
        <button type="submit" disabled={busy || !symbol.trim() || !modelId} className="px-2 py-1 text-xs font-mono rounded bg-accent/20 text-accent hover:bg-accent/30 disabled:opacity-50" data-testid="automation-create">
          {busy ? 'Creating…' : 'Create rule'}
        </button>
      </form>

      <div className="flex-1 px-3 py-2 space-y-1.5">
        {rules.length === 0 ? (
          <div className="text-xs text-text-muted py-6 text-center" data-testid="automations-empty">
            No automations yet — create one above, or ask the copilot to set one up.
          </div>
        ) : (
          rules.map((r) => (
            <div key={r.id} className="rounded border border-[var(--border-color)] bg-surface-1 px-3 py-2" data-testid="automation-row">
              <div className="flex items-center justify-between gap-2">
                <div className="min-w-0">
                  <div className="flex items-center gap-2">
                    <span className={`inline-block h-1.5 w-1.5 rounded-full ${r.enabled ? 'bg-bull' : 'bg-text-muted'}`} />
                    <span className="text-xs font-medium text-text-primary truncate">{r.name}</span>
                    <span className="text-[10px] font-mono text-text-muted uppercase">{r.kind}</span>
                  </div>
                  <div className="mt-0.5 text-[11px] font-mono text-text-secondary">
                    {r.model_id} on {r.symbol} · every {fmtInterval(r.interval_secs)} · qty {r.quantity} @ {r.broker_id} · p≥{r.threshold}
                  </div>
                  <div className="mt-0.5 text-[10px] text-text-muted font-mono truncate">
                    last run: {fmtTime(r.last_run_at)}
                    {r.last_result ? ` — ${r.last_result}` : ''}
                  </div>
                </div>
                <div className="flex items-center gap-1 shrink-0">
                  <button
                    onClick={() => void toggle(r.id, r.enabled)}
                    className={`px-2 py-0.5 text-[10px] font-mono rounded border ${
                      r.enabled
                        ? 'border-warning/40 text-warning hover:bg-warning/10'
                        : 'border-bull/40 text-bull hover:bg-bull/10'
                    }`}
                    data-testid="automation-toggle"
                  >
                    {r.enabled ? 'Pause' : 'Resume'}
                  </button>
                  <button
                    onClick={() => void remove(r.id)}
                    className="px-2 py-0.5 text-[10px] font-mono rounded border border-bear/40 text-bear hover:bg-bear/10"
                    data-testid="automation-delete"
                  >
                    Delete
                  </button>
                </div>
              </div>
            </div>
          ))
        )}
      </div>
    </div>
  );
};
