import { useEffect, useRef, useState } from 'react';
import { getVersion } from '@tauri-apps/api/app';
import { checkForUpdate, type Update } from './updateCommands';
import { native } from './bridge';
import { clearUpdateAttempt, readUpdateAttempt, writeUpdateAttempt } from './updateAttempt';
import s from './Updates.module.css';

export default function Updates({ dirty, busy, setBusy, close }: { dirty: boolean; busy: boolean; setBusy: (value: boolean) => void; close: () => void }) {
  const dialog = useRef<HTMLDialogElement>(null);
  const pending = useRef<Update | null>(null);
  const downloadedOffer = useRef<Update | null>(null);
  const [version, setVersion] = useState('');
  const [available, setAvailable] = useState<Update | null>(null);
  const [checking, setChecking] = useState(true);
  const [installing, setInstalling] = useState(false);
  const [status, setStatus] = useState('Checking GitHub releases…');
  const [recovery, setRecovery] = useState('');
  const [recoveryBlocked, setRecoveryBlocked] = useState(false);
  const [error, setError] = useState('');
  useEffect(() => {
    dialog.current?.showModal();
    let disposed = false;
    void (async () => {
      try {
        if (!native) throw new Error('Updates are available in the installed Windows app.');
        const current = await getVersion();
        if (!disposed) {
          setVersion(current);
          const attempt = await readUpdateAttempt();
          if (attempt?.targetVersion === current) await clearUpdateAttempt();
          else if (attempt?.fromVersion === current) {
            if (attempt.phase === 'downloading') setRecovery('The previous update download was interrupted. You can retry.');
            else {
              setRecoveryBlocked(true);
              setRecovery('An update installer was started, but this version is still running. Close PDF Workstation, wait for the installer to finish, then reopen the app. If this version still opens, run the latest signed installer manually.');
            }
          } else if (attempt) await clearUpdateAttempt();
        }
        const update = await checkForUpdate();
        if (disposed) { await update?.close(); return; }
        pending.current = update; setAvailable(update);
        setStatus(update ? `Version ${update.version} is available.` : 'You have the latest released version.');
      } catch (e) { if (!disposed) { setError(`Could not check for updates: ${String(e)}`); setStatus('Close and try again when you are online.'); } }
      finally { if (!disposed) setChecking(false); }
    })();
    return () => { disposed = true; const release = pending.current?.close(); if (release) void release.catch(() => {}); };
  }, []);
  const install = async () => {
    if (!available || dirty || busy || installing || recoveryBlocked) return;
    const alreadyDownloaded = downloadedOffer.current === available;
    setInstalling(true); setBusy(true); setError(''); setRecovery(''); setStatus(alreadyDownloaded ? 'Starting installer…' : 'Downloading signed update…');
    let downloaded = 0, total = 0;
    let downloadComplete = alreadyDownloaded;
    try {
      if (!alreadyDownloaded) {
        try {
          await writeUpdateAttempt({ schemaVersion: 1, fromVersion: version, targetVersion: available.version, phase: 'downloading' });
        } catch (reason) {
          throw new Error(`Update recovery state could not be saved. The update was not started. You can retry. ${String(reason)}`);
        }
        await available.download(event => {
          if (event.event === 'Started') total = event.data.contentLength || 0;
          if (event.event === 'Progress') {
            downloaded += event.data.chunkLength;
            setStatus(total ? `Downloading update: ${Math.min(100, Math.round(downloaded / total * 100))}%` : `Downloaded ${(downloaded / 1048576).toFixed(1)} MB`);
          }
          if (event.event === 'Finished') setStatus('Verifying update and starting installer…');
        });
        downloadComplete = true;
        downloadedOffer.current = available;
      }
      try {
        await writeUpdateAttempt({ schemaVersion: 1, fromVersion: version, targetVersion: available.version, phase: 'installing' });
      } catch (reason) {
        throw new Error(`Update recovery state could not be saved. The installer was not started. You can retry. ${String(reason)}`);
      }
      setStatus(`Waiting for you to confirm installing version ${available.version}…`);
      if (await available.install() === 'cancelled') {
        try { await clearUpdateAttempt(); } catch {}
        setStatus(`Version ${available.version} was not installed. Your installed version is unchanged. You can install it later.`);
        return;
      }
      throw new Error('Installer returned without closing the app.');
    } catch (e) {
      try { await clearUpdateAttempt(); } catch {}
      setError(`Update failed: ${String(e)}`);
      if (!downloadComplete) setStatus('Your installed version is unchanged. You can retry.');
      else {
        pending.current = null; downloadedOffer.current = null; setAvailable(null);
        try {
          await available.close();
        } catch {
          setStatus('The downloaded update could not be released safely. Close and reopen PDF Workstation to retry.');
          return;
        }
        try {
          const refreshed = await checkForUpdate();
          pending.current = refreshed; setAvailable(refreshed);
          setStatus(refreshed ? 'Your installed version is unchanged. You can retry.' : 'You have the latest released version.');
        } catch {
          setStatus('The updater could not refresh safely. Close and reopen Software updates to retry.');
        }
      }
    }
    finally { setInstalling(false); setBusy(false); }
  };
  return <dialog ref={dialog} className={s.dialog} aria-labelledby="updates-title" onCancel={e => { e.preventDefault(); if (!installing) close(); }}>
    <h2 id="updates-title">Software updates</h2>
    {version && <p>Installed version: {version}</p>}
    <p role="status" aria-live="polite">{status}</p>
    {recovery && <p>{recovery}</p>}
    {error && <p role="alert" className={s.error}>{error}</p>}
    {available?.body && <pre className={s.notes}>{available.body}</pre>}
    {available && <p>The app will close and reopen during installation. {dirty ? 'Save a copy of every edited document or discard those edits before updating.' : 'Save your work before continuing.'}</p>}
    <div className={s.actions}><button autoFocus disabled={installing} onClick={close}>Close</button>{available && <button disabled={checking || installing || dirty || busy || recoveryBlocked} onClick={() => void install()}>Install update and restart</button>}</div>
  </dialog>;
}
