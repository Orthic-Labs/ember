import { useSyncExternalStore } from 'react';
import { Button, IconButton, ModalShell } from '@rightkit/react-shell/react';
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
    <ModalShell label={document.title} onClose={closeLegalDocument} width={860} autoFocus={false} overlayStyle={{ zIndex: 3000 }} overlayClassName="lc-dialog-backdrop" className="lc-dialog">
      <div className="lc-dialog-header"><h2 className="rk-modal__title">{document.title}</h2><IconButton label="Close" onClick={closeLegalDocument}>×</IconButton></div>
      <div className="lc-dialog-body"><LegalDocumentBody content={document.body} format={document.format} markdown="extended" /></div>
      <div className="rk-modal__actions lc-dialog-actions"><Button variant="primary" className="lc-button lc-button-primary" autoFocus onClick={closeLegalDocument}>Done</Button></div>
    </ModalShell>
  );
}
