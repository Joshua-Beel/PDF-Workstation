import { execFileSync } from 'node:child_process';
import { deflateSync } from 'node:zlib';
import { mkdtempSync, mkdirSync, symlinkSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { join, resolve } from 'node:path';
import { describe, expect, it } from 'vitest';
import { LICENSES, MODEL_BYTES, MODEL_SHA256, OCR_ARGS, PINS, RECIPE_INPUTS, aggregate, decodePng, decodePnm, formatTable, levenshtein, normalizeText, parseManifest, resampleToDpi, resolveInside, runManifest, scoreText, toP6, verifyEngine } from './accuracy-lib.mjs';

const repo = resolve(__dirname, '../..');
const scratch = () => {
  const base = process.env.TEMP || process.env.TMP;
  if (!base) throw new Error('TEMP or TMP must be set; refusing to write scratch files into the repository.');
  return mkdtempSync(join(base, 'ocr-accuracy-'));
};

function crc32(bytes: Buffer) { let crc = 0xffffffff; for (const byte of bytes) { crc ^= byte; for (let bit = 0; bit < 8; bit++) crc = (crc >>> 1) ^ ((crc & 1) ? 0xedb88320 : 0); } return (crc ^ 0xffffffff) >>> 0; }
function chunk(type: string, data: Buffer) { const name = Buffer.from(type, 'ascii'); const len = Buffer.alloc(4); len.writeUInt32BE(data.length); const crc = Buffer.alloc(4); crc.writeUInt32BE(crc32(Buffer.concat([name, data]))); return Buffer.concat([len, name, data, crc]); }
function png(width: number, height: number, colorType: number, rowsOf: (y: number) => number[], filterOf: (y: number) => number = () => 0) {
  const ihdr = Buffer.alloc(13); ihdr.writeUInt32BE(width, 0); ihdr.writeUInt32BE(height, 4); ihdr[8] = 8; ihdr[9] = colorType;
  const rows = []; for (let y = 0; y < height; y++) rows.push(Buffer.from([filterOf(y), ...rowsOf(y)]));
  return Buffer.concat([Buffer.from([137, 80, 78, 71, 13, 10, 26, 10]), chunk('IHDR', ihdr), chunk('IDAT', deflateSync(Buffer.concat(rows))), chunk('IEND', Buffer.alloc(0))]);
}

describe('normalizeText', () => {
  it('collapses whitespace, maps quotes and dashes, keeps case and punctuation', () => {
    expect(normalizeText('  Hello\r\n\tWorld  ')).toBe('Hello World');
    expect(normalizeText('\u201Cit\u2019s\u201D \u2013 fine\u2014ok')).toBe('"it\'s" - fine-ok');
    expect(normalizeText('Ab, C.')).toBe('Ab, C.');
    expect(normalizeText('\uFB01x \uFF21')).toBe('fix A');
  });
});

describe('levenshtein and scoring', () => {
  it('computes known edit distances', () => {
    expect(levenshtein(Array.from('kitten'), Array.from('sitting'))).toBe(3);
    expect(levenshtein([], Array.from('abc'))).toBe(3);
    expect(levenshtein(Array.from('abc'), [])).toBe(3);
    expect(levenshtein(['a', 'b'], ['a', 'b'])).toBe(0);
  });
  it('scores CER and WER on normalised text', () => {
    const exact = scoreText('The cat sat.', 'The  cat\nsat.');
    expect(exact).toMatchObject({ charEdits: 0, wordEdits: 0, cer: 0, wer: 0, refChars: 12, refWords: 3 });
    const one = scoreText('abcd efgh', 'abcd efgx');
    expect(one.charEdits).toBe(1);
    expect(one.cer).toBeCloseTo(1 / 9);
    expect(one.wordEdits).toBe(1);
    expect(one.wer).toBeCloseTo(1 / 2);
  });
  it('counts a missing line as deletions and extra text as insertions (CER may exceed 1)', () => {
    expect(scoreText('one two three', '').cer).toBe(1);
    expect(scoreText('ab', 'abcdef').cer).toBe(2);
    expect(scoreText('one two', 'one two three four').wer).toBe(1);
  });
  it('refuses empty ground truth', () => {
    expect(() => scoreText('   \n ', 'x')).toThrow(/empty/);
  });
  it('aggregates by total edits over total reference length, not by mean of ratios', () => {
    const total = aggregate([scoreText('aaaa', 'aaab'), scoreText('bbbbbbbbbbbb', 'bbbbbbbbbbbb')]);
    expect(total.charEdits).toBe(1);
    expect(total.refChars).toBe(16);
    expect(total.cer).toBeCloseTo(1 / 16);
  });
  it('formats a table with a TOTAL row', () => {
    const results = [{ id: 'a', ms: 5, ...scoreText('abcd', 'abcd') }];
    const text = formatTable({ results, total: aggregate(results) });
    expect(text).toContain('TOTAL');
    expect(text).toContain('0.00%');
  });
});

describe('manifest parsing', () => {
  const good = { schemaVersion: 1, scope: 'x', entries: [{ id: 'one', image: 'a.png', truth: 'a.txt', sourceDpi: 300, note: 'n' }, { id: 'two', image: 'sub/b.PNM', truth: 'sub/b.txt' }] };
  it('accepts a valid manifest', () => {
    const parsed = parseManifest(JSON.stringify(good));
    expect(parsed.entries).toEqual([{ id: 'one', image: 'a.png', truth: 'a.txt', sourceDpi: 300, note: 'n' }, { id: 'two', image: 'sub/b.PNM', truth: 'sub/b.txt', sourceDpi: null, note: null }]);
  });
  it.each([
    ['bad json', '{'],
    ['wrong version', JSON.stringify({ ...good, schemaVersion: 2 })],
    ['no entries', JSON.stringify({ schemaVersion: 1, entries: [] })],
    ['duplicate id', JSON.stringify({ schemaVersion: 1, entries: [good.entries[0], good.entries[0]] })],
    ['bad id', JSON.stringify({ schemaVersion: 1, entries: [{ ...good.entries[0], id: 'a b' }] })],
    ['absolute path', JSON.stringify({ schemaVersion: 1, entries: [{ ...good.entries[0], image: 'C:\\x\\a.png' }] })],
    ['parent path', JSON.stringify({ schemaVersion: 1, entries: [{ ...good.entries[0], truth: '../a.txt' }] })],
    ['pdf image', JSON.stringify({ schemaVersion: 1, entries: [{ ...good.entries[0], image: 'a.pdf' }] })],
    ['dpi below 150', JSON.stringify({ schemaVersion: 1, entries: [{ ...good.entries[0], sourceDpi: 100 }] })],
  ])('rejects %s', (_name, text) => {
    expect(() => parseManifest(text)).toThrow();
  });
  it('keeps resolved paths inside the manifest folder', () => {
    expect(resolveInside('/base', 'a/b.png').replaceAll('\\', '/')).toMatch(/base\/a\/b\.png$/);
    expect(() => resolveInside('/base', '../x')).toThrow(/escapes/);
  });
});

describe('image decoding', () => {
  it('decodes RGB, gray and RGBA PNGs with the filters and composites alpha on white', () => {
    const rgb = decodePng(png(2, 2, 2, (y) => (y === 0 ? [255, 0, 0, 0, 255, 0] : [0, 0, 255, 10, 10, 10])));
    expect([...rgb.rgb]).toEqual([255, 0, 0, 0, 255, 0, 0, 0, 255, 10, 10, 10]);
    const gray = decodePng(png(3, 1, 0, () => [0, 128, 255]));
    expect([...gray.rgb]).toEqual([0, 0, 0, 128, 128, 128, 255, 255, 255]);
    const alpha = decodePng(png(1, 1, 6, () => [0, 0, 0, 0]));
    expect([...alpha.rgb]).toEqual([255, 255, 255]);
    const sub = decodePng(png(3, 1, 0, () => [10, 5, 5], () => 1));
    expect([...sub.rgb].filter((_, i) => i % 3 === 0)).toEqual([10, 15, 20]);
    const up = decodePng(png(1, 2, 0, (y) => (y === 0 ? [100] : [20]), (y) => (y === 0 ? 0 : 2)));
    expect([...up.rgb].filter((_, i) => i % 3 === 0)).toEqual([100, 120]);
    const avg = decodePng(png(2, 2, 0, (y) => (y === 0 ? [100, 100] : [20, 20]), (y) => (y === 0 ? 0 : 3)));
    expect(avg.rgb[6]).toBe(70);
    const paeth = decodePng(png(2, 2, 0, (y) => (y === 0 ? [100, 100] : [5, 5]), (y) => (y === 0 ? 0 : 4)));
    expect(paeth.rgb[6]).toBe(105);
  });
  it('rejects non-PNG and interlaced data', () => {
    expect(() => decodePng(Buffer.from('nope'))).toThrow();
    const bytes = png(1, 1, 0, () => [0]);
    bytes[28] = 1;
    expect(() => decodePng(bytes)).toThrow(/Interlaced/);
  });
  it('decodes P5 and P6 PNM and emits P6 for the engine', () => {
    const p5 = decodePnm(Buffer.concat([Buffer.from('P5\n# c\n2 1\n255\n'), Buffer.from([7, 9])]));
    expect([...p5.rgb]).toEqual([7, 7, 7, 9, 9, 9]);
    const p6 = toP6(p5);
    expect(p6.subarray(0, 11).toString('ascii')).toBe('P6\n2 1\n255\n');
    expect(p6.length).toBe(11 + 6);
    expect(() => decodePnm(Buffer.from('P6\n2 1\n255\n\x00'))).toThrow();
  });
  it('area-averages a 300 DPI image down to 150 DPI and leaves 150 DPI alone', () => {
    const image = { width: 2, height: 2, rgb: Buffer.from([0, 0, 0, 100, 100, 100, 100, 100, 100, 200, 200, 200]) };
    expect(resampleToDpi(image, 150)).toBe(image);
    const half = resampleToDpi(image, 300);
    expect([half.width, half.height]).toEqual([1, 1]);
    expect(half.rgb[0]).toBe(100);
  });
});

describe('engine identity', () => {
  it('uses the same invocation as ocr_process.rs and the same pins as ocr_build.rs', () => {
    const processSource = readFileSync(join(repo, 'src-tauri/src/ocr_process.rs'), 'utf8');
    expect(processSource).toContain('"-l" "eng" "--oem" "1" "--psm" "6" "--dpi" "150" "--loglevel" "ERROR"');
    expect(OCR_ARGS.slice(4).join(' ')).toBe('-l eng --oem 1 --psm 6 --dpi 150 --loglevel ERROR');
    const build = readFileSync(join(repo, 'src-tauri/ocr_build.rs'), 'utf8');
    for (const value of [MODEL_SHA256, ...Object.values(PINS), ...RECIPE_INPUTS, ...LICENSES]) expect(build).toContain(value);
    expect(build).toContain('4_113_088');
    expect(MODEL_BYTES).toBe(4113088);
  });
  it('refuses a root with no manifest or a wrong manifest, before any OCR runs', async () => {
    const dir = scratch();
    try {
      await expect(verifyEngine(dir, repo)).rejects.toThrow(/manifest/);
      mkdirSync(join(dir, 'engine'), { recursive: true });
      writeFileSync(join(dir, 'engine', 'ocr-engine-manifest.json'), JSON.stringify({ schemaVersion: 2 }));
      await expect(verifyEngine(dir, repo)).rejects.toThrow(/schemaVersion/);
    } finally { rmSync(dir, { recursive: true, force: true }); }
  });
  it('refuses an oversize manifest and a root reached through a link', async () => {
    const dir = scratch();
    try {
      mkdirSync(join(dir, 'real', 'engine'), { recursive: true });
      writeFileSync(join(dir, 'real', 'engine', 'ocr-engine-manifest.json'), ' '.repeat(1024 * 1024 + 1));
      await expect(verifyEngine(join(dir, 'real'), repo)).rejects.toThrow(/too large/);
      let linked = true;
      try { symlinkSync(join(dir, 'real'), join(dir, 'link'), 'junction'); } catch { linked = false; }
      if (linked) await expect(verifyEngine(join(dir, 'link'), repo)).rejects.toThrow(/symbolic links or junctions/);
    } finally { rmSync(dir, { recursive: true, force: true }); }
  });
  it('scores a manifest run with a stubbed recognizer', async () => {
    const dir = scratch();
    try {
      writeFileSync(join(dir, 'a.pgm'), Buffer.concat([Buffer.from('P5\n1 1\n255\n'), Buffer.from([0])]));
      writeFileSync(join(dir, 'a.txt'), 'hello world\n');
      writeFileSync(join(dir, 'm.json'), JSON.stringify({ schemaVersion: 1, entries: [{ id: 'a', image: 'a.pgm', truth: 'a.txt' }] }));
      const report = await runManifest(join(dir, 'm.json'), {} as never, { recognize: async (_e: unknown, p6: Buffer) => { expect(p6.subarray(0, 2).toString()).toBe('P6'); return 'hello  wurld\n'; } });
      expect(report.results[0].charEdits).toBe(1);
      expect(report.results[0].wordEdits).toBe(1);
      expect(report.total.cer).toBeCloseTo(1 / 11);
    } finally { rmSync(dir, { recursive: true, force: true }); }
  });
});

describe('synthetic generator spec', () => {
  it('has unique ids, ASCII ground-truth lines and one skewed noisy variant', () => {
    const spec = JSON.parse(readFileSync(join(repo, 'scripts/ocr/accuracy-synthetic.json'), 'utf8'));
    const ids = spec.variants.map((v: { id: string }) => v.id);
    expect(new Set(ids).size).toBe(ids.length);
    expect(spec.scope).toContain('Not real scans');
    for (const line of spec.lines) expect(line).toMatch(/^[\x20-\x7e]+$/);
    expect(spec.variants.filter((v: { skewDegrees?: number; noiseDensity?: number }) => v.skewDegrees && v.noiseDensity)).toHaveLength(1);
  });
});

const engineRoot = process.env.PDF_WORKSTATION_OCR_SETUP_ROOT;
describe.skipIf(!engineRoot)('opt-in end to end run on generated synthetic fixtures (needs PDF_WORKSTATION_OCR_SETUP_ROOT and pwsh)', () => {
  it('verifies the engine, renders the fixtures and measures them', async () => {
    const dir = scratch();
    try {
      execFileSync('pwsh', ['-NoProfile', '-File', join(repo, 'scripts/ocr/generate-accuracy-fixtures.ps1'), '-OutputDir', dir], { stdio: 'pipe', timeout: 60000 });
      const engine = await verifyEngine(resolve(engineRoot as string), repo);
      const report = await runManifest(join(dir, 'manifest.json'), engine);
      expect(report.results).toHaveLength(6);
      for (const item of report.results) { expect(item.cer).toBeGreaterThanOrEqual(0); expect(item.refChars).toBeGreaterThan(300); }
      const clean = report.results.find((item: { id: string }) => item.id === 'arial-12');
      expect(clean.cer).toBeLessThan(0.05);
      console.log(`synthetic only: total CER ${(report.total.cer * 100).toFixed(2)}% WER ${(report.total.wer * 100).toFixed(2)}%`);
    } finally { rmSync(dir, { recursive: true, force: true }); }
  }, 120000);
});
