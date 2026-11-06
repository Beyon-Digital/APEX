import React, { useMemo } from 'react';
import { DIFF_LINE_CAP, diffLines, foldContext } from './auditUtils';

interface Props {
  before: string;
  after: string;
  beforeLabel: string;
  afterLabel: string;
}

const LINE_TONE = { same: 'text-text-secondary', add: 'bg-bull/10 text-bull', del: 'bg-bear/10 text-bear' } as const;
const MARK = { same: ' ', add: '+', del: '-' } as const;

/** Read-only unified line diff of two versions of the same object. */
export const VersionDiff: React.FC<Props> = ({ before, after, beforeLabel, afterLabel }) => {
  const diff = useMemo(() => diffLines(before, after), [before, after]);
  const rows = useMemo(() => foldContext(diff.ops), [diff]);
  const identical = diff.added === 0 && diff.removed === 0 && !diff.capped;
  return (
    <div className="border border-[var(--border-color)] bg-surface-0" data-testid="audit-diff">
      <div className="flex flex-wrap items-center gap-2 border-b border-[var(--border-color)] px-2 py-1 font-mono text-[10px] text-text-muted">
        <span>{beforeLabel}</span>
        <span aria-hidden="true">→</span>
        <span>{afterLabel}</span>
        <span className="flex-1" />
        <span className="text-bull" data-testid="audit-diff-added">+{diff.added}</span>
        <span className="text-bear" data-testid="audit-diff-removed">-{diff.removed}</span>
      </div>
      {diff.capped && (
        <div className="border-b border-warning/40 bg-warning/10 px-2 py-1 text-[10px] text-warning" data-testid="audit-diff-capped">
          Only the first {DIFF_LINE_CAP} lines of each version are compared. The stored versions are complete.
        </div>
      )}
      {identical ? (
        <div className="px-2 py-2 text-[11px] text-text-muted" data-testid="audit-diff-identical">These two versions have identical content.</div>
      ) : (
        <pre className="max-h-72 overflow-auto py-1 font-mono text-[11px] leading-snug">
          {rows.map((row, i) => (row.type === 'fold' ? (
            <div key={i} className="px-2 text-text-muted" data-testid="audit-diff-fold">… {row.count} unchanged line{row.count === 1 ? '' : 's'}</div>
          ) : (
            <div key={i} className={`whitespace-pre-wrap break-words px-2 ${LINE_TONE[row.type]}`} data-testid="audit-diff-line" data-op={row.type}>
              {MARK[row.type]} {row.text}
            </div>
          )))}
        </pre>
      )}
    </div>
  );
};
