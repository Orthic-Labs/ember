import { APP_NAME, APP_ICON_URL } from '../branding';
import React, { useEffect, useRef, useState } from 'react';
import { createPortal } from 'react-dom';
import { acknowledgePreview, cancelMergePreview, convertFileSrc, requestMergePreview } from '../api';
import { useDesktop } from '../desktop';
import { ImportCandidatePreview } from '../preview/ImportCandidatePreview';
import { CullReviewDialog } from './CullReviewDialog';
import type { DesktopContextValue, DialogState, JsonObject, MergePreviewDescriptor, UiState } from '../types';
import './dialogs.css';

type AnyRecord = Record<string, any>;
type HostProps = { desktop?: DesktopContextValue };
let mergePreviewSequence = 0;
let settingsCameraSequence = 0;

const text = (v: unknown, fallback = '') => typeof v === 'string' ? v : fallback;
const bool = (v: unknown, fallback = false) => typeof v === 'boolean' ? v : fallback;
const num = (v: unknown, fallback = 0) => typeof v === 'number' && Number.isFinite(v) ? v : fallback;
const arr = (v: unknown): any[] => Array.isArray(v) ? v : [];
const errorText = (reason: unknown) => reason instanceof Error ? reason.message : reason && typeof reason === 'object' && 'message' in reason ? text((reason as AnyRecord).message, JSON.stringify(reason)) : String(reason);
const displayValue = (value: unknown): string => {
  if (typeof value === 'string' || typeof value === 'number' || typeof value === 'boolean') return String(value);
  if (Array.isArray(value)) return value.map(displayValue).join(' · ');
  if (value && typeof value === 'object') return Object.entries(value as AnyRecord).map(([key, item]) => `${key}: ${displayValue(item)}`).join(', ');
  return String(value ?? 'Unknown');
};

let lastNonOverlayFocus: HTMLElement | null = null;
function restorableFocusTarget(value: Element | null): value is HTMLElement {
  if (!(value instanceof HTMLElement) || !value.isConnected || value === document.body) return false;
  if (value.closest('.rk-overlay, .lc-dialog-backdrop, [aria-hidden="true"]')) return false;
  let node: HTMLElement | null = value;
  while (node) { if (node.inert) return false; node = node.parentElement; }
  return true;
}
function focusCandidate(value: EventTarget | null): Element | null {
  if (!(value instanceof Element)) return null;
  return value.matches('button,a[href],input,select,textarea,[tabindex]:not([tabindex="-1"])') ? value : value.closest('button,a[href],input,select,textarea,[tabindex]:not([tabindex="-1"])');
}
function useFocusHistory() {
  useEffect(() => {
    const remember = (event: FocusEvent) => {
      const target = focusCandidate(event.target);
      if (restorableFocusTarget(target)) lastNonOverlayFocus = target;
    };
    const rememberPointer = (event: PointerEvent) => {
      const target = focusCandidate(event.target);
      if (restorableFocusTarget(target)) lastNonOverlayFocus = target;
    };
    document.addEventListener('focusin', remember, true);
    document.addEventListener('pointerdown', rememberPointer, true);
    return () => { document.removeEventListener('focusin', remember, true); document.removeEventListener('pointerdown', rememberPointer, true); };
  }, []);
}

function useModalScope(open: boolean, onClose: () => void, dismissible = true) {
  const ref = useRef<HTMLDivElement>(null);
  const closeRef = useRef(onClose);
  const dismissibleRef = useRef(dismissible);
  closeRef.current = onClose;
  dismissibleRef.current = dismissible;
  useEffect(() => {
    if (!open) return;
    const active = document.activeElement as HTMLElement | null;
    const restoreTarget = restorableFocusTarget(active) ? active : lastNonOverlayFocus;
    const root = ref.current;
    const siblings = Array.from(document.body.children).filter((node) => node !== root?.parentElement);
    const previous = siblings.map((node) => ({ node, inert: (node as HTMLElement).inert, hidden: node.getAttribute('aria-hidden') }));
    siblings.forEach((node) => { (node as HTMLElement).inert = true; node.setAttribute('aria-hidden', 'true'); });
    const focusables = () => root ? Array.from(root.querySelectorAll<HTMLElement>('button,[href],input,select,textarea,[tabindex]:not([tabindex="-1"])')).filter((el) => !el.hasAttribute('disabled')) : [];
    focusables()[0]?.focus();
    const keydown = (event: KeyboardEvent) => {
      if (event.key === 'Escape' && dismissibleRef.current) { event.preventDefault(); closeRef.current(); return; }
      if (event.key !== 'Tab') return;
      const fields = focusables();
      if (!fields.length) return;
      const first = fields[0]; const last = fields[fields.length - 1];
      if (event.shiftKey && document.activeElement === first) { event.preventDefault(); last.focus(); }
      else if (!event.shiftKey && document.activeElement === last) { event.preventDefault(); first.focus(); }
    };
    document.addEventListener('keydown', keydown);
    return () => { document.removeEventListener('keydown', keydown); previous.forEach(({ node, inert, hidden }) => { (node as HTMLElement).inert = inert; if (hidden === null) node.removeAttribute('aria-hidden'); else node.setAttribute('aria-hidden', hidden); }); if (restorableFocusTarget(restoreTarget)) restoreTarget.focus(); };
  // onClose & dismissible are interaction policy, not scope lifecycle; refs prevent input rerenders from resetting focus.
  }, [open]);
  return ref;
}

function Frame({ title, children, actions, onClose, wide = false, busy = false, dismissible = true }: { title: string; children: React.ReactNode; actions?: React.ReactNode; onClose: () => void; wide?: boolean; busy?: boolean; dismissible?: boolean }) {
  const ref = useModalScope(true, onClose, dismissible);
  const frame = <div className="lc-dialog-backdrop" role="presentation" onMouseDown={(e) => { if (e.target === e.currentTarget && dismissible) onClose(); }}>
    <section ref={ref} className={`lc-dialog ${wide ? 'lc-dialog-wide' : ''}`} role="dialog" aria-modal="true" aria-labelledby="lc-dialog-title">
      <header className="lc-dialog-header"><h2 id="lc-dialog-title">{title}</h2>{dismissible && <button className="lc-icon-button" aria-label="Close" onClick={onClose}>×</button>}</header>
      <div className="lc-dialog-body">{children}</div>
      {actions && <footer className="lc-dialog-actions">{busy && <span className="lc-dialog-busy" aria-live="polite">Working…</span>}<span className="lc-dialog-action-group">{actions}</span></footer>}
    </section>
  </div>;
  return typeof document === 'undefined' ? frame : createPortal(frame, document.body);
}

function Button({ children, primary = false, disabled = false, onClick, type = 'button' }: { children: React.ReactNode; primary?: boolean; disabled?: boolean; onClick?: () => void; type?: 'button' | 'submit' }) { return <button type={type} disabled={disabled} className={primary ? 'lc-button lc-button-primary' : 'lc-button'} onClick={onClick}>{children}</button>; }
function Field({ label, value, onChange, type = 'text', min, max, step, placeholder, disabled = false }: { label: string; value: string | number; onChange: (v: string) => void; type?: string; min?: number; max?: number; step?: number; placeholder?: string; disabled?: boolean }) { return <label className="lc-field"><span>{label}</span><input type={type} value={value} onChange={(e) => onChange(e.target.value)} min={min} max={max} step={step} placeholder={placeholder} disabled={disabled} /></label>; }
function TextArea({ label, value, onChange, placeholder }: { label: string; value: string; onChange: (v: string) => void; placeholder?: string }) { return <label className="lc-field"><span>{label}</span><textarea rows={3} value={value} onChange={(e) => onChange(e.target.value)} placeholder={placeholder} style={{ width: '100%', boxSizing: 'border-box', minHeight: 72, padding: '6px 9px', border: '1px solid var(--rk-color-border,#c8d0da)', borderRadius: 7, background: 'var(--rk-color-surface,#fff)', color: 'inherit', font: '13px Inter,system-ui,sans-serif', resize: 'vertical' }} /></label>; }
function Select({ label, value, options, onChange, disabled = false }: { label: string; value: string; options: [string, string][]; onChange: (v: string) => void; disabled?: boolean }) { return <label className="lc-field"><span>{label}</span><select value={value} onChange={(e) => onChange(e.target.value)} disabled={disabled}>{options.map(([v, l]) => <option key={v} value={v}>{l}</option>)}</select></label>; }
function Check({ children, checked, onChange, disabled = false }: { children: React.ReactNode; checked: boolean; onChange: (v: boolean) => void; disabled?: boolean }) { return <label className="lc-check"><input type="checkbox" checked={checked} onChange={(e) => onChange(e.target.checked)} disabled={disabled} /><span>{children}</span></label>; }
function Note({ children, tone = 'normal' }: { children: React.ReactNode; tone?: 'normal' | 'warning' | 'error' }) { return <p className={`lc-note lc-note-${tone}`} role={tone === 'error' ? 'alert' : undefined}>{children}</p>; }
type JobTerminal = 'done' | 'failed' | 'cancelled';
function Progress({ job, label, onCancel, cancelling = false }: { job: AnyRecord | null; label: string; onCancel?: () => void; cancelling?: boolean }) {
  const completed = num(job?.completed); const total = Math.max(1, num(job?.total, 1));
  return <div className="lc-progress" aria-live="polite"><div className="lc-progress-label"><span>{text(job?.label, label)}</span><span>{Math.min(100, Math.round(completed / total * 100))}%</span></div><progress max={total} value={completed} />{onCancel && job && job.cancellable !== false && <Button disabled={cancelling} onClick={onCancel}>{cancelling ? 'Cancelling…' : 'Cancel'}</Button>}</div>;
}

function JobDialog({ kind, desktop, d }: { kind: string; desktop: DesktopContextValue; d: DialogState }) {
  const lightroomInspect = kind.includes('lightroominspect');
  const lightroomImport = kind.includes('lightroomimport');
  const importReview = kind.includes('importreview');
  const taskId = text(d.params?.taskId);
  const [job, setJob] = useState<AnyRecord | null>(null);
  const [terminal, setTerminal] = useState<JobTerminal | null>(null);
  const [completion, setCompletion] = useState<AnyRecord | null>(null);
  const [cancelling, setCancelling] = useState(false);
  const [pollError, setPollError] = useState('');
  const snapshotRef = useRef(desktop.snapshot);
  const seenRef = useRef(false);
  const terminalRef = useRef<JobTerminal | null>(null);
  const cancelRequested = useRef(false);
  snapshotRef.current = desktop.snapshot;
  const label = lightroomInspect ? 'Inspect LRCAT catalog' : lightroomImport ? 'Import LRCAT catalog' : kind.includes('import') ? 'Import photos' : kind.includes('merge') ? 'Build merged photo' : 'Export photos';
  const markTerminal = (value: JobTerminal) => { if (!terminalRef.current) { terminalRef.current = value; setTerminal(value); } };
  useEffect(() => {
    let live = true;
    const poll = async () => {
      if (!live) return;
      try { await desktop.refresh(); } catch (reason) { if (live) setPollError(errorText(reason)); return; }
      if (!live) return;
      const current = snapshotRef.current;
      const found = taskId ? current?.status?.jobs?.find((candidate) => candidate.id === taskId) : undefined;
      if (found) {
        seenRef.current = true;
        setJob(found);
        if (found.error) markTerminal('failed');
        return;
      }
      const completed = taskId ? current?.status?.completedJobs?.find((candidate) => candidate.id === taskId) : undefined;
      if (completed) {
        const result = completed.result && typeof completed.result === 'object' && !Array.isArray(completed.result) ? completed.result as AnyRecord : null;
        const completionError = text(completed.error, text(result?.error));
        setCompletion(result);
        setJob({ id: completed.id, label: completed.label, completed: 1, total: 1, cancellable: false, ...(completionError ? { error: completionError } : {}) });
        markTerminal(completed.state);
        if (completed.state === 'done' && (lightroomInspect || lightroomImport || importReview)) {
          const terminalResult: AnyRecord = { ...(result || {}), ...(completionError && !result?.error ? { error: completionError } : {}) };
          if (lightroomInspect) desktop.setDialog({ kind: 'lightroom', params: { path: text(d.params?.path), report: terminalResult } });
          else if (lightroomImport) desktop.setDialog({ kind: 'lightroomResult', params: { path: text(d.params?.path), updateExisting: bool(d.params?.updateExisting), result: terminalResult } });
          else if (importReview) {
            const params: AnyRecord = { ...(d.params || {}), candidates: arr(terminalResult.candidates) };
            delete params.taskId;
            desktop.setDialog({ kind: 'import', params });
          }
        }
        return;
      }
      if (seenRef.current && current?.status?.error) setPollError(text(current.status.error));
    };
    void poll();
    const timer = window.setInterval(() => void poll(), 300);
    return () => { live = false; window.clearInterval(timer); };
  }, [desktop.refresh, kind, label, taskId]);
  const reveal = async () => { const paramsFiles = arr(d.params?.files); const paramsPaths = arr(d.params?.paths); const resultFiles = arr(completion?.files); const resultPaths = arr(completion?.paths); const files = paramsFiles.length ? paramsFiles : paramsPaths.length ? paramsPaths : resultFiles.length ? resultFiles : resultPaths; const path = text(files[0] || d.params?.path || d.params?.dir); if (path) await desktop.native('reveal', { path }); };
  const cancel = async () => {
    if (!taskId || cancelling || terminalRef.current) return;
    cancelRequested.current = true;
    setCancelling(true);
    setPollError('');
    try {
      const result = await desktop.run('task.cancel', { id: taskId }) as AnyRecord;
      if (num(result?.cancelled) < 1) setPollError('Operation is no longer running.');
    } catch (reason) { cancelRequested.current = false; setPollError(errorText(reason)); }
    finally { setCancelling(false); }
  };
  const retryLightroom = () => {
    if (!lightroomInspect && !lightroomImport) return;
    desktop.setDialog({ kind: 'lightroom', params: { path: text(d.params?.path), updateExisting: bool(d.params?.updateExisting), ...(d.params?.report ? { report: d.params.report } : {}) } });
  };
  const retryImportReview = () => {
    if (!importReview) return;
    const params: AnyRecord = { ...(d.params || {}) };
    delete params.taskId;
    delete params.candidates;
    delete params.checked;
    desktop.setDialog({ kind: 'import', params });
  };
  const successful = terminal === 'done';
  const imported = completion ? (Array.isArray(completion.imported) ? completion.imported.length : num(completion.imported)) : 0;
  const title = importReview ? 'Reviewing Photos' : lightroomInspect ? 'Inspecting LRCAT Catalog' : lightroomImport ? 'Importing LRCAT Catalog' : kind.includes('import') ? 'Importing Photos' : kind.includes('merge') ? 'Merging Photos' : 'Exporting Photos';
  return <Frame title={title} dismissible={terminal !== null} onClose={() => terminal && desktop.setDialog(null)} actions={<>{terminal === 'failed' && (lightroomInspect || lightroomImport) && <Button primary onClick={retryLightroom}>Retry</Button>}{importReview && terminal && terminal !== 'done' && <Button primary onClick={retryImportReview}>Retry review</Button>}<Button disabled={!terminal} onClick={() => desktop.setDialog(null)}>Close</Button>{kind.includes('export') && successful && <Button primary onClick={() => void reveal()}>Show in Folder</Button>}</>}><Progress job={job} label={label} onCancel={terminal ? undefined : () => void cancel()} cancelling={cancelling} />{pollError && <Note tone="error">{pollError}</Note>}{terminal === 'done' && !importReview && <Note>Operation complete. Changes are recorded in library history.{imported > 0 ? ` ${imported} photos imported.` : ''}</Note>}{terminal === 'cancelled' && <Note tone="warning">{importReview ? 'Review cancelled. Chosen paths remain available for retry.' : 'Operation cancelled.'}</Note>}{terminal === 'failed' && <Note tone="error">{text(job?.error, 'Operation failed.')}{importReview && ' Chosen paths remain available for retry.'}</Note>}</Frame>;
}

