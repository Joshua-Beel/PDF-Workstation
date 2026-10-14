import { useEffect, useLayoutEffect, useMemo, useRef, useState } from 'react';
import { renderPage, type Annotation, type CommentRect } from './bridge';
import { clampPage, currentPageAt, fitPageScale, maxPageWidth, pageLayout, scaleAnchoredTop, visiblePageRange, type DocumentInfo } from './model';
import type { FitMode } from './preferences';
import { planDestination, type BookmarkDestination, type DestinationPlan } from './bookmarkDestination';
import TextLayer from './TextLayer';
import CommentLayer from './CommentLayer';
import type { TextHighlightSelection, TextHighlightSelectionSource } from './textHighlightSelection';
import type { ActiveSearch } from './SearchPanel';
import styles from './Workspace.module.css';

function Page({ id, index, width, height, scale, revision, selectable, search, annotations, commentMode, highlightMode, redactionMode, redactions, annotationAvailable, annotationInteractive, onCommentCreate, onHighlightCreate, onRedactionCreate, onAnnotationSelect, onTextSelection }: { id: number; index: number; width: number; height: number; scale: number; revision: number; selectable: boolean; search?: ActiveSearch; annotations: Annotation[]; commentMode: boolean; highlightMode: boolean; redactionMode: boolean; redactions: CommentRect[]; annotationAvailable: boolean; annotationInteractive: boolean; onCommentCreate: (page: number, rect: CommentRect) => void; onHighlightCreate: (page: number, rect: CommentRect) => void; onRedactionCreate: (page: number, rect: CommentRect) => void; onAnnotationSelect: (annotation: Annotation) => void; onTextSelection?: (selection: TextHighlightSelection | null, source: TextHighlightSelectionSource) => void }) {
  const image = useRef<HTMLImageElement>(null);
  const [url, setUrl] = useState('');
  const [error, setError] = useState('');
  const [imageReady, setImageReady] = useState(false);
  useEffect(() => {
    let disposed = false;
    let objectUrl = '';
    setUrl(''); setError(''); setImageReady(false);
    const timer = window.setTimeout(() => {
      renderPage(id, index, Math.min(3000, Math.round(width * scale * window.devicePixelRatio))).then(result => {
        objectUrl = result;
        if (disposed) URL.revokeObjectURL(result); else setUrl(result);
      }).catch(e => { if (!disposed) setError(String(e)); });
    }, 35);
    return () => { disposed = true; clearTimeout(timer); if (objectUrl) URL.revokeObjectURL(objectUrl); };
  }, [id, index, width, scale, revision]);
  return <div className={styles.paper} style={{ width: width * scale, height: height * scale }} aria-label={`Page ${index + 1}`}>
    {url ? <><img ref={image} src={url} alt={`Page ${index + 1}`} draggable={false} onLoad={() => setImageReady(true)} /><TextLayer id={id} page={index} revision={revision} enabled={selectable} imageReady={imageReady} pageWidth={width * scale} pageHeight={height * scale} search={search} onTextSelection={onTextSelection} />{imageReady && <CommentLayer page={index} pageWidth={width} pageHeight={height} annotations={annotations} creatingComment={commentMode && annotationAvailable} creatingHighlight={highlightMode && annotationAvailable} creatingRedaction={redactionMode} redactions={redactions} interactive={annotationInteractive && annotationAvailable} onCommentCreate={onCommentCreate} onHighlightCreate={onHighlightCreate} onRedactionCreate={onRedactionCreate} onSelect={onAnnotationSelect} imageBounds={() => image.current?.getBoundingClientRect() ?? null} layoutKey={`${revision}:${width * scale}:${height * scale}`} />}</> : <div className={styles.pageLoading}>{error || `Rendering page ${index + 1}…`}</div>}
  </div>;
}

