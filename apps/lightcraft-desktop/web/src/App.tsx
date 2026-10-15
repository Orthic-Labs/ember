import { useCallback, useEffect, useMemo, useRef, useState, type CSSProperties, type KeyboardEvent as ReactKeyboardEvent, type ReactNode } from 'react';
import { AppShell, SegmentedControl, ShellProvider, createThemeStore, useCommands, useShortcuts, type Command, type NavGroup } from '@rightkit/react-shell/react';
import { getPlatform } from '@rightkit/platform-ui';
import { createShell } from '@rightkit/tauri-shell';
import { DesktopProvider, useDesktop } from './desktop';
import { paletteCommands } from './commands';
import { Icon } from './icons';
import { Filmstrip, LibraryShellSidebar, LibraryWorkspace, libraryGroups, type LibraryGroup } from './library';
import { StageWorkspace } from './stage/StageWorkspace';
import { Inspector } from './inspector';
import { RetainedLayout } from './layout/RetainedLayout';
import { DialogHost } from './dialogs';
import { BackgroundActivity } from './BackgroundActivity';
import { APP_NAME } from './branding';
import './app.css';

const themeStore = createThemeStore({ storageKey: 'lightcraft.theme', storage: null });
const shell = createShell();

const sourceIcons: Record<string, 'library' | 'search' | 'import' | 'export' | 'chevron'> = {
  photos: 'library', clock: 'search', 'flag-pick': 'export', trash: 'chevron', album: 'library', folder: 'import', sparkles: 'export', calendar: 'search', warning: 'chevron',
};

const builtinSourceIds = new Set(['all', 'recently-added', 'picks', 'recently-deleted', 'by-date', 'local', 'missing']);

function sourceNav(groups: LibraryGroup[], t: (source: string) => string): NavGroup[] {
  return groups.map((group) => ({
    title: t(group.title),
    items: group.items.map((item) => ({ id: item.id, label: builtinSourceIds.has(item.id) ? t(item.label) : item.label, icon: <Icon name={sourceIcons[item.icon] ?? 'library'} />, badge: item.count === undefined ? undefined : String(item.count), keywords: [item.id, item.icon, item.label] })),
  }));
}

function sourceId(snapshot: { source: unknown } | null): string {
  const source = snapshot?.source;
  if (!source || typeof source !== 'object') return 'all';
  const value = source as { kind?: unknown; id?: unknown };
  if (value.kind === 'album' && value.id !== undefined) return `album:${String(value.id)}`;
  if (typeof value.kind === 'string') {
    if (value.kind === 'folder') return 'local';
    return value.kind === 'all' ? 'all' : value.kind.replace(/[A-Z]/g, (part) => `-${part.toLowerCase()}`);
  }
  return 'all';
}

function ShortcutBindings({ commands }: { commands: Command[] }) {
  useShortcuts(commands.filter((command) => command.chord).map((command) => ({
    id: command.id,
    chord: String(command.chord).replace(/Cmd/g, 'mod').replace(/Ctrl/g, 'mod'),
    label: command.label,
    group: command.group,
    run: () => { command.run(); },
    preventDefault: true,
  })));
  return null;
}

function resizeInspectorByKey(event: ReactKeyboardEvent<HTMLButtonElement>, width: number, setUi: (patch: { inspectorWidth: number }) => void) {
  const step = event.shiftKey ? 32 : 16;
  if (event.key === 'ArrowLeft') { event.preventDefault(); setUi({ inspectorWidth: width + step }); }
  else if (event.key === 'ArrowRight') { event.preventDefault(); setUi({ inspectorWidth: width - step }); }
  else if (event.key === 'Home') { event.preventDefault(); setUi({ inspectorWidth: 260 }); }
  else if (event.key === 'End') { event.preventDefault(); setUi({ inspectorWidth: 520 }); }
}

