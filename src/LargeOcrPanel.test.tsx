import { act, create, type ReactTestRenderer } from 'react-test-renderer';
import { afterEach, expect, it, vi } from 'vitest';
import LargeOcrPanel, { pauseText, timeLeft, type LargeOcrApi } from './LargeOcrPanel';
import type { LargeOcrJob, LargeOcrStatus } from './bridge';

const job = (overrides: Partial<LargeOcrJob> = {}, progress: Partial<LargeOcrJob['progress']> = {}): LargeOcrJob => ({
  job: 'abc', sourceName: 'big.pdf', destination: 'E:/out/big-searchable.pdf', running: true, pauseReason: null, report: [], interrupted: false,
  progress: { state: 'running', pagesDone: 411, pageCount: 1000, workers: 4, issues: 0, bytesWritten: 1, projectedBytes: null, sizeWarning: false, secondsLeft: 300, ...progress },
  ...overrides,
});
const status = (value: LargeOcrJob | null): LargeOcrStatus => ({ available: true, reason: null, maxPages: 10_000, job: value });
const api = (current: LargeOcrStatus): LargeOcrApi & { [key: string]: ReturnType<typeof vi.fn> } => ({
  status: vi.fn().mockResolvedValue(current), pause: vi.fn().mockResolvedValue(current), cancel: vi.fn().mockResolvedValue(status(null)),
  resume: vi.fn().mockResolvedValue(current), dismiss: vi.fn().mockResolvedValue(status(null)), relocate: vi.fn().mockResolvedValue(null),
});
const flatten = (node: unknown): string => typeof node === 'string' ? node : Array.isArray(node) ? node.map(flatten).join('') : node && typeof node === 'object' && 'children' in node ? flatten((node as { children: unknown }).children) : '';
const text = (ui: ReactTestRenderer) => flatten(ui.toJSON());
const button = (ui: ReactTestRenderer, label: string) => ui.root.findAllByType('button').find(item => item.children.join('') === label)!;

afterEach(() => { vi.useRealTimers(); });

async function mount(value: LargeOcrApi, document: { id: number; revision: number } | null = { id: 3, revision: 9 }) {
  const openCopy = vi.fn();
  let ui!: ReactTestRenderer;
  await act(async () => { ui = create(<LargeOcrPanel document={document} refresh={0} openCopy={openCopy} api={value} />); });
  return { ui, openCopy };
}

it('renders nothing without a job', async () => {
  const { ui } = await mount(api(status(null)));
  expect(ui.toJSON()).toBeNull();
});

it('shows progress, pauses, and asks before cancelling', async () => {
  const calls = api(status(job()));
  const { ui } = await mount(calls);
  expect(text(ui)).toContain('Page 412 of 1,000');
  expect(text(ui)).toContain('about 5 minutes left');
  await act(async () => { button(ui, 'Pause').props.onClick(); });
  expect(calls.pause).toHaveBeenCalledOnce();
  await act(async () => { button(ui, 'Cancel').props.onClick(); });
  expect(calls.cancel).not.toHaveBeenCalled();
  expect(text(ui)).toContain('finished pages will be discarded');
  await act(async () => { button(ui, 'Cancel OCR').props.onClick(); });
  expect(calls.cancel).toHaveBeenCalledOnce();
});

it('warns about a projected size over 1 GiB', async () => {
  const { ui } = await mount(api(status(job({}, { sizeWarning: true }))));
  expect(text(ui)).toContain('larger than 1 GiB');
});

it('resumes a paused job with the open document and offers discard', async () => {
  const paused = job({ running: false, pauseReason: { kind: 'engineFailing', message: '5 pages in a row could not be read' } }, { state: 'paused', pagesDone: 640 });
  const calls = api(status(paused));
  const { ui } = await mount(calls);
  expect(text(ui)).toContain('5 pages in a row could not be read');
  await act(async () => { button(ui, 'Resume').props.onClick(); });
  expect(calls.resume).toHaveBeenCalledWith(3, 9);
  expect(text(ui)).toContain('The copy will be saved as E:/out/big-searchable.pdf.');
  await act(async () => { button(ui, 'Save elsewhere…').props.onClick(); });
  expect(calls.relocate).toHaveBeenCalledWith(3, 9);
  await act(async () => { button(ui, 'Discard progress').props.onClick(); });
  expect(calls.cancel).toHaveBeenCalledOnce();
});

it('cannot resume without an open document', async () => {
  const { ui } = await mount(api(status(job({ running: false, interrupted: true }, { state: 'paused' }))), null);
  expect(button(ui, 'Resume').props.disabled).toBe(true);
  expect(text(ui)).toContain('when PDF Workstation closed');
});

it('lists unread and reduced pages when the copy is saved, and opens it', async () => {
  const saved = job({ running: false, report: [
    { page: 36, outcome: 'imageOnly', issue: 'timedOut', reducedResolution: false, droppedWords: 0 },
    { page: 87, outcome: 'placeholder', issue: 'renderFailed', reducedResolution: false, droppedWords: 0 },
    { page: 200, outcome: 'text', issue: 'none', reducedResolution: true, droppedWords: 0 },
  ] }, { state: 'published', pagesDone: 1000 });
  const { ui, openCopy } = await mount(api(status(saved)));
  expect(text(ui)).toContain('998 of 1,000 pages have searchable text');
  expect(text(ui)).toContain('page 37 (timed out)');
  expect(text(ui)).toContain('page 88 (could not be displayed and is blank in the copy)');
  expect(text(ui)).toContain('page 201 (reduced resolution)');
  await act(async () => { button(ui, 'Open copy').props.onClick(); });
  expect(openCopy).toHaveBeenCalledWith('E:/out/big-searchable.pdf');
});

it('words time left and pause reasons plainly', () => {
  expect(timeLeft(null)).toBe('');
  expect(timeLeft(20)).toBe('less than a minute left');
  expect(timeLeft(61)).toBe('about 1 minute left');
  expect(timeLeft(4 * 3600)).toBe('about 4 hours left');
  expect(pauseText({ kind: 'sizeLimit' }, job())).toContain('larger than 1 GiB');
  expect(pauseText({ kind: 'disk', message: 'There is not enough space on the disk.' }, job())).toContain('Free some space');
});
