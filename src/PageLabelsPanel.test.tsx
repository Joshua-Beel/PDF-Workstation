import { act, create, type ReactTestRenderer } from 'react-test-renderer';
import { beforeEach, expect, it, vi } from 'vitest';
import PageLabelsPanel, { describeRange } from './PageLabelsPanel';
import { editPageLabels, type PageLabelEditResult, type PageLabelRange } from './bridge';
import type { DocumentInfo } from './model';

vi.mock('./bridge', () => ({ editPageLabels: vi.fn() }));

const document = (dirty = false): DocumentInfo => ({ id: 3, name: 'book.pdf', path: 'C:/book.pdf', pages: Array.from({ length: 8 }, () => ({ width: 612, height: 792 })), revision: 2, dirty, can_undo: false, can_redo: false });
const range = (start_page: number, end_page: number, patch: Partial<PageLabelRange> = {}): PageLabelRange => ({ start_page, end_page, style: 'decimal', prefix: '', first: 1, first_label: '1', last_label: '2', ...patch });
const result = (ranges: PageLabelRange[], patch: Partial<PageLabelEditResult['view']> = {}, dirty = false): PageLabelEditResult => ({ view: { editable: true, reason: null, version: 0, page_count: 8, ranges, can_undo: false, can_redo: false, changed: false, ...patch }, document: document(dirty) });
const buttons = (ui: ReactTestRenderer) => ui.root.findAllByType('button');
const button = (ui: ReactTestRenderer, label: string) => ui.root.findAll(item => item.type === 'button' && item.props['aria-label'] === label)[0];
const text = (ui: ReactTestRenderer) => JSON.stringify(ui.toJSON());
async function mount(first: PageLabelEditResult, props: Partial<Parameters<typeof PageLabelsPanel>[0]> = {}) {
  vi.mocked(editPageLabels).mockResolvedValueOnce(first);
  let ui!: ReactTestRenderer;
  await act(async () => { ui = create(<PageLabelsPanel document={document()} page={2} close={() => {}} {...props} />); });
  return ui;
}
const submit = async (ui: ReactTestRenderer) => act(async () => ui.root.findByType('form').props.onSubmit({ preventDefault() {} }));
const change = (ui: ReactTestRenderer, label: string, value: string) => act(() => {
  const field = ui.root.findByType('form').findAll(item => (item.type === 'input' || item.type === 'select') && item.parent?.type === 'label' && String(item.parent.props.children[0]).startsWith(label))[0];
  field.props.onChange({ target: { value } });
});

beforeEach(() => vi.clearAllMocks());

it('describes ranges in words with their first and last label', () => {
  expect(describeRange(range(0, 3, { first_label: 'i', last_label: 'iv' }))).toBe('Pages 1 to 4: labels i to iv');
  expect(describeRange(range(4, 4, { first_label: 'Index', last_label: 'Index' }))).toBe('Page 5: label Index');
  expect(describeRange(range(0, 1, { first_label: '', last_label: '' }))).toBe('Pages 1 to 2: label (blank)');
});

it('loads the ranges without changing anything and offers the current page as the start', async () => {
  const ui = await mount(result([range(0, 3, { style: 'roman_lower', first_label: 'i', last_label: 'iv' }), range(4, 7, { first_label: '1', last_label: '4' })]));
  expect(editPageLabels).toHaveBeenCalledExactlyOnceWith(3, 2, null);
  expect(text(ui)).toContain('Pages 1 to 4: labels i to iv');
  expect(text(ui)).toContain('Pages 5 to 8: labels 1 to 4');
  expect(ui.root.findAllByType('input')[0].props.value).toBe('3');
  expect(ui.root.findByProps({ role: 'note' }).children.join('')).toContain('not part of recovery points');
  expect(button(ui, 'Undo page label change').props.disabled).toBe(true);
  expect(buttons(ui).find(item => item.children.includes('Add range'))).toBeTruthy();
});

it('adds a range with the entered style, prefix and number using zero-based pages', async () => {
  const onDocument = vi.fn(), onChanged = vi.fn();
  const ui = await mount(result([]), { onDocument, onChanged });
  expect(text(ui)).toContain('This document has no page labels');
  change(ui, 'Start at page', '5'); change(ui, 'Numbering', 'alpha_upper'); change(ui, 'Prefix', 'App-'); change(ui, 'First number', '3');
  vi.mocked(editPageLabels).mockResolvedValueOnce(result([range(0, 3), range(4, 7, { style: 'alpha_upper', prefix: 'App-', first: 3, first_label: 'App-C', last_label: 'App-F' })], { version: 1, changed: true, can_undo: true }, true));
  await submit(ui);
  expect(editPageLabels).toHaveBeenLastCalledWith(3, 2, { version: 0, op: { kind: 'set', start_page: 4, style: 'alpha_upper', prefix: 'App-', first: 3 } });
  expect(onDocument).toHaveBeenCalledWith(expect.objectContaining({ dirty: true, revision: 2 }));
  expect(onChanged).toHaveBeenCalledTimes(1);
  expect(text(ui)).toContain('Pages 5 to 8: labels App-C to App-F');
  expect(ui.root.findByProps({ role: 'status' }).children.join('')).toContain('page 5');
  expect(button(ui, 'Undo page label change').props.disabled).toBe(false);
});

