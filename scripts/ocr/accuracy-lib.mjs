import { createHash } from 'node:crypto';
import { spawn } from 'node:child_process';
import { lstat, readFile, stat } from 'node:fs/promises';
import { dirname, extname, isAbsolute, join, resolve, sep } from 'node:path';
import { inflateSync } from 'node:zlib';

export const OCR_ARGS = ['stdin', 'stdout', '--tessdata-dir', '<tessdata>', '-l', 'eng', '--oem', '1', '--psm', '6', '--dpi', '150', '--loglevel', 'ERROR'];
export const OCR_DPI = 150;
export const OCR_INPUT_LIMIT = 16 * 1024 * 1024;
export const OCR_MAX_EDGE = 16384;
export const OCR_TIMEOUT_MS = 30000;
export const OCR_STDOUT_LIMIT = 1024 * 1024;
export const SETUP_ROOT_ENV = 'PDF_WORKSTATION_OCR_SETUP_ROOT';
export const MODEL_BYTES = 4113088;
export const MODEL_SHA256 = '7D4322BD2A7749724879683FC3912CB542F19906C83BCC1A52132556427170B2';
export const MAX_ENGINE_BYTES = 64 * 1024 * 1024;
export const PINS = {
  tesseractVersion: '5.5.3',
  tesseractCommit: 'db0ec62f81b0737fbbe184d8fea40af5738f8eef',
  leptonicaVersion: '1.87.0',
  leptonicaCommit: '13275a278eb55b5746e33f95fbf5a2c8f604b3ab',
  modelVersion: '4.1.0',
  modelCommit: '65727574dfcd264acbb0c3e07860e4e9e9b22185',
};
export const RECIPE_INPUTS = ['scripts/setup-ocr.ps1', 'scripts/ocr/pins.json', 'scripts/ocr/tesseract-options.cmake', 'scripts/ocr/generate-smoke.ps1'];
export const LICENSES = ['engine/licenses/Tesseract-Apache-2.0.txt', 'engine/licenses/Leptonica-BSD-2-Clause.txt', 'engine/licenses/eng-fast-Apache-2.0.txt'];
export const MAX_EDIT_CELLS = 400_000_000;

const QUOTES = [[/[‘’‚‛′]/g, "'"], [/[“”„‟″]/g, '"'], [/[‐-―−]/g, '-']];

export function normalizeText(text) {
  let out = String(text).normalize('NFKC').replace(/^﻿/, '');
  for (const [pattern, replacement] of QUOTES) out = out.replace(pattern, replacement);
  return out.replace(/\s+/gu, ' ').trim();
}

export function levenshtein(a, b) {
  const n = a.length;
  const m = b.length;
  if (n * m > MAX_EDIT_CELLS) throw new Error(`Edit distance of ${n} x ${m} units is over the ${MAX_EDIT_CELLS} cell limit.`);
  if (n === 0) return m;
  if (m === 0) return n;
  let previous = new Array(m + 1);
  let current = new Array(m + 1);
  for (let j = 0; j <= m; j++) previous[j] = j;
  for (let i = 1; i <= n; i++) {
    current[0] = i;
    for (let j = 1; j <= m; j++) {
      const cost = a[i - 1] === b[j - 1] ? 0 : 1;
      current[j] = Math.min(previous[j] + 1, current[j - 1] + 1, previous[j - 1] + cost);
    }
    [previous, current] = [current, previous];
  }
  return previous[m];
}

export function scoreText(reference, hypothesis) {
  const ref = normalizeText(reference);
  const hyp = normalizeText(hypothesis);
  const refChars = Array.from(ref);
  const hypChars = Array.from(hyp);
  const refWords = ref === '' ? [] : ref.split(' ');
  const hypWords = hyp === '' ? [] : hyp.split(' ');
  if (refChars.length === 0) throw new Error('Ground truth is empty after normalisation.');
  const charEdits = levenshtein(refChars, hypChars);
  const wordEdits = levenshtein(refWords, hypWords);
  return {
    refChars: refChars.length,
    refWords: refWords.length,
    charEdits,
    wordEdits,
    cer: charEdits / refChars.length,
    wer: wordEdits / refWords.length,
  };
}

