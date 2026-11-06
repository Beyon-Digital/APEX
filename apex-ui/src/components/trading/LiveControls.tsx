import React, { useCallback, useEffect, useState } from 'react';
import { killSwitch, setLiveArmed } from '../../lib/tauri';
import { useLiveStore } from '../../stores/liveStore';

/**
 * Live-trading arm toggle and kill switch, shown in the status bar.
 * Ctrl+Shift+K triggers the kill switch from anywhere.
 */
export const LiveControls: React.FC = () => {
  const status = useLiveStore((s) => s.status);
  const setStatus = useLiveStore((s) => s.setStatus);
  const [message, setMessage] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const toggleArmed = useCallback(async () => {
    setBusy(true);
    try {
      setStatus(await setLiveArmed(!status.armed));
      setMessage(null);
    } catch (err) {
      setMessage(err instanceof Error ? err.message : String(err));
    } finally {
      setBusy(false);
    }
  }, [setStatus, status.armed]);

  const stopEverything = useCallback(async () => {
    setBusy(true);
    try {
      const result = await killSwitch();
      setStatus({ ...useLiveStore.getState().status, armed: false, halted: true });
      setMessage(
        result.failed.length > 0
          ? `Kill switch: cancelled ${result.cancelled.length}, ${result.failed.length} failed`
          : `Kill switch: trading halted, ${result.cancelled.length} live order(s) cancelled`,
      );
    } catch (err) {
      setMessage(err instanceof Error ? err.message : String(err));
    } finally {
      setBusy(false);
    }
  }, [setStatus]);

  useEffect(() => {
    const handler = (event: KeyboardEvent) => {
      if ((event.ctrlKey || event.metaKey) && event.shiftKey && event.key.toLowerCase() === 'k') {
        // This chord means "stop everything" app-wide. Swallow it completely so the code
        // editor never also runs its own binding (Delete Line) on the same keypress.
        event.preventDefault();
        event.stopImmediatePropagation();
        void stopEverything();
      }
    };
    // Capture phase on window: the code editor handles Ctrl+Shift+K itself (Delete Line) and
    // stops the event, so a bubbling document listener would never see it there.
    window.addEventListener('keydown', handler, true);
    return () => window.removeEventListener('keydown', handler, true);
  }, [stopEverything]);

  useEffect(() => {
    if (!message) return;
    const timer = setTimeout(() => setMessage(null), 8000);
    return () => clearTimeout(timer);
  }, [message]);

  return (
    <div className="flex items-center gap-2" data-testid="live-controls">
      {message && <span className="text-[10px] text-warning" role="status" data-testid="live-message">{message}</span>}
      <button
        type="button"
        onClick={() => void toggleArmed()}
        disabled={busy}
        aria-pressed={status.armed}
        title={`Live orders today: ${status.orders_today}/${status.max_orders_per_day || '—'} · max qty ${status.max_qty_per_order || '—'}`}
        data-testid="live-arm-toggle"
        className={`border px-2 py-0.5 text-[10px] font-semibold uppercase tracking-wider ${
          status.armed ? 'border-bear bg-bear/20 text-bear' : 'border-[var(--border-color)] text-text-muted hover:text-text-primary'
        }`}
      >
        {status.armed ? 'Live armed' : 'Live off'}
      </button>
      <button
        type="button"
        onClick={() => void stopEverything()}
        disabled={busy}
        title="Halt trading and cancel all working live orders (Ctrl+Shift+K)"
        data-testid="kill-switch"
        className="border border-bear px-2 py-0.5 text-[10px] font-bold uppercase tracking-wider text-bear hover:bg-bear/20"
      >
        Kill
      </button>
    </div>
  );
};
