import React, { useEffect, useRef, useState } from 'react';
import {
  CUSTOM_NAME_MAX, EXAMPLE_FORMULAS, LANGUAGE_NOTES, LANGUAGE_REFERENCE, caretLine, newRowKey, otherSymbols, uniqueFeatureName,
  validateFeatureName, type CustomRow, type RowCheck,
} from './datasetModel';

const inputCls =
  'px-2 py-1 text-xs bg-surface-0 border border-[var(--border-color)] text-text-primary focus:border-accent focus:outline-none disabled:opacity-50';
const btnCls =
  'px-2 py-1 text-xs font-mono border border-[var(--border-color)] bg-surface-2 text-text-secondary hover:text-text-primary hover:border-accent disabled:opacity-50 disabled:cursor-not-allowed';

function fmtValue(v: number | null): string {
  if (v === null || !Number.isFinite(v)) return '-';
  return Math.abs(v) >= 1000 ? v.toFixed(0) : v.toFixed(4);
}

export interface CustomFeatureEditorProps {
  rows: CustomRow[];
  onChange: (rows: CustomRow[]) => void;
  checks: Record<string, RowCheck>;
  catalogIds: string[];
  /** Symbol the formulas are checked against (the first selected symbol). */
  checkSymbol: string | null;
  contextSymbols: string[];
  onAddContext: (symbol: string) => void;
}