function UnsavedQuitDialog({ desktop, d }: { desktop: DesktopContextValue; d: DialogState }) {
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState('');
  const keepOpen = () => desktop.setDialog(null);
  const saveAndQuit = async () => {
    setBusy(true);
    setError('');
    try {
      await desktop.native('saveBeforeClose', d.params || {});
      desktop.setDialog(null);
    } catch (reason) {
      setError(errorText(reason));
    } finally {
      setBusy(false);
    }
  };
  const quitAnyway = async () => {
    setBusy(true);
    setError('');
    try {
      await desktop.native('closeWindow', { force: true });
    } catch (reason) {
      setError(errorText(reason));
      setBusy(false);
    }
  };
  return <Frame title="Quit with unsaved changes?" busy={busy} onClose={keepOpen} actions={<><Button disabled={busy} onClick={keepOpen}>Keep Open</Button><Button disabled={busy} onClick={() => void quitAnyway()}>Quit Anyway</Button><Button primary disabled={busy} onClick={() => void saveAndQuit()}>Retry Save &amp; Quit</Button></>}><Note tone="warning">Some library changes could not be saved. Try saving again before closing.</Note>{typeof d.params?.error === 'string' && !error && <Note>{d.params.error}</Note>}{error && <Note tone="error">{error}</Note>}</Frame>;
}

function ConfirmDeleteDialog({ desktop, d }: { desktop: DesktopContextValue; d: DialogState }) {
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState('');
  const command = d.params?.command === 'photo.deletePermanently' ? 'photo.deletePermanently' : 'photo.delete';
  const permanent = command === 'photo.deletePermanently';
  const confirm = async () => {
    setBusy(true);
    setError('');
    try {
      const payload: AnyRecord = { ...(d.params || {}), confirmed: true, skipConfirmation: true };
      delete payload.command;
      await desktop.run(command, payload);
      desktop.setDialog(null);
    } catch (reason) {
      setError(errorText(reason));
    } finally {
      setBusy(false);
    }
  };
  return <Frame title={permanent ? 'Delete Photos Permanently?' : 'Move Photos to Recently Deleted?'} busy={busy} onClose={() => desktop.setDialog(null)} actions={<><Button disabled={busy} onClick={() => desktop.setDialog(null)}>Cancel</Button><Button primary disabled={busy} onClick={() => void confirm()}>{permanent ? 'Delete Permanently' : 'Delete Photos'}</Button></>}><Note tone="warning">{permanent ? 'These photos cannot be restored.' : 'Photos can be restored from Recently Deleted.'}</Note>{error && <Note tone="error">{error}</Note>}</Frame>;
}

async function choose(native: DesktopContextValue['native'], action: string, params: JsonObject = {}) { const result: AnyRecord = await native(action, params) as AnyRecord; if (typeof result === 'string') return [result]; if (Array.isArray(result)) return result.filter((p) => typeof p === 'string'); return arr(result?.paths || result?.files || result?.selected || (result?.path ? [result.path] : [] )).filter((p) => typeof p === 'string'); }

function ImportDialog({ d, desktop }: { d: DialogState; desktop: DesktopContextValue }) {
  const p = d.params || {};
  const initialPaths = arr(p.paths).filter((path): path is string => typeof path === 'string' && path.length > 0);
  const suppliedCandidates = Array.isArray(p.candidates);
  const initialCandidates = suppliedCandidates ? arr(p.candidates) as AnyRecord[] : [];
  const initialSource = text(p.initialSource);
  const initialAlbum = typeof p.album === 'number' ? String(p.album) : text(p.album);
  const initialRenameStart = typeof p.renameStart === 'number' ? String(p.renameStart) : text(p.renameStart, '1');
  const [source, setSource] = useState(text(p.source, initialPaths.join(', ')));
  const [sourceType, setSourceType] = useState(initialSource);
  const [mode, setMode] = useState(text(p.mode, initialSource === 'device' ? 'copy' : 'add'));
  const [album, setAlbum] = useState(initialAlbum);
  const [newAlbum, setNewAlbum] = useState(text(p.newAlbum, text(p.albumName)));
  const [keywords, setKeywords] = useState(text(p.keywords));
  const [organize, setOrganize] = useState(text(p.organize, 'date'));
  const [folderTemplate, setFolderTemplate] = useState(text(p.folderTemplate, '{date:%Y}/{date:%Y%m%d}'));
  const [destination, setDestination] = useState(text(p.destination));
  const [rename, setRename] = useState(text(p.rename));
  const [renameStart, setRenameStart] = useState(initialRenameStart);
  const [preset, setPreset] = useState(text(p.preset));
  const [metadataPreset, setMetadataPreset] = useState(text(p.metadataPreset));
  const [dng, setDng] = useState(bool(p.dng));
  const [includeSubfolders, setIncludeSubfolders] = useState(bool(p.includeSubfolders, true));
  const [rawJpegPolicy, setRawJpegPolicy] = useState<'keepBoth' | 'rawOnly'>(() => text(p.rawJpegPolicy) === 'rawOnly' ? 'rawOnly' : 'keepBoth');
  const extensionText = (value: unknown) => Array.isArray(value) ? value.filter((item): item is string => typeof item === 'string').join(', ') : text(value);
  const [allowedExtensions, setAllowedExtensions] = useState(extensionText(p.allowedExtensions));
  const [excludedExtensions, setExcludedExtensions] = useState(extensionText(p.excludedExtensions));
  const [candidates, setCandidates] = useState<AnyRecord[]>(initialCandidates);
  const candidateDefaultChecked = (candidate: AnyRecord) => !candidate.duplicate && !candidate.error && candidate.policyExcluded !== 'rawOnly';
  const [checked, setChecked] = useState<boolean[]>(arr(p.checked).length ? arr(p.checked).map(Boolean) : initialCandidates.map(candidateDefaultChecked));
  const [busy, setBusy] = useState(false);
  const [phase, setPhase] = useState<'idle' | 'choosing' | 'scanning' | 'submitting'>('idle');
  const [error, setError] = useState('');
  const [scanPaths, setScanPaths] = useState<string[]>(initialPaths);
  const liveRef = useRef(true);
  const busyRef = useRef(false);
  const scanSequenceRef = useRef(0);

  useEffect(() => {
    liveRef.current = true;
    return () => { liveRef.current = false; scanSequenceRef.current += 1; };
  }, []);

  const scanOptions = () => ({ includeSubfolders, allowedExtensions: allowedExtensions.split(',').map((v) => v.trim()).filter(Boolean), excludedExtensions: excludedExtensions.split(',').map((v) => v.trim()).filter(Boolean), rawJpegPolicy });
  const preview = async (paths: string[], kind: string, sequence: number) => {
    if (!paths.length) return;
    setPhase('scanning');
    setScanPaths(paths);
    setSource(paths.join(', '));
    if (kind) setSourceType(kind);
    setError('');
    try {
      const result = (await desktop.run('library.importPreview', { paths, ...scanOptions() })) as AnyRecord;
      if (!liveRef.current || sequence !== scanSequenceRef.current) return;
      const taskId = text(result?.taskId);
      if (taskId) {
        const reviewMode = kind === 'device' ? 'copy' : mode;
        const params: AnyRecord = {
          ...p,
          paths,
          source: paths.join(', '),
          initialSource: kind,
          mode: reviewMode,
          album,
          newAlbum,
          keywords,
          organize,
          folderTemplate,
          destination,
          rename,
          renameStart,
          preset,
          metadataPreset,
          dng,
          includeSubfolders,
          allowedExtensions,
          excludedExtensions,
          rawJpegPolicy,
        };
        delete params.candidates;
        delete params.checked;
        desktop.setDialog({ kind: 'importReviewProgress', params: { ...params, taskId } });
        return;
      }
      const nextCandidates = arr(result?.candidates) as AnyRecord[];
      setCandidates(nextCandidates);
      setChecked(nextCandidates.map(candidateDefaultChecked));
    } catch (e) {
      if (liveRef.current && sequence === scanSequenceRef.current) {
        setCandidates([]);
        setChecked([]);
        setError(errorText(e));
      }
    } finally {
      if (liveRef.current && sequence === scanSequenceRef.current) {
        busyRef.current = false;
        setBusy(false);
        setPhase('idle');
      }
    }
  };

  useEffect(() => {
    if (suppliedCandidates || !initialPaths.length) return;
    busyRef.current = true;
    setBusy(true);
    const sequence = ++scanSequenceRef.current;
    if (initialSource === 'device') setMode('copy');
    void preview(initialPaths, initialSource, sequence);
  }, []);

  const pick = async (kind: 'files' | 'folder' | 'device') => {
    if (busyRef.current) return;
    busyRef.current = true;
    setBusy(true);
    setPhase('choosing');
    setError('');
    setSource('');
    setSourceType(kind);
    setCandidates([]);
    setChecked([]);
    if (kind === 'device') setMode('copy');
    const sequence = ++scanSequenceRef.current;
    try {
      const paths = await choose(desktop.native, kind === 'folder' ? 'chooseFolder' : kind === 'device' ? 'chooseDevice' : 'chooseFiles', { multiple: kind === 'files' });
      if (!liveRef.current || sequence !== scanSequenceRef.current) return;
      if (!paths.length) return;
      await preview(paths, kind, sequence);
    } catch (e) {
      if (liveRef.current && sequence === scanSequenceRef.current) setError(errorText(e));
    } finally {
      if (liveRef.current && sequence === scanSequenceRef.current) {
        busyRef.current = false;
        setBusy(false);
        setPhase('idle');
      }
    }
  };

  const rescan = () => {
    if (busyRef.current || !scanPaths.length) return;
    busyRef.current = true;
    setBusy(true);
    const sequence = ++scanSequenceRef.current;
    void preview(scanPaths, sourceType, sequence).finally(() => {
      busyRef.current = false;
      setBusy(false);
    });
  };

  const submit = async () => {
    if (busyRef.current) return;
    const paths = candidates.filter((_, i) => checked[i]).map((c) => c.path).filter((path): path is string => typeof path === 'string' && path.length > 0);
    if (!paths.length) { setError('Choose at least one photo.'); return; }
    const custom = folderTemplate.trim();
    const templateError = mode !== 'add' && organize === 'custom' ? folderTemplateError(custom) : null;
    if (templateError) { setError(templateError); return; }
    busyRef.current = true;
    setBusy(true);
    setPhase('submitting');
    setError('');
    try {
      const payload: AnyRecord = { ...p, paths, mode, includeSubfolders, rawJpegPolicy, allowedExtensions: scanOptions().allowedExtensions, excludedExtensions: scanOptions().excludedExtensions, album: album ? Number(album) : undefined, albumName: newAlbum.trim() || undefined, keywords: keywords.split(',').map((v) => v.trim()).filter(Boolean), preset: preset || undefined, metadataPreset: metadataPreset || undefined, destination: destination || undefined, organize: mode === 'add' ? undefined : organize === 'custom' ? custom : organize || undefined, rename: rename || undefined, renameStart: renameStart ? Number(renameStart) : undefined, dng: dng && mode === 'copy' };
      delete payload.candidates;
      delete payload.checked;
      delete payload.taskId;
      delete payload.initialSource;
      delete payload.source;
      const result = await desktop.run('library.import', payload) as AnyRecord;
      const taskId = text(result?.taskId);
      if (!taskId) throw new Error('Import did not return a task ID.');
      if (liveRef.current) desktop.setDialog({ kind: 'importProgress', params: { taskId, paths } });
    } catch (e) {
      if (liveRef.current) setError(errorText(e));
    } finally {
      if (liveRef.current) {
        busyRef.current = false;
        setBusy(false);
        setPhase('idle');
      }
    }
  };

  const close = () => {
    liveRef.current = false;
    scanSequenceRef.current += 1;
    desktop.setDialog(null);
  };
  const sourceLabel = sourceType === 'folder' ? 'Folder' : sourceType === 'device' ? 'Camera or card' : 'Photos';
  return <Frame title="Import Photos" wide busy={busy} onClose={close} actions={<><Button disabled={busy} onClick={close}>Cancel</Button><Button primary disabled={busy || !candidates.length} onClick={() => void submit()}>Import {candidates.filter((_, i) => checked[i]).length || ''} Photos</Button></>}><div className="lc-dialog-toolbar"><Button disabled={busy} onClick={() => void pick('files')}>Choose Photos…</Button><Button disabled={busy} onClick={() => void pick('folder')}>Choose Folder…</Button><Button disabled={busy} onClick={() => void pick('device')}>Camera or Card…</Button><Button disabled={busy || !scanPaths.length} onClick={rescan}>Rescan</Button></div>{source && <Note>Source: {sourceLabel} — {source}</Note>}{phase === 'scanning' && <Note>Reviewing photos…</Note>}{error && <Note tone="error">{error}</Note>}<div className="lc-import-layout"><div className="lc-candidate-list"><div className="lc-section-heading"><strong>Review</strong><span>{candidates.length} found</span></div>{candidates.length ? candidates.map((c, i) => { const pairing = c.pairing && typeof c.pairing === 'object' ? c.pairing as AnyRecord : null; const pairingNotice = c.policyExcluded === 'rawOnly' ? ` · RAW only default (paired with ${text(pairing?.with, 'JPEG')})` : pairing ? ` · Paired ${text(pairing.kind, 'variant')}: ${text(pairing.with)}` : ''; return <label className={`lc-candidate ${c.duplicate || c.error ? 'lc-candidate-muted' : ''}`} key={`${c.path || c.name || i}-${i}`}><input type="checkbox" checked={checked[i] ?? false} disabled={busy || Boolean(c.duplicate || c.error)} onChange={(e) => setChecked((old) => old.map((v, n) => n === i ? e.target.checked : v))} /><ImportCandidatePreview preview={c.preview} aria-hidden="true" /><span className="lc-candidate-copy"><strong>{text(c.name, text(c.path, 'Photo'))}</strong><small>{text(c.path)}{c.duplicate ? ` · Duplicate (${c.duplicate})` : c.error ? ` · ${c.error}` : ''}{pairingNotice}</small></span></label>; }) : <Note>Choose photos or a folder to review files before adding them.</Note>}</div><div className="lc-form-stack"><Check checked={includeSubfolders} onChange={setIncludeSubfolders} disabled={busy}>Include subfolders</Check><Field label="Only extensions" value={allowedExtensions} onChange={setAllowedExtensions} placeholder="raw, dng (optional)" disabled={busy} /><Field label="Exclude extensions" value={excludedExtensions} onChange={setExcludedExtensions} placeholder="jpg, jpeg (optional)" disabled={busy} /><Select label="RAW/JPEG pairs" value={rawJpegPolicy} options={[['keepBoth', 'Keep both (default)'], ['rawOnly', 'RAW only']]} onChange={(value) => setRawJpegPolicy(value === 'rawOnly' ? 'rawOnly' : 'keepBoth')} disabled={busy} /><Select label="Add photos" value={mode} options={[["add", 'In place'], ['copy', 'Copy into library'], ['move', 'Move into library']]} onChange={setMode} disabled={busy} /><Field label="Existing album ID" value={album} onChange={setAlbum} placeholder="Optional" disabled={busy} /><Field label="New album" value={newAlbum} onChange={setNewAlbum} placeholder="Optional" disabled={busy} /><Field label="Keywords" value={keywords} onChange={setKeywords} placeholder="Comma-separated" disabled={busy} />{mode !== 'add' && <><Field label="Destination folder" value={destination} onChange={setDestination} placeholder="Library Originals by default" disabled={busy} /><Select label="Organize copies" value={organize} options={[["date", 'By capture date'], ['month', 'By month'], ['flat', 'One folder'], ['custom', 'Custom template']]} onChange={setOrganize} disabled={busy} />{organize === 'custom' && <><Field label="Folder template" value={folderTemplate} onChange={setFolderTemplate} placeholder="{date:%Y}/{date:%Y%m%d}" disabled={busy} /><Note>Use relative folders & photo tags such as {`{date:%Y}`}; no drive or parent folders.</Note></>}<Field label="File naming" value={rename} onChange={setRename} placeholder="Keep original names" disabled={busy} /><Check checked={dng} onChange={setDng} disabled={busy || mode !== 'copy'}>Copy raw files as DNG</Check></>}<Field label="Develop preset ID" value={preset} onChange={setPreset} placeholder="Optional" disabled={busy} /><Field label="Metadata preset" value={metadataPreset} onChange={setMetadataPreset} placeholder="Optional" disabled={busy} /></div></div></Frame>;
}

