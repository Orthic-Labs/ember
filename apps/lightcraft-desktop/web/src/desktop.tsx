import { createContext, useCallback, useContext, useEffect, useMemo, useRef, useState, type ReactNode } from 'react';
import { listen } from '@tauri-apps/api/event';
import { getSnapshot, nativeAction, openSecondWindow, runCommand, savePreferences } from './api';
import { allUiMetadata, applyUiCommand, UI_COMMAND_IDS } from './commands';
import { normalizeLocale, translate } from './i18n';
import type { DesktopContextValue, DesktopSnapshot, DialogState, JsonObject, StartupView, UiState, ViewMode, WindowBootstrap } from './types';

const MIN_STAGE = 360;
const DEFAULT_UI: UiState = {
  view: 'photoGrid', panel: 'info', tool: '', zoom: 'fit', clickZoom: 1, beforeAfter: 'off',
  sidebarCollapsed: false, inspectorCollapsed: false, sidebarWidth: 246, inspectorWidth: 292,
  thumbSize: 180, filmstrip: true, filterBar: true, referenceId: null, compareIds: [], navigator: false,
  slideshow: false, infoOverlay: 0, maskOverlay: false, maskOverlayMode: 'selected', maskPins: true,
  clipping: false, theme: 'system', locale: 'en', sections: { light: true, color: true, effects: false, detail: false, optics: false },
  autoAdvance: false, gridInfo: true, softProof: false, brushSize: 100, brushFeather: 50, cropOverlay: 'thirds', filterText: '',
  startupView: 'last', confirmDelete: true, gpu: true, previewEdge: 2560, memoryMb: 0, externalEditor: '', filmNames: true, filmBadges: true,
};

const DesktopContext = createContext<DesktopContextValue | null>(null);

function messageOf(reason: unknown): string {
  if (reason instanceof Error) return reason.message || 'Desktop session unavailable';
  if (typeof reason === 'string') return reason;
  if (reason && typeof reason === 'object' && 'message' in reason) return String((reason as { message?: unknown }).message);
  return 'Desktop session unavailable';
}

function mergeUi(value: unknown): Partial<UiState> {
  if (!value || typeof value !== 'object' || Array.isArray(value)) return {};
  const source = value as Record<string, unknown>;
  const next: Partial<UiState> = {};
  (Object.keys(DEFAULT_UI) as Array<keyof UiState>).forEach((key) => {
    const candidate = source[key] ?? (key === 'startupView' ? source.startup_view : undefined);
    if (candidate === undefined) return;
    if (key === 'locale') (next[key] as unknown) = normalizeLocale(candidate);
    else if (key === 'startupView') {
      const startupView = startupViewOf(candidate);
      if (startupView !== null) next.startupView = startupView;
    } else (next[key] as unknown) = candidate;
  });
  return next;
}

function objectOf(value: unknown): Record<string, unknown> | null {
  return value && typeof value === 'object' && !Array.isArray(value) ? value as Record<string, unknown> : null;
}

function finiteNumber(value: unknown): number | null {
  return typeof value === 'number' && Number.isFinite(value) ? value : null;
}

function startupViewOf(value: unknown): StartupView | null {
  if (typeof value !== 'string') return null;
  const normalized = value.trim().toLowerCase();
  if (normalized === 'last' || normalized === 'lastview' || normalized === 'last_view') return 'last';
  if (normalized === 'grid' || normalized === 'photogrid' || normalized === 'photo_grid') return 'photoGrid';
  if (normalized === 'detail') return 'detail';
  return null;
}

