import React, { useEffect } from 'react';
import { Workspace } from './components/workspace/Workspace';
import { CommandBar } from './components/workspace/CommandBar';
import { StatusBar } from './components/workspace/StatusBar';
import { TickerTape } from './components/workspace/TickerTape';
import { KeyboardHud } from './components/workspace/KeyboardHud';
import { VaultGate } from './components/settings/VaultGate';
import { useDataSync } from './lib/useDataSync';
import { useTauriEventBridge } from './hooks/useTauriEvents';
import { useArtifactEvents } from './hooks/useArtifactEvents';
import { useWorkspaceStore } from './stores/workspaceStore';
import { syncAppearance } from './lib/appearance';

export default function App() {
  useDataSync();
  useTauriEventBridge();
  useArtifactEvents();

  useEffect(() => {
    void syncAppearance();
  }, []);

  const setCommandSymbol = useWorkspaceStore((s) => s.setCommandSymbol);
  const setCommandTab = useWorkspaceStore((s) => s.setCommandTab);

  return (
    <div className="relative flex flex-col h-screen bg-surface-0">
      <CommandBar onSelectSymbol={setCommandSymbol} onSwitchTab={setCommandTab} />
      <TickerTape />
      <main className="flex-1 overflow-hidden">
        <Workspace />
      </main>
      <StatusBar />
      <KeyboardHud />
      <VaultGate />
    </div>
  );
}
