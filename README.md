# PDF Workstation

A Windows PDF app being built as an alternative to Acrobat, with a familiar layout and tools that work locally. PDF Workstation is the working name for now.

You can already read PDFs, rearrange pages, and save your changes to a new file. There's still plenty to build before it can replace Acrobat for everyday work. Your PDFs stay on your computer: there's no document upload service or telemetry.

## Install

PDF Workstation is free, open-source software under the [MIT License](LICENSE).

Download the Windows setup file from the [latest release](https://github.com/Joshua-Beel/PDF-Workstation/releases/latest). The installer includes the PDF engine. It does not include Microsoft's WebView2 Runtime: on a machine without WebView2, the installer is configured to download it, so that install needs network access once. Installing without WebView2 has not been exercised. Once installed, open a file or choose **Explore a sample PDF** to try it out.

For updates, use **Menu > Check for updates**. The app shows what's new and lets you choose when to install. Save any edited documents first; the app won't install an update while you have unsaved changes.

Version 0.2.0 is the current published release, with a signed update package but no Windows publisher signature; a signed 0.2.6 draft is unpublished, and its hosted checks cover only the exercised flows on controlled packaged fixtures. Details: [release and signing status](docs/features.md#release-and-signing-status) and [Releases and signing](docs/releases.md).

## What works

"Source build" means the feature is in the code but not yet in a published release. Full descriptions and limits: [docs/features.md](docs/features.md).

**[Reading and navigation](docs/features.md#reading-and-navigation)**
- Reading: tabs, scrolling, pan, jump to page, zoom 10–400%, fit width or page; the source build restores each tab's last page for the session.
- Drop to open (source build): drop one `.pdf` onto the window; native drag/drop remains unverified.
- Pages sidebar (source build): renders only nearby rows, with keyboard navigation.
- Page labels (source build): PDF page labels beside physical page numbers. [Guide](docs/tools/page-labels.md)
- Bookmarks (source build): up to 1,000 nested bookmarks with destination zoom; no bookmark editing, and scroll and zoom feel in the running app unverified.
- Passwords (source build): open, retry or cancel; passwords aren't saved and encrypted PDFs stay read-only. Verified in the signed installed app on controlled fixtures.
- Document properties (source build): Ctrl+D shows metadata, sizes, permissions and signature count; signatures aren't validated.

**[Text and search](docs/features.md#text-and-search)**
- Page text (source build): view and copy the current page's embedded text.
- On-page text selection (source build): select supported embedded text directly on the page.
- Search (source build): Ctrl+F across embedded text, with case matching, one excerpt per matching page (up to 500 pages) and on-page highlights where text geometry is supported.

**[OCR](docs/features.md#ocr)** (source build; planned for the default installer from 0.3.1, not yet verified in an installed build)
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

- Not ready yet: general image conversion, arbitrary text and image editing, OCR in a published release (planned for 0.3.1), signatures, form preparation, content-preserving or multi-page redaction, replies, author identity, and foreign-annotation interchange.
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

- In-app update check: accepts GitHub's longer token format (`.`, `-`), anchored so a trailing newline is rejected (SM0; script and test only).
- In-app update check: tauri-driver start moved inside the session try, so a start failure is cleaned up and reported as itself (SM6; review P3).
- In-app update check: 7-Zip and the standalone silent installers run with the shared minimal environment, as tauri-driver already does (SM6; review P3).
- In-app update check: before the post-update session, waits up to 60 s for updater setup processes started from temp after the retry to exit, and fails if any remain (SM6; review P3).
- In-app update check: a WebDriver session stop in the main flow now fails if the driver or any owned process did not exit; error-path cleanup stays best effort (SM6; review P3).
- Docs: `docs/releases.md` in-app update section names the current receipt schema (v3) for the false publisher-UI claims (SM6; review P3).
- In-app update check: the receipt also records the shell-window probe taken at the end of the relaunch wait (`updater.shellWindowAtRelaunch`) (SM6; review P3).
- In-app update check: the two GitHub API release-metadata reads use the job's read-only token (API host only, no redirects, removed from the environment first); public downloads stay unauthenticated (SM6).
Full history: [docs/changelog.md](docs/changelog.md)

- Save-copy prune: regression test that an indirect destination to a kept page survives (SM2); README recent changes trimmed to 15 lines (SM0). Full entries in [docs/changelog.md](docs/changelog.md).
- In-app update check: automatic relaunch still required when an Explorer shell window exists; without one it is recorded as not observed, with its reason (schema v3) (SM6; script, tests, docs).
- In-app update check: WebDriver lifecycle now uses the shared hosted-proven launch helpers (30 s session create, native-driver wait, owned-process cleanup); firewall precheck added (SM6; scripts and tests).
- In-app update check: stopped assigning the read-only `$HOME` (run 40618989631) and `$Matches`; suppressed stray `WaitForExit` output; AST test guards both (SM6; scripts and tests).
- Save-copy prune follow-ups (SM2): dead page links are dropped when the `/Dest` or GoTo `/D` is an indirect array too; links that never had a destination stay; `/Annots` null cleanup tested.
- In-app update check: the installed-facts check now expects Tauri's quoted InstallLocation and names any mismatched field (no paths). Run 40596111704 failed there (SM6; script and tests).
- In-app update check: hosted noninteractive publisher mode, as in the OCR upgrade check; Explorer dialog not attempted, signatures still verified. Run 40588308128 failed there (SM6; scripts, workflow, tests).
