import { create } from 'zustand';
import type { LiveStatusDto } from '../lib/types';

interface LiveState {
  status: LiveStatusDto;
  setStatus: (status: LiveStatusDto) => void;
}

export const useLiveStore = create<LiveState>((set) => ({
  status: { armed: false, orders_today: 0, max_orders_per_day: 0, max_qty_per_order: 0, halted: false },
  setStatus: (status) => set({ status }),
}));
