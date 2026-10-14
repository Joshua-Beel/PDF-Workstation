import { act, create, type ReactTestRenderer } from 'react-test-renderer';
import { expect, it, vi } from 'vitest';
import { documentBookmarks } from './bridge';
import BookmarksPanel from './BookmarksPanel';
import type { DocumentInfo } from './model';
vi.mock('./bridge', () => ({ documentBookmarks: vi.fn() }));
const document: DocumentInfo = { id: 1, name: 'test.pdf', path: 'test.pdf', pages: [{ width: 612, height: 792 }], revision: 0, dirty: false, can_undo: false, can_redo: false };
it('navigates internal bookmarks and disables unsupported destinations', async () => {
  vi.mocked(documentBookmarks).mockResolvedValue({ items: [{ title: 'Start', page: 0, depth: 0 }, { title: 'External', page: null, depth: 1 }], truncated: true });
  const go = vi.fn(); let ui!: ReactTestRenderer;
  await act(async () => { ui = create(<BookmarksPanel document={document} go={go} close={vi.fn()} />); });
  act(() => ui.root.findAllByType('button').find(b => b.children.includes('Start'))!.props.onClick());
  expect(go).toHaveBeenCalledWith(0);
  const external = ui.root.findAllByType('button').find(b => b.children.includes('External'))!;
  expect(external.props.disabled).toBe(true); act(() => external.props.onClick()); expect(go).toHaveBeenCalledTimes(1);
  expect(JSON.stringify(ui.toJSON())).toContain('first 1,000 bookmarks');
  act(() => ui.unmount());
});
it('shows extraction errors without claiming the document has no bookmarks', async () => {
  vi.mocked(documentBookmarks).mockRejectedValue(new Error('Document is closed'));
  let ui!: ReactTestRenderer;
  await act(async () => { ui = create(<BookmarksPanel document={document} go={vi.fn()} close={vi.fn()} />); });
  expect(JSON.stringify(ui.toJSON())).toContain('Document is closed');
  expect(JSON.stringify(ui.toJSON())).not.toContain('has no bookmarks');
  act(() => ui.unmount());
});
it('passes a parsed destination to navigation and plain page jumps without one', async () => {
  const destination = { mode: 'xyz' as const, x: 72, y: 700, rect: null, zoom: 1.5, view: { left: 0, bottom: 0, width: 612, height: 792, rotation: 0 } };
  vi.mocked(documentBookmarks).mockResolvedValue({ items: [{ title: 'Placed', page: 0, depth: 0, destination }, { title: 'Plain', page: 0, depth: 0, destination: null }], truncated: false });
  const go = vi.fn(); let ui!: ReactTestRenderer;
  await act(async () => { ui = create(<BookmarksPanel document={document} go={go} close={vi.fn()} />); });
  const click = (title: string) => act(() => ui.root.findAllByType('button').find(b => b.children.includes(title))!.props.onClick());
  click('Placed'); expect(go).toHaveBeenLastCalledWith(0, destination);
  click('Plain'); expect(go).toHaveBeenLastCalledWith(0);
  act(() => ui.unmount());
});