export function aggregate(scores) {
  const total = scores.reduce((sum, item) => ({
    refChars: sum.refChars + item.refChars,
    refWords: sum.refWords + item.refWords,
    charEdits: sum.charEdits + item.charEdits,
    wordEdits: sum.wordEdits + item.wordEdits,
  }), { refChars: 0, refWords: 0, charEdits: 0, wordEdits: 0 });
  return {
    ...total,
    cer: total.refChars ? total.charEdits / total.refChars : null,
    wer: total.refWords ? total.wordEdits / total.refWords : null,
  };
}

const IMAGE_EXTENSIONS = new Set(['.png', '.pnm', '.ppm', '.pgm']);

export function parseManifest(text) {
  let data;
  try { data = JSON.parse(text); } catch { throw new Error('Manifest is not valid JSON.'); }
  if (!data || typeof data !== 'object' || data.schemaVersion !== 1) throw new Error('Manifest schemaVersion must be 1.');
  if (!Array.isArray(data.entries) || data.entries.length === 0) throw new Error('Manifest needs a non-empty entries array.');
  const seen = new Set();
  const entries = data.entries.map((entry, index) => {
    const where = `entries[${index}]`;
    if (!entry || typeof entry !== 'object') throw new Error(`${where} must be an object.`);
    const { id, image, truth, sourceDpi, note } = entry;
    if (typeof id !== 'string' || !/^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$/.test(id)) throw new Error(`${where}.id must be 1-64 letters, digits, dot, dash or underscore.`);
    if (seen.has(id)) throw new Error(`Duplicate entry id ${id}.`);
    seen.add(id);
    for (const [name, value] of [['image', image], ['truth', truth]]) {
      if (typeof value !== 'string' || value === '') throw new Error(`${where}.${name} must be a relative path.`);
      assertRelativeInside(value, `${where}.${name}`);
    }
    if (!IMAGE_EXTENSIONS.has(extname(image).toLowerCase())) throw new Error(`${where}.image must be .png, .pnm, .ppm or .pgm (export PDF pages as images first).`);
    if (sourceDpi !== undefined && (!Number.isFinite(sourceDpi) || sourceDpi < OCR_DPI || sourceDpi > 1200)) throw new Error(`${where}.sourceDpi must be between ${OCR_DPI} and 1200.`);
    if (note !== undefined && typeof note !== 'string') throw new Error(`${where}.note must be a string.`);
    return { id, image, truth, sourceDpi: sourceDpi ?? null, note: note ?? null };
  });
  return { scope: typeof data.scope === 'string' ? data.scope : null, entries };
}

function assertRelativeInside(path, where) {
  if (isAbsolute(path) || /^[A-Za-z]:/.test(path) || path.startsWith('\\\\')) throw new Error(`${where} must be relative to the manifest folder.`);
  if (path.split(/[\\/]/).includes('..')) throw new Error(`${where} must not contain "..".`);
}

export function resolveInside(base, relative) {
  const root = resolve(base);
  const full = resolve(root, relative);
  if (full !== root && !full.startsWith(root + sep)) throw new Error(`Path escapes the manifest folder: ${relative}`);
  return full;
}

