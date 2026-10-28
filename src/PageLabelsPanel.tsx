import { useCallback, useEffect, useRef, useState, type FormEvent } from 'react';
import { editPageLabels, type PageLabelEditResult, type PageLabelOp, type PageLabelRange, type PageLabelStyle } from './bridge';
import type { DocumentInfo } from './model';
import s from './SearchPanel.module.css';
import e from './PageLabelsPanel.module.css';

const STYLES: { value: PageLabelStyle | 'none'; text: string }[] = [
  { value: 'decimal', text: 'Numbers (1, 2, 3)' },
  { value: 'roman_lower', text: 'Roman lowercase (i, ii, iii)' },
  { value: 'roman_upper', text: 'Roman uppercase (I, II, III)' },
  { value: 'alpha_lower', text: 'Letters lowercase (a, b, c)' },
  { value: 'alpha_upper', text: 'Letters uppercase (A, B, C)' },
  { value: 'none', text: 'No number (prefix only)' },
];
const shown = (label: string) => label.length === 0 ? '(blank)' : label;
export const describeRange = (range: PageLabelRange) => {
  const pages = range.start_page === range.end_page ? `Page ${range.start_page + 1}` : `Pages ${range.start_page + 1} to ${range.end_page + 1}`;
  const labels = range.first_label === range.last_label ? `label ${shown(range.first_label)}` : `labels ${shown(range.first_label)} to ${shown(range.last_label)}`;
  return `${pages}: ${labels}`;
};

type Form = { start: string; style: PageLabelStyle | 'none'; prefix: string; first: string };

export default function PageLabelsPanel({ document, page = 0, close, onDocument, onChanged }: { document: DocumentInfo; page?: number; close: () => void; onDocument?: (info: DocumentInfo) => void; onChanged?: () => void }) {
  const [edit, setEdit] = useState<PageLabelEditResult | null>(null);
  const [error, setError] = useState('');
  const [status, setStatus] = useState('');
  const [busy, setBusy] = useState(false);
  const [form, setForm] = useState<Form>({ start: String(page + 1), style: 'decimal', prefix: '', first: '1' });
  const working = useRef(false);
  const mounted = useRef(true);
  useEffect(() => { mounted.current = true; return () => { mounted.current = false; }; }, []);

  useEffect(() => {
    let disposed = false;
    editPageLabels(document.id, document.revision, null).then(result => {
      if (disposed) return;
      setEdit(result); setError(result.view.editable ? '' : result.view.reason ?? 'These page labels cannot be edited.');
    }).catch(reason => { if (!disposed) setError(String(reason)); });
    return () => { disposed = true; };
  }, [document.id, document.revision]);

  const run = useCallback(async (op: PageLabelOp, message: string) => {
    if (!edit || working.current) return;
    working.current = true; setBusy(true); setStatus(''); setError('');
    try {
      const result = await editPageLabels(document.id, document.revision, { version: edit.view.version, op });
      if (!mounted.current) return;
      setEdit(result); onDocument?.(result.document); onChanged?.(); setStatus(message);
    } catch (reason) { if (mounted.current) setError(String(reason)); }
    finally { working.current = false; if (mounted.current) setBusy(false); }
  }, [edit, document.id, document.revision, onDocument, onChanged]);

  const pages = document.pages.length;
  const submit = (event: FormEvent) => {
    event.preventDefault();
    const start = Number(form.start), first = Number(form.first);
    if (!Number.isInteger(start) || start < 1 || start > pages) { setError(`Enter a page number from 1 to ${pages}.`); return; }
    const numbered = form.style !== 'none';
    if (numbered && (!Number.isInteger(first) || first < 1 || first > 1_000_000_000)) { setError('Enter a first number of 1 or more.'); return; }
    if (!numbered && form.prefix.length === 0) { setError('Choose a numbering style or enter a prefix.'); return; }
    void run({ kind: 'set', start_page: start - 1, style: numbered ? form.style as PageLabelStyle : null, prefix: form.prefix, first: numbered ? first : 1 }, `Labelled pages from page ${start} until the next range.`);
  };
  const load = (range: PageLabelRange) => setForm({ start: String(range.start_page + 1), style: range.style ?? 'none', prefix: range.prefix, first: String(range.first) });
  const ranges = edit?.view.ranges ?? [];
  const editable = !!edit?.view.editable;
  const replacing = ranges.some(range => range.start_page === Number(form.start) - 1);
  const toolbarButton = (label: string, text: string, disabled: boolean, onClick: () => void) =>
    <button type="button" aria-label={label} title={label} disabled={disabled || busy} onClick={onClick}>{text}</button>;

  return <aside className={s.panel} aria-label="Page labels">
    <div className={s.heading}><h2>Page labels</h2><button aria-label="Close page labels" onClick={close}>×</button></div>
    {error && <p role="alert">{error}</p>}
    {!edit && !error && <p role="status">Loading page labels…</p>}
    {editable && <>
      <p className={e.note} role="note">Label changes stay in this open document until you save a copy. They are not part of recovery points, and the original file is never changed. Navigation still uses the physical page numbers.</p>
      {!ranges.length && <p>This document has no page labels. Pages are labelled 1, 2, 3 and so on.</p>}
      <ul className={e.ranges} aria-label="Page label ranges">{ranges.map(range => <li key={range.start_page}>
        <p>{describeRange(range)}</p>
        <div className={e.rowActions}>
          <button type="button" disabled={busy} aria-label={`Edit the range starting at page ${range.start_page + 1}`} onClick={() => load(range)}>Edit</button>
          <button type="button" disabled={busy} aria-label={`Delete the range starting at page ${range.start_page + 1}`} onClick={() => void run({ kind: 'delete', start_page: range.start_page }, range.start_page === 0 ? 'Reset the first pages to plain numbers.' : `Removed the range that started at page ${range.start_page + 1}.`)}>Delete</button>
        </div>
      </li>)}</ul>
      <form className={e.form} aria-label="Label pages" onSubmit={submit}>
        <label>Start at page<input type="number" min={1} max={pages} value={form.start} onChange={event => setForm({ ...form, start: event.target.value })} /></label>
        <label>Numbering<select value={form.style} onChange={event => setForm({ ...form, style: event.target.value as Form['style'] })}>{STYLES.map(style => <option key={style.value} value={style.value}>{style.text}</option>)}</select></label>
        <label>Prefix (optional)<input type="text" value={form.prefix} onChange={event => setForm({ ...form, prefix: event.target.value })} /></label>
        <label>First number<input type="number" min={1} disabled={form.style === 'none'} value={form.first} onChange={event => setForm({ ...form, first: event.target.value })} /></label>
        <p className={e.note}>The labels apply from this page until the next range starts.</p>
        <div className={s.actions}><button type="submit" disabled={busy}>{replacing ? 'Replace range' : 'Add range'}</button></div>
      </form>
      <div className={e.toolbar} role="toolbar" aria-label="Page label history">
        {toolbarButton('Undo page label change', 'Undo', !edit?.view.can_undo, () => void run({ kind: 'undo' }, 'Undid the last page label change.'))}
        {toolbarButton('Redo page label change', 'Redo', !edit?.view.can_redo, () => void run({ kind: 'redo' }, 'Redid the page label change.'))}
      </div>
      {status && <p role="status">{status}</p>}
    </>}
  </aside>;
}
