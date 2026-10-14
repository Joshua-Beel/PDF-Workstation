import { act, create, type ReactTestRenderer } from 'react-test-renderer';
import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import App from './App';
import { reopenDocument } from './bridge';
import { readRecentFiles, rememberFile, saveRecentFiles, type RecentFile } from './recentFiles';
import type { DocumentInfo } from './model';

vi.mock('./bridge', () => ({ native: false, openDocument: vi.fn(), reopenDocument: vi.fn(), closeDocument: vi.fn(), editPages: vi.fn(), saveCopy: vi.fn(), documentPageLabels: vi.fn() }));
vi.mock('./Viewer', () => ({ default: () => <div>Rendered document</div> }));
let storage: Map<string, string>;
const file: RecentFile = { path: 'C:/docs/one.pdf', name: 'one.pdf', pages: 1, starred: false };
const document: DocumentInfo = { id: 1, name: file.name, path: file.path, pages: [{ width: 612, height: 792 }], revision: 0, dirty: false, can_undo: false, can_redo: false };
beforeEach(() => {
  vi.clearAllMocks(); storage = new Map();
  vi.stubGlobal('window', { localStorage: { getItem: (key: string) => storage.get(key) ?? null, setItem: (key: string, value: string) => storage.set(key, value) }, addEventListener: vi.fn(), removeEventListener: vi.fn() });
});
afterEach(() => vi.unstubAllGlobals());
it('validates stored history, removes duplicates and bounds it to 50 entries', () => {
  const values = [null, {}, { ...file, pages: -1 }, { ...file, path: '\0bad' }, file, file, ...Array.from({ length: 60 }, (_, i) => ({ ...file, path: `C:/docs/${i}.pdf` }))];
  storage.set('pdf-workstation.recent-files.v1', JSON.stringify(values));
  expect(readRecentFiles()).toHaveLength(50);
  expect(readRecentFiles()[0]).toEqual(file);
  storage.set('pdf-workstation.recent-files.v1', 'broken json'); expect(readRecentFiles()).toEqual([]);
});
it('refreshes metadata and keeps stars without storing contents or unsaved edits', () => {
  const result = rememberFile([{ ...file, starred: true }], { ...document, dirty: true, revision: 5 });
  expect(result).toEqual([{ ...file, starred: true }]);
  expect(saveRecentFiles(result)).toBe(true); expect(readRecentFiles()).toEqual(result);
});
it('persists stars across app remounts, reopens history and reuses an existing tab', async () => {
  saveRecentFiles([file]); vi.mocked(reopenDocument).mockResolvedValue({ status: 'opened', document });
  let ui!: ReactTestRenderer;
  await act(async () => { ui = create(<App />); });
  act(() => ui.root.findByProps({ 'aria-label': 'Star one.pdf' }).props.onClick());
  act(() => ui.unmount());
  await act(async () => { ui = create(<App />); });
  expect(ui.root.findByProps({ 'aria-label': 'Unstar one.pdf' })).toBeTruthy();
  const row = () => ui.root.findAllByType('button').find(button => button.findAllByType('small').some(small => small.children.includes('PDF document')))!;
  await act(async () => row().props.onClick());
  expect(reopenDocument).toHaveBeenCalledWith(file.path);
  expect(JSON.stringify(ui.toJSON())).toContain('Rendered document');
  act(() => ui.root.findByProps({ 'aria-label': 'Home' }).props.onClick());
  await act(async () => row().props.onClick());
  expect(reopenDocument).toHaveBeenCalledOnce();
  act(() => ui.unmount());
});
it('shows a gate refusal on its own without the moved-or-deleted wording', async () => {
  saveRecentFiles([file]); vi.mocked(reopenDocument).mockRejectedValue('This path cannot be opened: network (UNC) paths are not supported.');
  let ui!: ReactTestRenderer;
  await act(async () => { ui = create(<App />); });
  const row = ui.root.findAllByType('button').find(button => button.findAllByType('small').some(small => small.children.includes('PDF document')))!;
  await act(async () => row.props.onClick());
  const text = JSON.stringify(ui.toJSON());
  expect(text).toContain('network (UNC) paths are not supported'); expect(text).not.toContain('moved or been deleted');
  act(() => ui.unmount());
});
it('shows a size or open-count refusal without the moved-or-deleted wording', async () => {
  saveRecentFiles([file]); vi.mocked(reopenDocument).mockRejectedValue('Up to 32 PDFs can be open at once. Close a tab, then open this PDF again. Nothing was opened.');
  let ui!: ReactTestRenderer;
  await act(async () => { ui = create(<App />); });
  const row = ui.root.findAllByType('button').find(button => button.findAllByType('small').some(small => small.children.includes('PDF document')))!;
  await act(async () => row.props.onClick());
  const text = JSON.stringify(ui.toJSON());
  expect(text).toContain('Could not reopen this file. Up to 32 PDFs can be open at once.'); expect(text).not.toContain('moved or been deleted');
  act(() => ui.unmount());
});
it('reports missing files and clears history without closing tabs or deleting PDFs', async () => {
  saveRecentFiles([file]); vi.mocked(reopenDocument).mockRejectedValue(new Error('File not found'));
  let ui!: ReactTestRenderer;
  await act(async () => { ui = create(<App />); });
  const row = ui.root.findAllByType('button').find(button => button.findAllByType('small').some(small => small.children.includes('PDF document')))!;
  await act(async () => row.props.onClick());
  expect(JSON.stringify(ui.toJSON())).toContain('Could not reopen this file');
  expect(JSON.stringify(ui.toJSON())).toContain('moved or been deleted');
  expect(readRecentFiles()).toEqual([file]);
  act(() => ui.root.findAllByType('button').find(button => button.children.includes('Clear file history'))!.props.onClick());
  expect(readRecentFiles()).toEqual([]);
  act(() => ui.unmount());
});