export function decodePng(buffer) {
  if (buffer.length < 33 || !buffer.subarray(0, 8).equals(Buffer.from([137, 80, 78, 71, 13, 10, 26, 10]))) throw new Error('Not a PNG file.');
  let at = 8;
  let header = null;
  let palette = null;
  const data = [];
  while (at + 12 <= buffer.length) {
    const length = buffer.readUInt32BE(at);
    const type = buffer.toString('ascii', at + 4, at + 8);
    const body = buffer.subarray(at + 8, at + 8 + length);
    if (body.length !== length) throw new Error('Truncated PNG chunk.');
    at += 12 + length;
    if (type === 'IHDR') header = body;
    else if (type === 'PLTE') palette = body;
    else if (type === 'IDAT') data.push(body);
    else if (type === 'IEND') break;
  }
  if (!header || header.length !== 13 || data.length === 0) throw new Error('PNG is missing IHDR or IDAT.');
  const width = header.readUInt32BE(0);
  const height = header.readUInt32BE(4);
  const depth = header[8];
  const colorType = header[9];
  if (header[12] !== 0) throw new Error('Interlaced PNG is not supported; re-save without interlacing.');
  if (width === 0 || height === 0 || width > OCR_MAX_EDGE || height > OCR_MAX_EDGE) throw new Error('PNG dimensions are outside the OCR limits.');
  const channels = { 0: 1, 2: 3, 3: 1, 4: 2, 6: 4 }[colorType];
  const validDepths = { 0: [1, 2, 4, 8, 16], 2: [8, 16], 3: [1, 2, 4, 8], 4: [8, 16], 6: [8, 16] }[colorType];
  if (!channels || !validDepths.includes(depth)) throw new Error('Unsupported PNG color type or bit depth.');
  if (colorType === 3 && !palette) throw new Error('Palette PNG is missing PLTE.');
  const bitsPerPixel = channels * depth;
  const stride = Math.ceil((width * bitsPerPixel) / 8);
  const bytesPerPixel = Math.max(1, bitsPerPixel >> 3);
  if (stride * height > 256 * 1024 * 1024) throw new Error('PNG is too large to decode.');
  const raw = inflateSync(Buffer.concat(data));
  if (raw.length !== (stride + 1) * height) throw new Error('PNG pixel data has the wrong length.');
  const image = Buffer.alloc(stride * height);
  for (let y = 0; y < height; y++) {
    const filter = raw[y * (stride + 1)];
    const source = y * (stride + 1) + 1;
    const target = y * stride;
    for (let x = 0; x < stride; x++) {
      const left = x >= bytesPerPixel ? image[target + x - bytesPerPixel] : 0;
      const up = y > 0 ? image[target - stride + x] : 0;
      const upLeft = y > 0 && x >= bytesPerPixel ? image[target - stride + x - bytesPerPixel] : 0;
      let predictor = 0;
      if (filter === 1) predictor = left;
      else if (filter === 2) predictor = up;
      else if (filter === 3) predictor = (left + up) >> 1;
      else if (filter === 4) {
        const p = left + up - upLeft;
        const pa = Math.abs(p - left), pb = Math.abs(p - up), pc = Math.abs(p - upLeft);
        predictor = pa <= pb && pa <= pc ? left : pb <= pc ? up : upLeft;
      } else if (filter !== 0) throw new Error('Unknown PNG filter.');
      image[target + x] = (raw[source + x] + predictor) & 255;
    }
  }
  const rgb = Buffer.alloc(width * height * 3);
  const sampleAt = (row, index) => {
    if (depth === 8) return image[row + index];
    if (depth === 16) return image[row + index * 2];
    const bit = index * depth;
    const value = (image[row + (bit >> 3)] >> (8 - depth - (bit & 7))) & ((1 << depth) - 1);
    return colorType === 3 ? value : Math.round((value * 255) / ((1 << depth) - 1));
  };
  for (let y = 0; y < height; y++) {
    const row = y * stride;
    for (let x = 0; x < width; x++) {
      let r, g, b, a = 255;
      if (colorType === 0) r = g = b = sampleAt(row, x);
      else if (colorType === 2) { r = sampleAt(row, x * 3); g = sampleAt(row, x * 3 + 1); b = sampleAt(row, x * 3 + 2); }
      else if (colorType === 3) { const index = sampleAt(row, x) * 3; if (index + 2 >= palette.length) throw new Error('PNG palette index is out of range.'); r = palette[index]; g = palette[index + 1]; b = palette[index + 2]; }
      else if (colorType === 4) { r = g = b = sampleAt(row, x * 2); a = sampleAt(row, x * 2 + 1); }
      else { r = sampleAt(row, x * 4); g = sampleAt(row, x * 4 + 1); b = sampleAt(row, x * 4 + 2); a = sampleAt(row, x * 4 + 3); }
      const out = (y * width + x) * 3;
      rgb[out] = Math.round((r * a + 255 * (255 - a)) / 255);
      rgb[out + 1] = Math.round((g * a + 255 * (255 - a)) / 255);
      rgb[out + 2] = Math.round((b * a + 255 * (255 - a)) / 255);
    }
  }
  return { width, height, rgb };
}

