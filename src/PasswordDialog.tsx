import { useEffect, useRef, useState } from 'react';
import { cancelPasswordRequest, closeDocument, unlockDocument, type PasswordChallenge } from './bridge';
import type { DocumentInfo } from './model';
import s from './PasswordDialog.module.css';

const MAX_PASSWORD_BYTES = 1024;
const TOO_LONG = 'Passwords cannot be longer than 1 KiB.';
const REFUSED = /^[^\n]*Nothing was opened\.$/;

export default function PasswordDialog({ challenge, onOpened, onClose, onRefused }: { challenge: PasswordChallenge; onOpened: (document: DocumentInfo) => void; onClose: () => void; onRefused?: (message: string) => void }) {
  const dialog = useRef<HTMLDialogElement>(null);
  const active = useRef(true), submitting = useRef(false);
  const currentRequest = useRef(challenge.request_id);
  const [password, setPassword] = useState('');
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState(challenge.incorrect ? 'Incorrect password. Try again.' : '');
  useEffect(() => {
    active.current = true;
    currentRequest.current = challenge.request_id;
    dialog.current?.showModal();
    return () => {
      active.current = false;
      queueMicrotask(() => {
        if (!active.current || currentRequest.current !== challenge.request_id) void cancelPasswordRequest(challenge.request_id).catch(() => {});
      });
    };
  }, [challenge.request_id]);
  function cancel() {
    active.current = false; setPassword(''); onClose();
  }
  async function submit() {
    if (submitting.current) return;
    submitting.current = true; setBusy(true); setError('');
    const entered = password; setPassword('');
    try {
      const result = await unlockDocument(challenge.request_id, entered);
      if (!active.current) {
        if (result.status === 'opened') await closeDocument(result.document.id);
        return;
      }
      if (result.status === 'opened') onOpened(result.document);
      else setError('Incorrect password. Try again.');
    } catch (e) {
      if (active.current && REFUSED.test(String(e))) { active.current = false; setPassword(''); if (onRefused) onRefused(String(e)); else onClose(); return; }
      if (active.current) setError(String(e).includes(TOO_LONG) ? TOO_LONG : 'Unable to unlock this PDF. Cancel and open the file again.');
    } finally {
      submitting.current = false;
      if (active.current) setBusy(false);
    }
  }
  return <dialog ref={dialog} className={s.dialog} aria-labelledby="password-title" onCancel={event => { event.preventDefault(); cancel(); }}>
    <h2 id="password-title">Password required</h2>
    <p>{challenge.name}</p>
    <form onSubmit={event => { event.preventDefault(); void submit(); }}>
      <label>PDF password<input autoFocus type="password" autoComplete="off" value={password} disabled={busy} maxLength={MAX_PASSWORD_BYTES} onChange={event => { const next = event.target.value; if (new TextEncoder().encode(next).length <= MAX_PASSWORD_BYTES) { setPassword(next); setError(''); } else setError(TOO_LONG); }} /></label>
      <p>Your password is used only to open this document.</p>
      {error && <p role="alert">{error}</p>}
      <div className={s.actions}><button type="button" onClick={cancel}>Cancel</button><button type="submit" disabled={busy}>{busy ? 'Opening…' : 'Open PDF'}</button></div>
    </form>
  </dialog>;
}
