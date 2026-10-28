import { useCallback, useEffect, useLayoutEffect, useRef, useState } from 'react';
import { getCurrentWindow } from '@tauri-apps/api/window';
import { Undo2, Redo2 } from 'lucide-react';
import { ArrowDownToLine, ArrowUpRight, Bookmark, ChevronDown, ChevronLeft, ChevronRight, ChevronsUpDown, CircleHelp, Combine, File, FileCheck2, FileImage, FileOutput, FilePenLine, FilePlus2, Files, FolderOpen, Hand, Highlighter, Home, LayoutGrid, List, Maximize, Menu, MessageSquare, Minimize2, Minus, MoreHorizontal, MousePointer2, PanelLeftClose, Pencil, Plus, Printer, RotateCw, Save, ScanLine, Search, ShieldCheck, Signature, SlidersHorizontal, Star, Sun, Type, X, ZoomIn, ZoomOut, type LucideIcon } from 'lucide-react';
import { closeDocument, cancelPasswordRequest, native, openDocument, reopenDocument, checkpointRecovery, restoreRecovery, keepRecoveredEdits, openOriginal, editPages, saveCopy, splitDocument, cropPages, resetCrops, combineDocuments, insertPagesCopy, replacePagesCopy, documentFormFields, fillFormCopy, documentAnnotations, documentPageLabels, createPdfFromImage, chooseImagePdfSources, createPdfFromImages, cancelImagePdfSources, inspectImageReplacementTarget, replacePdfImageCopy, cancelImageReplacement, inspectTextReplacementTarget, replacePdfTextCopy, cancelTextReplacement, exportPageImage, redactDocument, createComment, updateComment, deleteComment, createHighlight, createTextHighlight, updateHighlight, deleteHighlight, createSearchableOcrCopy, cancelPageOcr, startLargeOcr, ocrCapability as getOcrCapability, type Annotation, type CommentRect, type CropInsets, type CreatePdfOptions, type DocumentAnnotations, type DocumentFormFields, type FormPatch, type ImageReplacementTarget, type TextReplacementTarget, type OcrCapability, type OpenResult, type PageImageExport, type PageImageExportRequest, type RasterRedactionRect, type SplitOutput, type SavedCopy, type RecoveryOffer } from './bridge';
import { discardRecovery } from './bridge';
import { recoveryCommittedReopenRequired } from './recoveryErrors';
import { clampPage, toolGroups, type DocumentInfo, type PageEdit } from './model';
import { pageLabelDescription, pageLabelFor, validatePageLabels, type DocumentPageLabels } from './pageLabels';
import Viewer from './Viewer';
import Organizer from './Organizer';
import ConfirmDialog from './ConfirmDialog';
import Updates from './Updates';
import SearchPanel, { type ActiveSearch } from './SearchPanel';
import BookmarksPanel from './BookmarksPanel';
import PageLabelsPanel from './PageLabelsPanel';
import type { BookmarkDestination } from './bookmarkDestination';
import PageText from './PageText';
import PageOcrDialog, { type PageOcrTarget } from './PageOcrDialog';
import SearchableOcrDialog from './SearchableOcrDialog';
import LargeOcrPanel from './LargeOcrPanel';
import { validateSearchableOcrCapability, type SearchableOcrTarget } from './searchableOcr';
import PasswordDialog from './PasswordDialog';
import PrintDialog from './PrintDialog';
import DocumentProperties from './DocumentProperties';
import DependencyNotices from './DependencyNotices';
import CombineDialog from './CombineDialog';
import InsertPagesDialog from './InsertPagesDialog';
import ReplacePagesDialog from './ReplacePagesDialog';
import FillFormsDialog from './FillFormsDialog';
import CreatePdfDialog from './CreatePdfDialog';
import MultiImagePdfDialog from './MultiImagePdfDialog';
import ImageReplacementDialog from './ImageReplacementDialog';
import TextReplacementDialog from './TextReplacementDialog';
import ExportPageImageDialog, { type PageImageTarget } from './ExportPageImageDialog';
import PageList from './PageList';
import CommentsPanel from './CommentsPanel';
import RedactPanel from './RedactPanel';
import CommentEditor, { type AnnotationDraft } from './CommentEditor';
import RecoveryOfferDialog from './RecoveryOfferDialog';
import type { TextHighlightSelection, TextHighlightSelectionSource } from './textHighlightSelection';
import { appendRedaction, type RedactionDraft } from './redaction';
import { readPreferences, savePreferences, type FitMode } from './preferences';

import { readRecentFiles, saveRecentFiles, rememberFile } from './recentFiles';
import { restoreModalFocus } from './modalFocus';
import s from './Workspace.module.css';

const icons: Record<string, LucideIcon> = { 'Create a PDF': FilePlus2, 'Combine files': Combine, 'Organize pages': LayoutGrid, 'Edit a PDF': FilePenLine, 'Export a PDF': FileOutput, 'Scan & OCR': ScanLine, 'Fill forms': Signature, 'Protect a PDF': ShieldCheck, 'Redact a PDF': ShieldCheck, 'Comment': MessageSquare, 'Compress a PDF': ArrowDownToLine };
function IconButton({ icon: Icon, label, onClick, onPointerDown, disabled = false, active }: { icon: LucideIcon; label: string; onClick?: () => void; onPointerDown?: () => void; disabled?: boolean; active?: boolean }) {
  return <button className={`${s.iconButton} ${active ? s.activeIcon : ''}`} aria-pressed={active} aria-label={label} title={disabled && !onClick ? `${label} — not implemented yet` : label} onPointerDown={onPointerDown} onClick={onClick} disabled={disabled}><Icon size={19} strokeWidth={1.7} /></button>;
}
type AnnotationsLoad = { request: string; annotations: DocumentAnnotations | null; error: string };
type FormsLoad = { request: string; fields: DocumentFormFields | null; error: string };
type PageLabelsLoad = { request: string; labels: DocumentPageLabels | null };

const annotationRequests = new Map<string, Promise<DocumentAnnotations>>();
const formRequests = new Map<string, Promise<DocumentFormFields>>();
function shareRequest<T>(requests: Map<string, Promise<T>>, key: string, start: () => Promise<T>): Promise<T> {
  const existing = requests.get(key);
  if (existing) return existing;
  const request = start();
  requests.set(key, request);
  void request.then(
    () => { if (requests.get(key) === request) requests.delete(key); },
    () => { if (requests.get(key) === request) requests.delete(key); },
  );
  return request;
}