export const CustomFeatureEditor: React.FC<CustomFeatureEditorProps> = ({
  rows, onChange, checks, catalogIds, checkSymbol, contextSymbols, onAddContext,
}) => {
  const [showRef, setShowRef] = useState(false);
  const inputs = useRef<Record<string, HTMLInputElement | null>>({});
  const nameInputs = useRef<Record<string, HTMLInputElement | null>>({});
  const lastFocus = useRef<string | null>(null);
  /** Row whose name field should get the cursor once it exists (after pressing Add). */
  const focusNext = useRef<string | null>(null);

  useEffect(() => {
    const key = focusNext.current;
    if (key && nameInputs.current[key]) { nameInputs.current[key]?.focus(); focusNext.current = null; }
  }, [rows]);

  const update = (key: string, patch: Partial<CustomRow>) =>
    onChange(rows.map((r) => (r.key === key ? { ...r, ...patch } : r)));
  const addRow = (name = '', expr = '') => {
    const key = newRowKey();
    const unique = name ? uniqueFeatureName(name, [...rows.map((r) => r.name), ...catalogIds]) : '';
    onChange([...rows, { key, name: unique, expr }]);
    lastFocus.current = key;
    return key;
  };

  /** Put a snippet into the formula that was edited last, at the cursor (or into a new row). */
  const insertSnippet = (snippet: string) => {
    const target = rows.find((r) => r.key === lastFocus.current) ?? rows[rows.length - 1];
    if (!target) { addRow('', snippet); return; }
    const el = inputs.current[target.key];
    const start = el?.selectionStart ?? target.expr.length;
    const end = el?.selectionEnd ?? start;
    const expr = target.expr.slice(0, start) + snippet + target.expr.slice(end);
    update(target.key, { expr });
    lastFocus.current = target.key;
    requestAnimationFrame(() => {
      const input = inputs.current[target.key];
      if (!input) return;
      input.focus();
      const caret = start + snippet.length;
      input.setSelectionRange(caret, caret);
    });
  };

  return (
    <div className="space-y-2" data-testid="ml-cf-editor">
      <div className="text-[11px] text-text-muted">
        Write your own indicators as formulas over the bars. Each one becomes a column of the dataset, so you can train on custom features alone.
      </div>

      {rows.length === 0 && <div className="text-[11px] text-text-muted" data-testid="ml-cf-empty">No custom features. Add one, or click an example below.</div>}

      {rows.map((row, i) => {
        const check = checks[row.key] ?? { status: 'empty' as const };
        const others = rows.filter((r) => r.key !== row.key).map((r) => r.name);
        const nameError = validateFeatureName(row.name, catalogIds, others);
        const result = check.result;
        const missingContext = otherSymbols(row.expr).filter((s) => !contextSymbols.includes(s));
        return (
          <div key={row.key} className="space-y-1 border border-[var(--border-color)] bg-surface-0 p-2" data-testid={`ml-cf-row-${i}`}>
            <div className="flex flex-wrap items-start gap-1.5">
              <label className="text-[10px] uppercase tracking-wider text-text-muted">
                Name
                <input
                  ref={(el) => { nameInputs.current[row.key] = el; }}
                  className={`${inputCls} mt-0.5 block w-40 font-mono normal-case`}
                  value={row.name}
                  maxLength={CUSTOM_NAME_MAX + 10}
                  onChange={(e) => update(row.key, { name: e.target.value })}
                  aria-invalid={nameError !== null}
                  placeholder="my_feature"
                  data-testid={`ml-cf-name-${i}`}
                />
              </label>
              <label className="min-w-[16rem] flex-1 text-[10px] uppercase tracking-wider text-text-muted">
                Formula
                <input
                  ref={(el) => { inputs.current[row.key] = el; }}
                  className={`${inputCls} mt-0.5 block w-full font-mono normal-case`}
                  value={row.expr}
                  onChange={(e) => update(row.key, { expr: e.target.value })}
                  onFocus={() => { lastFocus.current = row.key; }}
                  aria-invalid={check.status === 'error'}
                  placeholder="sma(close, 20) / close - 1"
                  spellCheck={false}
                  autoComplete="off"
                  data-testid={`ml-cf-expr-${i}`}
                />
              </label>
              <button type="button" className={`${btnCls} mt-4 hover:text-bear`} onClick={() => onChange(rows.filter((r) => r.key !== row.key))} aria-label={`Remove custom feature ${i + 1}`} data-testid={`ml-cf-remove-${i}`}>Remove</button>
            </div>

            {nameError && (row.name || row.expr) && <div className="text-[11px] text-bear" data-testid={`ml-cf-name-error-${i}`}>{nameError}</div>}

            <div className="text-[11px]" data-testid={`ml-cf-status-${i}`} data-state={check.status} role="status">
              {check.status === 'empty' && <span className="text-text-muted">Enter a formula.</span>}
              {check.status === 'nosymbol' && <span className="text-text-muted" data-testid={`ml-cf-nosymbol-${i}`}>Add a symbol above to check this formula against real bars.</span>}
              {check.status === 'checking' && <span className="text-text-muted">Checking{checkSymbol ? ` on ${checkSymbol}` : ''}...</span>}
              {check.status === 'ok' && result && (
                <span className="text-bull" data-testid={`ml-cf-ok-${i}`}>
                  Valid. <span className="text-text-secondary" data-testid={`ml-cf-warmup-${i}`}>Warm-up {result.warmup_bars} bar{result.warmup_bars === 1 ? '' : 's'}.</span>
                </span>
              )}
              {check.status === 'error' && result && (
                <div className="text-bear" data-testid={`ml-cf-error-${i}`}>
                  {result.error ?? 'The formula is not valid.'}
                  {result.error_pos !== null && (
                    <pre className="mt-0.5 overflow-x-auto border border-bear/30 bg-bear/5 px-1.5 py-0.5 font-mono text-[11px] leading-tight" data-testid={`ml-cf-marker-${i}`} data-pos={result.error_pos}>
                      {row.expr.trim()}{'\n'}{caretLine(row.expr.trim(), result.error_pos)}
                    </pre>
                  )}
                </div>
              )}
            </div>

            {check.status === 'ok' && result && result.sample.length > 0 && (
              <div className="flex flex-wrap items-center gap-x-3 gap-y-0.5 text-[11px] font-mono text-text-secondary" data-testid={`ml-cf-sample-${i}`}>
                <span className="text-text-muted">Last values on {checkSymbol}</span>
                {result.sample.map((p) => (
                  <span key={p.time} title={p.time}>{p.time.slice(0, 10)} <span className="text-text-primary">{fmtValue(p.value)}</span></span>
                ))}
              </div>
            )}

            {missingContext.map((s) => (
              <div key={s} className="flex flex-wrap items-center gap-2 text-[11px] text-warning" data-testid={`ml-cf-context-warn-${i}-${s}`}>
                <span>This formula reads {s}, which is not a context symbol yet.</span>
                <button type="button" className={btnCls} onClick={() => onAddContext(s)} data-testid={`ml-cf-context-add-${i}-${s}`}>Add {s} as context</button>
              </div>
            ))}
          </div>
        );
      })}

      <div className="flex flex-wrap items-center gap-1.5">
        <button type="button" className={btnCls} onClick={() => { focusNext.current = addRow(); }} data-testid="ml-cf-add">Add custom feature</button>
        <button type="button" className={btnCls} onClick={() => setShowRef(!showRef)} aria-expanded={showRef} data-testid="ml-cf-reference-toggle">
          {showRef ? 'Hide formula reference' : 'Formula reference'}
        </button>
        <span className="text-[11px] font-mono text-text-secondary" data-testid="ml-cf-count">{rows.length} custom</span>
      </div>

      <div className="flex flex-wrap items-center gap-1.5" data-testid="ml-cf-examples">
        <span className="text-[11px] text-text-muted">Examples</span>
        {EXAMPLE_FORMULAS.map((ex) => (
          <button key={ex.id} type="button" className={btnCls} title={`${ex.help}: ${ex.expr}`} onClick={() => addRow(ex.name, ex.expr)} data-testid={`ml-cf-example-${ex.id}`}>
            {ex.name}
          </button>
        ))}
      </div>

      {showRef && (
        <div className="space-y-1 border border-[var(--border-color)] bg-surface-0 p-2" data-testid="ml-cf-reference">
          <div className="text-[11px] text-text-muted">Click a function to insert its example at the cursor of the formula you edited last.</div>
          <ul className="grid grid-cols-1 gap-x-4 gap-y-0.5 md:grid-cols-2">
            {LANGUAGE_REFERENCE.map((fn) => (
              <li key={fn.name} className="text-[11px]">
                <button type="button" className="font-mono text-accent hover:underline" onClick={() => insertSnippet(fn.insert)} data-testid={`ml-cf-ref-${fn.name}`} title={`Insert ${fn.insert}`}>
                  {fn.signature}
                </button>
                <span className="text-text-secondary"> {fn.help} </span>
                <span className="font-mono text-text-muted">e.g. {fn.example}</span>
              </li>
            ))}
          </ul>
          <ul className="list-disc pl-4 text-[11px] text-text-muted">
            {LANGUAGE_NOTES.map((n) => <li key={n}>{n}</li>)}
          </ul>
        </div>
      )}
    </div>
  );
};
