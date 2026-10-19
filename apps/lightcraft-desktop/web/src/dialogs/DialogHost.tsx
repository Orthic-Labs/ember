import { APP_NAME, APP_ICON_URL } from '../branding';
import React, { useEffect, useRef, useState } from 'react';
import { createPortal } from 'react-dom';
import { Button, Checkbox, Field, IconButton, Input, ModalShell, Notice, ProgressBar, Select } from '@rightkit/react-shell/react';
import { useInertSiblings } from './inertSiblings';
import { acknowledgePreview, cancelMergePreview, convertFileSrc, requestMergePreview } from '../api';
import { useDesktop } from '../desktop';
import { ImportCandidatePreview } from '../preview/ImportCandidatePreview';
import { CullReviewDialog } from './CullReviewDialog';
import { EmberSettings } from '../settings/EmberSettings';
import { openLegalDocument } from '../legal';
import type { DesktopContextValue, DialogState, JsonObject, MergePreviewDescriptor } from '../types';
import './dialogs.css';

type AnyRecord = Record<string, any>;
type HostProps = { desktop?: DesktopContextValue };
let mergePreviewSequence = 0;

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

type JobTerminal = 'done' | 'failed' | 'cancelled';

/**
 * The one modal surface for every dialog here. ModalShell supplies the backdrop, Escape, the Tab trap and focus restored
 * to the opener. `#lc-dialog-title` and the `.lc-dialog*` hooks stay for the native QA selectors.
 */
function Frame({ title, children, actions, onClose, wide = false, busy = false, dismissible = true }: { title: string; children: React.ReactNode; actions?: React.ReactNode; onClose: () => void; wide?: boolean; busy?: boolean; dismissible?: boolean }) {
  const content = useRef<HTMLDivElement>(null);
  useInertSiblings(content);
  const frame = <ModalShell label={title} labelledBy="lc-dialog-title" width={wide ? 860 : 560} onClose={onClose} dismissible={dismissible} overlayClassName="lc-dialog-backdrop" className="lc-dialog">
    <div className="lc-dialog-header"><h2 id="lc-dialog-title" className="rk-modal__title">{title}</h2>{dismissible && <IconButton label="Close" onClick={onClose}>×</IconButton>}</div>
    <div ref={content} className="lc-dialog-body">{children}</div>
    {actions && <div className="rk-modal__actions lc-dialog-actions">{busy && <span className="lc-dialog-busy" aria-live="polite">Working…</span>}{actions}</div>}
  </ModalShell>;
  return typeof document === 'undefined' ? frame : createPortal(frame, document.body);
}

