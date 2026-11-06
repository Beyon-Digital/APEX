import React, { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { SymbolField } from '../common/SymbolField';
import {
  createAutomation, listActivity, listAutomations, listBrokerConnections, listRecipes, modelSignalHistory, predictModelSignal, recipeBuildingBlocks,
  saveRecipe,
} from '../../lib/tauri';
import { useMLStore } from '../../stores/mlStore';
import { openItem } from '../../stores/navStore';
import { useArtifactStore } from '../../stores/artifactStore';
import type {
  ActivityDto, AutomationDto, BrokerConnectionDto, ModelDetailDto, ModelSignalDto, ModelSignalHistoryDto, RecipeDto,
} from '../../lib/types';
import {
  ChartLegend, FoldAccuracyChart, FoldReturnChart, HorizontalBars, SignalChart,
} from './ModelCharts';
import {
  HOLDOUT_GAP_PTS, SOURCE_LABEL, TRADE_MODE_HINT, baselineAccuracy, bytesLabel, classMeaning, displayName, exactTime, fmtValue,
  holdoutVerdict, importanceReason, isLegacyModel, isRecipeModel, isValidRecipeName, labelText, modelAlgorithmLabel, modelVerdict, num,
  paramRows, pct, recipeStatus, relativeTime, retrainDetailFor, shortingContext, signedPct, suggestRecipeName, tradeModeLabel,
} from './modelUtils';
import type { RecipeStatus, ShortingContext } from './modelUtils';
import { RecipeSummary } from './RecipeSummary';
import { VersionHistory } from '../audit/VersionHistory';

export interface ModelPatch {
  displayName?: string;
  notes?: string;
  tags?: string[];
}

export interface ModelDetailProps {
  detail: ModelDetailDto;
  /** Persist an edit. Rejects on failure; the caller has already rolled the optimistic change back. */
  onSave: (patch: ModelPatch) => Promise<void>;
  onRetrain: (detail: ModelDetailDto) => void;
  onDelete: (detail: ModelDetailDto) => void;
  onClose: () => void;
  /** Ask the registry to refetch (after creating an automation). */
  onChanged: () => void;
}

export const SourceBadge: React.FC<{ source: ModelDetailDto['source']; testId?: string }> = ({ source, testId = 'model-source' }) => {
  const tone = source === 'copilot' ? 'border-accent/50 text-accent bg-accent/10' : source === 'user' ? 'border-bull/40 text-bull bg-bull/10' : 'border-[var(--border-color)] text-text-muted bg-surface-2';
  return <span className={`inline-block border px-1.5 py-px font-mono text-[10px] uppercase ${tone}`} data-testid={testId} data-source={source}>{SOURCE_LABEL[source]}</span>;
};

const Section: React.FC<{
  id: string; title: string; defaultOpen?: boolean; right?: React.ReactNode; children: React.ReactNode;
}> = ({ id, title, defaultOpen = true, right, children }) => {
  const [open, setOpen] = useState(defaultOpen);
  return (
    <section className="border border-[var(--border-color)] bg-surface-1" id={`model-sec-${id}`} data-testid={`model-section-${id}`}>
      <div className="flex items-center justify-between px-3 py-1.5">
        <button
          type="button" onClick={() => setOpen((o) => !o)} aria-expanded={open}
          className="flex items-center gap-2 text-[11px] font-mono uppercase tracking-wide text-text-secondary hover:text-text-primary"
          data-testid={`model-section-toggle-${id}`}
        >
          <span className="w-3">{open ? '▾' : '▸'}</span>{title}
        </button>
        {right}
      </div>
      {open && <div className="border-t border-[var(--border-color)] px-3 py-2">{children}</div>}
    </section>
  );
};

const inputCls = 'w-full border border-[var(--border-color)] bg-surface-0 px-2 py-1 text-xs text-text-primary focus:border-accent focus:outline-none';
const btnCls = 'border border-[var(--border-color)] bg-surface-2 px-2 py-1 text-[11px] font-mono text-text-secondary hover:text-text-primary disabled:opacity-50';
const accentBtn = 'border border-accent/50 bg-accent/15 px-2 py-1 text-[11px] font-mono text-accent hover:bg-accent/25 disabled:opacity-50';

const Card: React.FC<{ name: string; label: string; value: string; sub?: string; tone?: 'good' | 'bad' | 'neutral'; extra?: React.ReactNode }> = ({ name, label, value, sub, tone = 'neutral', extra }) => (
  <div className="border border-[var(--border-color)] bg-surface-0 px-2 py-1.5" data-testid={`model-metric-${name}`}>
    <div className="text-[10px] uppercase text-text-muted">{label}</div>
    <div className="flex items-baseline gap-2">
      <div className={`font-mono text-sm ${tone === 'good' ? 'text-bull' : tone === 'bad' ? 'text-bear' : 'text-text-primary'}`} data-testid={`model-metric-${name}-value`}>{value}</div>
      {extra}
    </div>
    {sub && <div className="text-[10px] text-text-muted">{sub}</div>}
  </div>
);

/** Automations using a model and the brokers they trade on, to tell whether a long/short result is reachable. */
function useShortingContext(detail: ModelDetailDto): { context: ShortingContext; automations: AutomationDto[] } {
  const version = useArtifactStore((s) => s.versions.automation);
  const [automations, setAutomations] = useState<AutomationDto[]>([]);
  const [brokers, setBrokers] = useState<BrokerConnectionDto[]>([]);
  useEffect(() => {
    if (detail.trade_mode !== 'long_short') return undefined;
    let live = true;
    void listAutomations().then((a) => { if (live) setAutomations(a); }).catch(() => { /* note stays off */ });
    void listBrokerConnections().then((b) => { if (live) setBrokers(b); }).catch(() => { /* note stays off */ });
    return () => { live = false; };
  }, [detail.model_id, detail.trade_mode, detail.used_by.length, version]);
  const context = useMemo(() => shortingContext(detail, automations, brokers), [detail, automations, brokers]);
  return { context, automations };
}

const CopyButton: React.FC<{ text: string; testId: string }> = ({ text, testId }) => {
  const [copied, setCopied] = useState(false);
  const copy = async () => {
    try { await navigator.clipboard.writeText(text); setCopied(true); setTimeout(() => setCopied(false), 1500); } catch { setCopied(false); }
  };
  return (
    <button type="button" className="shrink-0 border border-[var(--border-color)] bg-surface-2 px-1.5 py-px font-mono text-[10px] text-text-secondary hover:text-text-primary" onClick={() => void copy()} data-testid={testId} aria-label="Copy formula">
      {copied ? 'Copied' : 'Copy'}
    </button>
  );
};

const HoldoutCard: React.FC<{ detail: ModelDetailDto }> = ({ detail }) => {
  const v = holdoutVerdict(detail);
  const h = detail.holdout;
  const toneCls = { good: 'border-bull/40 bg-bull/10 text-bull', warn: 'border-warning/40 bg-warning/10 text-warning', bad: 'border-bear/40 bg-bear/10 text-bear', info: 'border-[var(--border-color)] bg-surface-2 text-text-secondary' }[v.tone];
  return (
    <div className="mt-2 border border-[var(--border-color)] bg-surface-0 px-2 py-1.5" data-testid="model-holdout" data-verdict={v.id}>
      <div className="text-[10px] uppercase text-text-muted">Hold-out (final period never seen in cross-validation)</div>
      {h && (
        <div className="mt-1 grid grid-cols-2 gap-1.5 sm:grid-cols-4">
          <div><div className="text-[10px] text-text-muted">Rows</div><div className="font-mono text-sm" data-testid="holdout-rows">{h.rows}</div></div>
          <div>
            <div className="text-[10px] text-text-muted">Accuracy</div>
            <div className="font-mono text-sm" data-testid="holdout-accuracy">{pct(h.accuracy)}</div>
            <div className="text-[10px] text-text-muted" data-testid="holdout-baseline">{h.baseline_accuracy === null ? 'no baseline' : `baseline ${pct(h.baseline_accuracy)}`}</div>
          </div>
          <div>
            <div className="text-[10px] text-text-muted">Trading result</div>
            <div className={`font-mono text-sm ${h.trading ? (h.trading.strategy_return_pct >= 0 ? 'text-bull' : 'text-bear') : ''}`} data-testid="holdout-trading">{h.trading ? signedPct(h.trading.strategy_return_pct) : 'n/a'}</div>
            <div className="text-[10px] text-text-muted">{h.trading ? `buy and hold ${signedPct(h.trading.buy_hold_return_pct)} · ${h.trading.trades} trades` : 'not recorded'}</div>
          </div>
          <div>
            <div className="text-[10px] text-text-muted">Cross-validation</div>
            <div className="font-mono text-sm" data-testid="holdout-cv">{pct(detail.metrics.accuracy)}</div>
          </div>
        </div>
      )}
      <div className={`mt-1.5 border px-2 py-1 text-[11px] ${toneCls}`} data-testid="model-holdout-verdict" data-tone={v.tone} title={`Flagged as likely overfit when hold-out accuracy is ${HOLDOUT_GAP_PTS} or more points below cross-validation accuracy.`}>{v.text}</div>
    </div>
  );
};

const NameEditor: React.FC<{ detail: ModelDetailDto; onSave: ModelDetailProps['onSave'] }> = ({ detail, onSave }) => {
  const [editing, setEditing] = useState(false);
  const [draft, setDraft] = useState('');
  const [error, setError] = useState<string | null>(null);
  const done = useRef(true);
  const shown = displayName(detail);

  const commit = async () => {
    if (done.current) return;
    done.current = true;
    const next = draft.trim();
    setEditing(false);
    if (next === detail.display_name.trim() || (next === '' && !detail.display_name)) return;
    setError(null);
    try { await onSave({ displayName: next }); } catch (e) { setError(e instanceof Error ? e.message : String(e)); }
  };

  return (
    <div className="min-w-[10rem] flex-1">
      {editing ? (
        <input
          autoFocus className={`${inputCls} font-mono text-sm`} value={draft} maxLength={80} aria-label="Model name"
          onChange={(e) => setDraft(e.target.value)} onBlur={() => void commit()} data-testid="model-name-input"
          onKeyDown={(e) => {
            if (e.key === 'Enter') void commit();
            if (e.key === 'Escape') { done.current = true; setEditing(false); }
          }}
        />
      ) : (
        <button
          type="button" title="Click to rename" className="max-w-full truncate text-left font-mono text-sm font-semibold text-text-primary hover:text-accent"
          onClick={() => { done.current = false; setDraft(detail.display_name || ''); setEditing(true); }} data-testid="model-name"
        >
          {shown}
        </button>
      )}
      {detail.display_name && <div className="truncate font-mono text-[10px] text-text-muted" data-testid="model-id">{detail.model_id}</div>}
      {error && <div className="text-[11px] text-bear" data-testid="model-save-error">Could not save: {error}</div>}
    </div>
  );
};

const TagEditor: React.FC<{ detail: ModelDetailDto; onSave: ModelDetailProps['onSave'] }> = ({ detail, onSave }) => {
  const [draft, setDraft] = useState('');
  const [error, setError] = useState<string | null>(null);

  const save = async (tags: string[]) => {
    setError(null);
    try { await onSave({ tags }); } catch (e) { setError(e instanceof Error ? e.message : String(e)); }
  };
  const add = () => {
    const tag = draft.trim().slice(0, 24);
    setDraft('');
    if (!tag || detail.tags.some((t) => t.toLowerCase() === tag.toLowerCase()) || detail.tags.length >= 12) return;
    void save([...detail.tags, tag]);
  };

  return (
    <div className="flex flex-wrap items-center gap-1" data-testid="model-tags">
      {detail.tags.map((tag) => (
        <span key={tag} className="flex items-center gap-1 border border-[var(--border-color)] bg-surface-2 px-1.5 py-px font-mono text-[10px] text-text-secondary" data-testid={`model-tag-${tag}`}>
          {tag}
          <button type="button" aria-label={`Remove tag ${tag}`} className="text-text-muted hover:text-bear" onClick={() => void save(detail.tags.filter((t) => t !== tag))} data-testid={`model-tag-remove-${tag}`}>×</button>
        </span>
      ))}
      <input
        className="w-24 border border-dashed border-[var(--border-color)] bg-transparent px-1.5 py-px font-mono text-[10px] text-text-primary placeholder:text-text-muted focus:border-accent focus:outline-none"
        placeholder="+ add tag" value={draft} maxLength={24} aria-label="Add tag" data-testid="model-tag-input"
        onChange={(e) => setDraft(e.target.value)}
        onKeyDown={(e) => { if (e.key === 'Enter') { e.preventDefault(); add(); } }}
      />
      {error && <span className="text-[11px] text-bear" data-testid="model-tags-error">Could not save: {error}</span>}
    </div>
  );
};

const NotesEditor: React.FC<{ detail: ModelDetailDto; onSave: ModelDetailProps['onSave'] }> = ({ detail, onSave }) => {
  const [draft, setDraft] = useState(detail.notes);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [changedElsewhere, setChangedElsewhere] = useState(false);
  const dirty = draft !== detail.notes;

  // The notes can change under the editor (the Copilot edits them, or another refresh lands).
  // A clean draft follows; a draft with unsaved typing is kept and the user is told.
  const lastNotes = useRef(detail.notes);
  useEffect(() => {
    const previous = lastNotes.current;
    lastNotes.current = detail.notes;
    if (previous === detail.notes) return;
    if (draft === previous) setDraft(detail.notes);
    else if (draft !== detail.notes) setChangedElsewhere(true);
    // Only a change of the saved notes should run this, not every keystroke.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [detail.notes]);

  const save = async () => {
    setSaving(true);
    setError(null);
    try { await onSave({ notes: draft }); setChangedElsewhere(false); } catch (e) { setError(e instanceof Error ? e.message : String(e)); } finally { setSaving(false); }
  };

  return (
    <div>
      <textarea
        className={`${inputCls} h-16 resize-y`} placeholder="Notes: what you tried, what to watch for…" value={draft} maxLength={4000}
        onChange={(e) => setDraft(e.target.value)} aria-label="Model notes" data-testid="model-notes"
      />
      <div className="mt-1 flex items-center gap-2">
        <button type="button" className={accentBtn} disabled={!dirty || saving} onClick={() => void save()} data-testid="model-notes-save">{saving ? 'Saving…' : 'Save notes'}</button>
        {dirty && <span className="text-[10px] text-warning" data-testid="model-notes-unsaved">Unsaved changes</span>}
        {dirty && changedElsewhere && (
          <span className="text-[10px] text-warning" data-testid="model-notes-changed">
            These notes were changed elsewhere.{' '}
            <button type="button" className="underline" onClick={() => { setDraft(detail.notes); setChangedElsewhere(false); }} data-testid="model-notes-use-latest">Use the latest</button>
          </span>
        )}
        {error && <span className="text-[11px] text-bear" data-testid="model-notes-error">Could not save: {error}</span>}
      </div>
    </div>
  );
};

const TryIt: React.FC<{ detail: ModelDetailDto }> = ({ detail }) => {
  const [symbol, setSymbol] = useState(detail.symbols[0] ?? '');
  const [busy, setBusy] = useState<'score' | 'history' | null>(null);
  const [score, setScore] = useState<ModelSignalDto | null>(null);
  const [history, setHistory] = useState<ModelSignalHistoryDto | null>(null);
  const [error, setError] = useState<string | null>(null);

  const run = async (kind: 'score' | 'history') => {
    const sym = symbol.trim();
    if (!sym) { setError('Enter a symbol first.'); return; }
    setBusy(kind);
    setError(null);
    try {
      if (kind === 'score') setScore(await predictModelSignal(detail.model_id, sym));
      else setHistory(await modelSignalHistory(detail.model_id, sym, 120));
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(null);
    }
  };

  const label = (s: number) => (s > 0 ? 'LONG' : s < 0 ? 'SHORT' : 'FLAT');
  return (
    <div className="space-y-2">
      <div className="flex flex-wrap items-center gap-2">
        <div className="w-40">
          <SymbolField value={symbol} onChange={setSymbol} className={`${inputCls} font-mono`} placeholder="Symbol" testId="try-symbol" />
        </div>
        <button type="button" className={accentBtn} disabled={busy !== null} onClick={() => void run('score')} data-testid="try-score">{busy === 'score' ? 'Scoring…' : 'Score now'}</button>
        <button type="button" className={btnCls} disabled={busy !== null} onClick={() => void run('history')} data-testid="try-history">{busy === 'history' ? 'Loading…' : 'Signal history'}</button>
      </div>
      {error && <div className="text-[11px] text-bear" data-testid="try-error">{error}</div>}
      {score && (
        <div className="border border-[var(--border-color)] bg-surface-0 px-2 py-1.5 text-xs" data-testid="try-result">
          <span className="text-text-muted">{score.symbol}: </span>
          <span className={`font-mono font-semibold ${score.signal > 0 ? 'text-bull' : score.signal < 0 ? 'text-bear' : 'text-text-secondary'}`} data-testid="try-result-signal">{label(score.signal)}</span>
          <span className="ml-3 text-text-muted">probability </span>
          <span className="font-mono" data-testid="try-result-probability">{score.probability === null ? 'n/a' : pct(score.probability, 1)}</span>
        </div>
      )}
      {history && (
        <div data-testid="try-history-result">
          <ChartLegend items={[{ label: 'Long', color: 'var(--color-bull)' }, { label: 'Short', color: 'var(--color-bear)' }, { label: 'Flat', color: 'var(--color-muted)' }]} />
          <SignalChart points={history.points} />
        </div>
      )}
    </div>
  );
};

const AutomationForm: React.FC<{ detail: ModelDetailDto; onCreated: () => void }> = ({ detail, onCreated }) => {
  const [name, setName] = useState(`${displayName(detail)} signal`.slice(0, 60));
  const [symbol, setSymbol] = useState(detail.symbols[0] ?? '');
  const [quantity, setQuantity] = useState('1');
  const [threshold, setThreshold] = useState('0.6');
  const [interval, setIntervalSecs] = useState('300');
  const [broker, setBroker] = useState('paper');
  const [brokers, setBrokers] = useState<BrokerConnectionDto[]>([]);
  // null = follow the default (on only for a model evaluated long/short); a click makes it the user's choice.
  const [shortChoice, setShortChoice] = useState<boolean | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [created, setCreated] = useState<string | null>(null);

  useEffect(() => {
    let live = true;
    void listBrokerConnections().then((b) => { if (live) setBrokers(b); }).catch(() => { /* paper only */ });
    return () => { live = false; };
  }, []);

  const brokerInfo = brokers.find((b) => b.broker_id === broker);
  const canShort = broker === 'paper' ? (brokerInfo?.can_short ?? true) : Boolean(brokerInfo?.can_short);
  const allowShort = canShort && (shortChoice ?? detail.trade_mode === 'long_short');
  const shortBlockedReason = broker === 'paper'
    ? 'The paper broker could not be read.'
    : `${broker} cannot short. Live shorting needs allow_live_shorting in config and an adapter that supports it.`;

  const submit = async (e: React.FormEvent) => {
    e.preventDefault();
    const qty = Number(quantity);
    const thr = Number(threshold);
    const secs = Number(interval);
    if (!symbol.trim()) return setError('Choose a symbol.');
    if (!(qty > 0)) return setError('Quantity must be greater than zero.');
    if (!(thr >= 0 && thr <= 1)) return setError('Threshold must be between 0 and 1.');
    if (!(secs >= 30)) return setError('Interval must be at least 30 seconds.');
    setBusy(true);
    setError(null);
    setCreated(null);
    try {
      const rule = await createAutomation({
        name: name.trim() || `${displayName(detail)} signal`, kind: 'model_signal', symbol: symbol.trim(), model_id: detail.model_id,
        interval_secs: secs, quantity: qty, threshold: thr, broker_id: broker, allow_short: allowShort,
      });
      setCreated(rule.name);
      onCreated();
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setBusy(false);
    }
  };

  const brokerIds = [...new Set(['paper', ...brokers.map((b) => b.broker_id)])];
  return (
    <form onSubmit={(e) => void submit(e)} className="grid grid-cols-2 gap-2" data-testid="auto-form">
      <label className="col-span-2 text-[10px] uppercase text-text-muted">Name
        <input className={`${inputCls} mt-0.5`} value={name} onChange={(e) => setName(e.target.value)} data-testid="auto-name" />
      </label>
      <label className="text-[10px] uppercase text-text-muted">Symbol
        <div className="mt-0.5"><SymbolField value={symbol} onChange={setSymbol} className={`${inputCls} font-mono`} placeholder="Symbol" testId="auto-symbol" /></div>
      </label>
      <label className="text-[10px] uppercase text-text-muted">Broker
        <select className={`${inputCls} mt-0.5`} value={broker} onChange={(e) => setBroker(e.target.value)} data-testid="auto-broker">
          {brokerIds.map((b) => <option key={b} value={b}>{b === 'paper' ? 'paper (simulated)' : b}</option>)}
        </select>
      </label>
      <label className="text-[10px] uppercase text-text-muted">Quantity
        <input className={`${inputCls} mt-0.5 font-mono`} value={quantity} onChange={(e) => setQuantity(e.target.value)} data-testid="auto-qty" />
      </label>
      <label className="text-[10px] uppercase text-text-muted">Min probability (0-1)
        <input className={`${inputCls} mt-0.5 font-mono`} value={threshold} onChange={(e) => setThreshold(e.target.value)} data-testid="auto-threshold" />
      </label>
      <label className="text-[10px] uppercase text-text-muted">Run every (seconds)
        <input className={`${inputCls} mt-0.5 font-mono`} value={interval} onChange={(e) => setIntervalSecs(e.target.value)} data-testid="auto-interval" />
      </label>
      <div className="col-span-2" data-testid="auto-short-block">
        <label className={`flex items-center gap-2 text-[11px] ${canShort ? 'text-text-secondary' : 'text-text-muted'}`}>
          <input
            type="checkbox" checked={allowShort} disabled={!canShort} data-testid="auto-allow-short"
            onChange={(e) => setShortChoice(e.target.checked)}
          />
          Allow shorting
        </label>
        {!canShort && <p className="mt-0.5 text-[11px] text-warning" data-testid="auto-short-reason">{shortBlockedReason}</p>}
        <p className="mt-0.5 text-[10px] text-text-muted" data-testid="auto-short-help">
          {classMeaning(detail.label)} This model was evaluated {tradeModeLabel(detail.trade_mode).toLowerCase()}.
        </p>
        {allowShort && broker !== 'paper' && <p className="text-[11px] text-warning" data-testid="auto-short-live-warning">Live shorting needs allow_live_shorting in config and live trading armed.</p>}
      </div>
      <div className="flex items-end">
        <button type="submit" className={accentBtn} disabled={busy} data-testid="auto-create">{busy ? 'Creating…' : 'Create automation'}</button>
      </div>
      <p className="col-span-2 text-[10px] text-text-muted" data-testid="auto-note">
        Orders go to the paper broker unless you pick another one. Live brokers also need the live switch armed and
        <span className="font-mono"> [automations] allow_live_trading</span> enabled before any order is sent.
      </p>
      {broker !== 'paper' && <p className="col-span-2 text-[11px] text-warning" data-testid="auto-live-warning">{broker} may place real orders once live trading is armed.</p>}
      {error && <p className="col-span-2 text-[11px] text-bear" data-testid="auto-error">{error}</p>}
      {created && <p className="col-span-2 text-[11px] text-bull" data-testid="auto-created">Created automation “{created}”. It appears under Used by.</p>}
    </form>
  );
};

const ModelActivity: React.FC<{ modelId: string }> = ({ modelId }) => {
  const version = useArtifactStore((s) => s.activityVersion);
  const [rows, setRows] = useState<ActivityDto[]>([]);
  const [error, setError] = useState<string | null>(null);
  useEffect(() => {
    let live = true;
    listActivity({ targetId: modelId, limit: 50 })
      .then((r) => { if (live) { setRows(r); setError(null); } })
      .catch((e) => { if (live) setError(e instanceof Error ? e.message : String(e)); });
    return () => { live = false; };
  }, [modelId, version]);
  return (
    <div data-testid="model-activity">
      {error && <p className="text-[11px] text-bear" data-testid="model-activity-error">{error}</p>}
      {!error && rows.length === 0 && <p className="text-[11px] text-text-muted" data-testid="model-activity-empty">No recorded activity for this model.</p>}
      <ul className="space-y-1">
        {rows.map((a) => (
          <li key={a.id} className={`flex gap-2 text-[11px] ${a.ok ? '' : 'text-bear'}`} data-testid="model-activity-row">
            <span className="w-20 shrink-0 font-mono text-text-muted" title={exactTime(a.ts)}>{relativeTime(a.ts)}</span>
            <span className="w-16 shrink-0 font-mono text-text-secondary">{a.actor}</span>
            <span className="min-w-0 flex-1 break-words">{a.summary}{a.ok ? '' : ' (failed)'}</span>
          </li>
        ))}
      </ul>
      <button type="button" className={`${btnCls} mt-2`} onClick={() => openItem('activity', modelId)} data-testid="model-activity-open">Open in Activity panel</button>
    </div>
  );
};

/** Where the saved recipe of this name stands compared with the copy the model carries. */
function useSavedRecipeStatus(copy: RecipeDto | null): { status: RecipeStatus | 'unknown' } {
  const version = useArtifactStore((s) => s.activityVersion);
  const [saved, setSaved] = useState<RecipeDto[] | null>(null);
  // The trained copy has every step's default parameters written out; the building blocks supply them for the saved side.
  const blocks = useMLStore((s) => s.blocks);
  const blocksStatus = useMLStore((s) => s.blocksStatus);
  const loadBlocks = useMLStore((s) => s.loadBlocks);
  useEffect(() => { if (copy) void loadBlocks(); }, [copy?.name, loadBlocks]); // eslint-disable-line react-hooks/exhaustive-deps
  useEffect(() => {
    if (!copy) return undefined;
    let live = true;
    void listRecipes().then((r) => { if (live) setSaved(r); }).catch(() => { if (live) setSaved(null); });
    return () => { live = false; };
  }, [copy?.name, version]); // eslint-disable-line react-hooks/exhaustive-deps
  const ready = Boolean(blocks) || blocksStatus === 'error';
  return useMemo(() => (copy && saved && ready ? recipeStatus(copy, saved, blocks) : { status: 'unknown' as const }), [copy, saved, ready, blocks]);
}

const STATUS_TEXT: Record<RecipeStatus | 'unknown', string> = {
  same: 'The saved recipe with this name is unchanged since this model was trained.',
  edited: 'The saved recipe with this name has been edited since this model was trained.',
  missing: 'The saved recipe with this name has been deleted.',
  unknown: 'Checking the saved recipe...',
};

interface RetrainPlan { status: 'edited' | 'missing'; current: RecipeDto | null; taken: string[] }

/** Shown when the recipe a model was built from is no longer saved as it was: offer to save the model's copy first. */
const RecipeRetrainPrompt: React.FC<{
  detail: ModelDetailDto; copy: RecipeDto; plan: RetrainPlan; onCancel: () => void; onRetrain: (d: ModelDetailDto) => void;
}> = ({ detail, copy, plan, onCancel, onRetrain }) => {
  const [name, setName] = useState(() => suggestRecipeName(copy.name, plan.taken));
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const trimmed = name.trim();
  const problem = !isValidRecipeName(trimmed)
    ? 'Use lowercase letters, digits and underscores, starting with a letter (up to 41 characters).'
    : plan.taken.includes(trimmed) ? `A recipe named ${trimmed} already exists.` : null;

  const saveAndRetrain = async () => {
    if (problem) return;
    setBusy(true);
    setError(null);
    try {
      const { created_at: _c, updated_at: _u, ...rest } = copy;
      const saved = await saveRecipe({ ...rest, name: trimmed, label: `${copy.label || copy.name} (copy)` });
      onRetrain(retrainDetailFor(detail, saved));
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
      setBusy(false);
    }
  };

  return (
    <div className="border border-warning/40 bg-warning/10 px-3 py-2 text-[11px]" data-testid="recipe-retrain-prompt" data-status={plan.status}>
      <p className="text-warning" data-testid="recipe-retrain-message">
        {plan.status === 'edited'
          ? `The recipe “${copy.name}” was edited after this model was trained. The model keeps its own copy of the original.`
          : `The recipe “${copy.name}” this model was built from has been deleted. The model keeps its own copy.`}
      </p>
      <p className="mt-1 text-text-secondary">Save the model's copy as a recipe to train with the same algorithm again.</p>
      <div className="mt-1.5 flex flex-wrap items-center gap-2">
        <input
          className={`${inputCls} w-56 font-mono`} value={name} maxLength={41} aria-label="Name for the saved copy"
          onChange={(e) => setName(e.target.value)} data-testid="recipe-retrain-name"
        />
        <button type="button" className={accentBtn} disabled={busy || problem !== null} onClick={() => void saveAndRetrain()} data-testid="recipe-retrain-save">
          {busy ? 'Saving…' : 'Save copy and retrain'}
        </button>
        {plan.status === 'edited' && plan.current && (
          <button type="button" className={btnCls} disabled={busy} onClick={() => plan.current && onRetrain(retrainDetailFor(detail, plan.current))} data-testid="recipe-retrain-use-saved">
            Retrain with the edited recipe
          </button>
        )}
        <button type="button" className={btnCls} disabled={busy} onClick={onCancel} data-testid="recipe-retrain-cancel">Cancel</button>
      </div>
      {problem && <p className="mt-1 text-bear" data-testid="recipe-retrain-problem">{problem}</p>}
      {error && <p className="mt-1 text-bear" data-testid="recipe-retrain-error">Could not save: {error}</p>}
    </div>
  );
};

export const ModelDetail: React.FC<ModelDetailProps> = ({ detail, onSave, onRetrain, onDelete, onClose, onChanged }) => {
  const rootRef = useRef<HTMLDivElement>(null);
  const [logOpen, setLogOpen] = useState(false);
  const { context: shorting } = useShortingContext(detail);
  const verdict = useMemo(() => modelVerdict(detail, shorting), [detail, shorting]);
  const legacy = isLegacyModel(detail);
  const baseline = baselineAccuracy(detail.cv_folds);
  const t = detail.trading;
  const m = detail.metrics;
  const recipeModel = isRecipeModel(detail);
  const recipeCopy = recipeModel ? detail.recipe : null;
  const savedStatus = useSavedRecipeStatus(recipeCopy);
  const [retrainPlan, setRetrainPlan] = useState<RetrainPlan | null>(null);
  const [retrainError, setRetrainError] = useState<string | null>(null);
  useEffect(() => { setRetrainPlan(null); setRetrainError(null); }, [detail.model_id]);
  const algorithmName = modelAlgorithmLabel(detail);
  const noImportanceText = recipeModel
    ? (importanceReason(recipeCopy) ?? 'Feature importances could not be worked out for this recipe.')
    : 'This algorithm does not expose feature importances.';
  const selectionNote = recipeCopy?.steps.some((s) => s.kind === 'select_k_best' || s.kind === 'variance_threshold')
    ? 'Only the features kept by the selection steps are listed.' : null;

  const startRetrain = async () => {
    if (!recipeCopy) { onRetrain(detail); return; }
    setRetrainError(null);
    try {
      const saved = await listRecipes();
      const blocks = useMLStore.getState().blocks ?? await recipeBuildingBlocks().catch(() => null);
      const relation = recipeStatus(recipeCopy, saved, blocks);
      if (relation.status === 'same') { setRetrainPlan(null); onRetrain(retrainDetailFor(detail, relation.current)); return; }
      setRetrainPlan({ status: relation.status, current: relation.current, taken: saved.map((r) => r.name) });
    } catch (e) {
      setRetrainError(e instanceof Error ? e.message : String(e));
    }
  };
  const params = useMemo(() => paramRows(detail.algorithm, detail.hyperparams), [detail.algorithm, detail.hyperparams]);
  const changedCount = params.filter((p) => p.changed).length;
  const maxImportance = Math.max(...detail.feature_importance.map((f) => f.importance), 1e-9);

  const jump = useCallback((id: string) => {
    rootRef.current?.querySelector(`#model-sec-${id}`)?.scrollIntoView({ block: 'start', behavior: 'smooth' });
  }, []);
  const sections: [string, string][] = [
    ['overview', 'Overview'], ['cv', 'Folds'], ['features', 'Features'], ...(recipeModel ? [['recipe', 'Recipe'] as [string, string]] : []),
    ['config', 'Config'], ['provenance', 'Audit'], ['usedby', 'Used by'], ['try', 'Try it'], ['log', 'Log'], ['activity', 'History'],
  ];
  const toneCls = { good: 'border-bull/40 bg-bull/10 text-bull', warn: 'border-warning/40 bg-warning/10 text-warning', bad: 'border-bear/40 bg-bear/10 text-bear', info: 'border-[var(--border-color)] bg-surface-2 text-text-secondary' }[verdict.tone];

  return (
    <div ref={rootRef} className="flex h-full min-w-0 flex-col" data-testid="model-detail" data-model-id={detail.model_id}>
      <div className="space-y-1.5 border-b border-[var(--border-color)] bg-surface-1 px-3 py-2">
        <div className="flex flex-wrap items-start justify-between gap-2">
          <NameEditor key={detail.model_id} detail={detail} onSave={onSave} />
          <div className="flex shrink-0 items-center gap-1">
            <button type="button" className={accentBtn} onClick={() => void startRetrain()} data-testid="model-retrain" title="Open the Train tab with these settings">Retrain with these settings</button>
            <button type="button" className={`${btnCls} hover:text-bear`} onClick={() => onDelete(detail)} data-testid="model-delete" title="The model leaves the list; its history is kept and it can be restored">Delete (history is kept)</button>
            <VersionHistory kind="model" id={detail.model_id} onRestored={onChanged} className={btnCls} />
            <button type="button" className={btnCls} onClick={onClose} aria-label="Close detail" data-testid="model-detail-close">×</button>
          </div>
        </div>
        <div className="flex flex-wrap items-center gap-2 text-[11px] text-text-muted">
          <SourceBadge source={detail.source} />
          <span className="font-mono" data-testid="model-algorithm">{algorithmName}</span>
          {recipeModel && <span className="border border-accent/40 bg-accent/10 px-1.5 py-px font-mono text-[10px] uppercase text-accent" data-testid="model-recipe-badge">recipe</span>}
          <span title={exactTime(detail.created_at)} data-testid="model-created">{relativeTime(detail.created_at)}</span>
        </div>
        {retrainError && <div className="text-[11px] text-bear" data-testid="model-retrain-error">Could not check the saved recipes: {retrainError}</div>}
        {retrainPlan && recipeCopy && (
          <RecipeRetrainPrompt
            key={detail.model_id} detail={detail} copy={recipeCopy} plan={retrainPlan}
            onCancel={() => setRetrainPlan(null)} onRetrain={(d) => { setRetrainPlan(null); onRetrain(d); }}
          />
        )}
        <TagEditor key={`tags-${detail.model_id}`} detail={detail} onSave={onSave} />
        <nav className="flex flex-wrap gap-x-3 gap-y-0.5 pt-0.5" aria-label="Sections" data-testid="model-nav">
          {sections.map(([id, label]) => (
            <button key={id} type="button" className="text-[10px] font-mono uppercase text-text-muted hover:text-accent" onClick={() => jump(id)} data-testid={`model-nav-${id}`}>{label}</button>
          ))}
        </nav>
      </div>

      <div className="flex-1 space-y-2 overflow-auto p-2" data-testid="model-detail-body">
        {legacy && (
          <div className="border border-warning/40 bg-warning/10 px-3 py-2 text-[11px] text-warning" data-testid="model-legacy-banner">
            This run was recorded before detailed run data was kept, so fold results, feature importance and trading figures are unavailable. Retrain it to get them.
          </div>
        )}

        <Section id="overview" title="Overview">
          <div className={`mb-2 border px-2 py-1.5 text-xs ${toneCls}`} data-testid="model-verdict" data-tone={verdict.tone}>{verdict.text}</div>
          {detail.trade_mode === 'long_short' && shorting.limited && (
            <div className="mb-1 border border-warning/40 bg-warning/10 px-2 py-1 text-[11px] text-warning" data-testid="model-shorting-note">
              The trading return assumes shorting. {shorting.reasons.join('; ')}. Those automations cannot earn the short side of this result.
            </div>
          )}
          {verdict.warnings.map((w) => (
            <div key={w.id} className="mb-1 border border-warning/40 bg-warning/10 px-2 py-1 text-[11px] text-warning" data-testid={`model-warning-${w.id}`}>{w.text}</div>
          ))}
          <div className="grid grid-cols-2 gap-1.5 sm:grid-cols-4">
            <Card name="accuracy" label="Accuracy" value={pct(m.accuracy)} sub={baseline !== null ? `baseline ${pct(baseline)}` : 'no baseline'} tone={verdict.edgePts === null ? 'neutral' : verdict.edgePts >= 1 ? 'good' : 'bad'} />
            <Card name="f1" label="F1" value={num(m.f1, 3)} />
            <Card name="precision" label="Precision" value={num(m.precision, 3)} />
            <Card name="recall" label="Recall" value={num(m.recall, 3)} />
            <Card
              name="trading-return" label="Trading return" value={t ? signedPct(t.strategy_return_pct) : 'n/a'} sub={t ? `buy and hold ${signedPct(t.buy_hold_return_pct)}` : 'not recorded'}
              tone={t ? (t.strategy_return_pct >= t.buy_hold_return_pct ? 'good' : 'bad') : 'neutral'}
              extra={t && (
                <span className="cursor-help border-b border-dotted border-text-muted font-mono text-[10px] text-text-secondary" title={TRADE_MODE_HINT[detail.trade_mode ?? 'long_flat']} data-testid="model-trading-rule" data-mode={detail.trade_mode ?? 'long_flat'}>
                  Trading rule: {tradeModeLabel(detail.trade_mode)}
                </span>
              )}
            />
            <Card name="hit-rate" label="Hit rate" value={t ? pct(t.hit_rate) : 'n/a'} sub={t ? `${t.trades} trades` : undefined} />
            <Card name="max-drawdown" label="Max drawdown" value={t ? signedPct(t.max_drawdown_pct) : 'n/a'} />
            <Card name="sharpe" label="Sharpe" value={t ? num(t.sharpe, 2) : 'n/a'} />
          </div>
          <HoldoutCard detail={detail} />
          <div className="mt-2">
            <div className="mb-0.5 text-[10px] uppercase text-text-muted">Notes</div>
            <NotesEditor key={`${detail.model_id}-notes`} detail={detail} onSave={onSave} />
          </div>
        </Section>

        <Section id="cv" title="Cross-validation by fold">
          {detail.cv_folds.length === 0 ? (
            <p className="text-[11px] text-text-muted" data-testid="cv-legacy-note">Fold results were recorded before detailed run data was kept, so there is nothing to chart for this run.</p>
          ) : (
            <div className="space-y-2">
              <div className="grid gap-3 sm:grid-cols-2">
                <div>
                  <div className="mb-0.5 text-[10px] uppercase text-text-muted">Accuracy vs baseline</div>
                  <ChartLegend items={[{ label: 'Model', color: 'var(--color-accent)' }, { label: 'Majority-class baseline', color: 'var(--color-muted)' }]} />
                  <FoldAccuracyChart folds={detail.cv_folds} />
                </div>
                <div>
                  <div className="mb-0.5 text-[10px] uppercase text-text-muted">Trade return per fold</div>
                  <ChartLegend items={[{ label: 'Gain', color: 'var(--color-bull)' }, { label: 'Loss', color: 'var(--color-bear)' }]} />
                  <FoldReturnChart folds={detail.cv_folds} />
                </div>
              </div>
              <div className="overflow-x-auto">
                <table className="w-full text-[11px]" data-testid="cv-table">
                  <thead>
                    <tr className="text-left text-[10px] uppercase text-text-muted">
                      {['Fold', 'Train', 'Test', 'Accuracy', 'Baseline', 'F1', 'Return', 'Hit rate', 'Trades'].map((h) => <th key={h} className="px-1.5 py-0.5 font-normal">{h}</th>)}
                    </tr>
                  </thead>
                  <tbody className="font-mono">
                    {detail.cv_folds.map((f) => (
                      <tr key={f.fold} className="border-t border-[var(--border-color)]" data-testid="cv-row">
                        <td className="px-1.5 py-0.5">{f.fold}</td>
                        <td className="px-1.5 py-0.5">{f.train_rows}</td>
                        <td className="px-1.5 py-0.5">{f.test_rows}</td>
                        <td className="px-1.5 py-0.5">{pct(f.accuracy)}</td>
                        <td className="px-1.5 py-0.5">{pct(f.baseline_accuracy)}</td>
                        <td className="px-1.5 py-0.5">{num(f.f1, 3)}</td>
                        <td className={`px-1.5 py-0.5 ${f.trade_return_pct === null ? '' : f.trade_return_pct >= 0 ? 'text-bull' : 'text-bear'}`}>{signedPct(f.trade_return_pct, 2)}</td>
                        <td className="px-1.5 py-0.5">{pct(f.hit_rate)}</td>
                        <td className="px-1.5 py-0.5">{f.trades ?? 'n/a'}</td>
                      </tr>
                    ))}
                  </tbody>
                </table>
              </div>
            </div>
          )}
        </Section>

        <Section id="features" title="Feature importance">
          {detail.feature_importance.length === 0 ? (
            <p className="text-[11px] text-text-muted" data-testid="features-empty">
              {legacy ? 'Feature importance was recorded before detailed run data was kept.' : noImportanceText}
            </p>
          ) : (
            <>
              {selectionNote && <p className="mb-1 text-[11px] text-text-muted" data-testid="features-selection-note">{selectionNote}</p>}
              <HorizontalBars
                testId="feature-bars"
                items={[...detail.feature_importance].sort((a, b) => b.importance - a.importance).map((f) => ({ label: f.feature, value: f.importance }))}
                max={maxImportance}
              />
            </>
          )}
        </Section>

        {recipeModel && (
          <Section id="recipe" title="Recipe">
            {recipeCopy ? (
              <div className="space-y-2">
                <RecipeSummary recipe={recipeCopy} />
                <p className="border border-[var(--border-color)] bg-surface-0 px-2 py-1 text-[11px] text-text-secondary" data-testid="recipe-saved-copy-note">
                  This is a saved copy of the recipe as it was when the model was trained. Editing or deleting the recipe later does not change this model.
                </p>
                <p className={`text-[11px] ${savedStatus.status === 'same' || savedStatus.status === 'unknown' ? 'text-text-muted' : 'text-warning'}`} data-testid="recipe-saved-status" data-status={savedStatus.status}>
                  {STATUS_TEXT[savedStatus.status]}
                </p>
              </div>
            ) : (
              <p className="text-[11px] text-warning" data-testid="recipe-missing-copy">This model was built from a recipe, but the recipe was not recorded with it, so it cannot be shown or reused.</p>
            )}
          </Section>
        )}

        <Section id="config" title="Configuration">
          <dl className="grid grid-cols-[8rem_1fr] gap-x-3 gap-y-1 text-[11px]" data-testid="model-config">
            <dt className="text-text-muted">Algorithm</dt><dd className="font-mono" data-testid="config-algorithm">{recipeModel ? `${algorithmName} (recipe)` : algorithmName}</dd>
            <dt className="text-text-muted">CV splits</dt><dd className="font-mono" data-testid="config-splits">{detail.n_splits}</dd>
            <dt className="text-text-muted">Lag periods</dt><dd className="font-mono" data-testid="config-lags">{detail.lag_periods.length ? detail.lag_periods.join(', ') : 'none'}</dd>
            <dt className="text-text-muted">Label</dt><dd data-testid="config-label">{labelText(detail.label)}</dd>
            <dt className="text-text-muted">Trading rule</dt><dd className="font-mono" data-testid="config-trade-mode" title={TRADE_MODE_HINT[detail.trade_mode ?? 'long_flat']}>{tradeModeLabel(detail.trade_mode)}</dd>
            <dt className="text-text-muted">Fee assumed</dt><dd className="font-mono" data-testid="config-fee">{detail.fee_pct ? `${detail.fee_pct}% per round trip, taken off every forward return` : 'none'}</dd>
            <dt className="text-text-muted">Random state</dt>
            <dd className="font-mono" data-testid="config-random-state">{detail.hyperparams.random_state === undefined ? 'default (fixed seed)' : fmtValue(detail.hyperparams.random_state)}</dd>
            <dt className="text-text-muted">Dataset</dt>
            <dd>
              {detail.dataset
                ? <button type="button" className="font-mono text-accent hover:underline" onClick={() => openItem('dataset', detail.dataset ?? '')} data-testid="config-dataset-link">{detail.dataset}</button>
                : <span className="font-mono text-text-secondary" data-testid="config-data-path" title="Trained directly from a file">{detail.data_path}</span>}
            </dd>
            <dt className="text-text-muted">Symbols</dt><dd className="font-mono" data-testid="config-symbols">{detail.symbols.length ? detail.symbols.join(', ') : 'n/a'}</dd>
            <dt className="text-text-muted">Timeframe</dt><dd className="font-mono" data-testid="config-timeframe">{detail.timeframe ?? 'n/a'}</dd>
            <dt className="text-text-muted">Rows</dt><dd className="font-mono" data-testid="config-rows">{detail.rows ?? 'n/a'}</dd>
            <dt className="text-text-muted">Target column</dt><dd className="font-mono">{detail.target_column}</dd>
            <dt className="text-text-muted">Features ({detail.feature_names.length})</dt>
            <dd className="flex flex-wrap gap-1">{detail.feature_names.map((f) => <span key={f} className="border border-[var(--border-color)] bg-surface-2 px-1 font-mono text-[10px]">{f}</span>)}</dd>
            {detail.context_symbols.length > 0 && (<>
              <dt className="text-text-muted">Context symbols</dt>
              <dd className="font-mono" data-testid="config-context-symbols" title="Read by custom formulas with other(&quot;SYMBOL&quot;, field). Not rows of the training data.">{detail.context_symbols.join(', ')}</dd>
            </>)}
            {detail.estimator_file && (<>
              <dt className="text-text-muted">Estimator file</dt>
              <dd>
                <span className="font-mono" data-testid="config-estimator">{detail.estimator_file}</span>
                <div className="mt-0.5 border border-warning/40 bg-warning/10 px-2 py-1 text-[11px] text-warning" data-testid="config-estimator-warning">
                  Custom code. Training and scoring run the copy saved with this model, not later edits to estimators/{detail.estimator_file}.
                </div>
              </dd>
            </>)}
          </dl>
          {detail.custom_features.length > 0 && (
            <div className="mt-2" data-testid="config-custom-features">
              <div className="mb-0.5 text-[10px] uppercase text-text-muted">Custom features ({detail.custom_features.length})</div>
              <ul className="space-y-1">
                {detail.custom_features.map((c) => (
                  <li key={c.name} className="flex items-center gap-2 text-[11px]" data-testid={`custom-feature-${c.name}`}>
                    <span className="w-32 shrink-0 truncate font-mono text-accent" title={c.name} data-testid={`custom-feature-name-${c.name}`}>{c.name}</span>
                    <code className="min-w-0 flex-1 break-all bg-surface-0 px-1.5 py-0.5 font-mono text-[11px] text-text-primary" data-testid={`custom-feature-expr-${c.name}`}>{c.expr}</code>
                    <CopyButton text={c.expr} testId={`custom-feature-copy-${c.name}`} />
                  </li>
                ))}
              </ul>
            </div>
          )}
          {recipeModel ? (
            <p className="mt-2 text-[11px] text-text-muted" data-testid="config-recipe-params">
              The parameters of each step and model are part of the recipe. See the Recipe section above.
            </p>
          ) : (<>
            <div className="mt-2 text-[10px] uppercase text-text-muted">Hyperparameters ({changedCount} changed from defaults)</div>
            <table className="mt-0.5 w-full text-[11px]" data-testid="hyper-table">
              <thead><tr className="text-left text-[10px] uppercase text-text-muted"><th className="px-1.5 py-0.5 font-normal">Parameter</th><th className="px-1.5 py-0.5 font-normal">Used</th><th className="px-1.5 py-0.5 font-normal">Default</th><th className="px-1.5 py-0.5 font-normal" /></tr></thead>
              <tbody className="font-mono">
                {params.map((p) => (
                  <tr key={p.key} className={`border-t border-[var(--border-color)] ${p.changed ? 'text-accent' : 'text-text-secondary'}`} data-testid={`hyper-row-${p.key}`} data-changed={p.changed}>
                    <td className="px-1.5 py-0.5">{p.key}</td>
                    <td className="px-1.5 py-0.5">{fmtValue(p.value)}</td>
                    <td className="px-1.5 py-0.5">{p.hasDefault ? fmtValue(p.def) : 'n/a'}</td>
                    <td className="px-1.5 py-0.5 text-[10px] uppercase">{p.changed ? 'changed' : 'default'}</td>
                  </tr>
                ))}
                {params.length === 0 && <tr><td className="px-1.5 py-0.5 text-text-muted" colSpan={4}>No tunable parameters recorded.</td></tr>}
              </tbody>
            </table>
          </>)}
        </Section>

        <Section id="provenance" title="Provenance and audit">
          <dl className="grid grid-cols-[8rem_1fr] gap-x-3 gap-y-1 text-[11px]" data-testid="model-provenance">
            <dt className="text-text-muted">Trained by</dt><dd><SourceBadge source={detail.source} testId="prov-source" /></dd>
            <dt className="text-text-muted">Created</dt><dd className="font-mono" data-testid="prov-created">{exactTime(detail.created_at)}</dd>
            <dt className="text-text-muted">Duration</dt><dd className="font-mono" data-testid="prov-duration">{detail.duration_secs === null ? 'n/a' : `${detail.duration_secs.toFixed(1)} s`}</dd>
            <dt className="text-text-muted">Rows</dt><dd className="font-mono">{detail.rows ?? 'n/a'}</dd>
            <dt className="text-text-muted">Artifact size</dt><dd className="font-mono" data-testid="prov-size">{bytesLabel(detail.artifact_bytes)}</dd>
            <dt className="text-text-muted">Model id</dt><dd className="break-all font-mono">{detail.model_id}</dd>
            <dt className="text-text-muted">Libraries</dt>
            <dd className="font-mono" data-testid="prov-libs">
              {Object.keys(detail.library_versions).length ? Object.entries(detail.library_versions).map(([k, v]) => `${k} ${v}`).join(' · ') : 'n/a'}
            </dd>
          </dl>
        </Section>

        <Section id="usedby" title={`Used by (${detail.used_by.length})`}>
          {detail.used_by.length === 0 ? (
            <p className="text-[11px] text-text-muted" data-testid="used-by-empty">No automation uses this model yet. Create one below.</p>
          ) : (
            <ul className="space-y-1">
              {detail.used_by.map((u) => (
                <li key={u.id} className="flex items-center gap-2 text-[11px]" data-testid="used-by-row">
                  <span className={`inline-block h-1.5 w-1.5 rounded-full ${u.enabled ? 'bg-bull' : 'bg-text-muted'}`} />
                  <span className="min-w-0 flex-1 truncate" data-testid="used-by-name">{u.name}</span>
                  <span className="font-mono text-text-muted">{u.symbol}</span>
                  <span className={`font-mono text-[10px] uppercase ${u.enabled ? 'text-bull' : 'text-text-muted'}`} data-testid="used-by-state">{u.enabled ? 'enabled' : 'paused'}</span>
                  <button type="button" className={btnCls} onClick={() => openItem('automation', u.id)} data-testid="used-by-open">Open</button>
                </li>
              ))}
            </ul>
          )}
        </Section>

        <Section id="try" title="Try it">
          <TryIt key={detail.model_id} detail={detail} />
          <div className="mt-3 border-t border-[var(--border-color)] pt-2">
            <div className="mb-1 text-[10px] uppercase text-text-muted">Create automation from this model</div>
            <AutomationForm key={`auto-${detail.model_id}`} detail={detail} onCreated={onChanged} />
          </div>
        </Section>

        <Section
          id="log" title="Training log" defaultOpen={false}
          right={<button type="button" className="text-[10px] text-text-muted hover:text-text-primary" onClick={() => setLogOpen((o) => !o)} data-testid="model-log-wrap">{logOpen ? 'Collapse' : 'Expand'}</button>}
        >
          <pre className={`overflow-auto whitespace-pre-wrap break-words bg-surface-0 p-2 font-mono text-[10px] text-text-secondary ${logOpen ? 'max-h-[28rem]' : 'max-h-28'}`} data-testid="model-log">{detail.log_tail || 'No log was kept for this run.'}</pre>
        </Section>

        <Section id="activity" title="Activity timeline">
          <ModelActivity modelId={detail.model_id} />
        </Section>
      </div>
    </div>
  );
};
