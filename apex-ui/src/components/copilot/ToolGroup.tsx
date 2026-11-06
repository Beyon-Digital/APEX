import React, { useState } from 'react';
import type { ToolCallTraceDto } from '../../lib/types';
import { openItem } from '../../stores/navStore';
import { TOOL_LABELS } from './toolLabels';

type Artifact = NonNullable<ToolCallTraceDto['artifact']>;

const KIND_LABEL: Record<Artifact['kind'], string> = {
  strategy: 'Strategy',
  model: 'Model',
  dataset: 'Dataset',
  automation: 'Automation',
};

const OPEN_TITLE: Record<Artifact['kind'], string> = {
  strategy: 'Open in the Strategy IDE',
  model: 'Open in the ML Workbench',
  dataset: 'Open in the ML Workbench',
  automation: 'Open in Automations',
};

/** Collapsible "Used N tools" group. Things the Copilot created are listed below it with an Open button. */
export const ToolGroup: React.FC<{ calls: ToolCallTraceDto[] }> = ({ calls }) => {
  const [open, setOpen] = useState(false);
  const failed = calls.filter((c) => !c.ok).length;

  const artifacts: Artifact[] = [];
  for (const c of calls) {
    const a = c.artifact;
    if (a && !artifacts.some((x) => x.kind === a.kind && x.id === a.id)) artifacts.push(a);
  }

  return (
    <div className="mb-2 space-y-1" data-testid="copilot-tool-trace">
      <button
        type="button"
        onClick={() => setOpen((v) => !v)}
        aria-expanded={open}
        className="flex items-center gap-1.5 font-mono text-[11px] text-text-muted hover:text-text-primary"
        data-testid="copilot-tools-toggle"
      >
        <span className="text-accent">{open ? '▾' : '▸'}</span>
        <span>Used {calls.length} {calls.length === 1 ? 'tool' : 'tools'}</span>
        {failed > 0 && <span className="text-bear">· {failed} failed</span>}
      </button>

      {open && (
        <div className="space-y-0.5" data-testid="copilot-tool-list">
          {calls.map((c, i) => (
            <div
              key={i}
              className={`flex items-start gap-1.5 border px-1.5 py-0.5 font-mono text-[10px] ${
                c.ok ? 'border-[var(--border-color)] text-text-muted' : 'border-bear/40 text-bear'
              }`}
              data-testid="copilot-tool-call"
              data-ok={c.ok ? 'true' : 'false'}
            >
              <span className={c.ok ? 'text-bull' : 'text-bear'}>{c.ok ? '✓' : '✕'}</span>
              <span className="shrink-0 font-medium">{TOOL_LABELS[c.name] ?? c.name}</span>
              {c.detail && <span className="min-w-0 break-words opacity-80">{c.detail}</span>}
            </div>
          ))}
        </div>
      )}

      {artifacts.map((a) => (
        <div
          key={`${a.kind}:${a.id}`}
          className="flex items-center gap-2 border border-accent/40 bg-accent/5 px-2 py-1 text-[11px]"
          data-testid="copilot-artifact"
        >
          <span className="font-mono text-[10px] uppercase tracking-wider text-accent">{KIND_LABEL[a.kind]}</span>
          <span className="min-w-0 flex-1 truncate font-mono text-text-secondary" title={a.id}>{a.id}</span>
          <button
            type="button"
            onClick={() => openItem(a.kind, a.id)}
            title={OPEN_TITLE[a.kind]}
            className="shrink-0 border border-accent/60 px-2 py-px font-mono text-[10px] text-accent hover:bg-accent/20"
            data-testid="copilot-open-artifact"
            data-kind={a.kind}
            data-id={a.id}
          >
            Open
          </button>
        </div>
      ))}
    </div>
  );
};
