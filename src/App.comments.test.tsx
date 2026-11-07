import { act, create, type ReactTestRenderer } from 'react-test-renderer';
import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import App from './App';
import Viewer from './Viewer';
import { closeDocument, createComment, createHighlight, createTextHighlight, documentAnnotations, openDocument, saveCopy } from './bridge';
import type { DocumentInfo } from './model';

vi.mock('./bridge', () => ({ native: false, openDocument: vi.fn(), reopenDocument: vi.fn(), closeDocument: vi.fn(), editPages: vi.fn(), saveCopy: vi.fn(), documentAnnotations: vi.fn(), createPdfFromImage: vi.fn(), documentPageLabels: vi.fn(), createComment: vi.fn(), updateComment: vi.fn(), deleteComment: vi.fn(), createHighlight: vi.fn(), createTextHighlight: vi.fn(), updateHighlight: vi.fn(), deleteHighlight: vi.fn() }));
vi.mock('./Viewer', () => ({ default: (props: { commentMode: boolean; highlightMode: boolean; annotationInteractive: boolean; hand: boolean; onCommentCreate: (page: number, rect: { x: number; y: number; width: number; height: number }) => void; onHighlightCreate: (page: number, rect: { x: number; y: number; width: number; height: number }) => void; onTextSelection: (selection: { id: number; page: number; revision: number; start: number; end: number } | null, source: { id: number; page: number; revision: number }) => void }) => <div data-comment-mode={String(props.commentMode)} data-highlight-mode={String(props.highlightMode)} data-annotation-interactive={String(props.annotationInteractive)} data-hand={String(props.hand)}><button onClick={() => props.onCommentCreate(0, { x: .1, y: .2, width: .03, height: .03 })}>Place comment</button><button onClick={() => props.onHighlightCreate(0, { x: .1, y: .2, width: .03, height: .03 })}>Place highlight</button><button onClick={() => props.onTextSelection({ id: 1, page: 0, revision: 0, start: 1, end: 4 }, { id: 1, page: 0, revision: 0 })}>Select text range</button><button onClick={() => props.onTextSelection({ id: 1, page: 0, revision: 0, start: 4, end: 6 }, { id: 1, page: 0, revision: 0 })}>Select next text range</button><button onClick={() => props.onTextSelection(null, { id: 1, page: 1, revision: 0 })}>Clear other layer range</button><button onClick={() => props.onTextSelection({ id: 2, page: 0, revision: 0, start: 1, end: 4 }, { id: 2, page: 0, revision: 0 })}>Select stale text range</button></div> }));

const document = (revision = 0): DocumentInfo => ({ id: 1, name: 'notes.pdf', path: 'C:/notes.pdf', pages: [{ width: 612, height: 792 }], revision, dirty: revision > 0, can_undo: revision > 0, can_redo: false });
const annotations = (documentId = 1, revision = 0, items: { id: string; kind: 'note' | 'highlight'; page: number; rect: { x: number; y: number; width: number; height: number } | null; contents: string | null }[] = []) => ({ documentId, revision, status: 'supported' as const, reason: null, annotations: items, viewOnly: [], viewOnlyTruncated: 0 });
let keydown: ((event: KeyboardEvent) => void) | undefined;
beforeEach(() => {
  vi.clearAllMocks(); const storage = new Map(); keydown = undefined;
  vi.stubGlobal('window', { localStorage: { getItem: (key: string) => storage.get(key) ?? null, setItem: (key: string, value: string) => storage.set(key, value) }, addEventListener: (type: string, listener: (event: KeyboardEvent) => void) => { if (type === 'keydown') keydown = listener; }, removeEventListener: vi.fn() });
});
afterEach(() => vi.unstubAllGlobals());
const button = (ui: ReactTestRenderer, label: string) => ui.root.findByProps({ 'aria-label': label });
async function open(ui: ReactTestRenderer) {
  vi.mocked(openDocument).mockResolvedValue({ status: 'opened', document: document() });
  await act(async () => ui.root.findAllByType('button').find(item => item.children.includes('Open a file'))!.props.onClick());
}

