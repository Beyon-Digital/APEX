import type { ActivityActor, ActivityDto } from '../../lib/types';

export const ACTOR_LABEL: Record<ActivityActor, string> = { user: 'You', copilot: 'Copilot', automation: 'Automation', system: 'System' };
export const ACTOR_TONE: Record<ActivityActor, string> = {
  user: 'border-bull/40 bg-bull/10 text-bull',
  copilot: 'border-accent/50 bg-accent/10 text-accent',
  automation: 'border-warning/40 bg-warning/10 text-warning',
  system: 'border-[var(--border-color)] bg-surface-2 text-text-muted',
};

const ACTION_LABEL: Record<string, string> = {
  'model.train': 'Trained model',
  'model.update': 'Edited model',
  'model.delete': 'Deleted model',
  'strategy.save': 'Saved strategy',
  'strategy.delete': 'Deleted strategy',
  'automation.create': 'Created automation',
  'automation.toggle': 'Toggled automation',
  'automation.delete': 'Deleted automation',
  'automation.short': 'Changed automation shorting',
  'dataset.build': 'Built dataset',
  'dataset.delete': 'Deleted dataset',
};

export const actionLabel = (action: string): string => ACTION_LABEL[action] ?? action.replace(/[._]/g, ' ');

const MAX_DETAIL_CHARS = 20_000;
export const FETCH_LIMIT = 1000;

/** Pretty-print JSON details when they parse, otherwise show the text as is. */
export function formatDetails(details: string): string {
  const text = details.length > MAX_DETAIL_CHARS ? `${details.slice(0, MAX_DETAIL_CHARS)}\n… (${details.length - MAX_DETAIL_CHARS} more characters not shown)` : details;
  try {
    const parsed: unknown = JSON.parse(details);
    if (parsed !== null && typeof parsed === 'object') return JSON.stringify(parsed, null, 2).slice(0, MAX_DETAIL_CHARS);
  } catch { /* plain text */ }
  return text;
}

const csvCell = (value: string): string => {
  // Neutralise spreadsheet formulas in untrusted text.
  const safe = /^[=+\-@\t\r]/.test(value) ? `'${value}` : value;
  return /[",\n\r]/.test(safe) ? `"${safe.replace(/"/g, '""')}"` : safe;
};

export function activityToCsv(rows: ActivityDto[]): string {
  const head = ['time', 'actor', 'action', 'kind', 'target', 'status', 'summary', 'details'];
  const lines = rows.map((a) => [a.ts, a.actor, a.action, a.kind, a.target_id, a.ok ? 'ok' : 'failed', a.summary, a.details ?? ''].map(csvCell).join(','));
  return [head.join(','), ...lines].join('\n');
}