export function decodePnm(buffer) {
  let at = 0;
  const token = () => {
    while (at < buffer.length) {
      if (buffer[at] === 0x23) { while (at < buffer.length && buffer[at] !== 0x0a) at++; }
      else if (buffer[at] === 0x20 || buffer[at] === 0x0a || buffer[at] === 0x0d || buffer[at] === 0x09) at++;
      else break;
    }
    const start = at;
    while (at < buffer.length && ![0x20, 0x0a, 0x0d, 0x09].includes(buffer[at])) at++;
    return buffer.toString('ascii', start, at);
  };
  const magic = token();
  if (magic !== 'P6' && magic !== 'P5') throw new Error('Only binary P5 and P6 PNM files are supported.');
  const width = Number(token());
  const height = Number(token());
  const max = Number(token());
  at += 1;
  if (!Number.isInteger(width) || !Number.isInteger(height) || width < 1 || height < 1 || width > OCR_MAX_EDGE || height > OCR_MAX_EDGE || max !== 255) throw new Error('PNM header is not 8-bit or is out of range.');
  const samples = magic === 'P6' ? 3 : 1;
  const pixels = buffer.subarray(at);
  if (pixels.length !== width * height * samples) throw new Error('PNM pixel data has the wrong length.');
  if (samples === 3) return { width, height, rgb: Buffer.from(pixels) };
  const rgb = Buffer.alloc(width * height * 3);
  for (let i = 0; i < width * height; i++) rgb[i * 3] = rgb[i * 3 + 1] = rgb[i * 3 + 2] = pixels[i];
  return { width, height, rgb };
}

export function decodeImage(path, buffer) {
  return extname(path).toLowerCase() === '.png' ? decodePng(buffer) : decodePnm(buffer);
}

export function resampleToDpi(image, sourceDpi) {
  if (!sourceDpi || sourceDpi <= OCR_DPI) return image;
  const scale = OCR_DPI / sourceDpi;
  const width = Math.max(1, Math.round(image.width * scale));
  const height = Math.max(1, Math.round(image.height * scale));
  const rgb = Buffer.alloc(width * height * 3);
  for (let y = 0; y < height; y++) {
    const y0 = (y * image.height) / height, y1 = ((y + 1) * image.height) / height;
    for (let x = 0; x < width; x++) {
      const x0 = (x * image.width) / width, x1 = ((x + 1) * image.width) / width;
      let r = 0, g = 0, b = 0, weight = 0;
      for (let sy = Math.floor(y0); sy < Math.ceil(y1); sy++) {
        const wy = Math.min(sy + 1, y1) - Math.max(sy, y0);
        for (let sx = Math.floor(x0); sx < Math.ceil(x1); sx++) {
          const w = wy * (Math.min(sx + 1, x1) - Math.max(sx, x0));
          const at = (Math.min(sy, image.height - 1) * image.width + Math.min(sx, image.width - 1)) * 3;
          r += image.rgb[at] * w; g += image.rgb[at + 1] * w; b += image.rgb[at + 2] * w; weight += w;
        }
      }
      const out = (y * width + x) * 3;
      rgb[out] = Math.round(r / weight); rgb[out + 1] = Math.round(g / weight); rgb[out + 2] = Math.round(b / weight);
    }
  }
  return { width, height, rgb };
}

export function toP6(image) {
  const out = Buffer.concat([Buffer.from(`P6\n${image.width} ${image.height}\n255\n`, 'ascii'), image.rgb]);
  if (out.length > OCR_INPUT_LIMIT) throw new Error('Image is over the 16 MiB OCR input limit; lower the resolution.');
  return out;
}

async function sha256File(path) {
  return createHash('sha256').update(await readFile(path)).digest('hex').toUpperCase();
}

function receipt(value, where) {
  if (!value || typeof value.path !== 'string' || !Number.isInteger(value.bytes) || typeof value.sha256 !== 'string' || !/^[0-9A-Fa-f]{64}$/.test(value.sha256)) throw new Error(`OCR manifest receipt ${where} is invalid.`);
  return { path: value.path.replaceAll('\\', '/'), bytes: value.bytes, sha256: value.sha256.toUpperCase() };
}

