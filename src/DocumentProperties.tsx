import { useEffect, useRef, useState } from 'react';
import { documentProperties, ownerUnlockDocument, type DocumentPropertiesInfo } from './bridge';
import type { DocumentInfo } from './model';
import s from './DocumentProperties.module.css';

const metadataLabels: Record<string, string> = { title: 'Title', author: 'Author', subject: 'Subject', keywords: 'Keywords', creator: 'Created with', producer: 'PDF producer', creation_date_raw: 'Created (as stored)', modification_date_raw: 'Modified (as stored)' };
const permissionLabels = { print_high_quality: 'High-quality printing', print_low_quality_only: 'Low-quality printing only', modify_contents: 'Modify contents', assemble_document: 'Assemble pages', fill_existing_forms: 'Fill existing forms' };

const MAX_PASSWORD_BYTES = 1024;
const TOO_LONG = 'Passwords cannot be longer than 1 KiB.';

export default function DocumentProperties({ document, close }: { document: DocumentInfo; close: () => void }) {
  const dialog = useRef<HTMLDialogElement>(null);
  const alive = useRef(true), submitting = useRef(false);
  const [info, setInfo] = useState<DocumentPropertiesInfo | null>(null);
  const [error, setError] = useState('');
  const [reload, setReload] = useState(0);
  const [unlocking, setUnlocking] = useState(false);
  const [password, setPassword] = useState('');
  const [unlockError, setUnlockError] = useState('');
  const [unlockNotice, setUnlockNotice] = useState('');
  const [busy, setBusy] = useState(false);
  useEffect(() => {
    dialog.current?.showModal();
    let disposed = false;
    setInfo(null); setError('');
    documentProperties(document.id, document.revision).then(value => { if (!disposed) setInfo(value); }).catch(reason => { if (!disposed) setError(String(reason)); });
    return () => { disposed = true; };
  }, [document.id, document.revision, reload]);
  useEffect(() => { alive.current = true; return () => { alive.current = false; }; }, []);
  async function unlock() {
    if (submitting.current || !password) return;
    submitting.current = true; setBusy(true); setUnlockError(''); setUnlockNotice('');
    const entered = password; setPassword('');
    try {
      await ownerUnlockDocument(document.id, document.revision, entered);
      if (!alive.current) return;
      setUnlocking(false); setUnlockNotice('Security removed for this session. This PDF is no longer restricted, and Save a Copy writes it without a password or restrictions. The file on disk is unchanged. Recovery points and OCR are off for this unlocked document so its pages are not written to temporary storage: save a copy to keep edits, and OCR that copy.'); setReload(count => count + 1);
    } catch (reason) {
      if (alive.current) setUnlockError(String(reason));
    } finally {
      submitting.current = false;
      if (alive.current) setBusy(false);
    }
  }
  return <dialog ref={dialog} className={s.dialog} aria-labelledby="properties-title" onCancel={event => { event.preventDefault(); close(); }}>
    <h2 id="properties-title">Document properties</h2>
    <p className={s.name}>{document.name}</p>
    {error ? <p role="alert">{error}</p> : info === null ? <p role="status">Reading document properties…</p> : <>
      <dl><dt>Location</dt><dd>{document.path}</dd><dt>Original file size</dt><dd>{info.source_size_bytes.toLocaleString()} bytes</dd><dt>PDF version</dt><dd>{info.version ?? 'Unknown'}</dd><dt>Current pages</dt><dd>{info.page_count.toLocaleString()}</dd><dt>Unsaved page edits</dt><dd>{document.dirty ? 'Yes' : 'No'}</dd></dl>
      <h3>Description</h3>
      {info.metadata.length ? <dl>{info.metadata.map(entry => <div className={s.row} key={entry.name}><dt>{metadataLabels[entry.name] ?? entry.name}</dt><dd>{entry.value}{entry.truncated && <span className={s.note}> (truncated)</span>}</dd></div>)}</dl> : <p>No description metadata was found.</p>}
      <h3>Page sizes</h3>
      <p>Current page sizes in points (72 points = 1 inch), including rotations.</p>
      <ul>{info.page_dimensions.map((size, index) => <li key={index}>{size.width_points.toLocaleString(undefined, { maximumFractionDigits: 2 })} × {size.height_points.toLocaleString(undefined, { maximumFractionDigits: 2 })} pt — {size.count} {size.count === 1 ? 'page' : 'pages'}</li>)}</ul>
      {info.page_dimensions_truncated && <p>Only the first 128 distinct page sizes are shown.</p>}
      <h3>Security</h3>
      <dl><dt>Encrypted</dt><dd>{info.security.encrypted === null ? 'Unknown' : info.security.encrypted ? 'Yes' : 'No'}</dd><dt>Security handler revision</dt><dd>{info.security.handler_revision ?? 'Not available'}</dd>{Object.entries(permissionLabels).map(([key, label]) => { const value = info.security[key as keyof typeof permissionLabels]; return <div className={s.row} key={key}><dt>{label}</dt><dd>{value === null ? 'Unknown' : value ? 'Yes' : 'No'}</dd></div>; })}<dt>Reported signatures</dt><dd>{info.reported_signature_count ?? 'Unknown'}</dd></dl>
      <p>Signature validity has not been checked. Reported permissions describe the PDF; available app tools may be more limited. Encrypted files are read-only until you unlock them with the owner password.</p>
      {unlockNotice && <p role="status">{unlockNotice}</p>}
      {info.security.encrypted !== false && (unlocking
        ? <form className={s.unlock} onSubmit={event => { event.preventDefault(); void unlock(); }}>
            <label>Owner (permissions) password<input autoFocus type="password" autoComplete="off" value={password} disabled={busy} maxLength={MAX_PASSWORD_BYTES} onChange={event => { const next = event.target.value; if (new TextEncoder().encode(next).length <= MAX_PASSWORD_BYTES) { setPassword(next); setUnlockError(''); } else setUnlockError(TOO_LONG); }} /></label>
            <p>This is the password that controls printing, copying and editing, not the password that opens the file. It is used once to check it, then discarded: never saved, logged or stored for recovery. Nothing changes unless it is correct.</p>
            {unlockError && <p role="alert">{unlockError}</p>}
            <div className={s.unlockActions}><button type="button" disabled={busy} onClick={() => { setPassword(''); setUnlockError(''); setUnlocking(false); }}>Cancel</button><button type="submit" disabled={busy || !password}>{busy ? 'Checking…' : 'Remove security'}</button></div>
          </form>
        : <p><button type="button" onClick={() => { setUnlockNotice(''); setUnlocking(true); }}>Unlock with owner password…</button></p>)}
      <p>Metadata and file size describe the original file opened in this session. Page counts and sizes include current edits.</p>
    </>}
    <div className={s.actions}><button autoFocus onClick={close}>Close</button></div>
  </dialog>;
}
