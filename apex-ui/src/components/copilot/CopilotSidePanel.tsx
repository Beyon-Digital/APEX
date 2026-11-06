import React, { useCallback, useRef } from 'react';
import { useWorkspaceStore } from '../../stores/workspaceStore';
import { CopilotTile } from './CopilotTile';

const STEP = 24;

/**
 * Focus mode: the Copilot docked on the right, beside the panel you are working in. It takes
 * layout space (nothing is covered) and can be resized by dragging the edge or with the arrow
 * keys when the edge is focused.
 */
export const CopilotSidePanel: React.FC = () => {
  const width = useWorkspaceStore((s) => s.copilotSideWidth);
  const setWidth = useWorkspaceStore((s) => s.setCopilotSideWidth);
  const setOpen = useWorkspaceStore((s) => s.setCopilotSideOpen);
  const drag = useRef<{ startX: number; startWidth: number } | null>(null);

  const onPointerDown = useCallback((event: React.PointerEvent<HTMLDivElement>) => {
    event.currentTarget.setPointerCapture(event.pointerId);
    drag.current = { startX: event.clientX, startWidth: useWorkspaceStore.getState().copilotSideWidth };
  }, []);
  const onPointerMove = useCallback((event: React.PointerEvent<HTMLDivElement>) => {
    if (!drag.current) return;
    // The handle is on the panel's left edge: dragging left makes the panel wider.
    setWidth(drag.current.startWidth + (drag.current.startX - event.clientX));
  }, [setWidth]);
  const endDrag = useCallback(() => { drag.current = null; }, []);
  const onKeyDown = useCallback((event: React.KeyboardEvent) => {
    if (event.key === 'ArrowLeft') { event.preventDefault(); setWidth(width + STEP); }
    if (event.key === 'ArrowRight') { event.preventDefault(); setWidth(width - STEP); }
  }, [setWidth, width]);

  return (
    <aside
      className="relative flex min-h-0 shrink-0 flex-col border-l border-[var(--border-color)] bg-surface-1"
      style={{ width }}
      aria-label="Copilot"
      data-testid="copilot-side-panel"
    >
      <div
        role="separator"
        aria-orientation="vertical"
        aria-valuenow={width}
        aria-valuemin={280}
        aria-valuemax={900}
        aria-label="Resize Copilot panel"
        tabIndex={0}
        onPointerDown={onPointerDown}
        onPointerMove={onPointerMove}
        onPointerUp={endDrag}
        onPointerCancel={endDrag}
        onKeyDown={onKeyDown}
        className="absolute inset-y-0 -left-1 z-10 w-2 cursor-col-resize hover:bg-accent/40 focus:bg-accent/60 focus:outline-none"
        data-testid="copilot-side-resize"
      />
      <div className="flex shrink-0 items-center justify-between border-b border-[var(--border-color)] bg-surface-2 px-2 py-1">
        <span className="text-[11px] font-semibold uppercase tracking-wider text-accent">✦ Copilot</span>
        <button
          type="button"
          onClick={() => setOpen(false)}
          title="Hide the Copilot (Ctrl+Shift+A)"
          aria-label="Hide Copilot"
          data-testid="copilot-side-close"
          className="px-1.5 text-text-muted hover:text-text-primary"
        >
          ×
        </button>
      </div>
      {/* The same header, history and mode switch as the tile: one Copilot, two places. */}
      <div className="min-h-0 flex-1">
        <CopilotTile />
      </div>
    </aside>
  );
};
