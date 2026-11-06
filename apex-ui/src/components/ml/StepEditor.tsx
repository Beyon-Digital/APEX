import React from 'react';
import type { RecipeIssueDto, StepKindDto } from '../../lib/types';
import { btnCls, CardIssues } from './MemberEditor';
import { ParamEditor } from './ParamEditor';
import type { EditStep } from './recipeModel';

export interface StepEditorProps {
  index: number;
  count: number;
  step: EditStep;
  /** The step's definition from the building blocks; `undefined` when the backend no longer offers it. */
  kind: StepKindDto | undefined;
  issues: RecipeIssueDto[];
  onChange: (step: EditStep) => void;
  /** `viaKey` is true for the keyboard shortcut, so focus can stay on the card instead of a button. */
  onMove: (delta: -1 | 1, viaKey?: boolean) => void;
  onRemove: () => void;
}

/**
 * One preprocessing step as a card. The card itself takes focus; Alt+Up and Alt+Down move it, and the
 * buttons do the same for pointer users.
 */
export const StepEditor: React.FC<StepEditorProps> = ({ index, count, step, kind, issues, onChange, onMove, onRemove }) => {
  const cardKey = `step-${index}`;
  const onKeyDown = (e: React.KeyboardEvent<HTMLDivElement>) => {
    if (e.target !== e.currentTarget || !e.altKey) return;
    if (e.key === 'ArrowUp' && index > 0) { e.preventDefault(); onMove(-1, true); }
    if (e.key === 'ArrowDown' && index < count - 1) { e.preventDefault(); onMove(1, true); }
  };
  const title = kind?.label ?? step.kind;
  return (
    <div
      className={`space-y-2 border p-2 bg-surface-1 focus:outline-none focus:border-accent ${issues.length > 0 ? 'border-bear/50' : 'border-[var(--border-color)]'}`}
      tabIndex={0} role="group" aria-label={`Step ${index + 1}: ${title}. Alt plus arrow keys move it.`}
      data-card-id={step.id} data-card-key={cardKey} data-testid={`rb-card-${cardKey}`} data-state={issues.length > 0 ? 'error' : 'ok'} data-kind={step.kind}
      onKeyDown={onKeyDown}
    >
      <div className="flex flex-wrap items-center gap-2">
        <span className="font-mono text-[11px] text-text-muted">{index + 1}.</span>
        <span className="min-w-0 truncate text-xs font-semibold text-text-primary" title={title} data-testid={`rb-step-title-${index}`}>{title}</span>
        <span className="ml-auto flex gap-1">
          <button type="button" className={btnCls} onClick={() => onMove(-1)} disabled={index === 0} data-testid={`rb-step-up-${index}`} aria-label={`Move step ${index + 1} up`}>Up</button>
          <button type="button" className={btnCls} onClick={() => onMove(1)} disabled={index === count - 1} data-testid={`rb-step-down-${index}`} aria-label={`Move step ${index + 1} down`}>Down</button>
          <button type="button" className={btnCls} onClick={onRemove} data-testid={`rb-step-remove-${index}`} aria-label={`Remove step ${index + 1}`}>Remove</button>
        </span>
      </div>
      {kind ? (
        <>
          <div className="text-[11px] text-text-muted">{kind.description}</div>
          {kind.params.length > 0 ? (
            <ParamEditor
              defs={kind.params} values={step.values} advanced="" prefix={`rb-s${index}`} showAdvanced={false} hideSearch
              onValues={(values) => onChange({ ...step, values })} onAdvanced={() => undefined}
            />
          ) : (
            <div className="text-[11px] text-text-muted" data-testid={`rb-s${index}-no-params`}>This step has no settings.</div>
          )}
        </>
      ) : (
        <div className="border border-warning/40 bg-warning/10 px-2 py-1 text-[11px] text-warning" data-testid={`rb-s${index}-unavailable`}>
          The step &quot;{step.kind}&quot; is not available here. Remove it to continue.
        </div>
      )}
      <CardIssues cardKey={cardKey} issues={issues} />
    </div>
  );
};
