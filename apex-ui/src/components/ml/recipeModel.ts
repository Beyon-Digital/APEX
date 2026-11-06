/**
 * Pure helpers for the algorithm builder: the editable form of a recipe, conversion to and from the
 * saved `RecipeDto`, starter templates, local checks that mirror the backend's rules, and plain-words
 * summaries.
 *
 * The editable form keeps every parameter as the text the user typed (like the Train form does), so a
 * half-typed "0." is never lost. Only values that differ from the default are written to the recipe.
 * The backend's `validate_recipe` stays the authority; the local checks only give instant feedback and
 * decide which card an error belongs to.
 */
import type {
  AlgorithmDto, BuildingBlocksDto, ParamDefDto, RecipeDto, RecipeIssueDto, RecipeMemberDto, RecipeModelDto, RecipeStepDto, StepKindDto,
} from '../../lib/types';
import { mergeParams, validateParams, type HyperparamValues } from './hyperparams';

export type ModelKind = 'single' | 'voting' | 'stacking';
export type Calibrate = RecipeDto['calibrate'];

export const NAME_RE = /^[a-z][a-z0-9_]{0,40}$/;
export const MAX_NAME_LENGTH = 41;
export const MAX_LABEL_LENGTH = 80;
export const MAX_DESCRIPTION_LENGTH = 500;
export const DEFAULT_LIMITS: BuildingBlocksDto['limits'] = { max_steps: 8, max_members: 6, min_members: 2, stacking_blocks: [3, 8] };

export interface EditParams {
  /** Raw field text per generated parameter. */
  values: HyperparamValues;
  /** The "Advanced parameters" JSON text (members only). */
  advanced: string;
}

export interface EditStep extends EditParams {
  id: string;
  kind: string;
}

export interface EditMember extends EditParams {
  id: string;
  algorithm: string;
  /** Voting only. Text so a half-typed number survives. */
  weight: string;
}

export interface EditRecipe {
  name: string;
  /** True once the user typed in the name field, which stops it following the label. */
  nameTouched: boolean;
  label: string;
  description: string;
  steps: EditStep[];
  kind: ModelKind;
  /** The one model when kind is "single". */
  single: EditMember;
  voting: 'soft' | 'hard';
  /** Members for voting and stacking (they share the list, so switching between the two keeps it). */
  members: EditMember[];
  /** Final estimator for stacking. */
  final: EditMember;
  blocks: string;
  calibrate: Calibrate;
}

/** What conversion needs to know about the backend. */
export interface RecipeContext {
  blocks: BuildingBlocksDto;
  /** Output of listAlgorithms: the parameter definitions of every algorithm. */
  algorithms: AlgorithmDto[];
}

let idCounter = 0;
export function newEditId(prefix = 'e'): string {
  idCounter += 1;
  return `${prefix}${idCounter}`;
}

/* ---------- Names ---------- */

/** "My Voting Mix!" -> "my_voting_mix". Always satisfies the name rule, or is empty. */
export function slugifyName(label: string): string {
  let s = label.toLowerCase().replace(/[^a-z0-9]+/g, '_').replace(/^_+|_+$/g, '');
  if (s === '') return '';
  if (!/^[a-z]/.test(s)) s = `a_${s}`;
  return s.slice(0, MAX_NAME_LENGTH).replace(/_+$/, '');
}

/** `base`, or `base_2`, `base_3` ... when taken. Stays within the name length limit. */
export function uniqueSlug(base: string, taken: string[]): string {
  const used = new Set(taken);
  const root = base || 'algorithm';
  if (!used.has(root)) return root;
  for (let n = 2; ; n += 1) {
    const suffix = `_${n}`;
    const candidate = `${root.slice(0, MAX_NAME_LENGTH - suffix.length)}${suffix}`;
    if (!used.has(candidate)) return candidate;
  }
}

/** "standard_scaler" -> "Standard scaler" (used only when the backend gave no label). */
export function humanize(id: string): string {
  const s = id.replace(/_/g, ' ').trim();
  return s ? s.charAt(0).toUpperCase() + s.slice(1) : id;
}

