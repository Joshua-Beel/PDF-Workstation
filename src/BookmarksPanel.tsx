import { useCallback, useEffect, useRef, useState, type FormEvent } from 'react';
import { documentBookmarks, editBookmarks, type BookmarkEditEntry, type BookmarkEditResult, type BookmarkList, type BookmarkOp } from './bridge';
import type { BookmarkDestination } from './bookmarkDestination';
import type { DocumentInfo } from './model';
import s from './SearchPanel.module.css';
import e from './BookmarksPanel.module.css';

type Form = { kind: 'add' | 'rename'; title: string };

export function previousSibling(entries: BookmarkEditEntry[], index: number): number | null {
  const depth = entries[index].depth;
  for (let cursor = index - 1; cursor >= 0; cursor--) {
    if (entries[cursor].depth === depth) return cursor;
    if (entries[cursor].depth < depth) return null;
  }
  return null;
}
export function subtreeEnd(entries: BookmarkEditEntry[], index: number): number {
  let end = index + 1;
  while (end < entries.length && entries[end].depth > entries[index].depth) end++;
  return end;
}
const pageText = (page: number | null) => page === null ? 'No page' : `Page ${page + 1}`;

export default function BookmarksPanel({ document, page = 0, go, close, onDocument }: { document: DocumentInfo; page?: number; go: (page: number, destination?: BookmarkDestination | null) => void; close: () => void; onDocument?: (info: DocumentInfo) => void }) {
  const [bookmarks, setBookmarks] = useState<BookmarkList | null>(null);
  const [error, setError] = useState('');
  const [reload, setReload] = useState(0);
  const [edit, setEdit] = useState<BookmarkEditResult | null>(null);
  const [selected, setSelected] = useState<number | null>(null);
  const [form, setForm] = useState<Form | null>(null);
  const [status, setStatus] = useState('');
  const [busy, setBusy] = useState(false);
  const working = useRef(false);
  const mounted = useRef(true);
  useEffect(() => { mounted.current = true; return () => { mounted.current = false; }; }, []);

  const begin = useCallback(async () => {
    if (working.current) return;
    working.current = true; setBusy(true); setStatus('');
    try {
      const result = await editBookmarks(document.id, document.revision, null);
      if (!mounted.current) return;
      if (!result.view.editable) { setError(result.view.reason ?? 'These bookmarks cannot be edited.'); return; }
      setError(''); setEdit(result);
    } catch (reason) { if (mounted.current) setError(String(reason)); }
    finally { working.current = false; if (mounted.current) setBusy(false); }
  }, [document.id, document.revision]);

  useEffect(() => {
    let disposed = false;
    documentBookmarks(document.id, document.revision).then(value => {
      if (disposed) return;
      setBookmarks(value);
      if (value.edited && reload === 0) void begin();
    }).catch(reason => { if (!disposed) setError(String(reason)); });
    return () => { disposed = true; };
  }, [document.id, document.revision, reload, begin]);

  const run = async (op: BookmarkOp, done?: (previous: BookmarkEditResult, next: BookmarkEditResult) => void) => {
    if (!edit || working.current) return;
    working.current = true; setBusy(true); setStatus(''); setError('');
    try {
      const result = await editBookmarks(document.id, document.revision, { version: edit.view.version, op });
      if (!mounted.current) return;
      setEdit(result); onDocument?.(result.document); done?.(edit, result);
    } catch (reason) { if (mounted.current) setError(String(reason)); }
    finally { working.current = false; if (mounted.current) setBusy(false); }
  };

  const finish = () => { setEdit(null); setForm(null); setSelected(null); setStatus(''); setReload(value => value + 1); };
  const entries = edit?.view.entries ?? [];
  const index = selected === null ? -1 : entries.findIndex(entry => entry.uid === selected);
  const current = index >= 0 ? entries[index] : null;
  const canUp = index >= 0 && previousSibling(entries, index) !== null;
  const end = index >= 0 ? subtreeEnd(entries, index) : 0;
  const canDown = index >= 0 && end < entries.length && entries[end].depth === entries[index].depth;
  const canIndent = canUp;
  const canOutdent = index >= 0 && entries[index].depth > 0;

  const submit = (event: FormEvent) => {
    event.preventDefault();
    if (!form) return;
    const title = form.title.trim();
    if (!title) { setError('Enter a bookmark title.'); return; }
    if (form.kind === 'add') {
      void run({ kind: 'add', page, title, after: selected }, (previous, next) => {
        const known = new Set(previous.view.entries.map(entry => entry.uid));
        const created = next.view.entries.find(entry => !known.has(entry.uid));
        if (created) setSelected(created.uid);
        setStatus(`Added bookmark “${title}” on page ${page + 1}.`);
      });
    } else if (current) {
      void run({ kind: 'rename', uid: current.uid, title }, () => setStatus(`Renamed bookmark to “${title}”.`));
    }
    setForm(null);
  };
  const remove = () => {
    if (!current) return;
    const count = end - index;
    void run({ kind: 'delete', uid: current.uid }, () => {
      setSelected(null);
      setStatus(count > 1 ? `Deleted “${current.title}” and ${count - 1} nested ${count === 2 ? 'bookmark' : 'bookmarks'}. Undo is available.` : `Deleted “${current.title}”. Undo is available.`);
    });
  };
  const simple = (kind: 'move_up' | 'move_down' | 'indent' | 'outdent', message: string) => { if (current) void run({ kind, uid: current.uid }, () => setStatus(message)); };

  const toolbarButton = (label: string, text: string, disabled: boolean, onClick: () => void, pressed?: boolean) =>
    <button type="button" aria-label={label} title={label} disabled={disabled || busy} aria-pressed={pressed} onClick={onClick}>{text}</button>;

  return <aside className={s.panel} aria-label="Document bookmarks">
    <div className={s.heading}><h2>Bookmarks</h2><button aria-label="Close bookmarks" onClick={close}>×</button></div>
    {error && <p role="alert">{error}</p>}
    {edit ? <>
      <p role="note">Bookmark changes stay in this open document until you save a copy. They are not part of recovery points, and the original file is never changed.</p>
      <div className={e.toolbar} role="toolbar" aria-label="Bookmark editing">
        {toolbarButton('Add bookmark at current page', 'Add', false, () => setForm({ kind: 'add', title: `Page ${page + 1}` }))}
        {toolbarButton('Rename bookmark', 'Rename', !current, () => current && setForm({ kind: 'rename', title: current.title }))}
        {toolbarButton('Delete bookmark and its nested bookmarks', 'Delete', !current, remove)}
        {toolbarButton('Move bookmark up', 'Up', !canUp, () => simple('move_up', 'Moved bookmark up.'))}
        {toolbarButton('Move bookmark down', 'Down', !canDown, () => simple('move_down', 'Moved bookmark down.'))}
        {toolbarButton('Indent bookmark under the one above', 'Indent', !canIndent, () => simple('indent', 'Indented bookmark.'))}
        {toolbarButton('Outdent bookmark one level', 'Outdent', !canOutdent, () => simple('outdent', 'Outdented bookmark.'))}
        {toolbarButton('Show nested bookmarks when the document opens', 'Open', !current?.has_children, () => { if (current) void run({ kind: 'set_open', uid: current.uid, open: !current.open }); }, !!current?.open)}
        {toolbarButton('Undo bookmark change', 'Undo', !edit.view.can_undo, () => void run({ kind: 'undo' }, () => setStatus('Undid the last bookmark change.')))}
        {toolbarButton('Redo bookmark change', 'Redo', !edit.view.can_redo, () => void run({ kind: 'redo' }, () => setStatus('Redid the bookmark change.')))}
      </div>
      {form && <form aria-label={form.kind === 'add' ? 'New bookmark' : 'Rename bookmark'} onSubmit={submit} onKeyDown={event => { if (event.key === 'Escape') { event.stopPropagation(); setForm(null); } }}>
        <label>{form.kind === 'add' ? `New bookmark title (page ${page + 1})` : 'Bookmark title'}
          <input autoFocus type="text" maxLength={512} value={form.title} onChange={event => setForm({ ...form, title: event.target.value })} />
        </label>
        <div className={s.actions}><button type="submit" disabled={busy}>{form.kind === 'add' ? 'Add bookmark' : 'Rename'}</button><button type="button" onClick={() => setForm(null)}>Cancel</button></div>
      </form>}
      {status && <p role="status">{status}</p>}
      {!entries.length && <p>This document has no bookmarks. Use Add to bookmark the current page.</p>}
      <ul className={e.list} aria-label="Bookmark tree">{entries.map(entry => <li key={entry.uid} style={{ paddingLeft: Math.min(entry.depth, 10) * 12 }}>
        <button type="button" className={entry.uid === selected ? e.selected : undefined} aria-pressed={entry.uid === selected}
          aria-label={`${entry.title}, level ${entry.depth + 1}, ${pageText(entry.page)}${entry.has_children ? (entry.open ? ', open' : ', closed') : ''}${entry.added ? ', new' : ''}`}
          onClick={() => { setSelected(entry.uid); if (entry.page !== null) go(entry.page); }}>{entry.title}</button>
        <span className={e.page}>{pageText(entry.page)}</span>
      </li>)}</ul>
      <div className={s.actions}><button type="button" onClick={finish}>Done editing</button></div>
    </> : error && !bookmarks ? null : !bookmarks ? <p role="status">Loading bookmarks…</p> : <>
      {!bookmarks.items.length && <p>This document has no bookmarks.</p>}
      {bookmarks.truncated && <p role="status">Showing the first 1,000 bookmarks.</p>}
      {bookmarks.edited && <p role="status">This list shows the original bookmarks. Your unsaved bookmark edits are not shown here.</p>}
      <div className={s.actions}><button type="button" disabled={busy} onClick={() => void begin()}>Edit bookmarks</button></div>
      <div className={s.results}>{bookmarks.items.map((bookmark, position) => <section key={position} style={{ paddingLeft: Math.min(bookmark.depth, 8) * 12 }}>
        <button disabled={bookmark.page === null} onClick={() => { if (bookmark.page !== null) { if (bookmark.destination) go(bookmark.page, bookmark.destination); else go(bookmark.page); } }}>{bookmark.title}</button>
        <p>{bookmark.page === null ? 'No supported destination in this document.' : `Page ${bookmark.page + 1}`}</p>
      </section>)}</div>
    </>}
  </aside>;
}
