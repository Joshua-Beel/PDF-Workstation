# Dependency license inventory

This inventory covers the current Windows x64 source tree, the locally extracted v0.2.4 draft installer, a fresh local unsigned debug installer extraction, and the verified signed v0.2.6 draft extraction. It identifies notice-packaging work; it does not mark the app's licensing review complete. Versions below come from the lockfiles and installed package manifests, not requested version ranges.

## What is packaged today

`src-tauri/tauri.conf.json` explicitly includes the PDFium DLL, its exact `VERSION` and `args.gn` provenance files, its top-level `LICENSE`, all files under `resources/pdfium/licenses/`, the generated Rust/frontend third-party notice collection, and five exact MPL source archives. The extracted 0.2.4 draft contains the older PDFium notice set. A fresh local unsigned debug installer extraction matched the generated inventory, full notices, source-archive manifest, and all five archive hashes. The signed v0.2.6 draft was independently extracted from [run 35250878119](https://github.com/Joshua-Beel/Smacrobat/actions/runs/35250878119) before the privacy rewrite: all five archives, their manifest, inventory, and both notice resources matched the then-tagged source. The rewritten [`v0.2.6` tag](https://github.com/Joshua-Beel/Smacrobat/commit/8cf90166623857dddd3854ce769549da55f6f5c4) records the cleaned source history; the draft assets were not rebuilt. That checks bundled resources only; it does not verify the new metadata files in an installer, installation, notice interaction, or complete licensing review.

The PDFium distribution is Chromium build **151.0.7881.0**, pinned by `scripts/setup-pdfium.ps1` to the `bblanchon/pdfium-binaries` release `chromium/7881` asset `pdfium-win-x64.tgz` (asset 441936973, 3,733,154 bytes, SHA-256 `73CC0DE638AC2095E7445BF56A38200A5B7C7CA0E9F4BA144598F2457377AC08`, matching the GitHub release-asset digest). The script refuses, deletes, and does not extract an archive with any other hash. Its local `args.gn` says Windows x64, standalone, V8 disabled, XFA disabled. Keep its complete upstream notice set rather than deriving a new list from the wrapper crate's license:

`abseil.txt`, `agg23.txt`, `fast_float.txt`, `freetype.txt`, `icu.txt`, `lcms.txt`, `libjpeg_turbo.ijg`, `libjpeg_turbo.md`, `libopenjpeg.txt`, `libpng.txt`, `libtiff.txt`, `llvm-libc.txt`, `pdfium.txt`, `simdutf.txt`, `zlib.txt`.

PDFium's version/build configuration files are now in the source bundle resource list beside the DLL and notices. The pinned draft-resource reconstruction also retains those two files. A final installer extraction still needs to bind their exact bytes to the packaged DLL before this provenance is considered installed evidence.

## Frontend production dependencies

These are the seven non-development entries in `package-lock.json`. A production bundler may remove unused code; this list is the conservative package inventory, not a claim that every package is present byte-for-byte in the final JavaScript.

| Package | Locked version | Declared license | Notice source in `node_modules` |
| --- | --- | --- | --- |
| `@tauri-apps/api` | 2.11.1 | Apache-2.0 OR MIT | `@tauri-apps/api/LICENSE_APACHE-2.0`, `LICENSE_MIT` |
| `lucide-react` | 1.46.0 | ISC in package metadata | `lucide-react/LICENSE` also contains Feather-derived icon attribution and MIT text; retain the whole file |
| `react` | 18.3.1 | MIT | `react/LICENSE` |
| `react-dom` | 18.3.1 | MIT | `react-dom/LICENSE` |
| `scheduler` | 0.23.2 | MIT | `scheduler/LICENSE` |
| `loose-envify` | 1.4.0 | MIT | `loose-envify/LICENSE` |
| `js-tokens` | 4.0.0 | MIT | `js-tokens/LICENSE` |

The npm `@tauri-apps/plugin-updater` package was removed: the app calls its own updater commands, and the Rust `tauri-plugin-updater` crate below remains.

Frontend development tools are separately declared: Tauri CLI 2.11.4, TypeScript 5.8.3, Vite 6.4.3, Vitest 3.2.4, React Vite plugin 4.7.0, React test renderer 18.3.1, and React type packages. They are not application runtime dependencies just because they appear in the lockfile. Their installed development trees still need their own upstream notices if those trees or tools are redistributed.

## Direct Rust dependencies

The following versions and expressions were read through offline, locked Cargo metadata for `x86_64-pc-windows-msvc`. Preserve upstream expressions: `AND` and `OR` are not interchangeable.

| Runtime dependency | Locked version | Declared license |
| --- | --- | --- |
| `tauri` | 2.11.5 | Apache-2.0 OR MIT |
| `tauri-plugin-updater` | 2.11.0 | Apache-2.0 OR MIT |
| `serde` | 1.0.229 | MIT OR Apache-2.0 |
| `serde_json` | 1.0.151 | MIT OR Apache-2.0 |
| `pdfium-render` | 0.9.4 | MIT OR Apache-2.0 |
| `image` | 0.25.10 | MIT OR Apache-2.0 |
| `rfd` | 0.15.4 | MIT |
| `tokio` | 1.53.1 | MIT |
| `lopdf` | 0.45.0 | MIT |
| `tempfile` | 3.27.0 | MIT OR Apache-2.0 |
| `windows` | 0.61.3 | MIT OR Apache-2.0 |

`tempfile` is used by the production atomic-save implementation, not only tests. `tauri-build` 2.6.3 is a build dependency (Apache-2.0 OR MIT). Procedural macros and build tools must be classified separately when generating the final inventory; simply copying every `Cargo.lock` package into a list of shipped libraries would overstate it.

## Rust transitive packages requiring more than a generic MIT notice

Walking normal dependency edges produced 340 third-party package identities. Excluding procedural-macro packages and their traversal produced 292. These are conservative metadata sets: feature unification and actual linker elimination still require release-build confirmation. Neither count is a verified count of linked libraries.

Examples that a notice generator must retain accurately:

| Package(s) | Declared expression / local evidence |
| --- | --- |
| `cssparser` 0.36.0, `dtoa-short` 0.3.5, `selectors` 0.36.1, `option-ext` 0.2.0 | MPL-2.0; still reachable after the procedural-macro exclusion. The first three occur through `dom_query`/CSS parsing; `option-ext` is a normal dependency of `dirs-sys`. Their exact `.crate` archives are bundled under `resources/third-party-sources/` with Cargo.lock SHA-256 values and official crates.io URLs in the source manifest and generated notices. |
| `cssparser-macros` 0.6.1 | MPL-2.0; procedural-macro package, classified separately from shipped runtime code. Its exact source archive is packaged with the same verification. |
| `ring` 0.17.14 | Apache-2.0 AND ISC. Its root `LICENSE` refers to `LICENSE-BoringSSL`, `LICENSE-other-bits`, and `src/polyfill/once_cell/LICENSE-APACHE` / `LICENSE-MIT`; copying the small root file alone loses the referenced texts. |
| `brotli` 8.0.4 | BSD-3-Clause AND MIT |
| `dpi` 0.1.2 | Apache-2.0 AND MIT |
| `encoding_rs` 0.8.41 | (Apache-2.0 OR MIT) AND BSD-3-Clause |
| `unicode-ident` 1.0.25 | (MIT OR Apache-2.0) AND Unicode-3.0 |
| ICU4X, `litemap`, `potential_utf`, `tinystr`, `writeable`, `yoke`, `zerofrom`, `zerotrie`, `zerovec` | Unicode-3.0 across the resolved versions; preserve individual package identity and notice files. |
| `alloc-no-stdlib`, `alloc-stdlib`, `subtle` | BSD-3-Clause |
| `libloading`, `rustls-webpki`, `untrusted` | ISC |
| `foldhash`, `zlib-rs` | Zlib |

This table highlights exceptions; it is not the complete transitive package list. Cached crate sources are under `%USERPROFILE%/.cargo/registry/src/<registry>/<crate>-<version>/`. The manifests, complete license files, notice files, and any referenced subcomponent licenses there are the collection sources.

## Generated collection

`scripts/dependency-notices.mjs` collects the conservative resolved Windows Cargo graph and production npm lock entries into `src-tauri/resources/third-party-licenses/inventory.json` and `THIRD-PARTY-NOTICES.txt`. There are 369 records (2026-10-16): 366 Cargo/npm package records, including build/auxiliary packages (not a count of libraries linked into the application), and three `ocr-engine` records described under [OCR engine notices](#ocr-engine-notices). The collection includes Lucide's Feather attribution and nested `ring` notices. The inventory was 202,858 bytes with SHA-256 `9916BFC0E818BE3B17813E53EF7635132CCCA5F0741F9CB25709B45AD5CA0AEF` after `package.json` and `src-tauri/Cargo.toml` gained `license` fields (only `inputHashes` changed); with the OCR records it is 204,537 bytes with SHA-256 `20DDB25EF07E0AD3E9EFA411853E6566E24BF0968A9E8916FD9F1648E664091A` (LF line endings, as generated). All 369 records have license evidence and zero records are marked for review. Moving `sha2` from a development dependency to a production dependency changed `sha2`, `block-buffer`, `cpufeatures`, `crypto-common`, `digest`, and `generic-array` from build/auxiliary to conservative runtime-candidate scope. Package identities, license evidence, source archives, and notice text are unchanged.

Twelve missing local notices were resolved using published crate commits and official upstream license files. `scripts/notice-supplements/manifest.json` records exact commits, provenance URLs and SHA-256 hashes. Offline generation verifies those inputs. Explicitly running `scripts/fetch-notice-supplements.mjs --fetch` retrieves the recorded files and rejects unexpected hashes.

`scripts/mpl-source-archives.manifest.json` pins `cssparser` 0.36.0, `cssparser-macros` 0.6.1, `dtoa-short` 0.3.5, `option-ext` 0.2.0, and `selectors` 0.36.1 to their Cargo.lock SHA-256 values and official crates.io archive URLs. `scripts/mpl-source-archives.mjs` verifies every cached archive hash and every extracted source file before copying the archives and a resource manifest to `src-tauri/resources/third-party-sources/`. It fails closed for a missing, tampered, stale, lock-mismatched, or source-mismatched archive without network access. The local cache comparison found no changed or missing source files; Cargo's generated `.cargo-ok` marker is excluded from that comparison.

The generated files are included in Tauri resources and available through **Menu > Third-party notices** in the source build. The installer script checks freshness before building. Existing PDFium notices remain separately packaged.

## OCR engine notices

The maintainer ruled on 2026-10-14 that every installer includes the OCR engine, from version 0.3.0. Since 2026-10-16 the generated collection above carries three `ocr-engine` records, so **Menu > Third-party notices** shows their texts in every build, with or without the engine:

| Record | Version | Declared license | Repository copy of the text | Upstream text | SHA-256 |
| --- | --- | --- | --- | --- | --- |
| `tesseract` | 5.5.3 | Apache-2.0 | `scripts/notice-supplements/tesseract-5.5.3-LICENSE` | `LICENSE` in the pinned 5.5.3 source archive | `CFC7749B96F63BD31C3C42B5C471BF756814053E847C10F3EB003417BC523D30` |
| `leptonica` | 1.87.0 | BSD-2-Clause | `scripts/notice-supplements/leptonica-1.87.0-leptonica-license.txt` | `leptonica-license.txt` in the pinned 1.87.0 source archive | `87829ABB5BBB00B55A107365DA89E9A33F86C4250169E5A1E5588505BE7D5806` |
| `tessdata_fast-eng` | 4.1.0 | Apache-2.0 | `scripts/notice-supplements/tessdata_fast-4.1.0-LICENSE` | `LICENSE` at tessdata_fast commit `65727574dfcd264acbb0c3e07860e4e9e9b22185` | `CFC7749B96F63BD31C3C42B5C471BF756814053E847C10F3EB003417BC523D30` |

`scripts/dependency-notices.mjs` takes the versions, source URLs and commits from `scripts/ocr/pins.json` and refuses a text whose size or SHA-256 differs from that file's license pin; `scripts/ocr/pins.json` is one of the inventory's `inputHashes`, so a pin change makes `--check` fail until the notices are regenerated. The three texts were copied byte-for-byte from the license outputs of a local `setup-ocr.ps1` run on 2026-10-13, which had verified them against the same pins; `--check` re-verifies their sizes and hashes on every run. The installer also keeps the three texts as sidecars beside the engine (`resources/ocr/<engine identity>/licenses/`). The app no longer appends those sidecars to the notices view: the generated text already contains them, so `notices.rs` returns the bundled text unchanged in every build.

Upstream notice files: neither the Tesseract 5.5.3 nor the Leptonica 1.87.0 source archive contains a `NOTICE` file (checked in the extracted archives of the 2026-10-13 setup). Whether the tessdata_fast repository has one was not checked. Apache-2.0 section 4(d) applies only where a `NOTICE` file exists. This is a provenance record, not legal advice.

Microsoft C runtime: `setup-ocr.ps1` builds the engine with `BUILD_SHARED_LIBS=OFF` and the `MultiThreaded` (`/MT`) runtime, then refuses any engine whose import table names a DLL other than `KERNEL32.DLL` (`dumpbin /dependents`). Every engine it produces therefore links the Visual C++ runtime statically, and no Visual C++ redistributable DLL is needed or packaged. The linked runtime code ships inside `tesseract.exe` under the Visual Studio license terms for distributing programs built with it; those terms ask for no notice text in the product as far as this read goes. This is an engineering read of the recipe and the import check, not a legal review of the Visual Studio license, and it remains unverified by counsel.

### Earlier opt-in evidence (2026-09-25 to 2026-10-13)

Until 2026-10-16 the generated collection had no OCR records, and the default installer extraction proof contained no `resources/ocr/` entries.

An explicitly requested unsigned local OCR installer proof copied a separate five-file resource set under `resources/ocr/1d0f85d0655ed8c0b5f6472cd29213bbd79dc5275ccbee7336360665a94f8c16/`: `bin/tesseract.exe`, `tessdata/eng.traineddata`, and these license-text sidecars:

| Component | Version / source record | Packaged relative path | License-text SHA-256 |
| --- | --- | --- | --- |
| Tesseract | 5.5.3; `scripts/ocr/pins.json` | `licenses/Tesseract-Apache-2.0.txt` | `CFC7749B96F63BD31C3C42B5C471BF756814053E847C10F3EB003417BC523D30` |
| Leptonica | 1.87.0; `scripts/ocr/pins.json` | `licenses/Leptonica-BSD-2-Clause.txt` | `87829ABB5BBB00B55A107365DA89E9A33F86C4250169E5A1E5588505BE7D5806` |
| English fast model | 4.1.0; `scripts/ocr/pins.json` | `licenses/eng-fast-Apache-2.0.txt` | `CFC7749B96F63BD31C3C42B5C471BF756814053E847C10F3EB003417BC523D30` |

The opt-in extraction receipt is `target/ocr-installer-opt-in-proof-20260925-v3/installer-verification.json`, SHA-256 `0E4B373E90916D2B50BD3C1B09000B15030E6A8646C91CA2B1C4362CBA0BDBC6`; the corresponding default receipt is `target/ocr-installer-default-proof-20260925-v3/installer-verification.json`, SHA-256 `E5962F54E3E85F6E78AA1A1EA29291100652ED11B4F1B30BAA1F63197865C50A`. Both checks only inspect unsigned installer contents. They do not verify installation, application launch, updates, signing, a release artifact, or the notice-dialog interaction.

The manual signed OCR validation [run 36499724415](https://github.com/Joshua-Beel/Smacrobat/actions/runs/36499724415) produced a separate non-release artifact from source `67d238218f4796ba7b8505d072868da0f397174a`. Its downloaded installer was 10,456,616 bytes, SHA-256 `8f7a167d1aed369d1a28b7c91692bad8770e774fe9d8afbbed970654144e428c`; its sanitized verification record was 7,386 bytes, SHA-256 `4ed82057155877f2677262479ff4f2b00398cf5f524c51302db50275d315e205`. Independent extraction matched all 26 base resources and five OCR resources, including the license sidecars, and verified valid timestamped **Joshua Beel** signatures on the installer, packaged application, PDFium, and Tesseract. It does not establish notice-dialog interaction, installation, launch, update, upgrade, a tag, draft, or release. This run is history: the signed installed checks now pin artifact 12510717103 from run `39969839829` (source `4a0f89c`, 28 base plus five OCR resources).

Until 2026-10-16 the notice command preserved the base 367-package text byte-for-byte in a default build. In a verified opt-in source build, it appended an `OPTIONAL OCR ENGINE LICENSES` section containing those three exact sidecar texts in the table order. It reads license texts only; it does not execute the engine. If an optional text is missing, tampered, oversized, non-UTF-8, or path-unsafe, it returns `Optional OCR license texts could not be verified.` before the intact base text and does not expose resource paths, hashes, or identities. A focused license-only fixture produced the successful append without an engine file; that fixture does not establish which files a complete installer contains. A retained opt-in bundled-resource readback produced a 24,473-byte appended section containing the three verified sidecar texts; its final warning-order log is `target/ocr-page-probe/logs/notices-retained-readback-warning-final-v2-20260925.log`, SHA-256 `71E67C086942CB88D628D7A874CA8102D7072801B6357AC54C2AB3F9F05A57AF`. Its resource inventory is `target/ocr-notices-proof-20260925/resource-inventory.json`, SHA-256 `6F37F252AE4A2F8717168DD17EF2923CC8DCC75AAA60F6ECDE20D3E41F4775BA`. The existing dialog displays the returned string; frontend mocks verified the default, ordered opt-in, and warning cases (replaced on 2026-10-16), but not native desktop or installed-app interaction.

That append was removed on 2026-10-16 (see above). Azure OCR remains refused except for the guarded artifact-only source mode with a verified unsigned setup, fresh target output, and explicit verified publisher. That mode stages the engine and PDFium before signing, preserves the source inventory, disables updater artifacts, and does not publish a release. The completed validation run proves a signed package and extraction, but not installation, update, or release behavior. This is an inventory and provenance record, not a complete licensing review or legal advice.

To regenerate after a dependency change:

```powershell
cargo fetch --locked --target x86_64-pc-windows-msvc --manifest-path src-tauri/Cargo.toml
node scripts/mpl-source-archives.mjs
node scripts/mpl-source-archives.mjs --check
node scripts/dependency-notices.mjs
node scripts/dependency-notices.mjs --check
```

## Remaining release checks

1. Check the installed app's notice entry after installation, including the three `ocr-engine` records. The signed v0.2.6 draft extraction establishes bundled resources, not installed-app interaction.
2. Record the WebView2 bootstrapper/distribution version and its associated terms separately. The config downloads the bootstrapper; this inventory did not inspect that payload. The app itself is MIT-licensed: the root `LICENSE` holds the text, and `package.json` and `src-tauri/Cargo.toml` declare `MIT`.

## Reproducing the metadata read

```text
cargo metadata --offline --locked --filter-platform x86_64-pc-windows-msvc --format-version 1 --manifest-path src-tauri/Cargo.toml
```

The initial inventory used local lockfiles, installed dependency notices and the extracted v0.2.4 draft's PDFium directory. The later supplement collection fetched the recorded upstream license files. The MPL archive collection compared all five official cached archives to their extracted Cargo source trees; fresh unsigned debug and signed v0.2.6 draft extractions hash-checked all five archives plus both notice resources. These checks establish reproducibility and resource inclusion, not complete release-license review.