/* ---------- Parameters <-> text ---------- */

function isScalar(v: unknown): v is string | number | boolean {
  return typeof v === 'string' || typeof v === 'number' || typeof v === 'boolean';
}

/**
 * Stored params back into form text. Keys the definitions list (and plain values) become field text;
 * everything else, including explicit nulls, goes to the JSON box so nothing is silently dropped.
 */
export function paramsToEdit(defs: ParamDefDto[], stored: Record<string, unknown> | undefined): EditParams {
  const known = new Set(defs.map((d) => d.key));
  const values: HyperparamValues = {};
  const extra: Record<string, unknown> = {};
  for (const [key, v] of Object.entries(stored ?? {})) {
    if (known.has(key) && isScalar(v)) values[key] = String(v);
    else extra[key] = v;
  }
  return { values, advanced: Object.keys(extra).length > 0 ? JSON.stringify(extra, null, 2) : '' };
}

/** Text back into stored params: only valid values that differ from the default, plus the JSON keys. */
export function editToParams(defs: ParamDefDto[], p: EditParams): Record<string, unknown> {
  return { ...mergeParams(defs, p.values, p.advanced).params };
}

const algoDefs = (algorithms: AlgorithmDto[], id: string): ParamDefDto[] => algorithms.find((a) => a.id === id)?.params ?? [];
const stepKind = (blocks: BuildingBlocksDto, kind: string): StepKindDto | undefined => blocks.steps.find((s) => s.kind === kind);
const stepDefs = (blocks: BuildingBlocksDto, kind: string): ParamDefDto[] => stepKind(blocks, kind)?.params ?? [];

/* ---------- Conversion ---------- */

const emptyMember = (algorithm: string, weight = '1'): EditMember => ({ id: newEditId('m'), algorithm, values: {}, advanced: '', weight });

/** The algorithm to preselect: the first of `preferred` the backend offers, else the first it offers. */
export function pickAlgorithm(blocks: BuildingBlocksDto | null, ...preferred: string[]): string {
  const ids = (blocks?.members ?? []).map((m) => m.id);
  return preferred.find((p) => ids.includes(p)) ?? ids[0] ?? preferred[0] ?? 'random_forest';
}

export function newMember(blocks: BuildingBlocksDto | null, usedAlgorithms: string[] = []): EditMember {
  const ids = (blocks?.members ?? []).map((m) => m.id);
  const fresh = ids.find((id) => !usedAlgorithms.includes(id));
  return emptyMember(fresh ?? pickAlgorithm(blocks, 'random_forest'));
}

export function newStep(kind: string): EditStep {
  return { id: newEditId('s'), kind, values: {}, advanced: '' };
}

export function blankRecipe(blocks: BuildingBlocksDto | null): RecipeDto {
  return {
    name: '', label: '', description: '', steps: [], calibrate: 'none',
    model: { kind: 'single', algorithm: pickAlgorithm(blocks, 'random_forest'), params: {} },
  };
}

function memberFrom(m: RecipeMemberDto, algorithms: AlgorithmDto[], withWeight: boolean): EditMember {
  return { id: newEditId('m'), algorithm: m.algorithm, ...paramsToEdit(algoDefs(algorithms, m.algorithm), m.params), weight: withWeight && m.weight !== undefined ? String(m.weight) : '1' };
}

