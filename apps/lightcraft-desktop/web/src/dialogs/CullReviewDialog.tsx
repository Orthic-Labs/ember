import { createPortal } from "react-dom";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import type { CompletedJobState, DesktopContextValue, JsonObject, JobStatus } from "../types";
import PhotoPreview from "../preview/PhotoPreview";
import { Button, Checkbox, IconButton, ModalShell, ProgressBar } from "@rightkit/react-shell/react";
import { useInertSiblings } from "./inertSiblings";
import "./cullReview.css";

type CullReviewDesktop = Pick<DesktopContextValue, "run" | "refresh" | "snapshot">;
type Flag = "none" | "pick" | "reject";
type CullPhase = "analysis" | "apply" | null;

export interface CullReviewPhoto {
  id: number;
  fileName?: string;
  flag?: Flag;
}

/** Props parent dialog host can supply without coupling review UI to catalog views. */
export interface CullReviewDialogProps {
  desktop: CullReviewDesktop;
  photoIds?: readonly number[];
  photos?: readonly CullReviewPhoto[];
  onClose: () => void;
}

interface ReviewRow {
  id: number;
  fileName: string;
  existingFlag: Flag;
  proposedFlag: Flag | null;
  sharpness: number | null;
  reason: string;
  uncertainty: string;
  failure: string | null;
  group: string | null;
  best: boolean;
}

const DEFAULT_REJECT_BELOW = 50;
const REVIEW_PAGE_SIZE = 50;
const EMPTY_PHOTOS: readonly CullReviewPhoto[] = [];

function record(value: unknown): Record<string, unknown> | null {
  return value && typeof value === "object" && !Array.isArray(value) ? value as Record<string, unknown> : null;
}

function finite(value: unknown): number | null {
  return typeof value === "number" && Number.isFinite(value) ? value : null;
}

function flag(value: unknown): Flag {
  return value === "pick" || value === "reject" ? value : "none";
}

function text(value: unknown, fallback: string): string {
  return typeof value === "string" && value.trim() ? value : fallback;
}

function humanize(value: string): string {
  return value.replace(/[_-]+/g, " ").replace(/\b\w/g, (letter) => letter.toUpperCase());
}

function parseReason(row: Record<string, unknown>): string {
  const reasonCodes = row.reasonCodes;
  if (Array.isArray(reasonCodes)) {
    const first = reasonCodes.find((item): item is string => typeof item === "string" && item.length > 0);
    if (first) return humanize(first);
  }
  if (row.failure) return "Analysis failed";
  if (row.proposedFlag === "reject") return "Below focus threshold";
  if (row.proposedFlag === "pick" || row.best === true) return "Sharpest in burst";
  if (row.group !== undefined && row.group !== null) return "Similar burst detected";
  return "No action suggested";
}

function parseUncertainty(row: Record<string, unknown>): string {
  if (typeof row.uncertainty === "string" && row.uncertainty.trim()) return humanize(row.uncertainty);
  if (row.failure) return "Unavailable";
  return "Review signal";
}

