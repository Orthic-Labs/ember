import { APP_NAME } from '../branding';
import { useCallback, useEffect, useMemo, useRef, useState, type CSSProperties, type MouseEvent, type KeyboardEvent, type UIEvent } from 'react';
import { navNeighborIndex } from '@rightkit/react-shell';
import { AppearanceMenu, NavList, PaletteTrigger, useShell, type NavGroup } from '@rightkit/react-shell/react';
import type { DesktopSnapshot, PhotoSummary } from '../types';
import { useDesktop } from '../desktop';
import { RetainedLayout } from '../layout/RetainedLayout';
import { PhotoPreview } from '../preview/PhotoPreview';
import { libraryGroups, usePhotoSlice, type LibraryNavItem } from './libraryData';
import './library.css';

// One bounded page spans a normal viewport, so a justified window stays contiguous at page edges.
const PAGE = 512;
const MAX_PAGE_GEOMETRY = 64;
const MIN_THUMB = 96;
const MAX_THUMB = 480;
const SCROLL_SYNC_FALLBACK_MS = 64;

type MenuState = { x: number; y: number; photo: PhotoSummary } | null;

function sourceKey(source: unknown): string {
  if (typeof source === 'string') return source;
  if (source && typeof source === 'object' && 'kind' in source) return String((source as { kind?: unknown }).kind ?? 'all');
  return 'all';
}

function photoAspect(photo: PhotoSummary): number {
  return photo.w > 0 && photo.h > 0 ? photo.w / photo.h : 1.5;
}

function boundedPageHeight(value: number | undefined, fallback: number): number {
  return typeof value === 'number' && Number.isFinite(value) && value > 0 ? value : fallback;
}

type JustifiedRow = { photos: PhotoSummary[]; height: number; aspectSum: number };
type JustifiedRowLayout = { row: JustifiedRow; top: number };

function buildJustifiedRows(photos: PhotoSummary[], width: number, targetHeight: number, gap: number): JustifiedRow[] {
  const result: JustifiedRow[] = [];
  let row: PhotoSummary[] = [];
  let aspectSum = 0;
  photos.forEach((photo) => {
    const aspect = photoAspect(photo);
    row.push(photo);
    aspectSum += aspect;
    if (row.length > 1 && aspectSum * targetHeight + gap * (row.length - 1) >= width) {
      result.push({ photos: row, height: Math.max(72, (width - gap * (row.length - 1)) / aspectSum), aspectSum });
      row = [];
      aspectSum = 0;
    }
  });
  if (row.length) {
    result.push({ photos: row, height: Math.min(targetHeight, Math.max(72, (width - gap * (row.length - 1)) / Math.max(aspectSum, 0.5))), aspectSum });
  }
  return result;
}

function layoutJustifiedRows(rows: JustifiedRow[], gap: number): JustifiedRowLayout[] {
  let top = 0;
  return rows.map((row, index) => {
    const result = { row, top };
    top += row.height + (index < rows.length - 1 ? gap : 0);
    return result;
  });
}

function visibleJustifiedRows(layout: JustifiedRowLayout[], start: number, end: number, overscan: number): { rows: JustifiedRow[]; tops: number[] } {
  const min = Math.max(0, start - overscan);
  const max = end + overscan;
  const visible = layout.filter(({ row, top }) => top + row.height >= min && top <= max);
  return { rows: visible.map(({ row }) => row), tops: visible.map(({ top }) => top) };
}

function PhotoBadges({ photo }: { photo: PhotoSummary }) {
  return (
    <div className="lc-photo-badges" aria-label="Photo metadata">
      {photo.flag === 'pick' && <span className="lc-badge lc-badge-pick" title="Picked">●</span>}
      {photo.flag === 'reject' && <span className="lc-badge lc-badge-reject" title="Rejected">×</span>}
      {photo.label && <span className={`lc-badge lc-badge-${photo.label}`} title={`${photo.label} label`} />}
      {photo.edited && <span className="lc-badge lc-badge-edited" title="Edited">✎</span>}
      {photo.previewOnly && <span className="lc-badge lc-badge-preview" title="Preview only">JPEG</span>}
    </div>
  );
}

function stars(rating: number): string {
  return rating > 0 ? `${'★'.repeat(Math.min(5, rating))}${'☆'.repeat(Math.max(0, 5 - rating))}` : '☆☆☆☆☆';
}

function navIsActive(snapshot: DesktopSnapshot | null, item: LibraryNavItem): boolean {
  if (!snapshot) return false;
  const source = sourceKey(snapshot.source);
  if (item.id === 'all') return source === 'all';
  if (item.id === 'recently-added') return source === 'recentlyAdded';
  if (item.id === 'picks') return source === 'picks';
  if (item.id === 'recently-deleted') return source === 'recentlyDeleted';
  if (item.id === 'local') return source === 'folder';
  if (item.params?.kind === 'album') {
    return source === 'album' && String((snapshot.source as { id?: unknown } | null)?.id ?? '') === String(item.params.id);
  }
  return false;
}

function NavIcon({ icon }: { icon: string }) {
  const glyph: Record<string, string> = { photos: '▦', clock: '◷', 'flag-pick': '⚑', trash: '⌫', album: '▱', folder: '□', sparkles: '✦', calendar: '▣', warning: '!' };
  return <span className={`lc-nav-icon lc-nav-icon-${icon}`} aria-hidden="true">{glyph[icon] ?? '·'}</span>;
}

type SidebarCollapseState = Record<string, boolean>;

