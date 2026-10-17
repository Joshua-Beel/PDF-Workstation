import { readFileSync, readdirSync, writeFileSync } from 'node:fs';
import { resolve, basename } from 'node:path';
import { pathToFileURL } from 'node:url';

export function createManifest(version, installer, signature, notes, date = new Date().toISOString()) {
  if (!/^\d+\.\d+\.\d+$/.test(version)) throw new Error('Use a stable major.minor.patch version.');
  if (!installer.endsWith('_x64-setup.exe')) throw new Error('Expected a Windows x64 NSIS installer.');
  if (!signature.trim()) throw new Error('Installer signature is missing.');
  const assetName = basename(installer).replace(/ /g, '.');
  return { version, notes, pub_date: date, platforms: { 'windows-x86_64': { signature: signature.trim(), url: `https://github.com/Joshua-Beel/PDF-Workstation/releases/download/v${version}/${encodeURIComponent(assetName)}` } } };
}

if (process.argv[1] && import.meta.url === pathToFileURL(resolve(process.argv[1])).href) {
  const config = JSON.parse(readFileSync('src-tauri/tauri.conf.json', 'utf8'));
  const pkg = JSON.parse(readFileSync('package.json', 'utf8'));
  const rust = readFileSync('src-tauri/Cargo.toml', 'utf8').match(/^version = "([^"]+)"/m)?.[1];
  if (pkg.version !== config.version || rust !== config.version) throw new Error('Version numbers must agree in package.json, Cargo.toml and tauri.conf.json.');
  const folder = 'src-tauri/target/release/bundle/nsis';
  const files = readdirSync(folder).filter(name => name.endsWith(`_${config.version}_x64-setup.exe`));
  if (files.length !== 1) throw new Error('Expected exactly one installer for this version.');
  const manifest = createManifest(config.version, files[0], readFileSync(`${folder}/${files[0]}.sig`, 'utf8'), readFileSync('docs/release-notes.md', 'utf8'));
  writeFileSync(`${folder}/latest.json`, JSON.stringify(manifest, null, 2) + '\n', 'utf8');
  console.log(`Created update manifest for ${config.version}.`);
}