function WorkspaceModeSwitch() {
  const { ui, t, setUi, run } = useDesktop();
  const value = ui.view === 'photoGrid' || ui.view === 'squareGrid' ? 'library' : 'develop';
  const change = (next: 'library' | 'develop') => {
    if (next === value) return;
    if (next === 'library') {
      setUi({ view: 'photoGrid', panel: 'info' });
      void run('view.photoGrid', {});
    } else {
      setUi({ view: 'detail', panel: 'edit' });
      void run('view.detail', {});
    }
  };
  return <SegmentedControl
    value={value}
    options={[{ value: 'library', label: t('Library'), title: t('Library') }, { value: 'develop', label: t('Develop'), title: t('Develop') }]}
    onChange={change}
    label={t('Workspace')}
  />;
}

function Workspace() {
  const desktop = useDesktop();
  const { snapshot, ui, locale, t, setUi, run, native, error, notice } = desktop;
  // Rust preferences own persisted choice; RightKit applies matching CSS tokens.
  useEffect(() => { themeStore.save(ui.theme); }, [ui.theme]);
  const groups = useMemo(() => libraryGroups(snapshot), [snapshot]);
  const navGroups = useMemo(() => sourceNav(groups, t), [groups, t]);
  const navItems = useMemo(() => groups.flatMap((group) => group.items), [groups]);
  const navigate = useCallback((id: string) => {
    const item = navItems.find((candidate) => candidate.id === id);
    if (!item) return;
    setUi({ view: 'photoGrid' });
    if (item.nativeAction) void native(item.nativeAction, item.params ?? {}).then((result) => {
      const path = result && typeof result === 'object' && 'path' in result && typeof (result as { path?: unknown }).path === 'string' ? (result as { path: string }).path : undefined;
      if (path) void run(item.command, { ...(item.params ?? {}), path });
    }).catch(() => undefined);
    else void run(item.command, item.params ?? {});
  }, [navItems, native, run, setUi]);
  const commands = useMemo(() => paletteCommands(snapshot, (id) => run(id), locale), [locale, run, snapshot]);
  useCommands(commands);
  useEffect(() => {
    let live = true;
    void (async () => {
      await shell.ready();
      await Promise.resolve();
      if (!live) return;
      document.documentElement.dataset.lightcraftReady = 'true';
      void native('startupReady', {});
    })();
    return () => { live = false; };
  }, [native]);
  return <>
    <ShortcutBindings commands={commands} />
    <AppShell
      groups={navGroups}
      activeId={sourceId(snapshot)}
      onNavigate={navigate}
      sidebar={<LibraryShellSidebar groups={navGroups} sectionIds={groups.map((group) => group.id)} activeId={sourceId(snapshot)} onNavigate={navigate} />}
      title={<WorkspaceModeSwitch />}
      wordmark={<span className="lc-wordmark">{APP_NAME}</span>}
      sidebarWidth={ui.sidebarCollapsed ? 48 : ui.sidebarWidth}
      sidebarCollapsed={ui.sidebarCollapsed}
      onSidebarCollapsedChange={(collapsed) => setUi({ sidebarCollapsed: collapsed })}
      bridge={shell.bridge()}
      platform={getPlatform()}
      theme={ui.theme}
      onThemeChange={(theme) => { setUi({ theme }); }}
      labels={{
        jumpTo: t('Jump to'), sections: t('Sections'), settings: t('Settings'), appearance: t('Appearance'),
        toggleSidebar: t('Toggle sidebar'), goTo: t('Go to'),
        theme: { system: t('System'), light: t('Light'), dark: t('Dark') },
      }}
      commands={commands}
      titlebarEnd={<div className="lc-title-status">{snapshot?.status.unsaved && <span className="lc-status-dot" title={t('Unsaved changes')} />} <span aria-live="polite">{error && <span className="lc-title-error">{error}</span>}{notice && !error && <span>{notice}</span>}</span><BackgroundActivity /></div>}
    >
      <div className={`lc-content ${ui.sidebarCollapsed ? 'is-shell-collapsed' : ''}`}>
        <WorkspaceLayout />
      </div>
    </AppShell>
    <DialogHost />
  </>;
}