export function LibraryShellSidebar({ groups, sectionIds, activeId, onNavigate }: { groups: readonly NavGroup[]; sectionIds: readonly string[]; activeId: string; onNavigate: (id: string) => void }) {
  const { ui, setUi, t } = useDesktop();
  const shell = useShell();
  const groupsRef = useRef<HTMLDivElement>(null);
  const collapsedSections = useMemo(() => Object.fromEntries(Object.entries(ui.sections).filter(([key, value]) => key.startsWith('sidebar:') && value === true).map(([key]) => [key.slice(8), true])), [ui.sections]);
  const toggleSection = (id: string) => {
    const key = `sidebar:${id}`;
    setUi({ sections: { ...ui.sections, [key]: !collapsedSections[id] } });
  };
  useEffect(() => {
    const buttons = Array.from(groupsRef.current?.querySelectorAll<HTMLButtonElement>('button[data-nav-id]') ?? []);
    if (!buttons.length) return;
    const selected = buttons.find((button) => button.dataset.navId === activeId) ?? buttons[0];
    buttons.forEach((button) => { button.tabIndex = button === selected ? 0 : -1; });
  }, [activeId, collapsedSections, groups, ui.sidebarCollapsed]);
  const onKeyDownCapture = (event: KeyboardEvent<HTMLDivElement>) => {
    const buttons = Array.from(groupsRef.current?.querySelectorAll<HTMLButtonElement>('button[data-nav-id]') ?? []);
    const target = event.target instanceof Element ? event.target.closest<HTMLButtonElement>('button[data-nav-id]') : null;
    if (!target) return;
    const current = buttons.indexOf(target);
    const next = navNeighborIndex('vertical', event.key, current < 0 ? 0 : current, buttons.length);
    if (next === null || !buttons[next]) return;
    event.preventDefault();
    event.stopPropagation();
    buttons[next].focus();
  };
  return <aside className="rk-side lc-shell-library-sidebar" aria-label={t('Sections')} hidden={ui.sidebarCollapsed}>
    <PaletteTrigger onOpen={shell.palette.show} label={t('Jump to')} />
    <div className="lc-shell-sidebar-groups" ref={groupsRef} onKeyDownCapture={onKeyDownCapture}>
      {groups.map((group, index) => {
        const id = sectionIds[index] ?? `group-${index}`;
        const sectionCollapsed = !ui.sidebarCollapsed && collapsedSections[id] === true;
        const visibleGroup = sectionCollapsed ? { ...group, items: [] } : group;
        return <section className={`lc-shell-sidebar-group${sectionCollapsed ? ' is-collapsed' : ''}`} key={id}>
          <button className="lc-shell-sidebar-group-header" type="button" aria-expanded={!sectionCollapsed} onClick={() => toggleSection(id)}>
            <span>{group.title}</span><span aria-hidden="true">{sectionCollapsed ? '›' : '⌄'}</span>
          </button>
          <NavList groups={[visibleGroup]} activeId={activeId} onNavigate={onNavigate} label={group.title} />
        </section>;
      })}
    </div>
    <div className="rk-side__foot"><footer className="rk-brand-foot"><span className="rk-wordmark">{APP_NAME}</span><AppearanceMenu value={ui.theme} onChange={(theme) => setUi({ theme })} labels={{ appearance: t('Appearance'), theme: { system: t('System'), light: t('Light'), dark: t('Dark') } }} /></footer></div>
  </aside>;
}

function SourceSidebar({ snapshot, collapsed, collapsedSections, onToggleSection, onNavigate }: { snapshot: DesktopSnapshot | null; collapsed: boolean; collapsedSections: SidebarCollapseState; onToggleSection: (id: string) => void; onNavigate: (item: LibraryNavItem) => void }) {
  const groups = useMemo(() => libraryGroups(snapshot), [snapshot]);
  return (
    <aside className={`lc-library-sidebar${collapsed ? ' is-collapsed' : ''}`} aria-label="Library sources">
      {groups.map((group) => {
        const sectionCollapsed = collapsedSections[group.id] === true;
        return <section className={`lc-source-group${sectionCollapsed ? ' is-collapsed' : ''}`} key={group.id}>
          {!collapsed && <button className="lc-source-group-header" type="button" aria-expanded={!sectionCollapsed} onClick={() => onToggleSection(group.id)}>
            <h2>{group.title}</h2><span className="lc-source-group-chevron" aria-hidden="true">{sectionCollapsed ? '›' : '⌄'}</span>
          </button>}
          {(!sectionCollapsed || collapsed) && group.items.map((item) => (
            <button
              className={`lc-source-item${navIsActive(snapshot, item) ? ' is-active' : ''}`}
              type="button"
              key={item.id}
              onClick={() => onNavigate(item)}
              title={collapsed ? item.label : undefined}
              aria-label={item.label}
              aria-current={navIsActive(snapshot, item) ? 'page' : undefined}
            >
              <NavIcon icon={item.icon} />
              {!collapsed && <span className="lc-source-label">{item.label}</span>}
              {!collapsed && item.count !== undefined && <span className="lc-source-count">{item.count}</span>}
            </button>
          ))}
        </section>
      })}
      {!collapsed && <div className="lc-sidebar-status" aria-live="polite">
        {snapshot?.status.unsaved && <span>Unsaved changes</span>}
        {snapshot?.status.importing && <span>Importing…</span>}
        {snapshot?.status.exporting && <span>Exporting…</span>}
        {!snapshot?.status.unsaved && !snapshot?.status.importing && !snapshot?.status.exporting && <span>Library ready</span>}
      </div>}
    </aside>
  );
}

