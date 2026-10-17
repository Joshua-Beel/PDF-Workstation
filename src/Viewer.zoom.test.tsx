import { act, create, type ReactTestRenderer } from 'react-test-renderer';
import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import Viewer from './Viewer';
import { renderPage, retryPdfEngine } from './bridge';
import type { DocumentInfo } from './model';

vi.mock('./bridge', () => ({ renderPage: vi.fn(), retryPdfEngine: vi.fn() }));
vi.mock('./TextLayer', () => ({ default: () => null }));
vi.mock('./CommentLayer', () => ({ default: () => null }));

const info: DocumentInfo = { id: 3, revision: 1, name: 'z.pdf', path: 'C:/z.pdf', pages: [{ width: 612, height: 792 }], dirty: false, can_undo: false, can_redo: false };
const viewport = { scrollTop: 0, scrollLeft: 0, scrollTo: vi.fn(), setPointerCapture: vi.fn() };
const view = (zoom: number) => <Viewer document={info} zoom={zoom} fit="none" target={{ page: 0, token: 0 }} onPage={() => {}} hand={false} />;
const revoke = vi.fn();

beforeEach(() => {
  vi.useFakeTimers(); vi.clearAllMocks();
  vi.stubGlobal('window', { devicePixelRatio: 1, setTimeout, clearTimeout });
  vi.stubGlobal('URL', { revokeObjectURL: revoke });
  vi.stubGlobal('ResizeObserver', class { observe() {} disconnect() {} });
});
afterEach(() => { vi.useRealTimers(); vi.unstubAllGlobals(); });

it('keeps the old page image on screen across a zoom step and releases it once the new render arrives', async () => {
  let finish!: (url: string) => void;
  vi.mocked(renderPage).mockResolvedValueOnce('blob:100').mockImplementationOnce(() => new Promise<string>(resolve => { finish = resolve; }));
  let ui!: ReactTestRenderer;
  act(() => { ui = create(view(100), { createNodeMock: () => viewport }); });
  await act(async () => { vi.advanceTimersByTime(35); await Promise.resolve(); await Promise.resolve(); });
  const images = () => ui.root.findAllByType('img');
  expect(images().map(image => image.props.src)).toEqual(['blob:100']);
  act(() => images()[0].props.onLoad());

  act(() => ui.update(view(150)));
  expect(images().map(image => image.props.src)).toEqual(['blob:100']);
  expect(JSON.stringify(ui.toJSON())).not.toContain('Rendering page');
  await act(async () => { vi.advanceTimersByTime(35); await Promise.resolve(); });
  expect(vi.mocked(renderPage)).toHaveBeenCalledTimes(2);
  expect(images().map(image => image.props.src)).toEqual(['blob:100']);
  expect(revoke).not.toHaveBeenCalled();

  await act(async () => { finish('blob:150'); await Promise.resolve(); });
  expect(images().map(image => image.props.src)).toEqual(['blob:150']);
  expect(revoke).toHaveBeenCalledTimes(1);
  expect(revoke).toHaveBeenCalledWith('blob:100');
  act(() => ui.unmount());
  expect(revoke).toHaveBeenLastCalledWith('blob:150');
});

it('releases a superseded render that arrives after a newer zoom step and never shows it', async () => {
  let finishOld!: (url: string) => void;
  vi.mocked(renderPage).mockResolvedValueOnce('blob:100').mockImplementationOnce(() => new Promise<string>(resolve => { finishOld = resolve; })).mockResolvedValueOnce('blob:200');
  let ui!: ReactTestRenderer;
  act(() => { ui = create(view(100), { createNodeMock: () => viewport }); });
  await act(async () => { vi.advanceTimersByTime(35); await Promise.resolve(); await Promise.resolve(); });
  act(() => ui.update(view(150)));
  await act(async () => { vi.advanceTimersByTime(35); await Promise.resolve(); });
  act(() => ui.update(view(200)));
  await act(async () => { vi.advanceTimersByTime(35); await Promise.resolve(); await Promise.resolve(); });
  expect(ui.root.findAllByType('img').map(image => image.props.src)).toEqual(['blob:200']);
  await act(async () => { finishOld('blob:150'); await Promise.resolve(); });
  expect(ui.root.findAllByType('img').map(image => image.props.src)).toEqual(['blob:200']);
  expect(revoke).toHaveBeenCalledWith('blob:150');
  expect(revoke).toHaveBeenCalledWith('blob:100');
  act(() => ui.unmount());
});

it('drops the old image and shows the error with Try again when the new render fails', async () => {
  vi.mocked(renderPage).mockResolvedValueOnce('blob:100').mockRejectedValueOnce('render failed');
  vi.mocked(retryPdfEngine).mockResolvedValue(undefined);
  let ui!: ReactTestRenderer;
  act(() => { ui = create(view(100), { createNodeMock: () => viewport }); });
  await act(async () => { vi.advanceTimersByTime(35); await Promise.resolve(); await Promise.resolve(); });
  act(() => ui.update(view(150)));
  await act(async () => { vi.advanceTimersByTime(35); await Promise.resolve(); await Promise.resolve(); });
  expect(ui.root.findAllByType('img')).toHaveLength(0);
  expect(ui.root.find(node => node.props.role === 'alert').props.children).toBe('render failed');
  expect(ui.root.findAll(node => node.type === 'button' && node.props.children === 'Try again')).toHaveLength(1);
  expect(revoke).toHaveBeenCalledWith('blob:100');
  act(() => ui.unmount());
});
