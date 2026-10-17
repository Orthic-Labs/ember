import type { SettingsBackend, SettingsSnapshot, SettingsValues } from '@rightkit/settings-ui';
import { getPreferences, savePreferences } from '../api';
import type { JsonObject, UiState } from '../types';

/**
 * The `SettingsBackend` port of `@rightkit/settings-ui` for Ember, over the existing native contract:
 *
 * - `lc_preferences` get/patch (host preferences, persisted natively by `rightkit-app-settings`): window UI
 *   choices under `ui`, and every RightKit-owned key that is not an Ember choice (for example the onboarding
 *   progress, `onboarding.state`) under `rightkit`, as flat dotted keys.
 * - Engine commands for library, import, XMP and performance settings, which the engine owns and persists
 *   (`library.preferences`, `library.xmpPreferences`, `library.autoImport`, `app.gpu`, `app.memoryBudget`).
 *
 * Settings apply as they change (one patch per change); there is no Save step. The theme is the one choice
 * mirrored into `appearance.theme`, the key the standard Appearance section reads and writes.
 */

/** The slice of the desktop context the backend needs (a structural subset of `DesktopContextValue`). */
export interface EmberSettingsHost {
  ui: UiState;
  setUi(patch: Partial<UiState>): void;
  run(id: string, params?: JsonObject): Promise<unknown>;
  native(action: string, params?: JsonObject): Promise<unknown>;
}

type Rec = Record<string, unknown>;
const rec = (value: unknown): Rec => (value && typeof value === 'object' && !Array.isArray(value) ? (value as Rec) : {});
const str = (value: unknown, fallback = ''): string => (typeof value === 'string' ? value : fallback);
const flag = (value: unknown, fallback = false): boolean => (typeof value === 'boolean' ? value : fallback);
const num = (value: unknown, fallback = 0): number => (typeof value === 'number' && Number.isFinite(value) ? value : fallback);

/** Settings key to `ui` preference field, for the choices the window itself persists. */
const UI_KEYS = {
  'appearance.theme': 'theme',
  'general.startupView': 'startupView',
  'general.autoAdvance': 'autoAdvance',
  'general.confirmDelete': 'confirmDelete',
  'general.externalEditor': 'externalEditor',
  'interface.filmstrip': 'filmstrip',
  'interface.filmNames': 'filmNames',
  'interface.filmBadges': 'filmBadges',
  'interface.gridInfo': 'gridInfo',
  'interface.navigator': 'navigator',
  'interface.infoOverlay': 'infoOverlay',
  'language.locale': 'locale',
  'performance.previewEdge': 'previewEdge',
  'performance.gpu': 'gpu',
  'performance.memoryMb': 'memoryMb',
} as const satisfies Record<string, keyof UiState>;

const IMPORT_KEYS = {
  'import.rawPreset': 'rawPreset',
  'import.otherPreset': 'otherPreset',
  'import.perCamera': 'perCamera',
  'import.cameras': 'cameras',
  'import.copyright': 'copyright',
  'import.creator': 'creator',
  'import.metadataPreset': 'metadataPreset',
} as const;

const LIBRARY_KEYS = { 'performance.cacheMb': 'cacheMb', 'performance.forgetLocalDays': 'forgetLocalDays' } as const;

const THEMES = ['system', 'light', 'dark'];
const STARTUP_VIEWS = ['last', 'photoGrid', 'detail'];

/** Where a key that is neither an Ember choice nor an engine setting is kept. */
const RIGHTKIT_BUCKET = 'rightkit';

function startupView(value: unknown, fallback: string): string {
  const v = value === 'grid' ? 'photoGrid' : value;
  return typeof v === 'string' && STARTUP_VIEWS.includes(v) ? v : fallback;
}

function infoOverlay(value: unknown, fallback: number): number {
  if (typeof value === 'string') return ({ off: 0, basic: 1, exposure: 2 } as Record<string, number>)[value] ?? fallback;
  return num(value, fallback);
}

function uiValues(ui: UiState, saved: Rec): SettingsValues {
  const stored = rec(saved.ui ?? saved.layout ?? saved);
  const legacy = rec(saved.settings);
  const pick = <T,>(key: string, current: T): unknown => stored[key] ?? legacy[key] ?? current;
  return {
    'appearance.theme': THEMES.includes(ui.theme) ? ui.theme : 'system',
    'general.startupView': startupView(pick('startupView', ui.startupView), 'last'),
    'general.autoAdvance': flag(pick('autoAdvance', ui.autoAdvance), ui.autoAdvance),
    'general.confirmDelete': flag(pick('confirmDelete', ui.confirmDelete), ui.confirmDelete),
    'general.externalEditor': str(pick('externalEditor', ui.externalEditor)),
    'interface.filmstrip': flag(pick('filmstrip', ui.filmstrip), ui.filmstrip),
    'interface.filmNames': flag(pick('filmNames', ui.filmNames), ui.filmNames),
    'interface.filmBadges': flag(pick('filmBadges', ui.filmBadges), ui.filmBadges),
    'interface.gridInfo': flag(pick('gridInfo', ui.gridInfo), ui.gridInfo),
    'interface.navigator': flag(pick('navigator', ui.navigator), ui.navigator),
    'interface.infoOverlay': Math.max(0, Math.min(2, Math.round(infoOverlay(pick('infoOverlay', ui.infoOverlay), ui.infoOverlay)))),
    'language.locale': str(ui.locale, 'en'),
    'performance.previewEdge': num(ui.previewEdge, 2048),
    'performance.gpu': ui.gpu,
    'performance.memoryMb': num(ui.memoryMb, 0),
  };
}