function folderTemplateError(template: string): string | null { const value = template.trim(); if (!value) return 'Enter a folder template.'; if (!value.includes('{') && !value.includes('/') && !value.includes('\\')) return 'Folder template must contain {, / or \\.'; if (value.startsWith('/') || value.startsWith('\\') || value.startsWith('~') || /^[A-Za-z]:/.test(value)) return 'Folder template must stay inside destination folder.'; if (value.split(/[\\/]/).some((part) => part.trim() === '.' || part.trim() === '..')) return 'Folder template cannot contain . or .. folders.'; return null; }

function LightroomDialog({ d, desktop }: { d: DialogState; desktop: DesktopContextValue }) {
  const [path, setCatalogPath] = useState(text(d.params?.path)); const [report, setReport] = useState<AnyRecord | null>((d.params?.report as AnyRecord) || null); const [updateExisting, setUpdateExisting] = useState(bool(d.params?.updateExisting)); const [busy, setBusy] = useState(false); const [error, setError] = useState('');
  const setPath = (value: string) => { setCatalogPath(value); setReport(null); setError(''); };
  const inspect = async (chosen?: string) => { const catalog = chosen || path; if (!catalog) return; setPath(catalog); setReport(null); setBusy(true); setError(''); try { const task = await desktop.run('library.inspectLightroom', { path: catalog }) as AnyRecord; const taskId = text(task.taskId); if (!taskId) throw new Error('LRCAT inspection did not return a task ID.'); desktop.setDialog({ kind: 'lightroomInspectProgress', params: { taskId, path: catalog } }); } catch (e) { setError(String(e)); setBusy(false); } };
  const importCatalog = async () => { if (!path) return; setBusy(true); setError(''); try { const task = await desktop.run('library.importLightroom', { path, updateExisting }) as AnyRecord; const taskId = text(task.taskId); if (!taskId) throw new Error('LRCAT import did not return a task ID.'); desktop.setDialog({ kind: 'lightroomImportProgress', params: { taskId, path, updateExisting, report: report || undefined } }); } catch (e) { setError(String(e)); setBusy(false); } };
  return <Frame title="Import LRCAT Catalog" wide busy={busy} onClose={() => desktop.setDialog(null)} actions={<><Button onClick={() => desktop.setDialog(null)}>Cancel</Button><Button primary disabled={!report || busy} onClick={() => void importCatalog()}>Import Catalog</Button></>}><div className="lc-dialog-toolbar"><Button onClick={async () => { const paths = await choose(desktop.native, 'pickLightroomCatalog'); if (paths[0]) void inspect(paths[0]); }}>Choose .lrcat…</Button></div><Field label="Catalog" value={path} onChange={setPath} placeholder="Select a LRCAT .lrcat catalog" />{error && <Note tone="error">{error}</Note>}{report ? <><div className="lc-summary-grid"><div><strong>{num(report.photos)}</strong><span>Photos</span></div><div><strong>{num(report.collections)}</strong><span>Collections</span></div><div><strong>{arr(report.missing).length}</strong><span>Missing originals</span></div><div><strong>{arr(report.warnings).length}</strong><span>Warnings</span></div></div><Check checked={updateExisting} onChange={setUpdateExisting}>Update existing edits when source catalog has changes</Check>{arr(report.missing).length > 0 && <Note tone="warning">Missing originals stay in catalog for later relinking.</Note>}{arr(report.warnings).length > 0 && <details className="lc-details"><summary>Review warnings</summary><ul>{arr(report.warnings).map((warning, i) => <li key={i}>{displayValue(warning)}</li>)}</ul></details>}</> : <Note>Ember reads catalog data without changing source catalog. Inspect first to review missing originals & warnings.</Note>}</Frame>;
}

function watermarkDefaults(value: unknown): AnyRecord {
  const source = typeof value === 'string' ? { text: value } : value && typeof value === 'object' && !Array.isArray(value) ? value as AnyRecord : {};
  return { ...source, text: text(source.text), vertical: bool(source.vertical), size: num(source.size, 0.035), opacity: num(source.opacity, 0.7), anchor: text(source.anchor, 'bottomRight'), inset: num(source.inset, 0.025), color: Array.isArray(source.color) ? source.color : [255, 255, 255], shadow: bool(source.shadow, true), image: text(source.image), imageWidth: num(source.imageWidth, 0.2) };
}
function watermarkHex(value: unknown): string { const color = arr(value).slice(0, 3).map((v) => Math.max(0, Math.min(255, num(v, 255)))); return `#${color.map((v) => Math.round(v).toString(16).padStart(2, '0')).join('') || 'ffffff'}`; }
function watermarkColor(value: string): number[] { const hex = value.replace('#', ''); return hex.length === 6 ? [0, 2, 4].map((offset) => Number.parseInt(hex.slice(offset, offset + 2), 16)) : [255, 255, 255]; }
function exportPercent(value: string, fallback: number, min: number, max: number): number { const parsed = Number(value); return Number.isFinite(parsed) ? Math.max(min, Math.min(max, parsed)) / 100 : fallback; }

function ExportDialog({ d, desktop }: { d: DialogState; desktop: DesktopContextValue }) {
  const p = d.params || {}; const supplied = (p.opts as AnyRecord) || {}; const [opts, setOpts] = useState<AnyRecord>({ ...supplied }); const [dir, setDir] = useState(text(p.dir)); const [fullSize, setFullSize] = useState(bool(p.fullSize, false)); const [preset, setPreset] = useState(text(p.preset)); const [watermark, setWatermark] = useState<AnyRecord>(() => watermarkDefaults(supplied.watermark)); const [watermarkOn, setWatermarkOn] = useState(() => { const value = watermarkDefaults(supplied.watermark); return Boolean(text(value.text).trim() || text(value.image).trim()); }); const [watermarkStyle, setWatermarkStyle] = useState(() => text(watermarkDefaults(supplied.watermark).image) ? 'graphic' : 'text'); const [busy, setBusy] = useState(false); const [error, setError] = useState(''); const set = (key: string, value: unknown) => setOpts((old) => ({ ...old, [key]: value })); const setWm = (key: string, value: unknown) => setWatermark((old) => ({ ...old, [key]: value })); const format = text(opts.format, 'jpeg'); const rendered = format !== 'dng' && format !== 'original';
  const submit = async () => { if (!dir) { setError('Choose an export folder.'); return; } setBusy(true); setError(''); try { const payload: AnyRecord = { ...opts, dir, ids: p.ids }; delete payload.rename; delete payload.resizeMode; delete payload.resizeValue; delete payload.fullSize; if (opts.rename) payload.naming = opts.rename; if (preset.trim()) payload.preset = preset.trim(); ['resize', 'longEdge', 'shortEdge', 'width', 'height', 'megapixels', 'percent'].forEach((key) => { delete payload[key]; }); if (fullSize) payload.longEdge = 0; else { const mode = text(opts.resizeMode, 'longEdge'); const key = mode === 'dimensions' ? 'width' : mode; payload[key] = num(opts.resizeValue, 2048); if (mode === 'dimensions') payload.height = num(opts.resizeHeight, num(opts.resizeValue, 2048)); payload.dontEnlarge = bool(opts.dontEnlarge, true); } if (watermarkOn && rendered) payload.watermark = { ...watermark }; else delete payload.watermark; const result = await desktop.run('app.export', payload) as AnyRecord; const taskId = text(result?.taskId); if (!taskId) throw new Error('Export did not return a task ID.'); desktop.setDialog({ kind: 'exportProgress', params: { taskId, dir, files: result?.files || [] } }); } catch (e) { setError(String(e)); setBusy(false); } };
  return (
    <Frame title="Export" wide busy={busy} onClose={() => desktop.setDialog(null)} actions={<><Button onClick={() => desktop.setDialog(null)}>Cancel</Button><Button primary disabled={busy} onClick={() => void submit()}>Export</Button></>}>
      <div className="lc-dialog-toolbar">
        <Button onClick={async () => { const paths = await choose(desktop.native, 'chooseFolder'); if (paths[0]) setDir(paths[0]); }}>Choose Export Folder…</Button>
        <Field label="Export folder" value={dir} onChange={setDir} placeholder="Choose or enter an export folder" />
      </div>
      {error && <Note tone="error">{error}</Note>}
      <div className="lc-export-columns">
        <div className="lc-form-stack">
          <Select label="Format" value={format} options={[["jpeg", 'JPEG'], ['png', 'PNG'], ['tiff', 'TIFF'], ['webp', 'WebP'], ['avif', 'AVIF'], ['original', 'Original + sidecar'], ['dng', 'DNG']]} onChange={(v) => set('format', v)} />
          {rendered && <>
            <Select label="Color space" value={text(opts.colorSpace, 'srgb')} options={[["srgb", 'sRGB'], ['displayP3', 'Display P3'], ['adobeRgb', 'Adobe RGB'], ['proPhoto', 'ProPhoto RGB'], ['rec2020', 'Rec. 2020']]} onChange={(v) => set('colorSpace', v)} />
            <Select label="Bit depth" value={String(opts.bitDepth ?? 8)} options={[["8", '8 bit'], ['16', '16 bit'], ['32', '32 bit']]} onChange={(v) => set('bitDepth', Number(v))} />
            <Field label="Quality" type="number" value={num(opts.quality, 90)} min={1} max={100} onChange={(v) => set('quality', Number(v))} />
            <Select label="Sharpen" value={text(opts.sharpen, 'none')} options={[["none", 'None'], ['screen', 'Screen'], ['matte', 'Matte'], ['glossy', 'Glossy']]} onChange={(v) => set('sharpen', v)} />
            {text(opts.sharpen, 'none') !== 'none' && <Select label="Sharpen amount" value={text(opts.sharpenAmount, 'standard')} options={[["low", 'Low'], ['standard', 'Standard'], ['high', 'High']]} onChange={(v) => set('sharpenAmount', v)} />}
          </>}
          <Field label="Naming template" value={text(opts.rename, text(opts.naming, '{name}'))} onChange={(v) => set('rename', v)} placeholder="{name}" />
          <Field label="Start number" type="number" value={num(opts.startNumber, 1)} min={1} onChange={(v) => set('startNumber', Number(v))} />
          <Select label="If file exists" value={text(opts.conflict, 'unique')} options={[["unique", 'Add a number'], ['overwrite', 'Overwrite'], ['skip', 'Skip']]} onChange={(v) => set('conflict', v)} />
          {format === 'tiff' && <Select label="TIFF compression" value={text(opts.tiffCompression, 'deflate')} options={[["none", 'None'], ['lzw', 'LZW'], ['deflate', 'ZIP']]} onChange={(v) => set('tiffCompression', v)} />}
          {format === 'dng' && <Select label="DNG compression" value={text(opts.dngCompression, 'lossless')} options={[["lossless", 'Lossless'], ['deflate', 'ZIP'], ['uncompressed', 'None']]} onChange={(v) => set('dngCompression', v)} />}
        </div>
        <div className="lc-form-stack">
          <Check checked={fullSize} onChange={setFullSize}>Full size</Check>
          {!fullSize && <>
            <Select label="Resize" value={text(opts.resizeMode, 'longEdge')} options={[["longEdge", 'Long edge'], ['shortEdge', 'Short edge'], ['width', 'Width'], ['height', 'Height'], ['dimensions', 'Width × height'], ['megapixels', 'Megapixels'], ['percent', 'Percentage']]} onChange={(v) => set('resizeMode', v)} />
            <Field label="Size" type="number" value={num(opts.resizeValue, 2048)} min={16} onChange={(v) => set('resizeValue', Number(v))} />
            {text(opts.resizeMode) === 'dimensions' && <Field label="Height" type="number" value={num(opts.resizeHeight, 2048)} min={16} onChange={(v) => set('resizeHeight', Number(v))} />}
            <Check checked={bool(opts.dontEnlarge, true)} onChange={(v) => set('dontEnlarge', v)}>Don't enlarge</Check>
          </>}
          <Field label="Resolution (ppi)" type="number" value={num(opts.ppi, 240)} min={1} onChange={(v) => set('ppi', Number(v))} />
          {rendered && <>
            <Check checked={watermarkOn} onChange={(enabled) => { setWatermarkOn(enabled); if (enabled && !text(watermark.text).trim() && !text(watermark.image).trim()) setWm('text', '© '); }}>Watermark</Check>
            {watermarkOn && <>
              <Select label="Watermark style" value={watermarkStyle} options={[["text", 'Text'], ['graphic', 'Graphic']]} onChange={(v) => { setWatermarkStyle(v); if (v === 'text') setWm('image', ''); }} />
              {watermarkStyle === 'graphic' ? <>
                <div className="lc-dialog-toolbar"><Button onClick={async () => { const paths = await choose(desktop.native, 'chooseFiles', { multiple: false }); if (paths[0]) setWm('image', paths[0]); }}>Choose Watermark Image…</Button></div>
                <Field label="Graphic path" value={text(watermark.image)} onChange={(v) => setWm('image', v)} placeholder="logo.png" />
                <Field label="Image width (%)" type="number" value={num(watermark.imageWidth, 0.2) * 100} min={2} max={100} step={1} onChange={(v) => setWm('imageWidth', exportPercent(v, 0.2, 2, 100))} />
              </> : <>
                <TextArea label="Watermark text" value={text(watermark.text)} onChange={(v) => setWm('text', v)} placeholder="© Your Name" />
                <Check checked={bool(watermark.vertical)} onChange={(v) => setWm('vertical', v)}>Vertical text</Check>
                <Field label="Size (%)" type="number" value={num(watermark.size, 0.035) * 100} min={1} max={15} step={0.5} onChange={(v) => setWm('size', exportPercent(v, 0.035, 1, 15))} />
                <Field label="Colour" type="color" value={watermarkHex(watermark.color)} onChange={(v) => setWm('color', watermarkColor(v))} />
                <Check checked={bool(watermark.shadow, true)} onChange={(v) => setWm('shadow', v)}>Shadow</Check>
              </>}
              <Select label="Watermark position" value={text(watermark.anchor, 'bottomRight')} options={[["topLeft", 'Top left'], ['topRight', 'Top right'], ['center', 'Center'], ['bottomLeft', 'Bottom left'], ['bottomRight', 'Bottom right']]} onChange={(v) => setWm('anchor', v)} />
              <Field label="Opacity (%)" type="number" value={num(watermark.opacity, 0.7) * 100} min={5} max={100} step={1} onChange={(v) => setWm('opacity', exportPercent(v, 0.7, 5, 100))} />
            </>}
          </>}
          <Select label="Metadata" value={text(opts.metadata, 'all')} options={[["all", 'All metadata'], ['allExceptCamera', 'All except camera'], ['copyright', 'Copyright only'], ['none', 'None']]} onChange={(v) => set('metadata', v)} />
          <Check checked={bool(opts.removeLocation)} onChange={(v) => set('removeLocation', v)}>Remove location</Check>
          <Field label="Subfolder" value={text(opts.subfolder)} onChange={(v) => set('subfolder', v)} placeholder="Optional" />
          <Field label="Limit file size (KB)" type="number" value={num(opts.limitKb, 0)} min={0} onChange={(v) => set('limitKb', Number(v))} />
          <Field label="Export preset" value={preset} onChange={setPreset} placeholder="Optional preset name" />
        </div>
      </div>
    </Frame>
  );
}