function parseRows(
  ids: readonly number[],
  photos: readonly CullReviewPhoto[],
  result: Record<string, unknown>,
): ReviewRow[] {
  const metadata = new Map(photos.map((photo) => [photo.id, photo]));
  const rows = new Map<number, ReviewRow>();
  for (const id of ids) {
    const photo = metadata.get(id);
    rows.set(id, {
      id,
      fileName: text(photo?.fileName, `Photo ${id}`),
      existingFlag: flag(photo?.flag),
      proposedFlag: null,
      sharpness: null,
      reason: "No action suggested",
      uncertainty: "Review signal",
      failure: null,
      group: null,
      best: false,
    });
  }

  const rawPhotos = Array.isArray(result.photos) ? result.photos : [];
  for (const value of rawPhotos) {
    const raw = record(value);
    const id = finite(raw?.id);
    if (!raw || id === null || !rows.has(id)) continue;
    const prior = rows.get(id)!;
    const resultFlag = flag(raw.previousFlag ?? raw.flag);
    const next: ReviewRow = {
      ...prior,
      fileName: text(raw.fileName ?? raw.name, prior.fileName),
      existingFlag: prior.existingFlag === "none" ? resultFlag : prior.existingFlag,
      proposedFlag: raw.proposedFlag === "pick" || raw.proposedFlag === "reject" ? raw.proposedFlag : null,
      sharpness: finite(raw.sharpness),
      group: raw.group === null || raw.group === undefined ? null : String(raw.group),
      best: raw.best === true,
      reason: parseReason(raw),
      uncertainty: parseUncertainty(raw),
    };
    rows.set(id, next);
  }

  if (Array.isArray(result.failed)) {
    for (const value of result.failed) {
      const pair = Array.isArray(value) ? value : null;
      const raw = record(value);
      const id = finite(pair?.[0] ?? raw?.id);
      if (id === null || !rows.has(id)) continue;
      const message = pair?.[1] ?? raw?.error;
      const prior = rows.get(id)!;
      rows.set(id, { ...prior, failure: text(message, "Analysis failed"), reason: "Analysis failed", uncertainty: "Unavailable" });
    }
  }
  return ids.map((id) => rows.get(id)!).filter(Boolean);
}

function resultProposal(result: Record<string, unknown>): Record<string, unknown> | null {
  return record(result.proposal) ?? (result.version === 1 && Array.isArray(result.photos) ? result : null);
}

function isStaleError(error: unknown): boolean {
  const message = error instanceof Error ? error.message : String(error);
  return /stale|changed|out of date/i.test(message);
}

