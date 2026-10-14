import { act, create, type ReactTestRenderer } from 'react-test-renderer';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { checkForUpdate, InstallRefused, type DownloadEvent } from './updateCommands';
import Updates from './Updates';

const attemptBridge = vi.hoisted(() => ({ read: vi.fn(), write: vi.fn(), clear: vi.fn() }));

vi.mock('@tauri-apps/api/app', () => ({ getVersion: async () => '0.2.0' }));
vi.mock('./updateCommands', async importOriginal => ({ ...await importOriginal<typeof import('./updateCommands')>(), checkForUpdate: vi.fn() }));
vi.mock('./bridge', () => ({ native: true }));
vi.mock('./updateAttempt', () => ({
  readUpdateAttempt: attemptBridge.read,
  writeUpdateAttempt: attemptBridge.write,
  clearUpdateAttempt: attemptBridge.clear
}));

describe('software updates', () => {
  beforeEach(() => {
    vi.clearAllMocks();
    attemptBridge.read.mockResolvedValue(null);
    attemptBridge.write.mockResolvedValue(undefined);
    attemptBridge.clear.mockResolvedValue(undefined);
  });
  const mount = async (dirty = false) => {
    const setBusy = vi.fn(), close = vi.fn();
    let ui!: ReactTestRenderer;
    await act(async () => { ui = create(<Updates dirty={dirty} busy={false} setBusy={setBusy} close={close} />); });
    return { ui, setBusy, close };
  };
  it('reports a successful no-update response', async () => {
    vi.mocked(checkForUpdate).mockResolvedValue(null);
    const { ui } = await mount();
    expect(JSON.stringify(ui.toJSON())).toContain('You have the latest released version.');
    act(() => ui.unmount());
  });
  it('retains and reports an interrupted download attempt', async () => {
    attemptBridge.read.mockResolvedValue({ schemaVersion: 1, fromVersion: '0.2.0', targetVersion: '0.3.0', phase: 'downloading' });
    vi.mocked(checkForUpdate).mockResolvedValue({ version: '0.3.0', download: vi.fn(), install: vi.fn(), close: vi.fn() } as never);
    const { ui } = await mount();
    expect(JSON.stringify(ui.toJSON())).toContain('The previous update download was interrupted. You can retry.');
    expect(attemptBridge.clear).not.toHaveBeenCalled();
    act(() => ui.unmount());
  });
  it('keeps installer recovery fail-closed until completion or manual repair', async () => {
    attemptBridge.read.mockResolvedValue({ schemaVersion: 1, fromVersion: '0.2.0', targetVersion: '0.3.0', phase: 'installing' });
    const download = vi.fn(), install = vi.fn();
    vi.mocked(checkForUpdate).mockResolvedValue({ version: '0.3.0', download, install, close: vi.fn() } as never);
    const { ui, setBusy } = await mount();
    const button = ui.root.findAllByType('button').find(node => node.children.includes('Install update and restart'))!;
    expect(button.props.disabled).toBe(true);
    expect(JSON.stringify(ui.toJSON())).toContain('Close PDF Workstation, wait for the installer to finish');
    expect(JSON.stringify(ui.toJSON())).toContain('run the latest signed installer manually');
    await act(async () => { await button.props.onClick(); });
    expect(download).not.toHaveBeenCalled();
    expect(install).not.toHaveBeenCalled();
    expect(attemptBridge.write).not.toHaveBeenCalled();
    expect(attemptBridge.clear).not.toHaveBeenCalled();
    expect(setBusy).not.toHaveBeenCalled();
    act(() => ui.unmount());
  });
  it.each([
    { schemaVersion: 1, fromVersion: '0.1.0', targetVersion: '0.2.0', phase: 'installing' },
    { schemaVersion: 1, fromVersion: '0.1.0', targetVersion: '0.3.0', phase: 'downloading' }
  ])('clears completed or stale attempt records', async attempt => {
    attemptBridge.read.mockResolvedValue(attempt);
    vi.mocked(checkForUpdate).mockResolvedValue(null);
    const { ui } = await mount();
    expect(attemptBridge.clear).toHaveBeenCalledOnce();
    expect(JSON.stringify(ui.toJSON())).not.toContain('previous update');
    act(() => ui.unmount());
  });
  it('blocks installation when any document is dirty, even if called directly', async () => {
    const download = vi.fn(), install = vi.fn(), close = vi.fn();
    vi.mocked(checkForUpdate).mockResolvedValue({ version: '0.3.0', download, install, close } as never);
    const { ui, setBusy } = await mount(true);
    const button = ui.root.findAllByType('button').find(node => node.children.includes('Install update and restart'))!;
    expect(button.props.disabled).toBe(true);
    await act(async () => { await button.props.onClick(); });
    expect(download).not.toHaveBeenCalled(); expect(install).not.toHaveBeenCalled(); expect(setBusy).not.toHaveBeenCalled();
    act(() => ui.unmount()); expect(close).toHaveBeenCalledOnce();
  });
  it('absorbs an asynchronous release failure while the dialog unmounts', async () => {
    const release = vi.fn().mockRejectedValue(new Error('updater cleanup unavailable'));
    vi.mocked(checkForUpdate).mockResolvedValue({ version: '0.3.0', download: vi.fn(), install: vi.fn(), close: release } as never);
    const { ui } = await mount();
    await act(async () => { ui.unmount(); await Promise.resolve(); });
    expect(release).toHaveBeenCalledOnce();
  });
  it('releases the busy guard and shows errors when signature verification fails', async () => {
    const download = vi.fn().mockRejectedValue(new Error('signature verification failed'));
    const install = vi.fn();
    vi.mocked(checkForUpdate).mockResolvedValue({ version: '0.3.0', download, install, close: vi.fn() } as never);
    const { ui, setBusy } = await mount();
    await act(async () => { ui.root.findAllByType('button').find(node => node.children.includes('Install update and restart'))!.props.onClick(); });
    expect(setBusy.mock.calls).toEqual([[true], [false]]);
    expect(JSON.stringify(ui.toJSON())).toContain('signature verification failed');
    expect(install).not.toHaveBeenCalled();
    expect(attemptBridge.clear).toHaveBeenCalledOnce();
    act(() => ui.unmount());
  });
  it('does not download when the interruption marker cannot be persisted', async () => {
    attemptBridge.write.mockRejectedValueOnce(new Error('flush failed'));
    const download = vi.fn(), install = vi.fn();
    vi.mocked(checkForUpdate).mockResolvedValue({ version: '0.3.0', download, install, close: vi.fn() } as never);
    const { ui, setBusy } = await mount();
    await act(async () => { ui.root.findAllByType('button').find(node => node.children.includes('Install update and restart'))!.props.onClick(); });
    expect(download).not.toHaveBeenCalled();
    expect(install).not.toHaveBeenCalled();
    expect(JSON.stringify(ui.toJSON())).toContain('The update was not started. You can retry.');
    expect(setBusy.mock.calls).toEqual([[true], [false]]);
    act(() => ui.unmount());
  });
  it('releases downloaded bytes and refreshes the updater handle when its handoff marker cannot be persisted', async () => {
    attemptBridge.write.mockResolvedValueOnce(undefined).mockRejectedValueOnce(new Error('flush failed'));
    const download = vi.fn().mockResolvedValue(undefined), install = vi.fn(), release = vi.fn().mockResolvedValue(undefined);
    const refreshed = { version: '0.3.0', download: vi.fn(), install: vi.fn(), close: vi.fn() };
    vi.mocked(checkForUpdate).mockResolvedValueOnce({ version: '0.3.0', download, install, close: release } as never).mockResolvedValueOnce(refreshed as never);
    const { ui, setBusy } = await mount();
    await act(async () => { ui.root.findAllByType('button').find(node => node.children.includes('Install update and restart'))!.props.onClick(); });
    expect(download).toHaveBeenCalledOnce();
    expect(install).not.toHaveBeenCalled();
    expect(release).toHaveBeenCalledOnce();
    expect(checkForUpdate).toHaveBeenCalledTimes(2);
    expect(JSON.stringify(ui.toJSON())).toContain('The installer was not started. You can retry.');
    expect(setBusy.mock.calls).toEqual([[true], [false]]);
    act(() => ui.unmount());
  });
  it('withholds retry when a downloaded updater resource cannot be released', async () => {
    attemptBridge.write.mockResolvedValueOnce(undefined).mockRejectedValueOnce(new Error('flush failed'));
    const release = vi.fn().mockRejectedValue(new Error('resource busy'));
    vi.mocked(checkForUpdate).mockResolvedValueOnce({ version: '0.3.0', download: vi.fn().mockResolvedValue(undefined), install: vi.fn(), close: release } as never);
    const { ui, setBusy } = await mount();
    await act(async () => { ui.root.findAllByType('button').find(node => node.children.includes('Install update and restart'))!.props.onClick(); });
    expect(release).toHaveBeenCalledOnce();
    expect(checkForUpdate).toHaveBeenCalledOnce();
    expect(ui.root.findAllByType('button').some(node => node.children.includes('Install update and restart'))).toBe(false);
    expect(JSON.stringify(ui.toJSON())).toContain('Close and reopen PDF Workstation to retry.');
    expect(setBusy.mock.calls).toEqual([[true], [false]]);
    act(() => ui.unmount());
  });
  it('reports network failure rather than claiming the app is current', async () => {
    vi.mocked(checkForUpdate).mockRejectedValue(new Error('offline'));
    const { ui } = await mount();
    expect(JSON.stringify(ui.toJSON())).toContain('Could not check for updates');
    expect(JSON.stringify(ui.toJSON())).not.toContain('latest released version');
    act(() => ui.unmount());
  });
  it('records each phase and clears the marker after a handled installer failure', async () => {
    let finishDownload!: () => void, failInstall!: (reason: Error) => void;
    const download = vi.fn(() => new Promise<void>(resolve => { finishDownload = resolve; }));
    const install = vi.fn(() => new Promise<void>((_, reject) => { failInstall = reject; }));
    const release = vi.fn().mockResolvedValue(undefined);
    const refreshed = { version: '0.3.0', download: vi.fn(), install: vi.fn(), close: vi.fn() };
    vi.mocked(checkForUpdate).mockResolvedValueOnce({ version: '0.3.0', download, install, close: release } as never).mockResolvedValueOnce(refreshed as never);
    const { ui, setBusy } = await mount();
    const button = ui.root.findAllByType('button').find(node => node.children.includes('Install update and restart'))!;
    await act(async () => button.props.onClick());
    expect(attemptBridge.write).toHaveBeenNthCalledWith(1, { schemaVersion: 1, fromVersion: '0.2.0', targetVersion: '0.3.0', phase: 'downloading' });
    await act(async () => finishDownload());
    expect(install).toHaveBeenCalledWith();
    expect(attemptBridge.write).toHaveBeenNthCalledWith(2, { schemaVersion: 1, fromVersion: '0.2.0', targetVersion: '0.3.0', phase: 'installing' });
    await act(async () => failInstall(new Error('ShellExecute failed')));
    expect(attemptBridge.clear).toHaveBeenCalledOnce();
    expect(release).toHaveBeenCalledOnce();
    expect(checkForUpdate).toHaveBeenCalledTimes(2);
    expect(JSON.stringify(ui.toJSON())).toContain('ShellExecute failed');
    expect(setBusy.mock.calls).toEqual([[true], [false]]);
    act(() => ui.unmount());
  });
  it('shows a declined native install confirmation calmly and keeps the offer', async () => {
    let answer!: (outcome: 'cancelled') => void;
    const download = vi.fn().mockResolvedValue(undefined);
    const install = vi.fn(() => new Promise<'cancelled'>(resolve => { answer = resolve; }));
    const release = vi.fn().mockResolvedValue(undefined);
    vi.mocked(checkForUpdate).mockResolvedValueOnce({ version: '0.3.0', download, install, close: release } as never);
    const { ui, setBusy } = await mount();
    const installButton = () => ui.root.findAllByType('button').find(node => node.children.includes('Install update and restart'))!;
    await act(async () => { installButton().props.onClick(); await Promise.resolve(); await Promise.resolve(); });
    expect(install).toHaveBeenCalledOnce();
    expect(JSON.stringify(ui.toJSON())).toContain('Waiting for you to confirm installing version 0.3.0');
    await act(async () => answer('cancelled'));
    expect(JSON.stringify(ui.toJSON())).toContain('Version 0.3.0 was not installed. Your installed version is unchanged. You can install it later.');
    expect(ui.root.findAllByProps({ role: 'alert' })).toHaveLength(0);
    expect(attemptBridge.clear).toHaveBeenCalledOnce();
    expect(release).not.toHaveBeenCalled();
    expect(checkForUpdate).toHaveBeenCalledOnce();
    expect(installButton().props.disabled).toBe(false);
    expect(setBusy.mock.calls).toEqual([[true], [false]]);
    act(() => ui.unmount());
    expect(release).toHaveBeenCalledOnce();
  });
  it('installs the kept download after a declined confirmation without downloading again', async () => {
    const download = vi.fn().mockResolvedValue(undefined);
    const install = vi.fn().mockResolvedValueOnce('cancelled').mockRejectedValueOnce(new Error('No downloaded update is available. Check for updates again.'));
    const release = vi.fn().mockResolvedValue(undefined);
    const refreshedDownload = vi.fn().mockResolvedValue(undefined);
    const refreshedInstall = vi.fn().mockResolvedValue('cancelled');
    vi.mocked(checkForUpdate)
      .mockResolvedValueOnce({ version: '0.3.0', download, install, close: release } as never)
      .mockResolvedValueOnce({ version: '0.3.0', download: refreshedDownload, install: refreshedInstall, close: vi.fn().mockResolvedValue(undefined) } as never);
    const { ui } = await mount();
    const installButton = () => ui.root.findAllByType('button').find(node => node.children.includes('Install update and restart'))!;
    await act(async () => installButton().props.onClick());
    expect(download).toHaveBeenCalledOnce();
    expect(JSON.stringify(ui.toJSON())).toContain('Version 0.3.0 was not installed');
    await act(async () => installButton().props.onClick());
    expect(download).toHaveBeenCalledOnce();
    expect(install).toHaveBeenCalledTimes(2);
    expect(attemptBridge.write.mock.calls.map(([attempt]) => attempt.phase)).toEqual(['downloading', 'installing', 'installing']);
    expect(ui.root.findByProps({ role: 'alert' }).children.join('')).toContain('No downloaded update is available');
    expect(release).toHaveBeenCalledOnce();
    await act(async () => installButton().props.onClick());
    expect(refreshedDownload).toHaveBeenCalledOnce();
    expect(refreshedInstall).toHaveBeenCalledOnce();
    act(() => ui.unmount());
  });
  it.each([
    'An update installation is already waiting for your answer.',
    'The update confirmation could not be attached to the PDF Workstation window. Nothing was installed.',
    'The update confirmation could not be shown. Nothing was installed.'
  ])('keeps the downloaded offer when the backend refuses before installing: %s', async refusal => {
    const download = vi.fn().mockResolvedValue(undefined);
    const install = vi.fn().mockRejectedValueOnce(new InstallRefused(refusal)).mockResolvedValueOnce('cancelled');
    const release = vi.fn().mockResolvedValue(undefined);
    vi.mocked(checkForUpdate).mockResolvedValueOnce({ version: '0.3.0', download, install, close: release } as never);
    const { ui, setBusy } = await mount();
    const installButton = () => ui.root.findAllByType('button').find(node => node.children.includes('Install update and restart'))!;
    await act(async () => installButton().props.onClick());
    expect(ui.root.findByProps({ role: 'alert' }).children.join('')).toBe(`Update not installed: ${refusal}`);
    expect(JSON.stringify(ui.toJSON())).toContain('The downloaded update is kept; you can retry.');
    expect(release).not.toHaveBeenCalled();
    expect(checkForUpdate).toHaveBeenCalledOnce();
    expect(attemptBridge.clear).toHaveBeenCalledOnce();
    expect(installButton().props.disabled).toBe(false);
    expect(setBusy.mock.calls).toEqual([[true], [false]]);
    await act(async () => installButton().props.onClick());
    expect(download).toHaveBeenCalledOnce();
    expect(install).toHaveBeenCalledTimes(2);
    expect(JSON.stringify(ui.toJSON())).toContain('Version 0.3.0 was not installed');
    act(() => ui.unmount());
    expect(release).toHaveBeenCalledOnce();
  });
  it('treats an installer promise resolving on Windows as a retryable failure', async () => {
    const download = vi.fn().mockResolvedValue(undefined);
    const install = vi.fn().mockResolvedValue(undefined);
    const release = vi.fn().mockResolvedValue(undefined);
    vi.mocked(checkForUpdate).mockResolvedValueOnce({ version: '0.3.0', download, install, close: release } as never).mockResolvedValueOnce(null);
    const { ui } = await mount();
    await act(async () => {
      ui.root.findAllByType('button').find(node => node.children.includes('Install update and restart'))!.props.onClick();
      await Promise.resolve();
      await Promise.resolve();
    });
    expect(JSON.stringify(ui.toJSON())).toContain('Installer returned without closing the app.');
    expect(release).toHaveBeenCalledOnce();
    act(() => ui.unmount());
  });
  it.each(['connection reset during download', 'download timed out'])('recovers from %s and retries without stale progress', async failure => {
    const attempts: { emit: (event: DownloadEvent) => void; resolve: () => void; reject: (reason: Error) => void }[] = [];
    const download = vi.fn((emit: (event: DownloadEvent) => void) => new Promise<void>((resolve, reject) => attempts.push({ emit, resolve, reject })));
    const install = vi.fn().mockRejectedValue(new Error('installer launch failed'));
    const release = vi.fn();
    const refreshedRelease = vi.fn();
    vi.mocked(checkForUpdate)
      .mockResolvedValueOnce({ version: '0.3.0', download, install, close: release } as never)
      .mockResolvedValueOnce({ version: '0.3.0', download: vi.fn(), install: vi.fn(), close: refreshedRelease } as never);
    const { ui, setBusy, close } = await mount();
    const installButton = () => ui.root.findAllByType('button').find(node => node.children.includes('Install update and restart'))!;
    const closeButton = () => ui.root.findAllByType('button').find(node => node.children.includes('Close'))!;
    await act(async () => installButton().props.onClick());
    expect(download).toHaveBeenCalledOnce();
    expect(download.mock.calls[0]).toEqual([expect.any(Function)]);
    expect(setBusy.mock.calls).toEqual([[true]]);
    expect(installButton().props.disabled).toBe(true);
    expect(closeButton().props.disabled).toBe(true);
    const preventDefault = vi.fn();
    act(() => ui.root.findByType('dialog').props.onCancel({ preventDefault }));
    expect(preventDefault).toHaveBeenCalledOnce();
    expect(close).not.toHaveBeenCalled();
    await act(async () => installButton().props.onClick());
    expect(download).toHaveBeenCalledOnce();
    act(() => {
      attempts[0].emit({ event: 'Started', data: { contentLength: 100 } });
      attempts[0].emit({ event: 'Progress', data: { chunkLength: 75 } });
    });
    expect(JSON.stringify(ui.toJSON())).toContain('Downloading update: 75%');
    await act(async () => attempts[0].reject(new Error(failure)));
    expect(setBusy.mock.calls).toEqual([[true], [false]]);
    expect(ui.root.findByProps({ role: 'alert' }).children.join('')).toContain(failure);
    expect(installButton().props.disabled).toBe(false);
    expect(closeButton().props.disabled).toBe(false);
    expect(release).not.toHaveBeenCalled();
    await act(async () => installButton().props.onClick());
    expect(download).toHaveBeenCalledTimes(2);
    expect(ui.root.findAllByProps({ role: 'alert' })).toHaveLength(0);
    expect(JSON.stringify(ui.toJSON())).toContain('Downloading signed update');
    act(() => {
      attempts[1].emit({ event: 'Started', data: { contentLength: 100 } });
      attempts[1].emit({ event: 'Progress', data: { chunkLength: 25 } });
    });
    expect(JSON.stringify(ui.toJSON())).toContain('Downloading update: 25%');
    act(() => attempts[1].emit({ event: 'Finished' }));
    expect(JSON.stringify(ui.toJSON())).toContain('Verifying update and starting installer');
    await act(async () => attempts[1].resolve());
    expect(install).toHaveBeenCalledWith();
    expect(setBusy.mock.calls).toEqual([[true], [false], [true], [false]]);
    act(() => ui.root.findByType('dialog').props.onCancel({ preventDefault }));
    expect(close).toHaveBeenCalledOnce();
    act(() => ui.unmount());
    expect(release).toHaveBeenCalledOnce();
    expect(refreshedRelease).toHaveBeenCalledOnce();
  });
});
