import { useEffect, useLayoutEffect, useRef, useState } from 'react';
import { ArrowDownToLine, ArrowLeft, ArrowRight, CheckSquare, Crop, Files, RotateCcw, RotateCw, Scissors, Trash2, Undo2, Redo2, X } from 'lucide-react';
import type { SplitOutput } from './bridge';
import type { DocumentInfo, PageEdit } from './model';
import { parsePageRange } from './model';
import { pageLabelDescription, pageLabelFor, type DocumentPageLabels } from './pageLabels';
import { renderPage } from './bridge';
import s from './Organizer.module.css';
import ConfirmDialog from './ConfirmDialog';
import SplitDialog from './SplitDialog';
import CropDialog, { type CropPage } from './CropDialog';
import type { CropInsets } from './bridge';
import { restoreModalFocus } from './modalFocus';

const clampPage = (page: number, pageCount: number) => Math.min(Math.max(page, 0), Math.max(pageCount - 1, 0));

function Thumbnail({ document, index }: { document: DocumentInfo; index: number }) {
  const element = useRef<HTMLDivElement>(null);
  const [visible, setVisible] = useState(false);
  const [url, setUrl] = useState('');
  const [error, setError] = useState('');
  useEffect(() => {
    const observer = new IntersectionObserver(([entry]) => setVisible(entry.isIntersecting), { rootMargin: '200px' });
    observer.observe(element.current!); return () => observer.disconnect();
  }, []);
  useEffect(() => {
    if (!visible) { setUrl(''); return; }
    let disposed = false, objectUrl = '';
    setUrl(''); setError('');
    renderPage(document.id, index, 260).then(value => {
      objectUrl = value; if (disposed) URL.revokeObjectURL(value); else setUrl(value);
    }).catch(e => { if (!disposed) setError(String(e)); });
    return () => { disposed = true; if (objectUrl) URL.revokeObjectURL(objectUrl); };
  }, [visible, document.id, document.revision, index]);
  const size = document.pages[index];
  return <div ref={element} className={s.thumbnail} style={{ aspectRatio: `${size.width}/${size.height}` }}>{url ? <img src={url} alt={`Page ${index + 1} preview`} draggable={false} /> : <span>{error ? 'Preview unavailable' : 'Loading…'}</span>}</div>;
}

