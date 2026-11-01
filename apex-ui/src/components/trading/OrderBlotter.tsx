import React, { useCallback, useEffect, useState } from 'react';
import { cancelOrder, getOrders } from '../../lib/tauri';
import type { OrderDto } from '../../lib/types';
import { formatPrice, formatQuantity } from '../../lib/format';
import { useBrokerStore } from '../../stores/brokerStore';

const POLL_MS = 3_000;

const STATUS_TONE: Record<string, string> = {
  Filled: 'text-bull',
  PartiallyFilled: 'text-warning',
  Pending: 'text-warning',
  New: 'text-accent',
  Open: 'text-warning',
  Cancelled: 'text-text-muted',
  Rejected: 'text-bear',
};

export const OrderBlotter: React.FC = () => {
  const [orders, setOrders] = useState<OrderDto[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [cancelError, setCancelError] = useState<string | null>(null);
  const [busyId, setBusyId] = useState<string | null>(null);
  const activeBrokerId = useBrokerStore((s) => s.activeBrokerId);

  const refresh = useCallback(async () => {
    try {
      setOrders(await getOrders(undefined, 200));
      setError(null);
    } catch (err) {
      setError(typeof err === 'string' ? err : err instanceof Error ? err.message : 'Unable to load orders');
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    void refresh();
    const id = setInterval(() => void refresh(), POLL_MS);
    return () => clearInterval(id);
  }, [refresh]);

  const handleCancel = useCallback(async (order: OrderDto) => {
    setBusyId(order.id);
    setCancelError(null);
    try {
      await cancelOrder(order.id, order.broker_id || activeBrokerId);
      await refresh();
    } catch (err) {
      setCancelError(typeof err === 'string' ? err : err instanceof Error ? err.message : 'Cancel failed');
    } finally {
      setBusyId(null);
    }
  }, [activeBrokerId, refresh]);

  const cancellable = (order: OrderDto) =>
    ['New', 'Pending', 'Open', 'PartiallyFilled'].includes(order.status);

  return (
    <div className="flex flex-col h-full" data-testid="order-blotter">
      <div className="flex items-center justify-between border-b border-[var(--border-color)] px-4 py-3">
        <span className="text-sm font-semibold text-text-primary">Orders</span>
        <span className="text-xs text-text-muted">{orders.length} orders</span>
      </div>
      {error && <p role="alert" className="px-3 py-1 text-xs text-bear">{error}</p>}
      {cancelError && <p role="alert" className="px-3 py-1 text-xs text-bear">Cancel failed: {cancelError}</p>}
      <div className="flex-1 overflow-auto">
        {loading ? (
          <div className="flex h-full items-center justify-center text-sm text-text-muted">Loading orders…</div>
        ) : error && orders.length === 0 ? null : orders.length === 0 ? (
          <div className="flex h-full flex-col items-center justify-center px-4 text-center">
            <p className="text-sm text-text-secondary">No orders yet</p>
            <p className="mt-1 text-xs text-text-muted">Submitted orders will appear here.</p>
          </div>
        ) : (
          <table className="w-full">
            <thead>
              <tr className="text-xs text-text-muted border-b border-[var(--border-color)]">
                <th className="px-2 py-1.5 text-left font-normal">Time</th>
                <th className="px-2 py-1.5 text-left font-normal">Symbol</th>
                <th className="px-2 py-1.5 text-left font-normal">Side</th>
                <th className="px-2 py-1.5 text-left font-normal">Type</th>
                <th className="px-2 py-1.5 text-right font-normal">Qty</th>
                <th className="px-2 py-1.5 text-right font-normal">Limit / fill</th>
                <th className="px-2 py-1.5 text-right font-normal">Stop trigger</th>
                <th className="px-2 py-1.5 text-right font-normal">Filled</th>
                <th className="px-2 py-1.5 text-left font-normal">Status</th>
                <th className="px-2 py-1.5 text-left font-normal">Broker</th>
                <th className="px-2 py-1.5 text-right font-normal"></th>
              </tr>
            </thead>
            <tbody>
              {orders.map((order) => (
                <tr key={order.id} className="border-b border-[var(--border-color)] hover:bg-surface-2" data-testid="blotter-row">
                  <td className="px-2 py-1.5 font-mono text-xs text-text-muted">
                    {new Date(order.created_at).toLocaleTimeString()}
                  </td>
                  <td className="px-2 py-1.5 font-mono text-xs">{order.symbol}</td>
                  <td className={`px-2 py-1.5 font-mono text-xs ${order.side === 'Buy' ? 'text-bull' : 'text-bear'}`}>
                    {order.side}
                  </td>
                  <td className="px-2 py-1.5 font-mono text-xs text-text-muted">{order.order_type}</td>
                  <td className="px-2 py-1.5 font-mono text-xs text-right" data-numeric>
                    {formatQuantity(order.quantity)}
                  </td>
                  <td className="px-2 py-1.5 font-mono text-xs text-right" data-numeric>
                    {order.filled_qty > 0 && order.avg_price > 0 ? formatPrice(order.avg_price) : order.price == null ? '—' : formatPrice(order.price)}
                  </td>
                  <td className="px-2 py-1.5 font-mono text-xs text-right" data-numeric>
                    {order.stop_price == null ? '—' : formatPrice(order.stop_price)}
                  </td>
                  <td className="px-2 py-1.5 font-mono text-xs text-right" data-numeric>
                    {formatQuantity(order.filled_qty)}
                  </td>
                  <td className={`px-2 py-1.5 font-mono text-xs ${STATUS_TONE[order.status] ?? 'text-text-muted'}`}>
                    {order.status}
                    {order.status === 'Rejected' && order.rejection_reason && (
                      <div className="max-w-56 whitespace-normal text-text-secondary" data-testid="blotter-rejection-reason">{order.rejection_reason}</div>
                    )}
                  </td>
                  <td className="px-2 py-1.5 font-mono text-xs text-text-muted">{order.broker_id}</td>
                  <td className="px-2 py-1.5 text-right">
                    {cancellable(order) && (
                      <button
                        onClick={() => void handleCancel(order)}
                        disabled={busyId === order.id}
                        className="text-[10px] text-bear hover:brightness-125 disabled:opacity-50"
                        data-testid={`blotter-cancel-${order.id}`}
                      >
                        Cancel
                      </button>
                    )}
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
