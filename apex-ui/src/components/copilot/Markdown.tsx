import React, { useState } from 'react';

/**
 * Small, safe markdown renderer for Copilot replies.
 *
 * It builds React elements directly and never uses dangerouslySetInnerHTML, so any HTML in the
 * model text is shown as plain text. Supported: headings, paragraphs, bold, italic, inline code,
 * fenced code blocks (with a Copy button), bullet and numbered lists, simple tables, block quotes
 * and links (http/https only).
 */

const SAFE_URL = /^https?:\/\//i;

type Block =
  | { type: 'heading'; level: number; text: string }
  | { type: 'code'; lang: string; code: string }
  | { type: 'ul'; items: string[] }
  | { type: 'ol'; items: string[] }
  | { type: 'table'; head: string[]; rows: string[][] }
  | { type: 'quote'; text: string }
  | { type: 'hr' }
  | { type: 'p'; text: string };

const FENCE = /^\s*(```+|~~~+)\s*([\w+-]*)\s*$/;
const HEADING = /^(#{1,6})\s+(.*?)\s*#*\s*$/;
const UL = /^\s*[-*+]\s+(.*)$/;
const OL = /^\s*\d+[.)]\s+(.*)$/;
const TABLE_SEP = /^\s*\|?\s*:?-{2,}:?\s*(\|\s*:?-{2,}:?\s*)*\|?\s*$/;

function splitRow(line: string): string[] {
  let t = line.trim();
  if (t.startsWith('|')) t = t.slice(1);
  if (t.endsWith('|')) t = t.slice(0, -1);
  return t.split('|').map((c) => c.trim());
}

function parseBlocks(src: string): Block[] {
  const lines = src.replace(/\r\n?/g, '\n').split('\n');
  const blocks: Block[] = [];
  let i = 0;
  while (i < lines.length) {
    const line = lines[i];
    if (!line.trim()) { i += 1; continue; }

    const fence = FENCE.exec(line);
    if (fence) {
      const marker = fence[1];
      const body: string[] = [];
      i += 1;
      // An unterminated fence (reply cut off) still renders as code up to the end.
      while (i < lines.length && !lines[i].trim().startsWith(marker)) { body.push(lines[i]); i += 1; }
      i += 1;
      blocks.push({ type: 'code', lang: fence[2], code: body.join('\n') });
      continue;
    }

    const heading = HEADING.exec(line);
    if (heading) { blocks.push({ type: 'heading', level: heading[1].length, text: heading[2] }); i += 1; continue; }

    if (/^\s*([-*_])\s*(\1\s*){2,}$/.test(line)) { blocks.push({ type: 'hr' }); i += 1; continue; }

    if (line.includes('|') && i + 1 < lines.length && TABLE_SEP.test(lines[i + 1]) && lines[i + 1].includes('-')) {
      const head = splitRow(line);
      const rows: string[][] = [];
      i += 2;
      while (i < lines.length && lines[i].trim() && lines[i].includes('|')) { rows.push(splitRow(lines[i])); i += 1; }
      blocks.push({ type: 'table', head, rows });
      continue;
    }

    if (UL.test(line)) {
      const items: string[] = [];
      while (i < lines.length && UL.test(lines[i])) { items.push(UL.exec(lines[i])![1]); i += 1; }
      blocks.push({ type: 'ul', items });
      continue;
    }
    if (OL.test(line)) {
      const items: string[] = [];
      while (i < lines.length && OL.test(lines[i])) { items.push(OL.exec(lines[i])![1]); i += 1; }
      blocks.push({ type: 'ol', items });
      continue;
    }

    if (/^\s*>/.test(line)) {
      const quote: string[] = [];
      while (i < lines.length && /^\s*>/.test(lines[i])) { quote.push(lines[i].replace(/^\s*>\s?/, '')); i += 1; }
      blocks.push({ type: 'quote', text: quote.join('\n') });
      continue;
    }

    const para: string[] = [];
    while (
      i < lines.length && lines[i].trim() && !FENCE.test(lines[i]) && !HEADING.test(lines[i])
      && !UL.test(lines[i]) && !OL.test(lines[i]) && !/^\s*>/.test(lines[i])
    ) { para.push(lines[i]); i += 1; }
    blocks.push({ type: 'p', text: para.join('\n') });
  }
  return blocks;
}

// Earliest match wins; each pattern is tried at every position via a single combined scan.
const INLINE = /(`+)([^`]|[^`][\s\S]*?[^`])\1(?!`)|\*\*([^*][\s\S]*?)\*\*|(?<![\w*])\*([^*\s][^*]*?)\*(?![\w*])|(?<![\w])_([^_\s][^_]*?)_(?![\w])|\[([^\]\n]+)\]\(([^)\s]+)\)/g;

