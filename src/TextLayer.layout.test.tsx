import { act, create } from 'react-test-renderer';
import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { pageTextGeometry, type PageTextGeometry } from './bridge';
import TextLayer from './TextLayer';

vi.mock('./bridge', () => ({ pageTextGeometry: vi.fn() }));

type Fake = { style: Record<string, string>; natural: { width: number; height: number }; offset: { left: number; top: number } };
const fake = (): Fake => ({ style: {}, natural: { width: 4, height: 8 }, offset: { left: 0, top: 0 } });
let spans: Fake[] = [];
let reads = 0;
const geometry: PageTextGeometry = {
  id: 1, page: 0, revision: 1, status: 'ok', truncated: false, reason: null,
  characters: [
    { text: 'A', bounds: { x: .1, y: .2, width: .05, height: .04 }, angle: 0 },
    { text: ' ', bounds: null, angle: 0 },
    { text: 'B', bounds: { x: .5, y: .5, width: .04, height: .1 }, angle: 90 }
  ]
};
const scaleOf = (span: Fake) => { const match = /scale\(([^,]+), ([^)]+)\)/.exec(span.style.transform)!; return [Number(Number(match[1]).toFixed(6)), Number(Number(match[2]).toFixed(6))]; };
const rectOf = (span: Fake) => {
  reads++;
  const matrix = /scale\(([^,]+), ([^)]+)\)/.exec(span.style.transform ?? '');
  const scaleX = matrix ? Number(matrix[1]) : 1, scaleY = matrix ? Number(matrix[2]) : 1;
  const left = Number.parseFloat(span.style.left ?? '0') + span.offset.left, top = Number.parseFloat(span.style.top ?? '0') + span.offset.top;
  return { left, top, width: span.natural.width * scaleX, height: span.natural.height * scaleY };
};

beforeEach(() => {
  spans = [fake(), fake(), fake()]; spans[0].offset = { left: 2, top: 3 }; reads = 0;
  vi.mocked(pageTextGeometry).mockResolvedValue(geometry);
  let current: Fake | null = null;
  vi.stubGlobal('document', { addEventListener() {}, removeEventListener() {}, getSelection: () => null, createRange: () => ({ selectNodeContents(node: Fake) { current = node; }, getBoundingClientRect: () => rectOf(current!) }) });
});
afterEach(() => vi.unstubAllGlobals());

it('sizes and places every positioned glyph in one batched pass without per-glyph state, correcting offsets at most twice', async () => {
  const layer = { get children() { return spans; }, getBoundingClientRect: () => ({ left: 10, top: 20 }) };
  let ui!: ReturnType<typeof create>;
  await act(async () => { ui = create(<TextLayer id={1} page={0} revision={1} enabled imageReady pageWidth={400} pageHeight={500} />, { createNodeMock: element => element.props['data-testid'] === 'text-layer' ? layer : null }); });
  expect(scaleOf(spans[0])).toEqual([5, 2.5]);
  expect(spans[2].style.transform).toMatch(/^rotate\(90deg\) scale\(/);
  expect(scaleOf(spans[2])).toEqual([12.5, 2]);
  expect(Number.parseFloat(spans[0].style.left)).toBeCloseTo(40 - 2 + 10);
  expect(Number.parseFloat(spans[0].style.top)).toBeCloseTo(100 - 3 + 20);
  expect(spans[1].style).toEqual({});
  const afterMount = reads;
  await act(async () => { ui.update(<TextLayer id={1} page={0} revision={1} enabled imageReady pageWidth={400} pageHeight={500} search={undefined} onTextSelection={undefined} />); });
  expect(reads).toBe(afterMount);
  act(() => ui.unmount());
});

it('does not touch glyphs again when only an unrelated parent re-render happens, and re-lays out when the page size changes', async () => {
  const layer = { get children() { return spans; }, getBoundingClientRect: () => ({ left: 0, top: 0 }) };
  let ui!: ReturnType<typeof create>;
  const node = { createNodeMock: (element: { props: Record<string, unknown> }) => element.props['data-testid'] === 'text-layer' ? layer : null };
  await act(async () => { ui = create(<TextLayer id={1} page={0} revision={1} enabled imageReady pageWidth={400} pageHeight={500} />, node); });
  const afterMount = reads;
  await act(async () => { ui.update(<TextLayer id={1} page={0} revision={1} enabled imageReady pageWidth={400} pageHeight={500} />); });
  expect(reads).toBe(afterMount);
  await act(async () => { ui.update(<TextLayer id={1} page={0} revision={1} enabled imageReady pageWidth={800} pageHeight={1000} />); });
  expect(reads).toBeGreaterThan(afterMount);
  expect(scaleOf(spans[0])).toEqual([10, 5]);
  act(() => ui.unmount());
});
