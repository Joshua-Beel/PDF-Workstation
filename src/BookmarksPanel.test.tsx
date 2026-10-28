import { act, create, type ReactTestRenderer } from 'react-test-renderer';
import { beforeEach, expect, it, vi } from 'vitest';
import { documentBookmarks, editBookmarks, type BookmarkEditEntry, type BookmarkEditResult } from './bridge';
import BookmarksPanel, { previousSibling, subtreeEnd } from './BookmarksPanel';
import type { DocumentInfo } from './model';
vi.mock('./bridge', () => ({ documentBookmarks: vi.fn(), editBookmarks: vi.fn() }));
const document: DocumentInfo = { id: 1, name: 'test.pdf', path: 'test.pdf', pages: [{ width: 612, height: 792 }, { width: 612, height: 792 }, { width: 612, height: 792 }], revision: 0, dirty: false, can_undo: false, can_redo: false };
const entry = (uid: number, title: string, depth: number, page: number | null, extra: Partial<BookmarkEditEntry> = {}): BookmarkEditEntry => ({ uid, title, depth, page, open: true, has_children: false, added: false, ...extra });
const tree = [entry(0, 'Intro', 0, 0, { has_children: true }), entry(1, 'Scope', 1, 1), entry(2, 'Terms', 1, 2), entry(3, 'Appendix', 0, 2)];
const result = (entries: BookmarkEditEntry[], extra: Partial<BookmarkEditResult['view']> = {}, dirty = false): BookmarkEditResult => ({ view: { editable: true, reason: null, version: 0, entries, can_undo: false, can_redo: false, changed: dirty, ...extra }, document: { ...document, dirty } });
beforeEach(() => { vi.mocked(documentBookmarks).mockReset(); vi.mocked(editBookmarks).mockReset(); });

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

const button = (ui: ReactTestRenderer, name: string) => ui.root.findAll(node => node.type === 'button' && (node.props['aria-label'] === name || node.children.includes(name)))[0];
const row = (ui: ReactTestRenderer, title: string) => ui.root.findAll(node => node.type === 'button' && node.children.includes(title) && node.props['aria-pressed'] !== undefined)[0];
const text = (ui: ReactTestRenderer) => JSON.stringify(ui.toJSON());
async function editing(entries = tree, page = 1, onDocument = vi.fn(), go = vi.fn()) {
  vi.mocked(documentBookmarks).mockResolvedValue({ items: [{ title: 'Intro', page: 0, depth: 0 }], truncated: false });
  vi.mocked(editBookmarks).mockResolvedValueOnce(result(entries));
  let ui!: ReactTestRenderer;
  await act(async () => { ui = create(<BookmarksPanel document={document} page={page} go={go} close={vi.fn()} onDocument={onDocument} />); });
  await act(async () => { button(ui, 'Edit bookmarks').props.onClick(); });
  return { ui, onDocument, go };
}

it('opens the editor from the read-only list and offers every editing command as a labelled button', async () => {
  const { ui } = await editing();
  expect(editBookmarks).toHaveBeenCalledWith(1, 0, null);
  const toolbar = ui.root.findByProps({ role: 'toolbar' });
  expect(toolbar.props['aria-label']).toBe('Bookmark editing');
  const labels = toolbar.findAllByType('button').map(b => b.props['aria-label']);
  expect(labels).toEqual(['Add bookmark at current page', 'Rename bookmark', 'Delete bookmark and its nested bookmarks', 'Move bookmark up', 'Move bookmark down', 'Indent bookmark under the one above', 'Outdent bookmark one level', 'Show nested bookmarks when the document opens', 'Undo bookmark change', 'Redo bookmark change']);
  for (const b of toolbar.findAllByType('button')) expect(b.props.type).toBe('button');
  expect(text(ui)).toContain('original file is never changed');
  expect(row(ui, 'Terms').props['aria-label']).toBe('Terms, level 2, Page 3');
  expect(row(ui, 'Intro').props['aria-label']).toBe('Intro, level 1, Page 1, open');
  act(() => ui.unmount());
});

