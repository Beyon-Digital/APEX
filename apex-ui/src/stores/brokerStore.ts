import { create } from 'zustand';
import { persist } from 'zustand/middleware';
import type { BrokerConnectionDto } from '../lib/types';

const DEFAULT_CONNECTIONS: BrokerConnectionDto[] = [
  {
    broker_id: 'paper',
    display_name: 'Paper Trading',
    mode: 'paper',
    status: 'ready',
    configured: true,
    authenticated: true,
    execution_available: true,
    market_data_available: false,
    token_field_label: '',
    message: 'Paper trading is active for safe simulated execution.',
    can_short: true,
  },
];

interface BrokerState {
  activeBrokerId: string;
  connections: BrokerConnectionDto[];
  setConnections: (connections: BrokerConnectionDto[]) => void;
  setActiveBrokerId: (brokerId: string) => void;
  updateConnection: (connection: BrokerConnectionDto) => void;
}

const isSelectableExecutionBroker = (broker: BrokerConnectionDto) => broker.mode === 'paper' || broker.execution_available;

export const useBrokerStore = create<BrokerState>()(
  persist(
    (set) => ({
      activeBrokerId: 'paper',
      connections: DEFAULT_CONNECTIONS,

      setConnections: (connections) => set((state) => {
        const nextConnections = connections.length > 0 ? connections : DEFAULT_CONNECTIONS;
        const selectable = new Set(nextConnections.filter(isSelectableExecutionBroker).map((broker) => broker.broker_id));
        const nextActiveBroker = selectable.has(state.activeBrokerId) ? state.activeBrokerId : 'paper';

        return {
          connections: nextConnections,
          activeBrokerId: nextActiveBroker,
        };
      }),

      setActiveBrokerId: (brokerId) => set((state) => {
        const selectable = new Set(state.connections.filter(isSelectableExecutionBroker).map((broker) => broker.broker_id));
        return selectable.has(brokerId)
          ? { activeBrokerId: brokerId }
          : { activeBrokerId: state.activeBrokerId };
      }),

      updateConnection: (connection) => set((state) => {
        const existingIndex = state.connections.findIndex((broker) => broker.broker_id === connection.broker_id);
        const connections = existingIndex >= 0
          ? state.connections.map((broker, index) => index === existingIndex ? connection : broker)
          : [...state.connections, connection];

        return { connections };
      }),
    }),
    {
      name: 'broker-storage',
      partialize: (state) => ({
        activeBrokerId: state.activeBrokerId,
      }),
    },
  ),
);