import { act, create, type ReactTestRenderer } from 'react-test-renderer';
import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import Viewer from './Viewer';
import { pageTextGeometry, renderPage, retryPdfEngine, type PageTextGeometry } from './bridge';
import type { DocumentInfo } from './model';

const report = (line: string) => (globalThis as unknown as { process: { stderr: { write(text: string): void } } }).process.stderr.write(line);
vi.mock('./bridge', () => ({ renderPage: vi.fn(), retryPdfEngine: vi.fn(), pageTextGeometry: vi.fn() }));
vi.mock('./CommentLayer', () => ({ default: () => null }));

const COLUMNS = 70, ROWS = 49, GLYPHS = COLUMNS * ROWS;
const geometry = (id: number, page: number): PageTextGeometry => ({
  id, page, revision: 1, status: 'ok', truncated: false, reason: null,
  characters: Array.from({ length: GLYPHS }, (_, index) => ({ text: String.fromCharCode(97 + index % 26), angle: 0, bounds: { x: .05 + index % COLUMNS * .0129, y: .04 + Math.floor(index / COLUMNS) * .019, width: .0125, height: .016 } }))
});
const info: DocumentInfo = { id: 9, revision: 1, name: 'dense.pdf', path: 'C:/dense.pdf', pages: Array.from({ length: 40 }, () => ({ width: 612, height: 300 })), dirty: false, can_undo: false, can_redo: false };

let reads = 0;
const rect = () => { reads++; return { left: 0, top: 0, right: 8, bottom: 8, width: 8, height: 8 }; };
const spans = Array.from({ length: GLYPHS }, () => ({ style: {} as Record<string, string>, getBoundingClientRect: rect, parentElement: { getBoundingClientRect: rect } }));
const viewport = { scrollTop: 0, scrollLeft: 0, scrollTo: vi.fn(), setPointerCapture: vi.fn() };
const nodeMock = (element: { type: unknown; props: Record<string, unknown> }) => {
  if (element.props['data-testid'] === 'text-layer') return { get children() { return spans; }, isConnected: true, getBoundingClientRect: rect };
  if (element.type === 'span') return spans[Number(element.props['data-geometry-index']) || 0];
  return viewport;
};
const scrollable = (ui: ReactTestRenderer) => ui.root.findAll(node => typeof node.props.onScroll === 'function')[0];
const median = (values: number[]) => [...values].sort((a, b) => a - b)[Math.floor(values.length / 2)];
const flush = async () => { await act(async () => { vi.advanceTimersByTime(40); for (let i = 0; i < 6; i++) await Promise.resolve(); }); };

beforeEach(() => {
  vi.useFakeTimers({ toFake: ['setTimeout', 'clearTimeout'] }); vi.clearAllMocks(); reads = 0;
  vi.mocked(renderPage).mockResolvedValue('blob:test');
  vi.mocked(retryPdfEngine).mockResolvedValue(undefined);
  vi.mocked(pageTextGeometry).mockImplementation(async (id, page) => geometry(id, page));
  vi.stubGlobal('window', { devicePixelRatio: 1, setTimeout, clearTimeout });
  vi.stubGlobal('document', { addEventListener() {}, removeEventListener() {}, getSelection: () => null, createRange: () => ({ selectNodeContents() {}, getBoundingClientRect: rect }) });
  vi.stubGlobal('URL', { revokeObjectURL: vi.fn() });
  vi.stubGlobal('ResizeObserver', class { observe() {} disconnect() {} });
});
afterEach(() => { vi.useRealTimers(); vi.unstubAllGlobals(); });

it('keeps scrolling off the glyph layer: no layout reads and no glyph re-render while the visible pages are unchanged', async () => {
  const view = () => <Viewer document={info} zoom={100} fit="none" target={{ page: 0, token: 0 }} onPage={() => {}} hand={false} annotations={[]} redactions={[]} onCommentCreate={() => {}} onHighlightCreate={() => {}} onRedactionCreate={() => {}} onAnnotationSelect={() => {}} onTextSelection={() => {}} />;
  let ui!: ReactTestRenderer;
  const mountStart = performance.now();
  act(() => { ui = create(view(), { createNodeMock: nodeMock }); });
  await flush();
  for (const image of ui.root.findAllByType('img')) act(() => image.props.onLoad());
  await flush();
  const mountMs = performance.now() - mountStart;
  const mountedPages = ui.root.findAllByProps({ 'data-testid': 'text-layer' }).length;
  const mountReads = reads;
  expect(mountedPages).toBeGreaterThanOrEqual(3);
  reads = 0;
  const samples: number[] = [];
  for (let step = 1; step <= 40; step++) {
    const start = performance.now();
    act(() => { scrollable(ui).props.onScroll({ currentTarget: { scrollTop: step * 3 } }); ui.update(view()); });
    samples.push(performance.now() - start);
  }
  const scrollReads = reads;
  report(`SCROLL-PROXY pages=${mountedPages} glyphs/page=${GLYPHS} mount+layout=${mountMs.toFixed(0)}ms mountLayoutReads=${mountReads} scroll p50=${median(samples).toFixed(1)}ms max=${Math.max(...samples).toFixed(1)}ms scrollLayoutReads=${scrollReads}
`);
  expect(scrollReads).toBe(0);
  act(() => ui.unmount());
}, 120_000);
