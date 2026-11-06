import React, { useCallback, useEffect, useRef, useState } from 'react';
import { useArtifactStore } from '../../stores/artifactStore';
import { useMLStore } from '../../stores/mlStore';
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

  const modelVersion = useArtifactStore((s) => s.versions.model);
  const datasetVersion = useArtifactStore((s) => s.versions.dataset);
  const navRequest = useNavStore((s) => s.request);
  const clearNav = useNavStore((s) => s.clear);

  useEffect(() => { void loadCatalog(); }, [loadCatalog]);
  useEffect(() => { void refreshModels(); }, [refreshModels, modelVersion]);
  useEffect(() => { void refreshDatasets(); }, [refreshDatasets, datasetVersion]);

  useEffect(() => {
    if (!navRequest) return;
    if (navRequest.kind === 'model') {
      setModelRequest({ id: navRequest.id, nonce: navRequest.nonce });
      setTab('models');
      clearNav(navRequest.nonce);
    } else if (navRequest.kind === 'dataset') {
      setDatasetRequest({ name: navRequest.id, nonce: navRequest.nonce });
      setTab('data');
      clearNav(navRequest.nonce);
    }
  }, [navRequest, clearNav]);

  // A navigation request is for one visit to the Models tab. Leaving the tab unmounts it, so drop
  // the request or the next visit would select the old model again.
  const previousTab = useRef(tab);
  useEffect(() => {
    if (previousTab.current === 'models' && tab !== 'models') setModelRequest(null);
    previousTab.current = tab;
  }, [tab]);

  const onRetrain = useCallback((detail: ModelDetailDto) => {
    prefillFromModel(detail);
    setTab('train');
  }, [prefillFromModel]);

  const useDatasetForTraining = useCallback((name: string) => {
    patchSettings({ mode: 'dataset', dataset: name, selectedFeatures: null });
    setTab('train');
  }, [patchSettings]);

  useEffect(() => {
    if (typeof window === 'undefined' || '__TAURI__' in window) return;
    const hook = {
      retrain: async (modelId: string) => onRetrain(await getMlModel(modelId)),
      /** Same as another panel calling `openItem(kind, id)`. */
      open: (kind: NavKind, id: string) => openItem(kind, id),
    };
    (window as unknown as { __apexMlTest?: typeof hook }).__apexMlTest = hook;
    return () => { delete (window as unknown as { __apexMlTest?: typeof hook }).__apexMlTest; };
  }, [onRetrain]);

  return (
    <div className="h-full flex flex-col" data-testid="ml-workbench">
      <div className="flex items-center gap-1 px-3 py-2 border-b border-[var(--border-color)] bg-surface-1" role="tablist">
        {TABS.map((t) => (
          <button
            key={t.id}
            type="button"
            role="tab"
            aria-selected={tab === t.id}
            onClick={() => setTab(t.id)}
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

      <div className="flex-1 overflow-auto p-3">
        <div className={tab === 'data' ? '' : 'hidden'} data-testid="ml-panel-data">
          <DatasetBuilder focusDataset={datasetRequest} onUseForTraining={useDatasetForTraining} />
        </div>
        <div className={tab === 'train' ? '' : 'hidden'} data-testid="ml-panel-train">
          <TrainingForm onOpenData={() => setTab('data')} />
        </div>
        {tab === 'models' && (
          <ModelsTab
            key={modelRequest?.nonce ?? 0}
            initialModelId={modelRequest?.id}
            onRetrain={onRetrain}
            onOpenTrain={() => setTab('train')}
          />
        )}
      </div>
    </div>
  );
});

MLWorkbench.displayName = 'MLWorkbench';
