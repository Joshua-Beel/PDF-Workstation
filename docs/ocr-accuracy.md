# OCR accuracy harness

Status: a source-build measuring tool. **No accuracy on real scans has been measured.** The only numbers it has produced so far come from synthetic text rendered by the generator described below, on one machine. They say nothing about scanned documents.

The harness runs the built opt-in OCR engine the way the app does and scores the result against typed ground truth with character error rate (CER) and word error rate (WER). It makes no network call and uploads nothing.

## What it runs

`scripts/ocr-accuracy.mjs` (library: `scripts/ocr/accuracy-lib.mjs`) starts the engine with the arguments from `src-tauri/src/ocr_process.rs`:

`tesseract stdin stdout --tessdata-dir <engine>/tessdata -l eng --oem 1 --psm 6 --dpi 150 --loglevel ERROR`

The image is decoded, flattened onto white, and sent on stdin as a binary P6 image, as the app sends a rendered page. A unit test checks the argument string against `ocr_process.rs` and the pins against `src-tauri/ocr_build.rs`, so a change on either side fails the test.

Before any OCR, the engine root must verify against its setup manifest, the same checks `ocr_build.rs` makes, including its refusal of symbolic links and junctions in the paths and its 1 MiB limit on the manifest: schema, tesseract/Leptonica/model version and commit pins, build settings, the pinned English "fast" model size and SHA-256, the executable's size and SHA-256 from the receipt, the license files, and the four recipe inputs in this repository. Any mismatch stops the run with no OCR performed. The harness is a development tool: it does not apply the app's job-object memory and process limits to the engine, only the 30-second timeout and 1 MiB output cap. The check happens once at the start; the harness does not hold the files locked the way the app does.

The images fed to the engine are used at their own pixel size with `--dpi 150`. The app renders pages at 150 DPI, so a scan taken at a higher resolution should carry `"sourceDpi"` in its manifest entry; the harness then area-averages it down to 150 DPI first. This resampler is the harness's own, not the app's PDFium renderer, so it approximates the app's input rather than reproducing it.

## What the harness cannot do

- It does not read PDFs. Export the page you want as a PNG at 150 DPI (the app's export-as-image) and score that. Pages that came from a PDF scan are therefore measured through the export path, not through the app's in-memory render.
- It scores the engine's plain text, not the searchable-PDF output, and not the printable-ASCII filtering used for searchable copies.
- PNG: non-interlaced, 1 to 16 bits, gray, RGB, palette, with or without alpha. PNM: binary P5 and P6, 8-bit.

## Supplying real scans (kept outside the repository)

Do not put scans or their ground truth in this repository. It is public. Use a folder outside it, for example `E:\Coding Projects\Smacrobat\ocr-corpus\`:

```
ocr-corpus\
  manifest.json
  receipt-01.png
  receipt-01.txt
  letter-02.png
  letter-02.txt
```

`manifest.json`:

```json
{
  "schemaVersion": 1,
  "scope": "Own scans, described here. Nothing else is claimed.",
  "entries": [
    { "id": "receipt-01", "image": "receipt-01.png", "truth": "receipt-01.txt", "sourceDpi": 300, "note": "thermal receipt, faded" },
    { "id": "letter-02", "image": "letter-02.png", "truth": "letter-02.txt" }
  ]
}
```

Paths are relative to the manifest folder and may not contain `..`. `sourceDpi` (150 to 1200) is optional. Ids are letters, digits, dot, dash and underscore.

Ground truth is a UTF-8 `.txt` of what a careful reader sees on the image, typed by hand, not produced by OCR and corrected. Reading order, line breaks and runs of spaces do not matter, but every word, digit, case and punctuation mark does. Include everything that is printed on the page, and nothing that is not. A page of about 150 to 400 words per entry is enough; ten varied pages (clean print, a faint or skewed scan, a table, a small font, a stamp or handwriting if you care about those) show more than one long document.

## Scoring

Both texts are normalised, then compared:

1. Unicode NFKC, a leading byte-order mark dropped.
2. Curly single and double quotes become `'` and `"`; hyphen, en dash, em dash and minus become `-`.
3. Every run of whitespace, including newlines, becomes one space; the ends are trimmed.
4. Case and punctuation are kept.

CER is the Levenshtein distance over Unicode characters divided by the number of reference characters. WER is the same distance over space-separated words divided by the number of reference words. Insertions count, so either rate can exceed 100%. The TOTAL row sums edits and divides by the summed reference length; it is not a mean of per-page rates. Because lines are not aligned, a column that OCR reads in a different order than the typed truth shows as errors; type the truth in the order the engine's page-segmentation mode would read it, or treat those pages as a layout finding.

## Running it

From a shell with the project environment variables set (see `CLAUDE.md`), and an engine built by `scripts/setup-ocr.ps1`:

```
pwsh -NoProfile -Command "node scripts/ocr-accuracy.mjs --manifest 'E:\Coding Projects\Smacrobat\ocr-corpus\manifest.json' --engine-root '<setup-ocr output root>' --out 'E:\Coding Projects\Smacrobat\ocr-corpus\report.json'"
```

`--engine-root` may instead be given as `PDF_WORKSTATION_OCR_SETUP_ROOT`, the variable the native tests use. `--keep-text` adds the raw engine output for each entry to the JSON. The run prints a table, and `--out` writes the full report: engine identity, arguments, normalisation text, per-entry counts, CER, WER and time.

Report the result with its scope: how many pages, which scans, which engine hashes. A number from ten of your own scans is a result for those ten scans.

## Synthetic fixtures

`scripts/ocr/accuracy-synthetic.json` holds the ground-truth lines and six variants (Arial 12, Times New Roman 11, Courier New 10, Verdana 14 and Arial 9 point, plus Arial 12 with a 1.5 degree rotation and seeded speckle noise). `scripts/ocr/generate-accuracy-fixtures.ps1` renders them to 150 DPI PNGs with Windows GDI+ and the system fonts, and writes a matching `manifest.json`. No image is committed. The noise is seeded and the geometry is fixed, but glyph rasterisation depends on the installed fonts and Windows version, so numbers can differ between machines.

```
pwsh -NoProfile -File scripts/ocr/generate-accuracy-fixtures.ps1 -OutputDir <empty folder outside the repo>
```

The Vitest file `scripts/ocr/accuracy.test.ts` covers the maths, manifest parsing, image decoding and engine-identity checks without an engine. Its end-to-end case generates the fixtures, verifies the engine and scores them; it is skipped unless `PDF_WORKSTATION_OCR_SETUP_ROOT` is set.

These fixtures are clean computer-rendered text. They exercise the harness and give a floor for regressions; they are not evidence about scans.
