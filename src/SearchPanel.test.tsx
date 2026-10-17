import { act, create, type ReactTestRenderer } from 'react-test-renderer';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { pageText } from './bridge';
import SearchPanel from './SearchPanel';
import type { DocumentInfo } from './model';

vi.mock('./bridge', () => ({ pageText: vi.fn() }));
const document: DocumentInfo = { id: 1, name: 'test.pdf', path: 'test.pdf', pages: [{ width: 612, height: 792 }, { width: 612, height: 792 }], revision: 3, dirty: false, can_undo: false, can_redo: false };
describe('document search', () => {
  beforeEach(() => vi.clearAllMocks());
  async function mount(query = 'needle', onHighlights = vi.fn()) {
    let ui!: ReactTestRenderer;
    const go = vi.fn();
    await act(async () => { ui = create(<SearchPanel document={document} go={go} close={vi.fn()} onHighlights={onHighlights} />); });
    act(() => ui.root.findByProps({ 'aria-label': 'Find in document', maxLength: 500 }).props.onChange({ target: { value: query } }));
    return { ui, go, onHighlights };
  }
  const submit = (ui: ReactTestRenderer) => ui.root.findByType('form').props.onSubmit({ preventDefault: vi.fn() });
  it('finds literal case-insensitive text and navigates to the matching current page', async () => {
    vi.mocked(pageText).mockResolvedValueOnce('nothing here').mockResolvedValueOnce('A NEEDLE.* here');
    const { ui, go } = await mount('needle.*');
    await act(async () => { submit(ui); });
    expect(JSON.stringify(ui.toJSON())).toContain('1 matching page.');
    act(() => ui.root.findAllByType('button').find(button => button.children.join('') === 'Go to page 2')!.props.onClick());
    expect(go).toHaveBeenCalledWith(1);
    expect(pageText).toHaveBeenNthCalledWith(2, 1, 1, 3);
    act(() => ui.unmount());
  });
  it('discards pending results when the query changes', async () => {
    let resolve!: (text: string) => void;
    vi.mocked(pageText).mockImplementationOnce(() => new Promise(done => { resolve = done; })).mockResolvedValue('needle');
    const long = { ...document, pages: Array.from({ length: 12 }, () => document.pages[0]) };
    let ui!: ReactTestRenderer;
    await act(async () => { ui = create(<SearchPanel document={long} go={vi.fn()} close={vi.fn()} />); });
    act(() => ui.root.findByProps({ maxLength: 500 }).props.onChange({ target: { value: 'needle' } }));
    act(() => submit(ui));
    act(() => ui.root.findByProps({ maxLength: 500 }).props.onChange({ target: { value: 'different' } }));
    await act(async () => resolve('needle'));
    expect(pageText).toHaveBeenCalledTimes(8);
    expect(vi.mocked(pageText).mock.calls.length).toBeLessThan(long.pages.length);
    expect(JSON.stringify(ui.toJSON())).not.toContain('Go to page');
    act(() => ui.unmount());
  });
  it('reports extraction failure without claiming there are no matches', async () => {
    vi.mocked(pageText).mockRejectedValue(new Error('Document changed. Search again.'));
    const { ui } = await mount();
    await act(async () => submit(ui));
    expect(JSON.stringify(ui.toJSON())).toContain('Search could not finish');
    expect(JSON.stringify(ui.toJSON())).not.toContain('No matching text found');
    act(() => ui.unmount());
  });
  it('supports case-sensitive searches', async () => {
    vi.mocked(pageText).mockResolvedValue('NEEDLE');
    const { ui } = await mount();
    act(() => ui.root.findByProps({ type: 'checkbox' }).props.onChange({ target: { checked: true } }));
    await act(async () => submit(ui));
    expect(JSON.stringify(ui.toJSON())).toContain('No matching text found');
    act(() => ui.unmount());
  });
  it('cycles matching pages, highlights literal text, and resets navigation for a new query', async () => {
    vi.mocked(pageText).mockResolvedValue('Before <NEEDLE> after');
    const { ui, go } = await mount('<needle>');
    const button = (label: string) => ui.root.findAllByType('button').find(item => item.children.join('') === label)!;
    expect(button('Next matching page').props.disabled).toBe(true);
    await act(async () => submit(ui));
    expect(ui.root.findAllByType('mark').map(mark => mark.children.join(''))).toEqual(['<NEEDLE>', '<NEEDLE>']);
    for (const label of ['Next matching page', 'Next matching page', 'Next matching page', 'Previous matching page']) {
      act(() => button(label).props.onClick());
    }
    expect(go.mock.calls.map(call => call[0])).toEqual([0, 1, 0, 1]);
    expect(button('Go to page 2').props['aria-current']).toBe('location');
    act(() => ui.root.findByProps({ maxLength: 500 }).props.onChange({ target: { value: 'different' } }));
    expect(button('Next matching page').props.disabled).toBe(true);
    expect(ui.root.findAllByType('mark')).toHaveLength(0);
    act(() => ui.unmount());
  });
  it('keeps partial results and ignores a pending extraction after Stop', async () => {
    let resolve!: (text: string) => void;
    vi.mocked(pageText).mockResolvedValueOnce('needle first').mockImplementationOnce(() => new Promise(done => { resolve = done; }));
    const { ui, onHighlights } = await mount();
    await act(async () => submit(ui));
    act(() => ui.root.findAllByType('button').find(button => button.children.join('') === 'Stop')!.props.onClick());
    await act(async () => resolve('needle second'));
    expect(ui.root.findAllByType('mark')).toHaveLength(1);
    expect(JSON.stringify(ui.toJSON())).toContain('Results below may be incomplete');
    expect(onHighlights.mock.calls.at(-1)).toEqual([{ documentId: 1, revision: 3, query: 'needle', matchCase: false, pages: [0] }]);
    act(() => ui.unmount());
  });
  it('publishes matching pages then clears overlays for a changed query', async () => {
    vi.mocked(pageText).mockResolvedValueOnce('needle').mockResolvedValueOnce('nothing');
    const { ui, onHighlights } = await mount();
    await act(async () => { submit(ui); });
    expect(onHighlights).toHaveBeenCalledWith({ documentId: 1, revision: 3, query: 'needle', matchCase: false, pages: [0] });
    act(() => ui.root.findByProps({ maxLength: 500 }).props.onChange({ target: { value: 'changed' } }));
    expect(onHighlights.mock.calls.at(-1)).toEqual([null]);
    act(() => ui.unmount());
  });
  it('stops extraction after 500 matching pages and allows navigation through the capped results', async () => {
    vi.mocked(pageText).mockResolvedValue('needle');
    let ui!: ReactTestRenderer; const go = vi.fn();
    await act(async () => { ui = create(<SearchPanel document={{ ...document, pages: Array.from({ length: 501 }, () => document.pages[0]) }} go={go} close={vi.fn()} />); });
    act(() => ui.root.findByProps({ maxLength: 500 }).props.onChange({ target: { value: 'needle' } }));
    await act(async () => submit(ui));
    expect(vi.mocked(pageText).mock.calls.length).toBeGreaterThanOrEqual(500);
    expect(vi.mocked(pageText).mock.calls.length).toBeLessThanOrEqual(501);
    expect(ui.root.findAllByType('mark')).toHaveLength(500);
    expect(JSON.stringify(ui.toJSON())).toContain('first 500 matching pages');
    act(() => ui.root.findAllByType('button').find(button => button.children.join('') === 'Previous matching page')!.props.onClick());
    expect(go).toHaveBeenCalledWith(499);
    act(() => ui.unmount());
  }, 15_000);
});
