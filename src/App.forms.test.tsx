import { act, create, type ReactTestRenderer } from 'react-test-renderer';
import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import App from './App';
import Viewer from './Viewer';
import { documentFormFields, fillFormCopy, openDocument } from './bridge';
import type { DocumentInfo } from './model';

vi.mock('./bridge', () => ({ native: false, openDocument: vi.fn(), reopenDocument: vi.fn(), closeDocument: vi.fn(), editPages: vi.fn(), saveCopy: vi.fn(), splitDocument: vi.fn(), cropPage: vi.fn(), cropPages: vi.fn(), combineDocuments: vi.fn(), insertPagesCopy: vi.fn(), replacePagesCopy: vi.fn(), documentFormFields: vi.fn(), createPdfFromImage: vi.fn(), documentPageLabels: vi.fn(), fillFormCopy: vi.fn() }));
vi.mock('./Viewer', () => ({ default: () => <div>Viewer</div> }));
const document = (id: number, revision: number): DocumentInfo => ({ id, name: `file-${id}.pdf`, path: `C:/file-${id}.pdf`, pages: [{ width: 612, height: 792 }], revision, dirty: true, can_undo: true, can_redo: false });
const fields = (id: number, revision: number) => ({ documentId: id, revision, status: 'supported' as const, reason: null, input: 'printable-latin1' as const, valueByteLimit: 4096, fields: [{ kind: 'text' as const, fieldId: 'name', name: 'Name', page: 0, value: 'Ada', maxLength: 20 }, { kind: 'checkbox' as const, fieldId: 'approved', name: 'Approve terms', page: 0, checked: false }, { kind: 'radio' as const, fieldId: 'delivery', name: 'Delivery', page: 0, options: [{ optionId: 'delivery-email', label: 'Email' }, { optionId: 'delivery-post', label: 'Post' }], selectedOptionId: null }, { kind: 'choice' as const, fieldId: 'priority', name: 'Priority', page: 0, presentation: 'dropdown' as const, options: [{ optionId: 'priority-low', label: 'Low' }, { optionId: 'priority-high', label: 'High' }], selectedOptionId: 'priority-low' }] });
beforeEach(() => { vi.clearAllMocks(); const storage = new Map(); vi.stubGlobal('window', { localStorage: { getItem: (key: string) => storage.get(key) ?? null, setItem: (key: string, value: string) => storage.set(key, value) }, addEventListener: vi.fn(), removeEventListener: vi.fn() }); });
afterEach(() => vi.unstubAllGlobals());
async function open(ui: ReactTestRenderer, info: DocumentInfo) { vi.mocked(openDocument).mockResolvedValue({ status: 'opened', document: info }); await act(async () => ui.root.findAllByType('button').find(item => item.children.includes('Open a file'))!.props.onClick()); }
const save = (ui: ReactTestRenderer) => ui.root.findAllByType('button').find(item => item.children.join('') === 'Save filled copy')!;

it('fills current tagged field patches into a new clean active tab while keeping the dirty source tab', async () => {
  const source = document(1, 4), copy: DocumentInfo = { ...document(2, 0), name: 'filled.pdf', path: 'C:/filled.pdf', dirty: false, can_undo: false, can_redo: false };
  vi.mocked(documentFormFields).mockResolvedValue(fields(1, 4));
  vi.mocked(fillFormCopy).mockResolvedValue({ path: copy.path, document: copy });
  let ui!: ReactTestRenderer; act(() => { ui = create(<App />); });
  await open(ui, source);
  act(() => ui.root.findAllByType('button').find(item => item.children.includes('All tools'))!.props.onClick());
  act(() => ui.root.findByProps({ title: 'Fill existing fields' }).props.onClick());
  await act(async () => {});
  act(() => ui.root.findByProps({ 'aria-label': 'Approve terms, page 1' }).props.onChange({ target: { checked: true } }));
  act(() => ui.root.findByProps({ 'aria-label': 'Delivery: Post, page 1' }).props.onChange());
  act(() => ui.root.findByProps({ 'aria-label': 'Priority, page 1' }).props.onChange({ target: { value: 'priority-high' } }));
  await act(async () => save(ui).props.onClick());
  expect(fillFormCopy).toHaveBeenCalledWith(1, 4, [{ fieldId: 'approved', kind: 'checkbox', checked: true }, { fieldId: 'delivery', kind: 'radio', optionId: 'delivery-post' }, { fieldId: 'priority', kind: 'choice', optionId: 'priority-high' }]);
  expect(ui.root.findByType(Viewer).props.document).toMatchObject({ id: 2, dirty: false, revision: 0 });
  expect(ui.root.findAllByType('span').some(item => item.children.includes('file-1.pdf') && item.children.includes(' *'))).toBe(true);
  act(() => ui.unmount());
});

