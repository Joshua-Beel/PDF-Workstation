# PDF Workstation

A Windows PDF app being built as an alternative to Acrobat, with a familiar layout and tools that work locally. PDF Workstation is the working name for now.

You can already read PDFs, rearrange pages, and save your changes to a new file. There's still plenty to build before it can replace Acrobat for everyday work. Your PDFs stay on your computer: there's no document upload service or telemetry.

## Install

PDF Workstation is free, open-source software under the [MIT License](LICENSE).

Download the Windows setup file from the [latest release](https://github.com/Joshua-Beel/PDF-Workstation/releases/latest). The installer includes the PDF engine. It does not include Microsoft's WebView2 Runtime: on a machine without WebView2, the installer is configured to download it, so that install needs network access once. Installing without WebView2 has not been exercised. Once installed, open a file or choose **Explore a sample PDF** to try it out.

For updates, use **Menu > Check for updates**. The app shows what's new and lets you choose when to install. Save any edited documents first; the app won't install an update while you have unsaved changes.

Version 0.3.1 is the current published release (2026-10-27): publisher-signed installer and app (Joshua Beel), a signed update package, and the OCR engine included. Hosted checks on GitHub runners installed it and exercised the listed flows on controlled fixtures, and published 0.2.0 updated itself to 0.3.1 in-app; arbitrary documents and other machines are not verified. Details: [release and signing status](docs/features.md#release-and-signing-status) and [Releases and signing](docs/releases.md).

## What works

"Source build" means the feature is in the code but not yet in a published release. Full descriptions and limits: [docs/features.md](docs/features.md).

**[Reading and navigation](docs/features.md#reading-and-navigation)**
- Reading: tabs, scrolling, pan, jump to page, zoom 10–400%, fit width or page; the source build restores each tab's last page for the session.
- Drop to open (source build): drop one `.pdf` onto the window; native drag/drop remains unverified.
- Pages sidebar (source build): renders only nearby rows, with keyboard navigation.
- Page labels (source build): PDF page labels beside physical page numbers. [Guide](docs/tools/page-labels.md)
- Bookmarks (source build): up to 1,000 nested bookmarks with destination zoom; add, rename, delete, move, indent and outdent save into a new copy (not in recovery points; no installed-app check); scroll and zoom feel in the running app unverified.
- Passwords (source build): open, retry or cancel; passwords aren't saved and encrypted PDFs stay read-only. Verified in the signed installed app on controlled fixtures.
- Document properties (source build): Ctrl+D shows metadata, sizes, permissions and signature count; signatures aren't validated.

**[Text and search](docs/features.md#text-and-search)**
- Page text (source build): view and copy the current page's embedded text.
- On-page text selection (source build): select supported embedded text directly on the page.
- Search (source build): Ctrl+F across embedded text, with case matching, one excerpt per matching page (up to 500 pages) and on-page highlights where text geometry is supported.

**[OCR](docs/features.md#ocr)** (in the 0.3.1 installer; installed use checked only on the packaged sample, including on the published installer, run 40672496983)
- Current-page OCR: recognize one page into a copyable dialog with a local English engine; accuracy and real-document use unverified.
- Searchable OCR copy: save a new 150-DPI raster PDF with an invisible text layer; real-scan accuracy unverified.

**[Printing](docs/features.md#printing)**
- Ctrl+P prints all pages, the current page, or ranges, with current edits. Verified in the signed installed app with Microsoft Print to PDF; physical paper and other drivers unverified.

**[Pages and documents](docs/features.md#pages-and-documents)**
- Organize pages: rotate, move, delete, extract; the source build adds Move to page, Split, Crop, Reset crop, Insert and Replace pages. [Guide](docs/tools/organize-pages.md)
- Combine two PDFs into a new copy (source build).
- Saving: undo/redo, then **Save a Copy**; the original is never overwritten.
- Create a PDF from one or up to 32 PNG/JPEG images (source build); installed-app and release behaviour unverified. [Guide](docs/tools/create-pdf.md)
- Replace a strict full-page image in a new copy (source build); installed-app behaviour unverified.
- Export the current page as PNG or JPEG at 72, 150 or 300 DPI (source build). [Guide](docs/tools/export-page-image.md)
- Raster redaction of rectangles on one page into a fully rasterized new copy (source build). [Guide](docs/tools/redaction.md)

**[Annotate and forms](docs/features.md#annotate-and-forms)**
- Comments and highlights (current source work): sticky notes, area highlights and selected-text highlights. [Guide](docs/tools/comments.md)
- Fill forms (current source work): fill a strict subset of existing fields into a new copy. [Guide](docs/tools/forms.md)

**[Workspace and privacy](docs/features.md#workspace-and-privacy)**
- Light and dark themes, All tools panel; the source build remembers preferences and up to 50 recent files. Recovery of unsaved edits after a restart remains unverified.

### Handy shortcuts

| Action | Shortcut |
| --- | --- |
| Open / save a copy / find | Ctrl+O / Ctrl+S / Ctrl+F |
| Print / document properties (source build) | Ctrl+P / Ctrl+D |
| Undo / redo | Ctrl+Z / Ctrl+Shift+Z or Ctrl+Y |
| Close a tab / switch tabs | Ctrl+W / Ctrl+Tab |
| Actual size / fit width / fit page | Ctrl+1 / Ctrl+2 / Ctrl+0 |
| Previous / next page; first / last page | Page Up / Page Down; Home / End |
| Toggle page list / tools panel | F4 / Shift+F4 |
| Toggle hand tool | H |

## What's still missing

- Not ready yet: general image conversion, arbitrary text and image editing, OCR checked on real documents, signatures, form preparation, content-preserving or multi-page redaction, replies, author identity, and foreign-annotation interchange.
- Saving always writes a new copy; some page operations are blocked on signed, form, tagged, bookmarked or annotated files ([Organize Pages guide](docs/tools/organize-pages.md)).
- Printing is unverified on physical paper and non-Microsoft drivers; more real-document testing is needed.
- Per-tool limits: [Known limits](docs/features.md#known-limits). Priorities: [Known gaps](docs/gaps.md) and the [development roadmap](docs/development-roadmap.md).

## Run from source

You'll need Windows, Node.js, Rust with the MSVC toolchain, Visual Studio C++ build tools, and WebView2. If you've just installed Rust, open a new terminal so `cargo` is available.

From the project folder, run these commands in PowerShell:

```powershell
npm ci
npm run fixtures
./scripts/setup-pdfium.ps1
npm run tauri -- dev
```

To build a standalone development executable:

```powershell
npm run tauri -- build --debug --no-bundle
```

You'll find it at `src-tauri/target/debug/pdf-workstation.exe`. Keep the `resources` folder beside it. This build runs without a development server.

To run the tests:

```powershell
cargo fetch --locked --target x86_64-pc-windows-msvc --manifest-path src-tauri/Cargo.toml
npm test
cargo test --locked --manifest-path src-tauri/Cargo.toml
```

To build an installer, see [Releases and signing](docs/releases.md). That guide covers signing prerequisites, GitHub Actions configuration, and publishing updates.

## Under the hood

Tauri 2, React 18, TypeScript and CSS Modules, with Rust for PDF work: PDFium renders pages on a dedicated thread and lopdf writes edited copies. The viewer renders pages near the viewport, with a native cache budget of 512 MiB covering PNG data and estimated decoded pixels.

An open document keeps a snapshot of its original bytes. Before saving, the app checks that PDFium can open the output and that the page count is right, then writes a new file without replacing an existing one.

More: [architecture notes](docs/decisions.md), [PDFium documentation](https://docs.rs/pdfium-render/0.9.4/pdfium_render/), and the [current Acrobat workspace](https://helpx.adobe.com/acrobat/desktop/get-started/learn-the-basics/workspace.html) the interface follows.

## Recent changes

Full history: [docs/changelog.md](docs/changelog.md)

- Bookmark editing (source build): add at the current page, rename, delete with children, move up or down, indent and outdent, undo and redo; saved only into a new copy with correct outline links and counts, untouched bookmarks keep their destinations; signed, encrypted or malformed outlines refused (SM1).
- Published 0.3.1 installer OCR check recorded: hosted run 40672496983 installed it fresh and current-page OCR on packaged welcome.pdf p1 was recognized (402 bytes); accuracy, searchable copies, real scans not verified (SM6; docs only).
- Viewer: renders for pages scrolled out of view are cancelled while still queued (last page of a 32-page fling 1.9 s to 65-130 ms, source build); an opened PDF's bytes are held once, not twice (SM5).
- Child-process spawning (OCR and PDF helper) now holds a process-wide lock while its pipes are inheritable, and test processes started with `std::process::Command` take the same lock: fixes intermittent "OCR output pipes did not close" in parallel `cargo test` (SM4; test-visible only).
- Release scripts: the installed print-dialog diagnostic keeps its last completed native-window receipt when the final poll is cut short by the deadline (fixes a CI flake), and the `WEBDRIVER_ERROR_DIAG` sanitizer now also redacts `file://server/share` and a UNC path glued to a word (SM6; scripts and tests only).
- Text replacement and selected-text highlights now pass the same two-reader copy-permission check as search and copy (SM3).
- In-app update test file gets a 60 s per-test timeout, so pwsh start-up on a busy CI runner no longer trips the 5 s default (SM0; test only).
- Text search, selection, copy and OCR now also need PDFium's own copy permission (`/P` bit 5), so a file whose readers pick different trailers cannot bypass a restriction (SM3).
- OCR save-in-parts review fixes: Discard after some parts are saved asks first and names them, a paused job lists saved parts, crash recovery after a cross-volume copy, no junction following, free-space check before each new part (SM4).
- Large-document OCR (source build): a copy that would pass 1 GiB can be saved in parts, `<name> (part 1 of N).pdf`, each at most 1 GiB, checked from disk and never overwriting; unit-tested with a small size limit only (SM4).
- The Open dialog is now titled "Open a PDF" (it showed the Windows default "Open"), matching the other titled native dialogs; both dialog verifiers expect it (SM3).
- A file refused for editing by the cross-reference check now keeps the copy permission of an empty-password encrypted file (search and copy no longer bypass `/P`) (SM3).
- Cross-reference review fixes: a file refused for editing keeps search, selection, copy and OCR; refusals need a used object; walks are bounded on stream input and offsets; raw-byte object-stream check (SM3).
- lopdf loads now resolve objects as the cross-reference chain says (free and missing entries, hybrid `/XRefStm`); where PDFium, lopdf and the standard disagree about a used object, editing is refused (SM3).
- Release scripts: the `WEBDRIVER_ERROR_DIAG` sanitizer now redacts forward-slash and mixed-slash Windows paths, UNC paths and paths containing spaces to the end of the token, with negative controls and a planted-offender control for the backslash-quote scan (SM6; test/script only).
