import { act, create, type ReactTestRenderer } from 'react-test-renderer';
import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import App from './App';
import Viewer from './Viewer';
import { documentBookmarks, editBookmarks, openDocument } from './bridge';
import type { DocumentInfo } from './model';

vi.mock('./bridge', () => ({ native: false, openDocument: vi.fn(), reopenDocument: vi.fn(), closeDocument: vi.fn(), editPages: vi.fn(), saveCopy: vi.fn(), documentAnnotations: vi.fn(), createPdfFromImage: vi.fn(), documentPageLabels: vi.fn(), documentBookmarks: vi.fn(), editBookmarks: vi.fn() }));
vi.mock('./Viewer', () => ({ default: () => <div /> }));

const document = (dirty = false): DocumentInfo => ({ id: 1, name: 'notes.pdf', path: 'C:/notes.pdf', pages: [{ width: 612, height: 792 }, { width: 612, height: 792 }], revision: 0, dirty, can_undo: false, can_redo: false });
beforeEach(() => {
  vi.clearAllMocks(); const storage = new Map();
  vi.stubGlobal('window', { localStorage: { getItem: (key: string) => storage.get(key) ?? null, setItem: (key: string, value: string) => storage.set(key, value) }, addEventListener: vi.fn(), removeEventListener: vi.fn() });
});
afterEach(() => vi.unstubAllGlobals());

it('marks the open document as having unsaved changes after a bookmark edit, without a new revision', async () => {
  vi.mocked(openDocument).mockResolvedValue({ status: 'opened', document: document() });
  vi.mocked(documentBookmarks).mockResolvedValue({ items: [], truncated: false });
  vi.mocked(editBookmarks).mockResolvedValueOnce({ view: { editable: true, reason: null, version: 0, entries: [], can_undo: false, can_redo: false, changed: false }, document: document() });
  let ui!: ReactTestRenderer; act(() => { ui = create(<App />); });
  await act(async () => ui.root.findAllByType('button').find(item => item.children.includes('Open a file'))!.props.onClick());
  await act(async () => ui.root.findByProps({ 'aria-label': 'Bookmarks' }).props.onClick());
  await act(async () => ui.root.findAllByType('button').find(item => item.children.includes('Edit bookmarks'))!.props.onClick());
  act(() => ui.root.findByProps({ 'aria-label': 'Add bookmark at current page' }).props.onClick());
  expect(ui.root.findByType('form').findByType('input').props.value).toBe('Page 1');
  vi.mocked(editBookmarks).mockResolvedValueOnce({ view: { editable: true, reason: null, version: 1, entries: [{ uid: 0, title: 'Page 1', depth: 0, page: 0, open: true, has_children: false, added: true }], can_undo: true, can_redo: false, changed: true }, document: document(true) });
  await act(async () => ui.root.findByType('form').props.onSubmit({ preventDefault() {} }));
  expect(editBookmarks).toHaveBeenLastCalledWith(1, 0, { version: 0, op: { kind: 'add', page: 0, title: 'Page 1', after: null } });
  const rendered = JSON.stringify(ui.toJSON());
  expect(rendered).toContain('"notes.pdf"," *"');
  expect(rendered).toContain('Unsaved changes');
  expect(ui.root.findByType(Viewer).props.document).toMatchObject({ id: 1, revision: 0, dirty: true });
  act(() => ui.unmount());
});
