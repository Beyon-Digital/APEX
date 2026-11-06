import React, { useMemo, useState } from 'react';
import type { ParamDefDto } from '../../lib/types';
import {
  changedCount, isChanged, mergeParams, parseAdvanced, placeholderFor, validateParams, type HyperparamValues,
} from './hyperparams';

const inputCls =
  'px-2 py-1 text-xs bg-surface-0 border border-[var(--border-color)] text-text-primary focus:border-accent focus:outline-none disabled:opacity-50';
const btnCls =
  'px-2 py-1 text-xs font-mono border border-[var(--border-color)] bg-surface-2 text-text-secondary hover:text-text-primary hover:border-accent disabled:opacity-50 disabled:cursor-not-allowed';

export interface ParamEditorProps {
  defs: ParamDefDto[];
  values: HyperparamValues;
  advanced: string;
  onValues: (values: HyperparamValues) => void;
  onAdvanced: (text: string) => void;
  /** Extra controls placed on the toolbar row (quick-fill buttons). */
  toolbarExtra?: React.ReactNode;
  /** Leading part of every data-testid and element id (default "ml"). Needed when several editors share a page. */
  prefix?: string;
  /** Show the "Advanced parameters (JSON)" box (default true). */
  showAdvanced?: boolean;
  /** Hide the search box and "Changed only" filter (for editors with a handful of fields). */
  hideSearch?: boolean;
}

const boolDefault = (def: ParamDefDto) => def.default === true || def.default === 'true';

/**
 * Parameter editor generated from the backend's parameter list for the chosen algorithm.
 * Every field starts empty with the default shown as its placeholder; only values that differ from
 * the default are ever sent. The JSON box takes any further parameters the generated list lacks.
 */
