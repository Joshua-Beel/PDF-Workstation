import { mkdirSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { randomUUID } from 'node:crypto';
import { join } from 'node:path';
import { spawnSync } from 'node:child_process';
import { describe, expect, it } from 'vitest';

const pin = JSON.parse(readFileSync('scripts/updater-signing-key.json', 'utf8'));
const tauri = JSON.parse(readFileSync('src-tauri/tauri.conf.json', 'utf8'));
const published020Signature = readFileSync('src-tauri/src/update_commands.rs', 'utf8').match(/PUBLISHED_0_2_0_SIGNATURE: &str = "([^"]+)"/)?.[1] ?? '';
const OLD_UPDATER_KEY = 'dW50cnVzdGVkIGNvbW1lbnQ6IG1pbmlzaWduIHB1YmxpYyBrZXk6IDg1OTcwQzM0QUVBQkQ3NzUKUldSMTE2dXVOQXlYaFdZN0k1Mk1CcSt2NS81bzNRTERRbU16ejYxTDNiS3V6b1RTZ0tuTlRjZVEK';

function keyId(base64: string, length: number) {
  const lines = Buffer.from(base64, 'base64').toString('utf8').split(/\r?\n/);
  const raw = Buffer.from(lines[1], 'base64');
  expect(raw.length).toBe(length);
  return raw.subarray(2, 10).toString('hex');
}

function runAssert(signature: string, pinValue: unknown) {
  const root = join('target', `updater-key-${randomUUID()}`);
  mkdirSync(join(root, 'scripts'), { recursive: true });
  try {
    writeFileSync(join(root, 'scripts', 'updater-signing-key.json'), JSON.stringify(pinValue), 'utf8');
    writeFileSync(join(root, 'installer.exe.sig'), signature, 'utf8');
    return spawnSync('pwsh.exe', ['-NoProfile', '-NonInteractive', '-File', 'scripts/assert-updater-signing-key.ps1', '-SignaturePath', join(root, 'installer.exe.sig'), '-SourceRoot', root], { encoding: 'utf8', timeout: 45_000 });
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
}

describe('updater signing key pin', () => {
  it('pins, for this release, the key that signed published 0.2.0 (the key every installed copy trusts)', () => {
    expect(Object.keys(pin)).toEqual(['releaseSigningPublicKey']);
    expect(pin.releaseSigningPublicKey).toBe(OLD_UPDATER_KEY);
    expect(keyId(published020Signature, 74)).toBe(keyId(pin.releaseSigningPublicKey, 42));
    expect(typeof tauri.plugins.updater.pubkey).toBe('string');
  });

  it('accepts a signature made with the pinned key and refuses any other key or malformed input', () => {
    const accepted = runAssert(published020Signature, pin);
    expect(accepted.status, accepted.stderr || accepted.stdout).toBe(0);
    expect(accepted.stdout).toContain('UPDATER_SIGNING_KEY_ID 75D7ABAE340C9785');

    const lines = Buffer.from(published020Signature, 'base64').toString('utf8').split('\n');
    const raw = Buffer.from(lines[1], 'base64');
    raw[5] ^= 0xff;
    lines[1] = raw.toString('base64');
    const otherKey = Buffer.from(lines.join('\n'), 'utf8').toString('base64');
    const refused = runAssert(otherKey, pin);
    expect(refused.status).not.toBe(0);
    expect(refused.stderr + refused.stdout).toContain('was not made with the key that installed apps trust');

    expect(runAssert('not base64 !', pin).status).not.toBe(0);
    expect(runAssert(published020Signature, { releaseSigningPublicKey: OLD_UPDATER_KEY, extra: 1 }).status).not.toBe(0);
    expect(runAssert(published020Signature, { releaseSigningPublicKey: published020Signature }).status).not.toBe(0);
  }, 120_000);

  it('runs the check in the release workflow and the draft verifier, which also requires the packaged app to embed the tagged key', () => {
    const workflow = readFileSync('.github/workflows/release.yml', 'utf8');
    expect(workflow).toContain('./scripts/assert-updater-signing-key.ps1 -SignaturePath (Join-Path $bundle "$installerName.sig") -SourceRoot (Get-Location).Path');
    expect(workflow).not.toMatch(/updater-signing-key\.json[^\n]*secrets\./);
    const verifier = readFileSync('scripts/verify-default-draft.ps1', 'utf8');
    expect(verifier).toContain("$null=& (Join-Path $PSScriptRoot 'assert-updater-signing-key.ps1') -SignaturePath $signaturePath -SourceRoot $TargetSourceRoot");
    expect(verifier).toContain("Test-ExecutableContainsAscii -Path $application.FullName -Marker $configuredUpdaterKey");
    expect(verifier).toContain('updaterSignatureKeyIsReleaseSigningKey=$true;packagedApplicationEmbedsTaggedUpdaterKey=$true;');
  });
});
