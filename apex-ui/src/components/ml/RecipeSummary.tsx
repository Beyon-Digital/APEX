import React from 'react';
import type { RecipeDto, RecipeMemberDto } from '../../lib/types';
import { algorithmLabel, calibrationText, describeModelKind, paramViews, stepLabel, stepParamViews } from './modelUtils';
import type { RecipeParamView } from './modelUtils';

/** Read-only view of a recipe: ordered steps with their parameters, then the model, then calibration. */
export interface RecipeSummaryProps {
  recipe: RecipeDto;
}

const Params: React.FC<{ views: RecipeParamView[]; testId: string }> = ({ views, testId }) => (
  <span className="flex flex-wrap gap-1" data-testid={testId}>
    {views.length === 0
      ? <span className="text-text-muted">default settings</span>
      : views.map((v) => (
        <span
          key={v.key} data-changed={v.changed}
          className={`border px-1 font-mono text-[10px] ${v.changed ? 'border-accent/40 bg-accent/10 text-accent' : 'border-[var(--border-color)] bg-surface-2 text-text-muted'}`}
          title={v.changed ? 'Different from the default' : 'Same as the default'}
        >
          {v.text}
        </span>
      ))}
  </span>
);

const Member: React.FC<{ member: RecipeMemberDto; weight?: boolean; testId: string }> = ({ member, weight = false, testId }) => (
  <li className="flex flex-wrap items-center gap-x-2 gap-y-0.5 text-[11px]" data-testid={testId} data-algorithm={member.algorithm}>
    <span className="font-mono text-text-primary" data-testid={`${testId}-name`}>{algorithmLabel(member.algorithm)}</span>
    {weight && <span className="font-mono text-[10px] text-text-secondary" data-testid={`${testId}-weight`}>weight {member.weight ?? 1}</span>}
    <Params views={paramViews(member.params)} testId={`${testId}-params`} />
  </li>
);

export const RecipeSummary: React.FC<RecipeSummaryProps> = ({ recipe }) => {
  const model = recipe.model;
  return (
    <div className="space-y-2 text-[11px]" data-testid="recipe-summary" data-recipe={recipe.name}>
      <div>
        <div className="font-mono text-sm text-text-primary" data-testid="recipe-summary-label">{recipe.label || recipe.name}</div>
        <div className="font-mono text-[10px] text-text-muted" data-testid="recipe-summary-name">{recipe.name}</div>
        {recipe.description && <p className="mt-0.5 text-text-secondary" data-testid="recipe-summary-description">{recipe.description}</p>}
      </div>

      <div>
        <div className="mb-0.5 text-[10px] uppercase text-text-muted">Preprocessing steps ({recipe.steps.length}), applied in order</div>
        {recipe.steps.length === 0 ? (
          <p className="text-text-muted" data-testid="recipe-no-steps">No preprocessing: the model reads the features as they are.</p>
        ) : (
          <ol className="space-y-1" data-testid="recipe-steps">
            {recipe.steps.map((s, i) => (
              <li key={`${s.kind}-${i}`} className="flex flex-wrap items-center gap-x-2 gap-y-0.5" data-testid="recipe-step" data-kind={s.kind}>
                <span className="w-4 font-mono text-text-muted">{i + 1}.</span>
                <span className="font-mono text-text-primary" data-testid="recipe-step-name">{stepLabel(s.kind)}</span>
                <Params views={stepParamViews(s)} testId="recipe-step-params" />
              </li>
            ))}
          </ol>
        )}
      </div>

      <div>
        <div className="mb-0.5 text-[10px] uppercase text-text-muted">Model</div>
        <div className="font-mono text-text-primary" data-testid="recipe-model-kind" data-kind={model?.kind}>{describeModelKind(recipe)}</div>
        {model?.kind === 'single' && (
          <ul className="mt-1 space-y-1"><Member member={{ algorithm: model.algorithm, params: model.params }} testId="recipe-member" /></ul>
        )}
        {model && model.kind !== 'single' && (
          <>
            <div className="mt-1 text-[10px] text-text-muted">{model.kind === 'voting' ? 'Members (weights decide how much each vote counts)' : 'Members (their out-of-fold predictions train the final estimator)'}</div>
            <ul className="mt-0.5 space-y-1" data-testid="recipe-members">
              {model.members.map((m, i) => <Member key={`${m.algorithm}-${i}`} member={m} weight={model.kind === 'voting'} testId="recipe-member" />)}
            </ul>
          </>
        )}
        {model?.kind === 'stacking' && (
          <div className="mt-1.5" data-testid="recipe-stacking">
            <div className="text-[10px] text-text-muted">Final estimator</div>
            <ul className="mt-0.5"><Member member={model.final} testId="recipe-final" /></ul>
            <div className="mt-1 font-mono text-text-secondary" data-testid="recipe-blocks">{model.blocks} time blocks, each predicted only by models trained on earlier rows</div>
          </div>
        )}
      </div>

      <div>
        <div className="text-[10px] uppercase text-text-muted">Probability calibration</div>
        <div className="font-mono text-text-primary" data-testid="recipe-calibration" data-value={recipe.calibrate}>
          {calibrationText(recipe.calibrate)}
          {recipe.calibrate !== 'none' && model?.kind === 'voting' && model.voting === 'hard' && (
            <span className="ml-2 text-warning" data-testid="recipe-calibration-ignored">ignored: hard voting has no probabilities</span>
          )}
        </div>
      </div>
    </div>
  );
};
