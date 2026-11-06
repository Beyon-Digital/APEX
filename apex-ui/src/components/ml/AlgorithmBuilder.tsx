import React, { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { useMLStore, isBuilderDirty, RECIPE_ALGORITHM_ID } from '../../stores/mlStore';
import { ledgerVersion, saveRecipe, validateRecipe } from '../../lib/tauri';
import { DeletedObjects } from '../audit/DeletedObjects';
import { VersionHistory } from '../audit/VersionHistory';
import type { BuildingBlocksDto, RecipeCheckDto, RecipeDto, VersionDto } from '../../lib/types';
import { btnCls, CardIssues, inputCls, MemberEditor } from './MemberEditor';
import { RecipeDiagram } from './RecipeDiagram';
import {
  CALIBRATION_HELP, CALIBRATION_NOTE_HARD, CARD_CALIBRATION, CARD_FINAL, CARD_GENERAL, CARD_IDENTITY, CARD_MODEL, CARD_STEPS, DEFAULT_LIMITS,
  MAX_DESCRIPTION_LENGTH, MAX_LABEL_LENGTH, NAME_RE, STACKING_NOTE, STEP_GROUPS, TEMPLATES, blankRecipe, cardForPath, editSnapshot, fromRecipe,
  groupByCard, localIssues, memberCard, mergeIssues, newMember, newStep, nodesFromRecipe, slugifyName, stepCard, toRecipe, uniqueSlug,
  type Calibrate, type EditMember, type EditRecipe, type EditStep, type ModelKind, type RecipeContext,
} from './recipeModel';
import { StepEditor } from './StepEditor';

const primaryBtnCls =
  'px-3 py-1.5 text-xs font-mono font-semibold bg-accent text-on-accent hover:opacity-90 disabled:opacity-50 disabled:cursor-not-allowed';
const cardCls = (state: 'ok' | 'error') =>
  `border p-3 space-y-2 bg-surface-1 focus:outline-none focus:border-accent ${state === 'error' ? 'border-bear/50' : 'border-[var(--border-color)]'}`;
const headCls = 'text-[11px] font-semibold uppercase tracking-wider text-text-secondary';
const DEBOUNCE_MS = 350;

const errorText = (err: unknown) => (err instanceof Error ? err.message : typeof err === 'string' ? err : 'Something went wrong');

const Arrow: React.FC = () => <div className="select-none pl-4 font-mono text-xs text-text-muted" aria-hidden="true">↓</div>;

/**
 * The no-code algorithm builder: preprocessing steps, one model (single, voting or stacking) and
 * calibration, saved as a named recipe. The draft lives in `mlStore`, so it survives switching tabs.
 */
export const AlgorithmBuilder: React.FC = () => {
  const builder = useMLStore((s) => s.builder);
  const blocks = useMLStore((s) => s.blocks);
  const blocksStatus = useMLStore((s) => s.blocksStatus);
  const blocksError = useMLStore((s) => s.blocksError);
  const algorithms = useMLStore((s) => s.algorithms);
  const recipes = useMLStore((s) => s.recipes);
  const loadBlocks = useMLStore((s) => s.loadBlocks);
  const loadAlgorithms = useMLStore((s) => s.loadAlgorithms);
  const refreshRecipes = useMLStore((s) => s.refreshRecipes);
  const initBuilder = useMLStore((s) => s.initBuilder);
  const closeBuilder = useMLStore((s) => s.closeBuilder);
  const [algorithmsReady, setAlgorithmsReady] = useState(false);

  useEffect(() => {
    // Ask again on every open: which algorithms are installed can change between visits.
    void loadBlocks(true);
    void loadAlgorithms().finally(() => setAlgorithmsReady(true));
    void refreshRecipes();
  }, [loadBlocks, loadAlgorithms, refreshRecipes]);

  // Convert the recipe being opened into editable form once the parameter definitions are known.
  const sourceKey = builder ? builder.epoch : null;
  useEffect(() => {
    if (!builder || builder.edit || !blocks || blocksStatus !== 'ready' || !algorithmsReady) return;
    const ctx: RecipeContext = { blocks, algorithms };
    const isNew = builder.savedName === null;
    const dto = builder.source ?? blankRecipe(blocks);
    const edit = fromRecipe(dto, ctx, !isNew || dto.name !== '');
    if (isNew && builder.source) edit.name = uniqueSlug(edit.name, recipes.map((r) => r.name));
    const baseline = isNew ? editSnapshot(fromRecipe(blankRecipe(blocks), ctx, false)) : editSnapshot(edit);
    initBuilder(edit, baseline);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [sourceKey, blocks, blocksStatus, algorithmsReady]);

  if (!builder) return null;

  if (blocksStatus === 'error') {
    return (
      <section className={cardCls('error')} data-testid="ml-recipe-builder">
        <div className={headCls}>Algorithm builder</div>
        <div className="text-xs text-bear" data-testid="rb-blocks-error">Could not load the building blocks: {blocksError}</div>
        <div className="flex gap-2">
          <button type="button" className={btnCls} onClick={() => void loadBlocks(true)} data-testid="rb-blocks-retry">Try again</button>
          <button type="button" className={btnCls} onClick={closeBuilder} data-testid="rb-discard">Close</button>
        </div>
      </section>
    );
  }
  if (!builder.edit || !blocks || blocksStatus !== 'ready') {
    return (
      <section className={cardCls('ok')} data-testid="ml-recipe-builder">
        <div className={headCls}>Algorithm builder</div>
        <div className="animate-pulse text-xs text-text-muted" data-testid="rb-loading" role="status">Loading building blocks...</div>
      </section>
    );
  }
  return <BuilderBody key={builder.epoch} builder={builder} blocks={blocks} algorithms={algorithms} />;
};

interface BodyProps {
  /** A builder whose `edit` is set. */
  builder: NonNullable<ReturnType<typeof useMLStore.getState>['builder']>;
  blocks: BuildingBlocksDto;
  algorithms: RecipeContext['algorithms'];
}

const BuilderBody: React.FC<BodyProps> = ({ builder, blocks, algorithms }) => {
  const edit = builder.edit!;
  const recipes = useMLStore((s) => s.recipes);
  const editBuilder = useMLStore((s) => s.editBuilder);
  const openBuilder = useMLStore((s) => s.openBuilder);
  const markSaved = useMLStore((s) => s.markBuilderSaved);
  const closeBuilder = useMLStore((s) => s.closeBuilder);
  const refreshRecipes = useMLStore((s) => s.refreshRecipes);
  const removeRecipe = useMLStore((s) => s.removeRecipe);
  const patchSettings = useMLStore((s) => s.patchSettings);

  const ctx = useMemo<RecipeContext>(() => ({ blocks, algorithms }), [blocks, algorithms]);
  const limits = blocks.limits ?? DEFAULT_LIMITS;
  const dirty = isBuilderDirty(builder);
  const savedName = builder.savedName;
  const rootRef = useRef<HTMLElement>(null);

  const takenNames = useMemo(() => recipes.filter((r) => r.name !== savedName).map((r) => r.name), [recipes, savedName]);
  const update = useCallback((fn: (e: EditRecipe) => EditRecipe) => editBuilder(fn), [editBuilder]);

  /* ----- the live Check ----- */
  const recipe = useMemo(() => toRecipe(edit, ctx), [edit, ctx]);
  const recipeKey = useMemo(() => JSON.stringify(recipe), [recipe]);
  const [check, setCheck] = useState<RecipeCheckDto | null>(null);
  const [checkError, setCheckError] = useState<string | null>(null);
  const [checkedKey, setCheckedKey] = useState<string | null>(null);
  useEffect(() => {
    let live = true;
    const timer = setTimeout(async () => {
      try {
        const result = await validateRecipe(JSON.parse(recipeKey) as RecipeDto);
        if (!live) return;
        setCheck(result); setCheckError(null);
      } catch (err) {
        if (!live) return;
        // The previous result belongs to an older recipe: showing its errors and size would contradict "saving will check again".
        setCheck(null);
        setCheckError(errorText(err));
      }
      setCheckedKey(recipeKey);
    }, DEBOUNCE_MS);
    return () => { live = false; clearTimeout(timer); };
  }, [recipeKey]);
  const checking = checkedKey !== recipeKey;

  const local = useMemo(() => localIssues(edit, ctx, { takenNames }), [edit, ctx, takenNames]);
  const issues = useMemo(() => mergeIssues(local, !checking && check ? check.errors : []), [local, check, checking]);
  const byCard = useMemo(() => groupByCard(issues), [issues]);
  const issuesFor = (key: string) => byCard[key] ?? [];
  const hasErrors = issues.length > 0;
  const settled = !checking;

  /* ----- focus after a reorder ----- */
  const focusAfter = useRef<{ id: string; button?: 'up' | 'down' } | null>(null);
  useEffect(() => {
    const target = focusAfter.current;
    if (!target) return;
    focusAfter.current = null;
    const card = rootRef.current?.querySelector<HTMLElement>(`[data-card-id="${target.id}"]`);
    if (!card) return;
    const button = target.button ? card.querySelector<HTMLElement>(`[data-testid^="rb-step-${target.button}-"]:not([disabled])`) : null;
    (button ?? card).focus();
  });

  /* ----- editing ----- */
  const setLabel = (label: string) => update((e) => {
    const slug = slugifyName(label);
    return { ...e, label, name: e.nameTouched ? e.name : slug ? uniqueSlug(slug, takenNames) : '' };
  });
  const setStep = (i: number, step: EditStep) => update((e) => ({ ...e, steps: e.steps.map((s, j) => (j === i ? step : s)) }));
  const moveStep = (i: number, delta: -1 | 1, button?: 'up' | 'down') => {
    const j = i + delta;
    if (j < 0 || j >= edit.steps.length) return;
    focusAfter.current = { id: edit.steps[i].id, button };
    update((e) => {
      const steps = [...e.steps];
      [steps[i], steps[j]] = [steps[j], steps[i]];
      return { ...e, steps };
    });
  };
  const [menuOpen, setMenuOpen] = useState(false);
  const addStep = (kind: string) => {
    const step = newStep(kind);
    focusAfter.current = { id: step.id };
    update((e) => ({ ...e, steps: [...e.steps, step] }));
    setMenuOpen(false);
  };
  const setMember = (i: number, m: EditMember) => update((e) => ({ ...e, members: e.members.map((x, j) => (j === i ? m : x)) }));
  const addMember = () => update((e) => (e.members.length >= limits.max_members ? e : { ...e, members: [...e.members, newMember(blocks, e.members.map((m) => m.algorithm))] }));
  const removeMember = (i: number) => update((e) => (e.members.length <= limits.min_members ? e : { ...e, members: e.members.filter((_, j) => j !== i) }));
  const setKind = (kind: ModelKind) => update((e) => ({ ...e, kind }));

  /* ----- actions ----- */
  const [busy, setBusy] = useState<null | 'save' | 'copy' | 'delete' | 'train'>(null);
  const [message, setMessage] = useState<{ ok: boolean; text: string } | null>(null);
  const [confirm, setConfirm] = useState<null | 'delete' | 'discard' | 'copy' | { template: string }>(null);
  const [copyName, setCopyName] = useState('');
  const [copyError, setCopyError] = useState<string | null>(null);

  const persist = async (dto: RecipeDto, source: EditRecipe): Promise<string | null> => {
    try {
      const saved = await saveRecipe(dto);
      const name = saved?.name || dto.name;
      markSaved(source, name);
      await refreshRecipes();
      return name;
    } catch (err) {
      setMessage({ ok: false, text: errorText(err) });
      return null;
    }
  };

  const save = async () => {
    setBusy('save'); setMessage(null);
    const name = await persist(recipe, edit);
    if (name) setMessage({ ok: true, text: `Saved "${name}".` });
    setBusy(null);
  };

  const startCopy = () => {
    setMessage(null); setCopyError(null);
    setCopyName(uniqueSlug(`${edit.name || 'algorithm'}_copy`.slice(0, 36), recipes.map((r) => r.name)));
    setConfirm('copy');
  };
  const doCopy = async () => {
    const name = copyName.trim();
    if (!NAME_RE.test(name)) { setCopyError('Use lowercase letters, digits and underscores, starting with a letter (up to 41 characters).'); return; }
    if (recipes.some((r) => r.name === name)) { setCopyError(`A saved algorithm called "${name}" already exists. Pick another name.`); return; }
    setBusy('copy'); setCopyError(null); setMessage(null);
    const label = /\(copy\)$/.test(edit.label) ? edit.label : `${edit.label.trim()} (copy)`.slice(-MAX_LABEL_LENGTH);
    const copy: EditRecipe = { ...edit, name, label, nameTouched: true };
    const saved = await persist({ ...recipe, name, label: label.trim() }, copy);
    if (saved) {
      update(() => copy);
      setConfirm(null);
      setMessage({ ok: true, text: `Saved a copy as "${saved}".` });
    } else {
      setCopyError('The copy could not be saved. See the message below.');
    }
    setBusy(null);
  };

  const doDelete = async () => {
    if (!savedName) return;
    setBusy('delete'); setMessage(null);
    const error = await removeRecipe(savedName);
    if (error) { setMessage({ ok: false, text: error }); setConfirm(null); } else closeBuilder();
    setBusy(null);
  };

  // A restore added a new version of a recipe: reload the list and show that version in the builder.
  const restoredRecipe = async (record: VersionDto) => {
    try {
      const v = await ledgerVersion('recipe', record.object_id, record.version);
      await refreshRecipes();
      if (v.text !== null) { openBuilder(JSON.parse(v.text) as RecipeDto, record.object_id); setMessage({ ok: true, text: `Restored "${record.object_id}" as version ${record.version}.` }); }
    } catch (err) {
      setMessage({ ok: false, text: errorText(err) });
    }
  };

  const needsSave = dirty || savedName === null;
  const trainWith = async () => {
    setBusy('train'); setMessage(null);
    let name = savedName;
    if (needsSave) name = await persist(recipe, edit);
    if (name) {
      patchSettings({ algorithm: RECIPE_ALGORITHM_ID, recipe: name });
      closeBuilder();
    }
    setBusy(null);
  };

  const discard = () => { if (dirty) setConfirm('discard'); else closeBuilder(); };

  const applyTemplate = (id: string) => {
    const t = TEMPLATES.find((x) => x.id === id);
    if (t) openBuilder(t.recipe, null);
  };
  const pickTemplate = (id: string) => { if (dirty) setConfirm({ template: id }); else applyTemplate(id); };

  /* ----- the overview diagram ----- */
  const modelHasIssue = Object.keys(byCard).some((k) => k === CARD_MODEL || k === CARD_FINAL || k.startsWith('member-'));
  const nodes = useMemo(() => nodesFromRecipe(recipe, blocks, algorithms).map((n) => {
    const bad = n.id === 'model' ? modelHasIssue : Boolean(byCard[n.id]);
    return bad ? { ...n, state: 'error' as const } : n;
  }), [recipe, blocks, algorithms, byCard, modelHasIssue]);
  const jumpTo = (nodeId: string) => {
    const el = rootRef.current?.querySelector<HTMLElement>(`[data-card-key="${nodeId}"]`);
    if (!el) return;
    el.scrollIntoView({ block: 'nearest', behavior: 'smooth' });
    el.focus();
  };

  const busyAny = busy !== null;
  const canSave = settled && !hasErrors && !busyAny;
  const calibrationIgnored = edit.kind === 'voting' && edit.voting === 'hard' && edit.calibrate !== 'none';
  const stepGroups = [
    ...STEP_GROUPS.map((g) => ({ ...g, kinds: blocks.steps.filter((s) => s.group === g.id) })),
    { id: 'other', label: 'Other', kinds: blocks.steps.filter((s) => !STEP_GROUPS.some((g) => g.id === s.group)) },
  ].filter((g) => g.kinds.length > 0);
  const stepsFull = edit.steps.length >= limits.max_steps;

  return (
    <section ref={rootRef} className="space-y-3" data-testid="ml-recipe-builder" aria-label="Algorithm builder">
      <div className="flex flex-wrap items-center gap-2">
        <h2 className="text-sm font-semibold text-text-primary">Algorithm builder</h2>
        <span className="font-mono text-[11px] text-text-muted" data-testid="rb-mode">{savedName ? `Editing ${savedName}` : 'New algorithm'}</span>
        {dirty && <span className="border border-warning/50 px-1 text-[10px] font-mono uppercase text-warning" data-testid="rb-dirty">Unsaved changes</span>}
      </div>
      <p className="text-[11px] text-text-muted">
        Put an algorithm together from parts: optional preprocessing steps, then one model or a team of models, then optional probability calibration. Nothing here runs code, so training with it needs no approval.
      </p>

      {savedName === null && (
        <div className="space-y-1 border border-[var(--border-color)] bg-surface-1 p-3" data-testid="rb-templates">
          <div className={headCls}>Start from an example</div>
          <div className="flex flex-wrap gap-2">
            {TEMPLATES.map((t) => (
              <button key={t.id} type="button" className={`${btnCls} text-left`} title={t.blurb} onClick={() => pickTemplate(t.id)} data-testid={`rb-template-${t.id}`}>{t.title}</button>
            ))}
          </div>
          <div className="text-[11px] text-text-muted">Examples only: every part stays editable.</div>
          {typeof confirm === 'object' && confirm !== null && (
            <div className="flex flex-wrap items-center gap-2 border border-warning/40 bg-warning/10 px-2 py-1 text-[11px] text-warning" data-testid="rb-template-confirm-bar" role="alert">
              Replace your unsaved changes with this example?
              <button type="button" className={btnCls} onClick={() => applyTemplate(confirm.template)} data-testid="rb-template-confirm">Replace</button>
              <button type="button" className={btnCls} onClick={() => setConfirm(null)} data-testid="rb-template-keep">Keep mine</button>
            </div>
          )}
        </div>
      )}

      <div className={cardCls(issuesFor(CARD_IDENTITY).length ? 'error' : 'ok')} data-card-key={CARD_IDENTITY} tabIndex={-1} data-testid="rb-card-identity" data-state={issuesFor(CARD_IDENTITY).length ? 'error' : 'ok'}>
        <div className={headCls}>Name</div>
        <div className="grid grid-cols-1 gap-2 md:grid-cols-2">
          <label className="text-[11px] text-text-secondary">Label
            <input className={`${inputCls} mt-0.5 block w-full`} value={edit.label} maxLength={MAX_LABEL_LENGTH} onChange={(e) => setLabel(e.target.value)} placeholder="My algorithm" data-testid="rb-label" />
          </label>
          <label className="text-[11px] text-text-secondary">Name (used in files and requests)
            <input
              className={`${inputCls} mt-0.5 block w-full font-mono`} value={edit.name} spellCheck={false} placeholder="my_algorithm"
              onChange={(e) => update((x) => ({ ...x, name: e.target.value, nameTouched: true }))} data-testid="rb-name" aria-invalid={issuesFor(CARD_IDENTITY).some((i) => i.path === 'name')}
            />
          </label>
          <label className="text-[11px] text-text-secondary md:col-span-2">Description (optional)
            <textarea
              className={`${inputCls} mt-0.5 block w-full`} rows={2} value={edit.description} maxLength={MAX_DESCRIPTION_LENGTH}
              onChange={(e) => update((x) => ({ ...x, description: e.target.value }))} data-testid="rb-description"
            />
          </label>
        </div>
        {savedName !== null && edit.name.trim() !== savedName && (
          <div className="text-[11px] text-text-muted" data-testid="rb-rename-note">Saving under a new name creates a new algorithm. &quot;{savedName}&quot; stays as it is.</div>
        )}
        <CardIssues cardKey={CARD_IDENTITY} issues={issuesFor(CARD_IDENTITY)} />
      </div>

      <div className="space-y-1">
        <div className={headCls}>Pipeline</div>
        <RecipeDiagram nodes={nodes} onSelect={jumpTo} testIdPrefix="rb-diagram" />
      </div>

      <div className="space-y-1" data-testid="rb-flow">
        <div className={cardCls('ok')} data-card-key="data" tabIndex={-1} data-testid="rb-card-data">
          <div className={headCls}>Data</div>
          <div className="text-[11px] text-text-muted">
            The features you pick in the Data section of this tab. Every step below is fitted on training rows only, so test rows never shape it.
          </div>
        </div>
        <Arrow />

        <div className="space-y-2" data-testid="rb-steps" data-card-key={CARD_STEPS} tabIndex={-1}>
          <div className="flex flex-wrap items-center gap-2">
            <span className={headCls}>Steps</span>
            <span className="text-[11px] text-text-muted" data-testid="rb-step-count">{edit.steps.length} of {limits.max_steps}</span>
          </div>
          <CardIssues cardKey={CARD_STEPS} issues={issuesFor(CARD_STEPS)} />
          {edit.steps.length === 0 && <div className="text-[11px] text-text-muted" data-testid="rb-no-steps">No steps. The features go to the model as they are.</div>}
          {edit.steps.map((step, i) => (
            <React.Fragment key={step.id}>
              <StepEditor
                index={i} count={edit.steps.length} step={step} kind={blocks.steps.find((k) => k.kind === step.kind)}
                issues={issuesFor(stepCard(i))} onChange={(s) => setStep(i, s)}
                onMove={(delta, viaKey) => moveStep(i, delta, viaKey ? undefined : delta < 0 ? 'up' : 'down')}
                onRemove={() => update((e) => ({ ...e, steps: e.steps.filter((_, j) => j !== i) }))}
              />
              <Arrow />
            </React.Fragment>
          ))}
          <div>
            <button type="button" className={btnCls} onClick={() => setMenuOpen((o) => !o)} disabled={stepsFull} aria-expanded={menuOpen} data-testid="rb-add-step">
              {menuOpen ? 'Close menu' : 'Add a step'}
            </button>
            {stepsFull && <span className="ml-2 text-[11px] text-text-muted" data-testid="rb-steps-full">The limit of {limits.max_steps} steps is reached.</span>}
            {menuOpen && !stepsFull && (
              <div className="mt-1 space-y-2 border border-[var(--border-color)] bg-surface-0 p-2" data-testid="rb-add-step-menu">
                {stepGroups.map((g) => (
                  <div key={g.id} data-testid={`rb-add-group-${g.id}`}>
                    <div className="text-[10px] font-semibold uppercase tracking-wider text-text-muted">{g.label}</div>
                    <div className="mt-0.5 flex flex-wrap gap-1">
                      {g.kinds.map((k) => (
                        <button key={k.kind} type="button" className={btnCls} title={k.description} onClick={() => addStep(k.kind)} data-testid={`rb-add-step-${k.kind}`}>{k.label}</button>
                      ))}
                    </div>
                  </div>
                ))}
              </div>
            )}
          </div>
        </div>

        <div className={cardCls(modelHasIssue ? 'error' : 'ok')} data-card-key={CARD_MODEL} tabIndex={-1} data-testid="rb-card-model" data-state={modelHasIssue ? 'error' : 'ok'}>
          <div className={headCls}>Model</div>
          <fieldset className="flex flex-wrap gap-3" data-testid="rb-model-kind">
            <legend className="sr-only">Model type</legend>
            {([['single', 'Single model'], ['voting', 'Voting'], ['stacking', 'Stacking']] as const).map(([id, label]) => (
              <label key={id} className="flex cursor-pointer items-center gap-1 text-xs text-text-primary">
                <input type="radio" name="rb-model-kind" className="accent-[rgb(var(--color-accent-rgb))]" checked={edit.kind === id} onChange={() => setKind(id)} data-testid={`rb-model-kind-${id}`} />
                {label}
              </label>
            ))}
          </fieldset>

          {edit.kind === 'single' && (
            <MemberEditor
              prefix="rb-single" cardKey={CARD_MODEL} title="Model" member={edit.single} blocks={blocks} algorithms={algorithms} bare issues={[]}
              onChange={(m) => update((e) => ({ ...e, single: m }))}
            />
          )}

          {edit.kind !== 'single' && (
            <div className="space-y-2">
              {edit.kind === 'voting' ? (
                <>
                  <fieldset className="flex flex-wrap items-center gap-3" data-testid="rb-voting">
                    <legend className="sr-only">Voting type</legend>
                    {([['soft', 'Soft (average probabilities)'], ['hard', 'Hard (majority of class votes)']] as const).map(([id, label]) => (
                      <label key={id} className="flex cursor-pointer items-center gap-1 text-xs text-text-primary">
                        <input type="radio" name="rb-voting" className="accent-[rgb(var(--color-accent-rgb))]" checked={edit.voting === id} onChange={() => update((e) => ({ ...e, voting: id }))} data-testid={`rb-voting-${id}`} />
                        {label}
                      </label>
                    ))}
                  </fieldset>
                  <div className="text-[11px] text-text-muted">Each model gets a vote. A higher weight counts more.</div>
                </>
              ) : (
                <>
                  <div className="text-[11px] text-text-muted" data-testid="rb-stacking-note">{STACKING_NOTE}</div>
                  <label className="flex items-center gap-2 text-[11px] text-text-secondary">
                    Time blocks ({limits.stacking_blocks[0]} to {limits.stacking_blocks[1]})
                    <input
                      className={`${inputCls} w-20 font-mono`} type="number" min={limits.stacking_blocks[0]} max={limits.stacking_blocks[1]} step={1}
                      value={edit.blocks} onChange={(e) => update((x) => ({ ...x, blocks: e.target.value }))} data-testid="rb-blocks"
                    />
                  </label>
                </>
              )}
              <div className="flex items-center gap-2">
                <span className="text-[11px] text-text-secondary" data-testid="rb-member-count">Models: {edit.members.length} of {limits.max_members} (at least {limits.min_members})</span>
                <button type="button" className={btnCls} onClick={addMember} disabled={edit.members.length >= limits.max_members} data-testid="rb-add-member">Add a model</button>
              </div>
              {edit.members.map((m, i) => (
                <MemberEditor
                  key={m.id} prefix={`rb-m${i}`} cardKey={memberCard(i)} title={`Model ${i + 1}`} member={m} blocks={blocks} algorithms={algorithms}
                  showWeight={edit.kind === 'voting'} issues={issuesFor(memberCard(i))} onChange={(next) => setMember(i, next)}
                  onRemove={edit.members.length > limits.min_members ? () => removeMember(i) : undefined}
                />
              ))}
              {edit.kind === 'stacking' && (
                <MemberEditor
                  prefix="rb-final" cardKey={CARD_FINAL} title="Final estimator (learns how to combine the models)" member={edit.final} blocks={blocks} algorithms={algorithms}
                  issues={issuesFor(CARD_FINAL)} onChange={(m) => update((e) => ({ ...e, final: m }))}
                />
              )}
            </div>
          )}
          <CardIssues cardKey={CARD_MODEL} issues={issuesFor(CARD_MODEL)} />
        </div>
        <Arrow />

        <div className={cardCls(issuesFor(CARD_CALIBRATION).length ? 'error' : 'ok')} data-card-key={CARD_CALIBRATION} tabIndex={-1} data-testid="rb-card-calibration" data-state={issuesFor(CARD_CALIBRATION).length ? 'error' : 'ok'}>
          <div className={headCls}>Calibration</div>
          <select className={`${inputCls} block w-56`} value={edit.calibrate} onChange={(e) => update((x) => ({ ...x, calibrate: e.target.value as Calibrate }))} data-testid="rb-calibrate" aria-label="Calibration">
            {(Object.keys(CALIBRATION_HELP) as Calibrate[]).map((c) => <option key={c} value={c}>{CALIBRATION_HELP[c].label}</option>)}
          </select>
          <div className="text-[11px] text-text-muted" data-testid="rb-calibrate-help">{CALIBRATION_HELP[edit.calibrate].text}</div>
          {calibrationIgnored && <div className="border border-warning/40 bg-warning/10 px-2 py-1 text-[11px] text-warning" data-testid="rb-calibrate-ignored">{CALIBRATION_NOTE_HARD}</div>}
          <CardIssues cardKey={CARD_CALIBRATION} issues={issuesFor(CARD_CALIBRATION)} />
        </div>
        <Arrow />

        <div className={cardCls('ok')} data-card-key="output" tabIndex={-1} data-testid="rb-card-output">
          <div className={headCls}>Predictions</div>
          <div className="text-[11px] text-text-muted">Each bar gets a probability and a buy, sell or hold signal. Training scores them with the same time-ordered cross-validation as any other algorithm.</div>
        </div>
      </div>

      <div className={cardCls(hasErrors ? 'error' : 'ok')} data-testid="rb-check" data-state={checking ? 'checking' : hasErrors ? 'error' : 'ok'} aria-live="polite">
        <div className="flex flex-wrap items-center gap-3">
          <span className={headCls}>Check</span>
          <span
            className={`text-xs ${checking ? 'text-text-muted' : hasErrors ? 'text-bear' : 'text-bull'}`} data-testid="rb-check-status"
          >
            {checking ? 'Checking...' : hasErrors ? `${issues.length} problem${issues.length === 1 ? '' : 's'} to fix` : 'No problems found'}
          </span>
          <span className="font-mono text-[11px] text-text-muted" data-testid="rb-check-counts">
            {check ? `${check.steps} step${check.steps === 1 ? '' : 's'}, ${check.models} model${check.models === 1 ? '' : 's'}` : `${edit.steps.length} steps`}
          </span>
          <span className="font-mono text-[11px] text-text-muted" data-testid="rb-check-size">
            {check && check.est_model_mb !== null ? `about ${check.est_model_mb} MB when trained` : 'size unknown'}
          </span>
        </div>
        {checkError && <div className="text-[11px] text-warning" data-testid="rb-check-error">The backend check did not run ({checkError}). Saving will check again.</div>}
        {!checking && check && check.warnings.length > 0 && (
          <ul className="list-disc space-y-0.5 border border-warning/40 bg-warning/10 py-1 pl-6 pr-2 text-[11px] text-warning" data-testid="rb-check-warnings">
            {check.warnings.map((w) => <li key={w}>{w}</li>)}
          </ul>
        )}
        {issues.length > 0 && !checking && (
          <ul className="list-disc space-y-0.5 pl-5 text-[11px] text-bear" data-testid="rb-check-errors">
            {issues.map((issue, i) => (
              <li key={`${issue.path}-${i}`}>
                <button type="button" className="text-left hover:underline" onClick={() => jumpTo(cardForPath(issue.path))} data-testid={`rb-check-error-${i}`}>
                  {issue.message}{issue.path ? ` (${issue.path})` : ''}
                </button>
              </li>
            ))}
          </ul>
        )}
        <CardIssues cardKey={CARD_GENERAL} issues={issuesFor(CARD_GENERAL)} />
      </div>

      {message && (
        <div className={`border px-2 py-1 text-xs ${message.ok ? 'border-bull/40 bg-bull/10 text-bull' : 'border-bear/40 bg-bear/10 text-bear'}`} data-testid={message.ok ? 'rb-info' : 'rb-error'} role={message.ok ? 'status' : 'alert'}>
          {message.text}
        </div>
      )}

      <div className="flex flex-wrap items-center gap-2" data-testid="rb-actions">
        <button type="button" className={primaryBtnCls} onClick={() => void save()} disabled={!canSave} data-testid="rb-save">{busy === 'save' ? 'Saving...' : 'Save'}</button>
        {savedName !== null && (
          <button type="button" className={btnCls} onClick={startCopy} disabled={busyAny || issues.some((i) => i.path !== 'name')} data-testid="rb-save-copy">Save as copy</button>
        )}
        <button type="button" className={btnCls} onClick={() => void trainWith()} disabled={busyAny || (needsSave ? !canSave : false)} data-testid="rb-train-with">Train with this</button>
        {savedName !== null && (
          <button type="button" className={`${btnCls} text-bear`} onClick={() => setConfirm('delete')} disabled={busyAny} data-testid="rb-delete" title="The algorithm leaves the list; its history is kept and it can be restored">Delete (history is kept)</button>
        )}
        {savedName !== null && <VersionHistory kind="recipe" id={savedName} onRestored={(r) => void restoredRecipe(r)} className={btnCls} />}
        <button type="button" className={btnCls} onClick={discard} disabled={busy === 'save' || busy === 'train'} data-testid="rb-discard">{dirty ? 'Discard changes' : 'Close'}</button>
      </div>

      <DeletedObjects kind="recipe" onRestored={() => void refreshRecipes()} />

      {confirm === 'copy' && (
        <div className="space-y-1 border border-[var(--border-color)] bg-surface-1 p-2" data-testid="rb-copy-bar">
          <label className="block text-[11px] text-text-secondary">Name for the copy
            <input
              className={`${inputCls} mt-0.5 block w-64 font-mono`} value={copyName} spellCheck={false} autoFocus
              onChange={(e) => { setCopyName(e.target.value); setCopyError(null); }} onKeyDown={(e) => { if (e.key === 'Enter') void doCopy(); if (e.key === 'Escape') setConfirm(null); }}
              data-testid="rb-copy-name"
            />
          </label>
          <div className="flex gap-2">
            <button type="button" className={btnCls} onClick={() => void doCopy()} disabled={busy === 'copy'} data-testid="rb-copy-confirm">Save copy</button>
            <button type="button" className={btnCls} onClick={() => setConfirm(null)} data-testid="rb-copy-cancel">Cancel</button>
          </div>
          {copyError && <div className="text-[11px] text-bear" data-testid="rb-copy-error">{copyError}</div>}
        </div>
      )}
      {confirm === 'delete' && (
        <div className="flex flex-wrap items-center gap-2 border border-bear/40 bg-bear/10 px-2 py-1 text-xs text-bear" data-testid="rb-delete-bar" role="alertdialog" aria-label="Confirm delete">
          Delete &quot;{savedName}&quot;? Its history is kept and it can be restored. Models already trained with it keep working.
          <button type="button" className={btnCls} onClick={() => void doDelete()} disabled={busy === 'delete'} data-testid="rb-delete-confirm">Delete (history is kept)</button>
          <button type="button" className={btnCls} onClick={() => setConfirm(null)} data-testid="rb-delete-cancel">Cancel</button>
        </div>
      )}
      {confirm === 'discard' && (
        <div className="flex flex-wrap items-center gap-2 border border-warning/40 bg-warning/10 px-2 py-1 text-xs text-warning" data-testid="rb-discard-bar" role="alertdialog" aria-label="Confirm discard">
          Discard your unsaved changes?
          <button type="button" className={btnCls} onClick={closeBuilder} data-testid="rb-discard-confirm">Discard</button>
          <button type="button" className={btnCls} onClick={() => setConfirm(null)} data-testid="rb-discard-keep">Keep editing</button>
        </div>
      )}
    </section>
  );
};
