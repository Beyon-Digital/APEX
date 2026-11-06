import React, { useEffect, useState } from 'react';
import { estimatorTemplate, ledgerVersion } from '../../lib/tauri';
import type { VersionDto } from '../../lib/types';
import { DeletedObjects } from '../audit/DeletedObjects';
import { VersionHistory } from '../audit/VersionHistory';
import { useMLStore } from '../../stores/mlStore';

const inputCls =
  'px-2 py-1 text-xs bg-surface-0 border border-[var(--border-color)] text-text-primary focus:border-accent focus:outline-none disabled:opacity-50';
const btnCls =
  'px-2 py-1 text-xs font-mono border border-[var(--border-color)] bg-surface-2 text-text-secondary hover:text-text-primary hover:border-accent disabled:opacity-50 disabled:cursor-not-allowed';
const primaryBtnCls =
  'px-3 py-1.5 text-xs font-mono font-semibold bg-accent text-on-accent hover:opacity-90 disabled:opacity-50 disabled:cursor-not-allowed';

/** Used when the backend cannot supply a template. */
export const FALLBACK_TEMPLATE = `"""A custom estimator for APEX Terminal."""
from sklearn.ensemble import ExtraTreesClassifier


def make_estimator(params: dict):
    """Return a scikit-learn style classifier: fit(X, y), predict(X), optionally predict_proba(X)."""
    return ExtraTreesClassifier(
        n_estimators=int(params.get("n_estimators", 200)),
        random_state=int(params.get("random_state", 42)),
        n_jobs=1,
    )
`;

const withExtension = (name: string) => {
  const n = name.trim();
  return n === '' || n.endsWith('.py') ? n : `${n}.py`;
};

/**
 * Write, save, pick and delete custom estimator files (`estimators/<name>.py`).
 * The file used for training is `settings.estimatorFile`.
 */
