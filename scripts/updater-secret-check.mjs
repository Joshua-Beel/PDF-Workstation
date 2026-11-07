import { spawnSync } from 'node:child_process';
import { randomBytes } from 'node:crypto';
import { existsSync, mkdirSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { createRequire } from 'node:module';
import { join, resolve } from 'node:path';
import { pathToFileURL } from 'node:url';
import { displayKeyId, parsePublicKey, parseSignature, verifyUpdaterSignature } from './verify-updater-signature.mjs';

const SIGNER_TIMEOUT_MS = 120_000;
const THROWAWAY_NAME = 'updater-secret-check.bin';

export function defaultSignerCommand() {
  const require = createRequire(import.meta.url);
  return [process.execPath, require.resolve('@tauri-apps/cli/tauri.js')];
}

export function runSecretCheck({ workDir, pinPath, env = process.env, signerCommand = defaultSignerCommand() }) {
  const pin = JSON.parse(readFileSync(pinPath, 'utf8'));
  const pinnedKeyId = displayKeyId(parsePublicKey(pin.releaseSigningPublicKey).keyId);
  if (!workDir || existsSync(workDir)) return { pass: false, keyId: pinnedKeyId, reason: 'work-dir-not-fresh' };
  mkdirSync(workDir);
  try {
    if (!env.TAURI_SIGNING_PRIVATE_KEY) return { pass: false, keyId: pinnedKeyId, reason: 'signing-key-secret-missing' };
    const file = join(workDir, THROWAWAY_NAME);
    writeFileSync(file, randomBytes(4096));
    const [command, ...prefix] = signerCommand;
    const signer = spawnSync(command, [...prefix, 'signer', 'sign', file], {
      cwd: workDir, env, stdio: 'ignore', timeout: SIGNER_TIMEOUT_MS, windowsHide: true,
    });
    if (signer.error || signer.status !== 0) return { pass: false, keyId: pinnedKeyId, reason: 'signer-failed' };
    const signaturePath = `${file}.sig`;
    if (!existsSync(signaturePath)) return { pass: false, keyId: pinnedKeyId, reason: 'signature-missing' };
    const signatureText = readFileSync(signaturePath, 'utf8');
    let signature;
    try { signature = parseSignature(signatureText); } catch { return { pass: false, keyId: pinnedKeyId, reason: 'signature-unreadable' }; }
    if (displayKeyId(signature.keyId) !== pinnedKeyId) return { pass: false, keyId: pinnedKeyId, reason: 'signed-with-other-key' };
    try {
      const verifiedKeyId = verifyUpdaterSignature(readFileSync(file), signatureText, pin.releaseSigningPublicKey);
      return { pass: verifiedKeyId === pinnedKeyId, keyId: pinnedKeyId, reason: verifiedKeyId === pinnedKeyId ? 'verified' : 'signature-invalid' };
    } catch {
      return { pass: false, keyId: pinnedKeyId, reason: 'signature-invalid' };
    }
  } finally {
    rmSync(workDir, { recursive: true, force: true });
  }
}

export function formatResult(result) {
  return `UPDATER_SECRET_CHECK=${result.pass ? 'pass' : 'fail'} key=${result.keyId} reason=${result.reason}`;
}

if (process.argv[1] && import.meta.url === pathToFileURL(resolve(process.argv[1])).href) {
  const [workDir, pinPath] = process.argv.slice(2);
  if (!workDir || !pinPath) {
    console.log('Usage: node scripts/updater-secret-check.mjs <fresh-work-dir> <scripts/updater-signing-key.json>');
    process.exit(2);
  }
  let result;
  try {
    result = runSecretCheck({ workDir: resolve(workDir), pinPath });
  } catch {
    console.log('UPDATER_SECRET_CHECK=fail reason=check-error');
    process.exit(1);
  }
  console.log(formatResult(result));
  process.exit(result.pass ? 0 : 1);
}
