import React, { useCallback, useEffect, useRef, useState } from 'react';
import { useArtifactStore } from '../../stores/artifactStore';
import { isBuilderDirty, useMLStore } from '../../stores/mlStore';
import { openItem, useNavStore, type NavKind } from '../../stores/navStore';
import { getMlModel } from '../../lib/tauri';
import type { ModelDetailDto } from '../../lib/types';
import { DatasetBuilder } from './DatasetBuilder';
import { ModelsTab } from './ModelsTab';
import { TrainingForm } from './TrainingForm';

type Tab = 'data' | 'train' | 'models';

const TABS: { id: Tab; label: string }[] = [
  { id: 'data', label: 'Data' },
  { id: 'train', label: 'Train' },
  { id: 'models', label: 'Models' },
];

/**
 * ML workbench shell: Data (build datasets), Train (configure and run training), Models (registry).
 *
 * The Data and Train tabs stay mounted while hidden so a half-filled form survives a tab switch;
 * the running job itself lives in `mlStore`, so it also survives the whole workbench unmounting.
 *
 * Test hooks (browser mode only): `window.__apexMlTest.retrain(modelId)` runs the same handler the
 * Models tab calls for "Train again", so specs can check the pre-fill without the Models UI, and
 * `window.__apexMlTest.open(kind, id)` navigates like another panel would.
 */