it('creates a note against the current revision and refreshes the active document', async () => {
  vi.mocked(documentAnnotations).mockResolvedValue(annotations());
  vi.mocked(createComment).mockResolvedValue(document(1));
  let ui!: ReactTestRenderer; act(() => { ui = create(<App />); });
  await open(ui);
  await act(async () => button(ui, 'Add comment').props.onClick());
  expect(ui.root.findByType(Viewer).props.commentMode).toBe(true);
  expect(ui.root.findByType(Viewer).props.hand).toBe(false);
  await act(async () => ui.root.findAllByType('button').find(item => item.children.includes('Place comment'))!.props.onClick());
  act(() => ui.root.findByProps({ 'aria-label': 'Comment text' }).props.onChange({ target: { value: 'Keep this' } }));
  await act(async () => ui.root.findAllByType('button').find(item => item.children.join('') === 'Save comment')!.props.onClick());
  expect(createComment).toHaveBeenCalledWith(1, 0, 0, { x: .1, y: .2, width: .03, height: .03 }, 'Keep this', 0);
  expect(ui.root.findByType(Viewer).props.document).toMatchObject({ id: 1, revision: 1, dirty: true });
  act(() => ui.unmount());
});

it('keeps placement modes exclusive through Pan, Select, and the H shortcut without mutating annotations', async () => {
  vi.mocked(documentAnnotations).mockResolvedValue(annotations());
  let ui!: ReactTestRenderer; act(() => { ui = create(<App />); });
  await open(ui);
  await act(async () => button(ui, 'Add comment').props.onClick());
  expect(ui.root.findByType(Viewer).props.commentMode).toBe(true);
  act(() => keydown!({ key: 'h', ctrlKey: false, target: { matches: () => false } } as unknown as KeyboardEvent));
  expect(ui.root.findByType(Viewer).props.commentMode).toBe(false);
  expect(ui.root.findByType(Viewer).props.hand).toBe(true);
  act(() => button(ui, 'Select text on page').props.onClick());
  expect(ui.root.findByType(Viewer).props.commentMode).toBe(false);
  expect(ui.root.findByType(Viewer).props.hand).toBe(false);
  await act(async () => button(ui, 'Add area highlight').props.onClick());
  expect(ui.root.findByType(Viewer).props.highlightMode).toBe(true);
  expect(ui.root.findByType(Viewer).props.commentMode).toBe(false);
  act(() => button(ui, 'Pan document').props.onClick());
  expect(ui.root.findByType(Viewer).props.highlightMode).toBe(false);
  expect(createComment).not.toHaveBeenCalled();
  act(() => ui.unmount());
});

it('leaves panel-launched placement mode with Escape', async () => {
  vi.mocked(documentAnnotations).mockResolvedValue(annotations());
  let ui!: ReactTestRenderer; act(() => { ui = create(<App />); });
  await open(ui);
  await act(async () => button(ui, 'Comments').props.onClick());
  await act(async () => ui.root.findAllByType('button').find(item => item.children.join('') === 'Add comment on page 1')!.props.onClick());
  expect(ui.root.findByType(Viewer).props.commentMode).toBe(true);
  act(() => keydown!({ key: 'Escape', ctrlKey: false, target: { matches: () => false } } as unknown as KeyboardEvent));
  expect(ui.root.findByType(Viewer).props.commentMode).toBe(false);
  act(() => ui.unmount());
});

it('keeps an open comments list from intercepting Pan or Select through area highlights', async () => {
  vi.mocked(documentAnnotations).mockResolvedValue(annotations(1, 0, [{ id: 'h1', kind: 'highlight', page: 0, rect: { x: .05, y: .05, width: .9, height: .9 }, contents: null }]));
  let ui!: ReactTestRenderer; act(() => { ui = create(<App />); });
  await open(ui);
  await act(async () => button(ui, 'Comments').props.onClick());
  expect(ui.root.findByType(Viewer).props.annotationInteractive).toBe(false);
  act(() => button(ui, 'Pan document').props.onClick());
  expect(ui.root.findByType(Viewer).props.annotationInteractive).toBe(false);
  act(() => button(ui, 'Select text on page').props.onClick());
  expect(ui.root.findByType(Viewer).props.annotationInteractive).toBe(false);
  act(() => ui.unmount());
});

