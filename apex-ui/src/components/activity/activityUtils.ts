import type { ActivityActor, ActivityDto } from '../../lib/types';

export type ActivityRowActor = ActivityActor | 'external';
export const ACTOR_LABEL: Record<ActivityRowActor, string> = { user: 'You', copilot: 'Copilot', automation: 'Automation', system: 'System', external: 'Outside APEX' };
export const ACTOR_TONE: Record<ActivityRowActor, string> = {
  user: 'border-bull/40 bg-bull/10 text-bull',
  copilot: 'border-accent/50 bg-accent/10 text-accent',
  automation: 'border-warning/40 bg-warning/10 text-warning',
  system: 'border-[var(--border-color)] bg-surface-2 text-text-muted',
  external: 'border-bear/50 bg-bear/10 text-bear',
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
  'recipe.save': 'Saved algorithm recipe',
  'recipe.delete': 'Deleted algorithm recipe',
  'recipe.restore': 'Restored algorithm recipe',
  'estimator.save': 'Saved estimator',
  'estimator.delete': 'Deleted estimator',
  'estimator.restore': 'Restored estimator',
  'dataset.fetch': 'Fetched market data',
  // Control plane: who changed a limit, a credential or the halt, and when.
  'settings.save': 'Changed settings',
  'risk.reset_halt': 'Reset trading halt',
  'adapter.save': 'Saved adapter credentials',
  'adapter.enable': 'Enabled or disabled adapter',
  'adapter.import_env': 'Imported adapter credentials from the environment',
  'adapter.delete': 'Deleted adapter credentials',
  'vault.init': 'Created vault',
  'vault.unlock': 'Unlocked vault',
  'vault.lock': 'Locked vault',
  'vault.change_passphrase': 'Changed vault passphrase',
  'broker.login': 'Logged in to broker',
  'broker.logout': 'Logged out of broker',
  'broker.session': 'Set broker session',
  'chatgpt.sign_in': 'Signed in with ChatGPT',
  'chatgpt.sign_out': 'Signed out of ChatGPT',
  'live.arm': 'Armed live trading',
  'live.disarm': 'Disarmed live trading',
  'alert.create': 'Created alert',
  'alert.delete': 'Deleted alert',
  'alert.restore': 'Restored alert',
  'settings.restore': 'Restored settings',
  'settings.external_edit': 'Settings file edited outside APEX',
  'system.kill_switch': 'Kill switch',
  'ledger.restore': 'Restored from history',
};

/** Prefixes of actions that change the control plane: limits, credentials, the vault, brokers, live trading (and its kill switch), alerts. */
export const CONTROL_GROUPS: { id: string; label: string; prefixes: string[] }[] = [
  { id: 'settings', label: 'Settings', prefixes: ['settings.'] },
  { id: 'risk', label: 'Risk', prefixes: ['risk.'] },
  { id: 'vault', label: 'Vault', prefixes: ['vault.'] },
  { id: 'adapters', label: 'Adapters', prefixes: ['adapter.'] },
  { id: 'brokers', label: 'Brokers', prefixes: ['broker.', 'chatgpt.', 'live.', 'system.kill_switch'] },
  { id: 'alerts', label: 'Alerts', prefixes: ['alert.'] },
];

export const CONTROL_ALL = 'control';

/** Does this action belong to the chosen group filter ("" = everything, "control" = every control-plane group)? */
export function inControlGroup(action: string, group: string): boolean {
  if (!group) return true;
  const groups = group === CONTROL_ALL ? CONTROL_GROUPS : CONTROL_GROUPS.filter((g) => g.id === group);
  return groups.some((g) => g.prefixes.some((p) => action.startsWith(p)));
}

export const actionLabel = (action: string): string => ACTION_LABEL[action] ?? action.replace(/[._]/g, ' ');

const MAX_DETAIL_CHARS = 20_000;
export const FETCH_LIMIT = 1000;

