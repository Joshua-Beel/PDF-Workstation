import { afterAll, beforeAll, describe, expect, it } from 'vitest';
import { existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { spawnSync } from 'node:child_process';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { displayKeyId, parsePublicKey } from './verify-updater-signature.mjs';

const workflow = readFileSync('.github/workflows/updater-secret-check.yml', 'utf8').replace(/\r\n/g, '\n');
const releaseWorkflow = readFileSync('.github/workflows/release.yml', 'utf8').replace(/\r\n/g, '\n');
const checkScript = readFileSync('scripts/updater-secret-check.mjs', 'utf8').replace(/\r\n/g, '\n');
const tauriCli = 'node_modules/@tauri-apps/cli/tauri.js';
const throwawayPassword = 'throwaway-test-password';

function runBlocks(source: string) {
  const blocks: string[] = [];
  const lines = source.split('\n');
  for (let index = 0; index < lines.length; index += 1) {
    const match = /^(\s*)(?:-\s+)?run:\s*(.*)$/.exec(lines[index]);
    if (!match) continue;
    if (match[2] !== '|') { blocks.push(match[2]); continue; }
    const body: string[] = [];
    const indent = match[1].length;
    while (index + 1 < lines.length && (lines[index + 1].trim() === '' || lines[index + 1].search(/\S/) > indent + 2)) body.push(lines[++index]);
    blocks.push(body.join('\n'));
  }
  return blocks;
}

function signingEnvLines(source: string) {
  return source.split('\n').filter(line => /^\s+TAURI_SIGNING_PRIVATE_KEY(?:_PASSWORD)?:/.test(line)).map(line => line.trim());
}

describe('updater secret check workflow contract', () => {
  it('runs only by manual dispatch', () => {
    expect(workflow).toMatch(/^on:\n  workflow_dispatch:\n(?!\s)/m);
    expect(workflow).not.toMatch(/^\s+(?:push|pull_request|pull_request_target|schedule|workflow_run|workflow_call|release|repository_dispatch):/m);
  });

  it('holds the secrets in one release-environment job with a read-only token and no persisted checkout credentials', () => {
    expect(workflow).toMatch(/^permissions: \{\}$/m);
    expect(workflow.match(/^\s+environment:/gm)).toHaveLength(1);
    expect(workflow).toContain('  check:\n    needs: master-only\n    if: github.ref == \'refs/heads/master\'\n    runs-on: windows-2022\n    environment: release\n    permissions:\n      contents: read\n    steps:\n');
    expect(workflow).toContain('  master-only:\n    runs-on: ubuntu-24.04\n    permissions: {}\n');
    expect(workflow).not.toMatch(/:\s*write\b/);
    expect(workflow).toContain('persist-credentials: false');
    expect(workflow).toContain('ref: ${{ github.sha }}');
  });

  it('refuses every ref but master before the environment is reached, and again inside it', () => {
    const guard = workflow.slice(workflow.indexOf('  master-only:\n'), workflow.indexOf('  check:\n'));
    expect(guard).toContain('if [ "$GITHUB_REF" != \'refs/heads/master\' ]; then');
    expect(guard).toContain('exit 1');
    expect(guard).not.toContain('environment:');
    expect(guard).not.toContain('secrets.');
    const check = workflow.slice(workflow.indexOf('  check:\n'));
    const firstStep = check.slice(check.indexOf('    steps:\n'), check.indexOf('      - uses:'));
    expect(firstStep).toContain("if ($env:GITHUB_REF -cne 'refs/heads/master') { throw 'Run the updater secret check from master only.' }");
  });

  it('uses exactly the updater signing secret names and passing of release.yml, in the signing step only', () => {
    const secretNames = [...new Set([...workflow.matchAll(/secrets\.([A-Z0-9_]+)/g)].map(match => match[1]))];
    expect(secretNames).toEqual(['TAURI_SIGNING_PRIVATE_KEY']);
    expect(workflow.match(/secrets\./g)).toHaveLength(1);
    expect(workflow).not.toMatch(/AZURE_|GITHUB_TOKEN|github\.token/);
    expect(signingEnvLines(workflow)).toEqual(signingEnvLines(releaseWorkflow));
    expect(signingEnvLines(workflow)).toHaveLength(2);
    const signingStep = workflow.slice(workflow.indexOf('      - name: Sign a throwaway file'), workflow.indexOf('      - name: Remove the throwaway file'));
    expect(signingStep).toContain('node scripts/updater-secret-check.mjs $root scripts/updater-signing-key.json');
    for (const line of signingEnvLines(workflow)) expect(signingStep).toContain(line);
  });

  it('uploads, caches, publishes, tags and writes nothing', () => {
    expect(workflow).not.toMatch(/upload-artifact|download-artifact|actions\/cache|rust-cache|^\s*cache:/m);
    expect(workflow).toContain("node-version: '22'\n          package-manager-cache: false\n");
    expect(workflow).not.toMatch(/\bgh\s|git\s+(?:push|tag|commit)|GITHUB_OUTPUT|GITHUB_ENV|GITHUB_STEP_SUMMARY|GITHUB_PATH|::add-mask::|::set-output/);
    expect(workflow).toContain('- run: npm ci --ignore-scripts\n');
  });

  it('pins every action to an immutable full commit SHA', () => {
    const uses = [...workflow.matchAll(/^\s*-?\s*uses:\s*([^\s#]+)\s*(?:#.*)?$/gm)].map(match => match[1]);
    expect(uses).toEqual([
      'actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1',
      'actions/setup-node@949feb2413d6458794dcd2491c4babbbce0c15c1',
    ]);
    for (const use of uses) expect(releaseWorkflow).toContain(`uses: ${use}`);
  });

  it('always removes the throwaway folder the check used', () => {
    const cleanup = workflow.slice(workflow.indexOf('      - name: Remove the throwaway file and signature'));
    expect(cleanup).toContain('        if: always()\n');
    const root = '$root = Join-Path $env:RUNNER_TEMP "updater-secret-check-$($env:GITHUB_RUN_ID)-$($env:GITHUB_RUN_ATTEMPT)"';
    expect(workflow.split(root)).toHaveLength(3);
    expect(cleanup).toContain(root);
    expect(cleanup).toContain('if (Test-Path -LiteralPath $root) { Remove-Item -LiteralPath $root -Recurse -Force }');
    expect(cleanup).toContain("if (Test-Path -LiteralPath $root) { throw 'The throwaway check folder could not be removed.' }");
    expect(cleanup).not.toContain('secrets.');
    expect(checkScript).toMatch(/\} finally \{\n\s+rmSync\(workDir, \{ recursive: true, force: true \}\);/);
  });

  it('never echoes, logs or writes the secret variables', () => {
    for (const block of runBlocks(workflow)) {
      expect(block).not.toMatch(/TAURI_SIGNING|secrets\.|set -x|Set-PSDebug|Get-ChildItem\s+env:|\bprintenv\b|\benv\b\s*$/m);
    }
    expect(workflow).not.toMatch(/ACTIONS_STEP_DEBUG|ACTIONS_RUNNER_DEBUG/);
    expect(checkScript).toContain("stdio: 'ignore'");
    expect(checkScript).not.toContain('TAURI_SIGNING_PRIVATE_KEY_PASSWORD');
    expect(checkScript.match(/TAURI_SIGNING_PRIVATE_KEY/g)).toHaveLength(1);
    expect(checkScript).toContain('if (!env.TAURI_SIGNING_PRIVATE_KEY) return');
    expect(checkScript).not.toMatch(/console\.(?:log|error|warn|info)\((?!formatResult\(result\)|'UPDATER_SECRET_CHECK=fail reason=check-error'|'Usage: )/);
    expect(checkScript).not.toMatch(/writeFileSync\([^)]*env/);
  });
});

describe('updater secret check script with throwaway keys', () => {
  let root: string;
  let plainKey: string;
  let protectedKey: string;
  let plainPin: string;
  let protectedPin: string;
  let plainKeyId: string;
  let protectedKeyId: string;

  function generate(name: string, password?: string) {
    const keyPath = join(root, name);
    const args = [tauriCli, 'signer', 'generate', '--ci', '-w', keyPath];
    if (password) args.push('-p', password);
    const result = spawnSync(process.execPath, args, { encoding: 'utf8', timeout: 60_000, stdio: ['ignore', 'pipe', 'pipe'] });
    expect(result.status).toBe(0);
    const pub = readFileSync(`${keyPath}.pub`, 'utf8').trim();
    const pinPath = join(root, `${name}-pin.json`);
    writeFileSync(pinPath, JSON.stringify({ releaseSigningPublicKey: pub, forbiddenEmbeddedPublicKey: null }));
    return { key: readFileSync(keyPath, 'utf8').trim(), pinPath, keyId: displayKeyId(parsePublicKey(pub).keyId) };
  }

  function check(workDir: string, pinPath: string, key: string | undefined, password: string) {
    const env: Record<string, string> = { ...process.env } as Record<string, string>;
    delete env.TAURI_SIGNING_PRIVATE_KEY;
    delete env.TAURI_SIGNING_PRIVATE_KEY_PASSWORD;
    delete env.TAURI_SIGNING_PRIVATE_KEY_PATH;
    if (key !== undefined) env.TAURI_SIGNING_PRIVATE_KEY = key;
    env.TAURI_SIGNING_PRIVATE_KEY_PASSWORD = password;
    return spawnSync(process.execPath, ['scripts/updater-secret-check.mjs', workDir, pinPath], { encoding: 'utf8', timeout: 60_000, env });
  }

  function expectNoSecretOutput(result: ReturnType<typeof check>, key: string) {
    const output = `${result.stdout}${result.stderr}`;
    expect(output).not.toContain(throwawayPassword);
    expect(output).not.toContain(key);
    expect(output).not.toContain(Buffer.from(key, 'base64').toString('utf8').split('\n')[1] ?? key);
    expect(result.stderr).toBe('');
  }

  beforeAll(() => {
    root = mkdtempSync(join(tmpdir(), 'smacrobat-secret-check-'));
    const plain = generate('plain.key');
    const protectedOne = generate('protected.key', throwawayPassword);
    ({ key: plainKey, pinPath: plainPin, keyId: plainKeyId } = plain);
    ({ key: protectedKey, pinPath: protectedPin, keyId: protectedKeyId } = protectedOne);
  }, 120_000);

  afterAll(() => {
    if (root) rmSync(root, { recursive: true, force: true });
  });

  it('passes for an unencrypted key with the empty password release.yml passes, and prints only the result line', () => {
    const workDir = join(root, 'work-plain');
    const result = check(workDir, plainPin, plainKey, '');
    expect(result.status).toBe(0);
    expect(result.stdout).toBe(`UPDATER_SECRET_CHECK=pass key=${plainKeyId} reason=verified\n`);
    expectNoSecretOutput(result, plainKey);
    expect(existsSync(workDir)).toBe(false);
  }, 60_000);

  it('passes for a password-protected key with its password', () => {
    const workDir = join(root, 'work-protected');
    const result = check(workDir, protectedPin, protectedKey, throwawayPassword);
    expect(result.status).toBe(0);
    expect(result.stdout).toBe(`UPDATER_SECRET_CHECK=pass key=${protectedKeyId} reason=verified\n`);
    expectNoSecretOutput(result, protectedKey);
    expect(existsSync(workDir)).toBe(false);
  }, 60_000);

  it('fails without printing anything secret when the password is wrong or empty', () => {
    for (const password of ['', 'not-the-password']) {
      const workDir = join(root, `work-bad-password-${password.length}`);
      const result = check(workDir, protectedPin, protectedKey, password);
      expect(result.status).toBe(1);
      expect(result.stdout).toBe(`UPDATER_SECRET_CHECK=fail key=${protectedKeyId} reason=signer-failed\n`);
      expectNoSecretOutput(result, protectedKey);
      expect(existsSync(workDir)).toBe(false);
    }
  }, 60_000);

  it('fails when the stored key is not the pinned release key', () => {
    const workDir = join(root, 'work-wrong-key');
    const result = check(workDir, protectedPin, plainKey, '');
    expect(result.status).toBe(1);
    expect(result.stdout).toBe(`UPDATER_SECRET_CHECK=fail key=${protectedKeyId} reason=signed-with-other-key\n`);
    expectNoSecretOutput(result, plainKey);
    expect(existsSync(workDir)).toBe(false);
  }, 60_000);

  it('fails when the key secret is missing', () => {
    const workDir = join(root, 'work-missing');
    const result = check(workDir, plainPin, undefined, '');
    expect(result.status).toBe(1);
    expect(result.stdout).toBe(`UPDATER_SECRET_CHECK=fail key=${plainKeyId} reason=signing-key-secret-missing\n`);
    expect(existsSync(workDir)).toBe(false);
  }, 60_000);

  it('refuses a work folder that already exists and leaves it alone', () => {
    const workDir = join(root, 'work-existing');
    mkdirSync(workDir);
    writeFileSync(join(workDir, 'keep.txt'), 'keep');
    const result = check(workDir, plainPin, plainKey, '');
    expect(result.status).toBe(1);
    expect(result.stdout).toBe(`UPDATER_SECRET_CHECK=fail key=${plainKeyId} reason=work-dir-not-fresh\n`);
    expect(readFileSync(join(workDir, 'keep.txt'), 'utf8')).toBe('keep');
  }, 60_000);

  it('verifies against releaseSigningPublicKey from the repository pin file', () => {
    const pin = JSON.parse(readFileSync('scripts/updater-signing-key.json', 'utf8'));
    expect(typeof pin.releaseSigningPublicKey).toBe('string');
    expect(checkScript).toContain('parsePublicKey(pin.releaseSigningPublicKey)');
    expect(checkScript).toContain('verifyUpdaterSignature(readFileSync(file), signatureText, pin.releaseSigningPublicKey)');
    expect(checkScript).toContain("from './verify-updater-signature.mjs'");
  });
});