function rightkitValues(saved: Rec): SettingsValues {
  const bucket = rec(saved[RIGHTKIT_BUCKET]);
  const out: SettingsValues = {};
  for (const [key, value] of Object.entries(bucket)) if (value !== null && value !== undefined) out[key] = value;
  return out;
}

export interface EmberSettingsBackend extends SettingsBackend {
  /** Re-read the smart preview location and size (shown in Performance). */
  smartPreviews(): Promise<{ path: string; count: number; bytes: number }>;
}

/** Create the backend over a desktop host. One instance per open Settings panel. */
export function createEmberSettingsBackend(host: EmberSettingsHost): EmberSettingsBackend {
  let revision = 0;
  let values: SettingsValues = {};

  async function read(): Promise<SettingsValues> {
    const [saved, library, xmp, gpu, memory] = await Promise.all([
      getPreferences(),
      host.run('library.preferences', {}),
      host.run('library.xmpPreferences', {}),
      host.run('app.gpu', {}),
      host.run('app.memoryBudget', {}),
    ]);
    const lib = rec(library);
    const imports = rec(lib.import);
    const xmpPrefs = rec(xmp);
    const next: SettingsValues = { ...rightkitValues(rec(saved)), ...uiValues(host.ui, rec(saved)) };
    next['import.rawPreset'] = imports.rawPreset == null ? 'default' : str(imports.rawPreset, 'default');
    next['import.otherPreset'] = imports.otherPreset == null ? 'default' : str(imports.otherPreset, 'default');
    next['import.perCamera'] = flag(imports.perCamera);
    next['import.cameras'] = Array.isArray(imports.cameras) ? imports.cameras.map((c) => ({ camera: str(rec(c).camera), preset: str(rec(c).preset) })) : [];
    next['import.copyright'] = str(imports.copyright);
    next['import.creator'] = str(imports.creator);
    next['import.metadataPreset'] = imports.metadataPreset == null ? 'none' : str(imports.metadataPreset, 'none');
    next['import.autoFolder'] = imports.autoFolder == null ? '' : str(imports.autoFolder);
    next['import.autoCopy'] = flag(imports.autoCopy);
    next['import.autoAlbum'] = imports.autoAlbum == null ? '' : str(imports.autoAlbum);
    next['xmp.autoWrite'] = flag(xmpPrefs.autoWrite);
    next['xmp.naming'] = str(xmpPrefs.naming, 'stem');
    next['performance.cacheMb'] = num(lib.cacheMb);
    next['performance.forgetLocalDays'] = num(lib.forgetLocalDays);
    const gpuState = rec(gpu);
    if (typeof gpuState.enabled === 'boolean') next['performance.gpu'] = gpuState.enabled;
    const budget = num(rec(memory).budget, Number.NaN);
    if (Number.isFinite(budget) && num(next['performance.memoryMb']) !== 0) next['performance.memoryMb'] = Math.round(budget / 1048576);
    return next;
  }

  async function apply(patch: SettingsValues): Promise<void> {
    const ui: Rec = {};
    const importPatch: Rec = {};
    const libraryPatch: Rec = {};
    const xmpPatch: Rec = {};
    const autoPatch: Rec = {};
    const bucket: Rec = {};
    let gpu: boolean | undefined;
    let memoryMb: number | undefined;
    let previewEdge: number | undefined;
    for (const [key, value] of Object.entries(patch)) {
      if (value === null) continue;
      if (key in UI_KEYS) {
        const field = UI_KEYS[key as keyof typeof UI_KEYS];
        if (key === 'appearance.theme') {
          if (typeof value !== 'string' || !THEMES.includes(value)) throw new Error('Unsupported theme choice.');
          ui[field] = value;
        } else if (key === 'general.startupView') {
          ui[field] = startupView(value, 'last');
        } else if (key === 'interface.infoOverlay') {
          ui[field] = Math.max(0, Math.min(2, Math.round(num(value, host.ui.infoOverlay))));
        } else if (key === 'performance.previewEdge') {
          previewEdge = Math.max(256, Math.min(8192, Math.round(num(value, host.ui.previewEdge))));
          ui[field] = previewEdge;
        } else if (key === 'performance.memoryMb') {
          memoryMb = Math.max(0, Math.round(num(value)));
          if (memoryMb !== 0 && memoryMb < 64) throw new Error('Memory budget must be 0 (automatic) or at least 64 MB.');
          ui[field] = memoryMb;
        } else if (key === 'performance.gpu') {
          gpu = flag(value);
          ui[field] = gpu;
        } else if (key === 'general.externalEditor') {
          ui[field] = str(value).trim();
        } else {
          ui[field] = value;
        }
      } else if (key in IMPORT_KEYS) {
        const field = IMPORT_KEYS[key as keyof typeof IMPORT_KEYS];
        if (field === 'rawPreset' || field === 'otherPreset') importPatch[field] = str(value).trim() || 'default';
        else if (field === 'perCamera') importPatch[field] = flag(value);
        else if (field === 'cameras') {
          importPatch[field] = (Array.isArray(value) ? value : [])
            .map((c) => ({ camera: str(rec(c).camera).trim(), preset: str(rec(c).preset).trim() || null }))
            .filter((c) => c.camera);
        } else if (field === 'metadataPreset') importPatch[field] = str(value).trim() === 'none' ? null : str(value).trim() || null;
        else importPatch[field] = str(value).trim();
      } else if (key in LIBRARY_KEYS) {
        libraryPatch[LIBRARY_KEYS[key as keyof typeof LIBRARY_KEYS]] = Math.max(0, Math.round(num(value)));
      } else if (key === 'xmp.autoWrite') xmpPatch.autoWrite = flag(value);
      else if (key === 'xmp.naming') xmpPatch.naming = str(value, 'stem');
      else if (key === 'import.autoFolder') autoPatch.folder = str(value).trim() || null;
      else if (key === 'import.autoCopy') autoPatch.copy = flag(value);
      else if (key === 'import.autoAlbum') autoPatch.album = str(value).trim() || null;
      else bucket[key] = value;
    }
    if (Object.keys(importPatch).length) await host.run('library.preferences', { import: importPatch as JsonObject });
    if (Object.keys(libraryPatch).length) await host.run('library.preferences', libraryPatch as JsonObject);
    if (Object.keys(xmpPatch).length) await host.run('library.xmpPreferences', xmpPatch as JsonObject);
    if (Object.keys(autoPatch).length) await host.run('library.autoImport', autoPatch as JsonObject);
    if (gpu !== undefined) await host.run('app.gpu', { enabled: gpu });
    if (memoryMb !== undefined && memoryMb >= 64) await host.run('app.memoryBudget', { mb: memoryMb });
    if (previewEdge !== undefined && previewEdge !== host.ui.previewEdge) await host.run('library.buildPreviews', { size: 'standard', edge: previewEdge });
    if (Object.keys(ui).length) {
      await host.native('preferences.patch', { ui: ui as JsonObject });
      host.setUi(ui as Partial<UiState>);
    }
    if (Object.keys(bucket).length) await savePreferences({ [RIGHTKIT_BUCKET]: bucket as JsonObject });
  }

  return {
    async load(): Promise<SettingsSnapshot> {
      values = await read();
      return { values, revision };
    },
    async patch(patch: SettingsValues): Promise<SettingsSnapshot> {
      await apply(patch);
      const next = { ...values };
      for (const [key, value] of Object.entries(patch)) if (value !== null) next[key] = value;
      values = next;
      revision += 1;
      return { values, revision };
    },
    async smartPreviews() {
      const smart = rec(await host.run('library.smartPreviewsLocation', {}));
      return { path: str(smart.path), count: num(smart.count), bytes: num(smart.bytes) };
    },
  };
}

