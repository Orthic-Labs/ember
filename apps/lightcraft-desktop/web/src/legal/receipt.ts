import { getPreferences, savePreferences } from '../api';
import { LEGAL_DOCUMENTS_HASH, LEGAL_VERSION } from './documents';

/** Same shape as `LegalAcceptanceRecord` in @rightkit/onboarding, so the onboarding lane can reuse the port below. */
export interface LegalAcceptanceRecord {
  version: string;
  documentsHash: string;
  /** UTC ISO-8601 timestamp (`Date#toISOString`). */
  acceptedAt: string;
  eligibilityBasis: 'individual' | 'enterprise';
}

/** Key in the app preferences file. The host keeps keys it does not know across a round trip. */
export const LEGAL_PREFERENCE_KEY = 'legalAcceptance';

function parseRecord(value: unknown): LegalAcceptanceRecord | null {
  if (!value || typeof value !== 'object' || Array.isArray(value)) return null;
  const r = value as Record<string, unknown>;
  if (typeof r.version !== 'string' || typeof r.documentsHash !== 'string' || typeof r.acceptedAt !== 'string') return null;
  if (r.eligibilityBasis !== 'individual' && r.eligibilityBasis !== 'enterprise') return null;
  return { version: r.version, documentsHash: r.documentsHash, acceptedAt: r.acceptedAt, eligibilityBasis: r.eligibilityBasis };
}

/** True only for a receipt taken against the current Privacy Notice text. */
export function legalReceiptCurrent(record: LegalAcceptanceRecord | null): boolean {
  return (
    record !== null &&
    record.version === LEGAL_VERSION &&
    record.documentsHash === LEGAL_DOCUMENTS_HASH &&
    record.acceptedAt.endsWith('Z') &&
    !Number.isNaN(Date.parse(record.acceptedAt))
  );
}

export async function readLegalReceipt(): Promise<LegalAcceptanceRecord | null> {
  const preferences = await getPreferences();
  return parseRecord(preferences[LEGAL_PREFERENCE_KEY]);
}

export async function writeLegalReceipt(record: LegalAcceptanceRecord): Promise<void> {
  await savePreferences({ [LEGAL_PREFERENCE_KEY]: { ...record } });
}

/** Port for `<Onboarding legalReceipt={...}>`: the preferences file stays the source of truth. */
export const legalReceiptPort = { read: readLegalReceipt, write: writeLegalReceipt };

export function buildLegalAcceptanceRecord(acceptedAt: string, eligibilityBasis: LegalAcceptanceRecord['eligibilityBasis']): LegalAcceptanceRecord {
  return { version: LEGAL_VERSION, documentsHash: LEGAL_DOCUMENTS_HASH, acceptedAt, eligibilityBasis };
}