/** Map legacy egui UiState once; a persisted `ui` object becomes migration sentinel. */
function migrateLegacyUi(value: JsonObject): Partial<UiState> {
  const next: Partial<UiState> = {};
  const legacy = value;
  const settings = objectOf(legacy.settings);
  const view = legacy.view;
  const panel = legacy.right;
  const viewModes: UiState['view'][] = ['photoGrid', 'squareGrid', 'detail', 'compare', 'survey', 'people', 'reference'];
  const panels: Array<Exclude<UiState['panel'], null>> = ['edit', 'profiles', 'crop', 'remove', 'masking', 'redeye', 'presets', 'info', 'keywords', 'versions', 'activity'];

  if (typeof legacy.language === 'string' && ['en', 'zh-hans', 'zh-hant', 'ja'].includes(legacy.language.toLowerCase())) next.locale = legacy.language.toLowerCase();
  if (typeof view === 'string' && viewModes.includes(view as UiState['view'])) next.view = view as UiState['view'];
  const startupView = startupViewOf(settings?.startupView)
    ?? startupViewOf(settings?.startup_view)
    ?? startupViewOf(legacy.startupView)
    ?? startupViewOf(legacy.startup_view);
  if (startupView !== null) next.startupView = startupView;
  if (typeof legacy.leftPanel === 'boolean') next.sidebarCollapsed = !legacy.leftPanel;
  const leftWidth = finiteNumber(legacy.leftWidth);
  if (leftWidth !== null) next.sidebarWidth = leftWidth;
  const rightWidth = finiteNumber(legacy.rightWidth);
  if (rightWidth !== null) next.inspectorWidth = rightWidth;
  if (typeof panel === 'string') {
    const normalized = panel === 'redEye' ? 'redeye' : panel;
    if (normalized === 'none') next.inspectorCollapsed = true;
    else if (panels.includes(normalized as Exclude<UiState['panel'], null>)) {
      next.panel = normalized as UiState['panel'];
      next.inspectorCollapsed = false;
    }
  }
  if (typeof legacy.filmstrip === 'boolean') next.filmstrip = legacy.filmstrip;
  if (typeof legacy.thumbSize === 'number' && Number.isFinite(legacy.thumbSize)) next.thumbSize = legacy.thumbSize;
  if (typeof legacy.beforeAfter === 'string' && ['off', 'sideBySide', 'split', 'topBottom', 'splitTopBottom', 'original'].includes(legacy.beforeAfter)) next.beforeAfter = legacy.beforeAfter as UiState['beforeAfter'];
  if (typeof legacy.clickZoom === 'number' && Number.isFinite(legacy.clickZoom)) next.clickZoom = Math.max(1, Math.min(8, legacy.clickZoom / 100));
  if (legacy.zoom === 'fit' || legacy.zoom === 'fill') next.zoom = legacy.zoom;
  else {
    const zoom = finiteNumber(legacy.zoom) ?? finiteNumber(objectOf(legacy.zoom)?.percent);
    if (zoom !== null) next.zoom = Math.max(0.1, zoom / (zoom > 16 ? 100 : 1));
  }
  if (typeof legacy.showClipping === 'boolean') next.clipping = legacy.showClipping;
  if (typeof legacy.softProof === 'boolean') next.softProof = legacy.softProof;
  if (typeof legacy.maskOverlay === 'boolean') next.maskOverlay = legacy.maskOverlay;
  if (typeof legacy.maskOverlayMode === 'string') next.maskOverlayMode = legacy.maskOverlayMode;
  if (typeof legacy.maskPins === 'boolean') next.maskPins = legacy.maskPins;
  if (typeof legacy.cropOverlay === 'string') next.cropOverlay = legacy.cropOverlay;
  if (typeof legacy.search === 'string') next.filterText = legacy.search;
  if (typeof legacy.filterBar === 'boolean') next.filterBar = legacy.filterBar;
  if (typeof legacy.autoAdvance === 'boolean') next.autoAdvance = legacy.autoAdvance;
  if (typeof legacy.navigator === 'boolean') next.navigator = legacy.navigator;
  if (typeof legacy.gridInfo === 'string') next.gridInfo = legacy.gridInfo !== 'none';
  else if (typeof legacy.showCounts === 'boolean') next.gridInfo = legacy.showCounts;
  const infoOverlay = legacy.infoOverlay;
  if (infoOverlay === 'off') next.infoOverlay = 0;
  else if (infoOverlay === 'basic') next.infoOverlay = 1;
  else if (infoOverlay === 'exposure') next.infoOverlay = 2;
  const sections = { ...DEFAULT_UI.sections };
  if (Array.isArray(legacy.openSections)) legacy.openSections.forEach((section) => { if (typeof section === 'string') sections[section] = true; });
  if (Array.isArray(legacy.openSections)) next.sections = sections;
  const gridBadges = settings?.gridBadges;
  if (gridBadges === 'never') next.gridInfo = false;
  else if (gridBadges === 'always' || gridBadges === 'auto') next.gridInfo = true;
  // the engine's settings save it as `askBeforeDelete` (on by default); `confirmDelete` is the older key
  const askBeforeDelete = settings?.askBeforeDelete ?? settings?.confirmDelete;
  if (typeof askBeforeDelete === 'boolean') next.confirmDelete = askBeforeDelete;
  if (typeof settings?.gpu === 'boolean') next.gpu = settings.gpu;
  const previewEdge = finiteNumber(settings?.previewEdge);
  if (previewEdge !== null) next.previewEdge = Math.max(512, Math.min(16384, Math.round(previewEdge)));
  const memoryMb = finiteNumber(settings?.memoryMb);
  if (memoryMb !== null) next.memoryMb = Math.max(0, Math.min(1_048_576, Math.round(memoryMb)));
  if (typeof settings?.externalEditor === 'string') next.externalEditor = settings.externalEditor;
  if (typeof settings?.filmNames === 'boolean') next.filmNames = settings.filmNames;
  if (typeof settings?.filmBadges === 'boolean') next.filmBadges = settings.filmBadges;
  return next;
}