function FilterBar({ snapshot, filterText, setFilterText }: { snapshot: DesktopSnapshot | null; filterText: string; setFilterText: (value: string) => void }) {
  const { run, setDialog } = useDesktop();
  const filter = snapshot?.filter as Record<string, unknown> | null;
  const [expanded, setExpanded] = useState(false);
  const [savedName, setSavedName] = useState('');
  const [presets, setPresets] = useState<Array<{ name: string }>>([]);
  const setFilter = (patch: Record<string, unknown>) => { void run('library.filter', patch); };
  const clear = () => { void run('library.clearFilter'); setFilterText(''); };
  const save = () => {
    const name = savedName.trim();
    if (name) { void run('filter.savePreset', { name }); setSavedName(''); }
  };
  useEffect(() => {
    if (!expanded) return;
    let live = true;
    void run('filter.presets', {}).then((value: unknown) => {
      if (!live || !Array.isArray(value)) return;
      setPresets(value.filter((item): item is { name: string } => Boolean(item && typeof item === 'object' && typeof (item as { name?: unknown }).name === 'string')));
    }).catch(() => undefined);
    return () => { live = false; };
  }, [expanded, run]);
  return (
    <div className={`lc-filter-bar${expanded ? ' is-expanded' : ''}`}>
      <label className="lc-filter-search">
        <span aria-hidden="true">⌕</span>
        <input value={filterText} onChange={(event) => setFilterText(event.target.value)} placeholder="Search photos" aria-label="Search photos" />
        {filterText && <button type="button" aria-label="Clear search" onClick={() => setFilterText('')}>×</button>}
      </label>
      <button className="lc-filter-pill" type="button" aria-pressed={Boolean(filter?.rating)} onClick={() => setFilter({ rating: filter?.rating ? 0 : 3, ratingOp: 'atLeast' })}>★ 3+</button>
      <button className="lc-filter-pill" type="button" aria-pressed={filter?.flag === 'pick'} onClick={() => setFilter({ flag: filter?.flag === 'pick' ? null : 'pick' })}>⚑ Picks</button>
      <button className="lc-filter-pill" type="button" aria-pressed={filter?.edited === true} onClick={() => setFilter({ edited: filter?.edited === true ? null : true })}>✎ Edited</button>
      <button className="lc-filter-more" type="button" aria-expanded={expanded} onClick={() => setExpanded((value) => !value)}>Filters <span aria-hidden="true">⌄</span></button>
      {(filter && Object.keys(filter).some((key) => filter[key] !== null && filter[key] !== undefined && filter[key] !== '' && filter[key] !== false && filter[key] !== 0)) && <button className="lc-filter-clear" type="button" onClick={clear}>Clear all</button>}
      {expanded && <div className="lc-filter-panel" role="region" aria-label="Full filters">
        <label>Rating <select value={String(filter?.rating ?? 0)} onChange={(event) => setFilter({ rating: Number(event.target.value) || 0 })}><option value="0">Any</option>{[1, 2, 3, 4, 5].map((rating) => <option key={rating} value={rating}>{rating}+ stars</option>)}</select></label>
        <label>Flag <select value={String(filter?.flag ?? '')} onChange={(event) => setFilter({ flag: event.target.value || null })}><option value="">Any</option><option value="pick">Picked</option><option value="reject">Rejected</option><option value="none">Unflagged</option></select></label>
        <label>Label <select value={String(filter?.label ?? '')} onChange={(event) => setFilter({ label: event.target.value || null })}><option value="">Any</option>{['red', 'yellow', 'green', 'blue', 'purple'].map((label) => <option key={label} value={label}>{label}</option>)}</select></label>
        <label>Kind <select value={String(filter?.kind ?? '')} onChange={(event) => setFilter({ kind: event.target.value || null })}><option value="">Any</option><option value="image">Image</option><option value="raw">Raw</option><option value="video">Video</option></select></label>
        <label>Date <input value={String(filter?.date ?? '')} onChange={(event) => setFilter({ date: event.target.value || null })} placeholder="YYYY-MM-DD" /></label>
        <label>Keyword <input value={String(filter?.keyword ?? '')} onChange={(event) => setFilter({ keyword: event.target.value || null })} /></label>
        <label>Camera <input value={String(filter?.camera ?? '')} onChange={(event) => setFilter({ camera: event.target.value || null })} /></label>
        <label className="lc-filter-check"><input type="checkbox" checked={filter?.edited === true} onChange={(event) => setFilter({ edited: event.target.checked ? true : null })} /> Edited only</label>
        {presets.length > 0 && <label>Saved <select defaultValue="" onChange={(event) => { if (event.target.value) void run('filter.applyPreset', { name: event.target.value }); }}><option value="">Choose…</option>{presets.map((preset) => <option key={preset.name} value={preset.name}>{preset.name}</option>)}</select></label>}
        <div className="lc-filter-save"><input value={savedName} onChange={(event) => setSavedName(event.target.value)} placeholder="Saved filter name" aria-label="Saved filter name" /><button type="button" onClick={save}>Save filter</button><button type="button" onClick={() => setDialog({ kind: 'smartAlbum', params: { fromFilter: true } })}>Smart album…</button></div>
      </div>}
    </div>
  );
}

function GridMenu({ menu, close, albums }: { menu: MenuState; close: () => void; albums: DesktopSnapshot['albums'] }) {
  const { run } = useDesktop();
  if (!menu) return null;
  const target = { ids: [menu.photo.id] };
  const execute = (id: string, params: Record<string, unknown> = {}) => { close(); void run(id, { ...params, ...target }); };
  return <div className="lc-grid-menu" role="menu" style={{ left: menu.x, top: menu.y }} onMouseLeave={close}>
    <button type="button" role="menuitem" onClick={() => execute('photo.pick')}>Pick <kbd>P</kbd></button>
    <button type="button" role="menuitem" onClick={() => execute('photo.reject')}>Reject <kbd>X</kbd></button>
    <button type="button" role="menuitem" onClick={() => execute('photo.unflag')}>Unflag <kbd>U</kbd></button>
    <div className="lc-menu-label">Rating</div>
    <div className="lc-menu-rating">{[1, 2, 3, 4, 5].map((rating) => <button key={rating} type="button" role="menuitem" aria-label={`${rating} stars`} onClick={() => execute('photo.rate', { rating })}>{rating <= menu.photo.rating ? '★' : '☆'}</button>)}</div>
    <div className="lc-menu-label">Label</div>
    <div className="lc-menu-labels">{['red', 'yellow', 'green', 'blue', 'purple', 'none'].map((label) => <button type="button" role="menuitem" className={`lc-label-dot ${label}`} key={label} aria-label={`${label} label`} onClick={() => execute('photo.label', { label })} />)}</div>
    <div className="lc-menu-label">Add to album</div>
    {albums.filter((album) => !album.folder && !album.smart).map((album) => <button type="button" role="menuitem" key={album.id} onClick={() => execute('album.addPhotos', { id: album.id })}>{album.name}</button>)}
    {albums.every((album) => album.folder || album.smart) && <span className="lc-menu-empty">No writable albums</span>}
    <button type="button" role="menuitem" onClick={() => execute('app.showInFinder')}>Reveal original</button>
    <button type="button" role="menuitem" onClick={() => execute(menu.photo.deleted ? 'photo.restore' : 'photo.delete')}>{menu.photo.deleted ? 'Restore' : 'Move to Recently Deleted'}</button>
  </div>;
}