/** Labelled textarea, kept local on purpose (exception B5): the published field set has Input only. */
function TextArea({ label, value, onChange, placeholder }: { label: string; value: string; onChange: (v: string) => void; placeholder?: string }) { return <label className="lc-field"><span>{label}</span><textarea rows={3} value={value} onChange={(e) => onChange(e.target.value)} placeholder={placeholder} style={{ width: '100%', boxSizing: 'border-box', minHeight: 72, padding: '6px 9px', border: '1px solid var(--rk-line,#c8d0da)', borderRadius: 7, background: 'var(--rk-elevated,#fff)', color: 'inherit', font: '13px Inter,system-ui,sans-serif', resize: 'vertical' }} /></label>; }

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
  const progressTotal = Math.max(1, num(job?.total, 1));
  const progressCompleted = num(job?.completed);
  const imported = completion ? (Array.isArray(completion.imported) ? completion.imported.length : num(completion.imported)) : 0;
  const title = importReview ? 'Reviewing Photos' : lightroomInspect ? 'Inspecting LRCAT Catalog' : lightroomImport ? 'Importing LRCAT Catalog' : kind.includes('import') ? 'Importing Photos' : kind.includes('merge') ? 'Merging Photos' : 'Exporting Photos';
  return <Frame title={title} dismissible={terminal !== null} onClose={() => terminal && desktop.setDialog(null)} actions={<>{terminal === 'failed' && (lightroomInspect || lightroomImport) && <Button className="lc-button lc-button-primary" variant="primary" onClick={retryLightroom}>Retry</Button>}{importReview && terminal && terminal !== 'done' && <Button className="lc-button lc-button-primary" variant="primary" onClick={retryImportReview}>Retry review</Button>}<Button className="lc-button" disabled={!terminal} onClick={() => desktop.setDialog(null)}>Close</Button>{kind.includes('export') && successful && <Button className="lc-button lc-button-primary" variant="primary" onClick={() => void reveal()}>Show in Folder</Button>}</>}><div className="lc-progress" aria-live="polite"><div className="lc-progress-label"><span>{text(job?.label, label)}</span><span>{Math.min(100, Math.round(progressCompleted / progressTotal * 100))}%</span></div><ProgressBar value={progressCompleted} max={progressTotal} label={text(job?.label, label)} />{!terminal && job && job.cancellable !== false && <Button className="lc-button" disabled={cancelling} onClick={() => void cancel()}>{cancelling ? 'Cancelling…' : 'Cancel'}</Button>}</div>{pollError && <Notice tone="bad">{pollError}</Notice>}{terminal === 'done' && !importReview && <Notice tone="info">Operation complete. Changes are recorded in library history.{imported > 0 ? ` ${imported} photos imported.` : ''}</Notice>}{terminal === 'cancelled' && <Notice tone="warn">{importReview ? 'Review cancelled. Chosen paths remain available for retry.' : 'Operation cancelled.'}</Notice>}{terminal === 'failed' && <Notice tone="bad">{text(job?.error, 'Operation failed.')}{importReview && ' Chosen paths remain available for retry.'}</Notice>}</Frame>;
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
  return <Frame title="Quit with unsaved changes?" busy={busy} onClose={keepOpen} actions={<><Button className="lc-button" disabled={busy} onClick={keepOpen}>Keep Open</Button><Button className="lc-button" disabled={busy} onClick={() => void quitAnyway()}>Quit Anyway</Button><Button className="lc-button lc-button-primary" variant="primary" disabled={busy} onClick={() => void saveAndQuit()}>Retry Save &amp; Quit</Button></>}><Notice tone="warn">Some library changes could not be saved. Try saving again before closing.</Notice>{typeof d.params?.error === 'string' && !error && <Notice tone="info">{d.params.error}</Notice>}{error && <Notice tone="bad">{error}</Notice>}</Frame>;
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
  return <Frame title={permanent ? 'Delete Photos Permanently?' : 'Move Photos to Recently Deleted?'} busy={busy} onClose={() => desktop.setDialog(null)} actions={<><Button className="lc-button" disabled={busy} onClick={() => desktop.setDialog(null)}>Cancel</Button><Button className="lc-button lc-button-primary" variant="primary" disabled={busy} onClick={() => void confirm()}>{permanent ? 'Delete Permanently' : 'Delete Photos'}</Button></>}><Notice tone="warn">{permanent ? 'These photos cannot be restored.' : 'Photos can be restored from Recently Deleted.'}</Notice>{error && <Notice tone="bad">{error}</Notice>}</Frame>;
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
  return <Frame title="Import Photos" wide busy={busy} onClose={close} actions={<><Button className="lc-button" disabled={busy} onClick={close}>Cancel</Button><Button className="lc-button lc-button-primary" variant="primary" disabled={busy || !candidates.length} onClick={() => void submit()}>Import {candidates.filter((_, i) => checked[i]).length || ''} Photos</Button></>}><div className="lc-dialog-toolbar"><Button className="lc-button" disabled={busy} onClick={() => void pick('files')}>Choose Photos…</Button><Button className="lc-button" disabled={busy} onClick={() => void pick('folder')}>Choose Folder…</Button><Button className="lc-button" disabled={busy} onClick={() => void pick('device')}>Camera or Card…</Button><Button className="lc-button" disabled={busy || !scanPaths.length} onClick={rescan}>Rescan</Button></div>{source && <Notice tone="info">Source: {sourceLabel} — {source}</Notice>}{phase === 'scanning' && <Notice tone="info">Reviewing photos…</Notice>}{error && <Notice tone="bad">{error}</Notice>}<div className="lc-import-layout"><div className="lc-candidate-list"><div className="lc-section-heading"><strong>Review</strong><span>{candidates.length} found</span></div>{candidates.length ? candidates.map((c, i) => { const pairing = c.pairing && typeof c.pairing === 'object' ? c.pairing as AnyRecord : null; const pairingNotice = c.policyExcluded === 'rawOnly' ? ` · RAW only default (paired with ${text(pairing?.with, 'JPEG')})` : pairing ? ` · Paired ${text(pairing.kind, 'variant')}: ${text(pairing.with)}` : ''; return <Checkbox className={`lc-candidate ${c.duplicate || c.error ? 'lc-candidate-muted' : ''}`} key={`${c.path || c.name || i}-${i}`} checked={checked[i] ?? false} disabled={busy || Boolean(c.duplicate || c.error)} onChange={(enabled) => setChecked((old) => old.map((v, n) => n === i ? enabled : v))} label={<><ImportCandidatePreview preview={c.preview} aria-hidden="true" /><span className="lc-candidate-copy"><strong>{text(c.name, text(c.path, 'Photo'))}</strong><small>{text(c.path)}{c.duplicate ? ` · Duplicate (${c.duplicate})` : c.error ? ` · ${c.error}` : ''}{pairingNotice}</small></span></>} />; }) : <Notice tone="info">Choose photos or a folder to review files before adding them.</Notice>}</div><div className="lc-form-stack"><Checkbox className="lc-check" checked={includeSubfolders} onChange={setIncludeSubfolders} disabled={busy} label={<>Include subfolders</>} /><div className="lc-field"><Field label={<span>{"Only extensions"}</span>}>{({ id }) => <Input id={id} value={allowedExtensions} placeholder="raw, dng (optional)" disabled={busy} onChange={(event) => (setAllowedExtensions)(event.target.value)} />}</Field></div><div className="lc-field"><Field label={<span>{"Exclude extensions"}</span>}>{({ id }) => <Input id={id} value={excludedExtensions} placeholder="jpg, jpeg (optional)" disabled={busy} onChange={(event) => (setExcludedExtensions)(event.target.value)} />}</Field></div><div className="lc-field"><Field label={<span>{"RAW/JPEG pairs"}</span>}>{() => <Select label="RAW/JPEG pairs" value={rawJpegPolicy} options={([['keepBoth', 'Keep both (default)'], ['rawOnly', 'RAW only']]).map(([value, label]) => ({ value, label }))} onChange={(value) => setRawJpegPolicy(value === 'rawOnly' ? 'rawOnly' : 'keepBoth')} disabled={busy} />}</Field></div><div className="lc-field"><Field label={<span>{"Add photos"}</span>}>{() => <Select label="Add photos" value={mode} options={([["add", 'In place'], ['copy', 'Copy into library'], ['move', 'Move into library']]).map(([value, label]) => ({ value, label }))} onChange={setMode} disabled={busy} />}</Field></div><div className="lc-field"><Field label={<span>{"Existing album ID"}</span>}>{({ id }) => <Input id={id} value={album} placeholder="Optional" disabled={busy} onChange={(event) => (setAlbum)(event.target.value)} />}</Field></div><div className="lc-field"><Field label={<span>{"New album"}</span>}>{({ id }) => <Input id={id} value={newAlbum} placeholder="Optional" disabled={busy} onChange={(event) => (setNewAlbum)(event.target.value)} />}</Field></div><div className="lc-field"><Field label={<span>{"Keywords"}</span>}>{({ id }) => <Input id={id} value={keywords} placeholder="Comma-separated" disabled={busy} onChange={(event) => (setKeywords)(event.target.value)} />}</Field></div>{mode !== 'add' && <><div className="lc-field"><Field label={<span>{"Destination folder"}</span>}>{({ id }) => <Input id={id} value={destination} placeholder="Library Originals by default" disabled={busy} onChange={(event) => (setDestination)(event.target.value)} />}</Field></div><div className="lc-field"><Field label={<span>{"Organize copies"}</span>}>{() => <Select label="Organize copies" value={organize} options={([["date", 'By capture date'], ['month', 'By month'], ['flat', 'One folder'], ['custom', 'Custom template']]).map(([value, label]) => ({ value, label }))} onChange={setOrganize} disabled={busy} />}</Field></div>{organize === 'custom' && <><div className="lc-field"><Field label={<span>{"Folder template"}</span>}>{({ id }) => <Input id={id} value={folderTemplate} placeholder="{date:%Y}/{date:%Y%m%d}" disabled={busy} onChange={(event) => (setFolderTemplate)(event.target.value)} />}</Field></div><Notice tone="info">Use relative folders & photo tags such as {`{date:%Y}`}; no drive or parent folders.</Notice></>}<div className="lc-field"><Field label={<span>{"File naming"}</span>}>{({ id }) => <Input id={id} value={rename} placeholder="Keep original names" disabled={busy} onChange={(event) => (setRename)(event.target.value)} />}</Field></div><Checkbox className="lc-check" checked={dng} onChange={setDng} disabled={busy || mode !== 'copy'} label={<>Copy raw files as DNG</>} /></>}<div className="lc-field"><Field label={<span>{"Develop preset ID"}</span>}>{({ id }) => <Input id={id} value={preset} placeholder="Optional" disabled={busy} onChange={(event) => (setPreset)(event.target.value)} />}</Field></div><div className="lc-field"><Field label={<span>{"Metadata preset"}</span>}>{({ id }) => <Input id={id} value={metadataPreset} placeholder="Optional" disabled={busy} onChange={(event) => (setMetadataPreset)(event.target.value)} />}</Field></div></div></div></Frame>;
}

function folderTemplateError(template: string): string | null { const value = template.trim(); if (!value) return 'Enter a folder template.'; if (!value.includes('{') && !value.includes('/') && !value.includes('\\')) return 'Folder template must contain {, / or \\.'; if (value.startsWith('/') || value.startsWith('\\') || value.startsWith('~') || /^[A-Za-z]:/.test(value)) return 'Folder template must stay inside destination folder.'; if (value.split(/[\\/]/).some((part) => part.trim() === '.' || part.trim() === '..')) return 'Folder template cannot contain . or .. folders.'; return null; }