export const EstimatorEditor: React.FC = () => {
  const files = useMLStore((s) => s.estimatorFiles);
  const selected = useMLStore((s) => s.settings.estimatorFile);
  const patch = useMLStore((s) => s.patchSettings);
  const refresh = useMLStore((s) => s.refreshEstimators);
  const saveEstimator = useMLStore((s) => s.saveEstimator);
  const removeEstimator = useMLStore((s) => s.removeEstimator);

  const [name, setName] = useState('');
  const [content, setContent] = useState('');
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [confirmDelete, setConfirmDelete] = useState<string | null>(null);

  useEffect(() => { void refresh(); }, [refresh]);

  // Show the file already chosen for training when the editor opens empty.
  useEffect(() => {
    if (name === '' && content === '' && selected) {
      const file = files.find((f) => f.name === selected);
      if (file) { setName(file.name); setContent(file.content); }
    }
  }, [files, selected, name, content]);

  const open = (fileName: string) => {
    const file = files.find((f) => f.name === fileName);
    if (!file) return;
    setName(file.name); setContent(file.content); setError(null); setNotice(null); setConfirmDelete(null);
  };

  const newFromTemplate = async () => {
    setError(null); setNotice(null);
    let template = '';
    try { template = (await estimatorTemplate()).trim(); } catch { template = ''; }
    setName('');
    setContent(template || FALLBACK_TEMPLATE);
  };

  const save = async () => {
    const fileName = withExtension(name);
    if (!fileName) { setError('Give the file a name, for example my_model.py.'); return; }
    setBusy(true); setError(null); setNotice(null);
    const result = await saveEstimator(fileName, content);
    setBusy(false);
    if (result.error || !result.file) { setError(result.error ?? 'Save failed'); return; }
    setName(result.file.name);
    patch({ estimatorFile: result.file.name });
    setNotice(`Saved ${result.file.name} and selected it for training.`);
  };

  // A restore added a new version: show that content if this file is the one open in the editor.
  const restored = async (record: VersionDto) => {
    await refresh();
    if (record.object_id !== withExtension(name)) return;
    try {
      const v = await ledgerVersion('estimator', record.object_id, record.version);
      if (v.text !== null) { setContent(v.text); setError(null); setNotice(`Restored as version ${record.version}.`); }
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    }
  };

  const remove = async (fileName: string) => {
    setBusy(true);
    const err = await removeEstimator(fileName);
    setBusy(false);
    setConfirmDelete(null);
    if (err) { setError(err); return; }
    if (name === fileName) { setName(''); setContent(''); }
    setNotice(`Deleted ${fileName}.`);
  };

  return (
    <div className="space-y-2 border border-[var(--border-color)] bg-surface-0 p-2" data-testid="ml-estimator-editor">
      <div className="text-[11px] text-text-secondary" data-testid="ml-est-contract">
        The file must define <span className="font-mono text-text-primary">make_estimator(params: dict)</span> and return a scikit-learn style classifier with <span className="font-mono">fit</span> and <span className="font-mono">predict</span> (<span className="font-mono">predict_proba</span> is optional). The parameters below arrive as <span className="font-mono">params</span>.
      </div>
      <div className="border border-warning/40 bg-warning/10 px-2 py-1 text-[11px] text-warning" data-testid="ml-est-warning">
        This file is arbitrary Python that runs on this machine when you train and when an automation scores with the model. Only use code you trust.
      </div>

      <div className="flex gap-3">
        <div className="w-56 shrink-0 space-y-1">
          <div className="text-[11px] text-text-secondary">Saved files</div>
          {files.length === 0 ? (
            <div className="text-[11px] text-text-muted" data-testid="ml-est-empty">No estimator files yet.</div>
          ) : (
            <ul className="space-y-1" data-testid="ml-est-list">
              {files.map((f) => (
                <li key={f.name} className={`border p-1 ${selected === f.name ? 'border-accent' : 'border-[var(--border-color)]'}`} data-testid={`ml-est-item-${f.name}`}>
                  <div className="truncate font-mono text-xs text-text-primary">{f.name}</div>
                  {confirmDelete === f.name ? (
                    <div className="flex items-center gap-1 text-[11px]">
                      <span className="text-bear" title="The file leaves the list, but every version stays in the history and it can be restored.">Delete (history is kept)?</span>
                      <button type="button" className="font-mono text-bear hover:underline" onClick={() => void remove(f.name)} disabled={busy} data-testid={`ml-est-delete-confirm-${f.name}`}>yes</button>
                      <button type="button" className="font-mono text-text-secondary hover:underline" onClick={() => setConfirmDelete(null)} data-testid={`ml-est-delete-cancel-${f.name}`}>no</button>
                    </div>
                  ) : (
                    <div className="flex flex-wrap gap-x-2 text-[10px] font-mono text-accent">
                      <button type="button" className="hover:underline" onClick={() => open(f.name)} data-testid={`ml-est-open-${f.name}`}>edit</button>
                      <button type="button" className="hover:underline" onClick={() => patch({ estimatorFile: f.name })} disabled={selected === f.name} data-testid={`ml-est-use-${f.name}`}>{selected === f.name ? 'in use' : 'use'}</button>
                      <button type="button" className="text-bear hover:underline" onClick={() => setConfirmDelete(f.name)} data-testid={`ml-est-delete-${f.name}`} title="History is kept; the file can be restored later">delete (history is kept)</button>
                    </div>
                  )}
                </li>
              ))}
            </ul>
          )}
          <DeletedObjects kind="estimator" onRestored={() => void refresh()} />
          <div className="text-[11px] text-text-muted" data-testid="ml-est-selected">
            {selected ? <>Training with <span className="font-mono text-text-primary">{selected}</span></> : 'No file selected for training.'}
          </div>
        </div>

        <div className="min-w-0 flex-1 space-y-1">
          <div className="flex flex-wrap items-center gap-2">
            <label className="text-[11px] text-text-secondary" htmlFor="ml-est-name">File name</label>
            <input id="ml-est-name" className={`${inputCls} w-48 font-mono`} value={name} onChange={(e) => setName(e.target.value)} placeholder="my_model.py" data-testid="ml-est-name" />
            <button type="button" className={btnCls} onClick={() => void newFromTemplate()} data-testid="ml-est-new">New from template</button>
            <button type="button" className={primaryBtnCls} onClick={() => void save()} disabled={busy || content.trim() === ''} data-testid="ml-est-save">Save</button>
            {files.some((f) => f.name === withExtension(name)) && <VersionHistory kind="estimator" id={withExtension(name)} onRestored={(r) => void restored(r)} className={btnCls} />}
          </div>
          <textarea
            className={`${inputCls} block h-56 w-full resize-y font-mono leading-snug`} spellCheck={false} wrap="off"
            value={content} onChange={(e) => setContent(e.target.value)} aria-label="Estimator source" data-testid="ml-est-code"
            placeholder="Click New from template to start."
          />
          {error && <div className="border border-bear/30 bg-bear/10 px-2 py-1 text-xs text-bear" data-testid="ml-est-error">{error}</div>}
          {notice && !error && <div className="text-[11px] text-bull" data-testid="ml-est-notice">{notice}</div>}
        </div>
      </div>
    </div>
  );
};
