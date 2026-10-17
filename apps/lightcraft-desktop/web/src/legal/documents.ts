import privacySource from './PRIVACY_NOTICE.md?raw';
import licenseMit from '../../../../../LICENSE-MIT?raw';
import licenseApache from '../../../../../LICENSE-APACHE?raw';
import noticeSource from '../../../../../NOTICE?raw';
import attributionSource from '../../../../../assets/ATTRIBUTION.md?raw';
import oflInter from '../../../../../assets/fonts/OFL-Inter.txt?raw';

/**
 * Ember's legal documents, bundled with the app (no hosted copies exist, so each opens in the app).
 *
 * Ember is free and open-source software, so the gate asks for one thing: acknowledging the Privacy
 * Notice. Nothing is agreed beyond the MIT OR Apache-2.0 licences the source already carries, and the
 * licences and notices are reference documents that never re-prompt.
 *
 * Versioning: raise LEGAL_BASE_VERSION for a change that is not in the text. Any change to the
 * Privacy Notice text changes LEGAL_VERSION through its content hash and asks everyone again.
 */
export type LegalDocumentId = 'ember-privacy-notice' | 'ember-licences';
export type LegalDocumentTreatment = 'acknowledge' | 'reference';

export interface LegalDocumentSource {
  id: LegalDocumentId;
  title: string;
  treatment: LegalDocumentTreatment;
  format: 'markdown' | 'text';
  body: string;
}

const RULE = '\n\n' + '-'.repeat(72) + '\n\n';

/** Everything that must stay reachable: both licences, NOTICE, asset attributions and the font licence. */
const LICENCES_AND_NOTICES = [
  'Ember is free and open-source software, licensed under the MIT License or the Apache License, Version 2.0, at your option.',
  'LICENSE-MIT\n\n' + licenseMit.trim(),
  'LICENSE-APACHE\n\n' + licenseApache.trim(),
  'NOTICE\n\n' + noticeSource.trim(),
  'assets/ATTRIBUTION.md\n\n' + attributionSource.trim(),
  'assets/fonts/OFL-Inter.txt\n\n' + oflInter.trim(),
].join(RULE);

export const LEGAL_DOCUMENT_SOURCES: readonly LegalDocumentSource[] = [
  { id: 'ember-privacy-notice', title: 'Privacy Notice', treatment: 'acknowledge', format: 'markdown', body: privacySource },
  { id: 'ember-licences', title: 'Licences and notices', treatment: 'reference', format: 'text', body: LICENCES_AND_NOTICES },
];

/** Change-detection hash (cyrb53), not a security hash: it decides when to ask again. */
export function contentHash(text: string): string {
  let h1 = 0xdeadbeef;
  let h2 = 0x41c6ce57;
  for (let i = 0; i < text.length; i += 1) {
    const ch = text.charCodeAt(i);
    h1 = Math.imul(h1 ^ ch, 2654435761);
    h2 = Math.imul(h2 ^ ch, 1597334677);
  }
  h1 = Math.imul(h1 ^ (h1 >>> 16), 2246822507) ^ Math.imul(h2 ^ (h2 >>> 13), 3266489909);
  h2 = Math.imul(h2 ^ (h2 >>> 16), 2246822507) ^ Math.imul(h1 ^ (h1 >>> 13), 3266489909);
  return (4294967296 * (2097151 & h2) + (h1 >>> 0)).toString(16).padStart(14, '0');
}

/** Only acknowledged documents enter the hash; reference notices change with every dependency bump. */
export const LEGAL_DOCUMENTS_HASH = LEGAL_DOCUMENT_SOURCES.filter((d) => d.treatment === 'acknowledge')
  .map((d) => `${d.id}#${contentHash(d.body.replace(/\r\n?/g, '\n'))}`)
  .join(';');

export const LEGAL_BASE_VERSION = '1';
export const LEGAL_VERSION = `${LEGAL_BASE_VERSION}+${contentHash(LEGAL_DOCUMENTS_HASH)}`;

export const LEGAL_AGREEMENT_STATEMENT =
  'I have read the Ember Privacy Notice. I understand that Ember is free and open-source software, licensed under the MIT License or the Apache License 2.0.';

export function legalDocument(id: LegalDocumentId): LegalDocumentSource {
  const found = LEGAL_DOCUMENT_SOURCES.find((d) => d.id === id);
  if (!found) throw new Error(`unknown legal document: ${id}`);
  return found;
}