function MergeDialog({ kind, d, desktop }: { kind: string; d: DialogState; desktop: DesktopContextValue }) {
  const defaults: AnyRecord = { align: true, deghost: 'none', showOverlay: false, autoSettings: true, stack: false, projection: 'auto', boundaryWarp: 0, autoCrop: true, fillEdges: false, bracket: 0 };
  const suppliedOptions = d.params?.options && typeof d.params.options === 'object' && !Array.isArray(d.params.options) ? d.params.options as AnyRecord : {};
  const [options, setOptions] = useState<AnyRecord>({ ...defaults, ...suppliedOptions });
  const [error, setError] = useState('');
  const [previewError, setPreviewError] = useState('');
  const [previewBusy, setPreviewBusy] = useState(false);
  const [mergeBusy, setMergeBusy] = useState(false);
  const [preview, setPreview] = useState<{ descriptor: MergePreviewDescriptor; url: string } | null>(null);
  const sequenceRef = useRef(0);
  const currentPreview = useRef<{ descriptor: MergePreviewDescriptor; url: string } | null>(null);
  const mounted = useRef(true);
  const slot = 'main__merge';
  const suppliedIds = arr(d.params?.ids ?? suppliedOptions.ids).filter((id): id is number => typeof id === 'number' && Number.isSafeInteger(id));
  const selectedIds = suppliedIds.length ? suppliedIds : (desktop.snapshot?.selection || []).filter((id): id is number => Number.isSafeInteger(id));
  const selectionKey = selectedIds.join(',');
  const set = (key: string, value: unknown) => setOptions((old) => ({ ...old, [key]: value }));
  const params = () => {
    const clean = Object.fromEntries(Object.entries(options).filter(([key]) => key !== 'preview' && key !== 'previewPath'));
    return selectedIds.length ? { ...clean, ids: selectedIds } : clean;
  };
  const acknowledge = (handles: Iterable<string>) => {
    const unique = [...new Set(handles)].filter(Boolean);
    if (!unique.length) return;
    void Promise.allSettled(unique.map((handle) => acknowledgePreview(handle)));
  };
  const cancelPreview = () => {
    const sequence = sequenceRef.current;
    if (!sequence) return;
    sequenceRef.current += 1;
    void cancelMergePreview({ slot, sequence }).catch(() => undefined);
  };
  useEffect(() => {
    mounted.current = true;
    return () => {
      mounted.current = false;
      cancelPreview();
      const visible = currentPreview.current?.descriptor.handle;
      acknowledge([visible || '']);
      currentPreview.current = null;
    };
  }, []);
  useEffect(() => {
    if (!desktop.snapshot) return;
    const sequence = ++mergePreviewSequence;
    sequenceRef.current = sequence;
    const viewGeneration = desktop.snapshot.viewGeneration;
    let cancelled = false;
    setPreviewBusy(true);
    setPreviewError('');
    const request = async () => {
      let descriptor: MergePreviewDescriptor | null = null;
      try {
        descriptor = await requestMergePreview({ command: kind as 'merge.hdr' | 'merge.panorama' | 'merge.hdrPanorama', params: params(), slot, viewGeneration, sequence });
        if (cancelled || !mounted.current || sequence !== sequenceRef.current || descriptor.sequence !== sequence || descriptor.viewGeneration !== viewGeneration) {
          acknowledge([descriptor.handle]);
          return;
        }
        const url = convertFileSrc(descriptor.handle);
        const image = new Image();
        await new Promise<void>((resolve, reject) => {
          image.onload = () => { void (image.decode ? image.decode() : Promise.resolve()).then(() => resolve()).catch(reject); };
          image.onerror = () => reject(new Error('Merge preview could not be decoded.'));
          image.src = url;
        });
        if (cancelled || !mounted.current || sequence !== sequenceRef.current) {
          acknowledge([descriptor.handle]);
          return;
        }
        const previous = currentPreview.current?.descriptor.handle;
        currentPreview.current = { descriptor, url };
        const next = { descriptor, url };
        descriptor = null;
        setPreview(next);
        setPreviewBusy(false);
        setPreviewError('');
        acknowledge([previous || '']);
      } catch (reason) {
        if (descriptor) acknowledge([descriptor.handle]);
        if (cancelled || !mounted.current || sequence !== sequenceRef.current) return;
        setPreviewBusy(false);
        setPreviewError(errorText(reason));
      }
    };
    void request();
    return () => {
      cancelled = true;
      void cancelMergePreview({ slot, sequence }).catch(() => undefined);
    };
  }, [kind, options, desktop.snapshot?.viewGeneration, selectionKey]);
  const merge = async () => {
    if (mergeBusy) return;
    setMergeBusy(true);
    setError('');
    try { const result = await desktop.run(kind, params()) as AnyRecord; const taskId = text(result?.taskId); if (!taskId) throw new Error('Merge did not return a task ID.'); desktop.setDialog({ kind: `${kind}Progress`, params: { taskId } }); } catch (e) { setMergeBusy(false); setError(errorText(e)); }
  };
  const pano = kind !== 'merge.hdr'; const hdr = kind !== 'merge.panorama';
  const info = preview?.descriptor.info || {};
  const used = arr(info.used).length;
  const ev = arr(info.ev).filter((value): value is number => typeof value === 'number').map((value) => `${value >= 0 ? '+' : ''}${value.toFixed(1)}`).join(' / ');
  const summary = pano ? `${used} photos${text(info.projection) ? ` · ${text(info.projection)}` : ''}` : `${used || arr(info.ev).length} photos${ev ? ` · exposures ${ev} EV` : ''}`;
  return <Frame title={kind === 'merge.panorama' ? 'Panorama Merge Preview' : kind === 'merge.hdrPanorama' ? 'HDR Panorama Merge Preview' : 'HDR Merge Preview'} wide busy={mergeBusy} onClose={() => { cancelPreview(); desktop.setDialog(null); }} actions={<><Button onClick={() => { cancelPreview(); desktop.setDialog(null); }}>Cancel</Button><Button primary disabled={mergeBusy} onClick={() => void merge()}>Merge</Button></>}><div className="lc-merge-layout"><div className="lc-merge-preview" aria-label="Merge preview">{preview ? <img src={preview.url} alt="Merge preview" /> : <span>{previewError || (previewBusy ? 'Building preview…' : 'Select photos to preview.')}</span>}{previewBusy && preview && <span className="lc-merge-preview-status" aria-live="polite">Updating preview…</span>}{previewError && preview && <span className="lc-merge-preview-status" role="alert">{previewError}</span>}{preview && <small className="lc-merge-preview-info">{summary} · {preview.descriptor.width} × {preview.descriptor.height}</small>}</div><div className="lc-form-stack">{pano && <><Select label="Projection" value={text(options.projection, 'auto')} options={[["auto", 'Auto Select'], ['spherical', 'Spherical'], ['cylindrical', 'Cylindrical'], ['perspective', 'Perspective']]} onChange={(v) => set('projection', v)} /><Field label="Boundary warp" type="number" value={num(options.boundaryWarp, 0)} min={0} max={100} onChange={(v) => set('boundaryWarp', Number(v))} /><Check checked={bool(options.fillEdges)} onChange={(v) => set('fillEdges', v)}>Fill edges</Check><Check checked={bool(options.autoCrop, true)} onChange={(v) => set('autoCrop', v)}>Auto crop</Check></>}{hdr && <><Check checked={bool(options.align, true)} onChange={(v) => set('align', v)}>Auto align</Check><Select label="Deghost amount" value={text(options.deghost, 'none')} options={[["none", 'None'], ['low', 'Low'], ['medium', 'Medium'], ['high', 'High']]} onChange={(v) => set('deghost', v)} />{kind === 'merge.hdr' && <Check checked={bool(options.showOverlay)} disabled={text(options.deghost, 'none') === 'none'} onChange={(v) => set('showOverlay', v)}>Show deghost overlay</Check>}{kind === 'merge.hdrPanorama' && <Field label="Photos per bracket (0 = auto)" type="number" value={num(options.bracket, 0)} min={0} max={9} step={1} onChange={(v) => set('bracket', Math.max(0, Math.min(9, Math.round(Number(v) || 0))))} />}</>}{error && <Note tone="error">{error}</Note>}{previewError && !preview && <Note tone="error">{previewError}</Note>}<Check checked={bool(options.autoSettings, true)} onChange={(v) => set('autoSettings', v)}>Auto settings</Check><Check checked={bool(options.stack)} onChange={(v) => set('stack', v)}>Create stack</Check></div></div></Frame>;
}

