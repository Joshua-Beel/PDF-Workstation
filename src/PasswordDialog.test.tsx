import { act, create, type ReactTestRenderer } from 'react-test-renderer';
import { beforeEach, expect, it, vi } from 'vitest';
import PasswordDialog from './PasswordDialog';
import { cancelPasswordRequest, closeDocument, unlockDocument } from './bridge';
import type { DocumentInfo } from './model';
vi.mock('react', async importOriginal => {
  const react = await importOriginal<typeof import('react')>();
  return { ...react, useEffect: (effect: () => void | (() => void), dependencies: unknown[]) => react.useEffect(() => {
    const cleanup = effect(); cleanup?.();
    return effect();
  }, dependencies) };
});
vi.mock('./bridge', () => ({ unlockDocument: vi.fn(), cancelPasswordRequest: vi.fn(), closeDocument: vi.fn() }));
const challenge = { status: 'password_required' as const, request_id: 42, name: 'locked.pdf', incorrect: false };
const document: DocumentInfo = { id: 7, name: 'locked.pdf', path: 'locked.pdf', pages: [{ width: 612, height: 792 }], revision: 0, dirty: false, can_undo: false, can_redo: false };
beforeEach(() => { vi.clearAllMocks(); vi.mocked(cancelPasswordRequest).mockResolvedValue(); vi.mocked(closeDocument).mockResolvedValue(); });
it('clears passwords immediately, reports retry and opens only after success', async () => {
  vi.mocked(unlockDocument).mockResolvedValueOnce({ ...challenge, incorrect: true }).mockResolvedValueOnce({ status: 'opened', document });
  const onOpened = vi.fn(); let ui!: ReactTestRenderer;
  act(() => { ui = create(<PasswordDialog challenge={challenge} onOpened={onOpened} onClose={vi.fn()} />); });
  await act(async () => {});
  expect(cancelPasswordRequest).not.toHaveBeenCalled();
  expect(ui.root.findByType('input').props.type).toBe('password');
  for (const password of ['wrong', 'right']) {
    act(() => ui.root.findByType('input').props.onChange({ target: { value: password } }));
    await act(async () => { ui.root.findByType('form').props.onSubmit({ preventDefault() {} }); });
    expect(unlockDocument).toHaveBeenLastCalledWith(42, password);
    expect(ui.root.findByType('input').props.value).toBe('');
    if (password === 'wrong') { expect(onOpened).not.toHaveBeenCalled(); expect(JSON.stringify(ui.toJSON())).toContain('Incorrect password'); }
  }
  expect(onOpened).toHaveBeenCalledWith(document);
  await act(async () => ui.unmount()); expect(cancelPasswordRequest).toHaveBeenCalledWith(42);
});
it('cancels during opening and closes a late successful document without adding it', async () => {
  let resolve!: (value: { status: 'opened'; document: DocumentInfo }) => void;
  vi.mocked(unlockDocument).mockImplementation(() => new Promise(done => { resolve = done; }));
  const onOpened = vi.fn(), onClose = vi.fn(); let ui!: ReactTestRenderer;
  act(() => { ui = create(<PasswordDialog challenge={challenge} onOpened={onOpened} onClose={onClose} />); });
  act(() => { ui.root.findByType('form').props.onSubmit({ preventDefault() {} }); ui.root.findByType('form').props.onSubmit({ preventDefault() {} }); });
  expect(unlockDocument).toHaveBeenCalledTimes(1);
  act(() => ui.root.findAllByType('button')[0].props.onClick()); expect(onClose).toHaveBeenCalledOnce();
  act(() => ui.unmount());
  await act(async () => resolve({ status: 'opened', document }));
  expect(onOpened).not.toHaveBeenCalled(); expect(closeDocument).toHaveBeenCalledWith(7);
});
it('shows the backend over-1-KiB message and limits the input to 1024 UTF-8 bytes', async () => {
  vi.mocked(unlockDocument).mockRejectedValue('Passwords cannot be longer than 1 KiB.');
  let ui!: ReactTestRenderer; act(() => { ui = create(<PasswordDialog challenge={challenge} onOpened={vi.fn()} onClose={vi.fn()} />); });
  expect(ui.root.findByType('input').props.maxLength).toBe(1024);
  act(() => ui.root.findByType('input').props.onChange({ target: { value: 'é'.repeat(512) } }));
  expect(ui.root.findByType('input').props.value).toHaveLength(512);
  act(() => ui.root.findByType('input').props.onChange({ target: { value: 'é'.repeat(513) } }));
  expect(ui.root.findByType('input').props.value).toHaveLength(512);
  expect(JSON.stringify(ui.toJSON())).toContain('1 KiB');
  await act(async () => ui.root.findByType('form').props.onSubmit({ preventDefault() {} }));
  expect(JSON.stringify(ui.toJSON())).toContain('1 KiB'); expect(JSON.stringify(ui.toJSON())).not.toContain('Unable to unlock');
  act(() => ui.unmount());
});
it('closes on a size or open-count refusal and hands the backend message to the app', async () => {
  const refusal = 'Up to 32 PDFs can be open at once. Close a tab, then open this PDF again. Nothing was opened.';
  vi.mocked(unlockDocument).mockRejectedValue(refusal);
  const onRefused = vi.fn(), onClose = vi.fn(), onOpened = vi.fn(); let ui!: ReactTestRenderer;
  act(() => { ui = create(<PasswordDialog challenge={challenge} onOpened={onOpened} onClose={onClose} onRefused={onRefused} />); });
  act(() => ui.root.findByType('input').props.onChange({ target: { value: 'right' } }));
  await act(async () => ui.root.findByType('form').props.onSubmit({ preventDefault() {} }));
  expect(onRefused).toHaveBeenCalledWith(refusal); expect(onClose).not.toHaveBeenCalled(); expect(onOpened).not.toHaveBeenCalled();
  expect(JSON.stringify(ui.toJSON())).not.toContain('Unable to unlock');
  await act(async () => ui.unmount()); expect(cancelPasswordRequest).toHaveBeenCalledWith(42);
});
it('does not display native errors that might contain entered secrets', async () => {
  vi.mocked(unlockDocument).mockRejectedValue(new Error('sensitive password'));
  let ui!: ReactTestRenderer; act(() => { ui = create(<PasswordDialog challenge={challenge} onOpened={vi.fn()} onClose={vi.fn()} />); });
  await act(async () => ui.root.findByType('form').props.onSubmit({ preventDefault() {} }));
  expect(JSON.stringify(ui.toJSON())).toContain('Unable to unlock'); expect(JSON.stringify(ui.toJSON())).not.toContain('sensitive password');
  act(() => ui.unmount());
});
