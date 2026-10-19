import { APP_NAME } from '../branding';
import { useCallback, useEffect, useMemo, useRef, useState, type CSSProperties, type MouseEvent, type KeyboardEvent, type UIEvent } from 'react';
import { AppearanceMenu, Button, Checkbox, ContextMenu, IconButton, NavList, Popover, PaletteTrigger, Select, Slider, useContextMenu, useShell, type MenuItem, type NavGroup } from '@rightkit/react-shell/react';
import type { DesktopSnapshot, PhotoSummary } from '../types';
import { useDesktop } from '../desktop';
import { RetainedLayout } from '../layout/RetainedLayout';
import { PhotoPreview } from '../preview/PhotoPreview';
import { usePhotoSlice } from './libraryData';
import './library.css';

// One bounded page spans a normal viewport, so a justified window stays contiguous at page edges.
const PAGE = 512;
const MAX_PAGE_GEOMETRY = 64;
const MIN_THUMB = 96;
const MAX_THUMB = 480;
const SCROLL_SYNC_FALLBACK_MS = 64;

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

export function LibraryShellSidebar({ groups, sectionIds, activeId, onNavigate }: { groups: readonly NavGroup[]; sectionIds: readonly string[]; activeId: string; onNavigate: (id: string) => void }) {
  const { ui, setUi, t } = useDesktop();
  const shell = useShell();
  const sectionKey = (index: number) => `sidebar:${sectionIds[index] ?? `group-${index}`}`;
  const collapsedTitles = groups.flatMap((group, index) => (ui.sections[sectionKey(index)] === true && group.title ? [group.title] : []));
  const onToggleGroup = (title: string) => {
    const index = groups.findIndex((group) => group.title === title);
    if (index < 0) return;
    const key = sectionKey(index);
    setUi({ sections: { ...ui.sections, [key]: ui.sections[key] !== true } });
  };
  return <aside className="rk-side lc-shell-library-sidebar" aria-label={t('Sections')} hidden={ui.sidebarCollapsed}>
    <PaletteTrigger onOpen={shell.palette.show} label={t('Jump to')} />
    <div className="lc-shell-sidebar-groups">
      <NavList groups={groups} activeId={activeId} onNavigate={onNavigate} label={t('Sections')} collapsedGroups={ui.sidebarCollapsed ? [] : collapsedTitles} onToggleGroup={onToggleGroup} renderTwisty={(open) => <span aria-hidden="true">{open ? '⌄' : '›'}</span>} />
    </div>
    <div className="rk-side__foot"><footer className="rk-brand-foot"><span className="rk-wordmark">{APP_NAME}</span><AppearanceMenu value={ui.theme} onChange={(theme) => setUi({ theme })} labels={{ appearance: t('Appearance'), theme: { system: t('System'), light: t('Light'), dark: t('Dark') } }} /></footer></div>
  </aside>;
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
        {filterText && <IconButton label="Clear search" onClick={() => setFilterText('')}>×</IconButton>}
      </label>
      <Button variant="ghost" className="lc-filter-pill" type="button" aria-pressed={Boolean(filter?.rating)} onClick={() => setFilter({ rating: filter?.rating ? 0 : 3, ratingOp: 'atLeast' })}>★ 3+</Button>
      <Button variant="ghost" className="lc-filter-pill" type="button" aria-pressed={filter?.flag === 'pick'} onClick={() => setFilter({ flag: filter?.flag === 'pick' ? null : 'pick' })}>⚑ Picks</Button>
      <Button variant="ghost" className="lc-filter-pill" type="button" aria-pressed={filter?.edited === true} onClick={() => setFilter({ edited: filter?.edited === true ? null : true })}>✎ Edited</Button>
      <Button variant="ghost" className="lc-filter-more" type="button" aria-expanded={expanded} onClick={() => setExpanded((value) => !value)}>Filters <span aria-hidden="true">⌄</span></Button>
      {(filter && Object.keys(filter).some((key) => filter[key] !== null && filter[key] !== undefined && filter[key] !== '' && filter[key] !== false && filter[key] !== 0)) && <Button variant="ghost" className="lc-filter-clear" type="button" onClick={clear}>Clear all</Button>}
      {expanded && <div className="lc-filter-panel" role="region" aria-label="Full filters">
        <label>Rating <Select value={String(filter?.rating ?? 0)} options={[{ value: '0', label: 'Any' }, ...[1, 2, 3, 4, 5].map((rating) => ({ value: String(rating), label: `${rating}+ stars` }))]} onChange={(value) => setFilter({ rating: Number(value) || 0 })} label="Rating" /></label>
        <label>Flag <Select value={String(filter?.flag ?? '')} options={[{ value: '', label: 'Any' }, { value: 'pick', label: 'Picked' }, { value: 'reject', label: 'Rejected' }, { value: 'none', label: 'Unflagged' }]} onChange={(value) => setFilter({ flag: value || null })} label="Flag" /></label>
        <label>Label <Select value={String(filter?.label ?? '')} options={[{ value: '', label: 'Any' }, ...['red', 'yellow', 'green', 'blue', 'purple'].map((label) => ({ value: label, label }))]} onChange={(value) => setFilter({ label: value || null })} label="Label" /></label>
        <label>Kind <Select value={String(filter?.kind ?? '')} options={[{ value: '', label: 'Any' }, { value: 'image', label: 'Image' }, { value: 'raw', label: 'Raw' }, { value: 'video', label: 'Video' }]} onChange={(value) => setFilter({ kind: value || null })} label="Kind" /></label>
        <label>Date <input value={String(filter?.date ?? '')} onChange={(event) => setFilter({ date: event.target.value || null })} placeholder="YYYY-MM-DD" /></label>
        <label>Keyword <input value={String(filter?.keyword ?? '')} onChange={(event) => setFilter({ keyword: event.target.value || null })} /></label>
        <label>Camera <input value={String(filter?.camera ?? '')} onChange={(event) => setFilter({ camera: event.target.value || null })} /></label>
        <Checkbox className="lc-filter-check" checked={filter?.edited === true} onChange={(checked) => setFilter({ edited: checked ? true : null })} label="Edited only" />
        {presets.length > 0 && <label>Saved <Select value="" options={[{ value: '', label: 'Choose…' }, ...presets.map((preset) => ({ value: preset.name, label: preset.name }))]} onChange={(value) => { if (value) void run('filter.applyPreset', { name: value }); }} label="Saved filters" /></label>}
        <div className="lc-filter-save"><input value={savedName} onChange={(event) => setSavedName(event.target.value)} placeholder="Saved filter name" aria-label="Saved filter name" /><Button type="button" onClick={save}>Save filter</Button><Button type="button" onClick={() => setDialog({ kind: 'smartAlbum', params: { fromFilter: true } })}>Smart album…</Button></div>
      </div>}
    </div>
  );
}