function renderInline(text: string, keyPrefix: string): React.ReactNode[] {
  const out: React.ReactNode[] = [];
  let last = 0;
  let n = 0;
  // A fresh regex per call: the nested renderInline calls below must not share lastIndex.
  const re = new RegExp(INLINE.source, 'g');
  let m: RegExpExecArray | null;
  while ((m = re.exec(text)) !== null) {
    if (m.index > last) out.push(text.slice(last, m.index));
    const key = `${keyPrefix}-${n++}`;
    if (m[1] !== undefined) {
      out.push(
        <code key={key} className="rounded bg-surface-3 px-1 py-px font-mono text-[0.9em] text-accent" data-testid="copilot-md-code">
          {m[2]}
        </code>,
      );
    } else if (m[3] !== undefined) {
      out.push(<strong key={key} className="font-semibold text-text-primary">{renderInline(m[3], key)}</strong>);
    } else if (m[4] !== undefined || m[5] !== undefined) {
      out.push(<em key={key}>{renderInline((m[4] ?? m[5]) as string, key)}</em>);
    } else if (m[6] !== undefined) {
      const url = m[7];
      if (SAFE_URL.test(url)) {
        out.push(
          <a
            key={key}
            href={url}
            target="_blank"
            rel="noopener noreferrer"
            className="text-accent underline underline-offset-2 hover:opacity-80"
            data-testid="copilot-md-link"
          >
            {m[6]}
          </a>,
        );
      } else {
        // Not http/https (javascript:, data:, relative...): show the label, never a link.
        out.push(m[6]);
      }
    }
    last = re.lastIndex;
  }
  if (last < text.length) out.push(text.slice(last));
  return out;
}

async function copyText(text: string): Promise<boolean> {
  try {
    await navigator.clipboard.writeText(text);
    return true;
  } catch {
    try {
      const area = document.createElement('textarea');
      area.value = text;
      area.style.position = 'fixed';
      area.style.opacity = '0';
      document.body.appendChild(area);
      area.select();
      const ok = document.execCommand('copy');
      area.remove();
      return ok;
    } catch {
      return false;
    }
  }
}

export const CopyButton: React.FC<{ text: string; label?: string; className?: string; testId: string }> = ({
  text, label = 'Copy', className = '', testId,
}) => {
  const [done, setDone] = useState(false);
  return (
    <button
      type="button"
      onClick={() => {
        void copyText(text).then((ok) => {
          if (!ok) return;
          setDone(true);
          setTimeout(() => setDone(false), 1500);
        });
      }}
      className={`rounded border border-[var(--border-color)] px-1.5 py-px font-mono text-[10px] text-text-muted hover:border-accent hover:text-accent ${className}`}
      data-testid={testId}
    >
      {done ? 'Copied' : label}
    </button>
  );
};

const HEADING_CLASS = [
  '',
  'text-base font-semibold text-text-primary mt-2',
  'text-sm font-semibold text-text-primary mt-2',
  'text-sm font-semibold text-accent mt-1.5',
  'text-xs font-semibold uppercase tracking-wide text-text-secondary mt-1.5',
  'text-xs font-semibold text-text-secondary mt-1',
  'text-xs font-semibold text-text-muted mt-1',
];

export const Markdown: React.FC<{ text: string }> = ({ text }) => {
  const blocks = React.useMemo(() => parseBlocks(text), [text]);
  return (
    <div className="space-y-2 break-words text-[13px] leading-relaxed text-text-primary" data-testid="copilot-md">
      {blocks.map((b, idx) => {
        const key = `b${idx}`;
        switch (b.type) {
          case 'heading':
            return <div key={key} role="heading" aria-level={b.level} className={HEADING_CLASS[b.level]}>{renderInline(b.text, key)}</div>;
          case 'code':
            return (
              <div key={key} className="overflow-hidden rounded border border-[var(--border-color)] bg-surface-0" data-testid="copilot-code-block">
                <div className="flex items-center justify-between border-b border-[var(--border-color)] bg-surface-2 px-2 py-0.5">
                  <span className="font-mono text-[10px] uppercase tracking-wider text-text-muted">{b.lang || 'text'}</span>
                  <CopyButton text={b.code} testId="copilot-code-copy" />
                </div>
                <pre className="overflow-x-auto px-2 py-1.5 font-mono text-[12px] leading-snug text-text-primary"><code>{b.code}</code></pre>
              </div>
            );
          case 'ul':
            return (
              <ul key={key} className="list-disc space-y-0.5 pl-5">
                {b.items.map((it, j) => <li key={j}>{renderInline(it, `${key}-${j}`)}</li>)}
              </ul>
            );
          case 'ol':
            return (
              <ol key={key} className="list-decimal space-y-0.5 pl-5">
                {b.items.map((it, j) => <li key={j}>{renderInline(it, `${key}-${j}`)}</li>)}
              </ol>
            );
          case 'table':
            return (
              <div key={key} className="overflow-x-auto" data-testid="copilot-md-table">
                <table className="min-w-full border-collapse text-xs">
                  <thead>
                    <tr>
                      {b.head.map((h, j) => (
                        <th key={j} className="border border-[var(--border-color)] bg-surface-2 px-2 py-1 text-left font-semibold">{renderInline(h, `${key}-h${j}`)}</th>
                      ))}
                    </tr>
                  </thead>
                  <tbody>
                    {b.rows.map((r, ri) => (
                      <tr key={ri}>
                        {b.head.map((_, ci) => (
                          <td key={ci} className="border border-[var(--border-color)] px-2 py-1 font-mono">{renderInline(r[ci] ?? '', `${key}-${ri}-${ci}`)}</td>
                        ))}
                      </tr>
                    ))}
                  </tbody>
                </table>
              </div>
            );
          case 'quote':
            return <blockquote key={key} className="border-l-2 border-accent/60 pl-2 text-text-secondary whitespace-pre-wrap">{renderInline(b.text, key)}</blockquote>;
          case 'hr':
            return <hr key={key} className="border-[var(--border-color)]" />;
          default:
            return <p key={key} className="whitespace-pre-wrap">{renderInline(b.text, key)}</p>;
        }
      })}
    </div>
  );
};
