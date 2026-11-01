/** Quote age reflects the market timestamp, not when we happened to poll it. */
export function quoteAge(updatedAt: string, now = Date.now()): { stale: boolean; label: string; title: string } {
  const time = Date.parse(updatedAt);
  if (!Number.isFinite(time) || time > now + 5_000) {
    return { stale: true, label: 'Time unknown', title: 'Quote time unavailable' };
  }
  const age = Math.max(0, now - time);
  const amount = age < 3_600_000 ? `${Math.floor(age / 60_000)}m` : age < 86_400_000 ? `${Math.floor(age / 3_600_000)}h` : `${Math.floor(age / 86_400_000)}d`;
  return { stale: age > 60_000, label: `${amount} ago`, title: `Last market update: ${new Date(time).toLocaleString()}` };
}