/** Saved recipe to editable form. Unused parts (say, the members of a single model) get sensible defaults. */
export function fromRecipe(dto: RecipeDto, ctx: RecipeContext, nameTouched = true): EditRecipe {
  const { blocks, algorithms } = ctx;
  const model = dto.model;
  const defaultMembers = (): EditMember[] => [emptyMember(pickAlgorithm(blocks, 'random_forest')), emptyMember(pickAlgorithm(blocks, 'logistic_regression', 'extra_trees'))];
  const defaultFinal = (): EditMember => emptyMember(pickAlgorithm(blocks, 'logistic_regression'), '1');
  return {
    name: dto.name ?? '',
    nameTouched,
    label: dto.label ?? '',
    description: dto.description ?? '',
    steps: (dto.steps ?? []).map((s) => ({ id: newEditId('s'), kind: s.kind, ...paramsToEdit(stepDefs(blocks, s.kind), s.params) })),
    kind: model?.kind ?? 'single',
    single: model?.kind === 'single' ? memberFrom(model, algorithms, false) : emptyMember(pickAlgorithm(blocks, 'random_forest')),
    voting: model?.kind === 'voting' ? model.voting : 'soft',
    members: model && model.kind !== 'single' ? model.members.map((m) => memberFrom(m, algorithms, model.kind === 'voting')) : defaultMembers(),
    final: model?.kind === 'stacking' ? memberFrom(model.final, algorithms, false) : defaultFinal(),
    blocks: model?.kind === 'stacking' ? String(model.blocks) : '4',
    calibrate: dto.calibrate ?? 'none',
  };
}

const num = (text: string): number => Number(text.trim());

/** Editable form to the recipe sent to the backend. Invalid field text is left out (the checks report it). */
export function toRecipe(edit: EditRecipe, ctx: RecipeContext): RecipeDto {
  const { blocks, algorithms } = ctx;
  const memberParams = (m: EditMember) => editToParams(algoDefs(algorithms, m.algorithm), m);
  const weight = (m: EditMember): number => { const w = num(m.weight); return m.weight.trim() !== '' && Number.isFinite(w) ? w : 1; };
  let model: RecipeModelDto;
  if (edit.kind === 'single') {
    model = { kind: 'single', algorithm: edit.single.algorithm, params: memberParams(edit.single) };
  } else if (edit.kind === 'voting') {
    model = { kind: 'voting', voting: edit.voting, members: edit.members.map((m) => ({ algorithm: m.algorithm, params: memberParams(m), weight: weight(m) })) };
  } else {
    const blocksN = num(edit.blocks);
    model = {
      kind: 'stacking',
      members: edit.members.map((m) => ({ algorithm: m.algorithm, params: memberParams(m) })),
      final: { algorithm: edit.final.algorithm, params: memberParams(edit.final) },
      blocks: edit.blocks.trim() !== '' && Number.isFinite(blocksN) ? blocksN : 0,
    };
  }
  return {
    name: edit.name.trim(),
    label: edit.label.trim(),
    description: edit.description.trim(),
    steps: edit.steps.map((s): RecipeStepDto => ({ kind: s.kind, params: editToParams(stepDefs(blocks, s.kind), { values: s.values, advanced: '' }) })),
    model,
    calibrate: edit.calibrate,
  };
}

/** Everything the user typed, without internal ids. Equal snapshots mean "nothing to save". */
export function editSnapshot(edit: EditRecipe): string {
  const strip = <T extends { id: string }>({ id: _id, ...rest }: T) => rest;
  return JSON.stringify({
    name: edit.name, label: edit.label, description: edit.description, calibrate: edit.calibrate, kind: edit.kind, voting: edit.voting, blocks: edit.blocks,
    steps: edit.steps.map(strip), single: strip(edit.single), members: edit.members.map(strip), final: strip(edit.final),
  });
}

/* ---------- Starter templates ---------- */

export interface RecipeTemplate {
  id: string;
  title: string;
  blurb: string;
  recipe: RecipeDto;
}

