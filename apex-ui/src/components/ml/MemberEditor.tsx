import React from 'react';
import type { AlgorithmDto, BuildingBlocksDto, RecipeIssueDto } from '../../lib/types';
import { ParamEditor } from './ParamEditor';
import type { EditMember } from './recipeModel';

export const inputCls =
  'px-2 py-1 text-xs bg-surface-0 border border-[var(--border-color)] text-text-primary focus:border-accent focus:outline-none disabled:opacity-50';
export const btnCls =
  'px-2 py-1 text-xs font-mono border border-[var(--border-color)] bg-surface-2 text-text-secondary hover:text-text-primary hover:border-accent disabled:opacity-50 disabled:cursor-not-allowed';

/** The problems the Check found for one card, shown inside it. */
export const CardIssues: React.FC<{ cardKey: string; issues: RecipeIssueDto[] }> = ({ cardKey, issues }) => {
  if (issues.length === 0) return null;
  return (
    <ul className="list-disc space-y-0.5 border border-bear/40 bg-bear/10 py-1 pl-6 pr-2 text-[11px] text-bear" data-testid={`rb-issues-${cardKey}`} role="alert">
      {issues.map((issue, i) => (
        <li key={`${issue.path}-${i}`} data-testid={`rb-issue-${cardKey}-${i}`}>
          {issue.message}
          {issue.path && <span className="ml-1 font-mono text-[10px] text-bear/70">{issue.path}</span>}
        </li>
      ))}
    </ul>
  );
};

export interface MemberEditorProps {
  /** Leading part of the card's controls' test ids, e.g. "rb-m0". */
  prefix: string;
  /** Which card this is for the issue list and focus handling: "member-0", "final" or "model". */
  cardKey: string;
  title: string;
  member: EditMember;
  blocks: BuildingBlocksDto;
  algorithms: AlgorithmDto[];
  showWeight?: boolean;
  issues: RecipeIssueDto[];
  onChange: (member: EditMember) => void;
  onRemove?: () => void;
  /** Render without its own frame (the model card already provides one). */
  bare?: boolean;
}

/** One model of a recipe: algorithm choice, optional weight, and its parameters. */
export const MemberEditor: React.FC<MemberEditorProps> = ({
  prefix, cardKey, title, member, blocks, algorithms, showWeight = false, issues, onChange, onRemove, bare = false,
}) => {
  const offered = blocks.members.some((m) => m.id === member.algorithm);
  const defs = algorithms.find((a) => a.id === member.algorithm)?.params ?? [];
  const description = blocks.members.find((m) => m.id === member.algorithm)?.description;
  const pick = (algorithm: string) => onChange({ ...member, algorithm, values: {}, advanced: '' });
  const frame = bare ? 'space-y-2' : `space-y-2 border p-2 bg-surface-0 ${issues.length > 0 ? 'border-bear/50' : 'border-[var(--border-color)]'}`;
  return (
    <div className={frame} data-card-key={bare ? undefined : cardKey} tabIndex={bare ? undefined : -1} data-testid={bare ? `${prefix}-body` : `rb-card-${cardKey}`} data-state={issues.length > 0 ? 'error' : 'ok'}>
      <div className="flex flex-wrap items-center gap-2">
        {!bare && <span className="min-w-0 truncate text-[11px] font-semibold uppercase tracking-wider text-text-secondary">{title}</span>}
        <label className="flex items-center gap-1 text-[11px] text-text-secondary">
          Algorithm
          <select className={`${inputCls} w-56 font-mono`} value={member.algorithm} onChange={(e) => pick(e.target.value)} data-testid={`${prefix}-algo`} aria-label={`${title} algorithm`}>
            {!offered && <option value={member.algorithm}>{member.algorithm} (not available)</option>}
            {blocks.members.map((m) => <option key={m.id} value={m.id}>{m.label}</option>)}
          </select>
        </label>
        {showWeight && (
          <label className="flex items-center gap-1 text-[11px] text-text-secondary">
            Weight
            <input
              className={`${inputCls} w-16 font-mono`} type="number" min={0} step="any" value={member.weight}
              onChange={(e) => onChange({ ...member, weight: e.target.value })} data-testid={`${prefix}-weight`} aria-label={`${title} weight`}
            />
          </label>
        )}
        {onRemove && <button type="button" className={`${btnCls} ml-auto`} onClick={onRemove} data-testid={`${prefix}-remove`} aria-label={`Remove ${title}`}>Remove</button>}
      </div>
      {description && <div className="text-[11px] text-text-muted">{description}</div>}
      {!offered && (
        <div className="border border-warning/40 bg-warning/10 px-2 py-1 text-[11px] text-warning" data-testid={`${prefix}-unavailable`}>
          The algorithm &quot;{member.algorithm}&quot; is not available here, for example because its package is not installed. Choose another to continue.
          {member.advanced.trim() !== '' && ' Its saved parameters are kept in the Advanced box until you do.'}
        </div>
      )}
      <ParamEditor
        defs={defs} values={member.values} advanced={member.advanced} prefix={prefix} hideSearch={defs.length <= 8}
        onValues={(values) => onChange({ ...member, values })} onAdvanced={(advanced) => onChange({ ...member, advanced })}
      />
      {!bare && <CardIssues cardKey={cardKey} issues={issues} />}
    </div>
  );
};
