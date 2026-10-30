import React, { useState, useCallback, useMemo } from 'react';
import { formatPrice } from '../../lib/format';
import { placeOrder, previewOrder, setLiveArmed } from '../../lib/tauri';
import type { NewOrderRequestDto, OrderPreviewDto } from '../../lib/types';
import { useBrokerStore } from '../../stores/brokerStore';
import { useLiveStore } from '../../stores/liveStore';

interface OrderEntryProps {
  defaultSymbol?: string;
}

type OrderType = 'MARKET' | 'LIMIT' | 'STOP' | 'STOP_LIMIT';

export const OrderEntry: React.FC<OrderEntryProps> = ({ defaultSymbol }) => {
  const [symbol, setSymbol] = useState(defaultSymbol ?? '');
  const [side, setSide] = useState<'BUY' | 'SELL'>('BUY');
  const [orderType, setOrderType] = useState<OrderType>('MARKET');
  const [quantity, setQuantity] = useState('');
  const [price, setPrice] = useState('');
  const [stopPrice, setStopPrice] = useState('');
  const [status, setStatus] = useState<{ type: 'idle' | 'submitting' | 'success' | 'error'; message?: string }>({ type: 'idle' });
  const [product, setProduct] = useState<'CNC' | 'MIS' | 'NRML'>('CNC');
  const [confirming, setConfirming] = useState<{ request: NewOrderRequestDto; preview: OrderPreviewDto } | null>(null);
  const liveArmed = useLiveStore((s) => s.status.armed);
  const setLiveStatus = useLiveStore((s) => s.setStatus);
  const brokerConnections = useBrokerStore((s) => s.connections);
  const activeBrokerId = useBrokerStore((s) => s.activeBrokerId);
  const setActiveBrokerId = useBrokerStore((s) => s.setActiveBrokerId);

  const executionBrokers = useMemo(() => {
    const available = brokerConnections.filter((broker) => broker.mode === 'paper' || broker.execution_available);
    return available.length > 0
      ? available
      : [{
          broker_id: 'paper', display_name: 'Paper Trading', mode: 'paper', status: 'ready',
          configured: true, authenticated: true, execution_available: true, market_data_available: false,
          token_field_label: '', message: 'Paper trading is active for safe simulated execution.',
        }];
  }, [brokerConnections]);
  const activeBroker = useMemo(
    () => executionBrokers.find((broker) => broker.broker_id === activeBrokerId) ?? executionBrokers[0],
    [activeBrokerId, executionBrokers],
  );

  React.useEffect(() => {
    if (defaultSymbol) setSymbol(defaultSymbol);
  }, [defaultSymbol]);

  const needsLimit = orderType === 'LIMIT' || orderType === 'STOP_LIMIT';
  const needsStop = orderType === 'STOP' || orderType === 'STOP_LIMIT';
  const positive = (value: string) => value.trim() !== '' && Number.isFinite(Number(value)) && Number(value) > 0;
  const missingLiveSession = activeBroker?.mode === 'live' && !activeBroker.authenticated;
  const canSubmit = Boolean(symbol.trim()) && positive(quantity)
    && (!needsLimit || positive(price)) && (!needsStop || positive(stopPrice))
    && !missingLiveSession && status.type !== 'submitting';
  const estimatedValue = positive(quantity) && positive(price) ? Number(quantity) * Number(price) : 0;
  const limitDirection = side === 'BUY' ? 'at or below' : 'at or above';
  const stopDirection = side === 'BUY' ? 'at or above' : 'at or below';
  const paperOrderHelp = orderType === 'LIMIT'
    ? `Fills when the last price is ${limitDirection} your limit.`
    : orderType === 'STOP'
      ? `Triggers when the last price is ${stopDirection} your stop, then fills at market.`
      : orderType === 'STOP_LIMIT'
        ? `Triggers when the last price is ${stopDirection} your stop; then waits for a price ${limitDirection} your limit.`
        : null;

  const isLive = activeBroker?.mode === 'live';
  const errorMessage = (err: unknown) => (typeof err === 'string' ? err : err instanceof Error ? err.message : 'Order could not be submitted');

  const submitOrder = useCallback(async (request: NewOrderRequestDto, confirmationId?: string | null) => {
    setStatus({ type: 'submitting' });
    try {
      const orderId = await placeOrder(request, confirmationId);
      setStatus({ type: 'success', message: `Submitted to ${activeBroker?.display_name ?? 'Paper Trading'}. See Blotter for status. ID: ${orderId}` });
      setQuantity('');
      setPrice('');
      setStopPrice('');
    } catch (err) {
      setStatus({ type: 'error', message: errorMessage(err) });
    }
  }, [activeBroker]);

  const handleSubmit = useCallback(async (e: React.FormEvent) => {
    e.preventDefault();
    if (!canSubmit) return;
    const request: NewOrderRequestDto = {
      symbol: symbol.trim().toUpperCase(), side: side.toLowerCase(), order_type: orderType.toLowerCase(),
      quantity: Number(quantity), price: needsLimit ? Number(price) : null,
      stop_price: needsStop ? Number(stopPrice) : null, broker_id: activeBroker?.broker_id ?? 'paper', tag: null,
      product: activeBroker?.broker_id === 'zerodha' ? product : null,
    };
    if (!isLive) {
      await submitOrder(request);
      return;
    }
    // Live orders are previewed first; the backend only accepts them with the confirmation id.
    setStatus({ type: 'submitting' });
    try {
      const preview = await previewOrder(request);
      if (!preview.confirmation_id) {
        setStatus({ type: 'error', message: preview.warnings.join(' ') || 'This order cannot be confirmed right now.' });
        return;
      }
      setStatus({ type: 'idle' });
      setConfirming({ request, preview });
    } catch (err) {
      setStatus({ type: 'error', message: errorMessage(err) });
    }
  }, [activeBroker, canSubmit, isLive, product, submitOrder, symbol, side, orderType, quantity, price, stopPrice, needsLimit, needsStop]);

  const armLive = useCallback(async () => {
    try {
      setLiveStatus(await setLiveArmed(true));
      setStatus({ type: 'idle' });
    } catch (err) {
      setStatus({ type: 'error', message: errorMessage(err) });
    }
  }, [setLiveStatus]);

  return (
    <form onSubmit={handleSubmit} className="flex h-full min-h-0 flex-col overflow-y-auto p-3" data-testid="order-entry-panel">
      <div className="flex items-center justify-between gap-3">
          <div className="min-w-0">
            <h2 className="text-sm font-semibold text-text-primary">Place an order</h2>
          </div>
          <select
            value={activeBroker?.broker_id ?? 'paper'}
            onChange={(e) => { setActiveBrokerId(e.target.value); setStatus({ type: 'idle' }); }}
            aria-label="Execution account"
            className="min-w-0 max-w-[55%] rounded border border-[var(--border-color)] bg-surface-2 px-2 py-1.5 text-xs text-text-primary focus:border-accent focus:outline-none"
            data-testid="order-broker-select"
          >
            {executionBrokers.map((broker) => (
              <option key={broker.broker_id} value={broker.broker_id}>
                {broker.mode === 'paper' ? 'Paper (Simulated)' : broker.display_name}
              </option>
            ))}
          </select>
      </div>

      <div className="mt-3 grid grid-cols-2 gap-2">
        <div className="grid grid-cols-2 gap-1 rounded bg-surface-0 p-1" role="group" aria-label="Order side">
          {(['BUY', 'SELL'] as const).map((value) => (
            <button key={value} type="button" onClick={() => { setSide(value); setStatus({ type: 'idle' }); }}
              aria-pressed={side === value} data-testid={`order-side-${value.toLowerCase()}`}
              className={`rounded px-2 py-1.5 text-xs font-semibold transition-colors ${side === value ? value === 'BUY' ? 'bg-bull text-on-bull' : 'bg-bear text-on-bear' : 'text-text-muted hover:text-text-primary'}`}>
              {value === 'BUY' ? 'Buy' : 'Sell'}
            </button>
          ))}
        </div>
        <select value={orderType} onChange={(e) => { setOrderType(e.target.value as OrderType); setStatus({ type: 'idle' }); }}
          aria-label="Order type" data-testid="order-type-select"
          className="w-full min-w-0 rounded border border-[var(--border-color)] bg-surface-0 px-2 py-1.5 text-sm text-text-primary focus:border-accent focus:outline-none">
          <option value="MARKET">Market</option><option value="LIMIT">Limit</option><option value="STOP">Stop</option><option value="STOP_LIMIT">Stop Limit</option>
        </select>
      </div>

      <div className="mt-3 grid grid-cols-2 gap-x-2 gap-y-2">
        <label className="min-w-0 text-xs text-text-secondary">Symbol
          <input type="text" value={symbol} onChange={(e) => setSymbol(e.target.value.toUpperCase())}
            aria-label="Symbol" placeholder="Symbol" data-testid="order-symbol-input"
            className="mt-1 w-full min-w-0 rounded border border-[var(--border-color)] bg-surface-0 px-2 py-1.5 font-mono text-sm text-text-primary focus:border-accent focus:outline-none" />
        </label>
        <label className="min-w-0 text-xs text-text-secondary">Quantity
          <input type="number" value={quantity} onChange={(e) => setQuantity(e.target.value)} aria-label="Quantity"
            placeholder="0" data-testid="order-quantity-input" className="mt-1 w-full min-w-0 rounded border border-[var(--border-color)] bg-surface-0 px-2 py-1.5 font-mono text-sm text-text-primary focus:border-accent focus:outline-none"
            step="any" min="0" />
        </label>
          {needsStop && <label className="min-w-0 text-xs text-text-secondary">Stop trigger
            <input type="number" value={stopPrice} onChange={(e) => setStopPrice(e.target.value)} aria-label="Stop trigger price" placeholder="Stop trigger"
              data-testid="order-stop-price-input" className="mt-1 w-full min-w-0 rounded border border-[var(--border-color)] bg-surface-0 px-2 py-1.5 font-mono text-sm text-text-primary focus:border-accent focus:outline-none" step="any" min="0" />
          </label>}
          {needsLimit && <label className="min-w-0 text-xs text-text-secondary">Limit price
            <input type="number" value={price} onChange={(e) => setPrice(e.target.value)} aria-label="Limit price" placeholder="Limit price"
              data-testid="order-price-input" className="mt-1 w-full min-w-0 rounded border border-[var(--border-color)] bg-surface-0 px-2 py-1.5 font-mono text-sm text-text-primary focus:border-accent focus:outline-none" step="any" min="0" />
          </label>}
      </div>
      {activeBroker?.broker_id === 'zerodha' && (
        <label className="mt-2 block text-xs text-text-secondary">Product
          <select value={product} onChange={(e) => setProduct(e.target.value as 'CNC' | 'MIS' | 'NRML')} aria-label="Product" data-testid="order-product-select"
            className="mt-1 w-full rounded border border-[var(--border-color)] bg-surface-0 px-2 py-1.5 text-sm text-text-primary focus:border-accent focus:outline-none">
            <option value="CNC">CNC (delivery)</option><option value="MIS">MIS (intraday)</option><option value="NRML">NRML (F&amp;O carry)</option>
          </select>
        </label>
      )}
      {isLive && (
        <div className={`mt-2 flex items-center justify-between gap-2 border px-2 py-1 text-xs ${liveArmed ? 'border-bear text-bear' : 'border-warning text-warning'}`} data-testid="order-live-banner">
          <span>{liveArmed ? 'LIVE: orders go to the real market.' : 'Live trading is not armed.'}</span>
          {!liveArmed && <button type="button" onClick={() => void armLive()} data-testid="order-arm-live" className="border border-warning px-2 py-0.5 font-semibold hover:bg-warning/20">ARM</button>}
        </div>
      )}
      {needsLimit && <div className="mt-2 flex items-center justify-between text-xs text-text-muted"><span>Estimated value</span><span className="font-mono">{estimatedValue > 0 ? formatPrice(estimatedValue) : '—'}</span></div>}

      {missingLiveSession ? (
        <div className="mt-2 text-xs text-warning" data-testid="order-broker-warning">{activeBroker.display_name} needs a session token before live orders can be sent.</div>
      ) : activeBroker?.mode === 'paper' ? (
        <div className="mt-2 space-y-1 text-xs text-text-muted" data-testid="paper-account-note">
          {paperOrderHelp && <p data-testid="paper-order-trigger">{paperOrderHelp}</p>}
          <p>Paper funds and open orders reset on restart. Prices are used without currency conversion.</p>
        </div>
      ) : null}

      {status.type === 'success' && <div role="status" className="mt-3 break-all text-xs text-bull" data-testid="order-confirmation">{status.message}</div>}
      {status.type === 'error' && <div role="alert" className="mt-3 break-words text-xs text-bear" data-testid="order-error-message">{status.message}</div>}

      <button type="submit" disabled={!canSubmit} data-testid="order-submit-button"
        className={`mt-3 shrink-0 rounded py-2 text-sm font-semibold ${side === 'BUY' ? 'bg-bull text-on-bull hover:brightness-110' : 'bg-bear text-on-bear hover:brightness-110'} transition-colors disabled:cursor-not-allowed disabled:opacity-50`}>
        {status.type === 'submitting' ? 'Submitting…' : `${side === 'BUY' ? 'Buy' : 'Sell'} ${symbol.trim() || '…'}`}
      </button>
      {confirming && (
        <div className="fixed inset-0 z-[90] flex items-center justify-center bg-black/70" role="dialog" aria-modal="true" aria-label="Confirm live order" data-testid="order-confirm-dialog">
          <div className="w-[380px] max-w-[92vw] border border-bear bg-surface-1 p-4">
            <h3 className="text-sm font-semibold uppercase tracking-wider text-bear">Confirm live order</h3>
            <dl className="mt-3 grid grid-cols-[auto_1fr] gap-x-4 gap-y-1 font-mono text-xs">
              <dt className="text-text-muted">Account</dt><dd>{activeBroker?.display_name}</dd>
              <dt className="text-text-muted">Action</dt><dd className={confirming.request.side === 'buy' ? 'text-bull' : 'text-bear'}>{confirming.request.side.toUpperCase()} {confirming.request.quantity} {confirming.request.symbol}</dd>
              <dt className="text-text-muted">Type</dt><dd>{confirming.request.order_type.toUpperCase()}{confirming.request.product ? ` · ${confirming.request.product}` : ''}</dd>
              {confirming.request.price != null && (<><dt className="text-text-muted">Limit</dt><dd>{formatPrice(confirming.request.price)}</dd></>)}
              {confirming.request.stop_price != null && (<><dt className="text-text-muted">Trigger</dt><dd>{formatPrice(confirming.request.stop_price)}</dd></>)}
              <dt className="text-text-muted">Est. value</dt><dd>{confirming.preview.estimated_value != null ? formatPrice(confirming.preview.estimated_value) : '—'}</dd>
            </dl>
            <p className="mt-3 text-[11px] text-text-muted">Confirmation expires in {confirming.preview.expires_in_secs}s.</p>
            <div className="mt-4 flex justify-end gap-2">
              <button type="button" onClick={() => setConfirming(null)} data-testid="order-confirm-cancel"
                className="border border-[var(--border-color)] px-3 py-1.5 text-xs text-text-secondary hover:text-text-primary">Cancel</button>
              <button type="button" autoFocus data-testid="order-confirm-submit"
                onClick={() => { const pendingOrder = confirming; setConfirming(null); void submitOrder(pendingOrder.request, pendingOrder.preview.confirmation_id); }}
                className="border border-bear bg-bear/10 px-3 py-1.5 text-xs font-semibold uppercase tracking-wider text-bear hover:bg-bear/20">
                Send live order
              </button>
            </div>
          </div>
        </div>
      )}
    </form>
  );
};
