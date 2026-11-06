import type { AutomationDto, PositionDto } from '../../lib/types';

export type PositionState = 'long' | 'short' | 'flat';

/**
 * Where an automation stands in its symbol, from the positions the app already holds.
 * Null when it cannot be known: positions are for the active broker, so another broker's symbol may simply be missing.
 */
export function positionState(rule: Pick<AutomationDto, 'symbol' | 'broker_id'>, positions: PositionDto[], activeBrokerId: string): PositionState | null {
  const symbol = rule.symbol.toUpperCase();
  const pos = positions.find((p) => p.symbol.toUpperCase() === symbol && (!p.broker_id || p.broker_id === rule.broker_id));
  if (pos && pos.quantity !== 0) {
    const side = pos.side.toLowerCase();
    return pos.quantity < 0 || side === 'sell' || side === 'short' ? 'short' : 'long';
  }
  return rule.broker_id === activeBrokerId ? 'flat' : null;
}