it('enables selection commands only where they can apply', async () => {
  const { ui } = await editing();
  const enabled = () => ['Rename bookmark', 'Delete bookmark and its nested bookmarks', 'Move bookmark up', 'Move bookmark down', 'Indent bookmark under the one above', 'Outdent bookmark one level', 'Show nested bookmarks when the document opens'].filter(name => !button(ui, name).props.disabled);
  expect(enabled()).toEqual([]); expect(button(ui, 'Add bookmark at current page').props.disabled).toBe(false);
  act(() => row(ui, 'Intro').props.onClick()); expect(enabled()).toEqual(['Rename bookmark', 'Delete bookmark and its nested bookmarks', 'Move bookmark down', 'Show nested bookmarks when the document opens']);
  act(() => row(ui, 'Scope').props.onClick()); expect(enabled()).toEqual(['Rename bookmark', 'Delete bookmark and its nested bookmarks', 'Move bookmark down', 'Outdent bookmark one level']);
  act(() => row(ui, 'Terms').props.onClick()); expect(enabled()).toEqual(['Rename bookmark', 'Delete bookmark and its nested bookmarks', 'Move bookmark up', 'Indent bookmark under the one above', 'Outdent bookmark one level']);
  act(() => row(ui, 'Appendix').props.onClick()); expect(enabled()).toEqual(['Rename bookmark', 'Delete bookmark and its nested bookmarks', 'Move bookmark up', 'Indent bookmark under the one above']);
  expect(row(ui, 'Appendix').props['aria-pressed']).toBe(true); expect(row(ui, 'Intro').props['aria-pressed']).toBe(false);
  act(() => ui.unmount());
});

it('adds a bookmark at the current page after the selection and selects it', async () => {
  const { ui, onDocument, go } = await editing(tree, 1);
  act(() => row(ui, 'Intro').props.onClick()); expect(go).toHaveBeenLastCalledWith(0);
  act(() => button(ui, 'Add bookmark at current page').props.onClick());
  const form = ui.root.findByType('form');
  expect(form.props['aria-label']).toBe('New bookmark');
  const input = ui.root.findByType('input'); expect(input.props.value).toBe('Page 2'); expect(input.props.autoFocus).toBe(true);
  act(() => input.props.onChange({ target: { value: '  Chapter two ' } }));
  const next = result([...tree, entry(9, 'Chapter two', 0, 1, { added: true })], { version: 1, can_undo: true, changed: true }, true);
  vi.mocked(editBookmarks).mockResolvedValueOnce(next);
  await act(async () => { form.props.onSubmit({ preventDefault() {} }); });
  expect(editBookmarks).toHaveBeenLastCalledWith(1, 0, { version: 0, op: { kind: 'add', page: 1, title: 'Chapter two', after: 0 } });
  expect(onDocument).toHaveBeenCalledWith(next.document);
  expect(ui.root.findAllByType('form')).toHaveLength(0);
  expect(row(ui, 'Chapter two').props['aria-pressed']).toBe(true);
  expect(row(ui, 'Chapter two').props['aria-label']).toContain('new');
  expect(text(ui)).toContain('Added bookmark');
  act(() => ui.unmount());
});

it('refuses an empty title without calling the backend', async () => {
  const { ui } = await editing(tree, 0);
  act(() => button(ui, 'Add bookmark at current page').props.onClick());
  act(() => ui.root.findByType('input').props.onChange({ target: { value: '   ' } }));
  await act(async () => { ui.root.findByType('form').props.onSubmit({ preventDefault() {} }); });
  expect(editBookmarks).toHaveBeenCalledTimes(1);
  expect(ui.root.findByProps({ role: 'alert' }).children.join('')).toContain('Enter a bookmark title');
  act(() => ui.unmount());
});

it('adds to the end when nothing is selected', async () => {
  const { ui } = await editing(tree, 2);
  act(() => button(ui, 'Add bookmark at current page').props.onClick());
  vi.mocked(editBookmarks).mockResolvedValueOnce(result([...tree, entry(9, 'Page 3', 0, 2, { added: true })], { version: 1, changed: true }, true));
  await act(async () => { ui.root.findByType('form').props.onSubmit({ preventDefault() {} }); });
  expect(editBookmarks).toHaveBeenLastCalledWith(1, 0, { version: 0, op: { kind: 'add', page: 2, title: 'Page 3', after: null } });
  act(() => ui.unmount());
});

