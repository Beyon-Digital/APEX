/**
 * Tauri Event Bridge Hooks
 *
 * These hooks bridge Tauri's real-time event system with React state.
 * When running in the browser (no Tauri), they fall back to polling.
 */
import { useEffect, useRef } from 'react';
import { useMarketStore } from '../stores/marketStore';
import { useOrderStore } from '../stores/orderStore';
import { useHealthStore } from '../stores/healthStore';
import { useNewsStore } from '../stores/newsStore';
import type { QuoteDto, OrderDto, PositionDto, SystemHealthDto, NewsItemDto, AlertDto } from '../lib/types';

const IS_TAURI = typeof window !== 'undefined' && '__TAURI__' in window;

type UnlistenFn = () => void;

/**
 * Subscribe to real-time quote updates from Tauri events.
 * Falls back to no-op in browser mode (useDataSync handles polling).
 */
export function useQuoteStream() {
  const updateQuote = useMarketStore((s) => s.updateQuote);
  const unlistenRef = useRef<UnlistenFn | null>(null);

  useEffect(() => {
    if (!IS_TAURI) return;

    let mounted = true;

    (async () => {
      try {
        const { listen } = await import('@tauri-apps/api/event');
        const unlisten = await listen<QuoteDto>('quote-update', (event) => {
          if (mounted && event.payload?.symbol) {
            updateQuote(event.payload);
          }
        });
        unlistenRef.current = unlisten;
      } catch {
        // Tauri event API not available
      }
    })();

    return () => {
      mounted = false;
      unlistenRef.current?.();
    };
  }, [updateQuote]);
}

/**
 * Subscribe to real-time order update events from Tauri.
 */
export function useOrderStream() {
  const setOrders = useOrderStore((s) => s.setOrders);
  const unlistenRef = useRef<UnlistenFn | null>(null);

  useEffect(() => {
    if (!IS_TAURI) return;

    let mounted = true;

    (async () => {
      try {
        const { listen } = await import('@tauri-apps/api/event');
        const unlisten = await listen<OrderDto>('order-update', (event) => {
          if (!mounted || !event.payload?.id) return;
          // Merge into the live store — event payloads must not clobber rows
          // seeded by the orders poller/initial fetch.
          const current = useOrderStore.getState().openOrders;
          const idx = current.findIndex((o) => o.id === event.payload.id);
          const next = [...current];
          if (idx >= 0) {
            next[idx] = event.payload;
          } else {
            next.push(event.payload);
          }
          setOrders(next);
        });
        unlistenRef.current = unlisten;
      } catch {
        // Tauri event API not available
      }
    })();

    return () => {
      mounted = false;
      unlistenRef.current?.();
    };
  }, [setOrders]);
}

/**
 * Subscribe to position update events from Tauri.
 */
export function usePositionStream() {
  const setPositions = useOrderStore((s) => s.setPositions);
  const unlistenRef = useRef<UnlistenFn | null>(null);

  useEffect(() => {
    if (!IS_TAURI) return;

    let mounted = true;

    (async () => {
      try {
        const { listen } = await import('@tauri-apps/api/event');
        const unlisten = await listen<PositionDto>('position-update', (event) => {
          if (!mounted || !event.payload?.symbol) return;
          const current = useOrderStore.getState().positions;
          const idx = current.findIndex((p) => p.symbol === event.payload.symbol);
          const next = [...current];
          if (idx >= 0) {
            next[idx] = event.payload;
          } else {
            next.push(event.payload);
          }
          setPositions(next.filter((position) => position.quantity !== 0));
        });
        unlistenRef.current = unlisten;
      } catch {
        // Tauri event API not available
      }
    })();

    return () => {
      mounted = false;
      unlistenRef.current?.();
    };
  }, [setPositions]);
}

/**
 * Subscribe to adapter health events from Tauri.
 */
export function useHealthStream() {
  const setHealth = useHealthStore((s) => s.setHealth);
  const unlistenRef = useRef<UnlistenFn | null>(null);

  useEffect(() => {
    if (!IS_TAURI) return;

    let mounted = true;

    (async () => {
      try {
        const { listen } = await import('@tauri-apps/api/event');
        const unlisten = await listen<SystemHealthDto>('adapter-health', (event) => {
          if (mounted && event.payload) {
            setHealth(event.payload);
          }
        });
        unlistenRef.current = unlisten;
      } catch {
        // Tauri event API not available
      }
    })();

    return () => {
      mounted = false;
      unlistenRef.current?.();
    };
  }, [setHealth]);
}

/**
 * Subscribe to news items pushed from the Rust news engine.
 */
export function useNewsStream() {
  const addItem = useNewsStore((s) => s.addItem);
  const unlistenRef = useRef<UnlistenFn | null>(null);

  useEffect(() => {
    if (!IS_TAURI) return;

    let mounted = true;

    (async () => {
      try {
        const { listen } = await import('@tauri-apps/api/event');
        const unlisten = await listen<NewsItemDto>('news-item', (event) => {
          if (mounted && event.payload?.id) {
            addItem(event.payload);
          }
        });
        unlistenRef.current = unlisten;
      } catch {
        // Tauri event API not available
      }
    })();

    return () => {
      mounted = false;
      unlistenRef.current?.();
    };
  }, [addItem]);
}

/**
 * Subscribe to fired-alert events pushed by the alert engine.
 */
export function useAlertStream() {
  const addFiredAlert = useNewsStore((s) => s.addFiredAlert);
  const unlistenRef = useRef<UnlistenFn | null>(null);

  useEffect(() => {
    if (!IS_TAURI) return;

    let mounted = true;

    (async () => {
      try {
        const { listen } = await import('@tauri-apps/api/event');
        const unlisten = await listen<AlertDto>('alert-fired', (event) => {
          if (mounted && event.payload?.rule_id) {
            addFiredAlert(event.payload);
          }
        });
        unlistenRef.current = unlisten;
      } catch {
        // Tauri event API not available
      }
    })();

    return () => {
      mounted = false;
      unlistenRef.current?.();
    };
  }, [addFiredAlert]);
}

/**
 * Master hook that sets up all Tauri event streams.
 * Call once from the app root to initialize all real-time subscriptions.
 */
export function useTauriEventBridge() {
  useQuoteStream();
  useOrderStream();
  usePositionStream();
  useHealthStream();
  useNewsStream();
  useAlertStream();
}