function LightroomDialog({ d, desktop }: { d: DialogState; desktop: DesktopContextValue }) {
  const [path, setCatalogPath] = useState(text(d.params?.path)); const [report, setReport] = useState<AnyRecord | null>((d.params?.report as AnyRecord) || null); const [updateExisting, setUpdateExisting] = useState(bool(d.params?.updateExisting)); const [busy, setBusy] = useState(false); const [error, setError] = useState('');
  const setPath = (value: string) => { setCatalogPath(value); setReport(null); setError(''); };
  const inspect = async (chosen?: string) => { const catalog = chosen || path; if (!catalog) return; setPath(catalog); setReport(null); setBusy(true); setError(''); try { const task = await desktop.run('library.inspectLightroom', { path: catalog }) as AnyRecord; const taskId = text(task.taskId); if (!taskId) throw new Error('LRCAT inspection did not return a task ID.'); desktop.setDialog({ kind: 'lightroomInspectProgress', params: { taskId, path: catalog } }); } catch (e) { setError(String(e)); setBusy(false); } };
  const importCatalog = async () => { if (!path) return; setBusy(true); setError(''); try { const task = await desktop.run('library.importLightroom', { path, updateExisting }) as AnyRecord; const taskId = text(task.taskId); if (!taskId) throw new Error('LRCAT import did not return a task ID.'); desktop.setDialog({ kind: 'lightroomImportProgress', params: { taskId, path, updateExisting, report: report || undefined } }); } catch (e) { setError(String(e)); setBusy(false); } };
  return <Frame title="Import LRCAT Catalog" wide busy={busy} onClose={() => desktop.setDialog(null)} actions={<><Button className="lc-button" onClick={() => desktop.setDialog(null)}>Cancel</Button><Button className="lc-button lc-button-primary" variant="primary" disabled={!report || busy} onClick={() => void importCatalog()}>Import Catalog</Button></>}><div className="lc-dialog-toolbar"><Button className="lc-button" onClick={async () => { const paths = await choose(desktop.native, 'pickLightroomCatalog'); if (paths[0]) void inspect(paths[0]); }}>Choose .lrcat…</Button></div><div className="lc-field"><Field label={<span>{"Catalog"}</span>}>{({ id }) => <Input id={id} value={path} placeholder="Select a LRCAT .lrcat catalog" onChange={(event) => (setPath)(event.target.value)} />}</Field></div>{error && <Notice tone="bad">{error}</Notice>}{report ? <><div className="lc-summary-grid"><div><strong>{num(report.photos)}</strong><span>Photos</span></div><div><strong>{num(report.collections)}</strong><span>Collections</span></div><div><strong>{arr(report.missing).length}</strong><span>Missing originals</span></div><div><strong>{arr(report.warnings).length}</strong><span>Warnings</span></div></div><Checkbox className="lc-check" checked={updateExisting} onChange={setUpdateExisting} label={<>Update existing edits when source catalog has changes</>} />{arr(report.missing).length > 0 && <Notice tone="warn">Missing originals stay in catalog for later relinking.</Notice>}{arr(report.warnings).length > 0 && <details className="lc-details"><summary>Review warnings</summary><ul>{arr(report.warnings).map((warning, i) => <li key={i}>{displayValue(warning)}</li>)}</ul></details>}</> : <Notice tone="info">Ember reads catalog data without changing source catalog. Inspect first to review missing originals & warnings.</Notice>}</Frame>;
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
    <Frame title="Export" wide busy={busy} onClose={() => desktop.setDialog(null)} actions={<><Button className="lc-button" onClick={() => desktop.setDialog(null)}>Cancel</Button><Button className="lc-button lc-button-primary" variant="primary" disabled={busy} onClick={() => void submit()}>Export</Button></>}>
      <div className="lc-dialog-toolbar">
        <Button className="lc-button" onClick={async () => { const paths = await choose(desktop.native, 'chooseFolder'); if (paths[0]) setDir(paths[0]); }}>Choose Export Folder…</Button>
        <div className="lc-field"><Field label={<span>{"Export folder"}</span>}>{({ id }) => <Input id={id} value={dir} placeholder="Choose or enter an export folder" onChange={(event) => (setDir)(event.target.value)} />}</Field></div>
      </div>
      {error && <Notice tone="bad">{error}</Notice>}
      <div className="lc-export-columns">
        <div className="lc-form-stack">
          <div className="lc-field"><Field label={<span>{"Format"}</span>}>{() => <Select label="Format" value={format} options={([["jpeg", 'JPEG'], ['png', 'PNG'], ['tiff', 'TIFF'], ['webp', 'WebP'], ['avif', 'AVIF'], ['original', 'Original + sidecar'], ['dng', 'DNG']]).map(([value, label]) => ({ value, label }))} onChange={(v) => set('format', v)} />}</Field></div>
          {rendered && <>
            <div className="lc-field"><Field label={<span>{"Color space"}</span>}>{() => <Select label="Color space" value={text(opts.colorSpace, 'srgb')} options={([["srgb", 'sRGB'], ['displayP3', 'Display P3'], ['adobeRgb', 'Adobe RGB'], ['proPhoto', 'ProPhoto RGB'], ['rec2020', 'Rec. 2020']]).map(([value, label]) => ({ value, label }))} onChange={(v) => set('colorSpace', v)} />}</Field></div>
            <div className="lc-field"><Field label={<span>{"Bit depth"}</span>}>{() => <Select label="Bit depth" value={String(opts.bitDepth ?? 8)} options={([["8", '8 bit'], ['16', '16 bit'], ['32', '32 bit']]).map(([value, label]) => ({ value, label }))} onChange={(v) => set('bitDepth', Number(v))} />}</Field></div>
            <div className="lc-field"><Field label={<span>{"Quality"}</span>}>{({ id }) => <Input id={id} type="number" value={num(opts.quality, 90)} min={1} max={100} onChange={(event) => ((v) => set('quality', Number(v)))(event.target.value)} />}</Field></div>
            <div className="lc-field"><Field label={<span>{"Sharpen"}</span>}>{() => <Select label="Sharpen" value={text(opts.sharpen, 'none')} options={([["none", 'None'], ['screen', 'Screen'], ['matte', 'Matte'], ['glossy', 'Glossy']]).map(([value, label]) => ({ value, label }))} onChange={(v) => set('sharpen', v)} />}</Field></div>
            {text(opts.sharpen, 'none') !== 'none' && <div className="lc-field"><Field label={<span>{"Sharpen amount"}</span>}>{() => <Select label="Sharpen amount" value={text(opts.sharpenAmount, 'standard')} options={([["low", 'Low'], ['standard', 'Standard'], ['high', 'High']]).map(([value, label]) => ({ value, label }))} onChange={(v) => set('sharpenAmount', v)} />}</Field></div>}
          </>}
          <div className="lc-field"><Field label={<span>{"Naming template"}</span>}>{({ id }) => <Input id={id} value={text(opts.rename, text(opts.naming, '{name}'))} placeholder="{name}" onChange={(event) => ((v) => set('rename', v))(event.target.value)} />}</Field></div>
          <div className="lc-field"><Field label={<span>{"Start number"}</span>}>{({ id }) => <Input id={id} type="number" value={num(opts.startNumber, 1)} min={1} onChange={(event) => ((v) => set('startNumber', Number(v)))(event.target.value)} />}</Field></div>
          <div className="lc-field"><Field label={<span>{"If file exists"}</span>}>{() => <Select label="If file exists" value={text(opts.conflict, 'unique')} options={([["unique", 'Add a number'], ['overwrite', 'Overwrite'], ['skip', 'Skip']]).map(([value, label]) => ({ value, label }))} onChange={(v) => set('conflict', v)} />}</Field></div>
          {format === 'tiff' && <div className="lc-field"><Field label={<span>{"TIFF compression"}</span>}>{() => <Select label="TIFF compression" value={text(opts.tiffCompression, 'deflate')} options={([["none", 'None'], ['lzw', 'LZW'], ['deflate', 'ZIP']]).map(([value, label]) => ({ value, label }))} onChange={(v) => set('tiffCompression', v)} />}</Field></div>}
          {format === 'dng' && <div className="lc-field"><Field label={<span>{"DNG compression"}</span>}>{() => <Select label="DNG compression" value={text(opts.dngCompression, 'lossless')} options={([["lossless", 'Lossless'], ['deflate', 'ZIP'], ['uncompressed', 'None']]).map(([value, label]) => ({ value, label }))} onChange={(v) => set('dngCompression', v)} />}</Field></div>}
        </div>
        <div className="lc-form-stack">
          <Checkbox className="lc-check" checked={fullSize} onChange={setFullSize} label={<>Full size</>} />
          {!fullSize && <>
            <div className="lc-field"><Field label={<span>{"Resize"}</span>}>{() => <Select label="Resize" value={text(opts.resizeMode, 'longEdge')} options={([["longEdge", 'Long edge'], ['shortEdge', 'Short edge'], ['width', 'Width'], ['height', 'Height'], ['dimensions', 'Width × height'], ['megapixels', 'Megapixels'], ['percent', 'Percentage']]).map(([value, label]) => ({ value, label }))} onChange={(v) => set('resizeMode', v)} />}</Field></div>
            <div className="lc-field"><Field label={<span>{"Size"}</span>}>{({ id }) => <Input id={id} type="number" value={num(opts.resizeValue, 2048)} min={16} onChange={(event) => ((v) => set('resizeValue', Number(v)))(event.target.value)} />}</Field></div>
            {text(opts.resizeMode) === 'dimensions' && <div className="lc-field"><Field label={<span>{"Height"}</span>}>{({ id }) => <Input id={id} type="number" value={num(opts.resizeHeight, 2048)} min={16} onChange={(event) => ((v) => set('resizeHeight', Number(v)))(event.target.value)} />}</Field></div>}
            <Checkbox className="lc-check" checked={bool(opts.dontEnlarge, true)} onChange={(v) => set('dontEnlarge', v)} label={<>Don't enlarge</>} />
          </>}
          <div className="lc-field"><Field label={<span>{"Resolution (ppi)"}</span>}>{({ id }) => <Input id={id} type="number" value={num(opts.ppi, 240)} min={1} onChange={(event) => ((v) => set('ppi', Number(v)))(event.target.value)} />}</Field></div>
          {rendered && <>
            <Checkbox className="lc-check" checked={watermarkOn} onChange={(enabled) => { setWatermarkOn(enabled); if (enabled && !text(watermark.text).trim() && !text(watermark.image).trim()) setWm('text', '© '); }} label={<>Watermark</>} />
            {watermarkOn && <>
              <div className="lc-field"><Field label={<span>{"Watermark style"}</span>}>{() => <Select label="Watermark style" value={watermarkStyle} options={([["text", 'Text'], ['graphic', 'Graphic']]).map(([value, label]) => ({ value, label }))} onChange={(v) => { setWatermarkStyle(v); if (v === 'text') setWm('image', ''); }} />}</Field></div>
              {watermarkStyle === 'graphic' ? <>
                <div className="lc-dialog-toolbar"><Button className="lc-button" onClick={async () => { const paths = await choose(desktop.native, 'chooseFiles', { multiple: false }); if (paths[0]) setWm('image', paths[0]); }}>Choose Watermark Image…</Button></div>
                <div className="lc-field"><Field label={<span>{"Graphic path"}</span>}>{({ id }) => <Input id={id} value={text(watermark.image)} placeholder="logo.png" onChange={(event) => ((v) => setWm('image', v))(event.target.value)} />}</Field></div>
                <div className="lc-field"><Field label={<span>{"Image width (%)"}</span>}>{({ id }) => <Input id={id} type="number" value={num(watermark.imageWidth, 0.2) * 100} min={2} max={100} step={1} onChange={(event) => ((v) => setWm('imageWidth', exportPercent(v, 0.2, 2, 100)))(event.target.value)} />}</Field></div>
              </> : <>
                <TextArea label="Watermark text" value={text(watermark.text)} onChange={(v) => setWm('text', v)} placeholder="© Your Name" />
                <Checkbox className="lc-check" checked={bool(watermark.vertical)} onChange={(v) => setWm('vertical', v)} label={<>Vertical text</>} />
                <div className="lc-field"><Field label={<span>{"Size (%)"}</span>}>{({ id }) => <Input id={id} type="number" value={num(watermark.size, 0.035) * 100} min={1} max={15} step={0.5} onChange={(event) => ((v) => setWm('size', exportPercent(v, 0.035, 1, 15)))(event.target.value)} />}</Field></div>
                <div className="lc-field"><Field label={<span>{"Colour"}</span>}>{({ id }) => <Input id={id} type="color" value={watermarkHex(watermark.color)} onChange={(event) => ((v) => setWm('color', watermarkColor(v)))(event.target.value)} />}</Field></div>
                <Checkbox className="lc-check" checked={bool(watermark.shadow, true)} onChange={(v) => setWm('shadow', v)} label={<>Shadow</>} />
              </>}
              <div className="lc-field"><Field label={<span>{"Watermark position"}</span>}>{() => <Select label="Watermark position" value={text(watermark.anchor, 'bottomRight')} options={([["topLeft", 'Top left'], ['topRight', 'Top right'], ['center', 'Center'], ['bottomLeft', 'Bottom left'], ['bottomRight', 'Bottom right']]).map(([value, label]) => ({ value, label }))} onChange={(v) => setWm('anchor', v)} />}</Field></div>
              <div className="lc-field"><Field label={<span>{"Opacity (%)"}</span>}>{({ id }) => <Input id={id} type="number" value={num(watermark.opacity, 0.7) * 100} min={5} max={100} step={1} onChange={(event) => ((v) => setWm('opacity', exportPercent(v, 0.7, 5, 100)))(event.target.value)} />}</Field></div>
            </>}
          </>}
          <div className="lc-field"><Field label={<span>{"Metadata"}</span>}>{() => <Select label="Metadata" value={text(opts.metadata, 'all')} options={([["all", 'All metadata'], ['allExceptCamera', 'All except camera'], ['copyright', 'Copyright only'], ['none', 'None']]).map(([value, label]) => ({ value, label }))} onChange={(v) => set('metadata', v)} />}</Field></div>
          <Checkbox className="lc-check" checked={bool(opts.removeLocation)} onChange={(v) => set('removeLocation', v)} label={<>Remove location</>} />
          <div className="lc-field"><Field label={<span>{"Subfolder"}</span>}>{({ id }) => <Input id={id} value={text(opts.subfolder)} placeholder="Optional" onChange={(event) => ((v) => set('subfolder', v))(event.target.value)} />}</Field></div>
          <div className="lc-field"><Field label={<span>{"Limit file size (KB)"}</span>}>{({ id }) => <Input id={id} type="number" value={num(opts.limitKb, 0)} min={0} onChange={(event) => ((v) => set('limitKb', Number(v)))(event.target.value)} />}</Field></div>
          <div className="lc-field"><Field label={<span>{"Export preset"}</span>}>{({ id }) => <Input id={id} value={preset} placeholder="Optional preset name" onChange={(event) => (setPreset)(event.target.value)} />}</Field></div>
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
  return <Frame title={kind === 'merge.panorama' ? 'Panorama Merge Preview' : kind === 'merge.hdrPanorama' ? 'HDR Panorama Merge Preview' : 'HDR Merge Preview'} wide busy={mergeBusy} onClose={() => { cancelPreview(); desktop.setDialog(null); }} actions={<><Button className="lc-button" onClick={() => { cancelPreview(); desktop.setDialog(null); }}>Cancel</Button><Button className="lc-button lc-button-primary" variant="primary" disabled={mergeBusy} onClick={() => void merge()}>Merge</Button></>}><div className="lc-merge-layout"><div className="lc-merge-preview" aria-label="Merge preview">{preview ? <img src={preview.url} alt="Merge preview" /> : <span>{previewError || (previewBusy ? 'Building preview…' : 'Select photos to preview.')}</span>}{previewBusy && preview && <span className="lc-merge-preview-status" aria-live="polite">Updating preview…</span>}{previewError && preview && <span className="lc-merge-preview-status" role="alert">{previewError}</span>}{preview && <small className="lc-merge-preview-info">{summary} · {preview.descriptor.width} × {preview.descriptor.height}</small>}</div><div className="lc-form-stack">{pano && <><div className="lc-field"><Field label={<span>{"Projection"}</span>}>{() => <Select label="Projection" value={text(options.projection, 'auto')} options={([["auto", 'Auto Select'], ['spherical', 'Spherical'], ['cylindrical', 'Cylindrical'], ['perspective', 'Perspective']]).map(([value, label]) => ({ value, label }))} onChange={(v) => set('projection', v)} />}</Field></div><div className="lc-field"><Field label={<span>{"Boundary warp"}</span>}>{({ id }) => <Input id={id} type="number" value={num(options.boundaryWarp, 0)} min={0} max={100} onChange={(event) => ((v) => set('boundaryWarp', Number(v)))(event.target.value)} />}</Field></div><Checkbox className="lc-check" checked={bool(options.fillEdges)} onChange={(v) => set('fillEdges', v)} label={<>Fill edges</>} /><Checkbox className="lc-check" checked={bool(options.autoCrop, true)} onChange={(v) => set('autoCrop', v)} label={<>Auto crop</>} /></>}{hdr && <><Checkbox className="lc-check" checked={bool(options.align, true)} onChange={(v) => set('align', v)} label={<>Auto align</>} /><div className="lc-field"><Field label={<span>{"Deghost amount"}</span>}>{() => <Select label="Deghost amount" value={text(options.deghost, 'none')} options={([["none", 'None'], ['low', 'Low'], ['medium', 'Medium'], ['high', 'High']]).map(([value, label]) => ({ value, label }))} onChange={(v) => set('deghost', v)} />}</Field></div>{kind === 'merge.hdr' && <Checkbox className="lc-check" checked={bool(options.showOverlay)} disabled={text(options.deghost, 'none') === 'none'} onChange={(v) => set('showOverlay', v)} label={<>Show deghost overlay</>} />}{kind === 'merge.hdrPanorama' && <div className="lc-field"><Field label={<span>{"Photos per bracket (0 = auto)"}</span>}>{({ id }) => <Input id={id} type="number" value={num(options.bracket, 0)} min={0} max={9} step={1} onChange={(event) => ((v) => set('bracket', Math.max(0, Math.min(9, Math.round(Number(v) || 0)))))(event.target.value)} />}</Field></div>}</>}{error && <Notice tone="bad">{error}</Notice>}{previewError && !preview && <Notice tone="bad">{previewError}</Notice>}<Checkbox className="lc-check" checked={bool(options.autoSettings, true)} onChange={(v) => set('autoSettings', v)} label={<>Auto settings</>} /><Checkbox className="lc-check" checked={bool(options.stack)} onChange={(v) => set('stack', v)} label={<>Create stack</>} /></div></div></Frame>;
}

function AlbumDialog({ folder, desktop, d }: { folder: boolean; desktop: DesktopContextValue; d: DialogState }) { const [name, setName] = useState(text(d.params?.name)); const [error, setError] = useState(''); const save = async () => { if (!name.trim()) { setError(`Enter a ${folder ? 'folder' : 'album'} name.`); return; } try { await desktop.run(folder ? 'album.create' : 'album.create', { name: name.trim(), folder, addSelected: !folder }); desktop.setDialog(null); } catch (e) { setError(String(e)); } }; return <Frame title={folder ? 'New Folder' : 'New Album'} onClose={() => desktop.setDialog(null)} actions={<><Button className="lc-button" onClick={() => desktop.setDialog(null)}>Cancel</Button><Button className="lc-button lc-button-primary" variant="primary" onClick={() => void save()}>Create</Button></>}><div className="lc-field"><Field label={<span>{"Name"}</span>}>{({ id }) => <Input id={id} value={name} placeholder={folder ? 'Folder name' : 'Album name'} onChange={(event) => (setName)(event.target.value)} />}</Field></div>{error && <Notice tone="bad">{error}</Notice>}</Frame>; }
function SmartAlbumDialog({ desktop, d, kind = 'smartAlbum' }: { desktop: DesktopContextValue; d: DialogState; kind?: string }) { const editing = kind === 'smartRules'; const [name, setName] = useState(text(d.params?.name)); const [field, setField] = useState(text(d.params?.field, 'rating')); const [op, setOp] = useState(text(d.params?.operator, field === 'rating' ? 'gte' : 'is')); const [value, setValue] = useState(text(d.params?.value, '3')); const save = async () => { try { const numeric = ['rating', 'iso', 'aperture', 'focalLength', 'megapixels', 'album', 'sharpness'].includes(field); const boolean = field === 'edited'; const parsed = Number(value); const ruleValue = numeric ? (Number.isFinite(parsed) ? parsed : 0) : boolean ? ['true', 'yes', '1'].includes(value.trim().toLowerCase()) : value; const rules = { ruleSet: { match: 'all', rules: [{ field, op, value: ruleValue }] } }; if (editing) await desktop.run('album.setRules', { id: num(d.params?.id), rules, replace: true }); else await desktop.run('album.createSmart', { name: name.trim() || 'Smart Album', rules }); desktop.setDialog(null); } catch (e) { desktop.setNotice(String(e)); } }; const options: [string, string][] = field === 'rating' ? [["is", 'Is'], ['gte', 'At least'], ['lte', 'At most']] : field === 'camera' || field === 'keywords' ? [["contains", 'Contains'], ['is', 'Is'], ['isNot', 'Is not']] : [["is", 'Is'], ['isNot', 'Is not']]; return <Frame title={editing ? 'Edit Smart Album' : 'Smart Album'} onClose={() => desktop.setDialog(null)} actions={<><Button className="lc-button" onClick={() => desktop.setDialog(null)}>Cancel</Button><Button className="lc-button lc-button-primary" variant="primary" onClick={() => void save()}>{editing ? 'Save Rules' : 'Create Smart Album'}</Button></>}><div className="lc-field"><Field label={<span>{"Name"}</span>}>{({ id }) => <Input id={id} value={name} placeholder="Smart Album" onChange={(event) => (setName)(event.target.value)} />}</Field></div><div className="lc-field"><Field label={<span>{"Match field"}</span>}>{() => <Select label="Match field" value={field} options={([["rating", 'Rating'], ['flag', 'Pick flag'], ['label', 'Color label'], ['camera', 'Camera'], ['keywords', 'Keywords'], ['edited', 'Has edits']]).map(([value, label]) => ({ value, label }))} onChange={(v) => { setField(v); setOp(v === 'rating' ? 'gte' : v === 'camera' || v === 'keywords' ? 'contains' : 'is'); }} />}</Field></div><div className="lc-field"><Field label={<span>{"Condition"}</span>}>{() => <Select label="Condition" value={op} options={(options).map(([value, label]) => ({ value, label }))} onChange={setOp} />}</Field></div><div className="lc-field"><Field label={<span>{"Value"}</span>}>{({ id }) => <Input id={id} value={value} onChange={(event) => (setValue)(event.target.value)} />}</Field></div></Frame>; }

function SettingsDialog({ desktop, d }: { desktop: DesktopContextValue; d: DialogState }) {
  // RightKit settings panel: settings save as they change, so the dialog has a single Done action.
  return <Frame title="Settings" wide onClose={() => desktop.setDialog(null)} actions={<Button className="lc-button lc-button-primary" variant="primary" onClick={() => desktop.setDialog(null)}>Done</Button>}><EmberSettings desktop={desktop} initialSection={text(d.params?.tab)} /></Frame>;
}

// The engine's `SettingsGroup` ids and labels (crates/develop/src/presets.rs), in its order; unknown ids are refused.
const SETTINGS_GROUPS: readonly (readonly [string, string])[] = [['profile', 'Profile'], ['treatment', 'Treatment'], ['whiteBalance', 'White Balance'], ['light', 'Light'], ['toneCurve', 'Tone Curve'], ['color', 'Color'], ['colorMixer', 'Color Mixer'], ['colorGrading', 'Color Grading'], ['effects', 'Effects'], ['vignette', 'Vignette'], ['grain', 'Grain'], ['detail', 'Detail'], ['optics', 'Optics'], ['geometry', 'Geometry'], ['crop', 'Crop'], ['masks', 'Masking'], ['spots', 'Remove'], ['redEye', 'Red Eye'], ['lensBlur', 'Lens Blur'], ['calibration', 'Calibration']];
const SETTINGS_GROUP_IDS = SETTINGS_GROUPS.map(([id]) => id);
// `SettingsGroup::default_copy`: everything but crop, masks, spots and red eye.
const DEFAULT_COPY_GROUPS = SETTINGS_GROUP_IDS.filter((id) => !['crop', 'masks', 'spots', 'redEye'].includes(id));
function GroupsDialog({ paste, desktop, d }: { paste: boolean; desktop: DesktopContextValue; d: DialogState }) { const initial = arr(d.params?.groups).map(String).filter((group) => SETTINGS_GROUP_IDS.includes(group)); const [groups, setGroups] = useState<string[]>(initial.length ? initial : DEFAULT_COPY_GROUPS); const toggle = (group: string) => setGroups((old) => old.includes(group) ? old.filter((item) => item !== group) : [...old, group]); const submit = async () => { try { await desktop.run(paste ? 'develop.paste' : 'develop.copy', { groups }); desktop.setDialog(null); } catch (e) { desktop.setNotice(String(e)); } }; return <Frame title={paste ? 'Paste Selected Edit Settings' : 'Choose Edit Settings to Copy'} onClose={() => desktop.setDialog(null)} actions={<><Button className="lc-button" onClick={() => desktop.setDialog(null)}>Cancel</Button><Button className="lc-button lc-button-primary" variant="primary" onClick={() => void submit()}>{paste ? 'Paste Settings' : 'Copy Settings'}</Button></>}><div className="lc-group-list">{SETTINGS_GROUPS.map(([group, label]) => <Checkbox className="lc-check" key={group} checked={groups.includes(group)} onChange={() => toggle(group)} label={<>{label}</>} />)}</div><div className="lc-dialog-toolbar"><Button className="lc-button" onClick={() => setGroups([...SETTINGS_GROUP_IDS])}>Select all</Button><Button className="lc-button" onClick={() => setGroups([])}>Clear</Button></div></Frame>; }

function PresetFileDialog({ exportMode, curve, desktop, d }: { exportMode: boolean; curve: boolean; desktop: DesktopContextValue; d: DialogState }) { const [path, setPath] = useState(text(d.params?.path)); const [busy, setBusy] = useState(false); const chooseAction = exportMode ? (curve ? 'saveCurvePresetFile' : 'savePresetFile') : (curve ? 'pickCurvePresetFiles' : 'pickPresetFiles'); const command = curve ? (exportMode ? 'curve.exportPresets' : 'curve.importPresets') : (exportMode ? 'preset.export' : 'preset.import'); const submit = async () => { const paths = path ? [path] : await choose(desktop.native, chooseAction); if (!paths[0]) return; setPath(paths[0]); setBusy(true); try { await desktop.run(command, exportMode ? { path: paths[0] } : { paths }); desktop.setDialog(null); } catch (e) { desktop.setNotice(String(e)); } finally { setBusy(false); } }; return <Frame title={`${exportMode ? 'Export' : 'Import'} ${curve ? 'Curve Presets' : 'Presets'}`} busy={busy} onClose={() => desktop.setDialog(null)} actions={<><Button className="lc-button" onClick={() => desktop.setDialog(null)}>Cancel</Button><Button className="lc-button lc-button-primary" variant="primary" disabled={busy} onClick={() => void submit()}>{exportMode ? 'Export' : 'Import'}</Button></>}><div className="lc-dialog-toolbar"><Button className="lc-button" onClick={() => void submit()}>{exportMode ? 'Choose Save Location…' : 'Choose Preset Files…'}</Button></div><div className="lc-field"><Field label={<span>{exportMode ? 'Save as' : 'Files or folder'}</span>}>{({ id }) => <Input id={id} value={path} placeholder={curve ? '.lccurve' : '.lcpreset, .xmp or .lrtemplate'} onChange={(event) => (setPath)(event.target.value)} />}</Field></div><Notice tone="info">Imported presets keep source names & report settings that cannot be carried over.</Notice></Frame>; }

function MetadataDialog({ d, desktop }: { d: DialogState; desktop: DesktopContextValue }) { const metadata = (d.params?.metadata as AnyRecord) || {}; return <Frame title="All Metadata" wide onClose={() => desktop.setDialog(null)} actions={<Button className="lc-button lc-button-primary" variant="primary" onClick={() => desktop.setDialog(null)}>Done</Button>}><dl className="lc-info-list">{Object.entries(metadata).map(([key, value]) => <React.Fragment key={key}><dt>{key}</dt><dd>{typeof value === 'string' ? value : JSON.stringify(value)}</dd></React.Fragment>)}</dl></Frame>; }

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
  return <Frame title={title} onClose={() => desktop.setDialog(null)} actions={<Button className="lc-button lc-button-primary" variant="primary" onClick={() => desktop.setDialog(null)}>Done</Button>}>
    {kind === 'about' && <div className="lc-about">
      <img className="lc-app-mark" src={APP_ICON_URL} alt="" /><h3>{APP_NAME}</h3><p>Native photo library & non-destructive developer.</p>
      <small>Ember, maintained by Orthic Labs & contributors.</small>
      <small>Open source · version {text(credits?.version, text(d.params?.version, '0.2.1'))}</small>
      <div className="lc-dialog-toolbar" style={{ justifyContent: 'center' }}><Button className="lc-button" onClick={() => openLegalDocument('ember-licences')}>Licences and notices</Button></div>
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
      </> : creditsError ? <><Notice tone="bad">Could not load credits: {creditsError}</Notice><Button className="lc-button" onClick={() => { aboutLoaded.current = false; setCredits(null); setCreditsError(null); setAboutAttempt((attempt) => attempt + 1); }}>Retry</Button></> : <Notice tone="info">Loading credits…</Notice>}
    </div>}
    {kind === 'systeminfo' && <dl className="lc-info-list">{rows.map((row, i) => { const pair = Array.isArray(row) ? row : [row.label, row.value]; return <React.Fragment key={i}><dt>{text(pair[0])}</dt><dd>{text(pair[1], JSON.stringify(pair[1]))}</dd></React.Fragment>; })}</dl>}
    {kind === 'shortcuts' && <div className="lc-shortcuts">{arr(d.params?.shortcuts).map((shortcut, i) => <div key={i}><span>{text(shortcut.label, text(shortcut.id))}</span><kbd>{text(shortcut.shortcut)}</kbd></div>)}</div>}
    {kind === 'whatsnew' && <div className="lc-release-notes">{text(d.params?.text, 'Recent improvements to import, develop, export & catalog interoperability.')}</div>}
  </Frame>;
}