it('renames the selected bookmark through a form that Escape closes', async () => {
  const { ui } = await editing();
  act(() => row(ui, 'Terms').props.onClick());
  act(() => button(ui, 'Rename bookmark').props.onClick());
  expect(ui.root.findByType('input').props.value).toBe('Terms');
  const stop = vi.fn(); act(() => ui.root.findByType('form').props.onKeyDown({ key: 'Escape', stopPropagation: stop }));
  expect(ui.root.findAllByType('form')).toHaveLength(0); expect(stop).toHaveBeenCalled();
  act(() => button(ui, 'Rename bookmark').props.onClick());
  act(() => ui.root.findByType('input').props.onChange({ target: { value: 'Definitions' } }));
  vi.mocked(editBookmarks).mockResolvedValueOnce(result(tree.map(e => e.uid === 2 ? { ...e, title: 'Definitions' } : e), { version: 1, changed: true, can_undo: true }, true));
  await act(async () => { ui.root.findByType('form').props.onSubmit({ preventDefault() {} }); });
  expect(editBookmarks).toHaveBeenLastCalledWith(1, 0, { version: 0, op: { kind: 'rename', uid: 2, title: 'Definitions' } });
  expect(row(ui, 'Definitions')).toBeTruthy();
  act(() => ui.unmount());
});

it('deletes a bookmark with its children, says how many went, and clears the selection', async () => {
  const { ui } = await editing();
  act(() => row(ui, 'Intro').props.onClick());
  vi.mocked(editBookmarks).mockResolvedValueOnce(result([tree[3]], { version: 1, changed: true, can_undo: true }, true));
  await act(async () => { button(ui, 'Delete bookmark and its nested bookmarks').props.onClick(); });
  expect(editBookmarks).toHaveBeenLastCalledWith(1, 0, { version: 0, op: { kind: 'delete', uid: 0 } });
  expect(text(ui)).toContain('Deleted');
  expect(text(ui)).toContain('2 nested bookmarks');
  expect(button(ui, 'Delete bookmark and its nested bookmarks').props.disabled).toBe(true);
  act(() => ui.unmount());
});

it('sends move, indent, outdent, open, undo and redo operations with the current version', async () => {
  const { ui, onDocument } = await editing();
  const cases: [string, number, object][] = [
    ['Move bookmark down', 1, { kind: 'move_down', uid: 1 }], ['Outdent bookmark one level', 1, { kind: 'outdent', uid: 1 }],
    ['Move bookmark up', 2, { kind: 'move_up', uid: 2 }], ['Indent bookmark under the one above', 3, { kind: 'indent', uid: 3 }],
  ];
  let version = 0;
  for (const [label, uid, op] of cases) {
    act(() => row(ui, tree[uid].title).props.onClick());
    vi.mocked(editBookmarks).mockResolvedValueOnce(result(tree, { version: version + 1, changed: true, can_undo: true }, true));
    await act(async () => { button(ui, label).props.onClick(); });
    expect(editBookmarks).toHaveBeenLastCalledWith(1, 0, { version, op }); version += 1;
  }
  act(() => row(ui, 'Intro').props.onClick());
  vi.mocked(editBookmarks).mockResolvedValueOnce(result([{ ...tree[0], open: false }, ...tree.slice(1)], { version: 5, changed: true, can_undo: true }, true));
  await act(async () => { button(ui, 'Show nested bookmarks when the document opens').props.onClick(); });
  expect(editBookmarks).toHaveBeenLastCalledWith(1, 0, { version: 4, op: { kind: 'set_open', uid: 0, open: false } });
  expect(button(ui, 'Show nested bookmarks when the document opens').props['aria-pressed']).toBe(false);
  vi.mocked(editBookmarks).mockResolvedValueOnce(result(tree, { version: 6, changed: true, can_undo: true, can_redo: true }, true));
  await act(async () => { button(ui, 'Undo bookmark change').props.onClick(); });
  expect(editBookmarks).toHaveBeenLastCalledWith(1, 0, { version: 5, op: { kind: 'undo' } });
  vi.mocked(editBookmarks).mockResolvedValueOnce(result(tree, { version: 7, changed: true, can_undo: true }, true));
  await act(async () => { button(ui, 'Redo bookmark change').props.onClick(); });
  expect(editBookmarks).toHaveBeenLastCalledWith(1, 0, { version: 6, op: { kind: 'redo' } });
  expect(onDocument).toHaveBeenCalledTimes(7);
  act(() => ui.unmount());
});