function clampUi(next: Partial<UiState>, current: UiState): Partial<UiState> {
  const result = { ...next } as Partial<UiState>;
  if (typeof result.previewEdge === 'number' && Number.isFinite(result.previewEdge)) result.previewEdge = Math.max(512, Math.min(16384, Math.round(result.previewEdge)));
  if (typeof result.memoryMb === 'number' && Number.isFinite(result.memoryMb)) result.memoryMb = Math.max(0, Math.min(1_048_576, Math.round(result.memoryMb)));
  if (typeof result.sidebarWidth === 'number') result.sidebarWidth = Math.max(220, Math.min(420, result.sidebarWidth));
  if (typeof result.inspectorWidth === 'number') result.inspectorWidth = Math.max(260, Math.min(520, result.inspectorWidth));
  if (typeof result.sidebarWidth === 'number' || typeof result.inspectorWidth === 'number') {
    const width = typeof window === 'undefined' ? 1600 : window.innerWidth;
    const sidebar = result.sidebarWidth ?? current.sidebarWidth;
    const inspector = result.inspectorWidth ?? current.inspectorWidth;
    const shellSidebar = current.sidebarCollapsed ? 48 : sidebar;
    const maxPanels = Math.max(220 + 260, width - MIN_STAGE - shellSidebar);
    if (sidebar + inspector > maxPanels) {
      if (next.inspectorWidth !== undefined) result.sidebarWidth = Math.max(220, maxPanels - inspector);
      else result.inspectorWidth = Math.max(260, maxPanels - sidebar);
    }
  }
  return result;
}

function preferenceUi(preferences: JsonObject | undefined): Partial<UiState> {
  if (!preferences) return {};
  if (objectOf(preferences.ui) || objectOf(preferences.layout)) {
    const source = objectOf(preferences.ui ?? preferences.layout) ?? {};
    const next = mergeUi(source);
    const general = objectOf(source.general) ?? {};
    const performance = objectOf(source.performance) ?? {};
    const nested = { ...general, ...performance };
    const setIfAbsent = <K extends keyof UiState>(key: K, value: unknown) => {
      if (next[key] !== undefined) return;
      (next[key] as unknown) = value;
    };
    if (general.theme === 'light' || general.theme === 'dark' || general.theme === 'system') setIfAbsent('theme', general.theme);
    if (typeof general.locale === 'string') setIfAbsent('locale', normalizeLocale(general.locale));
    const nestedAsk = nested.askBeforeDelete ?? nested.confirmDelete;
    if (typeof nestedAsk === 'boolean') setIfAbsent('confirmDelete', nestedAsk);
    if (typeof nested.gpu === 'boolean') setIfAbsent('gpu', nested.gpu);
    const edge = finiteNumber(nested.previewEdge);
    if (edge !== null) setIfAbsent('previewEdge', Math.max(512, Math.min(16384, Math.round(edge))));
    const memory = finiteNumber(nested.memoryMb ?? nested.cacheMb);
    if (memory !== null) setIfAbsent('memoryMb', Math.max(0, Math.min(1_048_576, Math.round(memory))));
    if (typeof nested.externalEditor === 'string') setIfAbsent('externalEditor', nested.externalEditor);
    if (typeof nested.filmNames === 'boolean') setIfAbsent('filmNames', nested.filmNames);
    if (typeof nested.filmBadges === 'boolean') setIfAbsent('filmBadges', nested.filmBadges);
    // Native migration preserves legacy root fields. Fill only settings absent from newer React UI.
    const legacy = migrateLegacyUi(preferences);
    (Object.keys(legacy) as Array<keyof UiState>).forEach((key) => {
      if (next[key] === undefined) (next[key] as unknown) = legacy[key];
    });
    return next;
  }
  return migrateLegacyUi(preferences);
}

function dialogForCommand(id: string): DialogState | null {
  if (id === 'app.settings') return { kind: 'settings' };
  if (id === 'app.about') return { kind: 'about' };
  if (id === 'app.systemInfo') return { kind: 'systemInfo' };
  if (id === 'app.whatsNew') return { kind: 'whatsNew' };
  if (id === 'app.shortcuts') return { kind: 'shortcuts' };
  if (id.startsWith('dialog.')) return { kind: id.slice(7) };
  return null;
}

function pathsFrom(value: unknown): string[] {
  if (typeof value === 'string') return value ? [value] : [];
  if (Array.isArray(value)) return value.filter((path): path is string => typeof path === 'string' && path.length > 0);
  if (!value || typeof value !== 'object') return [];
  const record = value as Record<string, unknown>;
  return pathsFrom(record.paths ?? record.files ?? record.selected ?? record.path);
}

function firstPath(value: unknown): string | undefined { return pathsFrom(value)[0]; }

function idsFrom(value: unknown): number[] {
  if (!Array.isArray(value)) return [];
  return value.filter((id): id is number => typeof id === 'number' && Number.isSafeInteger(id) && id > 0);
}