it('disposes a stale field response after its tab changes', async () => {
  const first = document(1, 4), second = document(2, 5);
  let resolve!: (value: ReturnType<typeof fields>) => void;
  vi.mocked(documentFormFields).mockImplementation(() => new Promise(done => { resolve = done; }));
  let ui!: ReactTestRenderer; act(() => { ui = create(<App />); });
  await open(ui, first); await open(ui, second);
  act(() => ui.root.findAllByType('button').find(item => item.children.includes('All tools'))!.props.onClick());
  act(() => ui.root.findByProps({ title: 'Fill existing fields' }).props.onClick());
  const firstTab = ui.root.findAllByType('button').find(item => item.findAllByType('span').some(span => span.children.includes('file-1.pdf')))!;
  act(() => firstTab.props.onClick());
  await act(async () => resolve(fields(2, 5)));
  expect(ui.root.findAllByProps({ 'aria-label': 'Name' })).toHaveLength(0);
  expect(fillFormCopy).not.toHaveBeenCalled();
  act(() => ui.unmount());
});

it('shares an in-flight field query when the form dialog is closed and reopened', async () => {
  let resolve!: (value: ReturnType<typeof fields>) => void;
  vi.mocked(documentFormFields).mockImplementation(() => new Promise(done => { resolve = done; }));
  let ui!: ReactTestRenderer; act(() => { ui = create(<App />); });
  await open(ui, document(1, 4));
  act(() => ui.root.findAllByType('button').find(item => item.children.includes('All tools'))!.props.onClick());
  act(() => ui.root.findByProps({ title: 'Fill existing fields' }).props.onClick());
  act(() => ui.root.findAllByType('button').find(item => item.children.join('') === 'Cancel')!.props.onClick());
  act(() => ui.root.findByProps({ title: 'Fill existing fields' }).props.onClick());
  expect(documentFormFields).toHaveBeenCalledTimes(1);
  await act(async () => resolve(fields(1, 4)));
  expect(ui.root.findByProps({ 'aria-label': 'Name' }).props.value).toBe('Ada');
  vi.mocked(documentFormFields).mockResolvedValue(fields(1, 4));
  act(() => ui.root.findAllByType('button').find(item => item.children.join('') === 'Cancel')!.props.onClick());
  act(() => ui.root.findByProps({ title: 'Fill existing fields' }).props.onClick());
  await act(async () => {});
  expect(documentFormFields).toHaveBeenCalledTimes(2);
  act(() => ui.unmount());
});

it('retries a field query after a shared request rejects', async () => {
  let reject!: (reason: Error) => void;
  vi.mocked(documentFormFields).mockImplementation(() => new Promise((_resolve, fail) => { reject = fail; }));
  let ui!: ReactTestRenderer; act(() => { ui = create(<App />); });
  await open(ui, document(1, 4));
  act(() => ui.root.findAllByType('button').find(item => item.children.includes('All tools'))!.props.onClick());
  act(() => ui.root.findByProps({ title: 'Fill existing fields' }).props.onClick());
  await act(async () => reject(new Error('query failed')));
  expect(JSON.stringify(ui.toJSON())).toContain('query failed');
  vi.mocked(documentFormFields).mockResolvedValue(fields(1, 4));
  act(() => ui.root.findAllByType('button').find(item => item.children.join('') === 'Cancel')!.props.onClick());
  act(() => ui.root.findByProps({ title: 'Fill existing fields' }).props.onClick());
  await act(async () => {});
  expect(documentFormFields).toHaveBeenCalledTimes(2);
  expect(ui.root.findByProps({ 'aria-label': 'Name' }).props.value).toBe('Ada');
  act(() => ui.unmount());
});