function AlbumDialog({ folder, desktop, d }: { folder: boolean; desktop: DesktopContextValue; d: DialogState }) { const [name, setName] = useState(text(d.params?.name)); const [error, setError] = useState(''); const save = async () => { if (!name.trim()) { setError(`Enter a ${folder ? 'folder' : 'album'} name.`); return; } try { await desktop.run(folder ? 'album.create' : 'album.create', { name: name.trim(), folder, addSelected: !folder }); desktop.setDialog(null); } catch (e) { setError(String(e)); } }; return <Frame title={folder ? 'New Folder' : 'New Album'} onClose={() => desktop.setDialog(null)} actions={<><Button onClick={() => desktop.setDialog(null)}>Cancel</Button><Button primary onClick={() => void save()}>Create</Button></>}><Field label="Name" value={name} onChange={setName} placeholder={folder ? 'Folder name' : 'Album name'} />{error && <Note tone="error">{error}</Note>}</Frame>; }
function SmartAlbumDialog({ desktop, d, kind = 'smartAlbum' }: { desktop: DesktopContextValue; d: DialogState; kind?: string }) { const editing = kind === 'smartRules'; const [name, setName] = useState(text(d.params?.name)); const [field, setField] = useState(text(d.params?.field, 'rating')); const [op, setOp] = useState(text(d.params?.operator, field === 'rating' ? 'gte' : 'is')); const [value, setValue] = useState(text(d.params?.value, '3')); const save = async () => { try { const numeric = ['rating', 'iso', 'aperture', 'focalLength', 'megapixels', 'album', 'sharpness'].includes(field); const boolean = field === 'edited'; const parsed = Number(value); const ruleValue = numeric ? (Number.isFinite(parsed) ? parsed : 0) : boolean ? ['true', 'yes', '1'].includes(value.trim().toLowerCase()) : value; const rules = { ruleSet: { match: 'all', rules: [{ field, op, value: ruleValue }] } }; if (editing) await desktop.run('album.setRules', { id: num(d.params?.id), rules, replace: true }); else await desktop.run('album.createSmart', { name: name.trim() || 'Smart Album', rules }); desktop.setDialog(null); } catch (e) { desktop.setNotice(String(e)); } }; const options: [string, string][] = field === 'rating' ? [["is", 'Is'], ['gte', 'At least'], ['lte', 'At most']] : field === 'camera' || field === 'keywords' ? [["contains", 'Contains'], ['is', 'Is'], ['isNot', 'Is not']] : [["is", 'Is'], ['isNot', 'Is not']]; return <Frame title={editing ? 'Edit Smart Album' : 'Smart Album'} onClose={() => desktop.setDialog(null)} actions={<><Button onClick={() => desktop.setDialog(null)}>Cancel</Button><Button primary onClick={() => void save()}>{editing ? 'Save Rules' : 'Create Smart Album'}</Button></>}><Field label="Name" value={name} onChange={setName} placeholder="Smart Album" /><Select label="Match field" value={field} options={[["rating", 'Rating'], ['flag', 'Pick flag'], ['label', 'Color label'], ['camera', 'Camera'], ['keywords', 'Keywords'], ['edited', 'Has edits']]} onChange={(v) => { setField(v); setOp(v === 'rating' ? 'gte' : v === 'camera' || v === 'keywords' ? 'contains' : 'is'); }} /><Select label="Condition" value={op} options={options} onChange={setOp} /><Field label="Value" value={value} onChange={setValue} /></Frame>; }

function SettingsDialog({ desktop, d }: { desktop: DesktopContextValue; d: DialogState }) {
  const record = (value: unknown): AnyRecord => value && typeof value === 'object' && !Array.isArray(value) ? value as AnyRecord : {};
  const persisted = record(desktop.snapshot?.preferences);
  const persistedSettings = record(persisted.settings);
  const persistedUi = record(persisted.ui ?? persisted.layout ?? persisted);
  const supplied = record(d.params?.values);
  const startup = text(supplied.startupView, text(persistedUi.startupView, text(persistedSettings.startupView, desktop.ui.startupView)));
  const startupView = startup === 'grid' ? 'photoGrid' : ['last', 'photoGrid', 'detail'].includes(startup) ? startup : 'last';
  const overlay = supplied.infoOverlay ?? persistedUi.infoOverlay ?? persistedSettings.infoOverlay;
  const infoOverlay = typeof overlay === 'string' ? ({ off: 0, basic: 1, exposure: 2 }[overlay] ?? desktop.ui.infoOverlay) : num(overlay, desktop.ui.infoOverlay);
  const initial: AnyRecord = {
    ...persistedSettings, ...persistedUi, ...supplied,
    theme: text(supplied.theme, text(persistedUi.theme, desktop.ui.theme)),
    locale: text(supplied.locale, text(persistedUi.locale, desktop.ui.locale)),
    startupView,
    infoOverlay,
    confirmDelete: typeof supplied.confirmDelete === 'boolean' ? supplied.confirmDelete : typeof persistedUi.confirmDelete === 'boolean' ? persistedUi.confirmDelete : desktop.ui.confirmDelete,
    autoAdvance: typeof supplied.autoAdvance === 'boolean' ? supplied.autoAdvance : typeof persistedUi.autoAdvance === 'boolean' ? persistedUi.autoAdvance : typeof persistedSettings.autoAdvance === 'boolean' ? persistedSettings.autoAdvance : desktop.ui.autoAdvance,
    filmstrip: typeof supplied.filmstrip === 'boolean' ? supplied.filmstrip : typeof persistedUi.filmstrip === 'boolean' ? persistedUi.filmstrip : typeof persistedSettings.filmstrip === 'boolean' ? persistedSettings.filmstrip : desktop.ui.filmstrip,
    filmNames: typeof supplied.filmNames === 'boolean' ? supplied.filmNames : typeof persistedUi.filmNames === 'boolean' ? persistedUi.filmNames : typeof persistedSettings.filmNames === 'boolean' ? persistedSettings.filmNames : desktop.ui.filmNames,
    filmBadges: typeof supplied.filmBadges === 'boolean' ? supplied.filmBadges : typeof persistedUi.filmBadges === 'boolean' ? persistedUi.filmBadges : typeof persistedSettings.filmBadges === 'boolean' ? persistedSettings.filmBadges : desktop.ui.filmBadges,
    gridInfo: typeof supplied.gridInfo === 'boolean' ? supplied.gridInfo : typeof persistedUi.gridInfo === 'boolean' ? persistedUi.gridInfo : typeof persistedSettings.gridInfo === 'boolean' ? persistedSettings.gridInfo : desktop.ui.gridInfo,
    navigator: typeof supplied.navigator === 'boolean' ? supplied.navigator : typeof persistedUi.navigator === 'boolean' ? persistedUi.navigator : typeof persistedSettings.navigator === 'boolean' ? persistedSettings.navigator : desktop.ui.navigator,
    externalEditor: text(supplied.externalEditor, text(persistedUi.externalEditor, text(persistedSettings.externalEditor, desktop.ui.externalEditor))),
    previewEdge: num(supplied.previewEdge, num(persistedUi.previewEdge, desktop.ui.previewEdge)),
    memoryMb: Number(supplied.memoryMb ?? supplied.memoryBudget ?? persistedUi.memoryMb ?? desktop.ui.memoryMb),
    gpu: typeof supplied.gpu === 'boolean' ? supplied.gpu : typeof persistedUi.gpu === 'boolean' ? persistedUi.gpu : desktop.ui.gpu,
  };
  const requestedTab = text(d.params?.tab, 'general');
  const tabs = ['general', 'import', 'performance', 'interface', 'language'];
  const [tab, setTab] = useState(tabs.includes(requestedTab) ? requestedTab : 'general');
  const [values, setValues] = useState<AnyRecord>(initial);
  const [presets, setPresets] = useState<AnyRecord[]>([]);
  const [metadataPresets, setMetadataPresets] = useState<AnyRecord[]>([]);
  const [smartOriginalPath, setSmartOriginalPath] = useState('');
  const [loaded, setLoaded] = useState(false);
  const [loadError, setLoadError] = useState('');
  const [smartError, setSmartError] = useState('');
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState('');
  const dirty = useRef(new Set<string>());
  const sectionFor = (key: string) => ['theme', 'confirmDelete', 'startupView', 'autoAdvance', 'externalEditor'].includes(key) ? 'general' : ['filmstrip', 'filmNames', 'filmBadges', 'gridInfo', 'navigator', 'infoOverlay'].includes(key) ? 'interface' : key === 'locale' ? 'language' : ['rawPreset', 'otherPreset', 'perCamera', 'cameras', 'copyright', 'creator', 'metadataPreset', 'xmpAutoWrite', 'xmpNaming', 'autoFolder', 'autoCopy', 'autoAlbum'].includes(key) ? 'import' : 'performance';
  const sectionDirty = (section: string) => [...dirty.current].some((key) => sectionFor(key) === section);
  const update = (key: string, value: unknown) => { dirty.current.add(key); setValues((old) => ({ ...old, [key]: value })); };
  const cameraDefaults = arr(values.cameras).map((value) => record(value));

  useEffect(() => {
    let live = true;
    const load = async () => {
      try {
        const [libraryPrefs, xmpPrefs, gpu, memory, presetResult, metadataResult] = await Promise.all([
          desktop.run('library.preferences', {}), desktop.run('library.xmpPreferences', {}), desktop.run('app.gpu', {}), desktop.run('app.memoryBudget', {}), desktop.run('presets.list', {}), desktop.run('metadata.presets', {}),
        ]);
        let smart: AnyRecord = {};
        try { smart = record(await desktop.run('library.smartPreviewsLocation', {})); } catch (reason) { if (live) setSmartError(errorText(reason)); }
        if (!live) return;
        const library = record(libraryPrefs); const imports = record(library.import); const xmp = record(xmpPrefs);
        const smartPath = text(smart.path); setSmartOriginalPath(smartPath);
        setPresets(arr(presetResult).map(record)); setMetadataPresets(arr(metadataResult).map(record));
        setValues((old) => {
          const next = { ...old };
          if (!dirty.current.has('rawPreset')) next.rawPreset = imports.rawPreset == null ? old.rawPreset : text(imports.rawPreset, 'default');
          if (!dirty.current.has('otherPreset')) next.otherPreset = imports.otherPreset == null ? old.otherPreset : text(imports.otherPreset, 'default');
          if (!dirty.current.has('perCamera') && typeof imports.perCamera === 'boolean') next.perCamera = imports.perCamera;
          if (!dirty.current.has('cameras') && Array.isArray(imports.cameras)) next.cameras = imports.cameras.map((camera: unknown) => ({ ...record(camera), _settingsKey: `camera-${settingsCameraSequence++}` }));
          if (!dirty.current.has('copyright')) next.copyright = text(imports.copyright, text(old.copyright));
          if (!dirty.current.has('creator')) next.creator = text(imports.creator, text(old.creator));
          if (!dirty.current.has('metadataPreset')) next.metadataPreset = imports.metadataPreset == null ? 'none' : text(imports.metadataPreset);
          if (!dirty.current.has('autoFolder')) next.autoFolder = imports.autoFolder == null ? '' : text(imports.autoFolder);
          if (!dirty.current.has('autoCopy') && typeof imports.autoCopy === 'boolean') next.autoCopy = imports.autoCopy;
          if (!dirty.current.has('autoAlbum')) next.autoAlbum = imports.autoAlbum == null ? '' : text(imports.autoAlbum);
          if (!dirty.current.has('xmpAutoWrite') && typeof xmp.autoWrite === 'boolean') next.xmpAutoWrite = xmp.autoWrite;
          if (!dirty.current.has('xmpNaming')) next.xmpNaming = text(xmp.naming, text(old.xmpNaming, 'stem'));
          if (!dirty.current.has('cacheMb')) next.cacheMb = num(library.cacheMb, num(old.cacheMb));
          if (!dirty.current.has('forgetLocalDays')) next.forgetLocalDays = num(library.forgetLocalDays, num(old.forgetLocalDays));
          if (!dirty.current.has('smartPath')) next.smartPath = smartPath;
          next.smartAvailable = Boolean(smartPath); next.smartCount = num(smart.count); next.smartBytes = num(smart.bytes);
          if (!dirty.current.has('gpu') && typeof record(gpu).enabled === 'boolean') next.gpu = record(gpu).enabled;
          if (!dirty.current.has('memoryMb')) next.memoryMb = num(old.memoryMb) === 0 ? 0 : num(record(memory).budget, num(old.memoryMb * 1048576)) / 1048576;
          return next;
        });
        setLoaded(true);
      } catch (reason) {
        if (live) { setLoadError(errorText(reason)); setLoaded(true); }
      }
    };
    void load();
    return () => { live = false; };
  }, []);

  const save = async () => {
    if (!loaded) return;
    setBusy(true); setError('');
    const fields = dirty.current;
    const requestedMemoryMb = num(values.memoryMb, num(values.memoryBudget));
    if (fields.has('memoryMb') && requestedMemoryMb !== 0 && requestedMemoryMb < 64) {
      setError('Memory budget must be 0 (automatic) or at least 64 MB.');
      setBusy(false);
      return;
    }
    try {
      if (sectionDirty('import')) {
        const importPatch: AnyRecord = {};
        if (fields.has('rawPreset')) importPatch.rawPreset = text(values.rawPreset).trim() || 'default';
        if (fields.has('otherPreset')) importPatch.otherPreset = text(values.otherPreset).trim() || 'default';
        if (fields.has('perCamera')) importPatch.perCamera = bool(values.perCamera);
        if (fields.has('cameras')) importPatch.cameras = arr(values.cameras).map((value: unknown): AnyRecord => record(value)).map((camera: AnyRecord): AnyRecord => ({ camera: text(camera.camera).trim(), preset: text(camera.preset).trim() || null })).filter((camera: AnyRecord) => camera.camera);
        if (fields.has('copyright')) importPatch.copyright = text(values.copyright).trim();
        if (fields.has('creator')) importPatch.creator = text(values.creator).trim();
        if (fields.has('metadataPreset')) importPatch.metadataPreset = text(values.metadataPreset).trim() || null;
        if (Object.keys(importPatch).length) await desktop.run('library.preferences', { import: importPatch });
        if (fields.has('xmpAutoWrite') || fields.has('xmpNaming')) await desktop.run('library.xmpPreferences', { ...(fields.has('xmpAutoWrite') ? { autoWrite: bool(values.xmpAutoWrite) } : {}), ...(fields.has('xmpNaming') ? { naming: text(values.xmpNaming, 'stem') } : {}) });
        if (fields.has('autoFolder') || fields.has('autoCopy') || fields.has('autoAlbum')) await desktop.run('library.autoImport', { ...(fields.has('autoFolder') ? { folder: text(values.autoFolder).trim() || null } : {}), ...(fields.has('autoCopy') ? { copy: bool(values.autoCopy) } : {}), ...(fields.has('autoAlbum') ? { album: text(values.autoAlbum).trim() || null } : {}) });
      }
      if (sectionDirty('performance')) {
        if (fields.has('cacheMb')) await desktop.run('library.preferences', { cacheMb: Math.max(0, Math.round(num(values.cacheMb))) });
        if (fields.has('forgetLocalDays')) await desktop.run('library.preferences', { forgetLocalDays: Math.max(0, Math.round(num(values.forgetLocalDays))) });
        if (fields.has('clearCache') && values.clearCache) await desktop.run('library.clearPreviews', {});
        const existing = text(values.smartExisting, 'move');
        if (fields.has('smartReset') && bool(values.smartReset)) await desktop.run('library.smartPreviewsLocation', { reset: true, existing });
        else if (fields.has('smartPath') && text(values.smartPath) && text(values.smartPath) !== smartOriginalPath) await desktop.run('library.smartPreviewsLocation', { path: text(values.smartPath), existing });
        if (fields.has('gpu') && typeof values.gpu === 'boolean') await desktop.run('app.gpu', { enabled: values.gpu });
        if (fields.has('memoryMb') && requestedMemoryMb >= 64) await desktop.run('app.memoryBudget', { mb: Math.round(requestedMemoryMb) });
        const edge = Math.max(256, Math.min(8192, Math.round(num(values.previewEdge, desktop.ui.previewEdge))));
        if (fields.has('previewEdge') && edge !== desktop.ui.previewEdge) await desktop.run('library.buildPreviews', { size: 'standard', edge });
      }
      const uiPatch: AnyRecord = {};
      if (fields.has('theme')) uiPatch.theme = values.theme;
      if (fields.has('confirmDelete')) uiPatch.confirmDelete = values.confirmDelete;
      if (fields.has('startupView')) uiPatch.startupView = text(values.startupView, 'last');
      if (fields.has('autoAdvance')) uiPatch.autoAdvance = bool(values.autoAdvance);
      if (fields.has('externalEditor')) uiPatch.externalEditor = text(values.externalEditor).trim();
      if (fields.has('filmstrip')) uiPatch.filmstrip = bool(values.filmstrip);
      if (fields.has('filmNames')) uiPatch.filmNames = bool(values.filmNames);
      if (fields.has('filmBadges')) uiPatch.filmBadges = bool(values.filmBadges);
      if (fields.has('gridInfo')) uiPatch.gridInfo = bool(values.gridInfo);
      if (fields.has('navigator')) uiPatch.navigator = bool(values.navigator);
      if (fields.has('infoOverlay')) uiPatch.infoOverlay = Math.max(0, Math.min(2, Math.round(num(values.infoOverlay, desktop.ui.infoOverlay))));
      if (fields.has('locale')) uiPatch.locale = values.locale;
      if (fields.has('previewEdge')) uiPatch.previewEdge = Math.max(256, Math.min(8192, Math.round(num(values.previewEdge, desktop.ui.previewEdge))));
      if (fields.has('gpu')) uiPatch.gpu = values.gpu;
      if (fields.has('memoryMb')) uiPatch.memoryMb = num(values.memoryMb, desktop.ui.memoryMb);
      if (Object.keys(uiPatch).length) { await desktop.native('preferences.patch', { ui: uiPatch }); desktop.setUi(uiPatch as Partial<UiState>); }
      desktop.setDialog(null);
    } catch (reason) { setError(errorText(reason)); } finally { setBusy(false); }
  };

  const addCamera = () => update('cameras', [...cameraDefaults, { _settingsKey: `camera-${settingsCameraSequence++}`, camera: '', preset: '' }]);
  const removeCamera = (index: number) => update('cameras', cameraDefaults.filter((_, i) => i !== index));
  const updateCamera = (index: number, key: string, value: string) => update('cameras', cameraDefaults.map((camera, i) => i === index ? { ...camera, [key]: value } : camera));
  const chooseAutoFolder = async () => { const paths = await choose(desktop.native, 'chooseFolder'); if (paths[0]) update('autoFolder', paths[0]); };
  const chooseSmartFolder = async () => { const paths = await choose(desktop.native, 'chooseFolder'); if (paths[0]) { update('smartPath', paths[0]); update('smartReset', false); } };
  const openLibrary = async () => { setBusy(true); setError(''); try { await desktop.run('app.openLibrary', {}); } catch (reason) { setError(errorText(reason)); } finally { setBusy(false); } };
  const presetChoices = (noneLabel: string, current = ''): [string, string][] => { const options: [string, string][] = [['default', noneLabel], ...presets.filter((preset) => text(preset.id)).map((preset): [string, string] => [text(preset.id), text(preset.name, text(preset.id))])]; if (current && current !== 'default' && !options.some(([id]) => id === current)) options.push([current, `${current} (missing)`]); return options; };
  const metadataValue = text(values.metadataPreset, 'none');
  const metadataChoices: [string, string][] = [['none', 'None'], ...metadataPresets.filter((preset) => text(preset.name)).map((preset): [string, string] => [text(preset.name), text(preset.name)])];
  if (metadataValue !== 'none' && !metadataChoices.some(([name]) => name === metadataValue)) metadataChoices.push([metadataValue, `${metadataValue} (missing)`]);
  const importTab = <>
    <Select label="Raw photos" value={text(values.rawPreset, 'default')} options={presetChoices('Ember Default', text(values.rawPreset))} onChange={(v) => update('rawPreset', v)} />
    <Select label="Non-raw photos" value={text(values.otherPreset, 'default')} options={presetChoices('None', text(values.otherPreset))} onChange={(v) => update('otherPreset', v)} />
    <Check checked={bool(values.perCamera)} onChange={(v) => update('perCamera', v)}>Use camera-specific defaults</Check>
    {bool(values.perCamera) && <div className="lc-form-stack">{cameraDefaults.map((camera, index) => <div key={text(camera._settingsKey, `camera-${index}`)} className="lc-dialog-toolbar"><Field label="Camera" value={text(camera.camera)} onChange={(v) => updateCamera(index, 'camera', v)} placeholder="Make Model" /><Select label="Preset" value={text(camera.preset, 'default')} options={presetChoices('Ember Default', text(camera.preset))} onChange={(v) => updateCamera(index, 'preset', v)} /><Button onClick={() => removeCamera(index)}>Remove</Button></div>)}<Button onClick={addCamera}>Add camera default</Button></div>}
    <Field label="Default copyright" value={text(values.copyright)} onChange={(v) => update('copyright', v)} /><Field label="Default creator" value={text(values.creator)} onChange={(v) => update('creator', v)} />
    <Select label="Metadata preset" value={metadataValue} options={metadataChoices} onChange={(v) => update('metadataPreset', v)} />
    <h3 className="lc-section-heading">XMP sidecars</h3><Check checked={bool(values.xmpAutoWrite)} onChange={(v) => update('xmpAutoWrite', v)}>Automatically write changes into XMP sidecars</Check><Select label="Sidecar names" value={text(values.xmpNaming, 'stem')} options={[['stem', 'IMG_1.xmp'], ['full', 'IMG_1.CR3.xmp']]} onChange={(v) => update('xmpNaming', v)} />
    <h3 className="lc-section-heading">Auto Import</h3><Note>Watched folder is scanned every few seconds while desktop is open.</Note><Field label="Watched folder" value={text(values.autoFolder)} onChange={(v) => update('autoFolder', v)} placeholder="Off" /><div className="lc-dialog-toolbar"><Button onClick={() => void chooseAutoFolder()}>Choose Folder…</Button><Button onClick={() => update('autoFolder', '')}>Turn Off</Button></div><Check checked={bool(values.autoCopy)} onChange={(v) => update('autoCopy', v)}>Copy into library</Check><Field label="Auto-import album" value={text(values.autoAlbum)} onChange={(v) => update('autoAlbum', v)} placeholder="None" />
  </>;
  const performanceTab = <>
    <Field label="Preview edge" type="number" value={num(values.previewEdge, desktop.ui.previewEdge)} min={256} max={8192} step={128} onChange={(v) => update('previewEdge', Number(v))} /><Field label="Cache size (MB)" type="number" value={num(values.cacheMb, 0)} min={0} onChange={(v) => update('cacheMb', Number(v))} /><Field label="Forget unchanged photos after (days)" type="number" value={num(values.forgetLocalDays, 0)} min={0} onChange={(v) => update('forgetLocalDays', Number(v))} /><Button onClick={() => update('clearCache', true)}>Clear thumbnail cache</Button><Field label="Memory budget (MB; 0 = automatic)" type="number" value={num(values.memoryMb, desktop.ui.memoryMb)} min={0} onChange={(v) => update('memoryMb', Number(v))} /><Check checked={bool(values.gpu, desktop.ui.gpu)} onChange={(v) => update('gpu', v)}>Use GPU rendering when available</Check>
    <h3 className="lc-section-heading">Smart previews</h3>{bool(values.smartAvailable) ? <><Field label="Folder" value={text(values.smartPath)} onChange={(v) => { update('smartPath', v); update('smartReset', false); }} /><Select label="Existing previews" value={text(values.smartExisting, 'move')} options={[['move', 'Move them'], ['leave', 'Leave them'], ['discard', 'Delete them']]} onChange={(v) => update('smartExisting', v)} /><div className="lc-dialog-toolbar"><Button onClick={() => void chooseSmartFolder()}>Choose Folder…</Button><Button onClick={() => update('smartReset', true)}>Use library folder</Button></div><Note>{num(values.smartCount)} smart previews · {Math.round(num(values.smartBytes) / 1048576)} MB</Note></> : <Note>Smart previews require an on-disk library.</Note>}
  </>;
  const libraryPath = text(desktop.snapshot?.libraryPath);
  const overlayValue = String(Math.max(0, Math.min(2, Math.round(num(values.infoOverlay, desktop.ui.infoOverlay)))));
  const generalTab = <><Select label="Theme" value={text(values.theme, desktop.ui.theme)} options={[['system', 'System'], ['light', 'Light'], ['dark', 'Dark']]} onChange={(v) => update('theme', v)} /><h3 className="lc-section-heading">Library</h3><div className="lc-dialog-toolbar"><span className="lc-path">{libraryPath || 'In memory — nothing is saved'}</span><Button disabled={busy} onClick={() => void openLibrary()}>Open Library…</Button></div><h3 className="lc-section-heading">Startup</h3><Select label="Open in" value={text(values.startupView, 'last')} options={[['last', 'Last view'], ['photoGrid', 'Photo Grid'], ['detail', 'Detail']]} onChange={(v) => update('startupView', v)} /><h3 className="lc-section-heading">Culling</h3><Check checked={bool(values.autoAdvance, desktop.ui.autoAdvance)} onChange={(v) => update('autoAdvance', v)}>Auto Advance: move to next photo after rating or flagging</Check><Check checked={bool(values.confirmDelete, desktop.ui.confirmDelete)} onChange={(v) => update('confirmDelete', v)}>Confirm photo deletion</Check><h3 className="lc-section-heading">External Editor</h3><Field label="Application" value={text(values.externalEditor)} onChange={(v) => update('externalEditor', v)} placeholder="System default" /><Note>Photo &gt; Edit in External Editor renders a TIFF copy, stacks it with original & opens it here.</Note></>;
  const interfaceTab = <><h3 className="lc-section-heading">Filmstrip</h3><Check checked={bool(values.filmstrip, desktop.ui.filmstrip)} onChange={(v) => update('filmstrip', v)}>Show filmstrip</Check><Check checked={bool(values.filmNames, desktop.ui.filmNames)} onChange={(v) => update('filmNames', v)}>Show file names</Check><Check checked={bool(values.filmBadges, desktop.ui.filmBadges)} onChange={(v) => update('filmBadges', v)}>Show ratings, flags & edit badges</Check><h3 className="lc-section-heading">Grid</h3><Check checked={bool(values.gridInfo, desktop.ui.gridInfo)} onChange={(v) => update('gridInfo', v)}>Show grid info</Check><h3 className="lc-section-heading">Detail</h3><Check checked={bool(values.navigator, desktop.ui.navigator)} onChange={(v) => update('navigator', v)}>Show Navigator while zoomed in</Check><Select label="Info overlay" value={overlayValue} options={[['0', 'Off'], ['1', 'File & date'], ['2', 'Exposure']]} onChange={(v) => update('infoOverlay', Number(v))} /></>;
  return <Frame title="Settings" wide busy={busy} onClose={() => desktop.setDialog(null)} actions={<><Button disabled={busy} onClick={() => desktop.setDialog(null)}>Cancel</Button><Button primary disabled={busy || !loaded || Boolean(loadError)} onClick={() => void save()}>Save</Button></>}><div className="lc-settings"><nav aria-label="Settings sections">{tabs.map((item) => <button key={item} data-settings-section={item} className={tab === item ? 'lc-settings-tab active' : 'lc-settings-tab'} onClick={() => setTab(item)}>{item[0].toUpperCase() + item.slice(1)}</button>)}</nav><div className="lc-settings-panel">{!loaded && <Note>Loading saved settings…</Note>}{loadError && <Note tone="error">Could not load saved settings: {loadError}</Note>}{tab === 'general' && generalTab}{tab === 'import' && importTab}{tab === 'performance' && performanceTab}{tab === 'interface' && interfaceTab}{tab === 'language' && <><Select label="Language" value={text(values.locale, desktop.ui.locale)} options={[['en', 'English'], ['zh-hans', '简体中文'], ['zh-hant', '繁體中文'], ['ja', '日本語'], ['pt-br', 'Português (Brasil)']]} onChange={(v) => update('locale', v)} /><Note>Language selection is saved & applied to shell and command labels.</Note></>}{smartError && <Note tone="error">Smart preview settings unavailable: {smartError}</Note>}{error && <Note tone="error">{error}</Note>}</div></div></Frame>;
}