it('drops a stale annotation query when the active tab changes', async () => {
  let resolveFirst!: (value: ReturnType<typeof annotations>) => void;
  const first = new Promise<ReturnType<typeof annotations>>(resolve => { resolveFirst = resolve; });
  vi.mocked(documentAnnotations).mockImplementation((id: number) => id === 1 ? first : Promise.resolve(annotations(2, 0, [{ id: 'fresh', kind: 'note', page: 0, rect: null, contents: 'Fresh note' }])));
  vi.mocked(openDocument).mockResolvedValueOnce({ status: 'opened', document: document() }).mockResolvedValueOnce({ status: 'opened', document: { ...document(), id: 2, name: 'other.pdf', path: 'C:/other.pdf' } });
  let ui!: ReactTestRenderer; act(() => { ui = create(<App />); });
  await act(async () => ui.root.findAllByType('button').find(item => item.children.includes('Open a file'))!.props.onClick());
  await act(async () => button(ui, 'Add comment').props.onClick());
  await act(async () => ui.root.findAllByType('button').find(item => item.children.includes('Open a file'))!.props.onClick());
  await act(async () => { resolveFirst(annotations(1, 0, [{ id: 'stale', kind: 'note', page: 0, rect: null, contents: 'Stale note' }])); });
  expect(JSON.stringify(ui.toJSON())).not.toContain('Stale note');
  expect(ui.root.findByType(Viewer).props.commentMode).toBe(false);
  act(() => ui.unmount());
});

it('closes and guides reopening when annotation recovery was committed but not confirmed', async () => {
  vi.mocked(documentAnnotations).mockResolvedValue(annotations());
  vi.mocked(createComment).mockRejectedValue({ code: 'recoveryCommittedReopenRequired', documentId: 1, requestedRevision: 0, committedRevision: 1 });
  vi.mocked(closeDocument).mockResolvedValue();
  let ui!: ReactTestRenderer; act(() => { ui = create(<App />); });
  await open(ui);
  await act(async () => button(ui, 'Add comment').props.onClick());
  await act(async () => ui.root.findAllByType('button').find(item => item.children.includes('Place comment'))!.props.onClick());
  act(() => ui.root.findByProps({ 'aria-label': 'Comment text' }).props.onChange({ target: { value: 'May recover' } }));
  await act(async () => ui.root.findAllByType('button').find(item => item.children.join('') === 'Save comment')!.props.onClick());
  expect(closeDocument).toHaveBeenCalledExactlyOnceWith(1);
  expect(ui.root.findAllByType(Viewer)).toHaveLength(0);
  expect(JSON.stringify(ui.toJSON())).toContain('edit may be saved in recovery');
  act(() => ui.unmount());
});

it('shares an in-flight annotation query when comments are closed and reopened', async () => {
  let resolve!: (value: ReturnType<typeof annotations>) => void;
  vi.mocked(documentAnnotations).mockImplementation(() => new Promise(done => { resolve = done; }));
  let ui!: ReactTestRenderer; act(() => { ui = create(<App />); });
  await open(ui);
  act(() => button(ui, 'Comments').props.onClick());
  act(() => button(ui, 'Close comments').props.onClick());
  act(() => button(ui, 'Comments').props.onClick());
  expect(documentAnnotations).toHaveBeenCalledTimes(1);
  await act(async () => resolve(annotations(1, 0, [{ id: 'shared', kind: 'note', page: 0, rect: null, contents: 'Shared note' }])));
  expect(JSON.stringify(ui.toJSON())).toContain('Shared note');
  act(() => ui.unmount());
});