function externalEditId(value: unknown): number | null {
  const result = objectOf(value);
  if (!result) return null;
  const id = finiteNumber(result.id);
  return id !== null && Number.isSafeInteger(id) && id > 0 ? id : idsFrom(result.ids)[0] ?? null;
}

const BOOTSTRAP_VIEWS: readonly ViewMode[] = ['photoGrid', 'squareGrid', 'detail', 'compare', 'survey', 'people', 'reference'];
const MAX_EXTERNAL_EDIT_IDS = 256;

function windowBootstrap(): WindowBootstrap {
  if (typeof window === 'undefined') return { secondary: false, view: null, active: null };
  const query = new URLSearchParams(window.location.search);
  const view = query.get('view');
  const active = Number(query.get('active'));
  return {
    secondary: query.get('window') === 'second',
    view: view && (BOOTSTRAP_VIEWS as readonly string[]).includes(view) ? view as ViewMode : null,
    active: Number.isSafeInteger(active) && active > 0 ? active : null,
  };
}

const HELP_URLS: Record<string, string> = {
  'app.help': 'https://github.com/Orthic-Labs/ember/tree/main/docs',
  'app.discord': 'https://github.com/Orthic-Labs/ember/issues',
  'app.feedback': 'https://github.com/Orthic-Labs/ember/issues/new',
  'app.website': 'https://github.com/Orthic-Labs/ember',
  'app.github': 'https://github.com/Orthic-Labs/ember',
  'app.artcraft': 'https://github.com/Orthic-Labs',
};