export default function Viewer({ document, zoom, fit, target, onPage, onView, hand, search, annotations = [], commentMode = false, highlightMode = false, redactionPage = null, redactions = [], annotationAvailable = false, annotationInteractive = false, onCommentCreate = () => {}, onHighlightCreate = () => {}, onRedactionCreate = () => {}, onAnnotationSelect = () => {}, onTextSelection }: { document: DocumentInfo; zoom: number; fit: FitMode; target: { page: number; token: number; destination?: BookmarkDestination | null }; onPage: (page: number) => void; onView?: (fit: FitMode, zoom: number) => void; hand: boolean; search?: ActiveSearch | null; annotations?: Annotation[]; commentMode?: boolean; highlightMode?: boolean; redactionPage?: number | null; redactions?: CommentRect[]; annotationAvailable?: boolean; annotationInteractive?: boolean; onCommentCreate?: (page: number, rect: CommentRect) => void; onHighlightCreate?: (page: number, rect: CommentRect) => void; onRedactionCreate?: (page: number, rect: CommentRect) => void; onAnnotationSelect?: (annotation: Annotation) => void; onTextSelection?: (selection: TextHighlightSelection | null, source: TextHighlightSelectionSource) => void }) {
  const viewport = useRef<HTMLDivElement>(null);
  const [bounds, setBounds] = useState({ width: 900, height: 800 });
  const [top, setTop] = useState(0);
  const drag = useRef<{ x: number; y: number; top: number; left: number } | null>(null);
  useEffect(() => {
    const element = viewport.current!;
    const observer = new ResizeObserver(([entry]) => setBounds({ width: entry.contentRect.width, height: entry.contentRect.height }));
    observer.observe(element); return () => observer.disconnect();
  }, []);
  const maxWidth = useMemo(() => maxPageWidth(document.pages), [document]);
  const targetPage = clampPage(target.page, document.pages.length);
  const scale = fit === 'width' ? Math.max(0.1, (bounds.width - 144) / maxWidth) : fit === 'page' ? fitPageScale(document.pages[targetPage], bounds.width, bounds.height) : zoom / 100 * 96 / 72;
  const layout = useMemo(() => pageLayout(document.pages, scale, maxWidth), [document, scale, maxWidth]);
  const previousLayout = useRef({ layout, scale });
  const pendingView = useRef<DestinationPlan | null>(null);
  const range = visiblePageRange(layout, top, bounds.height);
  const visible = Array.from({ length: range.end - range.start }, (_, index) => range.start + index);
  useLayoutEffect(() => {
    const element = viewport.current!;
    const old = previousLayout.current;
    if (old.scale !== scale) {
      element.scrollTop = scaleAnchoredTop(old.layout, layout, old.scale, scale, element.scrollTop);
      setTop(element.scrollTop);
    }
    const pending = pendingView.current;
    if (pending && old.scale !== scale) {
      pendingView.current = null;
      if (Math.abs(pending.scale - scale) < 1e-9) {
        element.scrollTop = pending.top;
        if (pending.left !== null) element.scrollLeft = pending.left;
        setTop(element.scrollTop);
      }
    }
    previousLayout.current = { layout, scale };
  }, [layout, scale]);
  useEffect(() => {
    pendingView.current = null;
    if (!target.destination) { viewport.current?.scrollTo({ top: layout.offsets[targetPage] - 24 }); return; }
    const plan = planDestination(target.destination, document.pages, targetPage, bounds, { fit, zoom, scale }, maxWidth);
    if (plan.fit !== fit || plan.zoom !== zoom) onView?.(plan.fit, plan.zoom);
    if (Math.abs(plan.scale - scale) < 1e-9) viewport.current?.scrollTo(plan.left === null ? { top: plan.top } : { top: plan.top, left: plan.left });
    else pendingView.current = plan;
  }, [target]);
  return <div ref={viewport} className={`${styles.viewport} ${hand ? styles.hand : ''}`} onScroll={event => {
    const scrollTop = event.currentTarget.scrollTop;
    setTop(scrollTop);
    onPage(currentPageAt(layout, scrollTop + 80));
  }} onPointerDown={event => {
    if (!hand || event.button !== 0) return;
    const element = viewport.current!;
    drag.current = { x: event.clientX, y: event.clientY, top: element.scrollTop, left: element.scrollLeft };
    element.setPointerCapture(event.pointerId);
  }} onPointerMove={event => {
    if (!drag.current) return;
    viewport.current!.scrollTop = drag.current.top - event.clientY + drag.current.y;
    viewport.current!.scrollLeft = drag.current.left - event.clientX + drag.current.x;
  }} onPointerUp={() => { drag.current = null; }} onLostPointerCapture={() => { drag.current = null; }}>
    <div className={styles.pageStack} style={{ height: layout.height, minWidth: layout.maxWidth * scale + 144 }}>
      {visible.map(index => <div key={`${document.id}-${index}`} className={styles.pagePosition} style={{ top: layout.offsets[index] }}>
        <Page id={document.id} index={index} {...document.pages[index]} scale={scale} revision={document.revision} selectable={!hand && !commentMode && !highlightMode && redactionPage === null} search={search?.documentId === document.id && search.revision === document.revision && search.pages.includes(index) ? search : undefined} annotations={annotations.filter(annotation => annotation.page === index && annotation.rect)} commentMode={commentMode} highlightMode={highlightMode} redactionMode={redactionPage === index} redactions={redactionPage === index ? redactions : []} annotationAvailable={annotationAvailable} annotationInteractive={annotationInteractive} onCommentCreate={onCommentCreate} onHighlightCreate={onHighlightCreate} onRedactionCreate={onRedactionCreate} onAnnotationSelect={onAnnotationSelect} onTextSelection={onTextSelection} />
      </div>)}
    </div>
  </div>;
}
