import React from 'react';
import { useOrderStore } from '../../stores/orderStore';
import { PnlValue } from '../common/PnlValue';
import { formatPrice, formatQuantity } from '../../lib/format';

export const PositionsPanel: React.FC = () => {
  const positions = useOrderStore((s) => s.positions);

  return (
    <div className="flex flex-col h-full" data-testid="positions-panel">
      <div className="flex items-center justify-between border-b border-[var(--border-color)] px-4 py-3">
        <span className="text-sm font-semibold text-text-primary">Positions</span>
        <span className="text-xs text-text-muted">{positions.length} open</span>
      </div>
      <div className="flex-1 overflow-auto">
        {positions.length === 0 ? (
          <div className="flex h-full flex-col items-center justify-center px-4 text-center">
            <p className="text-sm text-text-secondary">No open positions</p>
            <p className="mt-1 text-xs text-text-muted">Open holdings will appear here.</p>
          </div>
        ) : (
          <table className="w-full">
            <thead>
              <tr className="border-b border-[var(--border-color)] text-xs text-text-muted">
                <th className="px-3 py-1.5 text-left font-normal">Symbol</th>
                <th className="px-3 py-1.5 text-right font-normal">Qty</th>
                <th className="px-3 py-1.5 text-right font-normal">Avg</th>
                <th className="px-3 py-1.5 text-right font-normal">P&L</th>
              </tr>
            </thead>
            <tbody>
              {positions.map((pos) => (
                <tr key={`${pos.broker_id}:${pos.symbol}`} className="border-b border-[var(--border-color)] hover:bg-surface-2" data-testid={`position-${pos.symbol}`}>
                  <td className="px-3 py-2 font-mono text-sm" data-testid="position-row">
                    <span className={pos.side === 'Buy' ? 'text-bull' : 'text-bear'}>
                      {pos.side === 'Buy' ? '▲' : '▼'}
                    </span>{' '}
                    {pos.symbol}
                  </td>
                  <td className="px-3 py-2 font-mono text-sm text-right" data-numeric>
                    {formatQuantity(pos.quantity)}
                  </td>
                  <td className="px-3 py-2 font-mono text-sm text-right" data-numeric>
                    {formatPrice(pos.avg_price)}
                  </td>
                  <td className="px-3 py-1.5 text-right">
                    <PnlValue value={pos.pnl} className="text-sm" />
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        )}
      </div>
    </div>
  );
};