it('sends a prefix-only range without a style and refuses an empty one before calling the backend', async () => {
  const ui = await mount(result([]));
  change(ui, 'Numbering', 'none');
  await submit(ui);
  expect(ui.root.findByProps({ role: 'alert' }).children.join('')).toContain('numbering style or enter a prefix');
  expect(editPageLabels).toHaveBeenCalledTimes(1);
  change(ui, 'Prefix', 'Cover');
  vi.mocked(editPageLabels).mockResolvedValueOnce(result([range(0, 7)], { version: 1, changed: true }, true));
  await submit(ui);
  expect(editPageLabels).toHaveBeenLastCalledWith(3, 2, { version: 0, op: { kind: 'set', start_page: 2, style: null, prefix: 'Cover', first: 1 } });
  expect(ui.root.findAllByProps({ role: 'alert' })).toHaveLength(0);
});

it('validates the page and the first number locally', async () => {
  const ui = await mount(result([]));
  for (const [label, value, message] of [['Start at page', '0', 'from 1 to 8'], ['Start at page', '9', 'from 1 to 8'], ['Start at page', '1.5', 'from 1 to 8'], ['First number', '0', 'first number of 1 or more']] as const) {
    change(ui, 'Start at page', '2'); change(ui, 'First number', '1'); change(ui, label, value);
    await submit(ui);
    expect(ui.root.findByProps({ role: 'alert' }).children.join('')).toContain(message);
  }
  expect(editPageLabels).toHaveBeenCalledTimes(1);
});

it('loads a range into the form, replaces it, deletes it and undoes', async () => {
  const first = result([range(0, 3, { style: 'roman_lower', first_label: 'i', last_label: 'iv' }), range(4, 7, { style: null, prefix: 'Note ', first: 1, first_label: 'Note ', last_label: 'Note ' })], { version: 4, can_undo: true, changed: true }, true);
  const ui = await mount(first);
  await act(async () => button(ui, 'Edit the range starting at page 5').props.onClick());
  const inputs = ui.root.findAllByType('input');
  expect(inputs[0].props.value).toBe('5'); expect(inputs[1].props.value).toBe('Note '); expect(ui.root.findByType('select').props.value).toBe('none');
  expect(buttons(ui).find(item => item.children.includes('Replace range'))).toBeTruthy();

  vi.mocked(editPageLabels).mockResolvedValueOnce(result([range(0, 7, { style: 'roman_lower' })], { version: 5, can_undo: true, changed: true }, true));
  await act(async () => button(ui, 'Delete the range starting at page 5').props.onClick());
  expect(editPageLabels).toHaveBeenLastCalledWith(3, 2, { version: 4, op: { kind: 'delete', start_page: 4 } });
  expect(ui.root.findAllByType('li')).toHaveLength(1);
  expect(ui.root.findAllByType('li')[0].findByType('p').children.join('')).toContain('Pages 1 to 8');

  vi.mocked(editPageLabels).mockResolvedValueOnce(first);
  await act(async () => button(ui, 'Undo page label change').props.onClick());
  expect(editPageLabels).toHaveBeenLastCalledWith(3, 2, { version: 5, op: { kind: 'undo' } });
});

it('shows why labels cannot be edited and offers no form', async () => {
  const ui = await mount(result([], { editable: false, reason: 'Signed or certified PDFs cannot be edited in this build; signatures must remain intact.' }));
  expect(ui.root.findByProps({ role: 'alert' }).children.join('')).toContain('Signed or certified');
  expect(ui.root.findAllByType('form')).toHaveLength(0);
  const failing = vi.mocked(editPageLabels).mockRejectedValueOnce('The page labels changed. Reload them and try again.');
  let broken!: ReactTestRenderer; await act(async () => { broken = create(<PageLabelsPanel document={document()} close={() => {}} />); });
  expect(failing).toHaveBeenCalled();
  expect(broken.root.findByProps({ role: 'alert' }).children.join('')).toContain('changed');
});

it('reports a failed edit and keeps the earlier ranges', async () => {
  const ui = await mount(result([range(0, 7, { first_label: '1', last_label: '8' })]));
  vi.mocked(editPageLabels).mockRejectedValueOnce('Page is out of range.');
  await submit(ui);
  expect(ui.root.findByProps({ role: 'alert' }).children.join('')).toContain('out of range');
  expect(text(ui)).toContain('Pages 1 to 8: labels 1 to 8');
});

it('limits the prefix field to what the backend accepts and reports pending state', async () => {
  const onPending = vi.fn();
  const ui = await mount(result([], { changed: true }), { onPending });
  expect(onPending).toHaveBeenCalledWith(true);
  const prefix = ui.root.findByType('form').findAll(item => item.type === 'input' && item.props.type === 'text')[0];
  expect(prefix.props.maxLength).toBe(255);
});

it('returns focus into the panel after deleting a range', async () => {
  const focused: string[] = [];
  const nodeMock = (name: string) => ({ focus: () => { focused.push(name); }, querySelector: () => ({ focus: () => { focused.push('first remaining button'); } }) });
  vi.mocked(editPageLabels).mockResolvedValueOnce(result([range(0, 3), range(4, 7)], { changed: true }));
  let ui!: ReactTestRenderer;
  await act(async () => { ui = create(<PageLabelsPanel document={document()} page={2} close={() => {}} />, { createNodeMock: element => nodeMock(String(element.type)) }); });
  vi.mocked(editPageLabels).mockResolvedValueOnce(result([range(0, 7)], { version: 1, changed: true }));
  await act(async () => { button(ui, 'Delete the range starting at page 5').props.onClick(); });
  expect(focused).toEqual(['first remaining button']);
});
