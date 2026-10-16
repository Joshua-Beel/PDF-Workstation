import { copyFileSync, mkdirSync, mkdtempSync, readFileSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { createHash } from 'node:crypto';
import { expect, it } from 'vitest';
import { noticeFiles, ocrEngineRecords, runtimeCandidates, supplementalFiles } from './dependency-notices.mjs';

it('records the OCR engine, Leptonica and English model with license texts bound to the OCR pins', () => {
  const pins = JSON.parse(readFileSync('scripts/ocr/pins.json', 'utf8'));
  const records = ocrEngineRecords('.');
  expect(records.map(record => `${record.name}@${record.version}:${record.license}`)).toEqual([`tesseract@${pins.tesseract.version}:Apache-2.0`, `leptonica@${pins.leptonica.version}:BSD-2-Clause`, `tessdata_fast-eng@${pins.model.version}:Apache-2.0`]);
  expect(records.map(record => record.files[0].sha256.toUpperCase())).toEqual([pins.tesseract.license.sha256, pins.leptonica.license.sha256, pins.model.license.sha256]);
  expect(records.every(record => record.ecosystem === 'ocr-engine' && record.scope === 'ocr-engine-resource')).toBe(true);
  const inventory = JSON.parse(readFileSync('src-tauri/resources/third-party-licenses/inventory.json', 'utf8'));
  const notices = readFileSync('src-tauri/resources/third-party-licenses/THIRD-PARTY-NOTICES.txt', 'utf8').replace(/\r\n/g, '\n');
  expect(inventory.packages.filter(pkg => pkg.ecosystem === 'ocr-engine').map(pkg => pkg.name).sort()).toEqual(['leptonica', 'tessdata_fast-eng', 'tesseract']);
  expect(inventory.inputHashes['scripts/ocr/pins.json']).toMatch(/^[0-9a-f]{64}$/);
  expect(notices).toContain(`ocr-engine: tesseract ${pins.tesseract.version}\nDeclared license: Apache-2.0`);
  expect(notices).toContain(`ocr-engine: leptonica ${pins.leptonica.version}\nDeclared license: BSD-2-Clause`);
  expect(notices).toContain(`ocr-engine: tessdata_fast-eng ${pins.model.version}\nDeclared license: Apache-2.0`);
  expect(notices).toContain(`Upstream source commit: ${pins.tesseract.commit}`);
});

it('refuses an OCR license text that differs from the OCR pins', () => {
  const root = mkdtempSync(join(tmpdir(), 'pdf-ocr-notices-'));
  mkdirSync(join(root, 'scripts', 'ocr'), { recursive: true });
  mkdirSync(join(root, 'scripts', 'notice-supplements'));
  copyFileSync('scripts/ocr/pins.json', join(root, 'scripts', 'ocr', 'pins.json'));
  const pins = JSON.parse(readFileSync('scripts/ocr/pins.json', 'utf8'));
  for (const file of [`tesseract-${pins.tesseract.version}-LICENSE`, `leptonica-${pins.leptonica.version}-${pins.leptonica.license.source}`, `tessdata_fast-${pins.model.version}-LICENSE`]) {
    copyFileSync(join('scripts', 'notice-supplements', file), join(root, 'scripts', 'notice-supplements', file));
  }
  expect(ocrEngineRecords(root)).toHaveLength(3);
  const leptonica = join(root, 'scripts', 'notice-supplements', `leptonica-${pins.leptonica.version}-${pins.leptonica.license.source}`);
  writeFileSync(leptonica, readFileSync(leptonica, 'utf8').replace(/\n/g, '\r\n'), 'utf8');
  expect(() => ocrEngineRecords(root)).toThrow('does not match scripts/ocr/pins.json');
});

it('applies supplements only to exact versions and rejects changed source bytes', () => {
  const folder = mkdtempSync(join(tmpdir(), 'pdf-notice-supplement-'));
  const text = 'An exact upstream notice\n';
  writeFileSync(join(folder, 'LICENSE'), text, 'utf8');
  writeFileSync(join(folder, 'manifest.json'), JSON.stringify({ packages: { 'cargo:example@1.2.3': { crateCommit: 'a'.repeat(40), evidence: 'https://example.test/source', files: [{ file: 'LICENSE', url: 'https://example.test/license', sha256: createHash('sha256').update(text).digest('hex') }] } } }), 'utf8');
  expect(supplementalFiles(folder, 'cargo:example@1.2.4').files).toEqual([]);
  const result = supplementalFiles(folder, 'cargo:example@1.2.3');
  expect(result.files[0].text).toBe(text);
  expect(result.files[0].provenance.crateCommit).toBe('a'.repeat(40));
  writeFileSync(join(folder, 'LICENSE'), 'Changed', 'utf8');
  expect(() => supplementalFiles(folder, 'cargo:example@1.2.3')).toThrow('hash mismatch');
});

it('collects nested licenses and arbitrary license-directory names without copying ordinary source', () => {
  const folder = mkdtempSync(join(tmpdir(), 'pdf-notices-'));
  mkdirSync(join(folder, 'src', 'subcomponent'), { recursive: true });
  mkdirSync(join(folder, 'licenses'));
  writeFileSync(join(folder, 'LICENSE'), 'Root license\n', 'utf8');
  writeFileSync(join(folder, 'src', 'subcomponent', 'LICENSE-MIT'), 'Nested license\n', 'utf8');
  writeFileSync(join(folder, 'licenses', 'component.txt'), 'Other component\n', 'utf8');
  writeFileSync(join(folder, 'src', 'main.rs'), 'source excluded', 'utf8');
  const files = noticeFiles(folder);
  expect(files.map(file => file.path)).toEqual(['LICENSE', 'licenses/component.txt', 'src/subcomponent/LICENSE-MIT']);
  expect(files.every(file => /^[0-9a-f]{64}$/.test(file.sha256))).toBe(true);
  expect(files[2].text).toBe('Nested license\n');
});

it('keeps build edges and procedural macro descendants outside runtime candidates', () => {
  const pkg = (id: string, kind = 'lib') => ({ id, targets: [{ kind: [kind] }] });
  const dep = (pkg: string, kind: string | null = null) => ({ pkg, dep_kinds: [{ kind }] });
  const metadata = { packages: [pkg('root'), pkg('runtime'), pkg('build'), pkg('macro', 'proc-macro'), pkg('macro-helper')], resolve: { root: 'root', nodes: [
    { id: 'root', deps: [dep('runtime'), dep('build', 'build'), dep('macro')] },
    { id: 'runtime', deps: [] }, { id: 'build', deps: [] },
    { id: 'macro', deps: [dep('macro-helper')] }, { id: 'macro-helper', deps: [] },
  ] } };
  expect([...runtimeCandidates(metadata)]).toEqual(['runtime']);
});

it('bundles notices and exact MPL source archives without changing included license text', () => {
  const config = JSON.parse(readFileSync('src-tauri/tauri.conf.json', 'utf8'));
  expect(config.bundle.resources).toContain('resources/third-party-licenses/inventory.json');
  expect(config.bundle.resources).toContain('resources/third-party-licenses/THIRD-PARTY-NOTICES.txt');
  expect(config.bundle.resources).toContain('resources/third-party-sources/*');
  const inventory = JSON.parse(readFileSync('src-tauri/resources/third-party-licenses/inventory.json', 'utf8'));
  const notices = readFileSync('src-tauri/resources/third-party-licenses/THIRD-PARTY-NOTICES.txt', 'utf8');
  const sourceManifest = JSON.parse(readFileSync('src-tauri/resources/third-party-sources/MANIFEST.json', 'utf8'));
  expect(inventory.packages.find(pkg => pkg.name === 'lucide-react').notices.length).toBeGreaterThan(0);
  expect(notices).toContain('Cole Bemis');
  expect(notices).toContain('src/polyfill/once_cell/LICENSE-APACHE');
  expect(sourceManifest.archives).toHaveLength(5);
  const cssparser = inventory.packages.find(pkg => pkg.ecosystem === 'cargo' && pkg.name === 'cssparser' && pkg.version === '0.36.0');
  expect(cssparser.sourceArchive.installedResourcePath).toBe('resources/third-party-sources/cssparser-0.36.0.crate');
  expect(notices).toContain("Packaged source archive: resources/third-party-sources/cssparser-0.36.0.crate (relative to the application's resource directory)");
});