export const MLWorkbench: React.FC = React.memo(() => {
  const [tab, setTab] = useState<Tab>('data');
  const [modelRequest, setModelRequest] = useState<{ id: string; nonce: number } | null>(null);
  const [datasetRequest, setDatasetRequest] = useState<{ name: string; nonce: number } | null>(null);

  const modelCount = useMLStore((s) => s.models.length);
  const isTraining = useMLStore((s) => s.isTraining);
  const refreshModels = useMLStore((s) => s.refreshModels);
  const refreshDatasets = useMLStore((s) => s.refreshDatasets);
  const loadCatalog = useMLStore((s) => s.loadCatalog);
  const prefillFromModel = useMLStore((s) => s.prefillFromModel);
  const patchSettings = useMLStore((s) => s.patchSettings);
  const refreshRecipes = useMLStore((s) => s.refreshRecipes);
  const builderDirty = useMLStore((s) => isBuilderDirty(s.builder));
  const closeBuilder = useMLStore((s) => s.closeBuilder);
  /** A move to another tab that waits for the user to confirm leaving unsaved algorithm changes. */
  const [pendingLeave, setPendingLeave] = useState<{ tab: Tab; run?: () => void } | null>(null);

  const modelVersion = useArtifactStore((s) => s.versions.model);
  const datasetVersion = useArtifactStore((s) => s.versions.dataset);
  const navRequest = useNavStore((s) => s.request);
  const clearNav = useNavStore((s) => s.clear);

  useEffect(() => { void loadCatalog(); }, [loadCatalog]);
  useEffect(() => { void refreshModels(); }, [refreshModels, modelVersion]);
  useEffect(() => { void refreshDatasets(); }, [refreshDatasets, datasetVersion]);
  useEffect(() => { if (tab === 'train') void refreshRecipes(); }, [tab, refreshRecipes]);

  /** Switch tabs, asking first when that would leave an unsaved algorithm draft behind. */
  const go = useCallback((next: Tab, run?: () => void) => {
    if (builderDirty && tab === 'train' && next !== 'train') { setPendingLeave({ tab: next, run }); return; }
    run?.();
    setTab(next);
  }, [builderDirty, tab]);

  useEffect(() => {
    if (!navRequest) return;
    if (navRequest.kind === 'model') {
      const { id, nonce } = navRequest;
      go('models', () => setModelRequest({ id, nonce }));
      clearNav(navRequest.nonce);
    } else if (navRequest.kind === 'dataset') {
      const { id: name, nonce } = navRequest;
      go('data', () => setDatasetRequest({ name, nonce }));
      clearNav(navRequest.nonce);
    }
  }, [navRequest, clearNav, go]);

  // A navigation request is for one visit to the Models tab. Leaving the tab unmounts it, so drop
  // the request or the next visit would select the old model again.
  const previousTab = useRef(tab);
  useEffect(() => {
    if (previousTab.current === 'models' && tab !== 'models') setModelRequest(null);
    previousTab.current = tab;
  }, [tab]);

  /** An open builder replaces the whole Train form, so a clean one is closed to show the form that was just filled in. */
  const showTrainForm = useCallback(() => {
    const s = useMLStore.getState();
    if (!isBuilderDirty(s.builder)) s.closeBuilder();
    setTab('train');
  }, []);
  const onRetrain = useCallback((detail: ModelDetailDto) => {
    prefillFromModel(detail);
    showTrainForm();
  }, [prefillFromModel, showTrainForm]);
  const leaveConfirmed = () => {
    const pending = pendingLeave;
    setPendingLeave(null);
    if (!pending) return;
    closeBuilder();
    pending.run?.();
    setTab(pending.tab);
  };

  const useDatasetForTraining = useCallback((name: string) => {
    patchSettings({ mode: 'dataset', dataset: name, selectedFeatures: null });
    showTrainForm();
  }, [patchSettings, showTrainForm]);

  useEffect(() => {
    if (typeof window === 'undefined' || '__TAURI__' in window) return;
    const hook = {
      retrain: async (modelId: string) => onRetrain(await getMlModel(modelId)),
      /** Same as another panel calling `openItem(kind, id)`. */
      open: (kind: NavKind, id: string) => openItem(kind, id),
      /** Reload the saved recipes (specs seed some through `__apexMock` first). */
      refreshRecipes: () => refreshRecipes(),
    };
    (window as unknown as { __apexMlTest?: typeof hook }).__apexMlTest = hook;
    return () => { delete (window as unknown as { __apexMlTest?: typeof hook }).__apexMlTest; };
  }, [onRetrain, refreshRecipes]);

  return (
    <div className="h-full flex flex-col" data-testid="ml-workbench">
      <div className="flex items-center gap-1 px-3 py-2 border-b border-[var(--border-color)] bg-surface-1" role="tablist">
        {TABS.map((t) => (
          <button
            key={t.id}
            type="button"
            role="tab"
            aria-selected={tab === t.id}
            onClick={() => go(t.id)}
            data-testid={`ml-tab-${t.id}`}
            className={`px-3 py-1 text-xs font-mono border transition-colors ${
              tab === t.id
                ? 'border-accent bg-accent/15 text-accent'
                : 'border-transparent bg-surface-2 text-text-muted hover:text-text-primary'
            }`}
          >
            {t.label}
            {t.id === 'models' && ` (${modelCount})`}
            {t.id === 'train' && isTraining && <span className="ml-1.5 animate-pulse text-accent" data-testid="ml-train-running">●</span>}
          </button>
        ))}
      </div>

      {pendingLeave && (
        <div className="flex flex-wrap items-center gap-2 border-b border-warning/40 bg-warning/10 px-3 py-1.5 text-xs text-warning" data-testid="ml-leave-guard" role="alertdialog" aria-label="Unsaved algorithm changes">
          You have unsaved changes in the algorithm builder. Leave and lose them?
          <button type="button" className="px-2 py-0.5 text-xs font-mono border border-[var(--border-color)] bg-surface-2 text-text-primary hover:border-accent" onClick={leaveConfirmed} data-testid="ml-leave-confirm">Leave</button>
          <button type="button" className="px-2 py-0.5 text-xs font-mono border border-[var(--border-color)] bg-surface-2 text-text-primary hover:border-accent" onClick={() => setPendingLeave(null)} data-testid="ml-leave-stay">Stay</button>
        </div>
      )}

      <div className="flex-1 overflow-auto p-3">
        <div className={tab === 'data' ? '' : 'hidden'} data-testid="ml-panel-data">
          <DatasetBuilder focusDataset={datasetRequest} onUseForTraining={useDatasetForTraining} />
        </div>
        <div className={tab === 'train' ? '' : 'hidden'} data-testid="ml-panel-train">
          <TrainingForm onOpenData={() => go('data')} />
        </div>
        {tab === 'models' && (
          <ModelsTab
            key={modelRequest?.nonce ?? 0}
            initialModelId={modelRequest?.id}
            onRetrain={onRetrain}
            onOpenTrain={() => go('train')}
          />
        )}
      </div>
    </div>
  );
});

MLWorkbench.displayName = 'MLWorkbench';