const MASK = '<masked>';
const KEY_NAME = '(?:password|passphrase|secret|token|api[_-]?key|key_material)';
const HEX_HASH = /^(?:[0-9a-f]{32}|[0-9a-f]{40}|[0-9a-f]{64})$/i;

/**
 * Names such as "MomentumBreakoutStrategy2" or "RandomForest_nifty50_daily_2026" are made of words: most of
 * their characters sit in runs of three or more lower-case letters (optionally led by a capital). A random token
 * has few such runs.
 */
function looksLikeWords(token: string): boolean {
  const inWords = (token.match(/[A-Z]?[a-z]{3,}/g) ?? []).reduce((n, w) => n + w.length, 0);
  return inWords / token.length >= 0.6;
}

/**
 * Defensive masking for anything shown from the activity log. The backend never sends secrets; this
 * catches the case where one slipped into an error message or a note. It masks:
 *  - the password in URL userinfo (scheme://user:password@host),
 *  - the value of a key whose name says it is a secret (password=..., "api_key": "...", token: ...),
 *  - JWTs, `Bearer` credentials, and long token-like strings (24+ URL-safe characters with digits and
 *    mixed case, or 40+ with digits and no underscore). Hashes (32, 40 or 64 hex characters), ordinary
 *    snake_case ids, CamelCase names made of words, file paths and the word "changed" (what the backend
 *    writes for a secret setting) are left alone.
 */
export function maskSecrets(text: string): string {
  // A value the backend already redacted stays as it is, so the history reads "<redacted>" everywhere.
  const placeholder = (v: string) => /^["']?<(?:redacted|masked)>["']?$/.test(v) || v === 'changed';
  let out = text.replace(/(\b[a-z][a-z0-9+.-]*:\/\/[^\s:/@]+:)([^\s@/]+)(@)/gi, (_m, head: string, pw: string, at: string) => `${head}${placeholder(pw) ? pw : MASK}${at}`);
  out = out.replace(new RegExp(`("?${KEY_NAME}"?\\s*[:=]\\s*)("[^"]*"|'[^']*'|[^\\s,;&}]+)`, 'gi'), (_m, head: string, value: string) => {
    if (placeholder(value)) return `${head}${value}`;
    const quote = value.startsWith('"') ? '"' : value.startsWith("'") ? "'" : '';
    return `${head}${quote}${MASK}${quote}`;
  });
  out = out.replace(/\beyJ[A-Za-z0-9_-]{6,}\.[A-Za-z0-9_-]{6,}\.[A-Za-z0-9_-]*/g, MASK);
  out = out.replace(/\b(Bearer\s+)[A-Za-z0-9._~+/=-]{8,}/gi, `$1${MASK}`);
  out = out.replace(/[A-Za-z0-9_-]{24,}/g, (token) => {
    if (HEX_HASH.test(token) || !/\d/.test(token) || looksLikeWords(token)) return token;
    const mixedCase = /[a-z]/.test(token) && /[A-Z]/.test(token);
    // Ids such as "model_20260101_run_a" are lower-case with underscores: only very long ones are treated as tokens.
    return mixedCase || (token.length >= 40 && !token.includes('_')) ? MASK : token;
  });
  return out;
}

/** Pretty-print JSON details when they parse, otherwise show the text as is. Secret-looking values are masked. */
export function formatDetails(details: string): string {
  const safe = maskSecrets(details);
  const text = safe.length > MAX_DETAIL_CHARS ? `${safe.slice(0, MAX_DETAIL_CHARS)}\n… (${safe.length - MAX_DETAIL_CHARS} more characters not shown)` : safe;
  try {
    const parsed: unknown = JSON.parse(safe);
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
  const lines = rows.map((a) => [a.ts, a.actor, a.action, a.kind, a.target_id, a.ok ? 'ok' : 'failed', maskSecrets(a.summary), maskSecrets(a.details ?? '')].map(csvCell).join(','));
  return [head.join(','), ...lines].join('\n');
}

