import { existsSync, mkdirSync, readFileSync, readdirSync, writeFileSync } from 'node:fs';
import { createHash } from 'node:crypto';
import { dirname, join, relative, resolve } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { checkedSourceArchiveEntries, offlineCargoMetadata } from './mpl-source-archives.mjs';

const hash = bytes => createHash('sha256').update(bytes).digest('hex');
const slash = path => path.replaceAll('\\', '/');
const normalizeLines = text => text.replaceAll('\r\n', '\n');

export function supplementalFiles(folder, packageKey) {
  const manifest = JSON.parse(readFileSync(join(folder, 'manifest.json'), 'utf8'));
  const entry = manifest.packages[packageKey];
  if (!entry) return { files: [] };
  const files = entry.files.map(file => {
    if (file.file.includes('/') || file.file.includes('\\') || file.file === '..') throw new Error('Supplement filename must stay in its directory.');
    const bytes = readFileSync(join(folder, file.file));
    if (hash(bytes) !== file.sha256) throw new Error(`Supplement hash mismatch: ${file.file}`);
    return { path: `upstream-supplement/${file.file}`, sha256: file.sha256, provenance: { url: file.url, crateCommit: entry.crateCommit, evidence: entry.evidence }, text: bytes.toString('utf8') };
  });
  return { crateCommit: entry.crateCommit, files };
}

export function ocrEngineRecords(root) {
  const pins = JSON.parse(readFileSync(join(root, 'scripts/ocr/pins.json'), 'utf8'));
  const folder = join(root, 'scripts/notice-supplements');
  const components = [
    { name: 'tesseract', version: pins.tesseract.version, license: 'Apache-2.0', source: pins.tesseract.archive.url, sourceCommit: pins.tesseract.commit, file: `tesseract-${pins.tesseract.version}-LICENSE`, upstream: `${pins.tesseract.license.source} in ${pins.tesseract.archive.url}`, pin: pins.tesseract.license },
    { name: 'leptonica', version: pins.leptonica.version, license: 'BSD-2-Clause', source: pins.leptonica.archive.url, sourceCommit: pins.leptonica.commit, file: `leptonica-${pins.leptonica.version}-${pins.leptonica.license.source}`, upstream: `${pins.leptonica.license.source} in ${pins.leptonica.archive.url}`, pin: pins.leptonica.license },
    { name: `tessdata_fast-${pins.model.language}`, version: pins.model.version, license: 'Apache-2.0', source: pins.model.data.url, sourceCommit: pins.model.commit, file: `tessdata_fast-${pins.model.version}-LICENSE`, upstream: pins.model.license.url, pin: pins.model.license },
  ];
  return components.map(component => {
    const bytes = readFileSync(join(folder, component.file));
    if (bytes.length !== component.pin.bytes || hash(bytes) !== component.pin.sha256.toLowerCase()) throw new Error(`OCR license text does not match scripts/ocr/pins.json: ${component.file}`);
    return { ecosystem: 'ocr-engine', name: component.name, version: component.version, license: component.license, source: component.source, scope: 'ocr-engine-resource', files: [{ path: `upstream-supplement/${component.file}`, sha256: hash(bytes), provenance: { url: component.upstream, sourceCommit: component.sourceCommit }, text: bytes.toString('utf8') }] };
  });
}

export function noticeFiles(root) {
  const found = [];
  function visit(folder) {
    for (const entry of readdirSync(folder, { withFileTypes: true }).sort((a, b) => a.name.localeCompare(b.name, 'en'))) {
      const path = join(folder, entry.name);
      if (entry.isSymbolicLink()) continue;
      if (entry.isDirectory()) {
        if (!['.git', 'node_modules', 'target'].includes(entry.name)) visit(path);
      } else if (/^(licen[cs]e|notice|copying|copyright|authors)([._-].*)?$/i.test(entry.name) || /(^|[\\/])licen[cs]es[\\/]/i.test(relative(root, path))) {
        const bytes = readFileSync(path);
        if (bytes.includes(0)) throw new Error(`Binary notice input: ${path}`);
        found.push({ path: slash(relative(root, path)), sha256: hash(bytes), text: bytes.toString('utf8') });
      }
    }
  }
  visit(root);
  return found.sort((a, b) => a.path.localeCompare(b.path, 'en'));
}