export function DesktopProvider({ children }: { children: ReactNode }) {
  const bootstrap = useMemo(windowBootstrap, []);
  const isSecondary = bootstrap.secondary;
  const [snapshot, setSnapshot] = useState<DesktopSnapshot | null>(null);
  const [ui, setUiState] = useState<UiState>(DEFAULT_UI);
  const locale = normalizeLocale(ui.locale);
  const t = useCallback((source: string) => translate(locale, source), [locale]);
  const [dialog, setDialog] = useState<DialogState | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [histogram, setHistogram] = useState<unknown>(null);
  const snapshotRef = useRef<DesktopSnapshot | null>(null);
  const dialogRef = useRef<DialogState | null>(null);
  const prefHydrated = useRef(false);
  const savedUi = useRef<UiState | null>(null);
  const startupViewApplied = useRef(false);
  const enginePrefsApplied = useRef<{ gpu: boolean; memoryMb: number } | null>(null);
  const refreshInFlight = useRef<Promise<void> | null>(null);
  const noticeQueue = useRef<string[] | null>(null);
  const liveNotice = useRef<string | null>(null);
  const bootstrapSelectionApplied = useRef(false);
  const externalEditIds = useRef<Set<number>>(new Set());
  const externalReloadInFlight = useRef(false);
  const nativeFocused = useRef(typeof document === 'undefined' || document.hasFocus());

  snapshotRef.current = snapshot;
  dialogRef.current = dialog;

  const refresh = useCallback(async () => {
    if (refreshInFlight.current) return refreshInFlight.current;
    const operation = getSnapshot().then((next) => {
      setSnapshot(next);
      setError(null);
      if (!prefHydrated.current) {
        prefHydrated.current = true;
        const hydrated = preferenceUi(next.preferences);
        if (!isSecondary && !startupViewApplied.current) {
          startupViewApplied.current = true;
          const startupView = hydrated.startupView ?? DEFAULT_UI.startupView;
          if (startupView !== 'last') hydrated.view = startupView;
        }
        setUiState((current) => ({
          ...current,
          ...hydrated,
          ...(isSecondary ? { view: bootstrap.view ?? 'detail' } : {}),
        }));
      }
      const notices = Array.isArray(next.status?.notices) ? next.status.notices.filter((value): value is string => typeof value === 'string') : [];
      const previous = noticeQueue.current;
      const latest = notices.at(-1);
      const appended = previous === null
        ? notices.length > 0
        : notices.length > previous.length && previous.every((value, index) => notices[index] === value)
          || notices.length > 0 && notices.at(-1) !== previous.at(-1);
      const emittedByEvent = liveNotice.current;
      liveNotice.current = null;
      // Snapshot notices are a durable queue. Emit only on queue growth/replacement;
      // dismissing notice then polling same snapshot cannot resurrect it.
      if (latest && appended && emittedByEvent !== latest) setNotice(latest);
      noticeQueue.current = notices;
    }).catch((reason: unknown) => {
      setError(messageOf(reason));
    }).finally(() => {
      refreshInFlight.current = null;
    });
    refreshInFlight.current = operation;
    return operation;
  }, [bootstrap.view, isSecondary]);

  useEffect(() => { void refresh(); }, [refresh]);

  // Host snapshot is authoritative, but app-level rendering preferences are engine commands;
  // apply them once after persisted UI settings hydrate, before preview requests begin.
  useEffect(() => {
    if (isSecondary) return;
    if (!snapshot || !prefHydrated.current) return;
    const previous = enginePrefsApplied.current;
    if (previous?.gpu === ui.gpu && previous.memoryMb === ui.memoryMb) return;
    enginePrefsApplied.current = { gpu: ui.gpu, memoryMb: ui.memoryMb };
    const commands: Array<Promise<unknown>> = [];
    if (!previous || previous.gpu !== ui.gpu) commands.push(runCommand('app.gpu', { enabled: ui.gpu }));
    if ((!previous || previous.memoryMb !== ui.memoryMb) && ui.memoryMb >= 64) commands.push(runCommand('app.memoryBudget', { mb: ui.memoryMb }));
    if (!commands.length) return;
    void Promise.allSettled(commands).then((results) => {
      const rejected = results.find((result): result is PromiseRejectedResult => result.status === 'rejected');
      if (rejected) setError(messageOf(rejected.reason));
    });
  }, [isSecondary, snapshot, ui.gpu, ui.memoryMb]);

  useEffect(() => {
    let timer: number | undefined;
    let live = true;
    const tick = () => {
      if (!live) return;
      void refresh();
      const active = snapshot?.status.importing || snapshot?.status.exporting || snapshot?.status.previewBuild || (snapshot?.status.jobs.length ?? 0) > 0;
      timer = window.setTimeout(tick, active ? 250 : 1200);
    };
    timer = window.setTimeout(tick, 1200);
    return () => { live = false; if (timer !== undefined) window.clearTimeout(timer); };
  }, [refresh, snapshot?.status.exporting, snapshot?.status.importing, snapshot?.status.jobs.length, snapshot?.status.previewBuild]);

  useEffect(() => {
    if (isSecondary || !prefHydrated.current) return;
    // Save only the fields this window changed. Writing the whole UI state back would
    // overwrite preferences patched from elsewhere (agents, the control channel) with
    // the copy this window hydrated from.
    const saved = savedUi.current;
    if (!saved) {
      savedUi.current = ui;
      return;
    }
    const patch = Object.fromEntries(
      Object.entries(ui).filter(([key, value]) => JSON.stringify(value) !== JSON.stringify(saved[key as keyof UiState])),
    ) as Partial<UiState>;
    if (Object.keys(patch).length === 0) return;
    const timer = window.setTimeout(() => {
      void savePreferences({ ui: patch as unknown as JsonObject })
        .then(() => { savedUi.current = { ...(savedUi.current ?? saved), ...patch }; })
        .catch((reason: unknown) => setError(messageOf(reason)));
    }, 350);
    return () => window.clearTimeout(timer);
  }, [isSecondary, ui]);

  useEffect(() => {
    if (!isSecondary || bootstrap.active === null || !snapshot || bootstrapSelectionApplied.current) return;
    bootstrapSelectionApplied.current = true;
    if (snapshot.active === bootstrap.active && snapshot.selection.includes(bootstrap.active)) return;
    void runCommand('library.select', { ids: [bootstrap.active], active: bootstrap.active, mode: 'replace' })
      .then(() => refresh())
      .catch((reason: unknown) => setError(messageOf(reason)));
  }, [bootstrap.active, isSecondary, refresh, snapshot]);

  const setUi = useCallback((patch: Partial<UiState> | ((current: UiState) => Partial<UiState>)) => {
    setUiState((current) => {
      const change = typeof patch === 'function' ? patch(current) : patch;
      const next = { ...current, ...clampUi(change, current) };
      if (change.locale !== undefined) next.locale = normalizeLocale(change.locale);
      return next;
    });
  }, []);

  const reloadExternalEdits = useCallback(async () => {
    if (externalReloadInFlight.current || externalEditIds.current.size === 0) return;
    externalReloadInFlight.current = true;
    const ids = [...externalEditIds.current];
    try {
      const result = await runCommand('photo.reload', { ids });
      const payload = objectOf(result);
      if (!payload || !Array.isArray(payload.reloaded) || !payload.reloaded.every((id) => typeof id === 'number' && Number.isSafeInteger(id) && id > 0)) {
        throw new Error('photo.reload returned an invalid result');
      }
      const reloaded = new Set(payload.reloaded as number[]);
      reloaded.forEach((id) => externalEditIds.current.delete(id));
      await refresh();
      const changed = [...reloaded].filter((id) => ids.includes(id)).length;
      setNotice(changed ? `Reloaded ${changed} external edit${changed === 1 ? '' : 's'}` : 'Checked external edits');
    } catch (reason: unknown) {
      setError(messageOf(reason));
    } finally {
      externalReloadInFlight.current = false;
    }
  }, [refresh]);

  const run = useCallback(async (id: string, params: JsonObject = {}): Promise<unknown> => {
    try {
      if ((id === 'photo.delete' || id === 'photo.deletePermanently') && ui.confirmDelete && params.confirmed !== true) {
        setDialog({ kind: 'confirmDelete', params: { ...params, command: id } });
        return null;
      }
      if ((UI_COMMAND_IDS as readonly string[]).includes(id)) {
        if (id === 'view.secondWindow') {
          const active = typeof params.active === 'number' ? params.active : snapshotRef.current?.active ?? null;
          const view = typeof params.view === 'string' && (BOOTSTRAP_VIEWS as readonly string[]).includes(params.view)
            ? params.view as ViewMode
            : 'detail';
          return openSecondWindow(view, active);
        }
        const nextDialog = dialogForCommand(id);
        if (id === 'app.shortcuts') {
          const shortcuts = allUiMetadata(locale).filter((entry) => entry.shortcut !== null);
          setDialog({ kind: 'shortcuts', params: { shortcuts } });
        } else if (nextDialog) setDialog(nextDialog);
        setUi((current) => applyUiCommand(id, current) ?? {});
        if (id === 'view.fullScreenPreview') return nativeAction('toggleFullscreen', { ...params, enabled: params.enabled });
        if (id === 'view.enterFullScreen') return nativeAction('fullscreen', params);
        if (id === 'app.quit') return nativeAction('closeWindow', params);
        if (id === 'app.help' || id === 'app.discord' || id === 'app.feedback' || id === 'app.website' || id === 'app.github' || id === 'app.artcraft') {
          return nativeAction('openUrl', { url: HELP_URLS[id] });
        }
        if (id === 'app.openLibrary') {
          // Native host owns quiescence, persistence & Session replacement. `library.browse`
          // only changes Local source, so it must never back this menu action.
          const value = await nativeAction('openLibrary', params);
          await refresh();
          return value;
        }
        if (id === 'file.addPhotos' || id === 'file.addFolder' || id === 'file.addFromDevice') {
          const paths = pathsFrom(params.paths ?? params.path);
          setDialog({ kind: 'import', params: { paths, initialSource: id === 'file.addFolder' ? 'folder' : id === 'file.addFromDevice' ? 'device' : 'files', mode: id === 'file.addFromDevice' ? 'copy' : 'add' } });
          return null;
        }
        if (id === 'file.importLightroom') {
          const path = firstPath(params.path) || firstPath(await nativeAction('pickLightroomCatalog', params));
          if (!path) return null;
          const task = await runCommand('library.inspectLightroom', { path }) as JsonObject;
          const taskId = typeof task.taskId === 'string' ? task.taskId : '';
          if (!taskId) throw new Error('LRCAT inspection did not return a task ID.');
          setDialog({ kind: 'lightroomInspectProgress', params: { taskId, path } });
          return task;
        }
        if (id === 'file.findMissing') {
          const folder = firstPath(params.folder) || firstPath(await nativeAction('chooseFolder', params));
          if (!folder) return null;
          const value = await runCommand('library.findMissing', { ...params, folder });
          await refresh();
          return value;
        }
        if (id === 'file.backupLibrary' || id === 'file.restoreLibrary') {
          return nativeAction(id === 'file.backupLibrary' ? 'backupLibrary' : 'restoreLibrary', params);
        }
        if (id === 'photo.locate') {
          const idValue = params.id ?? snapshotRef.current?.active;
          if (typeof idValue !== 'number') throw new Error('no photo selected');
          const path = firstPath(params.path) || firstPath(await nativeAction('chooseFiles', { ...params, multiple: false }));
          if (!path) return null;
          const value = await runCommand('photo.relink', { ...params, id: idValue, path });
          await refresh();
          return value;
        }
        if (id === 'photo.editInExternal') {
          const result = await runCommand('photo.editExternal', params) as JsonObject;
          const editId = externalEditId(result);
          if (editId !== null) {
            externalEditIds.current.add(editId);
            while (externalEditIds.current.size > MAX_EXTERNAL_EDIT_IDS) {
              const oldest = externalEditIds.current.values().next().value;
              if (typeof oldest !== 'number') break;
              externalEditIds.current.delete(oldest);
            }
          }
          const path = firstPath(result?.path);
          const requestedEditor = typeof params.app === 'string' && params.app.trim() ? params.app : ui.externalEditor;
          if (path) await nativeAction('openExternalEditor', { path, app: requestedEditor || undefined });
          return result;
        }
        if (id === 'app.showInFinder') {
          const inspected = await runCommand('photo.inspect', params) as JsonObject;
          const path = firstPath(inspected?.path ?? inspected?.filePath);
          if (!path) throw new Error('active photo has no file path');
          return nativeAction('reveal', { path });
        }
        if (id === 'photo.tagFromTracklog') {
          const path = firstPath(params.path) || firstPath(await nativeAction('pickTracklog', params));
          if (!path) return null;
          if (params.offset === undefined) {
            setDialog({ kind: 'textPrompt', params: { title: 'Auto-Tag from Tracklog', hint: 'Camera time zone, e.g. -07:00 (empty: UTC)', command: 'photo.autoTagTracklog', key: 'offset', path } });
            return null;
          }
          const value = await runCommand('photo.autoTagTracklog', { ...params, path });
          await refresh();
          return value;
        }
        if (id === 'file.importPresets') {
          const selected = pathsFrom(params.paths).length ? params.paths : await nativeAction('pickPresetFiles', params);
          const paths = pathsFrom(selected);
          if (!paths.length) return null;
          const value = await runCommand('preset.import', { paths });
          await refresh();
          return value;
        }
        if (id === 'file.exportPresets' || id === 'file.exportCurvePresets') {
          const curve = id === 'file.exportCurvePresets';
          const path = firstPath(params.path) || firstPath(await nativeAction(curve ? 'saveCurvePresetFile' : 'savePresetFile', params));
          if (!path) return null;
          const value = await runCommand(curve ? 'curve.exportPresets' : 'preset.export', { ...params, path });
          await refresh();
          return value;
        }
        if (id === 'file.importCurvePresets') {
          const selected = pathsFrom(params.paths).length ? params.paths : await nativeAction('pickCurvePresetFiles', params);
          const paths = pathsFrom(selected);
          if (!paths.length) return null;
          const value = await runCommand('curve.importPresets', { paths });
          await refresh();
          return value;
        }
        if (id === 'app.systemInfo') {
          const info = await runCommand('library.info', params);
          setDialog({ kind: 'systemInfo', params: { rows: Array.isArray(info) ? info : [] } });
          return info;
        }
        if (id === 'local.addRoot') {
          const path = firstPath(params.path) || firstPath(await nativeAction('chooseFolder', params));
          if (!path) return null;
          const value = await runCommand('local.addRoot', { ...params, path });
          await refresh();
          return value;
        }
        if (id === 'local.hide' || id === 'local.restoreHidden' || id === 'compare.swap' || id === 'compare.makeSelect') {
          const value = await runCommand(id, params);
          await refresh();
          return value;
        }
        if (id === 'photo.setReference') {
          const referenceId = typeof params.id === 'number' ? params.id : snapshotRef.current?.active;
          if (typeof referenceId === 'number') setUi({ referenceId });
          return { reference: referenceId ?? null };
        }
        if (id === 'view.reference' && snapshotRef.current?.active !== null && snapshotRef.current?.active !== undefined) {
          setUi({ referenceId: snapshotRef.current.active });
          return { reference: snapshotRef.current.active };
        }
        if (id === 'dialog.allMetadata') {
          const metadata = await runCommand('photo.allMetadata', params);
          setDialog({ kind: 'allMetadata', params: { metadata } });
          return metadata;
        }
        if (id === 'app.export' || id === 'app.exportPrevious') {
          const value = await runCommand(id, params);
          await refresh();
          return value;
        }
        if (id === 'merge.hdrLast' || id === 'merge.panoramaLast' || id === 'merge.hdrPanoramaLast') {
          const kind = id.slice('merge.'.length).replace('Last', '');
          setDialog({ kind: `merge.${kind}`, params: { ...params, last: true } });
          return null;
        }
        if (id === 'view.focusSearch') {
          setUi({ filterBar: true });
          window.requestAnimationFrame(() => document.querySelector<HTMLInputElement>('.lc-filter-search input')?.focus());
          return null;
        }
        if (id === 'edit.selectOnlyActive') {
          // Edit ▸ Select Only Active Photo: the selection shrinks to the active photo
          const active = snapshotRef.current?.active;
          if (active === null || active === undefined) return null;
          const value = await runCommand('library.select', { ids: [active], active, mode: 'replace' });
          await refresh();
          return value;
        }
        if (id === 'view.back' && dialogRef.current) { setDialog(null); return null; }
        if (id === 'dialog.export') { setDialog({ kind: 'export', params }); return null; }
        if (id === 'dialog.copySettings') { setDialog({ kind: 'copySettings', params }); return null; }
        if (id === 'dialog.pasteSettings') { setDialog({ kind: 'pasteSettings', params }); return null; }
        if (id === 'dialog.mergeHdr' || id === 'dialog.mergePanorama' || id === 'dialog.mergeHdrPanorama') {
          const kind = id === 'dialog.mergeHdr' ? 'merge.hdr' : id === 'dialog.mergePanorama' ? 'merge.panorama' : 'merge.hdrPanorama';
          setDialog({ kind, params });
          return null;
        }
        if (id === 'dialog.saveMetadataPreset') { setDialog({ kind: 'saveMetadataPreset', params }); return null; }
        if (id === 'dialog.cull') { setDialog({ kind: 'cull', params }); return null; }
        return null;
      }
      const value = await runCommand(id, params);
      await refresh();
      return value;
    } catch (reason: unknown) {
      const text = messageOf(reason);
      if (text.startsWith('The SAM 3 model is not installed')) setDialog({ kind: 'samModelSetup', params: { error: text } });
      setError(text);
      throw reason;
    }
  }, [locale, refresh, setUi, ui.confirmDelete, ui.externalEditor]);

  const native = useCallback(async (action: string, params: JsonObject = {}): Promise<unknown> => {
    try {
      if (isSecondary && action === 'preferences.patch') {
        // Secondary windows have private UI preferences; never write their changes
        // into main window's persisted host preferences.
        const local = { ui: params.ui };
        setUi(preferenceUi(local));
        return local;
      }
      const nativeParams = action === 'secondWindow'
        ? {
            ...params,
            view: typeof params.view === 'string' && (BOOTSTRAP_VIEWS as readonly string[]).includes(params.view) ? params.view : 'detail',
            ...(typeof params.active === 'number' ? {} : snapshotRef.current?.active == null ? {} : { active: snapshotRef.current.active }),
          }
        : params;
      const result = action === 'secondWindow'
        ? await openSecondWindow(nativeParams.view as ViewMode, typeof nativeParams.active === 'number' ? nativeParams.active : null)
        : await nativeAction(action, nativeParams);
      if (action === 'preferences.patch' && result && typeof result === 'object' && !Array.isArray(result)) {
        // Persistence may lag current workspace state while its save is debounced.
        // Apply only requested fields; the full response can contain older UI state.
        const patch = preferenceUi(params);
        if (Object.keys(patch).length) setUi(patch);
      }
      setError(null);
      return result;
    } catch (reason: unknown) {
      const text = messageOf(reason);
      setError(text);
      throw reason;
    }
  }, [isSecondary, setUi]);

  useEffect(() => {
    let live = true;
    let unlisten: Array<() => void> = [];
    const expectedLabel = isSecondary ? 'second-main' : 'main';
    const focusChanged = (focused: boolean, label?: unknown) => {
      if (typeof label === 'string' && label !== expectedLabel) return;
      if (!focused) {
        nativeFocused.current = false;
        return;
      }
      const returned = !nativeFocused.current;
      nativeFocused.current = true;
      if (returned) void reloadExternalEdits();
    };
    const onBlur = () => focusChanged(false);
    const onFocus = () => focusChanged(true);
    window.addEventListener('blur', onBlur);
    window.addEventListener('focus', onFocus);
    void Promise.all([
      listen<JsonObject>('lc://native-drop', (event) => {
        if (!live) return;
        const paths = pathsFrom(event.payload);
        if (!paths.length) return;
        void nativeAction('dropImport', { paths }).then((validated) => {
          const accepted = pathsFrom(validated);
          if (accepted.length) void run('file.addPhotos', { paths: accepted });
        }).catch((reason: unknown) => setError(messageOf(reason)));
      }),
      listen<JsonObject>('lc://error', (event) => { if (live) setError(messageOf(event.payload)); }),
      listen<JsonObject>('lc://notice', (event) => {
        if (!live) return;
        const message = messageOf(event.payload);
        liveNotice.current = message;
        setNotice(message);
      }),
      listen<JsonObject>('lc://window-focus', (event) => {
        if (!live) return;
        focusChanged(event.payload?.focused === true, event.payload?.label);
      }),
      listen<JsonObject>('lc://close-requested', (event) => {
        if (!live) return;
        if (event.payload?.unsaved === true) {
          setDialog({ kind: 'unsavedQuit', params: { ...event.payload } });
          void refresh();
          return;
        }
        if (event.payload?.active === true) {
          const message = typeof event.payload.message === 'string' && event.payload.message.length > 0
            ? event.payload.message
            : 'A task is still running. Cancel it or wait for it to finish before closing.';
          setNotice(message);
          void refresh();
        }
      }),
    ]).then((stops) => { if (live) unlisten = stops; else stops.forEach((stop) => stop()); }).catch(() => undefined);
    return () => {
      live = false;
      window.removeEventListener('blur', onBlur);
      window.removeEventListener('focus', onFocus);
      unlisten.forEach((stop) => stop());
    };
  }, [isSecondary, reloadExternalEdits, run]);

  const value = useMemo<DesktopContextValue>(() => ({ snapshot, ui, locale, t, setUi, run, native, refresh, dialog, setDialog, error, notice, setNotice, histogram, setHistogram }), [dialog, error, histogram, locale, native, notice, refresh, run, setUi, snapshot, t, ui]);
  return <DesktopContext.Provider value={value}>{children}</DesktopContext.Provider>;
}

export function useDesktop(): DesktopContextValue {
  const value = useContext(DesktopContext);
  if (!value) throw new Error('useDesktop must be used inside DesktopProvider');
  return value;
}