function photoMenuItems(photo: PhotoSummary, albums: DesktopSnapshot['albums'], run: ReturnType<typeof useDesktop>['run']): MenuItem[] {
  const ids = [photo.id];
  const writable = albums.filter((album) => !album.folder && !album.smart);
  return [
    { id: 'pick', label: 'Pick', shortcut: 'P', run: () => void run('photo.pick', { ids }) },
    { id: 'reject', label: 'Reject', shortcut: 'X', run: () => void run('photo.reject', { ids }) },
    { id: 'unflag', label: 'Unflag', shortcut: 'U', run: () => void run('photo.unflag', { ids }) },
    // Rating and label are checkable rows under disabled group headings; every choice is a plain menu row, so arrows and Enter reach it.
    { id: 'rating-heading', label: 'Rating', separatorBefore: true, disabled: true },
    ...[1, 2, 3, 4, 5].map((value): MenuItem => ({ id: `rate-${value}`, label: `${value} star${value === 1 ? '' : 's'}`, checked: photo.rating === value, run: () => void run('photo.rate', { ids, rating: value }) })),
    { id: 'label-heading', label: 'Label', separatorBefore: true, disabled: true },
    { id: 'label-red', label: 'Red', checked: (photo.label ?? 'none') === 'red', run: () => void run('photo.label', { ids, label: 'red' }) },
    { id: 'label-yellow', label: 'Yellow', checked: (photo.label ?? 'none') === 'yellow', run: () => void run('photo.label', { ids, label: 'yellow' }) },
    { id: 'label-green', label: 'Green', checked: (photo.label ?? 'none') === 'green', run: () => void run('photo.label', { ids, label: 'green' }) },
    { id: 'label-blue', label: 'Blue', checked: (photo.label ?? 'none') === 'blue', run: () => void run('photo.label', { ids, label: 'blue' }) },
    { id: 'label-purple', label: 'Purple', checked: (photo.label ?? 'none') === 'purple', run: () => void run('photo.label', { ids, label: 'purple' }) },
    { id: 'label-none', label: 'No label', checked: (photo.label ?? 'none') === 'none', run: () => void run('photo.label', { ids, label: 'none' }) },
    { id: 'album-heading', label: 'Add to album', separatorBefore: true, disabled: true },
    ...writable.map((album): MenuItem => ({ id: `album-${album.id}`, label: album.name, run: () => void run('album.addPhotos', { ids, id: album.id }) })),
    ...(writable.length ? [] : [{ id: 'album-none', label: 'No writable albums', disabled: true }]),
    { id: 'reveal', label: 'Reveal original', separatorBefore: true, run: () => void run('app.showInFinder', { ids }) },
    { id: 'delete', label: photo.deleted ? 'Restore' : 'Move to Recently Deleted', run: () => void run(photo.deleted ? 'photo.restore' : 'photo.delete', { ids }) },
  ];
}