function requireValue(manifest, path, expected) {
  const actual = path.reduce((current, name) => (current && typeof current === 'object' ? current[name] : undefined), manifest);
  if (actual !== expected) throw new Error(`OCR manifest field ${path.join('.')} is not the supported value.`);
}

export async function rejectReparseAncestors(path) {
  let current = resolve(path);
  for (;;) {
    try {
      if ((await lstat(current)).isSymbolicLink()) throw new Error('OCR paths may not contain symbolic links or junctions.');
    } catch (error) {
      if (error.code !== 'ENOENT') throw error;
    }
    const parent = dirname(current);
    if (parent === current) return;
    current = parent;
  }
}

export const MANIFEST_LIMIT = 1024 * 1024;

async function verifyReceiptFile(root, item) {
  const full = resolveInside(root, item.path);
  await rejectReparseAncestors(full);
  const info = await stat(full).catch(() => null);
  if (!info || !info.isFile() || info.size !== item.bytes) throw new Error(`OCR receipt size mismatch for ${item.path}.`);
  if ((await sha256File(full)) !== item.sha256) throw new Error(`OCR receipt hash mismatch for ${item.path}.`);
  return full;
}

export async function verifyEngine(root, repository) {
  const manifestPath = join(root, 'engine', 'ocr-engine-manifest.json');
  await rejectReparseAncestors(root);
  await rejectReparseAncestors(manifestPath);
  const manifestInfo = await stat(manifestPath).catch(() => null);
  if (manifestInfo && manifestInfo.size > MANIFEST_LIMIT) throw new Error('The OCR setup manifest is too large.');
  let manifest;
  try { manifest = JSON.parse(await readFile(manifestPath, 'utf8')); } catch { throw new Error('Could not read the OCR setup manifest; run scripts/setup-ocr.ps1 and pass its output root.'); }
  requireValue(manifest, ['schemaVersion'], 1);
  requireValue(manifest, ['versions', 'recipe'], '1');
  requireValue(manifest, ['versions', 'tesseract', 'version'], PINS.tesseractVersion);
  requireValue(manifest, ['versions', 'tesseract', 'commit'], PINS.tesseractCommit);
  requireValue(manifest, ['versions', 'leptonica', 'version'], PINS.leptonicaVersion);
  requireValue(manifest, ['versions', 'leptonica', 'commit'], PINS.leptonicaCommit);
  requireValue(manifest, ['versions', 'model', 'language'], 'eng');
  requireValue(manifest, ['versions', 'model', 'variant'], 'fast');
  requireValue(manifest, ['versions', 'model', 'version'], PINS.modelVersion);
  requireValue(manifest, ['versions', 'model', 'commit'], PINS.modelCommit);
  requireValue(manifest, ['build', 'architecture'], 'x64');
  requireValue(manifest, ['build', 'configuration'], 'Release');
  requireValue(manifest, ['build', 'runtimeLibrary'], 'MultiThreaded');
  requireValue(manifest, ['build', 'tesseractCompileDefinition'], 'TESSERACT_DISABLE_DEBUG_FONTS');
  const artifacts = manifest.artifacts ?? {};
  const executable = receipt(artifacts.executable, 'executable');
  const model = receipt(artifacts.model, 'model');
  if (executable.path !== 'engine/bin/tesseract.exe') throw new Error('OCR executable path must be engine/bin/tesseract.exe.');
  if (model.path !== 'engine/tessdata/eng.traineddata') throw new Error('OCR model path must be engine/tessdata/eng.traineddata.');
  if (executable.bytes === 0 || executable.bytes > MAX_ENGINE_BYTES) throw new Error('OCR executable size is invalid.');
  if (model.bytes !== MODEL_BYTES || model.sha256 !== MODEL_SHA256) throw new Error('OCR model identity is not the pinned fast model.');
  const recipe = (artifacts.recipeInputs ?? []).map((item, index) => receipt(item, `recipeInputs[${index}]`));
  const licenses = (artifacts.licenses ?? []).map((item, index) => receipt(item, `licenses[${index}]`));
  if (JSON.stringify(recipe.map((item) => item.path).sort()) !== JSON.stringify([...RECIPE_INPUTS].sort())) throw new Error('OCR recipe input list is not the expected set.');
  if (JSON.stringify(licenses.map((item) => item.path).sort()) !== JSON.stringify([...LICENSES].sort())) throw new Error('OCR license list is not the expected set.');
  for (const item of recipe) await verifyReceiptFile(repository, item);
  for (const item of licenses) await verifyReceiptFile(root, item);
  const executablePath = await verifyReceiptFile(root, executable);
  await verifyReceiptFile(root, model);
  return { executablePath, tessdataDir: join(root, 'engine', 'tessdata'), executable, model, tesseract: PINS.tesseractVersion, leptonica: PINS.leptonicaVersion, modelVersion: PINS.modelVersion };
}