export function runtimeCandidates(metadata) {
  const nodes = new Map(metadata.resolve.nodes.map(node => [node.id, node]));
  const packages = new Map(metadata.packages.map(pkg => [pkg.id, pkg]));
  const found = new Set(), queue = [metadata.resolve.root];
  while (queue.length) {
    const id = queue.shift();
    if (found.has(id)) continue;
    found.add(id);
    for (const dep of nodes.get(id)?.deps ?? []) {
      if (dep.dep_kinds.some(kind => kind.kind === null) && !packages.get(dep.pkg).targets.some(target => target.kind.includes('proc-macro'))) queue.push(dep.pkg);
    }
  }
  found.delete(metadata.resolve.root);
  return found;
}

export function collectInventory(root) {
  const metadata = offlineCargoMetadata(root);
  const sourceArchives = checkedSourceArchiveEntries(root, metadata);
  const sourceArchiveByPackage = new Map(sourceArchives.map(archive => [`${archive.name}@${archive.version}`, archive]));
  const candidates = runtimeCandidates(metadata);
  const resolved = new Set(metadata.resolve.nodes.map(node => node.id));
  const records = [];
  for (const pkg of metadata.packages.filter(pkg => resolved.has(pkg.id) && pkg.id !== metadata.resolve.root)) {
    const files = noticeFiles(dirname(pkg.manifest_path));
    const supplement = supplementalFiles(join(root, 'scripts/notice-supplements'), `cargo:${pkg.name}@${pkg.version}`);
    if (supplement.files.length) {
      const vcs = JSON.parse(readFileSync(join(dirname(pkg.manifest_path), '.cargo_vcs_info.json'), 'utf8'));
      if (vcs.git.sha1 !== supplement.crateCommit) throw new Error(`Supplement source commit does not match published crate: ${pkg.name}@${pkg.version}`);
      files.push(...supplement.files);
    }
    if (pkg.license_file) {
      const licensePath = resolve(dirname(pkg.manifest_path), pkg.license_file);
      const relativeLicense = slash(relative(dirname(pkg.manifest_path), licensePath));
      if (relativeLicense.startsWith('../')) throw new Error(`License file points outside the cached package: ${pkg.name}`);
      if (!files.some(file => file.path === relativeLicense)) {
        const bytes = readFileSync(licensePath);
        if (bytes.includes(0)) throw new Error(`Binary license file: ${pkg.name}`);
        files.push({ path: relativeLicense, sha256: hash(bytes), text: bytes.toString('utf8') });
      }
    }
    const sourceArchive = sourceArchiveByPackage.get(`${pkg.name}@${pkg.version}`);
    records.push({ ecosystem: 'cargo', name: pkg.name, version: pkg.version, license: pkg.license ?? null, source: pkg.source, scope: candidates.has(pkg.id) ? 'runtime-candidate' : 'build-or-auxiliary', ...(sourceArchive ? { sourceArchive } : {}), files });
  }
  const lock = JSON.parse(readFileSync(join(root, 'package-lock.json'), 'utf8'));
  for (const [path, entry] of Object.entries(lock.packages)) {
    if (!path || entry.dev) continue;
    const folder = join(root, path);
    const pkg = JSON.parse(readFileSync(join(folder, 'package.json'), 'utf8'));
    if (pkg.version !== entry.version) throw new Error(`Installed npm version differs from lockfile: ${path}`);
    const files = noticeFiles(folder);
    records.push({ ecosystem: 'npm', name: pkg.name, version: pkg.version, license: entry.license ?? pkg.license ?? null, source: entry.resolved ?? null, scope: 'production-lock-entry', files });
  }
  records.push(...ocrEngineRecords(root));
  records.sort((a, b) => `${a.ecosystem}/${a.name}/${a.version}`.localeCompare(`${b.ecosystem}/${b.name}/${b.version}`, 'en'));
  const issues = records.flatMap(record => {
    const packageName = `${record.ecosystem}:${record.name}@${record.version}`;
    const issues = [];
    if (!record.license) issues.push({ package: packageName, issue: 'missing-license-expression' });
    if (!record.files.length) issues.push({ package: packageName, issue: 'no-local-notice-file-found' });
    if (record.license?.includes('MPL-2.0') && !record.sourceArchive) issues.push({ package: packageName, issue: 'source-availability-review-open', scope: record.scope });
    return issues;
  });
  const inventory = {
    schema: 1,
    target: 'x86_64-pc-windows-msvc',
    coverage: 'Conservative resolved Cargo Windows graph, including build/auxiliary packages, plus non-development npm lock entries. Runtime-candidate is not proof of linked code. PDFium notices are packaged separately. Five exact MPL-2.0 crate source archives are packaged with verified locations and hashes. PDF Workstation itself is MIT-licensed; this collection records third-party notices and is not a legal licensing review. The ocr-engine records cover the Tesseract engine, Leptonica and the English model packaged under resources/ocr/ by an installer built with an OCR setup root; their license texts are checked against scripts/ocr/pins.json.',
    inputHashes: Object.fromEntries(['src-tauri/Cargo.lock', 'src-tauri/Cargo.toml', 'package-lock.json', 'package.json', 'scripts/notice-supplements/manifest.json', 'scripts/mpl-source-archives.manifest.json', 'scripts/ocr/pins.json'].map(path => [path, hash(normalizeLines(readFileSync(join(root, path), 'utf8')))])),
    issues,
    packages: records.map(({ files, ...record }) => ({ ...record, notices: files.map(({ text, ...file }) => file) })),
  };
  const text = ['THIRD-PARTY DEPENDENCY NOTICES', '', inventory.coverage, '', 'Unresolved collection/review items:', ...(issues.length ? issues.map(issue => `- ${issue.package}: ${issue.issue}`) : ['- None recorded by this notice collection.']), '', ...records.flatMap(record => [
    '='.repeat(78), `${record.ecosystem}: ${record.name} ${record.version}`, `Declared license: ${record.license ?? 'UNKNOWN'}`, `Scope: ${record.scope}`, `Source: ${record.source ?? 'UNKNOWN'}`, '',
    ...(record.sourceArchive ? [`Packaged source archive: ${record.sourceArchive.installedResourcePath} (relative to the application's resource directory)`, `Packaged source repository path: ${record.sourceArchive.repositoryPath}`, `Official source archive: ${record.sourceArchive.url}`, `Source archive SHA-256: ${record.sourceArchive.sha256}`, ''] : []),
    ...(record.files.length ? record.files.flatMap(file => [`--- ${file.path} (SHA-256 ${file.sha256}) ---`, ...(file.provenance?.sourceCommit ? [`Upstream: ${file.provenance.url}`, `Upstream source commit: ${file.provenance.sourceCommit}`] : file.provenance ? [`Upstream: ${file.provenance.url}`, `Published crate commit: ${file.provenance.crateCommit}`, `Source evidence: ${file.provenance.evidence}`] : []), file.text, '']) : ['No standalone license/notice file was found in the installed package.', '']),
  ])].join('\n');
  return { inventory, text };
}

export function generate(root, check = false) {
  const { inventory, text } = collectInventory(root);
  const folder = join(root, 'src-tauri/resources/third-party-licenses');
  const outputs = { 'inventory.json': JSON.stringify(inventory, null, 2) + '\n', 'THIRD-PARTY-NOTICES.txt': text + '\n' };
  if (!check) mkdirSync(folder, { recursive: true });
  for (const [name, content] of Object.entries(outputs)) {
    const path = join(folder, name);
    if (check) {
      if (!existsSync(path) || normalizeLines(readFileSync(path, 'utf8')) !== normalizeLines(content)) throw new Error(`Dependency notices are missing or stale: ${name}. Run node scripts/dependency-notices.mjs.`);
    } else writeFileSync(path, content, 'utf8');
  }
  console.log(`${check ? 'Verified' : 'Generated'} notices for ${inventory.packages.length} packages; ${inventory.issues.length} review items remain.`);
  return inventory;
}

if (process.argv[1] && import.meta.url === pathToFileURL(resolve(process.argv[1])).href) {
  const root = resolve(dirname(fileURLToPath(import.meta.url)), '..');
  generate(root, process.argv.includes('--check'));
}