function VirtualPhotoGrid({ snapshot, mode, thumbSize }: { snapshot: DesktopSnapshot; mode: 'photoGrid' | 'squareGrid'; thumbSize: number }) {
  const { run, ui, setUi } = useDesktop();
  const [scrollTop, setScrollTop] = useState(0);
  const [viewportHeight, setViewportHeight] = useState(600);
  const [viewportWidth, setViewportWidth] = useState(900);
  const photoMenu = useContextMenu<PhotoSummary>((photo) => photoMenuItems(photo, snapshot.albums, run), 'Photo actions');
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
        return <button className={`lc-photo-cell${isSelected ? ' is-selected' : ''}${active === photo.id ? ' is-active' : ''}${ui.gridInfo ? ' is-grid-info' : ''}`} style={{ width, height: row.height }} type="button" role="gridcell" aria-selected={isSelected} key={photo.id} onClick={(event) => { event.stopPropagation(); select(photo, event); }} onDoubleClick={(event) => { event.stopPropagation(); select(photo, event); setUi({ view: 'detail', panel: 'info', filmstrip: true }); }} onContextMenu={(event) => photoMenu.open(event, photo)}>
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
    <div className={`lc-grid-scroll ${mode === 'squareGrid' ? 'is-square' : 'is-aspect'}`} ref={scrollRef} onScroll={onScroll}>
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
            return <button className={`lc-photo-cell${isSelected ? ' is-selected' : ''}${active === photo.id ? ' is-active' : ''}${ui.gridInfo ? ' is-grid-info' : ''}`} style={style} type="button" role="gridcell" aria-selected={isSelected} key={photo.id} onClick={(event) => { event.stopPropagation(); select(photo, event); }} onDoubleClick={(event) => { event.stopPropagation(); select(photo, event); setUi({ view: 'detail', panel: 'info', filmstrip: true }); }} onContextMenu={(event) => photoMenu.open(event, photo)}>
              <span className="lc-photo-frame"><PhotoPreview photoId={photo.id} slot={`grid-${photo.id}`} viewGeneration={snapshot.viewGeneration} width={Math.max(96, thumbSize)} height={Math.max(96, thumbSize)} quality="draft" className="lc-photo-preview" /><PhotoBadges photo={photo} /><span className="lc-photo-caption"><span className="lc-photo-name">{photo.fileName}</span><span className="lc-photo-stars">{stars(photo.rating)}</span></span></span>
            </button>;
          })}</>}
          {slice.loading && photos.length === 0 && <div className="lc-grid-loading" role="status">Loading photos…</div>}
          {slice.error && <div className="lc-grid-error" role="alert">Unable to load photos: {slice.error}</div>}
      </RetainedLayout>
      {photoMenu.element}
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
  const sortButtonRef = useRef<HTMLButtonElement>(null);
  const displayButtonRef = useRef<HTMLButtonElement>(null);
  const randomSort = sortIsRandom(snapshot.sort);
  const sortItems: MenuItem[] = [
    ...[['captureDate', 'Capture date'], ['importDate', 'Import date'], ['editDate', 'Modified'], ['fileName', 'Filename'], ['rating', 'Rating'], ['fileSize', 'File size'], ['random', 'Random']].map(([key, label]): MenuItem => ({ id: key, label, run: () => void run('library.sort', { key }) })),
    ...(randomSort ? [{ id: 'reshuffle', label: 'Reshuffle', separatorBefore: true, run: () => void run('library.shuffle', {}) }] : []),
    { id: 'ascending', label: 'Ascending', separatorBefore: !randomSort, disabled: randomSort, run: () => void run('library.sort', { ascending: true }) },
    { id: 'descending', label: 'Descending', disabled: randomSort, run: () => void run('library.sort', { ascending: false }) },
  ];
  return <header className="lc-library-header">
    <div className="lc-library-title"><h1>{sourceLabel}</h1><span>{snapshot.selection.length > 1 ? `${snapshot.selection.length} selected · ` : ''}{snapshot.total} photos</span></div>
    <div className="lc-library-actions">
      <Button variant="ghost" type="button" className="lc-header-action" onClick={() => void run('file.addPhotos', {})}>Import</Button>
      <Button variant="ghost" type="button" className="lc-header-action" onClick={() => setDialog({ kind: 'export' })}>Export</Button>
      <div className="lc-display-wrap"><Button ref={displayButtonRef} variant="ghost" type="button" className="lc-header-action" aria-expanded={displayOpen} onClick={() => setDisplayOpen((value) => !value)}>Display <span aria-hidden="true">⌄</span></Button></div>
      {displayOpen && displayButtonRef.current && <Popover anchor={displayButtonRef.current} label="Display" placement="bottom-end" width={200} className="lc-display-menu" onClose={() => setDisplayOpen(false)}>
        <div className="lc-display-views" role="group" aria-label="Grid style"><Button variant="ghost" type="button" className={mode === 'photoGrid' ? 'is-active' : undefined} aria-pressed={mode === 'photoGrid'} onClick={() => { setMode('photoGrid'); setUi({ view: 'photoGrid' }); }}>▦ Photo grid</Button><Button variant="ghost" type="button" className={mode === 'squareGrid' ? 'is-active' : undefined} aria-pressed={mode === 'squareGrid'} onClick={() => { setMode('squareGrid'); setUi({ view: 'squareGrid' }); }}>□ Square grid</Button></div>
        <div className="lc-display-size"><span>Thumbnail size</span><Slider value={thumbSize} min={MIN_THUMB} max={MAX_THUMB} step={4} label="Thumbnail size" onChange={(value) => setUi({ thumbSize: value })} /></div>
        <Button variant="ghost" className="lc-display-action lc-display-gridinfo" onClick={() => setUi({ gridInfo: !ui.gridInfo })}>Grid info: {ui.gridInfo ? 'On' : 'Off'}</Button>
        <Button variant="ghost" className="lc-display-action" onClick={() => { setDisplayOpen(false); setDialog({ kind: 'copySettings' }); }}>Copy settings</Button>
        <Button variant="ghost" className="lc-display-action" onClick={() => { setDisplayOpen(false); setDialog({ kind: 'pasteSettings' }); }}>Paste settings</Button>
      </Popover>}
      <Button ref={sortButtonRef} variant="ghost" type="button" className="lc-header-action" aria-haspopup="menu" aria-expanded={sortOpen} onClick={() => setSortOpen((value) => !value)}>Sort ▾</Button>
      {sortOpen && sortButtonRef.current && <ContextMenu anchor={sortButtonRef.current} placement="bottom-end" items={sortItems} label="Sort" width={176} onClose={() => setSortOpen(false)} />}
    </div>
  </header>;
}

