import { act, create, type ReactTestRenderer } from 'react-test-renderer';
import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import Viewer from './Viewer';
import { renderPage } from './bridge';
import type { DocumentInfo } from './model';

vi.mock('./bridge', () => ({ renderPage: vi.fn(), retryPdfEngine: vi.fn() }));
vi.mock('./TextLayer', () => ({ default: () => null }));
vi.mock('./CommentLayer', () => ({ default: () => null }));

const info: DocumentInfo = { id: 3, revision: 0, name: 'a.pdf', path: 'C:/a.pdf', pages: [{ width: 612, height: 792 }], dirty: false, can_undo: false, can_redo: false };
const viewport = { scrollTop: 0, scrollLeft: 0, scrollTo: vi.fn(), setPointerCapture: vi.fn() };

beforeEach(() => {
  vi.useFakeTimers(); vi.clearAllMocks();
  vi.mocked(renderPage).mockResolvedValue('blob:test');
  vi.stubGlobal('window', { devicePixelRatio: 1, setTimeout, clearTimeout });
  vi.stubGlobal('URL', { revokeObjectURL: vi.fn() });
  vi.stubGlobal('ResizeObserver', class { observe() {} disconnect() {} });
});
afterEach(() => { vi.useRealTimers(); vi.unstubAllGlobals(); });

it('makes the scrolling page area a keyboard-focusable named region and each page a named group', async () => {
  let ui!: ReactTestRenderer;
  act(() => { ui = create(<Viewer document={info} zoom={100} fit="none" target={{ page: 0, token: 0 }} onPage={vi.fn()} hand={false} />, { createNodeMock: () => viewport }); });
  await act(async () => { vi.advanceTimersByTime(35); await Promise.resolve(); });
  const region = ui.root.findAll(node => typeof node.props.onScroll === 'function')[0];
  expect(region.props.tabIndex).toBe(0);
  expect(region.props.role).toBe('region');
  expect(region.props['aria-label']).toBe('Document pages');
  const page = ui.root.findAll(node => node.props['aria-label'] === 'Page 1' && node.type === 'div')[0];
  expect(page.props.role).toBe('group');
});
