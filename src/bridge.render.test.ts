import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { invoke } from '@tauri-apps/api/core';
import { renderPage } from './bridge';

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn(), isTauri: () => true }));

beforeEach(() => { vi.clearAllMocks(); vi.stubGlobal('URL', { createObjectURL: vi.fn(() => 'blob:page') }); });
afterEach(() => { vi.unstubAllGlobals(); });

it('sends a ticket with a cancellable render and cancels exactly that ticket on abort', async () => {
  let finish!: (bytes: ArrayBuffer) => void;
  vi.mocked(invoke).mockImplementation(async (command: string) => command === 'render_page' ? new Promise<ArrayBuffer>(resolve => { finish = resolve; }) : undefined);
  const stale = new AbortController();
  const pending = renderPage(3, 7, 900, stale.signal);
  const [command, args] = vi.mocked(invoke).mock.calls[0] as [string, { id: number; page: number; width: number; ticket: number }];
  expect(command).toBe('render_page');
  expect(args).toMatchObject({ id: 3, page: 7, width: 900 });
  expect(Number.isSafeInteger(args.ticket)).toBe(true);
  stale.abort();
  expect(vi.mocked(invoke)).toHaveBeenLastCalledWith('cancel_render', { ticket: args.ticket });
  finish(new ArrayBuffer(4));
  await expect(pending).resolves.toBe('blob:page');
});

it('does not cancel a render that already finished, gives each render its own ticket and keeps the plain call unchanged', async () => {
  vi.mocked(invoke).mockResolvedValue(new ArrayBuffer(4));
  const done = new AbortController();
  await renderPage(1, 0, 600, done.signal);
  await renderPage(1, 1, 600, new AbortController().signal);
  done.abort();
  expect(vi.mocked(invoke).mock.calls.map(([command]) => command)).toEqual(['render_page', 'render_page']);
  const tickets = vi.mocked(invoke).mock.calls.map(([, args]) => (args as { ticket: number }).ticket);
  expect(tickets[1]).toBe(tickets[0] + 1);
  await renderPage(1, 2, 600);
  expect(vi.mocked(invoke)).toHaveBeenLastCalledWith('render_page', { id: 1, page: 2, width: 600 });
});

it('refuses an already aborted render without calling the worker', async () => {
  const stale = new AbortController(); stale.abort();
  await expect(renderPage(1, 0, 600, stale.signal)).rejects.toBe('Viewer render was cancelled.');
  expect(vi.mocked(invoke)).not.toHaveBeenCalled();
});
