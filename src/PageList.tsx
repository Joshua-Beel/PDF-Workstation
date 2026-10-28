import { Fragment, useLayoutEffect, useRef, useState, type KeyboardEvent } from 'react';
import { File } from 'lucide-react';
import type { DocumentInfo } from './model';
import { pageLabelDescription, pageLabelFor, type DocumentPageLabels } from './pageLabels';
import s from './PageList.module.css';

export const PAGE_LIST_ROW_HEIGHT = 96;
export const PAGE_LIST_OVERSCAN = 6;

const boundedPage = (page: number | null, count: number) => Number.isSafeInteger(page) && page !== null && page >= 0 && page < count ? page : null;

export function pageListWindow(count: number, scrollTop: number, viewportHeight: number, current: number, focused: number | null): number[] {
  if (!Number.isSafeInteger(count) || count < 1) return [];
  const safeTop = Number.isFinite(scrollTop) ? Math.max(0, scrollTop) : 0;
  const estimatedRows = Number.isFinite(viewportHeight) && viewportHeight > 0 ? Math.ceil(viewportHeight / PAGE_LIST_ROW_HEIGHT) : 6;
  const visibleRows = Math.max(1, estimatedRows);
  const first = Math.max(0, Math.floor(safeTop / PAGE_LIST_ROW_HEIGHT) - PAGE_LIST_OVERSCAN);
  const last = Math.min(count, first + visibleRows + PAGE_LIST_OVERSCAN * 2);
  const pages = new Set<number>();
  for (let page = first; page < last; page++) pages.add(page);
  const active = boundedPage(current, count), retainedFocus = boundedPage(focused, count);
  if (active !== null) pages.add(active);
  if (retainedFocus !== null) pages.add(retainedFocus);
  return [...pages].sort((left, right) => left - right);
}

export function pageListGaps(pages: number[]): { before: number; page: number }[] {
  let previous = 0;
  return pages.map(page => {
    const before = Math.max(0, page - previous);
    previous = page + 1;
    return { before, page };
  });
}

export default function PageList({ document, page, pageLabels, go }: { document: DocumentInfo; page: number; pageLabels: DocumentPageLabels | null; go: (page: number) => void }) {
  const list = useRef<HTMLDivElement>(null);
  const pendingFocus = useRef<number | null>(null);
  const [scrollTop, setScrollTop] = useState(0);
  const [viewportHeight, setViewportHeight] = useState(PAGE_LIST_ROW_HEIGHT * 6);
  const [focused, setFocused] = useState<number | null>(null);
  const count = document.pages.length;
  const pages = pageListWindow(count, scrollTop, viewportHeight, page, focused);
  const gaps = pageListGaps(pages);
  const tail = pages.length ? Math.max(0, count - pages[pages.length - 1] - 1) : count;
  const updateViewport = (element: HTMLDivElement) => setViewportHeight(Math.max(PAGE_LIST_ROW_HEIGHT, element.clientHeight || PAGE_LIST_ROW_HEIGHT * 6));
  const scrollToPage = (target: number) => {
    const element = list.current;
    if (!element) return;
    const height = Math.max(PAGE_LIST_ROW_HEIGHT, element.clientHeight || viewportHeight);
    const rowTop = target * PAGE_LIST_ROW_HEIGHT, rowBottom = rowTop + PAGE_LIST_ROW_HEIGHT;
    if (rowTop < element.scrollTop || rowBottom > element.scrollTop + height) {
      const next = Math.max(0, rowTop - Math.max(0, (height - PAGE_LIST_ROW_HEIGHT) / 2));
      element.scrollTop = next;
      setScrollTop(next);
    }
  };
  useLayoutEffect(() => {
    const element = list.current;
    if (!element) return;
    updateViewport(element);
    if (typeof ResizeObserver === 'undefined') return;
    const observer = new ResizeObserver(() => updateViewport(element));
    observer.observe(element);
    return () => observer.disconnect();
  }, []);
  useLayoutEffect(() => { scrollToPage(page); }, [page]);
  useLayoutEffect(() => {
    const target = pendingFocus.current;
    if (target === null) return;
    pendingFocus.current = null;
    list.current?.querySelector<HTMLButtonElement>(`[data-page-index="${target}"]`)?.focus();
  }, [pages]);
  const moveFocus = (target: number) => {
    const next = Math.max(0, Math.min(count - 1, target));
    pendingFocus.current = next;
    setFocused(next);
    scrollToPage(next);
  };
  const keyDown = (event: KeyboardEvent<HTMLButtonElement>, index: number) => {
    const current = focused ?? index;
    if (event.key === 'ArrowUp') { event.preventDefault(); event.stopPropagation(); moveFocus(current - 1); }
    else if (event.key === 'ArrowDown') { event.preventDefault(); event.stopPropagation(); moveFocus(current + 1); }
    else if (event.key === 'Home') { event.preventDefault(); event.stopPropagation(); moveFocus(0); }
    else if (event.key === 'End') { event.preventDefault(); event.stopPropagation(); moveFocus(count - 1); }
    else if (event.key === 'Enter' || event.key === ' ') { event.preventDefault(); event.stopPropagation(); go(index); }
  };
  return <div ref={list} className={s.list} onScroll={event => setScrollTop(event.currentTarget.scrollTop)} role="group" aria-label="Page list">
    {gaps.map(({ before, page: index }) => {
      const size = document.pages[index], label = pageLabelFor(pageLabels, index), description = label === null ? null : pageLabelDescription(label);
      return <Fragment key={index}>
        {before > 0 && <span className={s.spacer} aria-hidden="true" style={{ height: before * PAGE_LIST_ROW_HEIGHT }} />}
        <span className={s.row} style={{ height: PAGE_LIST_ROW_HEIGHT }}><button data-page-index={index} className={index === page ? s.current : ''} aria-current={index === page ? 'page' : undefined} tabIndex={focused === null ? (index === page ? 0 : -1) : (index === focused ? 0 : -1)} title={description === null ? `Page ${index + 1}` : `Label: ${description}`} onFocus={() => setFocused(index)} onKeyDown={event => keyDown(event, index)} onClick={() => go(index)}><File size={24} /><span className={s.contents}><span>Page {index + 1}</span>{description !== null && <small className={s.label}>Label: {description}</small>}<small className={s.size}>{(size.width / 72).toFixed(1)} × {(size.height / 72).toFixed(1)} in</small></span></button></span>
      </Fragment>;
    })}
    {tail > 0 && <span className={s.spacer} aria-hidden="true" style={{ height: tail * PAGE_LIST_ROW_HEIGHT }} />}
  </div>;
}