export function CullReviewDialog({ desktop, photoIds, photos, onClose }: CullReviewDialogProps) {
  const photoList = photos ?? EMPTY_PHOTOS;
  const stableIdsRef = useRef<number[] | null>(null);
  if (stableIdsRef.current === null) {
    const supplied = photoIds && photoIds.length > 0 ? photoIds : desktop.snapshot?.selection ?? [];
    stableIdsRef.current = Array.from(new Set(supplied)).filter((id): id is number => Number.isSafeInteger(id));
  }
  const stableIds = stableIdsRef.current ?? [];
  const photoMetadata = useMemo(() => new Map(photoList.map((photo) => [photo.id, photo])), [photoList]);
  const [rejectEnabled, setRejectEnabled] = useState(false);
  const [rejectBelowText, setRejectBelowText] = useState(String(DEFAULT_REJECT_BELOW));
  const [rows, setRows] = useState<ReviewRow[]>([]);
  const [page, setPage] = useState(0);
  const [proposal, setProposal] = useState<JsonObject | null>(null);
  const [selected, setSelected] = useState<Set<number>>(() => new Set());
  const [busy, setBusy] = useState(false);
  const [applying, setApplying] = useState(false);
  const [taskId, setTaskId] = useState<string | null>(null);
  const [phase, setPhase] = useState<CullPhase>(null);
  const [job, setJob] = useState<JobStatus | null>(null);
  const [cancelling, setCancelling] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [stale, setStale] = useState(false);
  const generationRef = useRef(0);
  const abandonedGenerationsRef = useRef<Set<number>>(new Set());
  const liveRef = useRef(true);
  const snapshotRef = useRef(desktop.snapshot);
  const terminalRef = useRef<CompletedJobState | null>(null);
  const pollingRef = useRef(false);
  const applyingRef = useRef(false);
  const autoStartedRef = useRef(false);
  const taskIdRef = useRef<string | null>(null);
  const cancelSentRef = useRef<string | null>(null);
  const applyCancelRequestedRef = useRef(false);
  const runRef = useRef(desktop.run);
  snapshotRef.current = desktop.snapshot;
  applyingRef.current = applying;
  taskIdRef.current = taskId;
  runRef.current = desktop.run;

  useEffect(() => {
    liveRef.current = true;
    return () => {
      liveRef.current = false;
      const generation = generationRef.current;
      // StrictMode simulates cleanup followed immediately by setup. Defer
      // abandonment so simulated remount can restore live state first.
      queueMicrotask(() => {
        if (liveRef.current) return;
        abandonedGenerationsRef.current.add(generation);
        if (generationRef.current === generation) generationRef.current += 1;
        const id = taskIdRef.current;
        if (id && !terminalRef.current && cancelSentRef.current !== id) {
          cancelSentRef.current = id;
          void runRef.current("task.cancel", { id }).catch(() => undefined);
        }
      });
    };
  }, []);

  const requestCancel = useCallback(async (id: string) => {
    if (cancelling || terminalRef.current || cancelSentRef.current === id) return;
    cancelSentRef.current = id;
    setCancelling(true);
    try {
      const result = record(await runRef.current("task.cancel", { id }));
      if (result && finite(result.cancelled) === 0 && liveRef.current && phase !== "apply" && !applyCancelRequestedRef.current) setError("Analysis is no longer running.");
    } catch (cause) {
      if (liveRef.current) setError(text(cause instanceof Error ? cause.message : cause, "Could not cancel analysis."));
    } finally {
      if (liveRef.current) setCancelling(false);
    }
  }, [cancelling, phase]);

  const cancelTask = useCallback(() => {
    if (taskId) void requestCancel(taskId);
  }, [requestCancel, taskId]);

  const close = useCallback(() => {
    abandonedGenerationsRef.current.add(generationRef.current);
    generationRef.current += 1;
    if (taskId && !terminalRef.current) void cancelTask();
    onClose();
  }, [cancelTask, onClose, taskId]);

  const bodyRef = useRef<HTMLDivElement>(null);
  useInertSiblings(bodyRef);

  useEffect(() => {
    if (!taskId) return;
    const taskGeneration = generationRef.current;
    let live = true;
    const poll = async () => {
      if (!live || pollingRef.current) return;
      pollingRef.current = true;
      try {
        await desktop.refresh();
        if (!live || !liveRef.current || generationRef.current !== taskGeneration) return;
        const current = snapshotRef.current;
        const found = current?.status.jobs.find((candidate) => candidate.id === taskId);
        if (found) {
          setJob(found);
          if (found.error) {
            terminalRef.current = "failed";
            setTaskId(null);
            setPhase(null);
            setBusy(false);
            setApplying(false);
            setStale(isStaleError(found.error));
            setError(found.error);
          }
          return;
        }
        const completed = current?.status.completedJobs.find((candidate) => candidate.id === taskId);
        if (!completed) return;
        const result = record(completed.result);
        const completionError = text(completed.error, text(result?.error, ""));
        terminalRef.current = completed.state;
        applyCancelRequestedRef.current = false;
        setCancelling(false);
        setJob({ id: completed.id, kind: completed.kind, label: completed.label, completed: 1, total: 1, cancellable: false, ...(completionError ? { error: completionError } : {}) });
        setTaskId(null);
        taskIdRef.current = null;
        setPhase(null);
        setBusy(false);
        const completedPhase = phase;
        setApplying(false);
        if (completedPhase === "apply") {
          if (completed.state === "done") {
            setError(null);
            if (liveRef.current) onClose();
            return;
          }
          if (completed.state === "cancelled") {
            setError("Apply cancelled. No changes were made.");
            return;
          }
          setStale(isStaleError(completionError));
          setError(isStaleError(completionError) ? "Photo set changed. Retry analysis." : completionError || "Could not apply cull proposal.");
          return;
        }
        if (completed.state === "done" && result) {
          setError(null);
          const parsedProposal = resultProposal(result);
          if (!parsedProposal) {
            setError("Cull analysis returned no review proposal.");
            return;
          }
          setProposal(parsedProposal);
          setPage(0);
          setRows(parseRows(stableIds, photoList, result));
          return;
        }
        if (completed.state === "cancelled") {
          setError("Analysis cancelled. No changes were made.");
          return;
        }
        setStale(isStaleError(completionError));
        setError(isStaleError(completionError) ? "Photo set changed. Retry analysis." : completionError || "Cull analysis failed.");
      } catch (cause) {
        if (live && liveRef.current) setError(text(cause instanceof Error ? cause.message : cause, "Could not read cull progress."));
      } finally {
        pollingRef.current = false;
      }
    };
    void poll();
    const timer = window.setInterval(() => void poll(), 300);
    return () => { live = false; window.clearInterval(timer); };
  }, [desktop.refresh, onClose, phase, photoList, stableIds, taskId]);

  const generate = useCallback(async () => {
    if (stableIds.length === 0) {
      setError("Select at least one photo to analyse.");
      return;
    }
    const threshold = Number(rejectBelowText);
    if (rejectEnabled && (!rejectBelowText.trim() || !Number.isFinite(threshold) || threshold < 0 || threshold > 100)) {
      setError("Reject threshold must be between 0 and 100.");
      return;
    }
    const generation = generationRef.current + 1;
    generationRef.current = generation;
    setBusy(true);
    setCancelling(false);
    terminalRef.current = null;
    applyCancelRequestedRef.current = false;
    setTaskId(null);
    taskIdRef.current = null;
    cancelSentRef.current = null;
    setPhase(null);
    setJob(null);
    setError(null);
    setStale(false);
    setProposal(null);
    setRows([]);
    setPage(0);
    setSelected(new Set());
    let backgroundTask = false;
    try {
      const params: JsonObject = { ids: stableIds, pickBest: true };
      if (rejectEnabled) params.rejectBelow = threshold;
      const rawResult = await desktop.run("photo.cullSuggest", params);
      const result = record(rawResult) ?? {};
      const returnedTaskId = text(result.taskId, "");
      if (!liveRef.current || generationRef.current !== generation || abandonedGenerationsRef.current.has(generation)) {
        if (returnedTaskId) {
          if (cancelSentRef.current !== returnedTaskId) {
            cancelSentRef.current = returnedTaskId;
            try { await runRef.current("task.cancel", { id: returnedTaskId }); } catch { /* best-effort orphan cleanup */ }
          }
        }
        return;
      }
      if (returnedTaskId) {
        backgroundTask = true;
        taskIdRef.current = returnedTaskId;
        cancelSentRef.current = null;
        setPhase("analysis");
        setTaskId(returnedTaskId);
        return;
      }
      const parsedProposal = resultProposal(result);
      if (!parsedProposal) {
        setError("Cull analysis returned no review proposal.");
        return;
      }
      setProposal(parsedProposal);
      // Keep top-level result for failed entries; nested proposal is apply payload.
      setPage(0);
      setRows(parseRows(stableIds, photoList, result));
    } catch (cause) {
      if (!liveRef.current || generationRef.current !== generation) return;
      setError(isStaleError(cause) ? "Photo set changed. Retry analysis." : text(cause instanceof Error ? cause.message : cause, "Cull analysis failed."));
      setStale(isStaleError(cause));
    } finally {
      if (liveRef.current && generationRef.current === generation && !backgroundTask) setBusy(false);
    }
  }, [desktop, photoList, rejectBelowText, rejectEnabled, stableIds]);

  const cancel = useCallback(() => {
    if (!busy && !applying) return;
    const wasApplying = applying;
    if (wasApplying) {
      applyCancelRequestedRef.current = true;
      if (taskId) void requestCancel(taskId);
      setError("Cancellation requested; waiting for host result…");
      return;
    }
    abandonedGenerationsRef.current.add(generationRef.current);
    generationRef.current += 1;
    void cancelTask();
    setTaskId(null);
    taskIdRef.current = null;
    setBusy(false);
    setPhase(null);
    setError("Analysis cancelled. No changes were made.");
  }, [applying, busy, cancelTask, requestCancel, taskId]);

  const apply = useCallback(async () => {
    if (!proposal || applying || stale) return;
    const accepted = rows
      .filter((row) => selected.has(row.id) && row.existingFlag === "none" && row.proposedFlag)
      .map((row) => ({ id: row.id, flag: row.proposedFlag! }));
    if (accepted.length === 0) {
      setError("Select suggested photos to accept before applying.");
      return;
    }
    setApplying(true);
    setPhase(null);
    setJob(null);
    terminalRef.current = null;
    applyCancelRequestedRef.current = false;
    setError(null);
    const generation = generationRef.current;
    let backgroundTask = false;
    try {
      const rawResult = await desktop.run("photo.cullApply", { proposal, accept: accepted });
      const result = record(rawResult) ?? {};
      const returnedTaskId = text(result.taskId, "");
      if (!liveRef.current || generationRef.current !== generation || abandonedGenerationsRef.current.has(generation)) {
        if (returnedTaskId && cancelSentRef.current !== returnedTaskId) {
          cancelSentRef.current = returnedTaskId;
          try { await runRef.current("task.cancel", { id: returnedTaskId }); } catch { /* best-effort orphan cleanup */ }
        }
        return;
      }
      if (returnedTaskId) {
        backgroundTask = true;
        taskIdRef.current = returnedTaskId;
        cancelSentRef.current = null;
        setPhase("apply");
        setTaskId(returnedTaskId);
        if (applyCancelRequestedRef.current) void requestCancel(returnedTaskId);
        return;
      }
      setApplying(false);
      setPhase(null);
      onClose();
    } catch (cause) {
      if (!liveRef.current) return;
      setApplying(false);
      if (isStaleError(cause)) {
        setStale(true);
        setError("Photo set changed. Retry analysis; no changes were applied.");
      } else {
        setError(text(cause instanceof Error ? cause.message : cause, "Could not apply cull proposal."));
      }
    } finally {
      if (liveRef.current && generationRef.current === generation && !backgroundTask) setApplying(false);
    }
  }, [applying, desktop, onClose, proposal, requestCancel, rows, selected, stale]);

  useEffect(() => {
    if (autoStartedRef.current) return;
    autoStartedRef.current = true;
    void generate();
    // Initial settings are intentionally captured once when dialog opens.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const toggle = (id: number) => {
    setSelected((previous) => {
      const next = new Set(previous);
      if (next.has(id)) next.delete(id); else next.add(id);
      return next;
    });
  };

  const progressTotal = Math.max(1, job?.total ?? stableIds.length);
  const progressCompleted = job ? Math.max(0, Math.min(job.completed, progressTotal)) : 0;
  const pageCount = Math.max(1, Math.ceil(rows.length / REVIEW_PAGE_SIZE));
  const currentPage = Math.min(page, pageCount - 1);
  const pageStart = currentPage * REVIEW_PAGE_SIZE;
  const pageRows = rows.slice(pageStart, pageStart + REVIEW_PAGE_SIZE);
  const pageEnd = Math.min(rows.length, pageStart + pageRows.length);

  const body = (
    <ModalShell label="Review focus suggestions" labelledBy="lc-cull-review-title" width={980} onClose={close} dismissible={!applying} overlayClassName="lc-cull-review-backdrop" className="lc-cull-review-dialog">
        <header className="lc-cull-review-header">
          <div>
            <p className="lc-cull-review-eyebrow">Cull review</p>
            <h2 id="lc-cull-review-title">Review focus suggestions</h2>
            <p className="lc-cull-review-subtitle">Suggestions are read-only until you explicitly select photos to apply.</p>
          </div>
          <IconButton className="lc-cull-review-close" label="Close" onClick={close} disabled={applying}>×</IconButton>
        </header>

        <div ref={bodyRef} className="lc-cull-review-body">
          <div className="lc-cull-review-settings" aria-label="Analysis settings">
            <Checkbox className="lc-cull-review-check" checked={rejectEnabled} onChange={setRejectEnabled} disabled={busy || applying} label="Suggest rejects below focus threshold" />
            <label className="lc-cull-review-threshold">Threshold
              <input type="number" min={0} max={100} step={1} value={rejectBelowText} onChange={(event) => setRejectBelowText(event.target.value)} disabled={!rejectEnabled || busy || applying} />
              <span>/ 100</span>
            </label>
            <Button className="lc-cull-review-button-secondary" onClick={() => void generate()} disabled={busy || applying || stableIds.length === 0}>Generate suggestions</Button>
          </div>

          <p className="lc-cull-review-note">Focus score is a qualitative review signal from current classical analysis; it is not an accuracy or confidence claim.</p>
          {(busy || applying) && <div className="lc-cull-review-progress" aria-live="polite"><span>{job?.label ?? (applying ? "Applying cull proposal…" : `Analysing ${stableIds.length} photo${stableIds.length === 1 ? "" : "s"}…`)}</span><ProgressBar className={job && job.total > 0 ? undefined : "is-indeterminate"} value={job && job.total > 0 ? progressCompleted : 0} max={job && job.total > 0 ? progressTotal : 1} label={applying ? "Applying cull proposal" : "Analysing photos"} /><Button className="lc-cull-review-button-secondary" onClick={cancel} disabled={cancelling}>{cancelling ? "Cancelling…" : applying ? "Cancel apply" : "Cancel"}</Button></div>}
          {error && <div className={`lc-cull-review-message ${stale ? "is-warning" : "is-error"}`} role="alert">{error}{stale && <Button className="lc-cull-review-button-secondary" onClick={() => void generate()} disabled={busy || applying}>Retry</Button>}</div>}

          {rows.length > 0 && <div className="lc-cull-review-list" aria-label="Cull suggestions">
            {pageRows.map((row) => {
              const canAccept = row.existingFlag === "none" && row.proposedFlag !== null && !row.failure;
              const metadata = photoMetadata.get(row.id);
              return <article className={`lc-cull-review-row ${row.failure ? "is-failure" : ""}`} key={row.id}>
                <div className="lc-cull-review-thumb"><PhotoPreview photoId={row.id} slot={`cull-review-${row.id}`} viewGeneration={desktop.snapshot?.viewGeneration ?? 0} width={88} height={66} quality="draft" alt={row.fileName} /></div>
                <div className="lc-cull-review-info"><strong>{metadata?.fileName ?? row.fileName}</strong><span>Photo ID {row.id}</span><span>{row.reason} · {row.uncertainty}</span>{row.failure && <span className="lc-cull-review-failure">{row.failure}</span>}</div>
                <div className="lc-cull-review-score"><span>Focus score</span><strong>{row.sharpness === null ? "—" : row.sharpness.toFixed(1)}</strong>{row.group && <small>Burst {row.group}{row.best ? " · best" : ""}</small>}</div>
                <div className="lc-cull-review-action">{row.existingFlag !== "none" ? <span className="lc-cull-review-existing">Existing {row.existingFlag}; unchanged</span> : <Checkbox className="lc-cull-review-check" checked={selected.has(row.id)} onChange={() => toggle(row.id)} disabled={!canAccept || busy || applying || stale} label={row.proposedFlag ? `Accept ${row.proposedFlag}` : "No action"} />}</div>
              </article>;
            })}
          </div>}
          {rows.length > 0 && <nav className="lc-cull-review-pagination" aria-label="Cull review pages"><span>Showing {pageStart + 1}–{pageEnd} of {rows.length} photos</span><div><Button className="lc-cull-review-button-secondary" onClick={() => setPage((current) => Math.max(0, current - 1))} disabled={currentPage === 0 || busy || applying}>Previous</Button><Button className="lc-cull-review-button-secondary" onClick={() => setPage((current) => Math.min(pageCount - 1, current + 1))} disabled={currentPage >= pageCount - 1 || busy || applying}>Next</Button></div></nav>}
          {!busy && rows.length === 0 && stableIds.length === 0 && <p className="lc-cull-review-empty">No photos selected.</p>}
        </div>
        <footer className="lc-cull-review-footer"><span>{selected.size} selected · Apply creates one undo step</span><div><Button className="lc-cull-review-button-secondary" onClick={close} disabled={applying}>Cancel</Button><Button variant="primary" className="lc-cull-review-button-primary" onClick={() => void apply()} disabled={applying || busy || stale || selected.size === 0}>{applying ? "Applying…" : "Apply selected"}</Button></div></footer>
    </ModalShell>
  );
  return typeof document === "undefined" ? null : createPortal(body, document.body);
}

export default CullReviewDialog;
