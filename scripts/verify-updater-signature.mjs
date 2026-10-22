import { readFileSync } from 'node:fs';
import { createHash, createPublicKey, verify } from 'node:crypto';
import { resolve } from 'node:path';
import { pathToFileURL } from 'node:url';

const ED25519_SPKI_PREFIX = Buffer.from('302a300506032b6570032100', 'hex');

function boxLines(base64, kind) {
  const decoded = Buffer.from(String(base64).trim(), 'base64');
  if (decoded.length === 0) throw new Error(`Updater ${kind} is not base64.`);
  const lines = decoded.toString('utf8').split(/\r?\n/);
  if (!lines[0]?.startsWith('untrusted comment: ')) throw new Error(`Updater ${kind} is not a minisign ${kind}.`);
  return lines;
}

export function displayKeyId(keyIdBytes) {
  return Buffer.from(keyIdBytes).reverse().toString('hex').toUpperCase();
}

export function parsePublicKey(base64) {
  const raw = Buffer.from(boxLines(base64, 'public key')[1] ?? '', 'base64');
  if (raw.length !== 42 || raw.subarray(0, 2).toString('latin1') !== 'Ed') throw new Error('Updater public key has an unsupported minisign layout.');
  return {
    keyId: raw.subarray(2, 10),
    key: createPublicKey({ key: Buffer.concat([ED25519_SPKI_PREFIX, raw.subarray(10)]), format: 'der', type: 'spki' }),
  };
}

export function parseSignature(base64) {
  const lines = boxLines(base64, 'signature');
  const raw = Buffer.from(lines[1] ?? '', 'base64');
  const algorithm = raw.subarray(0, 2).toString('latin1');
  if (raw.length !== 74 || (algorithm !== 'ED' && algorithm !== 'Ed')) throw new Error('Updater signature has an unsupported minisign layout.');
  if (!lines[2]?.startsWith('trusted comment: ')) throw new Error('Updater signature has no trusted comment.');
  const global = Buffer.from(lines[3] ?? '', 'base64');
  if (global.length !== 64) throw new Error('Updater signature has no global signature.');
  return { prehashed: algorithm === 'ED', keyId: raw.subarray(2, 10), signature: raw.subarray(10), trustedComment: lines[2].slice('trusted comment: '.length), global };
}

export function verifyTrustedComment(signatureBase64, publicKeyBase64) {
  const signature = parseSignature(signatureBase64);
  const publicKey = parsePublicKey(publicKeyBase64);
  if (!signature.keyId.equals(publicKey.keyId)) throw new Error(`Updater signature key ${displayKeyId(signature.keyId)} is not the expected key ${displayKeyId(publicKey.keyId)}.`);
  if (!verify(null, Buffer.concat([signature.signature, Buffer.from(signature.trustedComment, 'utf8')]), publicKey.key, signature.global)) throw new Error('Updater signature trusted comment does not verify.');
  return { signature, publicKey };
}

export function verifyUpdaterSignature(fileBytes, signatureBase64, publicKeyBase64) {
  const { signature, publicKey } = verifyTrustedComment(signatureBase64, publicKeyBase64);
  const message = signature.prehashed ? createHash('blake2b512').update(fileBytes).digest() : fileBytes;
  if (!verify(null, message, publicKey.key, signature.signature)) throw new Error('Updater signature does not verify for this file.');
  return displayKeyId(publicKey.keyId);
}

if (process.argv[1] && import.meta.url === pathToFileURL(resolve(process.argv[1])).href) {
  const [file, signaturePath, pinPath] = process.argv.slice(2);
  if (!file || !signaturePath || !pinPath) throw new Error('Usage: node scripts/verify-updater-signature.mjs <installer> <installer.sig> <scripts/updater-signing-key.json>');
  const pin = JSON.parse(readFileSync(pinPath, 'utf8'));
  const keyId = verifyUpdaterSignature(readFileSync(file), readFileSync(signaturePath, 'utf8'), pin.releaseSigningPublicKey);
  console.log(`UPDATER_SIGNATURE_VERIFIED key=${keyId}`);
}
