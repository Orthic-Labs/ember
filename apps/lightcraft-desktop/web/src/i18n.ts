import { createI18n } from '@rightkit/i18n';
import zhHans from '../../../../crates/ui-egui/locales/zh-hans.json';
import zhHant from '../../../../crates/ui-egui/locales/zh-hant.json';
import ja from '../../../../crates/ui-egui/locales/ja.json';
import ptBr from '../../../../crates/ui-egui/locales/pt-br.json';

export const LOCALES = ['en', 'zh-hans', 'zh-hant', 'ja', 'pt-br'] as const;
export type LocaleCode = (typeof LOCALES)[number];
type Strings = Record<string, string>;

/** Source strings are English, so the English catalog is empty and a missing key falls back to the key itself. */
const i18n = createI18n<Strings>(
  {},
  {
    defaultLocale: 'en',
    loaders: {
      'zh-hans': () => zhHans as Strings,
      'zh-hant': () => zhHant as Strings,
      ja: () => ja as Strings,
      'pt-br': () => ptBr as Strings,
    },
  },
);

/**
 * Catalogs that have finished loading, read synchronously by `translate`. The loaders return bundled JSON,
 * so every entry is in place after the first microtask checkpoint following module evaluation, before any
 * React render or user input. A locale not yet in this map reads as English rather than blocking.
 */
const loaded = new Map<LocaleCode, Strings>();
for (const code of LOCALES) {
  if (code === 'en') continue;
  i18n.loadLocale(code).then(
    (catalog) => loaded.set(code, catalog),
    () => undefined,
  );
}

/** Normalize persisted BCP-47-ish tags to baseline egui catalog codes. */
export function normalizeLocale(value: unknown): LocaleCode {
  const raw = typeof value === 'string' ? value.trim().toLowerCase().replace(/_/g, '-') : '';
  const tag = raw.split(/[.@]/, 1)[0];
  if (tag === 'pt' || tag.startsWith('pt-')) return 'pt-br';
  if (tag === 'ja' || tag.startsWith('ja-')) return 'ja';
  if (tag === 'zh-hant' || tag === 'zh-tw' || tag === 'zh-hk' || tag === 'zh-mo' || tag.startsWith('zh-hant-')) return 'zh-hant';
  if (tag === 'zh' || tag === 'zh-hans' || tag === 'zh-cn' || tag === 'zh-sg' || tag.startsWith('zh-hans-')) return 'zh-hans';
  return 'en';
}

/** Translate built-in source strings; user content and unknown strings fall back unchanged. */
export function translate(locale: unknown, source: string): string {
  const code = normalizeLocale(locale);
  if (code === 'en') return source;
  const catalog = loaded.get(code);
  return catalog && Object.prototype.hasOwnProperty.call(catalog, source) ? catalog[source] : source;
}