// The engine's `SettingsGroup` ids and labels (crates/develop/src/presets.rs), in its order; unknown ids are refused.
const SETTINGS_GROUPS: readonly (readonly [string, string])[] = [['profile', 'Profile'], ['treatment', 'Treatment'], ['whiteBalance', 'White Balance'], ['light', 'Light'], ['toneCurve', 'Tone Curve'], ['color', 'Color'], ['colorMixer', 'Color Mixer'], ['colorGrading', 'Color Grading'], ['effects', 'Effects'], ['vignette', 'Vignette'], ['grain', 'Grain'], ['detail', 'Detail'], ['optics', 'Optics'], ['geometry', 'Geometry'], ['crop', 'Crop'], ['masks', 'Masking'], ['spots', 'Remove'], ['redEye', 'Red Eye'], ['lensBlur', 'Lens Blur'], ['calibration', 'Calibration']];
const SETTINGS_GROUP_IDS = SETTINGS_GROUPS.map(([id]) => id);
// `SettingsGroup::default_copy`: everything but crop, masks, spots and red eye.
const DEFAULT_COPY_GROUPS = SETTINGS_GROUP_IDS.filter((id) => !['crop', 'masks', 'spots', 'redEye'].includes(id));
function GroupsDialog({ paste, desktop, d }: { paste: boolean; desktop: DesktopContextValue; d: DialogState }) { const initial = arr(d.params?.groups).map(String).filter((group) => SETTINGS_GROUP_IDS.includes(group)); const [groups, setGroups] = useState<string[]>(initial.length ? initial : DEFAULT_COPY_GROUPS); const toggle = (group: string) => setGroups((old) => old.includes(group) ? old.filter((item) => item !== group) : [...old, group]); const submit = async () => { try { await desktop.run(paste ? 'develop.paste' : 'develop.copy', { groups }); desktop.setDialog(null); } catch (e) { desktop.setNotice(String(e)); } }; return <Frame title={paste ? 'Paste Selected Edit Settings' : 'Choose Edit Settings to Copy'} onClose={() => desktop.setDialog(null)} actions={<><Button onClick={() => desktop.setDialog(null)}>Cancel</Button><Button primary onClick={() => void submit()}>{paste ? 'Paste Settings' : 'Copy Settings'}</Button></>}><div className="lc-group-list">{SETTINGS_GROUPS.map(([group, label]) => <Check key={group} checked={groups.includes(group)} onChange={() => toggle(group)}>{label}</Check>)}</div><div className="lc-dialog-toolbar"><Button onClick={() => setGroups([...SETTINGS_GROUP_IDS])}>Select all</Button><Button onClick={() => setGroups([])}>Clear</Button></div></Frame>; }