it('creates an area highlight using the current revision and an optional body', async () => {
  vi.mocked(documentAnnotations).mockResolvedValue(annotations());
  vi.mocked(createHighlight).mockResolvedValue(document(1));
  let ui!: ReactTestRenderer; act(() => { ui = create(<App />); });
  await open(ui);
  await act(async () => button(ui, 'Add area highlight').props.onClick());
  expect(ui.root.findByType(Viewer).props.highlightMode).toBe(true);
  expect(ui.root.findByType(Viewer).props.commentMode).toBe(false);
  await act(async () => ui.root.findAllByType('button').find(item => item.children.includes('Place highlight'))!.props.onClick());
  expect(ui.root.findByProps({ 'aria-label': 'Highlight description' }).props.value).toBe('');
  await act(async () => ui.root.findAllByType('button').find(item => item.children.join('') === 'Save area highlight')!.props.onClick());
  expect(createHighlight).toHaveBeenCalledWith(1, 0, 0, { x: .1, y: .2, width: .03, height: .03 }, '', 0);
  expect(ui.root.findByType(Viewer).props.document).toMatchObject({ id: 1, revision: 1, dirty: true });
  act(() => ui.unmount());
});

it('creates a text highlight using the exact selected geometry range, without inferred contents', async () => {
  vi.mocked(documentAnnotations).mockResolvedValue(annotations());
  vi.mocked(createTextHighlight).mockResolvedValue(document(1));
  let ui!: ReactTestRenderer; act(() => { ui = create(<App />); });
  await open(ui);
  act(() => button(ui, 'Select text on page').props.onClick());
  await act(async () => ui.root.findAllByType('button').find(item => item.children.includes('Select text range'))!.props.onClick());
  await act(async () => { await Promise.resolve(); await Promise.resolve(); });
  const action = button(ui, 'Highlight selected text');
  expect(action.props.disabled).toBe(false);
  act(() => action.props.onPointerDown());
  act(() => action.props.onClick());
  expect(ui.root.findByType(Viewer).props.commentMode).toBe(false);
  expect(ui.root.findByType(Viewer).props.highlightMode).toBe(false);
  expect(ui.root.findAllByType('h2').some(item => item.children.join('') === 'New text highlight on page 1')).toBe(true);
  await act(async () => ui.root.findAllByType('button').find(item => item.children.join('') === 'Save text highlight')!.props.onClick());
  expect(createTextHighlight).toHaveBeenCalledWith(1, 0, 0, 1, 4, '', 0);
  expect(ui.root.findByType(Viewer).props.document).toMatchObject({ id: 1, revision: 1, dirty: true });
  act(() => ui.unmount());
});

it('rejects stale text selection callbacks and clears the action outside Select mode', async () => {
  vi.mocked(documentAnnotations).mockResolvedValue(annotations());
  let ui!: ReactTestRenderer; act(() => { ui = create(<App />); });
  await open(ui);
  act(() => button(ui, 'Select text on page').props.onClick());
  await act(async () => ui.root.findAllByType('button').find(item => item.children.includes('Select stale text range'))!.props.onClick());
  expect(button(ui, 'Highlight selected text').props.disabled).toBe(true);
  await act(async () => ui.root.findAllByType('button').find(item => item.children.includes('Select text range'))!.props.onClick());
  await act(async () => { await Promise.resolve(); await Promise.resolve(); });
  expect(button(ui, 'Highlight selected text').props.disabled).toBe(false);
  act(() => button(ui, 'Pan document').props.onClick());
  expect(button(ui, 'Highlight selected text').props.disabled).toBe(true);
  act(() => ui.unmount());
});

it('keeps a new page selection when another text layer clears in the same selection update', async () => {
  vi.mocked(documentAnnotations).mockResolvedValue(annotations());
  let ui!: ReactTestRenderer; act(() => { ui = create(<App />); });
  await open(ui);
  act(() => button(ui, 'Select text on page').props.onClick());
  await act(async () => ui.root.findAllByType('button').find(item => item.children.includes('Select text range'))!.props.onClick());
  await act(async () => ui.root.findAllByType('button').find(item => item.children.includes('Clear other layer range'))!.props.onClick());
  expect(button(ui, 'Highlight selected text').props.disabled).toBe(false);
  act(() => ui.unmount());
});

