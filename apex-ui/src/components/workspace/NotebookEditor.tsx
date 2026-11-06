import React, { useCallback, useEffect, useMemo, useState } from 'react';

import {
  createNotebook,
  listNotebooks,
  loadNotebook,
  runNotebookCell,
  saveNotebook,
} from '../../lib/tauri';
import type {
  NotebookCellDto,
  NotebookDocumentDto,
  NotebookSummaryDto,
} from '../../lib/types';
import { DeletedObjects } from '../audit/DeletedObjects';
import { VersionHistory } from '../audit/VersionHistory';

const DEFAULT_NOTEBOOK_PATH = 'research.apexnb.json';

function makeCellId() {
  if (typeof crypto !== 'undefined' && typeof crypto.randomUUID === 'function') {
    return crypto.randomUUID();
  }
  return `cell-${Date.now()}-${Math.random().toString(16).slice(2)}`;
}

function createCell(kind: 'code' | 'markdown'): NotebookCellDto {
  return {
    id: makeCellId(),
    kind,
    content: kind === 'markdown' ? '## Notes\n\nCapture your next hypothesis here.' : 'print("hello from APEX notebook")',
    output: null,
  };
}

export const NotebookEditor: React.FC = () => {
  const [notebooks, setNotebooks] = useState<NotebookSummaryDto[]>([]);
  const [activeNotebook, setActiveNotebook] = useState<NotebookDocumentDto | null>(null);
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [runningCellId, setRunningCellId] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);

  const refreshNotebookList = useCallback(async () => {
    const summaries = await listNotebooks();
    setNotebooks(Array.isArray(summaries) ? summaries : []);
    return summaries;
  }, []);

  const openNotebook = useCallback(async (path: string) => {
    setLoading(true);
    try {
      const notebook = await loadNotebook(path);
      setActiveNotebook(notebook);
      setError(null);
    } catch (err) {
      setError(err instanceof Error ? err.message : 'Unable to load notebook');
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    let cancelled = false;

    (async () => {
      setLoading(true);
      try {
        const summaries = await refreshNotebookList();
        if (cancelled) {
          return;
        }

        if (summaries.length > 0) {
          const notebook = await loadNotebook(summaries[0].path);
          if (!cancelled) {
            setActiveNotebook(notebook);
          }
        } else {
          const created = await createNotebook(DEFAULT_NOTEBOOK_PATH);
          if (!cancelled) {
            setActiveNotebook(created);
            await refreshNotebookList();
          }
        }
        if (!cancelled) {
          setError(null);
        }
      } catch (err) {
        if (!cancelled) {
          setError(err instanceof Error ? err.message : 'Unable to initialize notebook workspace');
        }
      } finally {
        if (!cancelled) {
          setLoading(false);
        }
      }
    })();

    return () => {
      cancelled = true;
    };
  }, [refreshNotebookList]);

  const handleCreateNotebook = useCallback(async () => {
    const response = window.prompt('Notebook file name', `research-${notebooks.length + 1}`);
    if (!response) {
      return;
    }

    setLoading(true);
    try {
      const created = await createNotebook(response.trim());
      setActiveNotebook(created);
      await refreshNotebookList();
      setNotice('Created a fresh research notebook.');
      setError(null);
    } catch (err) {
      setError(err instanceof Error ? err.message : 'Unable to create notebook');
    } finally {
      setLoading(false);
    }
  }, [notebooks.length, refreshNotebookList]);

  const handleSaveNotebook = useCallback(async () => {
    if (!activeNotebook) {
      return;
    }

    setSaving(true);
    try {
      const saved = await saveNotebook(activeNotebook);
      setActiveNotebook(saved);
      await refreshNotebookList();
      setNotice('Notebook saved. Outputs and notes are now persisted.');
      setError(null);
    } catch (err) {
      setError(err instanceof Error ? err.message : 'Unable to save notebook');
      setNotice(null);
    } finally {
      setSaving(false);
    }
  }, [activeNotebook, refreshNotebookList]);

  const updateNotebook = useCallback((updater: (current: NotebookDocumentDto) => NotebookDocumentDto) => {
    setActiveNotebook((current) => current ? updater(current) : current);
    setNotice(null);
  }, []);

  const handleAddCell = useCallback((kind: 'code' | 'markdown') => {
    updateNotebook((current) => ({
      ...current,
      cells: [...current.cells, createCell(kind)],
    }));
  }, [updateNotebook]);

  const handleDeleteCell = useCallback((cellId: string) => {
    updateNotebook((current) => ({
      ...current,
      cells: current.cells.filter((cell) => cell.id !== cellId),
    }));
  }, [updateNotebook]);

  const handleRunCell = useCallback(async (cellId: string) => {
    if (!activeNotebook) {
      return;
    }

    setRunningCellId(cellId);
    try {
      const result = await runNotebookCell({
        path: activeNotebook.path,
        cells: activeNotebook.cells,
        cell_id: cellId,
      });

      updateNotebook((current) => ({
        ...current,
        cells: current.cells.map((cell) => cell.id === cellId
          ? {
              ...cell,
              output: [result.stdout, result.stderr].filter(Boolean).join('\n\n'),
            }
          : cell),
      }));

      setNotice(result.success ? 'Cell executed successfully.' : 'Cell execution finished with errors.');
      setError(result.success ? null : result.stderr || 'Notebook cell execution failed');
    } catch (err) {
      setError(err instanceof Error ? err.message : 'Unable to execute notebook cell');
      setNotice(null);
    } finally {
      setRunningCellId(null);
    }
  }, [activeNotebook, updateNotebook]);

  const notebookStats = useMemo(() => {
    if (!activeNotebook) {
      return null;
    }

    const codeCells = activeNotebook.cells.filter((cell) => cell.kind === 'code').length;
    const markdownCells = activeNotebook.cells.length - codeCells;
    return { codeCells, markdownCells, total: activeNotebook.cells.length };
  }, [activeNotebook]);

  return (
    <div className="h-full flex" data-testid="notebook-editor">
      <aside className="w-64 border-r border-[var(--border-color)] bg-surface-1 p-3 flex flex-col gap-3">
        <div className="flex items-center justify-between gap-2">
          <div>
            <h3 className="text-sm font-medium text-text-primary">Research Notebooks</h3>
            <p className="text-xs text-text-muted">Create, save, and replay your market experiments.</p>
          </div>
          <button
            type="button"
            onClick={() => void handleCreateNotebook()}
            className="rounded bg-primary-500 px-2 py-1 text-xs text-white hover:bg-primary-600"
            data-testid="create-notebook"
          >
            New
          </button>
        </div>

        <div className="space-y-2 overflow-auto" data-testid="notebook-list">
          {notebooks.map((notebook) => {
            const isActive = activeNotebook?.path === notebook.path;
            return (
              <button
                key={notebook.path}
                type="button"
                onClick={() => void openNotebook(notebook.path)}
                className={`w-full rounded border px-3 py-2 text-left text-sm ${isActive ? 'border-primary-500 bg-surface-0 text-text-primary' : 'border-[var(--border-color)] bg-surface-0 text-text-muted hover:text-text-primary'}`}
                data-testid={`notebook-item-${notebook.name}`}
              >
                <div className="font-medium">{notebook.name}</div>
                <div className="mt-1 text-[11px] text-text-muted">{new Date(notebook.updated_at).toLocaleString()}</div>
              </button>
            );
          })}
        </div>
        <DeletedObjects kind="notebook" onRestored={() => void refreshNotebookList()} />
      </aside>

      <div className="flex-1 flex flex-col overflow-hidden">
        <div className="border-b border-[var(--border-color)] bg-surface-1 px-4 py-3 flex flex-wrap items-center justify-between gap-3">
          <div>
            <h3 className="text-sm font-medium text-text-primary">{activeNotebook?.title ?? 'Loading notebook…'}</h3>
            <p className="text-xs text-text-muted">
              {notebookStats
                ? `${notebookStats.total} cells • ${notebookStats.codeCells} code • ${notebookStats.markdownCells} markdown`
                : 'No notebook loaded yet.'}
            </p>
          </div>

          <div className="flex items-center gap-2">
            <button
              type="button"
              onClick={() => handleAddCell('markdown')}
              disabled={!activeNotebook}
              className="rounded border border-[var(--border-color)] bg-surface-0 px-3 py-2 text-xs hover:bg-surface-2 disabled:opacity-50"
            >
              Add Markdown
            </button>
            <button
              type="button"
              onClick={() => handleAddCell('code')}
              disabled={!activeNotebook}
              className="rounded border border-[var(--border-color)] bg-surface-0 px-3 py-2 text-xs hover:bg-surface-2 disabled:opacity-50"
            >
              Add Code Cell
            </button>
            <button
              type="button"
              onClick={() => void handleSaveNotebook()}
              disabled={!activeNotebook || saving}
              className="rounded bg-primary-500 px-3 py-2 text-xs text-white hover:bg-primary-600 disabled:opacity-50"
              data-testid="save-notebook"
            >
              {saving ? 'Saving…' : 'Save Notebook'}
            </button>
            {activeNotebook && (
              <VersionHistory
                kind="notebook" id={activeNotebook.path}
                onRestored={() => { void refreshNotebookList(); void openNotebook(activeNotebook.path); setNotice('Restored as a new version. Nothing was overwritten.'); }}
                className="rounded border border-[var(--border-color)] bg-surface-0 px-3 py-2 text-xs hover:bg-surface-2"
              />
            )}
          </div>
        </div>

        <div className="flex-1 overflow-auto p-4 space-y-4 bg-surface-0">
          {notice && (
            <div className="rounded border border-bull/30 bg-bull/10 px-3 py-2 text-xs text-bull">
              {notice}
            </div>
          )}
          {error && (
            <div className="rounded border border-bear/30 bg-bear/10 px-3 py-2 text-xs text-bear">
              {error}
            </div>
          )}
          {loading ? (
            <div className="rounded border border-[var(--border-color)] bg-surface-1 px-4 py-6 text-sm text-text-muted">
              Loading notebook workspace…
            </div>
          ) : !activeNotebook ? (
            <div className="rounded border border-[var(--border-color)] bg-surface-1 px-4 py-6 text-sm text-text-muted">
              No notebook is currently loaded.
            </div>
          ) : (
            activeNotebook.cells.map((cell, index) => (
              <div key={cell.id} className="rounded border border-[var(--border-color)] bg-surface-1" data-testid={`notebook-cell-${index}`}>
                <div className="flex flex-wrap items-center justify-between gap-3 border-b border-[var(--border-color)] px-3 py-2">
                  <div className="flex items-center gap-2">
                    <span className="text-xs font-mono text-text-muted">Cell {index + 1}</span>
                    <select
                      value={cell.kind}
                      onChange={(event) => {
                        const nextKind = event.target.value as 'code' | 'markdown';
                        updateNotebook((current) => ({
                          ...current,
                          cells: current.cells.map((entry) => entry.id === cell.id ? { ...entry, kind: nextKind } : entry),
                        }));
                      }}
                      className="rounded border border-[var(--border-color)] bg-surface-0 px-2 py-1 text-xs text-text-primary"
                    >
                      <option value="markdown">Markdown</option>
                      <option value="code">Code</option>
                    </select>
                  </div>

                  <div className="flex items-center gap-2">
                    {cell.kind === 'code' && (
                      <button
                        type="button"
                        onClick={() => void handleRunCell(cell.id)}
                        disabled={runningCellId === cell.id}
                        className="rounded border border-[var(--border-color)] bg-surface-0 px-3 py-1 text-xs hover:bg-surface-2 disabled:opacity-50"
                        data-testid={`run-notebook-cell-${index}`}
                      >
                        {runningCellId === cell.id ? 'Running…' : 'Run'}
                      </button>
                    )}
                    <button
                      type="button"
                      onClick={() => handleDeleteCell(cell.id)}
                      disabled={activeNotebook.cells.length <= 1}
                      className="rounded border border-[var(--border-color)] bg-surface-0 px-3 py-1 text-xs hover:bg-surface-2 disabled:opacity-50"
                    >
                      Delete
                    </button>
                  </div>
                </div>

                <div className="p-3 space-y-3">
                  <textarea
                    value={cell.content}
                    onChange={(event) => {
                      updateNotebook((current) => ({
                        ...current,
                        cells: current.cells.map((entry) => entry.id === cell.id ? { ...entry, content: event.target.value } : entry),
                      }));
                    }}
                    className={`w-full min-h-[120px] rounded border border-[var(--border-color)] bg-surface-0 px-3 py-2 text-sm text-text-primary focus:outline-none ${cell.kind === 'code' ? 'font-mono' : ''}`}
                    spellCheck={cell.kind !== 'code'}
                  />

                  {cell.kind === 'code' && (
                    <div className="rounded border border-[var(--border-color)] bg-black/20 px-3 py-2">
                      <div className="mb-2 text-[11px] uppercase tracking-wide text-text-muted">Output</div>
                      <pre className="whitespace-pre-wrap text-xs font-mono text-text-primary">
                        {cell.output?.trim() || 'No output yet — run the cell to capture stdout/stderr.'}
                      </pre>
                    </div>
                  )}
                </div>
              </div>
            ))
          )}
        </div>
      </div>
    </div>
  );
};