function PresetFileDialog({ exportMode, curve, desktop, d }: { exportMode: boolean; curve: boolean; desktop: DesktopContextValue; d: DialogState }) { const [path, setPath] = useState(text(d.params?.path)); const [busy, setBusy] = useState(false); const chooseAction = exportMode ? (curve ? 'saveCurvePresetFile' : 'savePresetFile') : (curve ? 'pickCurvePresetFiles' : 'pickPresetFiles'); const command = curve ? (exportMode ? 'curve.exportPresets' : 'curve.importPresets') : (exportMode ? 'preset.export' : 'preset.import'); const submit = async () => { const paths = path ? [path] : await choose(desktop.native, chooseAction); if (!paths[0]) return; setPath(paths[0]); setBusy(true); try { await desktop.run(command, exportMode ? { path: paths[0] } : { paths }); desktop.setDialog(null); } catch (e) { desktop.setNotice(String(e)); } finally { setBusy(false); } }; return <Frame title={`${exportMode ? 'Export' : 'Import'} ${curve ? 'Curve Presets' : 'Presets'}`} busy={busy} onClose={() => desktop.setDialog(null)} actions={<><Button onClick={() => desktop.setDialog(null)}>Cancel</Button><Button primary disabled={busy} onClick={() => void submit()}>{exportMode ? 'Export' : 'Import'}</Button></>}><div className="lc-dialog-toolbar"><Button onClick={() => void submit()}>{exportMode ? 'Choose Save Location…' : 'Choose Preset Files…'}</Button></div><Field label={exportMode ? 'Save as' : 'Files or folder'} value={path} onChange={setPath} placeholder={curve ? '.lccurve' : '.lcpreset, .xmp or .lrtemplate'} /><Note>Imported presets keep source names & report settings that cannot be carried over.</Note></Frame>; }

function MetadataDialog({ d, desktop }: { d: DialogState; desktop: DesktopContextValue }) { const metadata = (d.params?.metadata as AnyRecord) || {}; return <Frame title="All Metadata" wide onClose={() => desktop.setDialog(null)} actions={<Button primary onClick={() => desktop.setDialog(null)}>Done</Button>}><dl className="lc-info-list">{Object.entries(metadata).map(([key, value]) => <React.Fragment key={key}><dt>{key}</dt><dd>{typeof value === 'string' ? value : JSON.stringify(value)}</dd></React.Fragment>)}</dl></Frame>; }

function InfoDialog({ kind, desktop, d }: { kind: string; desktop: DesktopContextValue; d: DialogState }) {
  const title = kind === 'about' ? 'About Ember' : kind === 'systeminfo' ? 'System Information' : kind === 'shortcuts' ? 'Keyboard Shortcuts' : "What's New";
  const initialRows = arr(d.params?.rows);
  const [rows, setRows] = useState<any[]>(initialRows);
  const [credits, setCredits] = useState<AnyRecord | null>(null);
  const [creditsError, setCreditsError] = useState<string | null>(null);
  const [aboutAttempt, setAboutAttempt] = useState(0);
  const loaded = useRef(false);
  const aboutLoaded = useRef(false);
  useEffect(() => {
    if (kind === 'systeminfo' && !rows.length && !loaded.current) {
      loaded.current = true;
      void desktop.run('library.info', {}).then((info) => {
        if (info && typeof info === 'object') setRows(Object.entries(info as AnyRecord));
      });
    }
  }, [kind, desktop, rows.length]);
  useEffect(() => {
    if (kind !== 'about' || aboutLoaded.current) return;
    aboutLoaded.current = true;
    let live = true;
    setCreditsError(null);
    void desktop.native('aboutInfo', {}).then((info) => {
      if (!live) return;
      if (info && typeof info === 'object' && !Array.isArray(info)) setCredits(info as AnyRecord);
      else setCreditsError('Native credits data is unavailable.');
    }).catch((reason: unknown) => {
      if (live) setCreditsError(errorText(reason));
    });
    return () => { live = false; };
  }, [kind, desktop, aboutAttempt]);
  const contributors = arr(credits?.contributors);
  const models = arr(credits?.models);
  return <Frame title={title} onClose={() => desktop.setDialog(null)} actions={<Button primary onClick={() => desktop.setDialog(null)}>Done</Button>}>
    {kind === 'about' && <div className="lc-about">
      <img className="lc-app-mark" src={APP_ICON_URL} alt="" /><h3>{APP_NAME}</h3><p>Native photo library & non-destructive developer.</p>
      <small>Ember, maintained by Orthic Labs & contributors.</small>
      <small>Open source · version {text(credits?.version, text(d.params?.version, '0.2.1'))}</small>
      {credits ? <>
        <details className="lc-details" open><summary>Contributors ({contributors.length})</summary><ul>{contributors.map((value, i) => {
          const contributor = value && typeof value === 'object' ? value as AnyRecord : {};
          return <li key={text(contributor.login, `contributor-${i}`)}><strong>{text(contributor.login, 'Unknown contributor')}</strong> · {num(contributor.prs)} PRs · {num(contributor.commits)} commits</li>;
        })}</ul></details>
        <details className="lc-details"><summary>AI models ({models.length})</summary><ul>{models.map((value, i) => {
          const model = value && typeof value === 'object' ? value as AnyRecord : {};
          const version = text(model.version);
          return <li key={text(model.id, `model-${i}`)}><strong>{text(model.company, 'Unknown')}</strong> · {text(model.model, 'Model')}{version ? ` ${version}` : ''} · {num(model.commits)} commits</li>;
        })}</ul></details>
      </> : creditsError ? <><Note tone="error">Could not load credits: {creditsError}</Note><Button onClick={() => { aboutLoaded.current = false; setCredits(null); setCreditsError(null); setAboutAttempt((attempt) => attempt + 1); }}>Retry</Button></> : <Note>Loading credits…</Note>}
    </div>}
    {kind === 'systeminfo' && <dl className="lc-info-list">{rows.map((row, i) => { const pair = Array.isArray(row) ? row : [row.label, row.value]; return <React.Fragment key={i}><dt>{text(pair[0])}</dt><dd>{text(pair[1], JSON.stringify(pair[1]))}</dd></React.Fragment>; })}</dl>}
    {kind === 'shortcuts' && <div className="lc-shortcuts">{arr(d.params?.shortcuts).map((shortcut, i) => <div key={i}><span>{text(shortcut.label, text(shortcut.id))}</span><kbd>{text(shortcut.shortcut)}</kbd></div>)}</div>}
    {kind === 'whatsnew' && <div className="lc-release-notes">{text(d.params?.text, 'Recent improvements to import, develop, export & catalog interoperability.')}</div>}
  </Frame>;
}

function FormDialog({ kind, d, desktop }: { kind: string; d: DialogState; desktop: DesktopContextValue }) { const formKind = kind.toLowerCase(); const p = d.params || {}; const [values, setValues] = useState<AnyRecord>({ ...p }); const update = (key: string, value: unknown) => setValues((old) => ({ ...old, [key]: value })); const config: AnyRecord = { rename: ['Rename Photos', 'photo.rename', [['template', 'Name template'], ['start', 'Start number']]], renamealbum: ['Rename Album', 'album.rename', [['id', 'Album ID'], ['name', 'Name']]], capturetime: ['Edit Capture Time', 'photo.setCaptureTime', [['time', 'Capture time'], ['shift', 'Shift seconds'], ['hours', 'Time-zone shift hours']]], autostack: ['Auto-Stack by Capture Time', 'stack.auto', [['gap', 'Maximum gap (seconds)']]], createpreset: ['Create Preset', 'preset.create', [['name', 'Preset name'], ['group', 'Group']]], copysettings: ['Copy Edit Settings', 'develop.copy', []], pastesettings: ['Paste Edit Settings', 'develop.paste', []], labelnames: ['Color Label Names', 'label.setNames', [['red', 'Red'], ['yellow', 'Yellow'], ['green', 'Green'], ['blue', 'Blue'], ['purple', 'Purple']]], savemetadatapreset: ['Save Metadata Preset', 'metadata.savePreset', [['name', 'Preset name']]], cull: ['Assisted Culling', 'photo.analyze', [['rejectBelow', 'Reject below focus score']]], textprompt: [text(p.title, 'Enter a value'), text(p.command), [[text(p.key, 'value'), text(p.hint, 'Value')]]], metadatapreset: ['Metadata Preset', 'metadata.applyPreset', [['name', 'Preset name']]], renamekeyword: ['Rename Keyword', 'keyword.rename', [['from', 'Current keyword'], ['to', 'New keyword']]], mergekeywords: ['Merge Keywords', 'keyword.merge', [['from', 'Keywords to merge (comma-separated)'], ['into', 'Merge into']]] }; const c = config[formKind]; const submit = async () => { try { let payload: AnyRecord = { ...values }; if (formKind === 'labelnames') payload = { names: Object.fromEntries(['red', 'yellow', 'green', 'blue', 'purple'].map((label) => [label, text(values[label]) || null])) }; else if (formKind === 'capturetime') { payload = {}; if (Array.isArray(values.ids)) payload.ids = values.ids; if (text(values.time)) payload.time = text(values.time); else if (values.shift !== undefined && text(values.shift) !== '') payload.shift = num(values.shift); else if (values.hours !== undefined && text(values.hours) !== '') payload.hours = num(values.hours); if (bool(values.each)) payload.each = true; } else if (formKind === 'rename') payload = { template: text(values.template), start: num(values.start, 1), ...(Array.isArray(values.ids) ? { ids: values.ids } : {}) }; else if (formKind === 'renamealbum') payload = { id: num(values.id), name: text(values.name) }; else if (formKind === 'autostack') payload = { gap: num(values.gap), ...(bool(values.preview) ? { preview: true } : {}) }; else if (formKind === 'createpreset') payload = { name: text(values.name), ...(text(values.group) ? { group: text(values.group) } : {}) }; else if (formKind === 'copysettings' || formKind === 'pastesettings') payload = { ...(Array.isArray(values.groups) ? { groups: values.groups } : {}) }; else if (formKind === 'metadatapreset') payload = { name: text(values.name) }; else if (formKind === 'savemetadatapreset') payload = { name: text(values.name), ...(values.fields ? { fields: values.fields } : {}) }; else if (formKind === 'renamekeyword') payload = { from: text(values.from), to: text(values.to) }; else if (formKind === 'mergekeywords') payload = { from: text(values.from).split(',').map((item) => item.trim()).filter(Boolean), into: text(values.into) }; else if (formKind === 'cull') payload = { ...(Array.isArray(values.ids) ? { ids: values.ids } : {}), rejectBelow: num(values.rejectBelow), pickBest: bool(values.pickBest, true) }; else if (formKind === 'textprompt') { const promptKey = text(p.key, 'value'); payload = { ...(typeof values.path === 'string' ? { path: values.path } : {}), [promptKey]: text(values[promptKey]) }; } await desktop.run(c[1], payload); desktop.setDialog(null); } catch (e) { desktop.setNotice(String(e)); } }; return <Frame title={c[0]} onClose={() => desktop.setDialog(null)} actions={<><Button onClick={() => desktop.setDialog(null)}>Cancel</Button><Button primary onClick={() => void submit()}>Apply</Button></>}><div className="lc-form-stack">{arr(c[2]).map((field: any[]) => <Field key={field[0]} label={field[1]} value={typeof values[field[0]] === 'number' ? values[field[0]] : text(values[field[0]])} type={typeof values[field[0]] === 'number' ? 'number' : 'text'} onChange={(v) => update(field[0], typeof values[field[0]] === 'number' ? Number(v) : v)} />)}{formKind === 'capturetime' && <Check checked={bool(values.each)} onChange={(v) => update('each', v)}>Set this time on every selected photo</Check>}{['copysettings', 'pastesettings'].includes(formKind) && <Note>Choose groups in Inspector before applying settings.</Note>}{formKind === 'cull' && <Check checked={bool(values.pickBest, true)} onChange={(v) => update('pickBest', v)}>Keep best frame in each burst</Check>}</div></Frame>; }