export const TEMPLATES: RecipeTemplate[] = [
  {
    id: 'scaled_logistic',
    title: 'Scaled logistic regression',
    blurb: 'Scale the features, then fit a simple linear model. A solid, fast baseline.',
    recipe: {
      name: 'scaled_logistic', label: 'Scaled logistic regression', description: 'Standard scaling followed by logistic regression.',
      steps: [{ kind: 'standard_scaler', params: {} }],
      model: { kind: 'single', algorithm: 'logistic_regression', params: {} }, calibrate: 'none',
    },
  },
  {
    id: 'voting_mix',
    title: 'Voting: forest + boosting + logistic',
    blurb: 'Three different models vote; their probabilities are averaged.',
    recipe: {
      name: 'voting_mix', label: 'Voting: forest + boosting + logistic', description: 'Random forest, gradient boosting and logistic regression averaging their probabilities.',
      steps: [{ kind: 'standard_scaler', params: {} }],
      model: {
        kind: 'voting', voting: 'soft',
        members: [
          { algorithm: 'random_forest', params: {}, weight: 1 },
          { algorithm: 'gradient_boosting', params: {}, weight: 1 },
          { algorithm: 'logistic_regression', params: {}, weight: 1 },
        ],
      },
      calibrate: 'none',
    },
  },
  {
    id: 'stacked_trees',
    title: 'Stacked trees',
    blurb: 'Tree models make predictions on earlier data; a logistic model learns how to combine them.',
    recipe: {
      name: 'stacked_trees', label: 'Stacked trees', description: 'Random forest, extra trees and gradient boosting combined by a logistic regression, trained time-aware.',
      steps: [],
      model: {
        kind: 'stacking',
        members: [
          { algorithm: 'random_forest', params: {} },
          { algorithm: 'extra_trees', params: {} },
          { algorithm: 'gradient_boosting', params: {} },
        ],
        final: { algorithm: 'logistic_regression', params: {} }, blocks: 4,
      },
      calibrate: 'none',
    },
  },
  {
    id: 'pca_forest',
    title: 'PCA then forest',
    blurb: 'Scale, compress correlated features into a few components, then fit a random forest.',
    recipe: {
      name: 'pca_forest', label: 'PCA then forest', description: 'Standard scaling, PCA keeping 90% of the variance, then a random forest.',
      steps: [{ kind: 'standard_scaler', params: {} }, { kind: 'pca', params: { n_components: 0.9 } }],
      model: { kind: 'single', algorithm: 'random_forest', params: {} }, calibrate: 'none',
    },
  },
];

/* ---------- Checks and where an issue belongs ---------- */

export const CARD_IDENTITY = 'identity';
export const CARD_STEPS = 'steps';
export const CARD_MODEL = 'model';
export const CARD_FINAL = 'final';
export const CARD_CALIBRATION = 'calibration';
export const CARD_GENERAL = 'general';
export const stepCard = (i: number) => `step-${i}`;
export const memberCard = (i: number) => `member-${i}`;

/** The card an issue path points at, e.g. "steps[1].params.k" -> "step-1", "model.final.params.C" -> "final". */
export function cardForPath(path: string): string {
  if (path === 'name' || path === 'label' || path === 'description' || path.startsWith('name.') || path.startsWith('label.')) return CARD_IDENTITY;
  const step = /^steps\[(\d+)\]/.exec(path);
  if (step) return stepCard(Number(step[1]));
  if (path === 'steps' || path.startsWith('steps.')) return CARD_STEPS;
  const member = /^model\.members\[(\d+)\]/.exec(path);
  if (member) return memberCard(Number(member[1]));
  if (path.startsWith('model.final')) return CARD_FINAL;
  if (path === 'calibrate' || path.startsWith('calibrate.')) return CARD_CALIBRATION;
  if (path === 'model' || path.startsWith('model.')) return CARD_MODEL;
  return CARD_GENERAL;
}

export interface CheckOptions {
  /** Names of other saved recipes (the one being edited excluded). */
  takenNames: string[];
}