function FormDialog({ kind, d, desktop }: { kind: string; d: DialogState; desktop: DesktopContextValue }) { const formKind = kind.toLowerCase(); const p = d.params || {}; const [values, setValues] = useState<AnyRecord>({ ...p }); const update = (key: string, value: unknown) => setValues((old) => ({ ...old, [key]: value })); const config: AnyRecord = { rename: ['Rename Photos', 'photo.rename', [['template', 'Name template'], ['start', 'Start number']]], renamealbum: ['Rename Album', 'album.rename', [['id', 'Album ID'], ['name', 'Name']]], capturetime: ['Edit Capture Time', 'photo.setCaptureTime', [['time', 'Capture time'], ['shift', 'Shift seconds'], ['hours', 'Time-zone shift hours']]], autostack: ['Auto-Stack by Capture Time', 'stack.auto', [['gap', 'Maximum gap (seconds)']]], createpreset: ['Create Preset', 'preset.create', [['name', 'Preset name'], ['group', 'Group']]], copysettings: ['Copy Edit Settings', 'develop.copy', []], pastesettings: ['Paste Edit Settings', 'develop.paste', []], labelnames: ['Color Label Names', 'label.setNames', [['red', 'Red'], ['yellow', 'Yellow'], ['green', 'Green'], ['blue', 'Blue'], ['purple', 'Purple']]], savemetadatapreset: ['Save Metadata Preset', 'metadata.savePreset', [['name', 'Preset name']]], cull: ['Assisted Culling', 'photo.analyze', [['rejectBelow', 'Reject below focus score']]], textprompt: [text(p.title, 'Enter a value'), text(p.command), [[text(p.key, 'value'), text(p.hint, 'Value')]]], metadatapreset: ['Metadata Preset', 'metadata.applyPreset', [['name', 'Preset name']]], renamekeyword: ['Rename Keyword', 'keyword.rename', [['from', 'Current keyword'], ['to', 'New keyword']]], mergekeywords: ['Merge Keywords', 'keyword.merge', [['from', 'Keywords to merge (comma-separated)'], ['into', 'Merge into']]] }; const c = config[formKind]; const submit = async () => { try { let payload: AnyRecord = { ...values }; if (formKind === 'labelnames') payload = { names: Object.fromEntries(['red', 'yellow', 'green', 'blue', 'purple'].map((label) => [label, text(values[label]) || null])) }; else if (formKind === 'capturetime') { payload = {}; if (Array.isArray(values.ids)) payload.ids = values.ids; if (text(values.time)) payload.time = text(values.time); else if (values.shift !== undefined && text(values.shift) !== '') payload.shift = num(values.shift); else if (values.hours !== undefined && text(values.hours) !== '') payload.hours = num(values.hours); if (bool(values.each)) payload.each = true; } else if (formKind === 'rename') payload = { template: text(values.template), start: num(values.start, 1), ...(Array.isArray(values.ids) ? { ids: values.ids } : {}) }; else if (formKind === 'renamealbum') payload = { id: num(values.id), name: text(values.name) }; else if (formKind === 'autostack') payload = { gap: num(values.gap), ...(bool(values.preview) ? { preview: true } : {}) }; else if (formKind === 'createpreset') payload = { name: text(values.name), ...(text(values.group) ? { group: text(values.group) } : {}) }; else if (formKind === 'copysettings' || formKind === 'pastesettings') payload = { ...(Array.isArray(values.groups) ? { groups: values.groups } : {}) }; else if (formKind === 'metadatapreset') payload = { name: text(values.name) }; else if (formKind === 'savemetadatapreset') payload = { name: text(values.name), ...(values.fields ? { fields: values.fields } : {}) }; else if (formKind === 'renamekeyword') payload = { from: text(values.from), to: text(values.to) }; else if (formKind === 'mergekeywords') payload = { from: text(values.from).split(',').map((item) => item.trim()).filter(Boolean), into: text(values.into) }; else if (formKind === 'cull') payload = { ...(Array.isArray(values.ids) ? { ids: values.ids } : {}), rejectBelow: num(values.rejectBelow), pickBest: bool(values.pickBest, true) }; else if (formKind === 'textprompt') { const promptKey = text(p.key, 'value'); payload = { ...(typeof values.path === 'string' ? { path: values.path } : {}), [promptKey]: text(values[promptKey]) }; } await desktop.run(c[1], payload); desktop.setDialog(null); } catch (e) { desktop.setNotice(String(e)); } }; return <Frame title={c[0]} onClose={() => desktop.setDialog(null)} actions={<><Button className="lc-button" onClick={() => desktop.setDialog(null)}>Cancel</Button><Button className="lc-button lc-button-primary" variant="primary" onClick={() => void submit()}>Apply</Button></>}><div className="lc-form-stack">{arr(c[2]).map((field: any[]) => <div className="lc-field"><Field label={<span>{field[1]}</span>}>{({ id }) => <Input id={id} key={field[0]} value={typeof values[field[0]] === 'number' ? values[field[0]] : text(values[field[0]])} type={typeof values[field[0]] === 'number' ? 'number' : 'text'} onChange={(event) => ((v) => update(field[0], typeof values[field[0]] === 'number' ? Number(v) : v))(event.target.value)} />}</Field></div>)}{formKind === 'capturetime' && <Checkbox className="lc-check" checked={bool(values.each)} onChange={(v) => update('each', v)} label={<>Set this time on every selected photo</>} />}{['copysettings', 'pastesettings'].includes(formKind) && <Notice tone="info">Choose groups in Inspector before applying settings.</Notice>}{formKind === 'cull' && <Checkbox className="lc-check" checked={bool(values.pickBest, true)} onChange={(v) => update('pickBest', v)} label={<>Keep best frame in each burst</>} />}</div></Frame>; }

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
  return <Frame title="SAM 3 Model Setup" busy={busy} onClose={() => desktop.setDialog(null)} dismissible={!running && !validating} actions={<><Button className="lc-button" disabled={busy && !validating} onClick={() => desktop.setDialog(null)}>{running || validating ? 'Close' : 'Cancel'}</Button>{installed ? <Button className="lc-button lc-button-primary" variant="primary" onClick={() => desktop.setDialog(null)}>Continue</Button> : running ? <Button className="lc-button" disabled={busy} onClick={() => void cancelDownload()}>Cancel Download</Button> : validating ? <Button className="lc-button" onClick={() => void cancelValidation()}>Cancel Verification</Button> : <Button className="lc-button lc-button-primary" variant="primary" disabled={busy || !accepted || mirrors === 0} onClick={() => void download()}>Download</Button>}</>}>
    <Notice tone="info">Object & Describe masks use SAM 3, Meta’s segmentation model. Everything else works without it.</Notice>
    <p>A one-time download of about {sizeGb} GB is saved in:</p>
    <code className="lc-path-value">{text(status?.dir, 'No model folder configured')}</code>
    <div className="lc-dialog-toolbar"><Button className="lc-button" disabled={busy || !status?.dir} onClick={() => void openFolder()}>Open Model Folder</Button><Button className="lc-button" disabled={busy} onClick={() => void chooseFolder()}>Select Existing Model Folder…</Button></div>
    <p>Licence: SAM License (Meta). Downloading means accepting Meta’s terms.</p>
    <div className="lc-dialog-toolbar"><Button className="lc-button" onClick={() => void desktop.native('openUrl', { url: licenseUrl })}>Read SAM License</Button><Button className="lc-button" onClick={() => void desktop.native('openUrl', { url: 'https://github.com/Orthic-Labs/ember/blob/HEAD/docs/ai-masks.md' })}>SAM 3 Install Guide</Button></div>
    {!installed && !running && <Checkbox className="lc-check" checked={accepted} onChange={setAccepted} label={<>I accept SAM License terms & want to download this model.</>} />}
    {mirrors === 0 && !installed && <Notice tone="warn">No HTTPS download mirror is configured. Set LIGHTCRAFT_SAM3_MIRRORS or install files manually, then select their folder.</Notice>}
    {running && <div className="lc-progress" aria-live="polite"><div className="lc-progress-label"><span>{text(downloadState.file, 'Downloading SAM 3…')}</span><span>{percent}%</span></div><ProgressBar value={percent} max={100} label={text(downloadState.file, 'Downloading SAM 3…')} /></div>}
    {validating && <div className="lc-progress" aria-live="polite"><div className="lc-progress-label"><span>Verifying SAM 3 model folder…</span><span>{validationPercent}%</span></div><ProgressBar value={validationPercent} max={100} label="Verifying SAM 3 model folder…" /></div>}
    {installed && <Notice tone="info">SAM 3 files are installed. Object & Describe masks are ready.</Notice>}
    {error && <Notice tone="bad">{error}</Notice>}
    {text(downloadState.error) && <Notice tone="bad">{text(downloadState.error)}</Notice>}
  </Frame>;
}

