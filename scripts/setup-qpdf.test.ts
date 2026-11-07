// Offline tests of scripts/setup-qpdf.ps1 (SM3 P2-5 scaffolding for SM7 review). They never touch the network:
// each case builds a small stand-in zip and its own pins file, and passes it with -ZipPath and -PinsPath.
import { spawnSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { existsSync, mkdtempSync, readdirSync, readFileSync, realpathSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { afterEach, describe, expect, it } from 'vitest';

const script = join(import.meta.dirname, 'setup-qpdf.ps1');
const folders: string[] = [];
afterEach(() => { for (const folder of folders.splice(0)) rmSync(folder, { recursive: true, force: true }); });
// Work roots under the temp folder are allowed either because CI is set (the hosted runner, whose TEMP is on
// C:) or because TEMP is off C: (local runs keep TEMP on E:). Tests that clear CI never use a temp work root.
const scratch = () => { const folder = mkdtempSync(join(tmpdir(), 'sm3-setup-qpdf-')); folders.push(folder); return folder; };
const sha256 = (path: string) => createHash('sha256').update(readFileSync(path)).digest('hex');
// The hosted runner's TEMP is an 8.3 short path (C:\Users\RUNNER~1\...), and the script's GetFullPath expands it
// to the long form, so paths are compared by their real long form, case-insensitively, as Windows does.
const inside = (path: string, folder: string) => {
  const child = realpathSync.native(path).toLowerCase();
  const parent = realpathSync.native(folder).toLowerCase().replace(/\\+$/, '');
  return child.startsWith(`${parent}\\`);
};

/** Builds a zip whose entries are the given names, each holding its own name as text. */
function zip(folder: string, entries: string[]): string {
  const path = join(folder, 'qpdf.zip');
  const list = entries.map(entry => `'${entry.replaceAll("'", "''")}'`).join(',');
  const command = `Add-Type -AssemblyName System.IO.Compression; $s=[IO.File]::Create('${path}'); $z=New-Object IO.Compression.ZipArchive($s,[IO.Compression.ZipArchiveMode]::Create); foreach($n in @(${list})){ $w=New-Object IO.StreamWriter($z.CreateEntry($n).Open()); $w.Write($n); $w.Dispose() }; $z.Dispose(); $s.Dispose()`;
  const made = spawnSync('pwsh.exe', ['-NoProfile', '-NonInteractive', '-Command', command], { encoding: 'utf8', timeout: 60_000 });
  expect(made.status, made.stderr).toBe(0);
  return path;
}

function pins(folder: string, zipPath: string, files: Record<string, string> | null, override: Partial<{ size: number; sha256: string }> = {}): string {
  const path = join(folder, 'pins.json');
  const size = readFileSync(zipPath).length;
  writeFileSync(path, JSON.stringify({ version: 'test', zip: { url: 'https://invalid.example/never-fetched.zip', size: override.size ?? size, sha256: override.sha256 ?? sha256(zipPath) }, files }));
  return path;
}

function run(args: string[], env: Record<string, string | undefined> = {}) {
  const result = spawnSync('pwsh.exe', ['-NoProfile', '-NonInteractive', '-File', script, ...args], { encoding: 'utf8', timeout: 90_000, env: { ...process.env, RUNNER_TEMP: undefined, ...env } });
  return { status: result.status, out: result.stdout, err: result.stderr };
}

const binHashes = (folder: string, names: string[]) => Object.fromEntries(names.map(name => [name, createHash('sha256').update(name).digest('hex')]));

/** cmd's `%~sI` short form of an existing folder, read only. */
function shortName(folder: string): string {
  const probe = spawnSync('cmd.exe', ['/d', '/s', '/c', `"for %I in ("${folder}") do @echo %~sI"`], { encoding: 'utf8', timeout: 60_000, windowsVerbatimArguments: true });
  return probe.status === 0 ? probe.stdout.trim() : '';
}

/** An environment variable looked up by name in any case, as Windows does (Git Bash passes PROGRAMFILES). */
function environment(name: string): string | undefined {
  const key = Object.keys(process.env).find(candidate => candidate.toLowerCase() === name.toLowerCase());
  return key === undefined ? undefined : process.env[key];
}

/** A folder with a distinct 8.3 short name: Program Files, the temp folder (RUNNER~1 on the hosted runner) or
 * the user profile, whichever has one first. `tried` says what each candidate gave, for the failure message. */
function shortFolder(): { found: { long: string; short: string } | null; tried: string[] } {
  const tried: string[] = [];
  const candidates: [string, string | undefined][] = [['ProgramFiles', environment('ProgramFiles')], ['TEMP', tmpdir()], ['USERPROFILE', environment('USERPROFILE')]];
  for (const [name, candidate] of candidates) {
    if (!candidate || !existsSync(candidate)) {
      tried.push(`${name}: ${candidate ?? 'not set'} (missing)`);
      continue;
    }
    const long = realpathSync.native(candidate);
    const short = shortName(long);
    tried.push(`${name}: ${long} -> ${short || 'no short form'}`);
    if (short.includes('~') && short.toLowerCase() !== long.toLowerCase() && existsSync(short)) return { found: { long, short }, tried };
  }
  return { found: null, tried };
}
const { found: shortPath, tried: shortTried } = shortFolder();

describe('path comparison used by these tests', () => {
  it.skipIf(!shortPath && !environment('CI'))('treats an 8.3 short path and its long form as the same folder, in both directions', () => {
    expect(shortPath, `CI must provide a folder with an 8.3 short name; tried ${shortTried.join('; ')}`).not.toBeNull();
    const { long, short } = shortPath!;
    const child = readdirSync(long)[0];
    expect(child, `${long} has an entry to compare`).toBeDefined();
    const folders = `long ${long}, short ${short}`;
    expect(inside(join(short, child), long), folders).toBe(true);
    expect(inside(join(long, child), short.toUpperCase()), folders).toBe(true);
    expect(inside(long, join(short, child)), folders).toBe(false);
    expect(inside(short, long), `a folder is not inside itself: ${folders}`).toBe(false);
  });
});

describe('setup-qpdf.ps1', { timeout: 240_000 }, () => {
  it('verifies the zip and every pinned binary, then names qpdf.exe in a fresh extract', () => {
    const folder = scratch();
    const names = ['qpdf-test/bin/qpdf.exe', 'qpdf-test/bin/qpdf30.dll', 'qpdf-test/README.txt'];
    const archive = zip(folder, names);
    const work = join(folder, 'work');
    const result = run(['-PinsPath', pins(folder, archive, binHashes(folder, names.slice(0, 2))), '-ZipPath', archive, '-WorkRoot', work]);
    expect(result.status, result.err).toBe(0);
    const line = result.out.split(/\r?\n/).find(text => text.startsWith('QPDF_EXE='));
    expect(line).toBeDefined();
    const exe = line!.slice('QPDF_EXE='.length);
    expect(inside(exe, work), `${exe} is not under ${work}`).toBe(true);
    expect(readFileSync(exe, 'utf8')).toBe('qpdf-test/bin/qpdf.exe');
    const again = run(['-PinsPath', pins(folder, archive, binHashes(folder, names.slice(0, 2))), '-ZipPath', archive, '-WorkRoot', work]);
    expect(again.status, again.err).toBe(0);
    expect(existsSync(exe), 'the previous extract is removed, never reused').toBe(false);
  });

  it('refuses a zip entry that would leave the extraction folder, and writes nothing outside it', () => {
    const folder = scratch();
    const archive = zip(folder, ['qpdf-test/bin/qpdf.exe', '../escaped.txt']);
    const work = join(folder, 'work');
    const result = run(['-PinsPath', pins(folder, archive, binHashes(folder, ['qpdf-test/bin/qpdf.exe'])), '-ZipPath', archive, '-WorkRoot', work]);
    expect(result.status).not.toBe(0);
    expect(result.err).toContain('would leave the extraction folder');
    expect(existsSync(join(work, 'escaped.txt')) || existsSync(join(folder, 'escaped.txt'))).toBe(false);
  });

  it('refuses a zip whose size or SHA-256 differs from the pins before extracting anything', () => {
    const folder = scratch();
    const archive = zip(folder, ['qpdf-test/bin/qpdf.exe']);
    const work = join(folder, 'work');
    const hashed = run(['-PinsPath', pins(folder, archive, {}, { sha256: '0'.repeat(64) }), '-ZipPath', archive, '-WorkRoot', work]);
    expect(hashed.status).not.toBe(0);
    expect(hashed.err).toContain('SHA-256');
    const sized = run(['-PinsPath', pins(folder, archive, {}, { size: 1 }), '-ZipPath', archive, '-WorkRoot', work]);
    expect(sized.status).not.toBe(0);
    expect(sized.err).toContain('size');
    expect(readdirSync(work), 'no run or extract folder is left behind').toEqual([]);
    expect(hashed.err + sized.err).not.toMatch(/zip entry|extracted item/);
  });

  it('refuses an unpinned or altered binary', () => {
    const folder = scratch();
    const names = ['qpdf-test/bin/qpdf.exe', 'qpdf-test/bin/extra.dll'];
    const archive = zip(folder, names);
    const work = join(folder, 'work');
    const unpinned = run(['-PinsPath', pins(folder, archive, binHashes(folder, names.slice(0, 1))), '-ZipPath', archive, '-WorkRoot', work]);
    expect(unpinned.err).toContain('unpinned executable or library qpdf-test/bin/extra.dll');
    const altered = run(['-PinsPath', pins(folder, archive, { ...binHashes(folder, names), 'qpdf-test/bin/qpdf.exe': 'f'.repeat(64) }), '-ZipPath', archive, '-WorkRoot', work]);
    expect(altered.err).toContain('does not match its pinned SHA-256');
  });

  it('with no per-file pins yet, prints the hashes to review and does not make qpdf available', () => {
    const folder = scratch();
    const archive = zip(folder, ['qpdf-test/bin/qpdf.exe']);
    const result = run(['-PinsPath', pins(folder, archive, null), '-ZipPath', archive, '-WorkRoot', join(folder, 'work')]);
    expect(result.status).toBe(3);
    expect(JSON.parse(result.out)).toEqual(binHashes(folder, ['qpdf-test/bin/qpdf.exe']));
    expect(result.out).not.toContain('QPDF_EXE=');
  });

  it('needs an explicit work root outside CI and refuses C: there', () => {
    const folder = scratch();
    const archive = zip(folder, ['qpdf-test/bin/qpdf.exe']);
    const pinsPath = pins(folder, archive, binHashes(folder, ['qpdf-test/bin/qpdf.exe']));
    expect(run(['-PinsPath', pinsPath, '-ZipPath', archive]).err).toContain('pass -WorkRoot');
    expect(run(['-PinsPath', pinsPath, '-ZipPath', archive, '-WorkRoot', 'C:\\sm3-qpdf-never-created'], { CI: undefined }).err).toContain('is on C:');
    expect(existsSync('C:\\sm3-qpdf-never-created')).toBe(false);
  });

  // The work root must be off C: so the work-root check passes and the zip-cache check is the one that fires.
  // The temp folder cannot be used: the hosted runner's TEMP is on C:. The repository is on E: locally and
  // on D: on the runner. The script refuses the cache before it creates the work root, so nothing is written.
  const repositoryWorkRoot = join(import.meta.dirname, '..', 'tmp', `sm3-qpdf-never-created-${process.pid}`);
  it.skipIf(/^[Cc]:/.test(repositoryWorkRoot))('refuses a zip cache on C: outside CI, before creating anything', () => {
    const folder = scratch();
    const archive = zip(folder, ['qpdf-test/bin/qpdf.exe']);
    const pinsPath = pins(folder, archive, binHashes(folder, ['qpdf-test/bin/qpdf.exe']));
    try {
      const refused = run(['-PinsPath', pinsPath, '-WorkRoot', repositoryWorkRoot, '-ZipCache', 'C:\\sm3-qpdf-cache-never-created'], { CI: undefined });
      expect(refused.status).not.toBe(0);
      expect(refused.err).toContain('zip cache');
      expect(existsSync('C:\\sm3-qpdf-cache-never-created')).toBe(false);
      expect(existsSync(repositoryWorkRoot), 'the work root is not created').toBe(false);
    } finally {
      rmSync(repositoryWorkRoot, { recursive: true, force: true });
    }
  });

  it('the committed pins name the reviewed release, its signature facts and every binary in it', () => {
    const committed = JSON.parse(readFileSync(join(import.meta.dirname, 'qpdf', 'pins.json'), 'utf8'));
    expect(committed.version).toBe('12.4.2');
    expect(committed.zip).toEqual({ url: 'https://github.com/qpdf/qpdf/releases/download/v12.4.2/qpdf-12.4.2-msvc64.zip', size: 28183147, sha256: 'db87077e683630c1217e0e8f9a20a9749d952ab676e881c3689187763a5de25d' });
    expect(committed.releaseChecksumFile.pgpClearSigned).toBe(true);
    expect(committed.releaseChecksumFile.signingKeyFingerprint).toBe('C2C96B10011FE009E6D1DF828A75D10998012C7E');
    expect(committed.releaseChecksumFile.fingerprintSource).toContain('qpdf/qpdf/main/README.md');
    const files = Object.entries(committed.files as Record<string, string>);
    expect(files).toHaveLength(12);
    for (const [name, hash] of files) {
      expect(name).toMatch(/^qpdf-12\.4\.2-msvc64\/bin\/[A-Za-z0-9_.-]+\.(exe|dll)$/);
      expect(hash).toMatch(/^[0-9a-f]{64}$/);
    }
    expect(committed.files['qpdf-12.4.2-msvc64/bin/qpdf.exe']).toBe('1bdaf878f37eaffea1ea3c30591214c77636a0f2fc7432427516b6e22e36c0ff');
  });
});
