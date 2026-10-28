import { useCallback, useEffect, useRef, useState } from 'react';
import { cancelLargeOcr, dismissLargeOcr, largeOcrStatus, pauseLargeOcr, relocateLargeOcr, resumeLargeOcr, type LargeOcrJob, type LargeOcrPageReport, type LargeOcrPauseReason, type LargeOcrStatus } from './bridge';
import s from './LargeOcrPanel.module.css';

export type LargeOcrApi = {
  status: () => Promise<LargeOcrStatus>;
  pause: () => Promise<LargeOcrStatus>;
  cancel: () => Promise<LargeOcrStatus>;
  resume: (documentId: number, revision: number) => Promise<LargeOcrStatus>;
  dismiss: () => Promise<LargeOcrStatus>;
  relocate: (documentId: number | null, revision: number | null) => Promise<LargeOcrStatus | null>;
};

const defaultApi: LargeOcrApi = { status: () => largeOcrStatus(), pause: () => pauseLargeOcr(), cancel: () => cancelLargeOcr(), resume: (documentId, revision) => resumeLargeOcr(documentId, revision), dismiss: () => dismissLargeOcr(), relocate: (documentId, revision) => relocateLargeOcr(documentId, revision) };

const issueText: Record<LargeOcrPageReport['issue'], string> = {
  none: 'reduced resolution',
  timedOut: 'timed out',
  engineStopped: 'the OCR engine stopped',
  engineOutputRefused: 'the OCR result could not be used',
  renderFailed: 'could not be displayed and is blank in the copy',
  tooManyCharacters: 'some words had characters past the 8,192-character limit',
  internalError: 'an internal error; the page is blank in the copy',
};

export function timeLeft(seconds: number | null): string {
  if (seconds === null) return '';
  if (seconds < 60) return 'less than a minute left';
  const minutes = Math.round(seconds / 60);
  if (minutes < 90) return `about ${minutes} minute${minutes === 1 ? '' : 's'} left`;
  return `about ${Math.round(minutes / 6) / 10} hours left`;
}

export function pauseText(reason: LargeOcrPauseReason | null, job: LargeOcrJob): string {
  const at = `page ${job.progress.pagesDone.toLocaleString()} of ${job.progress.pageCount.toLocaleString()}`;
  if (!reason || reason.kind === 'user') return job.interrupted
    ? `OCR of ${job.sourceName} stopped at ${at} when PDF Workstation closed. Open the same document to resume.`
    : `Paused at ${at}. Progress is saved; you can resume now or after restarting PDF Workstation.`;
  if (reason.kind === 'sizeLimit') return `OCR paused at ${at}: the copy would be larger than 1 GiB, the largest file PDF Workstation can open. Saving in parts is not available yet; discard the progress or OCR a smaller document.`;
  if (reason.kind === 'engineFailing') return `OCR paused at ${at}: ${reason.message}. Resume tries again from the next page.`;
  if (reason.kind === 'document') return `OCR paused at ${at}: ${reason.message} Undo the change or reopen the document, then resume.`;
  if (reason.kind === 'disk') return `OCR paused at ${at}: the copy could not be written (${reason.message}). Free some space, then resume.`;
  return `OCR paused: ${reason.message}`;
}

function reportText(report: LargeOcrPageReport[]): string {
  return report.slice(0, 20).map(entry => `page ${entry.page + 1} (${issueText[entry.issue]})`).join(', ') + (report.length > 20 ? `, and ${report.length - 20} more` : '');
}