function UnsupportedDialog({ desktop }: { kind: string; desktop: DesktopContextValue }) { return <Frame title="Dialog unavailable" onClose={() => desktop.setDialog(null)} actions={<Button className="lc-button lc-button-primary" variant="primary" onClick={() => desktop.setDialog(null)}>Close</Button>}><Notice tone="bad">Sorry, this action isn’t available in this version.</Notice></Frame>; }

function MissingDialog({ desktop, d }: { desktop: DesktopContextValue; d: DialogState }) { const [busy, setBusy] = useState(false); const [error, setError] = useState(''); const run = async () => { const paths = await choose(desktop.native, 'chooseFolder'); if (!paths[0]) return; setBusy(true); try { await desktop.run('library.findMissing', { folder: paths[0] }); desktop.setDialog(null); } catch (e) { setError(String(e)); } finally { setBusy(false); } }; return <Frame title="Find Missing Photos" busy={busy} onClose={() => desktop.setDialog(null)} actions={<><Button className="lc-button" onClick={() => desktop.setDialog(null)}>Cancel</Button><Button className="lc-button lc-button-primary" variant="primary" disabled={busy} onClick={() => void run()}>Choose Search Folder…</Button></>}>{error && <Notice tone="bad">{error}</Notice>}<Notice tone="info">Ember checks names, sizes & content hashes, then relinks matches in one undoable step.</Notice>{d.params?.count != null && <p>{num(d.params.count)} photos are missing.</p>}</Frame>; }
function LocalRootDialog({ desktop }: { desktop: DesktopContextValue }) { const [subfolders, setSubfolders] = useState(true); const chooseRoot = async () => { const paths = await choose(desktop.native, 'chooseFolder'); if (!paths[0]) return; try { await desktop.run('library.browse', { path: paths[0], subfolders }); desktop.setDialog(null); } catch (e) { desktop.setNotice(String(e)); } }; return <Frame title="Browse Local Folder" onClose={() => desktop.setDialog(null)} actions={<><Button className="lc-button" onClick={() => desktop.setDialog(null)}>Cancel</Button><Button className="lc-button lc-button-primary" variant="primary" onClick={() => void chooseRoot()}>Choose Folder…</Button></>}><Checkbox className="lc-check" checked={subfolders} onChange={setSubfolders} label={<>Include subfolders</>} /><Notice tone="info">Photos stay in place & remain outside catalog until you import them.</Notice></Frame>; }
function LightroomResult({ d, desktop }: { d: DialogState; desktop: DesktopContextValue }) {
  const [result, setResult] = useState<AnyRecord>((d.params?.result as AnyRecord) || {});
  const [busy, setBusy] = useState(false); const [error, setError] = useState('');
  const catalog = text(d.params?.path); const missing = arr(result.missing); const failed = arr(result.failed); const warnings = arr(result.warnings); const unmapped = result.unmapped && typeof result.unmapped === 'object' ? Object.entries(result.unmapped as AnyRecord) : []; const indexWarning = displayValue(result.indexWarning); const indexError = displayValue(result.error);
  const run = async (operation: () => Promise<unknown>, close = false, updateResult = false) => { setBusy(true); setError(''); try { const value = await operation(); if (close) desktop.setDialog(null); else if (updateResult && value && typeof value === 'object') setResult(value as AnyRecord); } catch (reason) { setError(errorText(reason)); } finally { setBusy(false); } };
  const reimport = async () => { if (!catalog) { setError('Original catalog path is unavailable.'); return; } setBusy(true); setError(''); try { const task = await desktop.run('library.importLightroom', { path: catalog, updateExisting: bool(d.params?.updateExisting) }) as AnyRecord; const taskId = text(task.taskId); if (!taskId) throw new Error('LRCAT import did not return a task ID.'); desktop.setDialog({ kind: 'lightroomImportProgress', params: { taskId, path: catalog, updateExisting: bool(d.params?.updateExisting), report: result } }); } catch (reason) { setError(errorText(reason)); setBusy(false); } };
  const undo = () => run(() => desktop.run('edit.undo'), true);
  const reveal = () => { const archive = text(result.archive); return archive ? run(() => desktop.native('reveal', { path: archive })) : setError('No recovery archive was created for this library.'); };
  const imported = Array.isArray(result.imported) ? result.imported.length : num(result.imported); const photos = num(result.photos, imported); const archive = text(result.archive);
  return <Frame title="LRCAT Catalog Imported" busy={busy} onClose={() => desktop.setDialog(null)} actions={<Button className="lc-button lc-button-primary" variant="primary" disabled={busy} onClick={() => desktop.setDialog(null)}>Done</Button>}>
    <div className="lc-summary-grid"><div><strong>{photos}</strong><span>Photos in catalog</span></div><div><strong>{imported}</strong><span>Files imported</span></div><div><strong>{num(result.collections)}</strong><span>Collections</span></div><div><strong>{missing.length}</strong><span>Missing originals</span></div><div><strong>{num(result.preservedExistingEdits)}</strong><span>Edits preserved</span></div></div>
    {error && <Notice tone="bad">{error}</Notice>}
    <Notice tone="info">Source catalog stayed read-only. Ember saved import identity & recovery data for this catalog.</Notice>
    {result.indexWarning != null && <Notice tone="warn">Recovery index warning: {indexWarning}</Notice>}
    {result.error != null && <Notice tone="warn">Recovery index error: {indexError}</Notice>}
    {archive ? <div className="lc-dialog-toolbar"><span className="lc-path">Recovery archive: {archive}</span><Button className="lc-button" disabled={busy} onClick={() => void reveal()}>Show Recovery Archive</Button></div> : <Notice tone="info">No recovery archive was created for this library.</Notice>}
    {missing.length > 0 && <details className="lc-details" open><summary>Missing originals ({missing.length})</summary><ul>{missing.map((path, i) => <li key={i}>{displayValue(path)}</li>)}</ul></details>}
    {warnings.length > 0 && <details className="lc-details"><summary>Warnings ({warnings.length})</summary><ul>{warnings.map((warning, i) => <li key={i}>{displayValue(warning)}</li>)}</ul></details>}
    {failed.length > 0 && <details className="lc-details"><summary>Files not imported ({failed.length})</summary><ul>{failed.map((failure, i) => <li key={i}>{displayValue(failure)}</li>)}</ul></details>}
    {unmapped.length > 0 && <details className="lc-details"><summary>Settings not mapped ({unmapped.length})</summary><ul>{unmapped.map(([id, values]) => <li key={id}>Photo {id}: {displayValue(values)}</li>)}</ul></details>}
    <div className="lc-dialog-toolbar"><Button className="lc-button" disabled={busy || !catalog} onClick={() => void reimport()}>Reimport Same Catalog</Button><Button className="lc-button" disabled={busy} onClick={() => void undo()}>Undo Import</Button></div>
  </Frame>;
}

export default function DialogHost({ desktop: provided }: HostProps = {}) {
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
