import { memo, useEffect, useLayoutEffect, useMemo, useRef, useState, type RefObject } from 'react';
import { pageTextGeometry, type PageTextGeometry } from './bridge';
import { matchedCharacterIndexes, type SearchHighlightQuery } from './searchHighlights';
import { textHighlightFromBrowserSelection, type TextHighlightSelection, type TextHighlightSelectionSource } from './textHighlightSelection';
import styles from './TextLayer.module.css';

const MAX_GLYPHS = 20_000;
type State = { request: string; kind: 'loading' } | { request: string; kind: 'ready'; geometry: PageTextGeometry } | { request: string; kind: 'fallback'; message: string; suggestPageText: boolean };
type Bounds = NonNullable<PageTextGeometry['characters'][number]['bounds']>;

function geometryProblem(geometry: PageTextGeometry): string | null {
  if (geometry.status !== 'ok') return geometry.reason || 'This page does not provide positioned text.';
  if (geometry.truncated) return geometry.reason || 'This page has more text than can be positioned safely.';
  if (geometry.characters.length > MAX_GLYPHS) return 'This page has more text than can be positioned safely.';
  if (!geometry.characters.some(character => character.bounds)) return 'This page has no positioned embedded text.';
  for (const character of geometry.characters) {
    const bounds = character.bounds;
    if (!bounds) continue;
    if (!Number.isFinite(bounds.x) || !Number.isFinite(bounds.y) || !Number.isFinite(bounds.width) || !Number.isFinite(bounds.height) || bounds.x < 0 || bounds.y < 0 || bounds.width <= 0 || bounds.height <= 0 || bounds.x > 1 || bounds.y > 1 || bounds.x + bounds.width > 1.0001 || bounds.y + bounds.height > 1.0001 || ![0, 90, 180, 270].includes(character.angle)) return 'This page returned invalid text positions.';
  }
  return null;
}

function targetBox(bounds: Bounds, pageWidth: number, pageHeight: number) {
  const targetWidth = (Math.min(1, bounds.x + bounds.width) - bounds.x) * pageWidth;
  const targetHeight = (Math.min(1, bounds.y + bounds.height) - bounds.y) * pageHeight;
  return { left: bounds.x * pageWidth, top: bounds.y * pageHeight, width: targetWidth, height: targetHeight };
}

const INITIAL_GLYPH_STYLE = { left: '0px', top: '0px', fontSize: '1px' };

const GlyphSpans = memo(function GlyphSpans({ characters }: { characters: PageTextGeometry['characters'] }) {
  return <>{characters.map((character, index) => character.bounds
    ? <span key={index} className={styles.glyph} data-angle={character.angle} data-geometry-index={index} style={INITIAL_GLYPH_STYLE}>{character.text}</span>
    : <span className={styles.unpositioned} data-geometry-index={index} key={index}>{character.text}</span>)}</>;
});

type Placed = { node: HTMLElement; baseLeft: number; baseTop: number; left: number; top: number };

function layoutGlyphs(layer: HTMLElement, characters: PageTextGeometry['characters'], pageWidth: number, pageHeight: number) {
  const nodes = layer.children;
  const range = document.createRange();
  const pending: Array<{ node: HTMLElement; bounds: Bounds; angle: number }> = [];
  for (let index = 0; index < characters.length; index++) {
    const bounds = characters[index].bounds, node = nodes[index] as HTMLElement | undefined;
    if (!bounds || !node) continue;
    node.style.left = '0px'; node.style.top = '0px'; node.style.transform = '';
    pending.push({ node, bounds, angle: characters[index].angle });
  }
  const natural = pending.map(item => { range.selectNodeContents(item.node); return range.getBoundingClientRect(); });
  const placed: Placed[] = [];
  pending.forEach((item, position) => {
    const rect = natural[position];
    if (!rect.width || !rect.height) return;
    const target = targetBox(item.bounds, pageWidth, pageHeight);
    const width = item.angle % 180 ? target.height : target.width;
    const height = item.angle % 180 ? target.width : target.height;
    item.node.style.left = `${target.left}px`; item.node.style.top = `${target.top}px`;
    item.node.style.transform = `${item.angle ? `rotate(${item.angle}deg) ` : ''}scale(${width / rect.width}, ${height / rect.height})`;
    placed.push({ node: item.node, baseLeft: target.left, baseTop: target.top, left: target.left, top: target.top });
  });
  for (let pass = 0; pass < 2 && placed.length; pass++) {
    const parent = layer.getBoundingClientRect();
    const rects = placed.map(item => { range.selectNodeContents(item.node); return range.getBoundingClientRect(); });
    let moved = false;
    placed.forEach((item, position) => {
      const deltaX = parent.left + item.baseLeft - rects[position].left, deltaY = parent.top + item.baseTop - rects[position].top;
      if (Math.abs(deltaX) <= .1 && Math.abs(deltaY) <= .1) return;
      item.left += deltaX; item.top += deltaY; moved = true;
      item.node.style.left = `${item.left}px`; item.node.style.top = `${item.top}px`;
    });
    if (!moved) break;
  }
}

