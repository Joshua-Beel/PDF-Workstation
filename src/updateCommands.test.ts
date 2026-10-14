import { beforeEach, describe, expect, it, vi } from 'vitest';
import { invoke } from '@tauri-apps/api/core';
import { checkForUpdate, InstallRefused, type DownloadEvent } from './updateCommands';

vi.mock('@tauri-apps/api/core', () => ({
  invoke: vi.fn(),
  Channel: class { onmessage: (event: unknown) => void = () => {}; }
}));

const offer = { currentVersion: '0.2.0', version: '0.3.0', body: 'Notes', signature: 'c2ln', check: 7 };
const key = { version: '0.3.0', signature: 'c2ln', check: 7 };
const forbidden = ['proxy', 'headers', 'target', 'allowDowngrades', 'endpoints', 'pubkey', 'timeout', 'restartAfterInstall', 'rid'];
const sentKeys = () => vi.mocked(invoke).mock.calls.flatMap(([, args]) => {
  const record = (args ?? {}) as Record<string, unknown>;
  return [...Object.keys(record), ...Object.keys((record.update ?? {}) as object)];
});

describe('app-owned updater commands', () => {
  beforeEach(() => { vi.mocked(invoke).mockReset(); });

  it('checks through the app command with no webview-supplied options', async () => {
    vi.mocked(invoke).mockResolvedValueOnce(null);
    expect(await checkForUpdate()).toBeNull();
    expect(invoke).toHaveBeenCalledExactlyOnceWith('check_for_update');
    expect(vi.mocked(invoke).mock.calls.some(([command]) => String(command).startsWith('plugin:updater'))).toBe(false);
  });

  it('downloads and installs only the checked offer, bound by version and signature', async () => {
    vi.mocked(invoke).mockResolvedValueOnce(offer);
    const update = (await checkForUpdate())!;
    expect(update.version).toBe('0.3.0');
    expect(update.body).toBe('Notes');
    const events: DownloadEvent[] = [];
    vi.mocked(invoke).mockImplementationOnce(async (_command, args) => {
      const channel = (args as { onEvent: { onmessage: (event: DownloadEvent) => void } }).onEvent;
      channel.onmessage({ event: 'Started', data: { contentLength: 10 } });
      channel.onmessage({ event: 'Finished' });
    });
    await update.download(event => events.push(event));
    expect(events.map(event => event.event)).toEqual(['Started', 'Finished']);
    expect(vi.mocked(invoke).mock.calls[1][0]).toBe('download_update');
    expect((vi.mocked(invoke).mock.calls[1][1] as { update: unknown }).update).toEqual(key);
    vi.mocked(invoke).mockResolvedValueOnce('cancelled');
    expect(await update.install()).toBe('cancelled');
    expect(vi.mocked(invoke).mock.calls[2]).toEqual(['install_update', { update: key }]);
    vi.mocked(invoke).mockResolvedValueOnce(undefined);
    await update.close();
    expect(vi.mocked(invoke).mock.calls[3]).toEqual(['release_update', { update: key }]);
    expect(sentKeys().filter(name => forbidden.includes(name))).toEqual([]);
  });

  it('marks only install refusals that keep the offer', async () => {
    vi.mocked(invoke).mockResolvedValueOnce(offer);
    const update = (await checkForUpdate())!;
    vi.mocked(invoke).mockRejectedValueOnce({ message: 'An update installation is already waiting for your answer.', offerKept: true });
    const kept = await update.install().catch(reason => reason);
    expect(kept).toBeInstanceOf(InstallRefused);
    expect(kept.message).toBe('An update installation is already waiting for your answer.');
    vi.mocked(invoke).mockRejectedValueOnce({ message: 'installer failed', offerKept: false });
    await expect(update.install()).rejects.toBe('installer failed');
    vi.mocked(invoke).mockRejectedValueOnce('plain failure');
    await expect(update.install()).rejects.toBe('plain failure');
  });

  it('releases only its own offer, so a stale dialog cannot release a newer check', async () => {
    vi.mocked(invoke).mockResolvedValueOnce(offer).mockResolvedValueOnce({ ...offer, check: 8 });
    const closed = (await checkForUpdate())!;
    const reopened = (await checkForUpdate())!;
    vi.mocked(invoke).mockResolvedValue(undefined);
    await closed.close();
    await reopened.close();
    expect(vi.mocked(invoke).mock.calls.slice(2)).toEqual([
      ['release_update', { update: key }],
      ['release_update', { update: { ...key, check: 8 } }]
    ]);
  });
});
