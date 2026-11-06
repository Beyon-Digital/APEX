/** Two-letter label for a feed id, shown in the watchlist's source column. */
export function feedShortName(adapterId: string | null | undefined): string {
  if (!adapterId) return '--';
  const known: Record<string, string> = {
    yahoo: 'YF', yahoo_finance: 'YF', zerodha_kite: 'ZK', angel_one: 'AO', groww: 'GW',
    robinhood: 'RH', binance: 'BN', coinbase: 'CB', polymarket: 'PM',
  };
  return known[adapterId] ?? adapterId.slice(0, 2).toUpperCase();
}