/** Instant checks that mirror the backend's rules. Returns errors only; the backend's Check adds warnings. */
export function localIssues(edit: EditRecipe, ctx: RecipeContext, opts: CheckOptions = { takenNames: [] }): RecipeIssueDto[] {
  const out: RecipeIssueDto[] = [];
  const bad = (path: string, message: string) => out.push({ path, message });
  const limits = ctx.blocks.limits ?? DEFAULT_LIMITS;
  const name = edit.name.trim();
  if (!name) bad('name', 'Give the algorithm a name.');
  else if (!NAME_RE.test(name)) bad('name', `Use lowercase letters, digits and underscores, starting with a letter (up to ${MAX_NAME_LENGTH} characters).`);
  else if (opts.takenNames.includes(name)) bad('name', `A saved algorithm called "${name}" already exists. Pick another name.`);
  if (!edit.label.trim()) bad('label', 'Give the algorithm a label.');

  if (edit.steps.length > limits.max_steps) bad('steps', `At most ${limits.max_steps} steps.`);
  edit.steps.forEach((s, i) => {
    const kind = stepKind(ctx.blocks, s.kind);
    if (!kind) { bad(`steps[${i}].kind`, `The step "${s.kind}" is not available here.`); return; }
    for (const [key, message] of Object.entries(validateParams(kind.params, s.values))) bad(`steps[${i}].params.${key}`, message);
  });

  const available = new Set(ctx.blocks.members.map((m) => m.id));
  const checkMember = (m: EditMember, path: string, weighted: boolean) => {
    if (!available.has(m.algorithm)) bad(`${path}.algorithm`, `The algorithm "${m.algorithm}" is not available here (for example, its package is not installed). Choose another.`);
    const merged = mergeParams(algoDefs(ctx.algorithms, m.algorithm), m.values, m.advanced);
    for (const [key, message] of Object.entries(validateParams(algoDefs(ctx.algorithms, m.algorithm), m.values))) bad(`${path}.params.${key}`, message);
    merged.errors.forEach((message) => bad(`${path}.params`, message));
    if (weighted) {
      const w = num(m.weight);
      if (m.weight.trim() === '' || !Number.isFinite(w) || w <= 0 || w > 100) bad(`${path}.weight`, 'Weight must be a number above 0 and at most 100.');
    }
  };
  if (edit.kind === 'single') {
    checkMember(edit.single, 'model', false);
  } else {
    const n = edit.members.length;
    if (n < limits.min_members || n > limits.max_members) bad('model.members', `Use ${limits.min_members} to ${limits.max_members} models.`);
    edit.members.forEach((m, i) => checkMember(m, `model.members[${i}]`, edit.kind === 'voting'));
    if (edit.kind === 'stacking') {
      checkMember(edit.final, 'model.final', false);
      const [lo, hi] = limits.stacking_blocks;
      const b = num(edit.blocks);
      if (edit.blocks.trim() === '' || !Number.isInteger(b) || b < lo || b > hi) bad('model.blocks', `Blocks must be a whole number from ${lo} to ${hi}.`);
    }
  }
  return out;
}

/** Local issues win over backend ones at the same path (the local message is usually plainer). */
export function mergeIssues(local: RecipeIssueDto[], remote: RecipeIssueDto[]): RecipeIssueDto[] {
  const paths = new Set(local.map((i) => i.path));
  return [...local, ...remote.filter((i) => !paths.has(i.path))];
}

export function groupByCard(issues: RecipeIssueDto[]): Record<string, RecipeIssueDto[]> {
  const out: Record<string, RecipeIssueDto[]> = {};
  for (const issue of issues) (out[cardForPath(issue.path)] ??= []).push(issue);
  return out;
}

/* ---------- Summaries and the diagram ---------- */

export const STEP_GROUPS: { id: string; label: string }[] = [
  { id: 'scale', label: 'Scale' },
  { id: 'clean', label: 'Clean' },
  { id: 'select', label: 'Select' },
  { id: 'reduce', label: 'Reduce' },
];

export const CALIBRATION_HELP: Record<Calibrate, { label: string; text: string }> = {
  none: { label: 'None', text: 'Use the model\'s probabilities as they are.' },
  sigmoid: { label: 'Sigmoid', text: 'Rescale probabilities with a smooth curve so "70% sure" comes closer to being right 70% of the time. Works well on small data.' },
  isotonic: { label: 'Isotonic', text: 'Rescale probabilities with a flexible step curve. More exact than sigmoid but needs plenty of rows.' },
};

export const STACKING_NOTE =
  'Stacking trains on earlier data and predicts later data, so it never peeks ahead. The training rows are cut into blocks in time order. Each block is predicted by models that only saw earlier blocks (with a gap for the label horizon), and the final model learns from those predictions. The first block is only ever used for learning, never as an example for the final model.';