function WorkspaceLayout() {
  const { ui, t, setUi } = useDesktop();
  const isLibrary = ui.view === 'photoGrid' || ui.view === 'squareGrid';
  const [viewportKey, setViewportKey] = useState(() => typeof window === 'undefined' ? 'unknown' : `${window.innerWidth}x${window.innerHeight}`);
  const drag = useRef<{ x: number; width: number } | null>(null);
  const [dragging, setDragging] = useState(false);
  const liveGeometryKey = `${isLibrary ? 'library' : 'stage'}:${ui.inspectorCollapsed ? 'collapsed' : 'expanded'}:${ui.inspectorWidth}:${ui.sidebarCollapsed ? 48 : ui.sidebarWidth}:${viewportKey}`;
  const [committedGeometryKey, setCommittedGeometryKey] = useState(liveGeometryKey);
  useEffect(() => {
    const update = () => setViewportKey(`${window.innerWidth}x${window.innerHeight}`);
    window.addEventListener('resize', update);
    window.visualViewport?.addEventListener('resize', update);
    return () => {
      window.removeEventListener('resize', update);
      window.visualViewport?.removeEventListener('resize', update);
    };
  }, []);
  useEffect(() => {
    if (!dragging) setCommittedGeometryKey(liveGeometryKey);
  }, [dragging, liveGeometryKey]);
  useEffect(() => {
    if (!dragging) return;
    const move = (event: PointerEvent) => { if (!drag.current) return; setUi({ inspectorWidth: drag.current.width - (event.clientX - drag.current.x) }); };
    const up = () => { drag.current = null; setDragging(false); };
    window.addEventListener('pointermove', move); window.addEventListener('pointerup', up);
    return () => { window.removeEventListener('pointermove', move); window.removeEventListener('pointerup', up); };
  }, [dragging, setUi]);
  const layoutClass = `${isLibrary ? 'lc-library-layout' : 'lc-stage-layout'}${ui.inspectorCollapsed ? ' is-inspector-collapsed' : ''}`;
  return <RetainedLayout geometryKey={committedGeometryKey} className={layoutClass} style={{ '--lc-inspector': `${ui.inspectorWidth}px` } as CSSProperties}>
    {isLibrary ? <div className="lc-library-center"><LibraryWorkspace showSidebar={false} />{ui.filmstrip && <Filmstrip />}</div> : <StageWorkspace />}
    {!ui.inspectorCollapsed && <button className="lc-inspector-resizer" type="button" role="slider" tabIndex={0} aria-orientation="vertical" aria-valuemin={260} aria-valuemax={520} aria-valuenow={ui.inspectorWidth} aria-label={t('Resize inspector')} onKeyDown={(event) => resizeInspectorByKey(event, ui.inspectorWidth, setUi)} onPointerDown={(event) => { drag.current = { x: event.clientX, width: ui.inspectorWidth }; setDragging(true); event.currentTarget.setPointerCapture(event.pointerId); }} />}
    <Inspector key="workspace-inspector" className={ui.inspectorCollapsed ? 'is-collapsed' : ''} />
  </RetainedLayout>;
}

function LocalizedShell({ children }: { children: ReactNode }) {
  const { t } = useDesktop();
  return <ShellProvider
    bridge={shell.bridge()}
    themeStore={themeStore}
    platform={getPlatform()}
    paletteLabels={{ placeholder: t('Search or run a command…'), title: t('Command palette'), empty: t('No matches.'), results: t('Results') }}
    helpTitle={t('Keyboard shortcuts')}
  >{children}</ShellProvider>;
}

export default function App() {
  return <DesktopProvider><LocalizedShell><Workspace /></LocalizedShell></DesktopProvider>;
}