function VirtualPhotoGrid({ snapshot, mode, thumbSize }: { snapshot: DesktopSnapshot; mode: 'photoGrid' | 'squareGrid'; thumbSize: number }) {
  const { run, ui, setUi } = useDesktop();
  const [scrollTop, setScrollTop] = useState(0);
  const [viewportHeight, setViewportHeight] = useState(600);
  const [viewportWidth, setViewportWidth] = useState(900);
  const [menu, setMenu] = useState<MenuState>(null);
  const pageHeights = useRef(new Map<number, number>());
  const retainedSlices = useRef(new Map<string, PhotoSummary[]>());
  const [geometryRevision, setGeometryRevision] = useState(0);
  const previousPageAnchor = useRef<{ page: number; top: number } | null>(null);
  const scrollRef = useRef<HTMLDivElement>(null);
  const pendingScrollTop = useRef(0);
  const scrollFrame = useRef<number | null>(null);
  const scrollFallback = useRef<number | null>(null);
  const scrollPoll = useRef<number | null>(null);
  const gap = 10;
  const columns = Math.max(1, Math.floor((viewportWidth + gap) / (thumbSize + gap)));
  const gridWidth = Math.max(1, viewportWidth - 32);
  const rowHeight = thumbSize + gap;
  const squareCellSize = Math.max(1, (gridWidth - gap * (columns - 1)) / columns);
  const squareRowHeight = squareCellSize + gap;
  const rows = Math.ceil(snapshot.total / columns);
  const isJustified = mode === 'photoGrid';
  const estimatedRowsPerPage = Math.max(1, Math.ceil(PAGE / columns));
  const estimatedPageHeight = Math.max(rowHeight, estimatedRowsPerPage * rowHeight + gap);
  const pageCount = Math.max(1, Math.ceil(snapshot.total / PAGE));
  const pageStart = (pageIndex: number): number => {
    const safeIndex = Math.max(0, Math.min(pageCount, pageIndex));
    let top = safeIndex * estimatedPageHeight + safeIndex * gap;
    for (const [knownPage, measuredHeight] of pageHeights.current) {
      if (knownPage < safeIndex) top += measuredHeight - estimatedPageHeight;
    }
    return Math.max(0, top);
  };
  const pageHeight = (pageIndex: number): number => boundedPageHeight(pageHeights.current.get(pageIndex), estimatedPageHeight);
  const resolvePage = (position: number): number => {
    if (!isJustified) return Math.max(0, Math.min(pageCount - 1, Math.floor(Math.max(0, position) / Math.max(1, estimatedPageHeight))));
    let low = 0;
    let high = pageCount - 1;
    while (low < high) {
      const middle = Math.ceil((low + high) / 2);
      if (pageStart(middle) <= position) low = middle;
      else high = middle - 1;
    }
    const index = low;
    return position >= pageStart(index) + pageHeight(index) && index < pageCount - 1 ? index + 1 : index;
  };
  const pageIndex = resolvePage(scrollTop);
  const chunkOffset = isJustified ? pageIndex * PAGE : Math.floor((Math.max(0, Math.floor(scrollTop / squareRowHeight) - 2) * columns) / PAGE) * PAGE;
  const chunkStartRow = Math.floor(chunkOffset / columns);
  const slice = usePhotoSlice(chunkOffset, PAGE);
  const sliceReady = slice.offset === chunkOffset && slice.generation === snapshot.viewGeneration && !slice.loading && !slice.error;
  const nextPageIndex = Math.min(pageCount - 1, pageIndex + 1);
  const nextChunkOffset = Math.min(snapshot.total, chunkOffset + PAGE);
  const nextSlice = usePhotoSlice(nextChunkOffset, PAGE);
  const nextSliceReady = nextChunkOffset > chunkOffset && nextSlice.offset === nextChunkOffset && nextSlice.generation === snapshot.viewGeneration && !nextSlice.loading && !nextSlice.error;
  const contextKey = JSON.stringify([snapshot.libraryPath ?? null, snapshot.source ?? null, snapshot.browse ?? null, snapshot.libraryFolder ?? null, snapshot.filter ?? null, snapshot.sort ?? '', snapshot.total, mode]);
  const sliceKey = (offset: number) => `${contextKey}:${offset}`;
  const currentSliceKey = sliceKey(chunkOffset);
  const nextSliceKey = sliceKey(nextChunkOffset);
  useEffect(() => {
    if (sliceReady) {
      retainedSlices.current.delete(currentSliceKey);
      retainedSlices.current.set(currentSliceKey, slice.photos);
    }
    if (nextSliceReady) {
      retainedSlices.current.delete(nextSliceKey);
      retainedSlices.current.set(nextSliceKey, nextSlice.photos);
    }
    while (retainedSlices.current.size > 4) {
      const oldest = retainedSlices.current.keys().next().value;
      if (oldest === undefined) break;
      retainedSlices.current.delete(oldest);
    }
  }, [currentSliceKey, nextSliceKey, nextSlice.photos, nextSliceReady, slice.photos, sliceReady]);
  const retainedCurrent = retainedSlices.current.get(currentSliceKey);
  const retainedNext = retainedSlices.current.get(nextSliceKey);
  const photos = sliceReady ? slice.photos : retainedCurrent ?? [];
  const nextCanRender = nextSliceReady || retainedNext !== undefined;
  const nextPhotos = nextSliceReady ? nextSlice.photos : retainedNext ?? [];
  const selected = new Set(snapshot.selection);
  const active = snapshot.active;
  const flushScrollTop = useCallback(() => {
    const frame = scrollFrame.current;
    scrollFrame.current = null;
    if (frame !== null) window.cancelAnimationFrame(frame);
    const fallback = scrollFallback.current;
    scrollFallback.current = null;
    if (fallback !== null) window.clearTimeout(fallback);
    const next = pendingScrollTop.current;
    setScrollTop((current) => current === next ? current : next);
  }, []);
  const scheduleScrollTop = useCallback((next: number) => {
    pendingScrollTop.current = Math.max(0, Number.isFinite(next) ? next : 0);
    if (scrollFrame.current === null) scrollFrame.current = window.requestAnimationFrame(flushScrollTop);
    // Hidden WebKit can suspend rAF; bounded timer keeps slice selection live.
    if (scrollFallback.current === null) scrollFallback.current = window.setTimeout(flushScrollTop, SCROLL_SYNC_FALLBACK_MS);
  }, [flushScrollTop]);
  const onScroll = (event: UIEvent<HTMLDivElement>) => scheduleScrollTop(event.currentTarget.scrollTop);
  useEffect(() => {
    const element = scrollRef.current;
    if (!element) return;
    const resize = () => {
      setViewportWidth(element.clientWidth);
      setViewportHeight(element.clientHeight);
      scheduleScrollTop(element.scrollTop);
    };
    resize();
    const observer = new ResizeObserver(resize);
    observer.observe(element);
    const nativeScroll = () => scheduleScrollTop(element.scrollTop);
    element.addEventListener('scroll', nativeScroll, { passive: true });
    const poll = () => {
      if (element.scrollTop !== pendingScrollTop.current) scheduleScrollTop(element.scrollTop);
      scrollPoll.current = window.setTimeout(poll, SCROLL_SYNC_FALLBACK_MS);
    };
    scrollPoll.current = window.setTimeout(poll, SCROLL_SYNC_FALLBACK_MS);
    return () => {
      observer.disconnect();
      element.removeEventListener('scroll', nativeScroll);
      const frame = scrollFrame.current;
      scrollFrame.current = null;
      if (frame !== null) window.cancelAnimationFrame(frame);
      const fallback = scrollFallback.current;
      scrollFallback.current = null;
      if (fallback !== null) window.clearTimeout(fallback);
      const pollHandle = scrollPoll.current;
      scrollPoll.current = null;
      if (pollHandle !== null) window.clearTimeout(pollHandle);
    };
  }, [scheduleScrollTop]);
  const select = (photo: PhotoSummary, event?: MouseEvent | KeyboardEvent) => {
    const modifier = Boolean(event && ('metaKey' in event ? event.metaKey : false) || event && ('ctrlKey' in event ? event.ctrlKey : false));
    const modeName = event?.shiftKey ? 'range' : modifier ? 'toggle' : 'replace';
    void run('library.select', { ids: [photo.id], active: photo.id, mode: modeName });
  };
  const aspectRows = useMemo(() => {
    return mode === 'photoGrid' && photos.length ? buildJustifiedRows(photos, gridWidth, thumbSize, gap) : [];
  }, [gap, gridWidth, mode, photos, thumbSize]);
  const nextAspectRows = useMemo(() => (isJustified && nextCanRender ? buildJustifiedRows(nextPhotos, gridWidth, thumbSize, gap) : []), [gap, gridWidth, isJustified, nextCanRender, nextPhotos, thumbSize]);
  const aspectRowLayout = useMemo(() => layoutJustifiedRows(aspectRows, gap), [aspectRows, gap]);
  const nextAspectRowLayout = useMemo(() => layoutJustifiedRows(nextAspectRows, gap), [gap, nextAspectRows]);
  const rowOverscan = Math.max(72, Math.min(viewportHeight * 0.75, rowHeight * 1.5));
  const moveSelection = (delta: number, event: KeyboardEvent) => {
    const visiblePhotos = nextSliceReady ? [...photos, ...nextPhotos] : photos;
    const visibleRows = nextSliceReady ? [...aspectRows, ...nextAspectRows] : aspectRows;
    const visibleIndex = active === null ? -1 : visiblePhotos.findIndex((photo) => photo.id === active);
    const currentIndex = visibleIndex >= 0 ? visibleIndex : 0;
    let targetLocalIndex = currentIndex + delta;
    const vertical = isJustified && Math.abs(delta) === columns;
    if (vertical && visibleIndex >= 0) {
      const currentRowIndex = visibleRows.findIndex((row) => row.photos.some((photo) => photo.id === active));
      if (currentRowIndex >= 0) {
        const row = visibleRows[currentRowIndex];
        const itemIndex = row.photos.findIndex((photo) => photo.id === active);
        const targetRow = visibleRows[currentRowIndex + (delta > 0 ? 1 : -1)];
        if (targetRow) {
          const targetItem = targetRow.photos[Math.min(targetRow.photos.length - 1, Math.round((itemIndex / Math.max(1, row.photos.length - 1)) * Math.max(0, targetRow.photos.length - 1)))];
          if (targetItem) targetLocalIndex = visiblePhotos.findIndex((photo) => photo.id === targetItem.id);
        }
      }
    }
    const index = Math.max(0, chunkOffset + targetLocalIndex);
    const targetIndex = Math.max(0, Math.min(snapshot.total - 1, index));
    const targetOffset = Math.floor(targetIndex / PAGE) * PAGE;
    if (targetOffset !== chunkOffset) {
      const targetPage = Math.floor(targetIndex / PAGE);
      const prefetchedTarget = visiblePhotos[targetIndex - chunkOffset];
      if (prefetchedTarget) select(prefetchedTarget, event);
      else scrollRef.current?.scrollTo({ top: isJustified ? pageStart(targetPage) : Math.floor(targetIndex / columns) * squareRowHeight });
      return;
    }
    const photo = visiblePhotos[targetIndex - chunkOffset];
    if (photo) select(photo, event);
  };
  const renderJustifiedRows = (rowSet: JustifiedRow[], keyPrefix: string, tops: number[] = []) => rowSet.map((row, rowIndex) => {
    const top = tops[rowIndex];
    const rowKey = row.photos[0]?.id ?? rowIndex;
    return <div className="lc-grid-row" style={top === undefined ? undefined : { position: 'absolute', top, left: 0, right: 0 }} key={`${keyPrefix}-${rowKey}`}>
      {row.photos.map((photo) => {
        const isSelected = selected.has(photo.id);
        const width = `${Math.max(0, ((gridWidth - gap * (row.photos.length - 1)) * photoAspect(photo)) / Math.max(row.aspectSum, 0.5))}px`;
        return <button className={`lc-photo-cell${isSelected ? ' is-selected' : ''}${active === photo.id ? ' is-active' : ''}${ui.gridInfo ? ' is-grid-info' : ''}`} style={{ width, height: row.height }} type="button" role="gridcell" aria-selected={isSelected} key={photo.id} onClick={(event) => { event.stopPropagation(); select(photo, event); }} onDoubleClick={(event) => { event.stopPropagation(); select(photo, event); setUi({ view: 'detail', panel: 'info', filmstrip: true }); }} onContextMenu={(event) => { event.preventDefault(); event.stopPropagation(); setMenu({ x: event.clientX, y: event.clientY, photo }); }}>
          <span className="lc-photo-frame"><PhotoPreview photoId={photo.id} slot={`grid-${photo.id}`} viewGeneration={snapshot.viewGeneration} width={Math.max(96, Math.round(thumbSize * photoAspect(photo)))} height={Math.max(96, Math.round(thumbSize))} quality="draft" className="lc-photo-preview" /><PhotoBadges photo={photo} /><span className="lc-photo-caption"><span className="lc-photo-name">{photo.fileName}</span><span className="lc-photo-stars">{stars(photo.rating)}</span></span></span>
        </button>;
      })}
    </div>;
  });
  const measuredPageHeight = useMemo(() => {
    if (!isJustified || !aspectRows.length) return null;
    return aspectRows.reduce((height, row) => height + row.height, 0) + Math.max(0, aspectRows.length - 1) * gap;
  }, [aspectRows, gap, isJustified]);
  const geometrySignature = `${mode}:${viewportWidth}:${thumbSize}:${columns}`;
  useEffect(() => {
    if (!isJustified) return;
    pageHeights.current.clear();
    previousPageAnchor.current = null;
    if (scrollRef.current && scrollRef.current.scrollTop !== 0) {
      scrollRef.current.scrollTop = 0;
      setScrollTop(0);
    }
    setGeometryRevision((value) => value + 1);
  }, [contextKey, isJustified]);
  useEffect(() => {
    if (!isJustified) return;
    pageHeights.current.clear();
    setGeometryRevision((value) => value + 1);
  }, [geometrySignature, isJustified, pageHeights]);
  useEffect(() => {
    if (!isJustified || !sliceReady || measuredPageHeight === null || !photos.length) return;
    const nextHeight = boundedPageHeight(measuredPageHeight, estimatedPageHeight);
    if (pageHeights.current.get(pageIndex) === nextHeight) return;
    pageHeights.current.set(pageIndex, nextHeight);
    while (pageHeights.current.size > MAX_PAGE_GEOMETRY) {
      const oldest = pageHeights.current.keys().next().value;
      if (oldest === undefined) break;
      pageHeights.current.delete(oldest);
    }
    setGeometryRevision((value) => value + 1);
  }, [estimatedPageHeight, isJustified, measuredPageHeight, pageIndex, pageHeights, photos.length, sliceReady]);
  const nextMeasuredPageHeight = useMemo(() => {
    if (!nextCanRender || !nextAspectRows.length) return null;
    return nextAspectRows.reduce((height, row) => height + row.height, 0) + Math.max(0, nextAspectRows.length - 1) * gap;
  }, [gap, nextAspectRows, nextCanRender]);
  useEffect(() => {
    if (!nextCanRender || nextMeasuredPageHeight === null || !nextPhotos.length) return;
    const nextHeight = boundedPageHeight(nextMeasuredPageHeight, estimatedPageHeight);
    if (pageHeights.current.get(nextPageIndex) === nextHeight) return;
    pageHeights.current.set(nextPageIndex, nextHeight);
    while (pageHeights.current.size > MAX_PAGE_GEOMETRY) {
      const oldest = pageHeights.current.keys().next().value;
      if (oldest === undefined) break;
      pageHeights.current.delete(oldest);
    }
    setGeometryRevision((value) => value + 1);
  }, [estimatedPageHeight, nextCanRender, nextMeasuredPageHeight, nextPageIndex, nextPhotos.length, pageHeights]);
  const currentPageTop = useMemo(() => (isJustified ? pageStart(pageIndex) : chunkStartRow * squareRowHeight), [chunkStartRow, geometryRevision, isJustified, pageIndex, squareRowHeight]);
  const currentVisibleRows = useMemo(() => visibleJustifiedRows(aspectRowLayout, scrollTop - currentPageTop, scrollTop - currentPageTop + viewportHeight, rowOverscan), [aspectRowLayout, currentPageTop, rowOverscan, scrollTop, viewportHeight]);
  const nextPageTop = pageStart(nextPageIndex);
  const nextVisibleRows = useMemo(() => visibleJustifiedRows(nextAspectRowLayout, scrollTop - nextPageTop, scrollTop - nextPageTop + viewportHeight, rowOverscan), [nextAspectRowLayout, nextPageTop, rowOverscan, scrollTop, viewportHeight]);
  const squareViewportStartRow = Math.max(0, Math.floor(scrollTop / squareRowHeight) - 2);
  const squareViewportEndRow = Math.ceil((scrollTop + viewportHeight) / squareRowHeight) + 2;
  const squareStartIndex = Math.max(chunkOffset, squareViewportStartRow * columns);
  const squareEndIndex = Math.min(snapshot.total, (squareViewportEndRow + 1) * columns);
  const squareLoadedPhotos = photos.length ? (nextCanRender ? [...photos, ...nextPhotos] : photos) : [];
  const squareLoadedEnd = Math.min(snapshot.total, chunkOffset + squareLoadedPhotos.length);
  const squareRenderStart = Math.min(squareLoadedEnd, squareStartIndex);
  const squareRenderEnd = Math.min(squareLoadedEnd, squareEndIndex);
  const squareLocalStart = Math.max(0, squareRenderStart - chunkOffset);
  const squareLocalEnd = Math.max(squareLocalStart, squareRenderEnd - chunkOffset);
  const squarePhotos = squareLoadedPhotos.slice(squareLocalStart, squareLocalEnd);
  const squareWindowTop = Math.floor(squareRenderStart / columns) * squareRowHeight;
  const squareLeadingCells = squarePhotos.length ? squareRenderStart % columns : 0;
  useEffect(() => {
    const previous = previousPageAnchor.current;
    if (isJustified && previous?.page === pageIndex && Math.abs(previous.top - currentPageTop) > 1 && scrollRef.current) {
      const nextTop = Math.max(0, scrollRef.current.scrollTop + currentPageTop - previous.top);
      scrollRef.current.scrollTop = nextTop;
      setScrollTop(nextTop);
    }
    previousPageAnchor.current = { page: pageIndex, top: currentPageTop };
  }, [currentPageTop, isJustified, pageIndex]);
  useEffect(() => {
    // Page measurements can clamp scrollTop without dispatching another scroll event.
    if (scrollRef.current) scheduleScrollTop(scrollRef.current.scrollTop);
  }, [currentPageTop, geometryRevision, scheduleScrollTop]);
  const totalHeight = isJustified
    ? Math.max(viewportHeight, pageCount * estimatedPageHeight + Math.max(0, pageCount - 1) * gap + Array.from(pageHeights.current.values()).reduce((delta, height) => delta + height - estimatedPageHeight, 0) + 24)
    : Math.max(viewportHeight, rows * squareRowHeight + 24);
  const showNextPage = isJustified && nextCanRender && scrollTop + viewportHeight >= currentPageTop + pageHeight(pageIndex) - rowHeight * 2;
  // Refresh geometry-only DOM when hidden WebKit keeps stale inline layout.
  // Photo content retains identity; scroll container & interaction state stay mounted.
  const spacerGeometryKey = `${geometrySignature}:${viewportHeight}:${totalHeight}`;
  const windowTop = mode === 'photoGrid' ? currentPageTop : squareWindowTop;
  const windowTemplate = `repeat(${columns}, minmax(0, 1fr))`;
  const windowGeometryKey = `${windowTop}:${windowTemplate}:${geometrySignature}:${gap}`;
  return (
    <div className={`lc-grid-scroll ${mode === 'squareGrid' ? 'is-square' : 'is-aspect'}`} ref={scrollRef} onScroll={onScroll} onClick={() => setMenu(null)}>
      <div className="lc-grid-spacer" key={spacerGeometryKey} style={{ height: totalHeight }} aria-hidden="true" />
      <RetainedLayout geometryKey={windowGeometryKey} className={`lc-grid-window${mode === 'photoGrid' ? ' is-justified' : ''}`} style={{ top: windowTop, gridTemplateColumns: windowTemplate, gap }} onKeyDown={(event) => {
          if (event.key === 'ArrowRight') { event.preventDefault(); moveSelection(1, event); }
          if (event.key === 'ArrowLeft') { event.preventDefault(); moveSelection(-1, event); }
          if (event.key === 'ArrowDown') { event.preventDefault(); moveSelection(columns, event); }
          if (event.key === 'ArrowUp') { event.preventDefault(); moveSelection(-columns, event); }
          if (event.key === 'Home') { event.preventDefault(); moveSelection(-snapshot.total, event); }
          if (event.key === 'End') { event.preventDefault(); moveSelection(snapshot.total, event); }
        }} role="grid" aria-rowcount={rows} aria-busy={slice.loading} tabIndex={0}>
          {mode === 'photoGrid' ? <>{renderJustifiedRows(currentVisibleRows.rows, `row-${chunkOffset}`, currentVisibleRows.tops)}{showNextPage && renderJustifiedRows(nextVisibleRows.rows, `row-${nextChunkOffset}`, nextVisibleRows.tops.map((top) => nextPageTop - currentPageTop + top))}</> : <>{Array.from({ length: squareLeadingCells }, (_, index) => <span className="lc-square-spacer" aria-hidden="true" key={`square-spacer-${index}`} />)}{squarePhotos.map((photo) => {
            const isSelected = selected.has(photo.id);
            const style = { '--lc-aspect': '1' } as CSSProperties;
            return <button className={`lc-photo-cell${isSelected ? ' is-selected' : ''}${active === photo.id ? ' is-active' : ''}${ui.gridInfo ? ' is-grid-info' : ''}`} style={style} type="button" role="gridcell" aria-selected={isSelected} key={photo.id} onClick={(event) => { event.stopPropagation(); select(photo, event); }} onDoubleClick={(event) => { event.stopPropagation(); select(photo, event); setUi({ view: 'detail', panel: 'info', filmstrip: true }); }} onContextMenu={(event) => { event.preventDefault(); event.stopPropagation(); setMenu({ x: event.clientX, y: event.clientY, photo }); }}>
              <span className="lc-photo-frame"><PhotoPreview photoId={photo.id} slot={`grid-${photo.id}`} viewGeneration={snapshot.viewGeneration} width={Math.max(96, thumbSize)} height={Math.max(96, thumbSize)} quality="draft" className="lc-photo-preview" /><PhotoBadges photo={photo} /><span className="lc-photo-caption"><span className="lc-photo-name">{photo.fileName}</span><span className="lc-photo-stars">{stars(photo.rating)}</span></span></span>
            </button>;
          })}</>}
          {slice.loading && photos.length === 0 && <div className="lc-grid-loading" role="status">Loading photos…</div>}
          {slice.error && <div className="lc-grid-error" role="alert">Unable to load photos: {slice.error}</div>}
      </RetainedLayout>
      <GridMenu menu={menu} albums={snapshot.albums} close={() => setMenu(null)} />
    </div>
  );
}