it('reports a refused edit and keeps the tree on screen', async () => {
  const { ui, onDocument } = await editing();
  act(() => row(ui, 'Terms').props.onClick());
  vi.mocked(editBookmarks).mockRejectedValueOnce('The bookmarks changed. Reload them and try again.');
  await act(async () => { button(ui, 'Move bookmark up').props.onClick(); });
  expect(ui.root.findByProps({ role: 'alert' }).children.join('')).toContain('bookmarks changed');
  expect(row(ui, 'Terms')).toBeTruthy(); expect(onDocument).not.toHaveBeenCalled();
  act(() => ui.unmount());
});

it('ignores a second command while one is in flight', async () => {
  const { ui } = await editing();
  act(() => row(ui, 'Terms').props.onClick());
  let release!: (value: BookmarkEditResult) => void;
  vi.mocked(editBookmarks).mockReturnValueOnce(new Promise(resolve => { release = resolve; }));
  await act(async () => { button(ui, 'Move bookmark up').props.onClick(); });
  expect(button(ui, 'Move bookmark up').props.disabled).toBe(true);
  await act(async () => { button(ui, 'Move bookmark up').props.onClick(); });
  expect(editBookmarks).toHaveBeenCalledTimes(2);
  await act(async () => { release(result(tree, { version: 1 }, true)); });
  act(() => ui.unmount());
});

it('explains why bookmarks cannot be edited and stays on the read-only list', async () => {
  vi.mocked(documentBookmarks).mockResolvedValue({ items: [{ title: 'Intro', page: 0, depth: 0 }], truncated: false });
  vi.mocked(editBookmarks).mockResolvedValueOnce(result([], { editable: false, reason: 'Signed or certified PDFs cannot be edited in this build; signatures must remain intact.' }));
  let ui!: ReactTestRenderer;
  await act(async () => { ui = create(<BookmarksPanel document={document} go={vi.fn()} close={vi.fn()} />); });
  await act(async () => { button(ui, 'Edit bookmarks').props.onClick(); });
  expect(ui.root.findByProps({ role: 'alert' }).children.join('')).toContain('Signed or certified');
  expect(ui.root.findAllByProps({ role: 'toolbar' })).toHaveLength(0); expect(button(ui, 'Intro')).toBeTruthy();
  act(() => ui.unmount());
});

it('reopens in the editor when unsaved edits exist and returns to a fresh read-only list on Done', async () => {
  vi.mocked(documentBookmarks).mockResolvedValue({ items: [{ title: 'Intro', page: 0, depth: 0 }], truncated: false, edited: true });
  vi.mocked(editBookmarks).mockResolvedValueOnce(result(tree, { changed: true, can_undo: true }, true));
  let ui!: ReactTestRenderer;
  await act(async () => { ui = create(<BookmarksPanel document={document} go={vi.fn()} close={vi.fn()} />); });
  expect(ui.root.findAllByProps({ role: 'toolbar' })).toHaveLength(1);
  expect(documentBookmarks).toHaveBeenCalledTimes(1);
  await act(async () => { button(ui, 'Done editing').props.onClick(); });
  expect(documentBookmarks).toHaveBeenCalledTimes(2);
  expect(ui.root.findAllByProps({ role: 'toolbar' })).toHaveLength(0);
  expect(text(ui)).toContain('shows the original bookmarks');
  expect(editBookmarks).toHaveBeenCalledTimes(1);
  act(() => ui.unmount());
});

it('finds previous siblings and subtree ends in the flat tree', () => {
  expect([0, 1, 2, 3].map(index => previousSibling(tree, index))).toEqual([null, null, 1, 0]);
  expect([0, 1, 2, 3].map(index => subtreeEnd(tree, index))).toEqual([3, 2, 3, 4]);
});
