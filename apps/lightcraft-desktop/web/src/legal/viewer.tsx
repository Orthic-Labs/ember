import { useSyncExternalStore, type ReactNode } from 'react';
import { legalDocument, type LegalDocumentId, type LegalDocumentSource } from './documents';

/** Which bundled document is open. A tiny store, so the gate, About and onboarding can open one without props. */
let openId: LegalDocumentId | null = null;
const listeners = new Set<() => void>();

function emit() {
  for (const listener of listeners) listener();
}

export function openLegalDocument(id: LegalDocumentId): void {
  openId = id;
  emit();
}

export function closeLegalDocument(): void {
  openId = null;
  emit();
}

function subscribe(listener: () => void): () => void {
  listeners.add(listener);
  return () => { listeners.delete(listener); };
}

type Block = { kind: 'heading'; text: string } | { kind: 'paragraph'; text: string } | { kind: 'list'; items: string[] };

/** The Markdown subset the bundled notice uses: headings, paragraphs, bullet lists and **bold**. */
function parseMarkdown(source: string): Block[] {
  const blocks: Block[] = [];
  let paragraph: string[] = [];
  let list: string[] = [];
  const flush = () => {
    if (paragraph.length) blocks.push({ kind: 'paragraph', text: paragraph.join(' ') });
    if (list.length) blocks.push({ kind: 'list', items: list });
    paragraph = [];
    list = [];
  };
  for (const raw of source.split(/\r?\n/)) {
    const line = raw.trim();
    const heading = /^#{1,3}\s+(.*)$/.exec(line);
    const bullet = /^-\s+(.*)$/.exec(line);
    if (line === '') flush();
    else if (heading) { flush(); blocks.push({ kind: 'heading', text: heading[1] }); }
    else if (bullet) { if (paragraph.length) flush(); list.push(bullet[1]); }
    else { if (list.length) flush(); paragraph.push(line); }
  }
  flush();
  return blocks;
}

function inline(text: string): ReactNode[] {
  return text.split(/(\*\*[^*]+\*\*)/).filter((part) => part !== '').map((part, i) =>
    part.startsWith('**') && part.endsWith('**') ? <strong key={i}>{part.slice(2, -2)}</strong> : part);
}

function DocumentBody({ document }: { document: LegalDocumentSource }) {
  if (document.format === 'text') {
    return <pre style={{ margin: 0, whiteSpace: 'pre-wrap', overflowWrap: 'anywhere', font: '12px/1.5 ui-monospace, monospace' }}>{document.body}</pre>;
  }
  return <>{parseMarkdown(document.body).map((block, i) => {
    if (block.kind === 'heading') return <h3 key={i} style={{ margin: '16px 0 6px', font: '600 15px/1.3 Inter, system-ui, sans-serif' }}>{block.text}</h3>;
    if (block.kind === 'list') return <ul key={i} style={{ paddingLeft: 20 }}>{block.items.map((item, n) => <li key={n}>{inline(item)}</li>)}</ul>;
    return <p key={i} style={{ margin: '8px 0', lineHeight: 1.5 }}>{inline(block.text)}</p>;
  })}</>;
}

/** Mount once, above everything that can open a document (including the first-run gate). */
export function LegalDocumentViewerHost() {
  const id = useSyncExternalStore(subscribe, () => openId, () => null);
  if (!id) return null;
  const document = legalDocument(id);
  return (
    <div className="lc-dialog-backdrop" style={{ zIndex: 3000 }} onClick={closeLegalDocument}>
      <div className="lc-dialog lc-dialog-wide" role="dialog" aria-modal="true" aria-label={document.title} onClick={(event) => event.stopPropagation()}>
        <div className="lc-dialog-header">
          <h2>{document.title}</h2>
          <button type="button" className="lc-icon-button" aria-label="Close" onClick={closeLegalDocument}>×</button>
        </div>
        <div className="lc-dialog-body"><DocumentBody document={document} /></div>
        <div className="lc-dialog-actions">
          <div className="lc-dialog-action-group">
            <button type="button" className="lc-button lc-button-primary" autoFocus onClick={closeLegalDocument}>Done</button>
          </div>
        </div>
      </div>
    </div>
  );
}
