// Central formatting utilities — never use toString() on prices

const priceFormatter = new Intl.NumberFormat('en-IN', {
  minimumFractionDigits: 2,
  maximumFractionDigits: 2,
});

const largeNumberFormatter = new Intl.NumberFormat('en-IN', {
  notation: 'compact',
  maximumFractionDigits: 1,
});

const pctFormatter = new Intl.NumberFormat('en-IN', {
  minimumFractionDigits: 2,
  maximumFractionDigits: 2,
  signDisplay: 'always',
});

export function formatPrice(value: number): string {
  return priceFormatter.format(value);
}

export function formatVolume(value: number): string {
  return largeNumberFormatter.format(value);
}

export function formatPct(value: number): string {
  return pctFormatter.format(value) + '%';
}

export function formatPnl(value: number): string {
  const sign = value >= 0 ? '+' : '';
  return sign + priceFormatter.format(value);
}

export function formatQuantity(value: number): string {
  if (Number.isInteger(value)) {
    return value.toString();
  }
  return value.toFixed(4);
}

/** JSON represents an infinite profit factor as null (no losing trades). */
export function formatProfitFactor(value: number | null, wins: number, losses: number): string {
  if (value === null || !Number.isFinite(value)) return wins > 0 && losses === 0 ? '∞' : '—';
  return value.toFixed(2);
}
