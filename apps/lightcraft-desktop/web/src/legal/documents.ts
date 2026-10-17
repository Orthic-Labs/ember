import licenseMit from '../../../../../LICENSE-MIT?raw';
import licenseApache from '../../../../../LICENSE-APACHE?raw';
import noticeSource from '../../../../../NOTICE?raw';
import attributionSource from '../../../../../assets/ATTRIBUTION.md?raw';
import oflInter from '../../../../../assets/fonts/OFL-Inter.txt?raw';

/**
 * Ember's licences and notices, bundled with the app and opened in the app (no hosted copies exist).
 * Reference only: nothing here is agreed or acknowledged, and nothing blocks first run.
 */
export type LegalDocumentId = 'ember-licences';

export interface LegalDocumentSource {
  id: LegalDocumentId;
  title: string;
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
  { id: 'ember-licences', title: 'Licences and notices', format: 'text', body: LICENCES_AND_NOTICES },
];

export function legalDocument(id: LegalDocumentId): LegalDocumentSource {
  const found = LEGAL_DOCUMENT_SOURCES.find((d) => d.id === id);
  if (!found) throw new Error(`unknown legal document: ${id}`);
  return found;
}
