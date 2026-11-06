import React from 'react';
import type { DiagramNode } from './recipeModel';

export interface RecipeDiagramProps {
  nodes: DiagramNode[];
  /** Makes the boxes clickable (buttons). Omit for a read-only diagram. */
  onSelect?: (nodeId: string) => void;
  /** Highlights one box. */
  selected?: string;
  /** Leading part of every data-testid (default "ml-recipe-diagram"). */
  testIdPrefix?: string;
  /** Lay the boxes out top to bottom instead of left to right. */
  vertical?: boolean;
}

const stateCls: Record<string, string> = {
  ok: 'border-[var(--border-color)]',
  warning: 'border-warning/60',
  error: 'border-bear/60',
};

/**
 * A compact pipeline diagram: boxes joined by arrows (CSS only). Used read-only in the Train form and
 * as a clickable overview inside the algorithm builder.
 */
export const RecipeDiagram: React.FC<RecipeDiagramProps> = ({ nodes, onSelect, selected, testIdPrefix = 'ml-recipe-diagram', vertical = false }) => (
  <ol
    className={`m-0 flex list-none items-stretch gap-1 p-0 ${vertical ? 'flex-col' : 'flex-wrap'}`}
    aria-label="Algorithm pipeline"
    data-testid={testIdPrefix}
  >
    {nodes.map((node, i) => {
      const body = (
        <>
          <span className="block max-w-[11rem] truncate text-[11px] font-semibold text-text-primary" title={node.title}>{node.title}</span>
          {node.detail && <span className="block max-w-[11rem] truncate font-mono text-[10px] text-text-muted" title={node.detail}>{node.detail}</span>}
        </>
      );
      const cls = `px-2 py-1 text-left bg-surface-0 border ${stateCls[node.state ?? 'ok']} ${selected === node.id ? 'border-accent bg-accent/10' : ''}`;
      return (
        <li key={node.id} className={`flex items-center gap-1 ${vertical ? 'flex-col items-start' : ''}`}>
          {onSelect ? (
            <button
              type="button" className={`${cls} hover:border-accent focus:border-accent focus:outline-none`}
              onClick={() => onSelect(node.id)} data-testid={`${testIdPrefix}-node-${node.id}`} data-state={node.state ?? 'ok'}
            >
              {body}
            </button>
          ) : (
            <div className={cls} data-testid={`${testIdPrefix}-node-${node.id}`} data-state={node.state ?? 'ok'}>{body}</div>
          )}
          {i < nodes.length - 1 && <span className="select-none font-mono text-xs text-text-muted" aria-hidden="true">{vertical ? '↓' : '→'}</span>}
        </li>
      );
    })}
  </ol>
);
