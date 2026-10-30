import { create } from 'zustand';
import type { VaultStatus } from '../lib/types';

interface VaultState {
  /** null until the first status check returns. */
  status: VaultStatus | null;
  /** The user chose to continue without unlocking (paper trading + free data only). */
  dismissed: boolean;
  setStatus: (status: VaultStatus) => void;
  dismiss: () => void;
}

export const useVaultStore = create<VaultState>((set) => ({
  status: null,
  dismissed: false,
  setStatus: (status) => set((state) => ({ status, dismissed: status === 'unlocked' ? false : state.dismissed })),
  dismiss: () => set({ dismissed: true }),
}));