const EXPLICIT_FORM_KINDS = new Set(['rename', 'renamealbum', 'capturetime', 'autostack', 'createpreset', 'copysettings', 'pastesettings', 'labelnames', 'savemetadatapreset', 'cull', 'textprompt', 'metadatapreset', 'renamekeyword', 'mergekeywords']);
function SamModelSetupDialogHost({ desktop }: { desktop: DesktopContextValue }) {
  const licenseUrl = 'https://github.com/facebookresearch/sam3/blob/main/LICENSE';
  const [status, setStatus] = useState<AnyRecord | null>(null);
  const [accepted, setAccepted] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState('');
  const load = async () => {
    try { setStatus((await desktop.run('segment.model.status', {})) as AnyRecord); }
    catch (reason) { setError(errorText(reason)); }
  };
  useEffect(() => {
    void load();
    const timer = window.setInterval(() => { void load(); }, 750);
    return () => window.clearInterval(timer);
  }, []);
  const download = async () => {
    if (!accepted || busy) return;
    setBusy(true); setError('');
    try { await desktop.run('segment.model.download', { acknowledged: true }); await load(); }
    catch (reason) { setError(errorText(reason)); }
    finally { setBusy(false); }
  };
  const cancelDownload = async () => {
    setBusy(true); setError('');
    try { await desktop.run('segment.model.cancel', {}); await load(); }
    catch (reason) { setError(errorText(reason)); }
    finally { setBusy(false); }
  };
  const chooseFolder = async () => {
    setBusy(true); setError('');
    try {
      const selected = await desktop.native('selectFolder', {});
      const path = typeof selected === 'string' ? selected : selected && typeof selected === 'object' && 'path' in selected ? text((selected as AnyRecord).path) : '';
      if (!path) return;
      await desktop.run('segment.model.selectFolder', { path });
      for (;;) {
        await new Promise<void>((resolve) => window.setTimeout(resolve, 250));
        const next = await desktop.run('segment.model.status', {}) as AnyRecord;
        setStatus(next);
        const validation = next.validation && typeof next.validation === 'object' ? next.validation as AnyRecord : {};
        if (!bool(validation.running)) {
          const validationError = text(validation.error);
          if (validationError) throw new Error(validationError);
          break;
        }
      }
      await desktop.native('preferences.patch', { sam3Dir: path });
      await load();
    } catch (reason) { setError(errorText(reason)); }
    finally { setBusy(false); }
  };
  const cancelValidation = async () => {
    setError('');
    try { await desktop.run('segment.model.cancelSelection', {}); }
    catch (reason) { setError(errorText(reason)); }
  };
  const openFolder = async () => {
    const path = text(status?.dir);
    if (!path) return;
    try { await desktop.native('openFolder', { path }); }
    catch (reason) { setError(errorText(reason)); }
  };
  const downloadState = (status?.download && typeof status.download === 'object') ? status.download as AnyRecord : {};
  const running = bool(downloadState.running);
  const validation = status?.validation && typeof status.validation === 'object' ? status.validation as AnyRecord : {};
  const validating = bool(validation.running);
  const installed = bool(status?.installed) && !validating;
  const mirrors = num(status?.mirrors);
  const done = num(downloadState.done);
  const total = num(downloadState.total);
  const percent = total > 0 ? Math.min(100, Math.round(done / total * 100)) : 0;
  const validationDone = num(validation.done);
  const validationTotal = num(validation.total, 3439938512);
  const validationPercent = validationTotal > 0 ? Math.min(100, Math.round(validationDone / validationTotal * 100)) : 0;
  const sizeGb = (num(status?.sizeBytes, 3439938512) / 1e9).toFixed(1);
  return <Frame title="SAM 3 Model Setup" busy={busy} onClose={() => desktop.setDialog(null)} dismissible={!running && !validating} actions={<><Button disabled={busy && !validating} onClick={() => desktop.setDialog(null)}>{running || validating ? 'Close' : 'Cancel'}</Button>{installed ? <Button primary onClick={() => desktop.setDialog(null)}>Continue</Button> : running ? <Button disabled={busy} onClick={() => void cancelDownload()}>Cancel Download</Button> : validating ? <Button onClick={() => void cancelValidation()}>Cancel Verification</Button> : <Button primary disabled={busy || !accepted || mirrors === 0} onClick={() => void download()}>Download</Button>}</>}>
    <Note>Object & Describe masks use SAM 3, Meta’s segmentation model. Everything else works without it.</Note>
    <p>A one-time download of about {sizeGb} GB is saved in:</p>
    <code className="lc-path-value">{text(status?.dir, 'No model folder configured')}</code>
    <div className="lc-dialog-toolbar"><Button disabled={busy || !status?.dir} onClick={() => void openFolder()}>Open Model Folder</Button><Button disabled={busy} onClick={() => void chooseFolder()}>Select Existing Model Folder…</Button></div>
    <p>Licence: SAM License (Meta). Downloading means accepting Meta’s terms.</p>
    <div className="lc-dialog-toolbar"><Button onClick={() => void desktop.native('openUrl', { url: licenseUrl })}>Read SAM License</Button><Button onClick={() => void desktop.native('openUrl', { url: 'https://github.com/Orthic-Labs/ember/blob/HEAD/docs/ai-masks.md' })}>SAM 3 Install Guide</Button></div>
    {!installed && !running && <Check checked={accepted} onChange={setAccepted}>I accept SAM License terms & want to download this model.</Check>}
    {mirrors === 0 && !installed && <Note tone="warning">No HTTPS download mirror is configured. Set LIGHTCRAFT_SAM3_MIRRORS or install files manually, then select their folder.</Note>}
    {running && <div className="lc-progress" aria-live="polite"><div className="lc-progress-label"><span>{text(downloadState.file, 'Downloading SAM 3…')}</span><span>{percent}%</span></div><progress max={100} value={percent} /></div>}
    {validating && <div className="lc-progress" aria-live="polite"><div className="lc-progress-label"><span>Verifying SAM 3 model folder…</span><span>{validationPercent}%</span></div><progress max={100} value={validationPercent} /></div>}
    {installed && <Note>SAM 3 files are installed. Object & Describe masks are ready.</Note>}
    {error && <Note tone="error">{error}</Note>}
    {text(downloadState.error) && <Note tone="error">{text(downloadState.error)}</Note>}
  </Frame>;
}

function UnsupportedDialog({ desktop }: { kind: string; desktop: DesktopContextValue }) { return <Frame title="Dialog unavailable" onClose={() => desktop.setDialog(null)} actions={<Button primary onClick={() => desktop.setDialog(null)}>Close</Button>}><Note tone="error">Sorry, this action isn’t available in this version.</Note></Frame>; }

function MissingDialog({ desktop, d }: { desktop: DesktopContextValue; d: DialogState }) { const [busy, setBusy] = useState(false); const [error, setError] = useState(''); const run = async () => { const paths = await choose(desktop.native, 'chooseFolder'); if (!paths[0]) return; setBusy(true); try { await desktop.run('library.findMissing', { folder: paths[0] }); desktop.setDialog(null); } catch (e) { setError(String(e)); } finally { setBusy(false); } }; return <Frame title="Find Missing Photos" busy={busy} onClose={() => desktop.setDialog(null)} actions={<><Button onClick={() => desktop.setDialog(null)}>Cancel</Button><Button primary disabled={busy} onClick={() => void run()}>Choose Search Folder…</Button></>}>{error && <Note tone="error">{error}</Note>}<Note>Ember checks names, sizes & content hashes, then relinks matches in one undoable step.</Note>{d.params?.count != null && <p>{num(d.params.count)} photos are missing.</p>}</Frame>; }
function LocalRootDialog({ desktop }: { desktop: DesktopContextValue }) { const [subfolders, setSubfolders] = useState(true); const chooseRoot = async () => { const paths = await choose(desktop.native, 'chooseFolder'); if (!paths[0]) return; try { await desktop.run('library.browse', { path: paths[0], subfolders }); desktop.setDialog(null); } catch (e) { desktop.setNotice(String(e)); } }; return <Frame title="Browse Local Folder" onClose={() => desktop.setDialog(null)} actions={<><Button onClick={() => desktop.setDialog(null)}>Cancel</Button><Button primary onClick={() => void chooseRoot()}>Choose Folder…</Button></>}><Check checked={subfolders} onChange={setSubfolders}>Include subfolders</Check><Note>Photos stay in place & remain outside catalog until you import them.</Note></Frame>; }
function LightroomResult({ d, desktop }: { d: DialogState; desktop: DesktopContextValue }) {
  const [result, setResult] = useState<AnyRecord>((d.params?.result as AnyRecord) || {});
  const [busy, setBusy] = useState(false); const [error, setError] = useState('');
  const catalog = text(d.params?.path); const missing = arr(result.missing); const failed = arr(result.failed); const warnings = arr(result.warnings); const unmapped = result.unmapped && typeof result.unmapped === 'object' ? Object.entries(result.unmapped as AnyRecord) : []; const indexWarning = displayValue(result.indexWarning); const indexError = displayValue(result.error);
  const run = async (operation: () => Promise<unknown>, close = false, updateResult = false) => { setBusy(true); setError(''); try { const value = await operation(); if (close) desktop.setDialog(null); else if (updateResult && value && typeof value === 'object') setResult(value as AnyRecord); } catch (reason) { setError(errorText(reason)); } finally { setBusy(false); } };
  const reimport = async () => { if (!catalog) { setError('Original catalog path is unavailable.'); return; } setBusy(true); setError(''); try { const task = await desktop.run('library.importLightroom', { path: catalog, updateExisting: bool(d.params?.updateExisting) }) as AnyRecord; const taskId = text(task.taskId); if (!taskId) throw new Error('LRCAT import did not return a task ID.'); desktop.setDialog({ kind: 'lightroomImportProgress', params: { taskId, path: catalog, updateExisting: bool(d.params?.updateExisting), report: result } }); } catch (reason) { setError(errorText(reason)); setBusy(false); } };
  const undo = () => run(() => desktop.run('edit.undo'), true);
  const reveal = () => { const archive = text(result.archive); return archive ? run(() => desktop.native('reveal', { path: archive })) : setError('No recovery archive was created for this library.'); };
  const imported = Array.isArray(result.imported) ? result.imported.length : num(result.imported); const photos = num(result.photos, imported); const archive = text(result.archive);
  return <Frame title="LRCAT Catalog Imported" busy={busy} onClose={() => desktop.setDialog(null)} actions={<Button primary disabled={busy} onClick={() => desktop.setDialog(null)}>Done</Button>}>
    <div className="lc-summary-grid"><div><strong>{photos}</strong><span>Photos in catalog</span></div><div><strong>{imported}</strong><span>Files imported</span></div><div><strong>{num(result.collections)}</strong><span>Collections</span></div><div><strong>{missing.length}</strong><span>Missing originals</span></div><div><strong>{num(result.preservedExistingEdits)}</strong><span>Edits preserved</span></div></div>
    {error && <Note tone="error">{error}</Note>}
    <Note>Source catalog stayed read-only. Ember saved import identity & recovery data for this catalog.</Note>
    {result.indexWarning != null && <Note tone="warning">Recovery index warning: {indexWarning}</Note>}
    {result.error != null && <Note tone="warning">Recovery index error: {indexError}</Note>}
    {archive ? <div className="lc-dialog-toolbar"><span className="lc-path">Recovery archive: {archive}</span><Button disabled={busy} onClick={() => void reveal()}>Show Recovery Archive</Button></div> : <Note>No recovery archive was created for this library.</Note>}
    {missing.length > 0 && <details className="lc-details" open><summary>Missing originals ({missing.length})</summary><ul>{missing.map((path, i) => <li key={i}>{displayValue(path)}</li>)}</ul></details>}
    {warnings.length > 0 && <details className="lc-details"><summary>Warnings ({warnings.length})</summary><ul>{warnings.map((warning, i) => <li key={i}>{displayValue(warning)}</li>)}</ul></details>}
    {failed.length > 0 && <details className="lc-details"><summary>Files not imported ({failed.length})</summary><ul>{failed.map((failure, i) => <li key={i}>{displayValue(failure)}</li>)}</ul></details>}
    {unmapped.length > 0 && <details className="lc-details"><summary>Settings not mapped ({unmapped.length})</summary><ul>{unmapped.map(([id, values]) => <li key={id}>Photo {id}: {displayValue(values)}</li>)}</ul></details>}
    <div className="lc-dialog-toolbar"><Button disabled={busy || !catalog} onClick={() => void reimport()}>Reimport Same Catalog</Button><Button disabled={busy} onClick={() => void undo()}>Undo Import</Button></div>
  </Frame>;
}

export default function DialogHost({ desktop: provided }: HostProps = {}) {
  useFocusHistory();
  const hookContext = useDesktop(); const context = provided || hookContext; const { dialog } = context; if (!dialog) return null; const kind = dialog.kind.toLowerCase();
  if (kind === 'unsavedquit') return <UnsavedQuitDialog d={dialog} desktop={context} />;
  if (kind === 'confirmdelete' || kind === 'confirm-delete') return <ConfirmDeleteDialog d={dialog} desktop={context} />;
  if (kind === 'import' || kind === 'importphotos' || kind === 'importfolder' || kind === 'importdevice') return <ImportDialog d={dialog} desktop={context} />;
  if (kind === 'lightroom' || kind === 'lightroomimport' || kind === 'importlightroom') return <LightroomDialog d={dialog} desktop={context} />;
  if (kind === 'export') return <ExportDialog d={dialog} desktop={context} />;
  if (kind === 'cull') return <CullReviewDialog desktop={context} photoIds={arr(dialog.params?.ids).length ? arr(dialog.params?.ids).filter((id): id is number => Number.isSafeInteger(id)) : context.snapshot?.selection} onClose={() => context.setDialog(null)} />;
  if (kind === 'importprogress' || kind === 'exportprogress' || kind.endsWith('progress')) return <JobDialog kind={kind} d={dialog} desktop={context} />;
  if (kind === 'merge.hdr' || kind === 'merge.panorama' || kind === 'merge.hdrpanorama') return <MergeDialog kind={dialog.kind} d={dialog} desktop={context} />;
  if (kind === 'newalbum') return <AlbumDialog folder={false} d={dialog} desktop={context} />;
  if (kind === 'newfolder') return <AlbumDialog folder d={dialog} desktop={context} />;
  if (kind === 'smartalbum' || kind === 'newsmartalbum') return <SmartAlbumDialog d={dialog} desktop={context} />;
  if (kind === 'smartrules') return <SmartAlbumDialog kind="smartRules" d={dialog} desktop={context} />;
  if (kind === 'missing' || kind === 'findmissing' || kind === 'locatemissing') return <MissingDialog d={dialog} desktop={context} />;
  if (kind === 'localroot' || kind === 'localroots' || kind === 'browsefolder') return <LocalRootDialog desktop={context} />;
  if (kind === 'lightroomresult') return <LightroomResult d={dialog} desktop={context} />;
  if (kind === 'allmetadata') return <MetadataDialog d={dialog} desktop={context} />;
  if (kind === 'about' || kind === 'systeminfo' || kind === 'shortcuts' || kind === 'whatsnew') return <InfoDialog kind={kind} d={dialog} desktop={context} />;
  if (kind === 'settings') return <SettingsDialog d={dialog} desktop={context} />;
  if (kind === 'sammodel' || kind === 'sammodelsetup' || kind === 'samsetup') return <SamModelSetupDialogHost desktop={context} />;
  if (kind === 'copysettings') return <GroupsDialog paste={false} d={dialog} desktop={context} />;
  if (kind === 'pastsettings' || kind === 'pastesettings') return <GroupsDialog paste d={dialog} desktop={context} />;
  if (kind === 'presetimport') return <PresetFileDialog exportMode={false} curve={false} d={dialog} desktop={context} />;
  if (kind === 'presetexport') return <PresetFileDialog exportMode curve={false} d={dialog} desktop={context} />;
  if (kind === 'curveimportpresets') return <PresetFileDialog exportMode={false} curve d={dialog} desktop={context} />;
  if (kind === 'curveexportpresets') return <PresetFileDialog exportMode curve d={dialog} desktop={context} />;
  if (EXPLICIT_FORM_KINDS.has(kind)) return <FormDialog kind={kind} d={dialog} desktop={context} />;
  return <UnsupportedDialog kind={dialog.kind} desktop={context} />;
}