/**
 * Settings port over host preferences alone (no engine commands), for what runs before the library is open:
 * the first-run flow and its gate read and save the RightKit keys (`onboarding.state`) through it. It shares the
 * `rightkit` bucket with the full backend, so both see the same values.
 */
export function createPreferenceSettingsBackend(): SettingsBackend {
  let revision = 0;
  const snapshot = async (): Promise<SettingsSnapshot> => {
    const saved = rec(await getPreferences());
    const theme = rec(saved.ui).theme;
    return { values: { ...rightkitValues(saved), 'appearance.theme': THEMES.includes(theme as string) ? theme : 'system' }, revision };
  };
  return {
    load: snapshot,
    async patch(patch: SettingsValues): Promise<SettingsSnapshot> {
      const bucket: Rec = {};
      let theme: unknown;
      for (const [key, value] of Object.entries(patch)) {
        if (key === 'appearance.theme') theme = value;
        else bucket[key] = value;
      }
      if (theme !== undefined && theme !== null && (typeof theme !== 'string' || !THEMES.includes(theme))) throw new Error('Unsupported theme choice.');
      const next: Rec = {};
      if (Object.keys(bucket).length) next[RIGHTKIT_BUCKET] = bucket;
      if (typeof theme === 'string') next.ui = { theme };
      if (Object.keys(next).length) await savePreferences(next as JsonObject);
      revision += 1;
      return snapshot();
    },
  };
}