export default function LargeOcrPanel({ document, refresh, openCopy, api = defaultApi }: { document: { id: number; revision: number } | null; refresh: number; openCopy: (path: string) => void; api?: LargeOcrApi }) {
  const [status, setStatus] = useState<LargeOcrStatus | null>(null);
  const [error, setError] = useState('');
  const [confirming, setConfirming] = useState(false);
  const [announced, setAnnounced] = useState('');
  const lastDecile = useRef(-1);
  const poll = useCallback(async () => {
    try { setStatus(await api.status()); } catch (reason) { setError(reason instanceof Error ? reason.message : String(reason)); }
  }, [api]);
  const running = status?.job?.running ?? false;
  useEffect(() => {
    void poll();
    const timer = setInterval(() => void poll(), running ? 500 : 2000);
    return () => clearInterval(timer);
  }, [poll, refresh, running]);
  const job = status?.job ?? null;
  useEffect(() => {
    if (!job) return;
    const decile = job.progress.pageCount ? Math.floor(job.progress.pagesDone * 10 / job.progress.pageCount) : 0;
    if (decile !== lastDecile.current || !job.running) {
      lastDecile.current = decile;
      setAnnounced(job.running ? `Searchable OCR ${decile * 10}% done.` : job.progress.state === 'published' ? 'Searchable OCR copy saved.' : job.progress.state === 'paused' ? 'Searchable OCR paused.' : '');
    }
  }, [job]);
  if (!job) return null;
  const act = async (action: () => Promise<LargeOcrStatus>) => {
    setError('');
    try { setStatus(await action()); } catch (reason) { setError(reason instanceof Error ? reason.message : String(reason)); }
  };
  const progress = job.progress;
  const done = `Page ${Math.min(progress.pagesDone + (job.running ? 1 : 0), progress.pageCount).toLocaleString()} of ${progress.pageCount.toLocaleString()}`;
  const unreadable = job.report.filter(entry => entry.outcome !== 'text');
  const reduced = job.report.filter(entry => entry.outcome === 'text' && entry.reducedResolution);
  let body: JSX.Element;
  if (job.running) {
    body = <>
      <p>Searchable OCR · <strong>{job.sourceName}</strong> — {progress.state === 'finalising' ? 'Checking the copy…' : <>{done}{progress.secondsLeft !== null && <> · {timeLeft(progress.secondsLeft)}</>}</>}</p>
      <progress max={progress.pageCount} value={progress.pagesDone} aria-label="Searchable OCR progress" />
      {progress.issues > 0 && <p>{progress.issues} page{progress.issues === 1 ? '' : 's'} could not be fully read so far.</p>}
      {progress.sizeWarning && <p role="alert">The copy is on course to be larger than 1 GiB, the largest file PDF Workstation can open. OCR will pause if it reaches that size.</p>}
      {confirming
        ? <div className={s.actions}><span>Cancel OCR? The {progress.pagesDone.toLocaleString()} finished pages will be discarded and no file will be saved. The source document is unchanged.</span><button onClick={() => setConfirming(false)}>Keep running</button><button onClick={() => { setConfirming(false); void act(api.cancel); }}>Cancel OCR</button></div>
        : <div className={s.actions}><button onClick={() => void act(api.pause)}>Pause</button><button onClick={() => setConfirming(true)}>Cancel</button></div>}
    </>;
  } else if (progress.state === 'published') {
    body = <>
      <p>Saved <strong>{job.destination}</strong> ({progress.pageCount.toLocaleString()} pages).</p>
      {unreadable.length > 0 && <p>{(progress.pageCount - unreadable.length).toLocaleString()} of {progress.pageCount.toLocaleString()} pages have searchable text. Not read: {reportText(unreadable)}.</p>}
      {reduced.length > 0 && <p>Rendered at reduced resolution: {reportText(reduced)}.</p>}
      <div className={s.actions}><button onClick={() => openCopy(job.destination)}>Open copy</button><button onClick={() => void act(api.dismiss)}>Close</button></div>
    </>;
  } else if (progress.state === 'cancelled') {
    body = <><p>OCR cancelled. No file was saved; the source document is unchanged.</p><div className={s.actions}><button onClick={() => void act(api.dismiss)}>Close</button></div></>;
  } else {
    body = <>
      <p>{pauseText(job.pauseReason, job)}</p>
      <p>The copy will be saved as <strong>{job.destination}</strong>.</p>
      <div className={s.actions}>
        <button disabled={!document} title={document ? undefined : 'Open the document this OCR job was started from.'} onClick={() => document && void act(() => api.resume(document.id, document.revision))}>Resume</button>
        <button onClick={() => void act(async () => (await api.relocate(document?.id ?? null, document?.revision ?? null)) ?? await api.status())}>Save elsewhere…</button>
        <button onClick={() => void act(api.cancel)}>Discard progress</button>
      </div>
    </>;
  }
  return <section className={s.panel} aria-label="Searchable OCR">
    {body}
    {error && <p role="alert">{error}</p>}
    <p role="status" className={s.live}>{announced}</p>
  </section>;
}
