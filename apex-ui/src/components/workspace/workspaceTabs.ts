export const VALID_TABS = [
  'chart',
  'market',
  'strategy',
  'ml',
  'data',
  'notebook',
  'health',
  'news',
  'blotter',
  'book',
  'graph',
  'scanner',
  'analytics',
  'backtest',
  'automations',
  'copilot',
  'activity',
] as const

export type CenterTab = (typeof VALID_TABS)[number]