const GlyphLayer = memo(function GlyphLayer({ layerRef, request, geometry, pageWidth, pageHeight }: { layerRef: RefObject<HTMLDivElement>; request: string; geometry: PageTextGeometry; pageWidth: number; pageHeight: number }) {
  useLayoutEffect(() => {
    const layer = layerRef.current;
    if (layer) layoutGlyphs(layer, geometry.characters, pageWidth, pageHeight);
  }, [geometry, layerRef, pageHeight, pageWidth]);
  return <div ref={layerRef} className={styles.layer} aria-hidden="true" data-testid="text-layer" data-text-layer={request}><GlyphSpans characters={geometry.characters} /></div>;
});

const SearchHighlights = memo(function SearchHighlights({ geometry, indexes, pageWidth, pageHeight }: { geometry: PageTextGeometry; indexes: Set<number>; pageWidth: number; pageHeight: number }) {
  return <div className={styles.highlightLayer} aria-hidden="true" data-testid="search-highlights">
    {geometry.characters.map((character, index) => character.bounds && indexes.has(index) ? <span key={index} className={styles.highlight} data-testid="search-highlight" style={targetBox(character.bounds, pageWidth, pageHeight)} /> : null)}
  </div>;
});

function TextLayer({ id, page, revision, imageReady, enabled, pageWidth, pageHeight, search, onTextSelection }: { id: number; page: number; revision: number; imageReady: boolean; enabled: boolean; pageWidth: number; pageHeight: number; search?: SearchHighlightQuery; onTextSelection?: (selection: TextHighlightSelection | null, source: TextHighlightSelectionSource) => void }) {
  const request = `${id}:${page}:${revision}`;
  const [state, setState] = useState<State>({ request: '', kind: 'loading' });
  const layer = useRef<HTMLDivElement>(null);
  const sentSelection = useRef(false);
  const needsGeometry = enabled || Boolean(search);
  useEffect(() => {
    if (!needsGeometry || !imageReady) return;
    let disposed = false;
    setState({ request, kind: 'loading' });
    pageTextGeometry(id, page, revision).then(geometry => {
      if (disposed || geometry.id !== id || geometry.page !== page || geometry.revision !== revision) return;
      const problem = geometryProblem(geometry);
      setState(problem ? { request, kind: 'fallback', message: problem, suggestPageText: true } : { request, kind: 'ready', geometry });
    }).catch(error => { if (!disposed) setState({ request, kind: 'fallback', message: String(error), suggestPageText: false }); });
    return () => { disposed = true; };
  }, [id, imageReady, needsGeometry, page, request, revision]);

  useEffect(() => {
    const element = layer.current;
    if (!enabled || !onTextSelection || !element || state.request !== request || state.kind !== 'ready') return;
    const context = { id, page, revision };
    const changed = () => {
      const selection = textHighlightFromBrowserSelection(document.getSelection(), element, context);
      if (selection) { sentSelection.current = true; onTextSelection(selection, context); }
      else if (sentSelection.current) { sentSelection.current = false; onTextSelection(null, context); }
    };
    document.addEventListener('selectionchange', changed);
    changed();
    return () => {
      document.removeEventListener('selectionchange', changed);
      if (sentSelection.current) { sentSelection.current = false; onTextSelection(null, context); }
    };
  }, [enabled, id, onTextSelection, page, request, revision, state]);

  const readyGeometry = state.kind === 'ready' && state.request === request ? state.geometry : null;
  const highlightIndexes = useMemo(() => readyGeometry && search ? matchedCharacterIndexes(readyGeometry.characters, search) : null, [readyGeometry, search]);

  if (!needsGeometry || !imageReady || state.request !== request || state.kind === 'loading') return null;
  if (state.kind === 'fallback') return <div className={styles.fallback} role="status">{search ? 'On-page search highlights are unavailable.' : 'On-page text selection is unavailable.'} {state.message}{state.suggestPageText ? ' Use Read and copy page text instead.' : null}</div>;
  const hasPositionedHighlight = highlightIndexes ? [...highlightIndexes].some(index => Boolean(state.geometry.characters[index].bounds)) : false;
  return <>
    {search && (hasPositionedHighlight
      ? <SearchHighlights geometry={state.geometry} indexes={highlightIndexes!} pageWidth={pageWidth} pageHeight={pageHeight} />
      : <div className={styles.fallback} role="status">On-page search highlights are unavailable for this match. Use Find results or Read and copy page text instead.</div>)}
    {enabled && <GlyphLayer layerRef={layer} request={request} geometry={state.geometry} pageWidth={pageWidth} pageHeight={pageHeight} />}
  </>;
}

export default memo(TextLayer);
