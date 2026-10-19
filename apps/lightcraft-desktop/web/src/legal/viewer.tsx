import { useSyncExternalStore } from 'react';
import { LegalDocumentBody } from '@rightkit/legal-ui';
import '@rightkit/legal-ui/legal-ui.css';
import { legalDocument, type LegalDocumentId } from './documents';

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
        <div className="lc-dialog-body"><LegalDocumentBody content={document.body} format={document.format} markdown="extended" /></div>
        <div className="lc-dialog-actions">
          <div className="lc-dialog-action-group">
            <button type="button" className="lc-button lc-button-primary" autoFocus onClick={closeLegalDocument}>Done</button>
          </div>
        </div>
      </div>
    </div>
  );
}