export default function Organizer({ document, currentPage = 0, pageLabels = null, labelEditsPending = false, busy, edit, save, split, crop, resetCrop = async () => {}, insert = () => {}, replace = () => {}, close }: { document: DocumentInfo; currentPage?: number; pageLabels?: DocumentPageLabels | null; labelEditsPending?: boolean; busy: boolean; edit: (action: PageEdit) => Promise<boolean>; save: (pages?: number[]) => Promise<void>; split: (pagesPerFile: number) => Promise<SplitOutput | null>; crop: (target: { id: number; revision: number; pages: number[] }, insets: CropInsets) => Promise<void>; resetCrop?: (target: { id: number; revision: number; pages: number[] }) => Promise<void>; insert?: () => void; replace?: (pages: number[]) => void; close: () => void }) {
  const [selected, setSelected] = useState<number[]>(() => [clampPage(currentPage, document.pages.length)]);
  const [range, setRange] = useState(() => String(clampPage(currentPage, document.pages.length) + 1));
  const [destination, setDestination] = useState('');
  const [error, setError] = useState('');
  const [confirmDelete, setConfirmDelete] = useState(false);
  const [splitOpen, setSplitOpen] = useState(false);
  const [cropTarget, setCropTarget] = useState<{ id: number; revision: number; pages: CropPage[] } | null>(null);
  const closeButton = useRef<HTMLButtonElement | null>(null);
  const lastWorkspaceFocus = useRef<HTMLElement | null>(null);
  const modalReturnFocus = useRef<HTMLElement | null>(null);
  const modalWasOpen = useRef(false);
  const lastClicked = useRef(clampPage(currentPage, document.pages.length));
  const resetInFlight = useRef(false);
  useEffect(() => {
    const fallback = clampPage(lastClicked.current, document.pages.length);
    const bounded = selected.filter(index => index >= 0 && index < document.pages.length);
    lastClicked.current = fallback;
    if (selected.length > 0 && !bounded.length) { setSelected([fallback]); setRange(String(fallback + 1)); }
    else if (bounded.length !== selected.length) setSelected(bounded);
  }, [document.pages.length, selected]);
  const labelHold = labelEditsPending ? 'Unavailable while page-label edits are pending. Save a copy or discard the label edits first.' : undefined;
  const modalOpen = confirmDelete || splitOpen || cropTarget !== null;
  useLayoutEffect(() => {
    if (modalOpen && !modalWasOpen.current) modalReturnFocus.current = lastWorkspaceFocus.current;
    else if (!modalOpen && modalWasOpen.current) {
      restoreModalFocus(modalReturnFocus.current, closeButton.current);
      modalReturnFocus.current = null;
    }
    modalWasOpen.current = modalOpen;
  }, [modalOpen]);
  const moveTo = async () => {
    if (busy || selected.length !== 1) return;
    const position = Number(destination);
    if (!/^\d+$/.test(destination.trim()) || !Number.isSafeInteger(position) || position < 1 || position > document.pages.length) {
      setError(`Enter a destination between 1 and ${document.pages.length}.`); return;
    }
    setError('');
    if (await edit({ kind: 'move', from: selected[0], to: position - 1 })) {
      setSelected([position - 1]); setRange(String(position)); lastClicked.current = position - 1;
    }
  };
  const change = async (action: PageEdit) => {
    if (await edit(action)) { setSelected([]); setRange(''); }
  };
  const select = (index: number, shift: boolean, additive: boolean) => {
    if (shift) {
      const start = Math.min(lastClicked.current, index), end = Math.max(lastClicked.current, index);
      setSelected(Array.from({ length: end - start + 1 }, (_, i) => start + i));
    } else if (additive) setSelected(list => list.includes(index) ? list.filter(i => i !== index) : [...list, index].sort((a, b) => a - b));
    else setSelected([index]);
    lastClicked.current = index;
  };
  const count = selected.length;
  const openCrop = () => {
    if (busy || !count) return;
    setCropTarget({ id: document.id, revision: document.revision, pages: selected.map(page => ({ page, width: document.pages[page].width, height: document.pages[page].height })) });
  };
  const resetSelectedCrops = () => {
    if (busy || resetInFlight.current || !count) return;
    const pages = [...new Set(selected)].filter(page => page >= 0 && page < document.pages.length).sort((left, right) => left - right);
    if (!pages.length) return;
    resetInFlight.current = true; setError('');
    void resetCrop({ id: document.id, revision: document.revision, pages }).finally(() => { resetInFlight.current = false; });
  };
  return <section className={s.organizer} aria-label="Organize pages workspace" onFocusCapture={event => { if (!event.target.closest('dialog')) lastWorkspaceFocus.current = event.target; }}>
    <div className={s.heading}><div><h1>Organize pages</h1><p>Rotate, reorder, or extract pages. Save your changes as a new PDF.</p></div><button ref={closeButton} onClick={close} disabled={busy}><X size={17} /> Close tool</button></div>
    <div className={s.toolbar}>
      <label>Pages <input aria-label="Page selection range" placeholder="1-3, 5" value={range} onChange={e => setRange(e.target.value)} onKeyDown={e => { if (e.key === 'Enter') { try { setSelected(parsePageRange(range, document.pages.length)); setError(''); } catch (e) { setError(String(e)); } } }} disabled={busy} /></label>
      <button disabled={busy} onClick={() => { try { setSelected(parsePageRange(range, document.pages.length)); setError(''); } catch (e) { setError(String(e)); } }}>Select</button>
      <button title="Select all pages" aria-label="Select all pages" disabled={busy} onClick={() => setSelected(document.pages.map((_, i) => i))}><CheckSquare size={17} /></button>
      <span className={s.separator} />
      <button aria-label="Rotate selected pages counterclockwise" disabled={busy || !count} onClick={() => void edit({ kind: 'rotate', pages: selected, clockwise: false })}><RotateCcw size={17} /></button>
      <button aria-label="Rotate selected pages clockwise" disabled={busy || !count} onClick={() => void edit({ kind: 'rotate', pages: selected, clockwise: true })}><RotateCw size={17} /></button>
      <button title={labelHold} disabled={busy || labelEditsPending || !count || count === document.pages.length} onClick={() => setConfirmDelete(true)}><Trash2 size={16} /> Delete</button>
      <button disabled={busy || !count} onClick={() => void save(selected)}><ArrowDownToLine size={16} /> Extract</button>
      <span className={s.separator} />
      <button aria-label="Move selected page earlier" title={labelHold} disabled={busy || labelEditsPending || count !== 1 || selected[0] === 0} onClick={() => { const to = selected[0] - 1; void edit({ kind: 'move', from: selected[0], to }).then(ok => { if (ok) setSelected([to]); }); }}><ArrowLeft size={17} /></button>
      <button aria-label="Move selected page later" title={labelHold} disabled={busy || labelEditsPending || count !== 1 || selected[0] === document.pages.length - 1} onClick={() => { const to = selected[0] + 1; void edit({ kind: 'move', from: selected[0], to }).then(ok => { if (ok) setSelected([to]); }); }}><ArrowRight size={17} /></button>
      <label>Move to page <input aria-label="Destination page position" inputMode="numeric" placeholder={`1-${document.pages.length}`} value={destination} title={labelHold} disabled={busy || labelEditsPending || count !== 1} onChange={event => setDestination(event.target.value)} onKeyDown={event => { if (event.key === 'Enter') { event.preventDefault(); void moveTo(); } }} /></label>
      <button title={labelHold} disabled={busy || labelEditsPending || count !== 1} onClick={() => void moveTo()}>Move</button>
      <button aria-label="Undo page edit" disabled={busy || !document.can_undo} onClick={() => void change({ kind: 'undo' })}><Undo2 size={17} /></button>
      <button aria-label="Redo page edit" disabled={busy || !document.can_redo} onClick={() => void change({ kind: 'redo' })}><Redo2 size={17} /></button>
      <button disabled={busy} onClick={insert}><Files size={16} /> Insert pages</button>
      <button disabled={busy || !count} onClick={() => replace(selected)}><Files size={16} /> Replace pages</button>
      <button disabled={busy || !count} onClick={openCrop}><Crop size={16} /> Crop</button>
      <button aria-label="Reset crop on selected pages" title="Remove crop edits made in this open session. Original PDF crop and page boxes stay unchanged." disabled={busy || !count} onClick={resetSelectedCrops}>Reset crop</button>
      <button disabled={busy} onClick={() => setSplitOpen(true)}><Scissors size={16} /> Split</button>
      <button className={s.save} disabled={busy} onClick={() => void save()}>Save a copy</button>
    </div>
    <div className={s.selectionInfo}><span>{count} selected · {document.pages.length} pages{document.dirty ? ' · Unsaved changes' : ''}</span><span>Ctrl+click to add · Shift+click for a range</span></div>
    {error && <p className={s.error} role="alert">{error}</p>}
    <div className={s.grid}>{document.pages.map((_, index) => { const label = pageLabelFor(pageLabels, index); return <button key={index} disabled={busy} aria-label={`Select page ${index + 1}`} aria-pressed={selected.includes(index)} className={`${s.card} ${selected.includes(index) ? s.selected : ''}`} onClick={e => select(index, e.shiftKey, e.ctrlKey || e.metaKey)}><Thumbnail document={document} index={index} /><span className={s.pageLabel}><span className={s.checkbox}>{selected.includes(index) ? '✓' : ''}</span><span style={{ display: 'flex', flexDirection: 'column', gap: 4, minWidth: 0, whiteSpace: 'pre-wrap', overflowWrap: 'anywhere' }}>Page {index + 1}{label !== null && <small style={{ color: 'var(--muted)', fontSize: 10, whiteSpace: 'pre-wrap' }}>Label: {pageLabelDescription(label)}</small>}</span></span></button>; })}</div>
    {confirmDelete && <ConfirmDialog title={`Delete ${count} selected ${count === 1 ? 'page' : 'pages'}?`} message="This changes the working document. You can undo it. Your original file stays unchanged." confirmLabel="Delete pages" onCancel={() => setConfirmDelete(false)} onConfirm={() => { setConfirmDelete(false); void change({ kind: 'delete', pages: selected }); }} />}
    {splitOpen && <SplitDialog pageCount={document.pages.length} busy={busy} split={split} close={() => setSplitOpen(false)} />}
    {cropTarget && <CropDialog pages={cropTarget.pages} busy={busy} crop={insets => crop({ id: cropTarget.id, revision: cropTarget.revision, pages: cropTarget.pages.map(page => page.page) }, insets)} close={() => setCropTarget(null)} />}
  </section>;
}
