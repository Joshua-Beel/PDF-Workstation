import { act, create, type ReactTestRenderer } from 'react-test-renderer';
import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import App from './App';
import Viewer from './Viewer';
import { documentPageLabels, editPageLabels, openDocument } from './bridge';
import type { DocumentInfo } from './model';

vi.mock('./bridge', () => ({ native: false, openDocument: vi.fn(), reopenDocument: vi.fn(), closeDocument: vi.fn(), editPages: vi.fn(), saveCopy: vi.fn(), documentAnnotations: vi.fn(), createPdfFromImage: vi.fn(), documentPageLabels: vi.fn(), editPageLabels: vi.fn(), documentBookmarks: vi.fn(), editBookmarks: vi.fn() }));
vi.mock('./Viewer', () => ({ default: () => <div /> }));

const document = (dirty = false): DocumentInfo => ({ id: 1, name: 'book.pdf', path: 'C:/book.pdf', pages: [{ width: 612, height: 792 }, { width: 612, height: 792 }, { width: 612, height: 792 }], revision: 0, dirty, can_undo: false, can_redo: false });
const snapshot = (labels: string[]) => ({ documentId: 1, revision: 0, status: 'supported' as const, reason: null, labels: labels.map((label, page) => ({ page, label })) });
beforeEach(() => {
  vi.clearAllMocks(); const storage = new Map();
  vi.stubGlobal('window', { localStorage: { getItem: (key: string) => storage.get(key) ?? null, setItem: (key: string, value: string) => storage.set(key, value) }, addEventListener: vi.fn(), removeEventListener: vi.fn() });
});
afterEach(() => vi.unstubAllGlobals());

it('edits page labels from the Pages panel, marks the document unsaved without a new revision, and refreshes the shown labels', async () => {
  vi.mocked(openDocument).mockResolvedValue({ status: 'opened', document: document() });
  vi.mocked(documentPageLabels).mockResolvedValueOnce({ documentId: 1, revision: 0, status: 'none' as const, reason: null, labels: [] });
  vi.mocked(editPageLabels).mockResolvedValueOnce({ view: { editable: true, reason: null, version: 0, page_count: 3, ranges: [], can_undo: false, can_redo: false, changed: false }, document: document() });
  let ui!: ReactTestRenderer; act(() => { ui = create(<App />); });
  await act(async () => ui.root.findAllByType('button').find(item => item.children.includes('Open a file'))!.props.onClick());
  await act(async () => ui.root.findByProps({ 'aria-label': 'Pages' }).props.onClick());
  await act(async () => ui.root.findAllByType('button').find(item => item.children.includes('Page labels'))!.props.onClick());
  expect(ui.root.findByProps({ 'aria-label': 'Page labels' }).type).toBe('aside');
  expect(ui.root.findAllByProps({ 'aria-label': 'Pages' }).filter(item => item.type === 'aside')).toHaveLength(0);

  vi.mocked(editPageLabels).mockResolvedValueOnce({ view: { editable: true, reason: null, version: 1, page_count: 3, ranges: [{ start_page: 0, end_page: 2, style: 'roman_lower', prefix: '', first: 1, first_label: 'i', last_label: 'iii' }], can_undo: true, can_redo: false, changed: true }, document: document(true) });
  vi.mocked(documentPageLabels).mockResolvedValueOnce(snapshot(['i', 'ii', 'iii']));
  const form = ui.root.findByType('form');
  act(() => form.findByType('select').props.onChange({ target: { value: 'roman_lower' } }));
  act(() => form.findAllByType('input')[0].props.onChange({ target: { value: '1' } }));
  await act(async () => ui.root.findByType('form').props.onSubmit({ preventDefault() {} }));
  expect(editPageLabels).toHaveBeenLastCalledWith(1, 0, { version: 0, op: { kind: 'set', start_page: 0, style: 'roman_lower', prefix: '', first: 1 } });
  expect(documentPageLabels).toHaveBeenCalledTimes(2);
  expect(JSON.stringify(ui.toJSON())).toContain('Unsaved changes');
  expect(ui.root.findByType(Viewer).props.document).toMatchObject({ id: 1, revision: 0, dirty: true });

  await act(async () => ui.root.findByProps({ 'aria-label': 'Close page labels' }).props.onClick());
  expect(ui.root.findAllByProps({ 'aria-label': 'Pages' }).some(item => item.type === 'aside')).toBe(true);
  expect(JSON.stringify(ui.toJSON())).toContain('Label: iii');
  act(() => ui.unmount());
});
