import { mkdir, writeFile } from 'node:fs/promises';
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { OCR_ARGS, OCR_DPI, SETUP_ROOT_ENV, formatTable, runManifest, verifyEngine } from './ocr/accuracy-lib.mjs';

const repository = resolve(dirname(fileURLToPath(import.meta.url)), '..');

function parseArgs(argv) {
  const options = { manifest: null, engineRoot: process.env[SETUP_ROOT_ENV] || null, out: null, keepText: false };
  for (let i = 0; i < argv.length; i++) {
    const arg = argv[i];
    const value = () => { if (i + 1 >= argv.length) throw new Error(`${arg} needs a value.`); return argv[++i]; };
    if (arg === '--manifest') options.manifest = value();
    else if (arg === '--engine-root') options.engineRoot = value();
    else if (arg === '--out') options.out = value();
    else if (arg === '--keep-text') options.keepText = true;
    else throw new Error(`Unknown argument ${arg}. Usage: node scripts/ocr-accuracy.mjs --manifest <manifest.json> [--engine-root <setup-ocr output root>] [--out <report.json>] [--keep-text]`);
  }
  if (!options.manifest) throw new Error('--manifest is required.');
  if (!options.engineRoot) throw new Error(`No engine: pass --engine-root or set ${SETUP_ROOT_ENV} to a scripts/setup-ocr.ps1 output root.`);
  return options;
}

try {
  const options = parseArgs(process.argv.slice(2));
  const engine = await verifyEngine(resolve(options.engineRoot), repository);
  const report = await runManifest(resolve(options.manifest), engine, { keepText: options.keepText });
  const output = {
    schemaVersion: 1,
    generatedAt: new Date().toISOString(),
    engine: { tesseract: engine.tesseract, leptonica: engine.leptonica, model: `eng fast ${engine.modelVersion}`, executableBytes: engine.executable.bytes, executableSha256: engine.executable.sha256, modelSha256: engine.model.sha256 },
    invocation: { arguments: OCR_ARGS.map((arg) => (arg === '<tessdata>' ? '<engine>/tessdata' : arg)), dpi: OCR_DPI, input: 'P6 on stdin, as the app sends it' },
    normalisation: 'NFKC, curly quotes and dashes mapped to ASCII, all whitespace collapsed to one space, trimmed; case and punctuation kept',
    scope: report.scope,
    results: report.results,
    total: report.total,
  };
  console.log(formatTable(report));
  console.log(`\nengine tesseract ${engine.tesseract}, ${output.engine.model}, exe ${engine.executable.sha256.slice(0, 8)}...`);
  console.log(`scope: ${report.scope ?? '(manifest gives none)'}`);
  if (options.out) {
    await mkdir(dirname(resolve(options.out)), { recursive: true });
    await writeFile(resolve(options.out), `${JSON.stringify(output, null, 2)}\n`, 'utf8');
    console.log(`report written to ${resolve(options.out)}`);
  }
} catch (error) {
  console.error(`ocr-accuracy: ${error.message}`);
  process.exitCode = 1;
}