export default function App() {
  const [preferences] = useState(readPreferences);
  const [documents, setDocuments] = useState<DocumentInfo[]>([]);
  const [active, setActive] = useState<number | null>(null);
  const [view, setView] = useState<'home' | 'tools' | 'document'>('home');
  const [section, setSection] = useState('Recent');
  const [query, setQuery] = useState('');
  const [toolsOpen, setToolsOpen] = useState(preferences.toolsOpen);
  const [nav, setNav] = useState(preferences.nav);
  const [searchOpen, setSearchOpen] = useState(false);
  const [activeSearch, setActiveSearch] = useState<ActiveSearch | null>(null);
  const [bookmarksOpen, setBookmarksOpen] = useState(false);
  const [labelsOpen, setLabelsOpen] = useState(false);
  const [labelsEpoch, setLabelsEpoch] = useState(0);
  const [pageTextOpen, setPageTextOpen] = useState(false);
  const [pageOcrTarget, setPageOcrTarget] = useState<PageOcrTarget | null>(null);
  const unavailableOcr = (reason: string): OcrCapability => ({ available: false, reason, language: null, searchablePdfAvailable: false, searchablePdfReason: reason, searchablePdfDpi: null, searchablePdfMaxPages: 32, searchablePdfMaxPixels: 33_554_432, searchablePdfMaxWords: 100_000, searchablePdfMaxTextBytes: 8_388_608, searchablePdfCharacters: null });
  const [ocr, setOcr] = useState<OcrCapability>(() => unavailableOcr(native ? 'Checking OCR availability…' : 'OCR is unavailable in the browser preview.'));
  const [searchableOcrTarget, setSearchableOcrTarget] = useState<SearchableOcrTarget | null>(null);
  const [largeOcrRefresh, setLargeOcrRefresh] = useState(0);
  const [printOpen, setPrintOpen] = useState(false);
  const [propertiesOpen, setPropertiesOpen] = useState(false);
  const [noticesOpen, setNoticesOpen] = useState(false);
  const [passwordRequest, setPasswordRequest] = useState<{ challenge: Extract<OpenResult, { status: 'password_required' }>; organize: boolean } | null>(null);
  const passwordBlocking = useRef(false);
  useEffect(() => { if (searchOpen) setBookmarksOpen(false); }, [searchOpen]);
  useEffect(() => { if (searchOpen || bookmarksOpen || !nav) setLabelsOpen(false); }, [searchOpen, bookmarksOpen, nav]);
  const [menu, setMenu] = useState(false);
  const [updatesOpen, setUpdatesOpen] = useState(false);
  const [dark, setDark] = useState(preferences.dark);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState('');
  const [notice, setNotice] = useState('');
  const [zoom, setZoom] = useState(preferences.zoom);
  const [fit, setFit] = useState<FitMode>(preferences.fit);
  const [hand, setHand] = useState(preferences.hand);
  const [page, setPage] = useState(0);
  const readingPages = useRef(new Map<number, number>());
  const [target, setTarget] = useState<{ page: number; token: number; destination?: BookmarkDestination | null }>({ page: 0, token: 0 });
  const [recentFiles, setRecentFiles] = useState(readRecentFiles);
  const [organizing, setOrganizing] = useState(false);
  const [combineOpen, setCombineOpen] = useState(false);
  const [createOpen, setCreateOpen] = useState(false);
  const [multiImageOpen, setMultiImageOpen] = useState(false);
  const [imageReplacementTarget,setImageReplacementTarget]=useState<ImageReplacementTarget|null>(null);
  const [textReplacementTarget,setTextReplacementTarget]=useState<TextReplacementTarget|null>(null);
  const newCopyModalOpen=multiImageOpen||imageReplacementTarget!==null||textReplacementTarget!==null;
  const [insertOpen, setInsertOpen] = useState(false);
  const [replaceOpen, setReplaceOpen] = useState(false);
  const [formsOpen, setFormsOpen] = useState(false);
  const [pageImageTarget, setPageImageTarget] = useState<PageImageTarget | null>(null);
  const [formsLoad, setFormsLoad] = useState<FormsLoad>({ request: '', fields: null, error: '' });
  const [replaceRange, setReplaceRange] = useState({ start: 0, count: 1 });
  const [commentsOpen, setCommentsOpen] = useState(false);
  const [commentMode, setCommentMode] = useState(false);
  const [highlightMode, setHighlightMode] = useState(false);
  const [redactionDraft, setRedactionDraft] = useState<RedactionDraft | null>(null);
  const [annotationsLoad, setAnnotationsLoad] = useState<AnnotationsLoad>({ request: '', annotations: null, error: '' });
  const [pageLabelsLoad, setPageLabelsLoad] = useState<PageLabelsLoad>({ request: '', labels: null });
  const [commentEditor, setCommentEditor] = useState<AnnotationDraft | null>(null);
  const [textHighlightSelection, setTextHighlightSelection] = useState<TextHighlightSelection | null>(null);
  const retainedTextHighlightSelection = useRef<TextHighlightSelection | null>(null);
  const pointerTextHighlightSelection = useRef<TextHighlightSelection | null>(null);
  const [pendingClose, setPendingClose] = useState<number | 'window' | null>(null);
  const [recoveryOffer, setRecoveryOffer] = useState<{ document: DocumentInfo; offer: RecoveryOffer } | null>(null);
  const [recoveryChoiceError, setRecoveryChoiceError] = useState('');
  const [recoveryLocked, setRecoveryLocked] = useState<Set<number>>(() => new Set());
  const menuButton = useRef<HTMLButtonElement | null>(null);
  const lastWorkspaceFocus = useRef<HTMLElement | null>(null);
  const modalReturnFocus = useRef<HTMLElement | null>(null);
  const modalWasOpen = useRef(false);
  const closingDocument = useRef<number | null>(null);
  const windowDiscarding = useRef(false);
  const mounted = useRef(true);
  useEffect(() => { mounted.current = true; return () => { mounted.current = false; }; }, []);
  const modalOpen = recoveryLocked.size > 0 || recoveryOffer !== null || updatesOpen || pageTextOpen || pageOcrTarget !== null || searchableOcrTarget !== null || newCopyModalOpen || printOpen || propertiesOpen || noticesOpen || passwordRequest !== null || pendingClose !== null || createOpen || pageImageTarget !== null || combineOpen || insertOpen || replaceOpen || formsOpen || commentEditor !== null;
  useLayoutEffect(() => {
    if (modalOpen && !modalWasOpen.current) {
      modalReturnFocus.current = lastWorkspaceFocus.current;
    } else if (!modalOpen && modalWasOpen.current) {
      restoreModalFocus(modalReturnFocus.current, menuButton.current);
      modalReturnFocus.current = null;
    }
    modalWasOpen.current = modalOpen;
  }, [modalOpen]);
  useEffect(() => {
    if (!savePreferences({ dark, zoom, fit, hand, toolsOpen, nav })) setNotice('Your reading preferences could not be saved. They will last for this session only.');
  }, [dark, zoom, fit, hand, toolsOpen, nav]);
  useEffect(() => {
    if (!saveRecentFiles(recentFiles)) setNotice('Recent files and stars could not be saved. They will last for this session only.');
  }, [recentFiles]);
  useEffect(() => {
    let disposed = false;
    if (!native) return;
    void getOcrCapability().then(capability => {
      const pageCoherent = capability?.available === true && capability.reason === null && capability.language === 'eng' || capability?.available === false && typeof capability.reason === 'string' && capability.reason.trim().length > 0 && capability.language === null;
      const searchableCoherent = typeof capability?.searchablePdfAvailable === 'boolean' && capability.searchablePdfMaxPages === 32 && capability.searchablePdfMaxPixels === 33_554_432 && capability.searchablePdfMaxWords === 100_000 && capability.searchablePdfMaxTextBytes === 8_388_608 && (capability.searchablePdfAvailable ? capability.searchablePdfReason === null && capability.searchablePdfDpi === 150 && capability.searchablePdfCharacters === 'Unicode words without control or whitespace characters' : typeof capability.searchablePdfReason === 'string' && capability.searchablePdfReason.trim().length > 0 && capability.searchablePdfDpi === null && capability.searchablePdfCharacters === null);
      const coherent = pageCoherent && searchableCoherent;
      if (!disposed && coherent) setOcr(capability);
      else if (!disposed) setOcr(unavailableOcr('OCR is unavailable in this build.'));
    }).catch(() => { if (!disposed) setOcr(unavailableOcr('OCR is unavailable in this build.')); });
    return () => { disposed = true; };
  }, []);
  const latest = useRef({ documents, busy, active, dialogOpen: false, recoveryLocked: false });
  latest.current = {
    documents, busy, active,
    dialogOpen: recoveryLocked.size > 0 || recoveryOffer !== null || updatesOpen || pageTextOpen || pageOcrTarget !== null || searchableOcrTarget !== null || multiImageOpen || imageReplacementTarget !== null || textReplacementTarget !== null || printOpen || propertiesOpen || noticesOpen || passwordRequest !== null || pendingClose !== null || createOpen || pageImageTarget !== null || combineOpen || insertOpen || replaceOpen || formsOpen || commentEditor !== null,
    recoveryLocked: recoveryLocked.size > 0,
  };
  useEffect(() => {
    if (!native) return;
    const unlisten = getCurrentWindow().onCloseRequested(event => {
      if (latest.current.busy || closingDocument.current !== null || latest.current.dialogOpen || latest.current.recoveryLocked) { event.preventDefault(); setNotice(latest.current.recoveryLocked ? 'Close the recovery-locked PDF before closing the window.' : 'Finish the current operation or dialog before closing.'); }
      else if (latest.current.documents.some(document => document.dirty)) { event.preventDefault(); setPendingClose('window'); }
    });
    return () => { void unlisten.then(stop => stop()); };
  }, []);
  const doc = documents.find(d => d.id === active);
  const receiveSearch = useCallback((search: ActiveSearch | null) => setActiveSearch(search), []);
  const closeSearch = useCallback(() => { setSearchOpen(false); setActiveSearch(null); }, []);
  useEffect(() => { setActiveSearch(null); }, [active, doc?.revision, organizing, searchOpen]);
  useEffect(() => { setCommentMode(false); setHighlightMode(false); setCommentsOpen(false); setCommentEditor(null); setFormsOpen(false); }, [active]);
  useEffect(() => { setRedactionDraft(null); }, [active, doc?.revision]);
  useEffect(() => { retainedTextHighlightSelection.current = null; pointerTextHighlightSelection.current = null; setTextHighlightSelection(null); }, [active, doc?.revision, hand, commentMode, highlightMode]);
  const annotationsNeeded = commentMode || highlightMode || commentsOpen || commentEditor !== null || textHighlightSelection !== null;
  const annotationsRequest = doc ? `${doc.id}:${doc.revision}` : '';
  useEffect(() => {
    if (!doc || !annotationsNeeded) { setAnnotationsLoad({ request: '', annotations: null, error: '' }); return; }
    let disposed = false;
    setAnnotationsLoad({ request: annotationsRequest, annotations: null, error: '' });
    shareRequest(annotationRequests, annotationsRequest, () => documentAnnotations(doc.id, doc.revision)).then(value => {
      if (disposed || value.documentId !== doc.id || value.revision !== doc.revision) return;
      setAnnotationsLoad({ request: annotationsRequest, annotations: value, error: '' });
    }).catch(reason => { if (!disposed) setAnnotationsLoad({ request: annotationsRequest, annotations: null, error: String(reason) }); });
    return () => { disposed = true; };
  }, [annotationsNeeded, annotationsRequest, doc?.id, doc?.revision]);
  const annotations = annotationsLoad.request === annotationsRequest ? annotationsLoad.annotations : null;
  const formsRequest = doc ? `${doc.id}:${doc.revision}` : '';
  useEffect(() => {
    if (!doc || !formsOpen) { setFormsLoad({ request: '', fields: null, error: '' }); return; }
    let disposed = false;
    setFormsLoad({ request: formsRequest, fields: null, error: '' });
    shareRequest(formRequests, formsRequest, () => documentFormFields(doc.id, doc.revision)).then(value => {
      if (disposed || value.documentId !== doc.id || value.revision !== doc.revision) return;
      setFormsLoad({ request: formsRequest, fields: value, error: '' });
    }).catch(reason => { if (!disposed) setFormsLoad({ request: formsRequest, fields: null, error: String(reason) }); });
    return () => { disposed = true; };
  }, [doc?.id, doc?.revision, formsOpen, formsRequest]);
  const formFields = formsLoad.request === formsRequest ? formsLoad.fields : null;
  const pageLabelsRequest = doc ? `${doc.id}:${doc.revision}:${labelsEpoch}` : '';
  useEffect(() => {
    if (!doc || typeof documentPageLabels !== 'function') { setPageLabelsLoad({ request: '', labels: null }); return; }
    let disposed = false;
    const { id, revision, pages } = doc;
    setPageLabelsLoad({ request: pageLabelsRequest, labels: null });
    Promise.resolve().then(() => documentPageLabels(id, revision)).then(value => {
      if (disposed) return;
      const snapshot = validatePageLabels(value, pages.length);
      if (!snapshot || snapshot.documentId !== id || snapshot.revision !== revision) return;
      setPageLabelsLoad({ request: pageLabelsRequest, labels: snapshot });
    }).catch(() => {});
    return () => { disposed = true; };
  }, [doc?.id, doc?.revision, doc?.pages.length, pageLabelsRequest, labelsEpoch]);
  const pageLabels = pageLabelsLoad.request === pageLabelsRequest ? pageLabelsLoad.labels : null;
  const activate = (id: number) => {
    if (busy || recoveryOffer || redactionDraft || searchableOcrTarget || newCopyModalOpen || closingDocument.current !== null) return;
    const document = documents.find(item => item.id === id);
    if (!document) return;
    if (pageOcrTarget && pageOcrTarget.id !== id) setPageOcrTarget(null);
    const next = clampPage(readingPages.current.get(id) ?? 0, document.pages.length);
    setActive(id); setView('document'); setPage(next); setTarget(value => ({ page: next, token: value.token + 1 }));
  };
  const trackPage = (value: number) => { if (doc) { const next = clampPage(value, doc.pages.length); readingPages.current.set(doc.id, next); setPage(next); } };
  const opened = (info: DocumentInfo, organize: boolean, initialPage = 0) => {
    const openedPage = clampPage(initialPage, info.pages.length);
    setDocuments(list => [...list, info]); setRecentFiles(list => rememberFile(list, info)); setOrganizing(organize);
    readingPages.current.set(info.id, openedPage);
    setActive(info.id); setView('document'); setPage(openedPage); setTarget(value => ({ page: openedPage, token: value.token + 1 }));
  };
  const acceptOpen = (result: OpenResult | null, organize: boolean) => {
    if (!mounted.current) {
      if (result?.status === 'opened') void closeDocument(result.document.id).catch(() => {});
      else if (result?.status === 'password_required') void cancelPasswordRequest(result.request_id).catch(() => {});
      return;
    }
    if (result?.status === 'opened') {
      const offer = result.recovery;
      const coherentOffer = !offer || result.document.revision === 0 && !result.document.dirty && Number.isSafeInteger(offer.revision) && offer.revision > 0 && Number.isSafeInteger(offer.currentPage) && offer.currentPage >= 0 && offer.currentPage < result.document.pages.length;
      if (!coherentOffer) { void closeDocument(result.document.id).catch(() => {}); setError('Recovery offered invalid state for this PDF.'); return; }
      opened(result.document, organize);
      if (offer) { setRecoveryChoiceError(''); setRecoveryOffer({ document: result.document, offer }); }
    }
    else if (result?.status === 'password_required') { passwordBlocking.current = true; setPasswordRequest({ challenge: result, organize }); }
  };
  const open = useCallback(async (example = false, organize = false) => {
    if (busy || recoveryOffer || searchableOcrTarget || newCopyModalOpen || closingDocument.current !== null) return;
    setBusy(true); setError(''); setMenu(false);
    try {
      acceptOpen(await openDocument(example), organize);
    } catch (e) { setError(String(e)); } finally { setBusy(false); }
  }, [busy, recoveryOffer, searchableOcrTarget, newCopyModalOpen]);
  const reopen = async (path: string) => {
    if (busy || recoveryOffer || searchableOcrTarget || newCopyModalOpen || closingDocument.current !== null) return;
    const existing = documents.find(document => document.path === path);
    if (existing) { activate(existing.id); return; }
    setBusy(true); setError('');
    try {
      acceptOpen(await reopenDocument(path), false);
    } catch (e) { if (mounted.current) setError(/This path cannot be opened|Nothing was opened\./.test(String(e)) ?`Could not reopen this file. ${String(e)}` : `Could not reopen this file. It may have moved or been deleted. ${String(e)}`); }
    finally { if (mounted.current) setBusy(false); }
  };
  const recoveryBlocked = busy || recoveryOffer !== null || redactionDraft !== null || searchableOcrTarget !== null || newCopyModalOpen || updatesOpen || pageTextOpen || pageOcrTarget !== null || printOpen || propertiesOpen || noticesOpen || passwordRequest !== null || pendingClose !== null || createOpen || pageImageTarget !== null || combineOpen || insertOpen || replaceOpen || formsOpen || commentEditor !== null || closingDocument.current !== null;
  const chooseRecovery = async (keep: boolean) => {
    const pending = recoveryOffer;
    if (!pending || busy) return;
    setBusy(true); setError(''); setNotice(''); setRecoveryChoiceError('');
    try {
      if (keep) {
        const result = await keepRecoveredEdits(pending.document.id, pending.document.revision);
        if (!mounted.current) return;
        const coherent = result.document.id === pending.document.id && result.document.dirty === true && result.document.revision === pending.offer.revision && result.currentPage === pending.offer.currentPage && result.currentPage >= 0 && result.currentPage < result.document.pages.length;
        if (!coherent) throw new Error('Recovered edits did not match the offered document revision.');
        setDocuments(list => list.map(document => document.id === result.document.id ? result.document : document));
        readingPages.current.set(result.document.id, result.currentPage); setPage(result.currentPage); setTarget(value => ({ page: result.currentPage, token: value.token + 1 }));
        setNotice('Recovered edits are open. The source PDF was not changed.');
      } else {
        const result = await openOriginal(pending.document.id, pending.document.revision);
        if (!mounted.current) return;
        if (result.id !== pending.document.id || result.revision !== pending.document.revision || result.dirty || result.pages.length !== pending.document.pages.length) throw new Error('Open original did not return the unchanged source session.');
        setDocuments(list => list.map(document => document.id === result.id ? result : document));
        setNotice('Opened the original PDF and marked the offered recovery edits as discarded.');
      }
      setRecoveryOffer(null);
    } catch (reason) {
      if (!await handleCommittedRecoveryFailure(pending.document.id, pending.document.revision, reason) && mounted.current) setRecoveryChoiceError(String(reason));
    }
    finally { if (mounted.current) setBusy(false); }
  };
  const checkpoint = async () => {
    if (!doc?.dirty || recoveryBlocked) return;
    const captured = { id: doc.id, revision: doc.revision, currentPage: clampPage(page, doc.pages.length) };
    setBusy(true); setError(''); setNotice(''); setMenu(false);
    try {
      const receipt = await checkpointRecovery(captured.id, captured.revision, captured.currentPage);
      if (receipt.documentId !== captured.id || receipt.revision !== captured.revision || receipt.currentPage !== captured.currentPage) throw new Error('Recovery checkpoint acknowledgement did not match this document revision.');
      if (mounted.current) setNotice('Recovery point saved for this revision. It holds page edits only; any bookmark or page-label edits are not included, so save a copy to keep them. Recovery is manual, not automatic; full power-loss durability is not proven.');
    } catch (e) {
      if (!await handleCommittedRecoveryFailure(captured.id, captured.revision, e) && mounted.current) setError(String(e));
    } finally { if (mounted.current) setBusy(false); }
  };
  const restore = async () => {
    if (recoveryBlocked) return;
    setBusy(true); setError(''); setNotice(''); setMenu(false);
    try {
      const result = await restoreRecovery();
      if (!result) return;
      const coherent = Number.isSafeInteger(result.document?.id) && result.document.id > 0 && result.document.dirty === true && Array.isArray(result.document.pages) && result.document.pages.length > 0 && Number.isInteger(result.currentPage) && result.currentPage >= 0 && result.currentPage < result.document.pages.length;
      if (!coherent || !mounted.current) {
        if (Number.isSafeInteger(result.document?.id) && result.document.id > 0) void closeDocument(result.document.id).catch(() => {});
        if (!mounted.current) return;
        throw new Error('Recovery returned an invalid edited document.');
      }
      opened(result.document, false, result.currentPage);
      setNotice('Recovered unsaved edits from the selected source PDF. The source file was not changed.');
    } catch (e) { if (mounted.current) setError(String(e)); } finally { if (mounted.current) setBusy(false); }
  };
  async function handleCommittedRecoveryFailure(id: number, revision: number, reason: unknown): Promise<boolean> {
    if (!recoveryCommittedReopenRequired(reason, id, revision)) return false;
    setRecoveryLocked(current => new Set(current).add(id));
    try {
      await closeDocument(id);
      readingPages.current.delete(id); setDocuments(list => list.filter(document => document.id !== id));
      setRecoveryLocked(current => { const next = new Set(current); next.delete(id); return next; });
      if (latest.current.active === id) { setActive(null); setView('home'); }
      setRecoveryOffer(current => current?.document.id === id ? null : current);
      setError('The edit may be saved in recovery, but its final confirmation failed. Reopen this PDF and choose Keep recovered edits or Open original.');
    } catch (closeReason) {
      setError(`The edit may be saved in recovery, but its final confirmation failed and the locked PDF could not close. Retry closing this tab before continuing. ${String(closeReason)}`);
    }
    return true;
  }
  const close = async (id: number, discard = false) => {
    const locked = recoveryLocked.has(id);
    if (busy || recoveryOffer && (!locked || recoveryOffer.document.id !== id) || redactionDraft || searchableOcrTarget || newCopyModalOpen || closingDocument.current !== null) return;
    if (!discard && !locked && documents.find(document => document.id === id)?.dirty) { setPendingClose(id); return; }
    if (pageOcrTarget?.id === id) setPageOcrTarget(null);
    closingDocument.current = id; setBusy(true); setError('');
    try {
      if (discard) {
        const current = documents.find(document => document.id === id);
        if (!current?.dirty) throw new Error('The document changed before its recovery edits could be discarded.');
        const receipt = await discardRecovery(current.id, current.revision);
        if (receipt.documentId !== current.id || receipt.revision !== current.revision) throw new Error('Recovery discard acknowledgement did not match this document revision.');
      }
      await closeDocument(id); readingPages.current.delete(id); setDocuments(list => list.filter(d => d.id !== id));
      if (locked) {
        setRecoveryLocked(current => { const next = new Set(current); next.delete(id); return next; });
        setRecoveryOffer(current => current?.document.id === id ? null : current);
        setError('The edit may be saved in recovery, but its final confirmation failed. Reopen this PDF and choose Keep recovered edits or Open original.');
      }
      if (latest.current.active === id) { setActive(null); setView('home'); }
    } catch (e) {
      const revision = documents.find(document => document.id === id)?.revision;
      if (revision === undefined || !await handleCommittedRecoveryFailure(id, revision, e)) setError(String(e));
    }
    finally { closingDocument.current = null; setBusy(false); }
  };
  const discardAndCloseWindow = async () => {
    if (busy || closingDocument.current !== null || windowDiscarding.current) return;
    windowDiscarding.current = true;
    const dirty = documents.filter(document => document.dirty).map(document => ({ id: document.id, revision: document.revision }));
    let acknowledged = 0;
    let currentId: number | null = null;
    setPendingClose(null); setBusy(true); setError(''); setNotice('');
    try {
      for (const document of dirty) {
        currentId = document.id;
        const receipt = await discardRecovery(document.id, document.revision);
        if (receipt.documentId !== document.id || receipt.revision !== document.revision) throw new Error('Recovery discard acknowledgement did not match this document revision.');
        acknowledged += 1;
      }
      await getCurrentWindow().destroy();
    } catch (reason) {
      windowDiscarding.current = false;
      const currentRevision = dirty.find(document => document.id === currentId)?.revision;
      if (currentId !== null && currentRevision !== undefined && await handleCommittedRecoveryFailure(currentId, currentRevision, reason)) { if (mounted.current) setBusy(false); return; }
      if (mounted.current) setError(`${acknowledged ? `Recovery was already discarded for ${acknowledged} open document${acknowledged === 1 ? '' : 's'}; those edits remain open and the next edit will create recovery again. ` : ''}${String(reason)}`);
      if (mounted.current) setBusy(false);
    }
  };
  const updateDocument = (info: DocumentInfo) => {
    setDocuments(list => list.map(document => document.id === info.id ? info : document));
    const next = clampPage(page, info.pages.length); readingPages.current.set(info.id, next); setPage(next); setTarget(value => ({ page: next, token: value.token + 1 }));
  };
  const edit = async (action: PageEdit): Promise<boolean> => {
    if (!doc || busy || searchableOcrTarget || newCopyModalOpen || closingDocument.current !== null) return false;
    setBusy(true); setError(''); setNotice('');
    try { updateDocument(await editPages(doc.id, doc.revision, action, clampPage(page, doc.pages.length))); return true; }
    catch (e) { if (!await handleCommittedRecoveryFailure(doc.id, doc.revision, e)) setError(String(e)); return false; } finally { setBusy(false); }
  };
  const save = async (pages?: number[]) => {
    if (!doc || busy || searchableOcrTarget || newCopyModalOpen || closingDocument.current !== null) return;
    setBusy(true); setError(''); setNotice('');
    try {
      const result = await saveCopy(doc.id, doc.revision, pages);
      if (result) { updateDocument(result.document); setNotice(`Saved ${pages ? 'selected pages' : 'a copy'} to ${result.path}`); }
    } catch (e) { setError(String(e)); } finally { setBusy(false); }
  };
  const split = async (pagesPerFile: number): Promise<SplitOutput | null> => {
    if (!doc || busy || closingDocument.current !== null) return null;
    setBusy(true); setError(''); setNotice('');
    try { return await splitDocument(doc.id, doc.revision, pagesPerFile); }
    finally { setBusy(false); }
  };
  const crop = async (target: { id: number; revision: number; pages: number[] }, insets: CropInsets): Promise<void> => {
    if (!doc || doc.id !== target.id || busy || closingDocument.current !== null) throw new Error('The document is no longer ready to crop.');
    setBusy(true); setError(''); setNotice('');
    try { updateDocument(await cropPages(target.id, target.revision, target.pages, insets, clampPage(page, doc.pages.length))); }
    catch (reason) { if (!await handleCommittedRecoveryFailure(target.id, target.revision, reason)) throw reason; }
    finally { setBusy(false); }
  };
  const reopenRef = useRef(reopen);
  reopenRef.current = reopen;
  const dropOpening = useRef(false);
  useEffect(() => {
    if (!native) return;
    let disposed = false;
    let stop: (() => void) | null = null;
    const window = getCurrentWindow();
    if (typeof window.onDragDropEvent !== 'function') return;
    void window.onDragDropEvent(event => {
      if (disposed || event.payload.type !== 'drop') return;
      const paths = event.payload.paths;
      if (dropOpening.current || passwordBlocking.current || latest.current.busy || closingDocument.current !== null || latest.current.dialogOpen) {
        setNotice('Finish the current operation or dialog before opening a dropped PDF.');
        return;
      }
      if (paths.length !== 1 || !paths[0] || paths[0].trim().length === 0) {
        setError('Drop exactly one PDF file at a time.');
        return;
      }
      const path = paths[0];
      if (!path.toLowerCase().endsWith('.pdf')) {
        setError('Drop a PDF file to open it.');
        return;
      }
      dropOpening.current = true;
      void reopenRef.current(path).finally(() => { dropOpening.current = false; });
    }).then(unlisten => {
      if (disposed) unlisten();
      else stop = unlisten;
    }).catch(() => {});
    return () => { disposed = true; stop?.(); };
  }, []);
  useEffect(() => {
    if (pageOcrTarget && !documents.some(document => document.id === pageOcrTarget.id && document.revision === pageOcrTarget.revision)) setPageOcrTarget(null);
  }, [documents, pageOcrTarget]);
  const resetCrop = async (target: { id: number; revision: number; pages: number[] }): Promise<void> => {
    if (!doc || doc.id !== target.id || doc.revision !== target.revision || busy || closingDocument.current !== null) { setError('The document changed. Reset crop again.'); return; }
    setBusy(true); setError(''); setNotice('');
    try {
      const result = await resetCrops(target.id, target.revision, target.pages, clampPage(page, doc.pages.length));
      if (result.id !== target.id || !Number.isSafeInteger(result.revision) || (result.revision !== target.revision && result.revision !== target.revision + 1)) throw new Error('The crop reset returned an unexpected document.');
      if (result.revision === target.revision) setNotice('The selected pages have no crop edits from this session.');
      else updateDocument(result);
    } catch (reason) { if (!await handleCommittedRecoveryFailure(target.id, target.revision, reason)) setError(String(reason)); }
    finally { setBusy(false); }
  };
  const mutateAnnotation = async (operation: (document: DocumentInfo, currentPage: number) => Promise<DocumentInfo>) => {
    const current = doc;
    if (!current || busy || closingDocument.current !== null) throw new Error('The document is no longer ready for comments.');
    const latestDocument = latest.current.documents.find(document => document.id === current.id);
    if (!latestDocument || latestDocument.revision !== current.revision) throw new Error('The document changed. Reload its comments and try again.');
    setBusy(true); setError(''); setNotice('');
    try { updateDocument(await operation(current, clampPage(page, current.pages.length))); }
    catch (reason) { if (!await handleCommittedRecoveryFailure(current.id, current.revision, reason)) throw reason; }
    finally { setBusy(false); }
  };
  const receiveTextHighlightSelection = useCallback((selection: TextHighlightSelection | null, source: TextHighlightSelectionSource) => {
    if (!selection || !doc || busy || hand || commentMode || highlightMode || selection.id !== doc.id || selection.revision !== doc.revision || selection.page < 0 || selection.page >= doc.pages.length || selection.start < 0 || selection.end <= selection.start) {
      setTextHighlightSelection(current => current?.id === source.id && current.page === source.page && current.revision === source.revision ? null : current);
      if (retainedTextHighlightSelection.current?.id === source.id && retainedTextHighlightSelection.current.page === source.page && retainedTextHighlightSelection.current.revision === source.revision) retainedTextHighlightSelection.current = null;
      return;
    }
    pointerTextHighlightSelection.current = null;
    retainedTextHighlightSelection.current = selection;
    setTextHighlightSelection(selection);
  }, [busy, commentMode, doc, hand, highlightMode]);
  const captureTextHighlightSelection = () => { pointerTextHighlightSelection.current = retainedTextHighlightSelection.current; };
  const beginTextHighlight = () => {
    const selection = pointerTextHighlightSelection.current || retainedTextHighlightSelection.current;
    pointerTextHighlightSelection.current = null;
    if (!doc || busy || newCopyModalOpen || hand || commentMode || highlightMode || annotations?.status !== 'supported' || !selection || selection.id !== doc.id || selection.revision !== doc.revision || selection.page < 0 || selection.page >= doc.pages.length || !Number.isSafeInteger(selection.start) || !Number.isSafeInteger(selection.end) || selection.start < 0 || selection.end <= selection.start) return;
    retainedTextHighlightSelection.current = null;
    setTextHighlightSelection(null);
    setCommentsOpen(true);
    setCommentEditor({ kind: 'create-text-highlight', page: selection.page, start: selection.start, end: selection.end });
  };
  const beginComment = (commentPage: number, rect: CommentRect) => {
    if (!doc || busy || newCopyModalOpen || annotations?.status !== 'supported') return;
    setHighlightMode(false); setCommentsOpen(true); setCommentEditor({ kind: 'create', type: 'note', page: commentPage, rect });
  };
  const beginHighlight = (highlightPage: number, rect: CommentRect) => {
    if (!doc || busy || newCopyModalOpen || annotations?.status !== 'supported') return;
    setCommentMode(false); setCommentsOpen(true); setCommentEditor({ kind: 'create', type: 'area-highlight', page: highlightPage, rect });
  };
  const selectAnnotation = (annotation: Annotation) => {
    if (!doc || busy || newCopyModalOpen) return;
    go(annotation.page); setCommentsOpen(true); setCommentEditor({ kind: 'edit', annotation });
  };
  const addCommentOnCurrentPage = () => {
    if (!doc || busy || newCopyModalOpen || annotations?.status !== 'supported') return;
    setHand(false); setHighlightMode(false); setCommentMode(true); setCommentsOpen(false);
  };
  const addHighlightOnCurrentPage = () => {
    if (!doc || busy || newCopyModalOpen || annotations?.status !== 'supported') return;
    setHand(false); setCommentMode(false); setHighlightMode(true); setCommentsOpen(false);
  };
  const launchComments = () => {
    if (!doc || busy || redactionDraft || newCopyModalOpen || closingDocument.current !== null) return;
    closeSearch(); setBookmarksOpen(false); setOrganizing(false); setView('document'); setCommentsOpen(true);
  };
  const launchCommentMode = () => {
    if (newCopyModalOpen) return;
    launchComments(); setRedactionDraft(null); setHand(false); setHighlightMode(false); setCommentMode(true);
  };
  const launchHighlightMode = () => {
    if (newCopyModalOpen) return;
    launchComments(); setRedactionDraft(null); setHand(false); setCommentMode(false); setHighlightMode(true);
  };
  const launchRedaction = () => {
    if (!doc || busy || searchableOcrTarget || newCopyModalOpen || closingDocument.current !== null) return;
    const currentPage = clampPage(page, doc.pages.length);
    retainedTextHighlightSelection.current=null; pointerTextHighlightSelection.current=null; setTextHighlightSelection(null);
    setMenu(false); setOrganizing(false); setView('document'); closeSearch(); setBookmarksOpen(false); setCommentsOpen(false); setCommentEditor(null); setCommentMode(false); setHighlightMode(false); setHand(false);
    setRedactionDraft({ id: doc.id, revision: doc.revision, page: currentPage, rectangles: [] });
  };
  const addRedaction = (redactionPage: number, rect: RasterRedactionRect) => {
    if (!busy) setRedactionDraft(draft => appendRedaction(draft, doc, redactionPage, rect));
  };
  const applyRedaction = async () => {
    const draft = redactionDraft;
    if (!draft || !doc || busy || closingDocument.current !== null || draft.id !== doc.id || draft.revision !== doc.revision || draft.page < 0 || draft.page >= doc.pages.length || draft.rectangles.length < 1 || draft.rectangles.length > 256) { setError('The document changed. Start redaction again.'); return; }
    setBusy(true); setError(''); setNotice('');
    try {
      const result = await redactDocument(draft);
      if (result) { setRedactionDraft(null); opened(result.document, false); }
    } catch (reason) { setError(String(reason)); }
    finally { setBusy(false); }
  };
  const combine = async (first: { id: number; revision: number }, second: { id: number; revision: number }): Promise<SavedCopy | null> => {
    if (busy || closingDocument.current !== null) throw new Error('The workspace is not ready to combine PDFs.');
    if (first.id === second.id) throw new Error('Choose two different open PDFs.');
    const firstDocument = documents.find(document => document.id === first.id);
    const secondDocument = documents.find(document => document.id === second.id);
    if (!firstDocument || !secondDocument) throw new Error('One selected PDF is no longer open. Choose two open PDFs.');
    if (firstDocument.revision !== first.revision || secondDocument.revision !== second.revision) throw new Error('A selected PDF changed. Choose the order again.');
    if (firstDocument.pages.length + secondDocument.pages.length > 4096) throw new Error('These PDFs exceed the 4,096-page combine limit.');
    setBusy(true); setError(''); setNotice('');
    try {
      const result = await combineDocuments(first, second);
      if (result) opened(result.document, false);
      return result;
    } finally { setBusy(false); }
  };
  const insert = async (target: { id: number; revision: number }, donor: { id: number; revision: number }, at: number): Promise<SavedCopy | null> => {
    if (busy || closingDocument.current !== null) throw new Error('The workspace is not ready to insert pages.');
    if (target.id === donor.id) throw new Error('Choose two different open PDFs.');
    if (!Number.isSafeInteger(at) || at < 0) throw new Error('Choose a valid insertion boundary.');
    const targetDocument = documents.find(document => document.id === target.id);
    const donorDocument = documents.find(document => document.id === donor.id);
    if (!targetDocument || !donorDocument) throw new Error('One selected PDF is no longer open. Choose two open PDFs.');
    if (targetDocument.revision !== target.revision || donorDocument.revision !== donor.revision) throw new Error('A selected PDF changed. Choose the documents again.');
    if (at > targetDocument.pages.length) throw new Error('Choose a valid insertion boundary.');
    if (targetDocument.pages.length + donorDocument.pages.length > 4096) throw new Error('These PDFs exceed the 4,096-page insert limit.');
    setBusy(true); setError(''); setNotice('');
    try {
      const result = await insertPagesCopy(target, donor, at);
      if (result) opened(result.document, false);
      return result;
    } finally { setBusy(false); }
  };
  const replace = async (target: { id: number; revision: number }, donor: { id: number; revision: number }, start: number, count: number): Promise<SavedCopy | null> => {
    if (busy || closingDocument.current !== null) throw new Error('The workspace is not ready to replace pages.');
    if (target.id === donor.id) throw new Error('Choose two different open PDFs.');
    if (!Number.isSafeInteger(start) || !Number.isSafeInteger(count) || start < 0 || count < 1) throw new Error('Choose one valid target page range.');
    const targetDocument = documents.find(document => document.id === target.id);
    const donorDocument = documents.find(document => document.id === donor.id);
    if (!targetDocument || !donorDocument) throw new Error('One selected PDF is no longer open. Choose two open PDFs.');
    if (targetDocument.revision !== target.revision || donorDocument.revision !== donor.revision) throw new Error('A selected PDF changed. Choose the documents again.');
    if (start + count > targetDocument.pages.length) throw new Error('Choose one valid target page range.');
    if (targetDocument.pages.length + donorDocument.pages.length > 4096) throw new Error('These input PDFs exceed the 4,096-page replace limit.');
    setBusy(true); setError(''); setNotice('');
    try {
      const result = await replacePagesCopy(target, donor, start, count);
      if (result) opened(result.document, false);
      return result;
    } finally { setBusy(false); }
  };
  const fillForms = async (id: number, revision: number, values: FormPatch[]): Promise<SavedCopy | null> => {
    if (busy || closingDocument.current !== null) throw new Error('The workspace is not ready to fill this form.');
    const source = documents.find(document => document.id === id);
    if (!source || source.revision !== revision) throw new Error('This PDF changed. Reload its fields and try again.');
    if (!values.length || values.some(value => !value.fieldId)) throw new Error('Change at least one field before saving a new copy.');
    if (new Set(values.map(value => value.fieldId)).size !== values.length) throw new Error('This form contains duplicate field updates. Reload it and try again.');
    setBusy(true); setError(''); setNotice('');
    try {
      const result = await fillFormCopy(id, revision, values);
      if (result) opened(result.document, false);
      return result;
    } finally { setBusy(false); }
  };
  const createPdf = async (options: CreatePdfOptions): Promise<SavedCopy | null> => {
    if (busy || closingDocument.current !== null) throw new Error('The workspace is not ready to create a PDF.');
    setBusy(true); setError(''); setNotice('');
    try {
      const result = await createPdfFromImage(options);
      if (result) opened(result.document, false);
      return result;
    } finally { setBusy(false); }
  };
  const createMultiImagePdf = async ({ selectionId, sourceIds, options }: { selectionId: string; sourceIds: string[]; options: CreatePdfOptions }): Promise<SavedCopy | null> => {
    if (busy || closingDocument.current !== null) throw new Error('The workspace is not ready to create a PDF.');
    setBusy(true); setError(''); setNotice('');
    try { const result = await createPdfFromImages(selectionId, sourceIds, options); if (result) opened(result.document, false); return result; }
    finally { setBusy(false); }
  };
  const exportCurrentPageImage = async (request: PageImageExportRequest): Promise<PageImageExport | null> => {
    if (busy || closingDocument.current !== null) throw new Error('The workspace is not ready to export an image.');
    const source = documents.find(document => document.id === request.id);
    if (!source || source.revision !== request.revision || request.page < 0 || request.page >= source.pages.length) throw new Error('This page changed. Reopen image export and try again.');
    setBusy(true); setError(''); setNotice('');
    try { return await exportPageImage(request); }
    finally { setBusy(false); }
  };
  const launchCreate = () => { if (busy || redactionDraft || searchableOcrTarget || newCopyModalOpen || closingDocument.current !== null) return; setMenu(false); setCreateOpen(true); };
  const launchMultiImageCreate = () => { if (busy || redactionDraft || searchableOcrTarget || newCopyModalOpen || closingDocument.current !== null) return; setMenu(false); setMultiImageOpen(true); };
  const launchImageReplacement=async()=>{if(!doc||busy||redactionDraft||searchableOcrTarget||newCopyModalOpen||closingDocument.current!==null)return;const captured={id:doc.id,revision:doc.revision,page:clampPage(page,doc.pages.length)};setMenu(false);setBusy(true);setError('');try{const target=await inspectImageReplacementTarget(captured.id,captured.revision,captured.page);if(target.documentId!==captured.id||target.revision!==captured.revision||target.page!==captured.page)throw new Error('Image replacement inspection returned the wrong document revision.');setImageReplacementTarget(target);}catch(reason){setError(reason instanceof Error?reason.message:String(reason));}finally{setBusy(false);}};
  const applyImageReplacement=async(target:ImageReplacementTarget):Promise<SavedCopy|null>=>{const source=documents.find(document=>document.id===target.documentId);if(!source||source.revision!==target.revision||busy||closingDocument.current!==null)throw new Error('This document changed. Inspect the image again.');setBusy(true);setError('');setNotice('');try{return await replacePdfImageCopy(target.selectionId,target.documentId,target.revision);}finally{setBusy(false);}};
  const launchTextReplacement=async()=>{if(!doc||busy||redactionDraft||searchableOcrTarget||multiImageOpen||imageReplacementTarget||textReplacementTarget||closingDocument.current!==null)return;const captured={id:doc.id,revision:doc.revision,page:clampPage(page,doc.pages.length)};setMenu(false);setBusy(true);setError('');try{const target=await inspectTextReplacementTarget(captured.id,captured.revision,captured.page);if(target.documentId!==captured.id||target.revision!==captured.revision||target.page!==captured.page)throw new Error('Text replacement inspection returned the wrong document revision.');setTextReplacementTarget(target);}catch(reason){setError(reason instanceof Error?reason.message:String(reason));}finally{setBusy(false);}};
  const applyTextReplacement=async(target:TextReplacementTarget,replacement:string):Promise<SavedCopy|null>=>{const source=documents.find(document=>document.id===target.documentId);if(!source||source.revision!==target.revision||busy||closingDocument.current!==null)throw new Error('This document changed. Inspect the text again.');setBusy(true);setError('');setNotice('');try{return await replacePdfTextCopy(target.selectionId,target.documentId,target.revision,target.runId,replacement);}finally{setBusy(false);}};
  const launchPageImageExport = () => {
    if (!doc || busy || redactionDraft || searchableOcrTarget || newCopyModalOpen || closingDocument.current !== null) return;
    const currentPage = clampPage(page, doc.pages.length), size = doc.pages[clampPage(page, doc.pages.length)];
    if (!size) return;
    setMenu(false); setPageImageTarget({ id: doc.id, revision: doc.revision, page: currentPage, size });
  };
  const launchOrganizer = () => { if (busy || redactionDraft || searchableOcrTarget || newCopyModalOpen || closingDocument.current !== null) return; if (doc) { setOrganizing(true); setView('document'); } else void open(false, true); };
  const launchCombine = () => {
    if (busy || redactionDraft || searchableOcrTarget || newCopyModalOpen || closingDocument.current !== null) return;
    if (documents.length < 2) { setNotice('Open two PDFs to combine them.'); return; }
    setMenu(false); setOrganizing(false); setView('document'); setCombineOpen(true);
  };
  const launchForms = () => {
    if (!doc || busy || redactionDraft || searchableOcrTarget || newCopyModalOpen || closingDocument.current !== null) return;
    setMenu(false); setOrganizing(false); closeSearch(); setBookmarksOpen(false); setFormsOpen(true);
  };
  const launchPageOcr = () => {
    if (!doc || busy || redactionDraft || searchableOcrTarget || newCopyModalOpen || closingDocument.current !== null || !ocr.available) return;
    const currentPage = clampPage(page, doc.pages.length);
    setMenu(false); setOrganizing(false); closeSearch(); setBookmarksOpen(false);
    setPageOcrTarget({ id: doc.id, revision: doc.revision, page: currentPage, name: doc.name });
  };
  const searchableCapability = { searchablePdfAvailable: ocr.searchablePdfAvailable, searchablePdfReason: ocr.searchablePdfReason, searchablePdfDpi: ocr.searchablePdfDpi, searchablePdfMaxPages: ocr.searchablePdfMaxPages, searchablePdfMaxPixels: ocr.searchablePdfMaxPixels, searchablePdfMaxWords: ocr.searchablePdfMaxWords, searchablePdfMaxTextBytes: ocr.searchablePdfMaxTextBytes, searchablePdfCharacters: ocr.searchablePdfCharacters };
  const searchableCapabilityError = validateSearchableOcrCapability(searchableCapability);
  const launchSearchableOcr = () => {
    if (!doc || busy || redactionDraft || pageOcrTarget || newCopyModalOpen || closingDocument.current !== null || searchableCapabilityError || doc.pages.length > ocr.searchablePdfMaxPages) return;
    setMenu(false); setOrganizing(false); closeSearch(); setBookmarksOpen(false);
    setSearchableOcrTarget({ documentId: doc.id, revision: doc.revision });
  };
  const launchLargeOcr = async () => {
    if (!doc || busy || redactionDraft || pageOcrTarget || newCopyModalOpen || closingDocument.current !== null || searchableCapabilityError) return;
    setMenu(false); setError('');
    try { if (await startLargeOcr(doc.id, doc.revision)) setLargeOcrRefresh(value => value + 1); }
    catch (reason) { setError(reason instanceof Error ? reason.message : String(reason)); }
  };
  const createSearchableCopy = async (request: { requestId: string; documentId: number; revision: number }): Promise<SavedCopy | null> => {
    const source = documents.find(document => document.id === request.documentId);
    if (!source || source.revision !== request.revision || busy || closingDocument.current !== null) throw new Error('This document changed. Reopen searchable OCR and try again.');
    setBusy(true); setError(''); setNotice('');
    try { return await createSearchableOcrCopy(request); }
    finally { setBusy(false); }
  };
  const launchInsert = () => {
    if (busy || newCopyModalOpen || closingDocument.current !== null) return;
    if (documents.length < 2) { setNotice('Open two PDFs to insert pages into a new copy.'); return; }
    setMenu(false); setInsertOpen(true);
  };
  const launchReplace = (selected: number[]) => {
    if (busy || newCopyModalOpen || closingDocument.current !== null) return;
    if (documents.length < 2) { setNotice('Open two PDFs to replace pages in a new copy.'); return; }
    if (!selected.length || selected.some((pageNumber, index) => pageNumber !== selected[0] + index)) { setNotice('Select one contiguous target page range to replace.'); return; }
    setReplaceRange({ start: selected[0], count: selected.length }); setMenu(false); setReplaceOpen(true);
  };
  const go = (value: number, destination?: BookmarkDestination | null) => { if (doc) { const next = clampPage(value, doc.pages.length); trackPage(next); setTarget(v => ({ page: next, token: v.token + 1, destination: destination ?? null })); } };
  const applyDestinationView = (nextFit: FitMode, nextZoom: number) => { setFit(nextFit); setZoom(nextZoom); };
  const selectPageFit = () => {
    setFit('page');
    if (!doc) return;
    const current = clampPage(page, doc.pages.length);
    setTarget(value => ({ page: current, token: value.token + 1 }));
  };
  useEffect(() => {
    const listener = (event: KeyboardEvent) => {
      if (recoveryOffer || updatesOpen || pageTextOpen || pageOcrTarget || searchableOcrTarget || newCopyModalOpen || printOpen || propertiesOpen || noticesOpen || passwordRequest || pendingClose !== null || createOpen || pageImageTarget || combineOpen || insertOpen || replaceOpen || commentEditor) return;
      if ((event.target as HTMLElement | null)?.closest?.('dialog')) return;
      if (event.key === 'Escape' && menu) { event.preventDefault(); setMenu(false); menuButton.current?.focus(); return; }
      if (redactionDraft && event.ctrlKey) { event.preventDefault(); return; }
      if (event.ctrlKey && event.key.toLowerCase() === 'o') { event.preventDefault(); void open(); }
      if (event.ctrlKey && event.key.toLowerCase() === 'f' && doc) { event.preventDefault(); setView('document'); setOrganizing(false); setSearchOpen(true); return; }
      if (event.ctrlKey && event.key.toLowerCase() === 'p' && doc) { event.preventDefault(); if (!busy) setPrintOpen(true); return; }
      if (event.ctrlKey && event.key.toLowerCase() === 'd' && doc) { event.preventDefault(); if (!busy) setPropertiesOpen(true); return; }
      if (event.key === 'Escape' && searchOpen) { closeSearch(); return; }
      if (event.key === 'Escape' && (commentsOpen || commentMode || highlightMode || redactionDraft)) { setCommentsOpen(false); setCommentMode(false); setHighlightMode(false); setRedactionDraft(null); return; }
      if ((event.target as HTMLElement).matches('input,select,textarea')) return;
      if (event.key === 'F4') { event.preventDefault(); event.shiftKey ? setToolsOpen(v => !v) : setNav(v => !v); }
      if (event.ctrlKey && event.key === '2') { event.preventDefault(); setFit('width'); }
      if (event.ctrlKey && event.key === '1') { event.preventDefault(); setFit('none'); setZoom(100); }
      if (event.ctrlKey && event.key === '0') { event.preventDefault(); selectPageFit(); }
      if (view !== 'document' || !doc) return;
      if (event.ctrlKey && event.key.toLowerCase() === 's') { event.preventDefault(); void save(); return; }
      if (event.ctrlKey && event.key.toLowerCase() === 'z') { event.preventDefault(); void edit({ kind: event.shiftKey ? 'redo' : 'undo' }); return; }
      if (event.ctrlKey && event.key.toLowerCase() === 'y') { event.preventDefault(); void edit({ kind: 'redo' }); return; }
      if (event.key === 'PageDown') { event.preventDefault(); go(page + 1); }
      if (event.key === 'PageUp') { event.preventDefault(); go(page - 1); }
      if (event.key === 'Home') { event.preventDefault(); go(0); }
      if (event.key === 'End') { event.preventDefault(); go(doc.pages.length - 1); }
      if (event.key.toLowerCase() === 'h' && !event.ctrlKey) { setHand(true); setCommentMode(false); setHighlightMode(false); setRedactionDraft(null); }
      if (event.ctrlKey && event.key === 'Tab') { event.preventDefault(); const i = documents.findIndex(d => d.id === active); activate(documents[(i + 1) % documents.length].id); }
      if (event.ctrlKey && event.key.toLowerCase() === 'w') { event.preventDefault(); void close(doc.id); }
    };
    window.addEventListener('keydown', listener); return () => window.removeEventListener('keydown', listener);
  });
  const listed = recentFiles.filter(d => d.name.toLowerCase().includes(query.toLowerCase()) && (section !== 'Starred' || d.starred));
  const changeZoom = (value: number) => { setFit('none'); setZoom(Math.max(10, Math.min(400, value))); };
  const toolRow = (name: string, index: number) => {
    const Icon = icons[name] || FileCheck2;
    const pageOcr = name === 'Scan & OCR';
    const redaction = name === 'Redact a PDF';
    const available = name === 'Create a PDF' || name === 'Export a PDF' || name === 'Organize pages' || name === 'Combine files' || name === 'Comment' || name === 'Fill forms' || redaction || pageOcr && ocr.available;
    const create = name === 'Create a PDF';
    const imageExport = name === 'Export a PDF';
    const combine = name === 'Combine files';
    const comment = name === 'Comment';
    const forms = name === 'Fill forms';
    const disabled = busy || !!redactionDraft || newCopyModalOpen || !available || (combine && documents.length < 2) || ((comment || forms || imageExport || redaction || pageOcr) && !doc);
    return <button key={name} className={s.toolRow} disabled={disabled} onClick={create ? launchCreate : imageExport ? launchPageImageExport : pageOcr ? launchPageOcr : combine ? launchCombine : comment ? launchCommentMode : forms ? launchForms : redaction ? launchRedaction : launchOrganizer} title={combine && documents.length < 2 ? 'Open two PDFs to combine them' : (comment || forms || imageExport || redaction || pageOcr) && !doc ? 'Open a PDF first' : imageExport ? 'Export the current page as image' : redaction ? 'Permanently blank marked areas in a rasterized copy' : pageOcr ? (ocr.available ? 'Recognize text on the current page in English' : ocr.reason || 'OCR is unavailable in this build.') : forms ? 'Fill existing fields' : available ? name : `${name} — planned, not implemented yet`}><span className={s.toolIcon} style={{ color: ['#7361b3', '#277bb4', '#239576', '#bc6b25'][index % 4] }}><Icon size={21} strokeWidth={1.7} /></span><span>{imageExport ? 'Export page as image' : name}</span></button>;
  };
  return <div className={`${s.app} ${dark ? s.dark : ''}`} onFocusCapture={event => { if (!event.target.closest('dialog')) lastWorkspaceFocus.current = event.target; }}>
    <header className={s.tabbar}>
      <div className={s.appMark}><Files size={21} /></div>
      <button ref={menuButton} className={s.menuButton} onClick={() => setMenu(v => !v)} aria-expanded={menu} aria-controls="app-menu"><Menu size={17} /> Menu</button>
      <button className={`${s.homeTab} ${view === 'home' ? s.selectedTab : ''}`} aria-label="Home" aria-current={view === 'home' ? 'page' : undefined} disabled={!!redactionDraft || newCopyModalOpen} onClick={() => { if (!newCopyModalOpen) setView('home'); }}><Home size={19} /></button>
      <div className={s.documentTabs}>{documents.map(document => <div key={document.id} className={`${s.documentTab} ${view === 'document' && active === document.id ? s.selectedTab : ''}`}><button aria-current={view === 'document' && active === document.id ? 'page' : undefined} disabled={busy || !!redactionDraft || newCopyModalOpen} onClick={() => activate(document.id)}><File size={15} /><span>{document.name}{document.dirty ? ' *' : ''}</span></button><IconButton icon={X} label={`Close ${document.name}`} disabled={busy || !!redactionDraft || newCopyModalOpen} onClick={() => void close(document.id)} /></div>)}</div>
      <button className={s.createButton} disabled={busy || !!redactionDraft || newCopyModalOpen} onClick={launchCreate} title="Create a PDF from an image"><Plus size={17} /> Create</button>
      <span className={s.windowTitle}>PDF Workstation</span>
    </header>
    {menu && <div id="app-menu" className={s.menuPopover}>
      <button disabled={!doc?.dirty || recoveryBlocked} onClick={() => void checkpoint()}>Create manual recovery point</button>
      <button disabled={recoveryBlocked} onClick={() => void restore()}>Restore manual recovery point</button><hr />
      <button disabled={!!redactionDraft || newCopyModalOpen} onClick={() => void open()}>Open… <kbd>Ctrl+O</kbd></button><button disabled={!!redactionDraft || newCopyModalOpen} onClick={() => void open(true)}>Open sample PDF</button>
      <button disabled={!doc || busy || !!redactionDraft || !!searchableOcrTarget || newCopyModalOpen} onClick={() => { setMenu(false); void save(); }}>Save a copy… <kbd>Ctrl+S</kbd></button>
      <button disabled={!doc || busy || !!redactionDraft || newCopyModalOpen} onClick={launchPageImageExport}>Export page as image…</button>
      <button disabled={busy || !!redactionDraft || !!searchableOcrTarget || multiImageOpen} onClick={launchMultiImageCreate}>Create PDF from images…</button>
      <button disabled={!doc || busy || !!redactionDraft || !!searchableOcrTarget || newCopyModalOpen} onClick={() => void launchImageReplacement()}>Replace full-page image in a new copy…</button>
      <button disabled={!doc || busy || !!redactionDraft || !!searchableOcrTarget || multiImageOpen || !!imageReplacementTarget || !!textReplacementTarget} onClick={() => void launchTextReplacement()}>Replace strict text in a new copy…</button>
      <button disabled={!doc || busy || !!redactionDraft || newCopyModalOpen || !ocr.available} onClick={launchPageOcr} title={ocr.available ? 'Recognize text on the current page in English' : ocr.reason || 'OCR is unavailable in this build.'}>Recognize current page…</button>
      <button disabled={!doc || busy || !!redactionDraft || newCopyModalOpen || !!searchableCapabilityError || (doc?.pages.length ?? 0) > ocr.searchablePdfMaxPages} onClick={launchSearchableOcr} title={searchableCapabilityError || ((doc?.pages.length ?? 0) > ocr.searchablePdfMaxPages ? `Searchable OCR is limited to ${ocr.searchablePdfMaxPages} pages.` : 'Create a fixed-150-DPI searchable copy in this verified opt-in build.')}>Create searchable OCR copy…</button>
      {(doc?.pages.length ?? 0) > ocr.searchablePdfMaxPages && <button disabled={!doc || busy || !!redactionDraft || newCopyModalOpen || !!searchableCapabilityError} onClick={() => void launchLargeOcr()} title={searchableCapabilityError || 'Create a searchable copy of a large document (up to 10,000 pages). OCR runs in the background; you can keep reading.'}>Create searchable OCR copy (large document)…</button>}
      <button disabled={!!redactionDraft || newCopyModalOpen} onClick={() => { setMenu(false); launchOrganizer(); }}>Organize pages</button>
      <button disabled={!doc || busy || !!redactionDraft || newCopyModalOpen} onClick={() => { if (!newCopyModalOpen) { setMenu(false); setPropertiesOpen(true); } }}>Document properties… <kbd>Ctrl+D</kbd></button><hr />
      <button onClick={() => { setDark(v => !v); setMenu(false); }}>Switch to {dark ? 'light' : 'dark'} theme</button>
      <button disabled={busy || newCopyModalOpen} onClick={() => { if (!newCopyModalOpen) { setMenu(false); setUpdatesOpen(true); } }}>Check for updates…</button>
      <button disabled={busy || newCopyModalOpen} onClick={() => { if (!newCopyModalOpen) { setMenu(false); setNoticesOpen(true); } }}>Third-party notices…</button>
      <button onClick={() => { setNotice(`PDF viewing, embedded-text search, Create a PDF from a PNG or JPEG image, Combine Files, Organize Pages, and filling a strict subset of existing plain text fields are available. Rotate, reorder, delete, extract, undo/redo, and save a new copy.${ocr.available ? ' Current-page English OCR is enabled in this opt-in source build.' : ' OCR is not enabled in this build.'} Text editing and signatures are not implemented yet.`); setMenu(false); }}>About this build</button>
    </div>}
    {updatesOpen && <Updates dirty={documents.some(document => document.dirty)} busy={busy} setBusy={setBusy} close={() => setUpdatesOpen(false)} />}
    {recoveryOffer && <RecoveryOfferDialog name={recoveryOffer.document.name} revision={recoveryOffer.offer.revision} busy={busy} error={recoveryChoiceError} keep={() => void chooseRecovery(true)} original={() => void chooseRecovery(false)} />}
    {pageTextOpen && doc && <PageText key={`${doc.id}-${doc.revision}-${page}`} document={doc} page={page} close={() => setPageTextOpen(false)} />}
    {pageOcrTarget && <PageOcrDialog key={`${pageOcrTarget.id}-${pageOcrTarget.revision}-${pageOcrTarget.page}`} target={pageOcrTarget} setBusy={setBusy} close={() => setPageOcrTarget(null)} />}
    {searchableOcrTarget && <SearchableOcrDialog key={`${searchableOcrTarget.documentId}-${searchableOcrTarget.revision}`} capability={searchableCapability} target={searchableOcrTarget} createRequestId={() => crypto.randomUUID()} create={createSearchableCopy} cancel={cancelPageOcr} opened={copy => opened(copy.document, false)} close={() => setSearchableOcrTarget(null)} />}
    {native && <LargeOcrPanel document={doc ? { id: doc.id, revision: doc.revision } : null} refresh={largeOcrRefresh} openCopy={path => void reopen(path)} />}
    {passwordRequest && <PasswordDialog key={passwordRequest.challenge.request_id} challenge={passwordRequest.challenge} onOpened={info => { passwordBlocking.current = false; opened(info, passwordRequest.organize); setPasswordRequest(null); }} onClose={() => { passwordBlocking.current = false; setPasswordRequest(null); }} onRefused={message => { passwordBlocking.current = false; setPasswordRequest(null); setError(message); }} />}
    {printOpen && doc && <PrintDialog document={doc} page={page} setBusy={setBusy} close={() => setPrintOpen(false)} />}
    {propertiesOpen && doc && <DocumentProperties key={`${doc.id}-${doc.revision}`} document={doc} close={() => setPropertiesOpen(false)} />}
    {noticesOpen && <DependencyNotices close={() => setNoticesOpen(false)} />}
    {createOpen && <CreatePdfDialog busy={busy} create={createPdf} close={() => setCreateOpen(false)} />}
    {multiImageOpen && <MultiImagePdfDialog busy={busy} choose={chooseImagePdfSources} create={createMultiImagePdf} cancel={cancelImagePdfSources} close={() => setMultiImageOpen(false)} />}
    {imageReplacementTarget&&<ImageReplacementDialog target={imageReplacementTarget} replace={applyImageReplacement} cancel={cancelImageReplacement} opened={copy=>opened(copy.document,false)} close={()=>setImageReplacementTarget(null)}/>}
    {textReplacementTarget&&<TextReplacementDialog target={textReplacementTarget} replace={applyTextReplacement} cancel={cancelTextReplacement} opened={copy=>opened(copy.document,false)} close={()=>setTextReplacementTarget(null)}/>}
    {pageImageTarget && <ExportPageImageDialog key={`${pageImageTarget.id}:${pageImageTarget.revision}:${pageImageTarget.page}`} target={pageImageTarget} busy={busy} exportPage={exportCurrentPageImage} close={() => setPageImageTarget(null)} />}
    {combineOpen && <CombineDialog documents={documents} activeId={active} busy={busy} combine={combine} close={() => setCombineOpen(false)} />}
    {insertOpen && <InsertPagesDialog documents={documents} activeId={active} busy={busy} insert={insert} close={() => setInsertOpen(false)} />}
    {replaceOpen && <ReplacePagesDialog documents={documents} activeId={active} initialRange={replaceRange} busy={busy} replace={replace} close={() => setReplaceOpen(false)} />}
    {formsOpen && doc && <FillFormsDialog key={`${doc.id}:${doc.revision}`} document={doc} formFields={formFields} error={formsLoad.request === formsRequest ? formsLoad.error : ''} busy={busy} fill={fillForms} close={() => setFormsOpen(false)} />}
    {commentEditor && doc && <CommentEditor key={`${doc.id}:${doc.revision}:${commentEditor.kind === 'edit' ? commentEditor.annotation.id : 'new'}`} draft={commentEditor} busy={busy} save={contents => commentEditor.kind === 'create-text-highlight'
      ? mutateAnnotation((current, currentPage) => createTextHighlight(current.id, current.revision, commentEditor.page, commentEditor.start, commentEditor.end, contents, currentPage))
      : commentEditor.kind === 'create' ? commentEditor.type === 'note' ? mutateAnnotation((current, currentPage) => createComment(current.id, current.revision, commentEditor.page, commentEditor.rect, contents || '', currentPage)) : mutateAnnotation((current, currentPage) => createHighlight(current.id, current.revision, commentEditor.page, commentEditor.rect, contents, currentPage))
      : commentEditor.annotation.kind === 'note' ? mutateAnnotation((current, currentPage) => updateComment(current.id, current.revision, commentEditor.annotation.id, contents || '', currentPage)) : mutateAnnotation((current, currentPage) => updateHighlight(current.id, current.revision, commentEditor.annotation.id, contents, currentPage))}
      remove={commentEditor.kind === 'edit' ? () => commentEditor.annotation.kind === 'note' ? mutateAnnotation((current, currentPage) => deleteComment(current.id, current.revision, commentEditor.annotation.id, currentPage)) : mutateAnnotation((current, currentPage) => deleteHighlight(current.id, current.revision, commentEditor.annotation.id, currentPage)) : undefined} close={() => setCommentEditor(null)} />}
    <div className={s.globalbar}>
      <nav className={s.primaryNav} aria-label="Primary"><button className={toolsOpen && view !== 'home' ? s.selectedNav : ''} aria-pressed={toolsOpen && view !== 'home'} disabled={!!redactionDraft || newCopyModalOpen} onClick={() => { if (!newCopyModalOpen) view === 'document' ? setToolsOpen(v => !v) : setView('tools'); }}>All tools</button><button disabled>Edit</button><button disabled>Convert</button><button disabled>E-sign</button></nav>
      <div className={s.globalActions}><IconButton icon={Search} label="Find text" disabled={!doc || busy || !!redactionDraft || !!searchableOcrTarget || newCopyModalOpen} active={searchOpen} onClick={() => { if (!newCopyModalOpen) { setView('document'); setOrganizing(false); searchOpen ? closeSearch() : setSearchOpen(true); } }} /><span className={s.divider} /><IconButton icon={Undo2} label="Undo" disabled={busy || !!redactionDraft || !!searchableOcrTarget || newCopyModalOpen || !doc?.can_undo} onClick={() => void edit({ kind: 'undo' })} /><IconButton icon={Redo2} label="Redo" disabled={busy || !!redactionDraft || !!searchableOcrTarget || newCopyModalOpen || !doc?.can_redo} onClick={() => void edit({ kind: 'redo' })} /><IconButton icon={Save} label="Save a copy" disabled={busy || !!redactionDraft || !!searchableOcrTarget || newCopyModalOpen || !doc} onClick={() => void save()} /><IconButton icon={Printer} label="Print" disabled={busy || !!redactionDraft || !!searchableOcrTarget || newCopyModalOpen || !doc} onClick={() => { if (!searchableOcrTarget && !newCopyModalOpen) setPrintOpen(true); }} /><IconButton icon={Sun} label="Toggle theme" onClick={() => setDark(v => !v)} /><IconButton icon={CircleHelp} label="Build information" onClick={() => setNotice('Organize Pages and document search are available. Other tools marked unavailable are planned for later milestones. Save a Copy writes a new file and preserves your original.')} /><button className={s.openButton} onClick={() => void open()} disabled={busy || !!redactionDraft || !!searchableOcrTarget || newCopyModalOpen}><FolderOpen size={16} /> {busy ? 'Working…' : 'Open a file'}</button></div>
    </div>
    {(error || notice) && <div className={`${s.banner} ${error ? s.error : ''}`} role={error ? 'alert' : 'status'}><span>{error || notice}</span><IconButton icon={X} label="Dismiss message" onClick={() => { setError(''); setNotice(''); }} /></div>}
    <main className={s.main}>
      {view === 'home' ? <>
        <aside className={s.homeSidebar} aria-label="Home navigation"><h2>Home</h2>{['Recent', 'Starred'].map(item => <button key={item} className={section === item ? s.sidebarSelected : ''} aria-current={section === item ? 'true' : undefined} onClick={() => setSection(item)}>{item === 'Recent' ? <Home size={18} /> : <Star size={18} />}{item}</button>)}<div className={s.sidebarCaption}>FILES</div><button onClick={() => void open()}><FolderOpen size={18} /> Your computer</button><div className={s.sidebarBottom}><ShieldCheck size={16} /><span>Local files. Yours to keep.</span></div></aside>
        <section className={s.homeContent}><div className={s.homeHeading}><div><p className={s.eyebrow}>YOUR WORKSPACE</p><h1>Work with your PDFs.</h1></div><button className={s.outlineButton} disabled={newCopyModalOpen} onClick={() => { if (!newCopyModalOpen) setView('tools'); }}>See all tools <ArrowUpRight size={16} /></button></div>
          <div className={s.quickCards}>{['Edit a PDF', 'Export a PDF', 'Combine files', 'Fill forms'].map((name, i) => { const Icon = icons[name]; return <div className={s.quickCard} key={name}><span style={{ color: ['#7260b4', '#2c80b2', '#25876b', '#b86a25'][i] }}><Icon size={29} strokeWidth={1.5} /></span><h3>{name === 'Export a PDF' ? 'Export page as image' : name}</h3><p>{['Update text and images.', 'Save the current page as an image.', 'Bring documents together.', 'Fill supported existing text fields.'][i]}</p><span className={s.planned}>{name === 'Combine files' ? 'Available with two open PDFs' : name === 'Fill forms' || name === 'Export a PDF' ? 'Available with an open PDF' : 'Planned'}</span></div>; })}</div>
          <div className={s.recentHeader}><h2>{section}</h2><div className={s.recentControls}><button className={s.textButton} disabled={!recentFiles.length} onClick={() => setRecentFiles([])}>Clear file history</button><label className={s.search}><Search size={16} /><input aria-label="Search recent files" placeholder="Search your files" value={query} onChange={e => setQuery(e.target.value)} /></label><IconButton icon={List} label="List view" active /></div></div>
          <div className={s.tableHeading}><span>NAME</span><span>LOCATION</span><span>PAGES</span><span /></div>
          {listed.map(file => <div className={s.fileRow} key={file.path}><button disabled={busy} onClick={() => void reopen(file.path)}><FileImage size={25} /><span>{file.name}<small>PDF document</small></span></button><span title={file.path}>This computer</span><span>{file.pages}</span><IconButton icon={Star} label={`${file.starred ? 'Unstar' : 'Star'} ${file.name}`} active={file.starred} onClick={() => setRecentFiles(list => list.map(item => item.path === file.path ? { ...item, starred: !item.starred } : item))} /></div>)}
          {!listed.length && <div className={s.empty}><div className={s.emptyIcon}><Files size={36} strokeWidth={1.25} /></div><h3>{query ? 'No matching files' : section === 'Starred' ? 'Keep important files close' : 'Your documents start here'}</h3><p>{query ? 'Try another file name.' : section === 'Starred' ? 'Star an open file to find it here.' : 'Open a PDF from your computer to start reading.'}</p>{!query && section !== 'Starred' && <><button className={s.openButton} onClick={() => void open()} disabled={busy}>Open a file</button><button className={s.textButton} onClick={() => void open(true)} disabled={busy}>Explore a sample PDF <ChevronRight size={15} /></button></>}</div>}
          <p className={s.foundationNote}>Viewer + Organize Pages · More tools are in development{!native ? ' · Browser preview' : ''}</p>
        </section>
      </> : view === 'tools' ? <section className={s.toolsCatalog}><div className={s.catalogHeading}><div><p className={s.eyebrow}>THE COMPLETE WORKSPACE</p><h1>All tools</h1><p>Create a PDF, export a page as image, Combine Files, Organize Pages, Fill forms, and raster redaction are ready. OCR is available only when enabled in an opt-in source build. Other advanced tools are planned for later milestones.</p></div><label className={s.search}><Search size={16} /><input aria-label="Search tools" placeholder="Find a tool" value={query} onChange={e => setQuery(e.target.value)} /></label></div>{toolGroups.map(group => <section key={group.name}><h2>{group.name}</h2><div className={s.catalogGrid}>{group.tools.filter(name => name.toLowerCase().includes(query.toLowerCase())).map((name, i) => <div className={s.catalogCard} key={name}>{toolRow(name, i)}<span className={s.planned}>{name === 'Create a PDF' || name === 'Export a PDF' || name === 'Organize pages' || name === 'Combine files' || name === 'Fill forms' || name === 'Redact a PDF' || name === 'Scan & OCR' && ocr.available ? 'Available' : 'Not available yet'}</span></div>)}</div></section>)}</section> : doc ? <>
        {toolsOpen && <aside className={s.toolsPanel} aria-label="All tools"><div className={s.panelHeading}><h2>All tools</h2><IconButton icon={PanelLeftClose} label="Collapse all tools" disabled={newCopyModalOpen} onClick={() => {if(!newCopyModalOpen)setToolsOpen(false);}} /></div>{['Export a PDF', 'Edit a PDF', 'Create a PDF', 'Combine files', 'Organize pages', 'Comment', 'Fill forms', 'Redact a PDF', 'Scan & OCR', 'Protect a PDF', 'Compress a PDF'].map(toolRow)}<button className={s.textButton} disabled={newCopyModalOpen} onClick={() => { if (!newCopyModalOpen) setView('tools'); }}>View all tools <ChevronRight size={15} /></button><div className={s.panelNote}>Export page as image, Create a PDF, Combine Files, Organize Pages, Fill forms, and raster redaction are available. More tools are in development.</div></aside>}
        {!organizing && <h1 className={s.srOnly}>{doc.name}</h1>}
        {organizing ? <Organizer key={doc.id} document={doc} currentPage={page} pageLabels={pageLabels} busy={busy} edit={edit} save={save} split={split} crop={crop} resetCrop={resetCrop} insert={launchInsert} replace={launchReplace} close={() => setOrganizing(false)} /> : <div className={s.documentArea}><Viewer key={`${doc.id}-${doc.revision}`} document={doc} zoom={zoom} fit={fit} target={target} onPage={trackPage} onView={applyDestinationView} hand={hand} search={activeSearch} annotations={annotations?.status === 'supported' ? annotations.annotations : []} commentMode={commentMode} highlightMode={highlightMode} redactionPage={redactionDraft?.page ?? null} redactions={redactionDraft?.rectangles ?? []} annotationAvailable={annotations?.status === 'supported'} annotationInteractive={commentMode && !hand && !highlightMode && !redactionDraft} onCommentCreate={beginComment} onHighlightCreate={beginHighlight} onRedactionCreate={addRedaction} onAnnotationSelect={selectAnnotation} onTextSelection={receiveTextHighlightSelection} /><div className={s.quickToolbar}><IconButton icon={MousePointer2} label="Select text on page" disabled={newCopyModalOpen} active={!hand && !commentMode && !highlightMode && !redactionDraft} onClick={() => { if (newCopyModalOpen) return; setHand(false); setCommentMode(false); setHighlightMode(false); setRedactionDraft(null); }} /><IconButton icon={Highlighter} label="Highlight selected text" disabled={busy || !!redactionDraft || newCopyModalOpen || !textHighlightSelection || annotations?.status !== 'supported'} onPointerDown={captureTextHighlightSelection} onClick={beginTextHighlight} /><IconButton icon={Hand} label="Pan document" disabled={newCopyModalOpen} active={hand} onClick={() => { if (newCopyModalOpen) return; setHand(true); setCommentMode(false); setHighlightMode(false); setRedactionDraft(null); }} /><IconButton icon={Type} label="Read and copy page text" disabled={!!redactionDraft || newCopyModalOpen} onClick={() => { if (!newCopyModalOpen) setPageTextOpen(true); }} /><span className={s.horizontalDivider} /><IconButton icon={MessageSquare} label="Add comment" active={commentMode} disabled={busy || !!redactionDraft || newCopyModalOpen} onClick={launchCommentMode} /><IconButton icon={Highlighter} label="Add area highlight" active={highlightMode} disabled={busy || !!redactionDraft || newCopyModalOpen} onClick={launchHighlightMode} /><IconButton icon={Pencil} label="Draw" disabled /><IconButton icon={Type} label="Fill existing fields" disabled={busy || !!redactionDraft || newCopyModalOpen} onClick={launchForms} /><IconButton icon={Signature} label="Add signature" disabled /><span className={s.horizontalDivider} /><IconButton icon={MoreHorizontal} label="Customize quick tools" disabled /></div></div>}
        {!organizing && redactionDraft && doc && <RedactPanel page={redactionDraft.page} rectangles={redactionDraft.rectangles} busy={busy} remove={index => setRedactionDraft(draft => draft ? { ...draft, rectangles: draft.rectangles.filter((_, current) => current !== index) } : null)} clear={() => setRedactionDraft(draft => draft ? { ...draft, rectangles: [] } : null)} apply={() => void applyRedaction()} close={() => setRedactionDraft(null)} />}
        {!organizing && commentsOpen && doc && <CommentsPanel key={`${doc.id}-${doc.revision}`} document={doc} page={page} annotations={annotations} error={annotationsLoad.request === annotationsRequest ? annotationsLoad.error : ''} selectedId={commentEditor?.kind === 'edit' ? commentEditor.annotation.id : null} onAddComment={addCommentOnCurrentPage} onAddHighlight={addHighlightOnCurrentPage} onSelect={selectAnnotation} close={() => setCommentsOpen(false)} />}
        {!organizing && bookmarksOpen && <BookmarksPanel key={`${doc.id}-${doc.revision}`} document={doc} page={clampPage(page, doc.pages.length)} go={go} close={() => setBookmarksOpen(false)} onDocument={info => setDocuments(list => list.map(document => document.id === info.id ? info : document))} />}
        {!organizing && searchOpen && !bookmarksOpen && <SearchPanel key={`${doc.id}-${doc.revision}`} document={doc} go={go} close={closeSearch} onHighlights={receiveSearch} />}
        {!organizing && nav && !searchOpen && !bookmarksOpen && labelsOpen && <PageLabelsPanel key={`${doc.id}-${doc.revision}`} document={doc} page={clampPage(page, doc.pages.length)} close={() => setLabelsOpen(false)} onDocument={info => setDocuments(list => list.map(document => document.id === info.id ? info : document))} onChanged={() => setLabelsEpoch(value => value + 1)} />}
        {!organizing && nav && !searchOpen && !bookmarksOpen && !labelsOpen && <aside className={s.pagesPanel} aria-label="Pages"><div className={s.panelHeading}><h2>Pages</h2><IconButton icon={X} label="Close pages" onClick={() => setNav(false)} /></div><button onClick={() => setBookmarksOpen(true)}>Bookmarks</button><button onClick={() => setLabelsOpen(true)}>Page labels</button><PageList key={`${doc.id}:${doc.revision}:${doc.pages.length}`} document={doc} page={page} pageLabels={pageLabels} go={go} /></aside>}
        <aside className={s.rightRail} aria-label="Panels and page controls"><div><IconButton icon={MessageSquare} label="Comments" active={commentsOpen} disabled={busy} onClick={() => { closeSearch(); setBookmarksOpen(false); setOrganizing(false); setCommentsOpen(value => !value); }} /><IconButton icon={Bookmark} label="Bookmarks" active={bookmarksOpen} onClick={() => { closeSearch(); setCommentsOpen(false); setOrganizing(false); setBookmarksOpen(v => !v); }} /><IconButton icon={Files} label="Pages" active={nav && !bookmarksOpen && !searchOpen} onClick={() => { closeSearch(); setCommentsOpen(false); setBookmarksOpen(false); setNav(v => !v); }} /></div><div className={s.pageControls}><IconButton icon={ChevronLeft} label="Previous page" disabled={page === 0} onClick={() => go(page - 1)} /><input aria-label="Page number" key={`${doc.id}-${page}`} type="number" min={1} max={doc.pages.length} defaultValue={page + 1} onKeyDown={e => { if (e.key === 'Enter') go(Number(e.currentTarget.value) - 1); }} onBlur={e => go(Number(e.currentTarget.value) - 1)} /><span className={s.pageCount}>/ {doc.pages.length}</span><IconButton icon={ChevronRight} label="Next page" disabled={page === doc.pages.length - 1} onClick={() => go(page + 1)} /><span className={s.horizontalDivider} /><IconButton icon={RotateCw} label="Rotate view" disabled /><IconButton icon={Maximize} label="Fit width" active={fit === 'width'} onClick={() => setFit('width')} /><IconButton icon={Minimize2} label="Fit page" active={fit === 'page'} onClick={selectPageFit} /><IconButton icon={ZoomIn} label="Zoom in" onClick={() => changeZoom(zoom + 25)} /><IconButton icon={ZoomOut} label="Zoom out" onClick={() => changeZoom(zoom - 25)} /></div></aside>
      </> : null}
    </main>
    <footer className={s.statusbar}><span style={{ whiteSpace: 'pre-wrap', overflowWrap: 'anywhere' }}>{view === 'document' && doc ? (() => { const currentPage = clampPage(page, doc.pages.length); const label = pageLabelFor(pageLabels, currentPage); return <>Page {currentPage + 1}{label !== null && <> · Label: {pageLabelDescription(label)}</>} · {(doc.pages[currentPage].width / 72).toFixed(2)} × {(doc.pages[currentPage].height / 72).toFixed(2)} in</>; })() : 'PDF Workstation'}</span><span aria-live="polite">{busy ? 'Working…' : view === 'document' && doc ? `${doc.name} · ${doc.dirty ? 'Unsaved changes' : 'Source preserved'}` : 'Files stay on your computer'}</span>{view === 'document' ? <select aria-label="Zoom" value={fit === 'none' ? zoom : fit} onChange={e => e.target.value === 'width' ? setFit('width') : e.target.value === 'page' ? selectPageFit() : changeZoom(Number(e.target.value))}><option value="width">Fit width</option><option value="page">Fit page</option>{Array.from(new Set([10,25,50,75,100,125,150,200,300,400,zoom])).sort((a,b) => a-b).map(z => <option value={z} key={z}>{z}%</option>)}</select> : <span>Local workspace</span>}</footer>
    {recoveryLocked.size > 0 ? (() => { const locked = documents.find(document => recoveryLocked.has(document.id)); return locked ? <ConfirmDialog title="Recovery confirmation failed" message="This PDF is locked because an edit may already be saved in recovery. Close it, then reopen the source PDF and choose Keep recovered edits or Open original." confirmLabel="Retry closing tab" onCancel={() => setError('Close this recovery-locked PDF before continuing.')} onConfirm={() => void close(locked.id)} /> : null; })() : pendingClose !== null && <ConfirmDialog title="Discard unsaved edits?" message="Page and bookmark edits that are not in a saved copy will be lost. Save a copy before closing to keep your changes. Your original PDF has not been modified." confirmLabel="Discard and close" onCancel={() => setPendingClose(null)} onConfirm={() => { const pending = pendingClose; if (pending === 'window') void discardAndCloseWindow(); else { setPendingClose(null); void close(pending, true); } }} />}
  </div>;
}