export const ParamEditor: React.FC<ParamEditorProps> = ({
  defs, values, advanced, onValues, onAdvanced, toolbarExtra, prefix = 'ml', showAdvanced = true, hideSearch = false,
}) => {
  const t = (name: string) => `${prefix}-${name}`;
  const [query, setQuery] = useState('');
  const [changedOnly, setChangedOnly] = useState(false);

  const errors = useMemo(() => validateParams(defs, values), [defs, values]);
  const changed = changedCount(defs, values);
  const merged = useMemo(() => mergeParams(defs, values, advanced), [defs, values, advanced]);
  const advancedParse = useMemo(() => parseAdvanced(advanced), [advanced]);
  const advancedError = advancedParse.ok ? null : advancedParse.error;
  const collisions = merged.collisions;
  const needle = query.trim().toLowerCase();

  const visible = defs.filter((d) => {
    if (changedOnly && !isChanged(d, values)) return false;
    if (!needle) return true;
    return `${d.key} ${d.label} ${d.help}`.toLowerCase().includes(needle);
  });

  const setValue = (key: string, text: string) => onValues({ ...values, [key]: text });
  const resetKey = (key: string) => {
    const next = { ...values };
    delete next[key];
    onValues(next);
  };
  const anythingToReset = Object.values(values).some((v) => v.trim() !== '') || advanced.trim() !== '';

  return (
    <div className="space-y-2" data-testid={t('param-editor')}>
      <div className="flex flex-wrap items-center gap-1.5 pt-1">
        <span className="text-[11px] text-text-secondary">Parameters</span>
        <span className="text-[11px] text-text-muted" data-testid={t('hp-changed-count')}>{changed} changed from default</span>
        <span className="ml-auto flex flex-wrap items-center gap-1.5">
          {toolbarExtra}
          <button type="button" className={btnCls} onClick={() => { onValues({}); onAdvanced(''); }} disabled={!anythingToReset} data-testid={t('hp-reset-all')}>Reset all</button>
        </span>
      </div>

      {defs.length > 0 && !hideSearch && (
        <div className="flex flex-wrap items-center gap-3">
          <input
            className={`${inputCls} w-56`} type="search" value={query} onChange={(e) => setQuery(e.target.value)}
            placeholder={`Search ${defs.length} parameters`} aria-label="Search parameters" data-testid={t('param-search')}
          />
          <label className="flex cursor-pointer items-center gap-1 text-[11px] text-text-secondary">
            <input type="checkbox" className="accent-[rgb(var(--color-accent-rgb))]" checked={changedOnly} onChange={(e) => setChangedOnly(e.target.checked)} data-testid={t('param-changed-only')} />
            Changed only
          </label>
          <span className="text-[11px] text-text-muted" data-testid={t('param-shown')}>Showing {visible.length} of {defs.length}</span>
        </div>
      )}

      {defs.length === 0 ? (
        <div className="text-[11px] text-text-muted" data-testid={t('param-none')}>
          This algorithm has no listed parameters. Pass any it accepts in the Advanced JSON box below.
        </div>
      ) : visible.length === 0 ? (
        <div className="text-[11px] text-text-muted" data-testid={t('param-empty')}>No parameters match.</div>
      ) : (
        <div className="grid grid-cols-1 gap-2 md:grid-cols-2" data-testid={t('hp-editor')}>
          {visible.map((def) => {
            const isOn = isChanged(def, values);
            const err = errors[def.key];
            const text = values[def.key] ?? '';
            const id = t(`hp-${def.key}`);
            const boolOn = text === '' ? boolDefault(def) : text === 'true';
            return (
              <div key={def.key} className={`border p-2 ${err ? 'border-bear/50' : isOn ? 'border-accent/50' : 'border-[var(--border-color)]'} bg-surface-0`} data-testid={t(`hp-field-${def.key}`)}>
                <div className="flex items-center justify-between gap-2">
                  <label className="text-[11px] text-text-secondary" htmlFor={id}>
                    {def.label} <span className="font-mono text-text-muted">{def.key}</span>
                  </label>
                  {(isOn || text !== '') && (
                    <button type="button" className="text-[10px] font-mono text-accent hover:underline" onClick={() => resetKey(def.key)} data-testid={t(`hp-reset-${def.key}`)}>reset</button>
                  )}
                </div>
                {def.type === 'choice' ? (
                  <select id={id} className={`${inputCls} mt-0.5 block w-full`} value={text} onChange={(e) => setValue(def.key, e.target.value)} data-testid={id}>
                    <option value="">{def.default === null || def.default === undefined ? 'Unset' : `Default (${String(def.default)})`}</option>
                    {(def.choices ?? []).map((c) => <option key={c} value={c}>{c}</option>)}
                  </select>
                ) : def.type === 'bool' ? (
                  <label className="mt-1 flex cursor-pointer items-center gap-1.5 text-xs text-text-primary">
                    <input
                      id={id} type="checkbox" className="accent-[rgb(var(--color-accent-rgb))]"
                      checked={boolOn} onChange={(e) => setValue(def.key, String(e.target.checked))} data-testid={id}
                    />
                    {boolOn ? 'On' : 'Off'}
                    <span className="text-text-muted">(default {def.default === null || def.default === undefined ? 'unset' : boolDefault(def) ? 'on' : 'off'})</span>
                  </label>
                ) : (
                  <input
                    id={id}
                    className={`${inputCls} mt-0.5 block w-full font-mono`}
                    type={def.type === 'int' || def.type === 'float' ? 'number' : 'text'}
                    min={def.min ?? undefined} max={def.max ?? undefined} step={def.type === 'int' ? 1 : 'any'}
                    placeholder={placeholderFor(def)}
                    value={text}
                    onChange={(e) => setValue(def.key, e.target.value)}
                    aria-invalid={err !== undefined}
                    data-testid={id}
                  />
                )}
                {def.help && <div className="mt-0.5 text-[10px] text-text-muted">{def.help}</div>}
                {err && <div className="text-[11px] text-bear" data-testid={t(`hp-error-${def.key}`)}>{err}</div>}
              </div>
            );
          })}
        </div>
      )}

      {showAdvanced && (
      <div className="space-y-1" data-testid={t('advanced')}>
        <label className="block text-[11px] text-text-secondary" htmlFor={t('adv-json')}>
          Advanced parameters (JSON)
          <span className="ml-2 text-text-muted" data-testid={t('adv-count')}>{advancedParse.ok ? `${merged.advancedKeys} key${merged.advancedKeys === 1 ? '' : 's'}` : 'invalid'}</span>
        </label>
        <textarea
          id={t('adv-json')} rows={3} spellCheck={false}
          className={`${inputCls} block w-full font-mono ${advancedError || collisions.length > 0 ? 'border-bear/50' : ''}`}
          placeholder={'{"n_jobs": 2, "ccp_alpha": 0.001}'}
          value={advanced} onChange={(e) => onAdvanced(e.target.value)}
          aria-invalid={Boolean(advancedError) || collisions.length > 0}
          data-testid={t('adv-json')}
        />
        <div className="text-[10px] text-text-muted">
          A flat JSON object of extra parameters, merged with the fields above. The trainer checks each key against the algorithm and rejects ones it does not accept.
        </div>
        {advancedError && <div className="text-[11px] text-bear" data-testid={t('adv-error')}>{advancedError}</div>}
        {!advancedError && collisions.length > 0 && (
          <div className="text-[11px] text-bear" data-testid={t('adv-collision')}>
            {collisions.map((k) => `"${k}" is set both in its field above and in the JSON. Clear one of them.`).join(' ')}
          </div>
        )}
      </div>
      )}
    </div>
  );
};