function sortIsRandom(sort: string): boolean {
  return /(?:key:\s*Random|key:\s*random|"key"\s*:\s*"random")/.test(sort);
}

function Header({ snapshot, mode, setMode, thumbSize }: { snapshot: DesktopSnapshot; mode: 'photoGrid' | 'squareGrid'; setMode: (mode: 'photoGrid' | 'squareGrid') => void; thumbSize: number }) {
  const { run, setDialog, setUi, ui } = useDesktop();
  const sourceLabel = typeof snapshot.source === 'object' && snapshot.source && 'label' in snapshot.source ? String((snapshot.source as { label?: unknown }).label) : sourceKey(snapshot.source) === 'all' ? 'All Photos' : sourceKey(snapshot.source);
  const [sortOpen, setSortOpen] = useState(false);
  const [displayOpen, setDisplayOpen] = useState(false);
  const randomSort = sortIsRandom(snapshot.sort);
  const sort = (key: string) => { setSortOpen(false); void run('library.sort', { key }); };
  useEffect(() => {
    if (!displayOpen) return;
    const dismissDisplay = (event: globalThis.KeyboardEvent) => {
      if (event.key === 'Escape') setDisplayOpen(false);
    };
    document.addEventListener('keydown', dismissDisplay);
    return () => document.removeEventListener('keydown', dismissDisplay);
  }, [displayOpen]);
  return <header className="lc-library-header">
    <div className="lc-library-title"><h1>{sourceLabel}</h1><span>{snapshot.selection.length > 1 ? `${snapshot.selection.length} selected · ` : ''}{snapshot.total} photos</span></div>
    <div className="lc-library-actions">
      <button type="button" className="lc-header-action" onClick={() => void run('file.addPhotos', {})}>Import</button>
      <button type="button" className="lc-header-action" onClick={() => setDialog({ kind: 'export' })}>Export</button>
      <div className="lc-display-wrap"><button type="button" className="lc-header-action" aria-expanded={displayOpen} onClick={() => setDisplayOpen((value) => !value)}>Display <span aria-hidden="true">⌄</span></button>{displayOpen && <div className="lc-display-menu" role="menu">
        <div className="lc-display-views" role="group" aria-label="Grid style"><button type="button" className={mode === 'photoGrid' ? 'is-active' : ''} aria-pressed={mode === 'photoGrid'} onClick={() => { setMode('photoGrid'); setUi({ view: 'photoGrid' }); }}>▦ Photo grid</button><button type="button" className={mode === 'squareGrid' ? 'is-active' : ''} aria-pressed={mode === 'squareGrid'} onClick={() => { setMode('squareGrid'); setUi({ view: 'squareGrid' }); }}>□ Square grid</button></div>
        <label className="lc-display-size">Thumbnail size <input type="range" min={MIN_THUMB} max={MAX_THUMB} step={4} value={thumbSize} onChange={(event) => setUi({ thumbSize: Number(event.target.value) })} /></label>
        <button type="button" role="menuitem" onClick={() => setUi({ gridInfo: !ui.gridInfo })}>Grid info: {ui.gridInfo ? 'On' : 'Off'}</button><button type="button" role="menuitem" onClick={() => { setDisplayOpen(false); setDialog({ kind: 'copySettings' }); }}>Copy settings</button><button type="button" role="menuitem" onClick={() => { setDisplayOpen(false); setDialog({ kind: 'pasteSettings' }); }}>Paste settings</button>
      </div>}</div>
      <div className="lc-sort-wrap"><button type="button" className="lc-header-action" aria-expanded={sortOpen} onClick={() => setSortOpen((value) => !value)}>Sort ▾</button>{sortOpen && <div className="lc-sort-menu" role="menu">{[['captureDate', 'Capture date'], ['importDate', 'Import date'], ['editDate', 'Modified'], ['fileName', 'Filename'], ['rating', 'Rating'], ['fileSize', 'File size']].map(([key, label]) => <button type="button" role="menuitem" key={key} onClick={() => sort(key)}>{label}</button>)}<button type="button" role="menuitem" className="lc-sort-random" onClick={() => sort('random')}>Random</button><hr />{randomSort && <button type="button" role="menuitem" onClick={() => { setSortOpen(false); void run('library.shuffle', {}); }}>Reshuffle</button>}<button type="button" role="menuitem" disabled={randomSort} aria-disabled={randomSort} onClick={() => { if (!randomSort) { setSortOpen(false); void run('library.sort', { ascending: true }); } }}>Ascending</button><button type="button" role="menuitem" disabled={randomSort} aria-disabled={randomSort} onClick={() => { if (!randomSort) { setSortOpen(false); void run('library.sort', { ascending: false }); } }}>Descending</button></div>}</div>
    </div>
  </header>;
}