export default function LibraryWorkspace() {
  const { snapshot, ui, setUi, run } = useDesktop();
  const [filterText, setFilterText] = useState(ui.filterText ?? '');
  const mode: 'photoGrid' | 'squareGrid' = ui.view === 'squareGrid' ? 'squareGrid' : 'photoGrid';
  const thumbSize = Math.max(MIN_THUMB, Math.min(MAX_THUMB, ui.thumbSize || 180));
  useEffect(() => { const handle = window.setTimeout(() => { setUi({ filterText }); void run('library.filter', { text: filterText }); }, 220); return () => window.clearTimeout(handle); }, [filterText, run, setUi]);
  if (!snapshot) return <main className="lc-library-empty" role="status">Reconnect to load library.</main>;
  return <div className="lc-library-workspace lc-library-workspace-shell-nav" style={{ '--lc-sidebar-width': '0px' } as CSSProperties}>
    <section className="lc-library-main" aria-label="Library">
      <Header snapshot={snapshot} mode={mode} setMode={(next) => setUi({ view: next })} thumbSize={thumbSize} />
      <FilterBar snapshot={snapshot} filterText={filterText} setFilterText={setFilterText} />
      <VirtualPhotoGrid snapshot={snapshot} mode={mode} thumbSize={thumbSize} />
    </section>
    {snapshot.status.error && <div className="lc-library-error" role="alert">{snapshot.status.error}</div>}
  </div>;
}
