import { useEffect, useRef, useState } from 'react';
import { Button } from '@rightkit/react-shell/react';
import { useDesktop } from './desktop';
import './backgroundActivity.css';

/** Session-owned jobs remain visible after their initiating dialog closes. */
export function BackgroundActivity() {
  const { snapshot, run, t } = useDesktop();
  const [open, setOpen] = useState(false);
  const [cancelling, setCancelling] = useState<string[]>([]);
  const [error, setError] = useState('');
  const root = useRef<HTMLDivElement>(null);
  const trigger = useRef<HTMLButtonElement>(null);
  const jobs = snapshot?.status.jobs ?? [];
  const completed = (snapshot?.status.completedJobs ?? []).slice(-8).reverse();
  useEffect(() => {
    if (!open) return;
    const outside = (event: PointerEvent) => {
      if (event.target instanceof Node && !root.current?.contains(event.target)) setOpen(false);
    };
    const escape = (event: KeyboardEvent) => {
      if (event.key !== 'Escape') return;
      event.preventDefault();
      event.stopPropagation();
      setOpen(false);
      trigger.current?.focus();
    };
    document.addEventListener('pointerdown', outside);
    // A completing task can remove its focused Cancel button. Escape must still
    // dismiss the open panel when the browser consequently returns focus to body.
    document.addEventListener('keydown', escape, true);
    return () => { document.removeEventListener('pointerdown', outside); document.removeEventListener('keydown', escape, true); };
  }, [open]);
  const cancel = async (id: string) => {
    if (cancelling.includes(id)) return;
    setCancelling((current) => [...current, id]);
    setError('');
    try { await run('task.cancel', { id }); }
    catch (reason) { setError(reason instanceof Error ? reason.message : String(reason)); }
    finally { setCancelling((current) => current.filter((value) => value !== id)); }
  };
  return <div className="lc-background-activity" ref={root}>
    <Button ref={trigger} size="sm" variant="ghost" aria-expanded={open} aria-controls="lc-background-jobs" aria-label={t('Background activity')} onClick={() => setOpen((value) => !value)}>
      {jobs.length > 0 && <span className="lc-job-indicator" aria-hidden="true" />}
      {t('Tasks')}{jobs.length > 0 && <span className="lc-job-count">{jobs.length}</span>}
    </Button>
    {open && <section id="lc-background-jobs" className="lc-background-jobs" aria-label={t('Background activity')}>
      <h2>{t('Background activity')}</h2>
      {jobs.length === 0 && <p>{t('No tasks running.')}</p>}
      <div className="lc-job-list">
        {jobs.map((job) => {
          const total = Math.max(0, job.total);
          const count = Math.max(0, Math.min(job.completed, total));
          return <div className="lc-job" key={job.id} data-task-id={job.id}>
            <div className="lc-job-heading"><strong>{t(job.label)}</strong><span>{total > 0 ? `${count} / ${total}` : t('Working…')}</span></div>
            <progress aria-label={t(job.label)} max={total || 1} value={total > 0 ? count : undefined} />
            {job.error && <p className="lc-job-error">{job.error}</p>}
            {job.cancellable && <Button size="sm" variant="ghost" disabled={cancelling.includes(job.id)} onClick={() => void cancel(job.id)}>{t(cancelling.includes(job.id) ? 'Cancelling…' : 'Cancel')}</Button>}
          </div>;
        })}
        {completed.length > 0 && <h3>{t('Recent tasks')}</h3>}
        {completed.map((job) => <div className="lc-job lc-job-terminal" key={job.id} data-task-id={job.id} data-task-state={job.state}>
          <div className="lc-job-heading"><strong>{t(job.label)}</strong><span>{t(job.state === 'done' ? 'Completed' : job.state === 'cancelled' ? 'Cancelled' : 'Failed')}</span></div>
          {job.error && <p className="lc-job-error">{job.error}</p>}
        </div>)}
      </div>
      {error && <p className="lc-job-error" role="alert">{error}</p>}
    </section>}
  </div>;
}