export default function LibraryWorkspace({ showSidebar = true }: { showSidebar?: boolean } = {}) {
  const { snapshot, ui, setUi, run, native } = useDesktop();
  const [filterText, setFilterText] = useState(ui.filterText ?? '');
  const mode: 'photoGrid' | 'squareGrid' = ui.view === 'squareGrid' ? 'squareGrid' : 'photoGrid';
  const thumbSize = Math.max(MIN_THUMB, Math.min(MAX_THUMB, ui.thumbSize || 180));
  const sidebarCollapsed = ui.sidebarCollapsed;
  // Namespaced UI keys retain section state independently from shell sidebarCollapsed.
  const collapsedSections = useMemo<SidebarCollapseState>(() => Object.fromEntries(Object.entries(ui.sections).filter(([key, value]) => key.startsWith('sidebar:') && value === true).map(([key]) => [key.slice(8), true])), [ui.sections]);
  const toggleSection = useCallback((id: string) => {
    const key = `sidebar:${id}`;
    setUi({ sections: { ...ui.sections, [key]: !collapsedSections[id] } });
  }, [collapsedSections, setUi, ui.sections]);
  useEffect(() => { const handle = window.setTimeout(() => { setUi({ filterText }); void run('library.filter', { text: filterText }); }, 220); return () => window.clearTimeout(handle); }, [filterText, run, setUi]);
  const navigate = useCallback(async (item: LibraryNavItem) => {
    if (item.id === 'local') {
      const result = await native('pickFolder', {});
      if (result && typeof result === 'object' && 'path' in result && typeof (result as { path?: unknown }).path === 'string') {
        const path = (result as { path: string }).path;
        await run('library.browse', { path, subfolders: true });
      }
      return;
    }
    await run(item.command, item.params ?? {});
  }, [native, run]);
  if (!snapshot) return <main className="lc-library-empty" role="status">Reconnect to load library.</main>;
  return <div className={`lc-library-workspace${showSidebar ? '' : ' lc-library-workspace-shell-nav'}`} style={{ '--lc-sidebar-width': showSidebar ? (sidebarCollapsed ? '48px' : `${ui.sidebarWidth || 268}px`) : '0px' } as CSSProperties}>
    {showSidebar && <SourceSidebar snapshot={snapshot} collapsed={sidebarCollapsed} collapsedSections={collapsedSections} onToggleSection={toggleSection} onNavigate={navigate} />}
    <section className="lc-library-main" aria-label="Library">
      {showSidebar && <div className="lc-library-sidebar-toggle"><button type="button" aria-label={sidebarCollapsed ? 'Expand library sidebar' : 'Collapse library sidebar'} aria-pressed={sidebarCollapsed} onClick={() => setUi({ sidebarCollapsed: !sidebarCollapsed })}>☰</button></div>}
      <Header snapshot={snapshot} mode={mode} setMode={(next) => setUi({ view: next })} thumbSize={thumbSize} />
      <FilterBar snapshot={snapshot} filterText={filterText} setFilterText={setFilterText} />
      <VirtualPhotoGrid snapshot={snapshot} mode={mode} thumbSize={thumbSize} />
    </section>
    {snapshot.status.error && <div className="lc-library-error" role="alert">{snapshot.status.error}</div>}
  </div>;
}