export const CALIBRATION_NOTE_HARD = 'Hard voting has no probabilities, so calibration is ignored.';

function paramText(params: Record<string, unknown> | undefined): string {
  const entries = Object.entries(params ?? {});
  return entries.length === 0 ? '' : entries.map(([k, v]) => `${k} ${JSON.stringify(v)}`).join(', ');
}

const algoLabel = (blocks: BuildingBlocksDto | null, algorithms: AlgorithmDto[], id: string): string =>
  blocks?.members.find((m) => m.id === id)?.label ?? algorithms.find((a) => a.id === id)?.label ?? humanize(id);

export function stepLabel(blocks: BuildingBlocksDto | null, kind: string): string {
  return stepKind(blocks ?? { steps: [], members: [], limits: DEFAULT_LIMITS }, kind)?.label ?? humanize(kind);
}

export interface RecipeSummaryText {
  steps: string[];
  model: string;
  members: string[];
  calibration: string;
}

export function summarizeRecipe(dto: RecipeDto, blocks: BuildingBlocksDto | null, algorithms: AlgorithmDto[]): RecipeSummaryText {
  const memberText = (m: RecipeMemberDto, weighted: boolean) => {
    const p = paramText(m.params);
    const w = weighted && m.weight !== undefined && m.weight !== 1 ? ` x${m.weight}` : '';
    return `${algoLabel(blocks, algorithms, m.algorithm)}${w}${p ? ` (${p})` : ''}`;
  };
  const steps = (dto.steps ?? []).map((s) => {
    const p = paramText(s.params);
    return `${stepLabel(blocks, s.kind)}${p ? ` (${p})` : ''}`;
  });
  const model = dto.model;
  let modelText = 'No model';
  let members: string[] = [];
  if (model?.kind === 'single') {
    modelText = memberText(model, false);
  } else if (model?.kind === 'voting') {
    modelText = `${model.voting === 'hard' ? 'Hard' : 'Soft'} voting of ${model.members.length} models`;
    members = model.members.map((m) => memberText(m, true));
  } else if (model?.kind === 'stacking') {
    modelText = `Stacking of ${model.members.length} models in ${model.blocks} time blocks, combined by ${memberText(model.final, false)}`;
    members = model.members.map((m) => memberText(m, false));
  }
  const calibration = dto.calibrate === 'none' || !dto.calibrate ? 'No calibration' : `${CALIBRATION_HELP[dto.calibrate].label} calibration`;
  return { steps, model: modelText, members, calibration };
}

export type NodeState = 'ok' | 'warning' | 'error';

export interface DiagramNode {
  /** "data", "step-0", "model", "calibration" or "output". */
  id: string;
  title: string;
  detail?: string;
  state?: NodeState;
}

export function nodesFromRecipe(dto: RecipeDto, blocks: BuildingBlocksDto | null, algorithms: AlgorithmDto[]): DiagramNode[] {
  const s = summarizeRecipe(dto, blocks, algorithms);
  const modelTitle = dto.model?.kind === 'voting' ? 'Voting' : dto.model?.kind === 'stacking' ? 'Stacking' : 'Model';
  const modelDetail = dto.model?.kind === 'single'
    ? s.model
    : dto.model?.kind === 'stacking'
      ? `${dto.model.members.length} models + ${algoLabel(blocks, algorithms, dto.model.final.algorithm)}`
      : dto.model?.kind === 'voting'
        ? `${dto.model.members.length} models, ${dto.model.voting}`
        : s.model;
  const nodes: DiagramNode[] = [{ id: 'data', title: 'Data', detail: 'features' }];
  (dto.steps ?? []).forEach((step, i) => nodes.push({ id: stepCard(i), title: stepLabel(blocks, step.kind), detail: paramText(step.params) || undefined }));
  nodes.push({ id: 'model', title: modelTitle, detail: modelDetail });
  nodes.push({ id: 'calibration', title: 'Calibration', detail: dto.calibrate ?? 'none' });
  nodes.push({ id: 'output', title: 'Predictions', detail: 'up / down' });
  return nodes;
}
