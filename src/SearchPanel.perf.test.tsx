import { Profiler } from 'react';
import { act, create, type ReactTestRenderer } from 'react-test-renderer';
import { beforeEach, expect, it, vi } from 'vitest';
import { pageText } from './bridge';
import SearchPanel from './SearchPanel';
import type { DocumentInfo } from './model';

vi.mock('./bridge', () => ({ pageText: vi.fn() }));

const PAGES = 400, ROUND_TRIP_MS = 2;
const document: DocumentInfo = { id: 1, name: 'long.pdf', path: 'long.pdf', pages: Array.from({ length: PAGES }, () => ({ width: 612, height: 792 })), revision: 1, dirty: false, can_undo: false, can_redo: false };

beforeEach(() => vi.clearAllMocks());

it('proxy: whole-document search over a simulated 2 ms IPC round trip, commits and wall time', async () => {
  let inFlight = 0, peak = 0;
  vi.mocked(pageText).mockImplementation((_id, page) => new Promise(resolve => {
    inFlight++; peak = Math.max(peak, inFlight);
    setTimeout(() => { inFlight--; resolve(page === PAGES - 1 ? 'the needle is here' : 'nothing on this page'); }, ROUND_TRIP_MS);
  }));
  let commits = 0; let ui!: ReactTestRenderer;
  await act(async () => { ui = create(<Profiler id="search" onRender={() => { commits++; }}><SearchPanel document={document} go={vi.fn()} close={vi.fn()} /></Profiler>); });
  act(() => ui.root.findByProps({ maxLength: 500 }).props.onChange({ target: { value: 'needle' } }));
  commits = 0;
  const start = performance.now();
  await act(async () => { ui.root.findByType('form').props.onSubmit({ preventDefault: vi.fn() }); await vi.waitFor(() => expect(JSON.stringify(ui.toJSON())).toContain('1 matching page.'), { timeout: 20_000, interval: 5 }); });
  const elapsed = performance.now() - start;
  process.stderr.write(`SEARCH-PROXY pages=${PAGES} roundTrip=${ROUND_TRIP_MS}ms calls=${vi.mocked(pageText).mock.calls.length} peakInFlight=${peak} commits=${commits} wall=${elapsed.toFixed(0)}ms\n`);
  expect(peak).toBeGreaterThan(1); expect(peak).toBeLessThanOrEqual(8);
  expect(ui.root.findAllByType('mark').map(mark => mark.children.join(''))).toEqual(['needle']);
  act(() => ui.unmount());
}, 60_000);