it('does not reuse a canceled pointer capture when keyboard activation follows a newer selection', async () => {
  vi.mocked(documentAnnotations).mockResolvedValue(annotations());
  vi.mocked(createTextHighlight).mockResolvedValue(document(1));
  let ui!: ReactTestRenderer; act(() => { ui = create(<App />); });
  await open(ui);
  act(() => button(ui, 'Select text on page').props.onClick());
  await act(async () => ui.root.findAllByType('button').find(item => item.children.includes('Select text range'))!.props.onClick());
  act(() => button(ui, 'Highlight selected text').props.onPointerDown());
  await act(async () => ui.root.findAllByType('button').find(item => item.children.includes('Select next text range'))!.props.onClick());
  await act(async () => button(ui, 'Highlight selected text').props.onClick());
  await act(async () => ui.root.findAllByType('button').find(item => item.children.join('') === 'Save text highlight')!.props.onClick());
  expect(createTextHighlight).toHaveBeenCalledWith(1, 0, 0, 4, 6, '', 0);
  act(() => ui.unmount());
});

it('opens a hidden note from the comments list without creating an on-page anchor', async () => {
  vi.mocked(documentAnnotations).mockResolvedValue(annotations(1, 0, [{ id: 'hidden', kind: 'note', page: 0, rect: null, contents: 'Hidden note' }]));
  let ui!: ReactTestRenderer; act(() => { ui = create(<App />); });
  await open(ui);
  await act(async () => button(ui, 'Comments').props.onClick());
  const hidden = ui.root.findAllByType('button').find(item => item.findAllByType('span').some(span => span.children.some(child => typeof child === 'string' && child.includes('Hidden note'))))!;
  act(() => hidden.props.onClick());
  expect(ui.root.findByProps({ 'aria-label': 'Comment text' }).props.value).toBe('Hidden note');
  act(() => ui.unmount());
});

it('does not route global shortcuts into the document while a comment editor is open', async () => {
  vi.mocked(documentAnnotations).mockResolvedValue(annotations());
  let ui!: ReactTestRenderer; act(() => { ui = create(<App />); });
  await open(ui);
  await act(async () => button(ui, 'Add comment').props.onClick());
  await act(async () => ui.root.findAllByType('button').find(item => item.children.includes('Place comment'))!.props.onClick());
  act(() => keydown!({ key: 's', ctrlKey: true, target: { matches: () => false } } as unknown as KeyboardEvent));
  act(() => keydown!({ key: 'h', ctrlKey: false, target: { matches: () => false } } as unknown as KeyboardEvent));
  expect(saveCopy).not.toHaveBeenCalled();
  expect(ui.root.findByProps({ 'aria-label': 'Comment text' })).toBeTruthy();
  expect(ui.root.findByType(Viewer).props.commentMode).toBe(true);
  act(() => ui.unmount());
});

it('keeps the active tab and applies the acknowledged revision when a note save is pending', async () => {
  vi.mocked(documentAnnotations).mockResolvedValue(annotations());
  let resolveCreate!: (value: DocumentInfo) => void;
  vi.mocked(createComment).mockImplementation(() => new Promise(resolve => { resolveCreate = resolve; }));
  let ui!: ReactTestRenderer; act(() => { ui = create(<App />); });
  await open(ui);
  await act(async () => button(ui, 'Add comment').props.onClick());
  await act(async () => ui.root.findAllByType('button').find(item => item.children.includes('Place comment'))!.props.onClick());
  act(() => ui.root.findByProps({ 'aria-label': 'Comment text' }).props.onChange({ target: { value: 'Persist me' } }));
  act(() => { void ui.root.findAllByType('button').find(item => item.children.join('') === 'Save comment')!.props.onClick(); });
  await act(async () => { await Promise.resolve(); });
  expect(button(ui, 'Close notes.pdf').props.disabled).toBe(true);
  act(() => keydown!({ key: 'Escape', ctrlKey: false, target: { matches: () => false } } as unknown as KeyboardEvent));
  act(() => keydown!({ key: 'w', ctrlKey: true, target: { matches: () => false } } as unknown as KeyboardEvent));
  act(() => { void button(ui, 'Close notes.pdf').props.onClick(); });
  expect(closeDocument).not.toHaveBeenCalled();
  await act(async () => { resolveCreate(document(1)); });
  expect(ui.root.findByType(Viewer).props.document).toMatchObject({ id: 1, revision: 1, dirty: true });
  expect(ui.root.findAllByProps({ 'aria-label': 'Comment text' })).toHaveLength(0);
  act(() => ui.unmount());
});
