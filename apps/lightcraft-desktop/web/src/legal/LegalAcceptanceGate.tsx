// First-run legal gate for Ember, through the shared @rightkit/legal-ui `LegalGate`.
//
// Mount points (owner files, see owner-hooks.patch):
//   standalone  main.tsx wraps <App /> in <LegalAcceptanceGate>. Children render only once the stored
//               receipt is current for the bundled Privacy Notice text.
//   onboarding  the rkshell lane passes `legalStepConfig()` as <Onboarding legal={...}> and
//               `legalReceiptPort` as `legalReceipt`, and drops this wrapper's gate (keep
//               <LegalDocumentViewerHost /> mounted, the config opens documents through it).
import { useEffect, useState, type ReactNode } from 'react';
import { LegalGate } from '@rightkit/legal-ui';
import '@rightkit/legal-ui/legal-ui.css';
import { LEGAL_AGREEMENT_STATEMENT, LEGAL_DOCUMENTS_HASH, LEGAL_DOCUMENT_SOURCES, LEGAL_VERSION, type LegalDocumentId } from './documents';
import { buildLegalAcceptanceRecord, legalReceiptCurrent, readLegalReceipt, writeLegalReceipt } from './receipt';
import { LegalDocumentViewerHost, openLegalDocument } from './viewer';

// Tokens of the Ember shell with fallbacks, so the gate follows the active light or dark theme.
const THEME = {
  bg: 'var(--rk-color-bg, #f5f7fa)',
  surface: 'var(--rk-color-surface, #ffffff)',
  border: 'var(--rk-color-border, #d8dee7)',
  text: 'var(--rk-color-text, #16202d)',
  muted: 'var(--rk-color-muted, #617185)',
  accent: '#0165dd',
  accentContrast: '#ffffff',
  fontDisplay: 'Inter, system-ui, sans-serif',
  fontBody: 'Inter, system-ui, sans-serif',
};

const TRUST_LINE =
  'Ember is free and open-source software. Your photos, catalog and edits stay on your computer, and every feature is available with no account or licence key.';

async function quitApp(): Promise<void> {
  try {
    const { getCurrentWindow } = await import('@tauri-apps/api/window');
    await getCurrentWindow().close();
  } catch {
    // Outside the desktop shell there is no window to close.
  }
}

/** Gate config shared by the standalone wrapper and the onboarding legal step. */
export function legalStepConfig(openDocument: (id: LegalDocumentId) => void = openLegalDocument) {
  return {
    appName: 'Ember',
    trustLine: TRUST_LINE,
    documents: LEGAL_DOCUMENT_SOURCES.map((d) => ({ title: d.title, onOpen: () => openDocument(d.id) })),
    agreementStatement: LEGAL_AGREEMENT_STATEMENT,
    version: LEGAL_VERSION,
    manifestSha256: LEGAL_DOCUMENTS_HASH,
    theme: THEME,
    eyebrow: 'Open-source notice',
    heading: 'Before you use Ember',
    agreeLabel: 'Continue',
    eligibilityLabels: {
      legend: 'How will you use Ember?',
      individual: { label: 'For myself, on my own devices.', description: '' },
      enterprise: { label: 'For or on behalf of an organization.', description: '' },
    },
    onQuit: () => { void quitApp(); },
  };
}

export function LegalAcceptanceGate({ children }: { children: ReactNode }) {
  const [state, setState] = useState<'loading' | 'needed' | 'accepted'>('loading');

  useEffect(() => {
    let live = true;
    readLegalReceipt().then(
      (record) => { if (live) setState(legalReceiptCurrent(record) ? 'accepted' : 'needed'); },
      // An unreadable receipt is not an acceptance: ask again.
      () => { if (live) setState('needed'); },
    );
    return () => { live = false; };
  }, []);

  return (
    <>
      {state === 'accepted' ? children : state === 'needed' ? (
        <LegalGate
          {...legalStepConfig()}
          variant="first-run"
          renderError={(error) => `Could not save your choice: ${error instanceof Error ? error.message : String(error)}`}
          onAgree={async ({ acceptedAt, eligibilityBasis }) => {
            await writeLegalReceipt(buildLegalAcceptanceRecord(acceptedAt, eligibilityBasis));
            setState('accepted');
          }}
        />
      ) : null}
      <LegalDocumentViewerHost />
    </>
  );
}