export function recognize(engine, p6, timeoutMs = OCR_TIMEOUT_MS) {
  return new Promise((resolvePromise, reject) => {
    const args = OCR_ARGS.map((arg) => (arg === '<tessdata>' ? engine.tessdataDir : arg));
    const child = spawn(engine.executablePath, args, { cwd: dirname(engine.executablePath), windowsHide: true, stdio: ['pipe', 'pipe', 'pipe'] });
    const out = [];
    const err = [];
    let outBytes = 0;
    let failure = null;
    const timer = setTimeout(() => { failure = new Error('OCR timed out.'); child.kill(); }, timeoutMs);
    child.stdout.on('data', (chunk) => { outBytes += chunk.length; if (outBytes > OCR_STDOUT_LIMIT) { failure = new Error('OCR output is over the limit.'); child.kill(); } else out.push(chunk); });
    child.stderr.on('data', (chunk) => { if (err.reduce((n, c) => n + c.length, 0) < 65536) err.push(chunk); });
    child.on('error', (error) => { clearTimeout(timer); reject(error); });
    child.stdin.on('error', () => {});
    child.on('close', (code) => {
      clearTimeout(timer);
      if (failure) return reject(failure);
      if (code !== 0) return reject(new Error(`OCR exited with code ${code}: ${Buffer.concat(err).toString('utf8').slice(0, 300)}`));
      resolvePromise(Buffer.concat(out).toString('utf8'));
    });
    child.stdin.end(p6);
  });
}

export async function runManifest(manifestPath, engine, hooks = {}) {
  const base = dirname(resolve(manifestPath));
  const manifest = parseManifest(await readFile(manifestPath, 'utf8'));
  const results = [];
  for (const entry of manifest.entries) {
    const imagePath = resolveInside(base, entry.image);
    const truthPath = resolveInside(base, entry.truth);
    const truth = await readFile(truthPath, 'utf8');
    const image = resampleToDpi(decodeImage(imagePath, await readFile(imagePath)), entry.sourceDpi);
    const started = Date.now();
    const text = hooks.textDir
      ? await readFile(resolveInside(hooks.textDir, `${entry.image.replace(/^.*[\\/]/, '').replace(/\.[^.]*$/, '')}.ocr.txt`), 'utf8')
      : await (hooks.recognize ?? recognize)(engine, toP6(image));
    results.push({ id: entry.id, note: entry.note, sourceDpi: entry.sourceDpi, ocrWidth: image.width, ocrHeight: image.height, ms: Date.now() - started, ...scoreText(truth, text), text: hooks.keepText ? text : undefined });
  }
  return { scope: manifest.scope, results, total: aggregate(results) };
}

export function formatTable(report) {
  const pct = (value) => (value === null ? 'n/a' : `${(value * 100).toFixed(2)}%`);
  const rows = [['id', 'chars', 'words', 'CER', 'WER', 'ms'], ...report.results.map((r) => [r.id, String(r.refChars), String(r.refWords), pct(r.cer), pct(r.wer), String(r.ms)]), ['TOTAL', String(report.total.refChars), String(report.total.refWords), pct(report.total.cer), pct(report.total.wer), '']];
  const widths = rows[0].map((_, i) => Math.max(...rows.map((row) => row[i].length)));
  return rows.map((row) => row.map((cell, i) => (i === 0 ? cell.padEnd(widths[i]) : cell.padStart(widths[i]))).join('  ')).join('\n');
}
