import { useEffect, useRef, useState } from 'react';
import type { SplitOutput } from './bridge';
import s from './SplitDialog.module.css';

const MAX_OUTPUT_FILES = 64;
const defaultPagesPerFile = (pageCount: number) => Math.max(1, Math.ceil(pageCount / MAX_OUTPUT_FILES));

export default function SplitDialog({ pageCount, busy, split, close }: { pageCount: number; busy: boolean; split: (pagesPerFile: number) => Promise<SplitOutput | null>; close: () => void }) {
  const dialog = useRef<HTMLDialogElement>(null);
  const inFlight = useRef(false);
  const [pagesPerFile, setPagesPerFile] = useState(String(defaultPagesPerFile(pageCount)));
  const [error, setError] = useState('');
  const [result, setResult] = useState<SplitOutput | null>(null);
  const [submitting, setSubmitting] = useState(false);
  useEffect(() => { dialog.current?.showModal(); }, []);
  const validate = () => {
    const value = pagesPerFile.trim();
    const number = Number(value);
    if (!/^\d+$/.test(value) || !Number.isSafeInteger(number) || number < 1) return 'Enter a whole number of pages per file.';
    const outputs = Math.ceil(pageCount / number);
    if (outputs > MAX_OUTPUT_FILES) return `Choose at least ${defaultPagesPerFile(pageCount)} pages per file to create no more than ${MAX_OUTPUT_FILES} files.`;
    return null;
  };
  const submit = async () => {
    if (busy || submitting || inFlight.current || result) return;
    const problem = validate();
    if (problem) { setError(problem); return; }
    inFlight.current = true; setSubmitting(true); setError('');
    try {
      const output = await split(Number(pagesPerFile));
      if (output === null) close(); else setResult(output);
    } catch (reason) { setError(String(reason)); }
    finally { inFlight.current = false; setSubmitting(false); }
  };
  const outputCount = /^\d+$/.test(pagesPerFile.trim()) && Number(pagesPerFile) > 0 && Number.isSafeInteger(Number(pagesPerFile)) ? Math.ceil(pageCount / Number(pagesPerFile)) : null;
  const working = busy || submitting;
  const folder = result?.folder.startsWith('\\\\?\\') ? result.folder.slice(4) : result?.folder;
  return <dialog ref={dialog} className={s.dialog} aria-labelledby="split-title" onCancel={event => { event.preventDefault(); if (!working) close(); }}>
    {result ? <><h2 id="split-title">Split complete</h2><p>Created {result.files.length} {result.files.length === 1 ? 'file' : 'files'} in {folder}.</p><div className={s.actions}><button autoFocus onClick={close}>Close</button></div></> : <><h2 id="split-title">Split PDF</h2><p>Each file uses the current edited page order. Windows will ask you to choose a folder; the files go into a new Split PDFs folder inside it.</p><label>Pages per file <input aria-label="Pages per split file" inputMode="numeric" value={pagesPerFile} disabled={working} onChange={event => setPagesPerFile(event.target.value)} onKeyDown={event => { if (event.key === 'Enter') { event.preventDefault(); void submit(); } }} /></label>
      <p className={s.preview}>{outputCount === null ? `Choose a whole number to preview up to ${MAX_OUTPUT_FILES} files.` : `Creates ${outputCount} ${outputCount === 1 ? 'file' : 'files'} from ${pageCount} current pages.`}</p>
      {error && <p role="alert">{error}</p>}
      {working && <p role="status">Choosing a folder and validating split files…</p>}
      <div className={s.actions}><button disabled={working} onClick={close}>Cancel</button><button className={s.confirm} disabled={working} onClick={() => void submit()}>Split PDF</button></div></>}
  </dialog>;
}
