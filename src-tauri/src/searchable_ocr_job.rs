use std::{sync::Arc, time::{Duration, Instant}};

use crate::{
    ocr::OcrPageRequest,
    ocr_process::{OcrCancellation, OcrOperation, OcrProcessError, OcrProcessRunner},
    searchable_ocr::{map_words_to_display, parse_tesseract_tsv},
    searchable_pdf::{write_searchable_pdf, SearchableRasterPage, SearchableWord},
    service::PdfService,
};

const _: () = assert!(
    crate::searchable_ocr::RASTER_DPI == crate::ocr::OCR_DPI as u32 && crate::ocr_commands::OCR_DPI == crate::ocr::OCR_DPI,
    "the containment slack DPI must equal the OCR raster DPI"
);

pub(crate) const MAX_DOCUMENT_PAGES: usize = 32;
pub(crate) const MAX_DOCUMENT_RUNTIME: Duration = Duration::from_secs(300);

pub(crate) struct PreparedSearchableOcr {
    pub(crate) bytes: Vec<u8>,
    pub(crate) pages: Vec<SearchableRasterPage>,
    pub(crate) expected_characters: Vec<String>,
    pub(crate) deadline: Instant,
    pub(crate) cancellation: OcrCancellation,
}

pub(crate) struct SearchableOcrCoordinator {
    service: PdfService,
    runner: Arc<OcrProcessRunner>,
}

impl SearchableOcrCoordinator {
    pub(crate) fn new(service: PdfService, runner: OcrProcessRunner) -> Self {
        Self { service, runner: Arc::new(runner) }
    }

    async fn prepare(
        &self,
        document_id: u64,
        revision: u64,
        page_count: usize,
        cancellation: OcrCancellation,
    ) -> Result<(PreparedSearchableOcr, OcrOperation), OcrProcessError> {
        if page_count == 0 || page_count > MAX_DOCUMENT_PAGES {
            return Err(OcrProcessError::Document("Searchable OCR supports 1 to 32 current pages.".into()));
        }
        let started = Instant::now();
        let deadline = started + MAX_DOCUMENT_RUNTIME;
        let mut operation = OcrOperation::try_begin(&cancellation)?;
        macro_rules! finish_try {
            ($value:expr) => {
                match $value { Ok(value) => value, Err(error) => return operation.finish(Err(error)) }
            };
        }
        let mut pages = Vec::with_capacity(page_count);
        let mut expected_characters = Vec::with_capacity(page_count);
        for index in 0..page_count {
            finish_try!(cancellation.ensure_runnable());
            let page = finish_try!(u16::try_from(index).map_err(|_| OcrProcessError::Document("Searchable OCR page is out of range.".into())));
            let request = OcrPageRequest { document_id, revision, page };
            let raster = finish_try!(self.service.ocr_raster(request, cancellation.clone(), operation.worker_hold()).await
                .map_err(OcrProcessError::Document));
            let remaining = finish_try!(MAX_DOCUMENT_RUNTIME.checked_sub(started.elapsed())
                .ok_or(OcrProcessError::TimedOut));
            let runner = self.runner.clone();
            let worker = tauri::async_runtime::spawn_blocking(move || {
                let result = runner.recognize_p6_tsv_admitted_with_timeout(&raster.p6, &operation, remaining);
                (operation, raster, result)
            }).await;
            let worker = match worker {
                Ok(worker) => worker,
                Err(error) => {
                    cancellation.cancel();
                    return Err(OcrProcessError::System(format!("OCR worker failed: {error}")));
                }
            };
            operation = worker.0;
            let raster = worker.1;
            let output = finish_try!(worker.2);
            finish_try!(cancellation.ensure_runnable());
            finish_try!(self.service.validate_ocr_result(request, operation.worker_hold()).await
                .map_err(OcrProcessError::Document));
            let parsed = finish_try!(parse_tesseract_tsv(output.text.as_bytes(), raster.width, raster.height)
                .map_err(OcrProcessError::InvalidInput));
            let displayed = finish_try!(map_words_to_display(&parsed, f64::from(raster.page_width), f64::from(raster.page_height))
                .map_err(OcrProcessError::InvalidInput));
            let rgb = finish_try!(p6_rgb(&raster.p6, raster.width, raster.height)
                .map_err(OcrProcessError::InvalidInput));
            // The characters of the recognised words, for the per-page
            // character-multiset check after the copy is reopened.
            expected_characters.push(parsed.words.iter().map(|word| word.text.as_str()).collect::<String>());
            pages.push(SearchableRasterPage {
                page_width: raster.page_width,
                page_height: raster.page_height,
                image_width: raster.width,
                image_height: raster.height,
                rgb,
                words: displayed.into_iter().map(|word| SearchableWord {
                    x: word.x, y: word.y, width: word.width, height: word.height, text: word.text,
                }).collect(),
            });
        }
        if started.elapsed() > MAX_DOCUMENT_RUNTIME { return operation.finish(Err(OcrProcessError::TimedOut)); }
        let bytes = finish_try!(write_searchable_pdf(&pages).map_err(OcrProcessError::InvalidInput));
        if Instant::now() >= deadline { return operation.finish(Err(OcrProcessError::TimedOut)); }
        finish_try!(cancellation.ensure_runnable());
        Ok((PreparedSearchableOcr { bytes, pages, expected_characters, deadline, cancellation }, operation))
    }

    pub(crate) async fn create(
        &self,
        document_id: u64,
        revision: u64,
        page_count: usize,
        path: std::path::PathBuf,
        cancellation: OcrCancellation,
    ) -> Result<crate::service::SavedCopy, OcrProcessError> {
        let (prepared, operation) = self.prepare(document_id, revision, page_count, cancellation).await?;
        let result = self.service.publish_searchable_ocr(document_id, revision, path, prepared).await.map_err(OcrProcessError::Document);
        operation.finish_committed(result)
    }
}

fn p6_rgb(input: &[u8], width: u32, height: u32) -> Result<Vec<u8>, String> {
    let header = format!("P6\n{width} {height}\n255\n");
    let pixels = input.strip_prefix(header.as_bytes()).ok_or("OCR raster P6 header is inconsistent")?;
    let expected = usize::try_from(u64::from(width).checked_mul(u64::from(height)).and_then(|value| value.checked_mul(3)).ok_or("OCR raster length overflows")?)
        .map_err(|_| "OCR raster length overflows")?;
    if pixels.len() != expected { return Err("OCR raster P6 length is inconsistent".into()); }
    Ok(pixels.to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn smoke_png(pnm: &std::path::Path, png_path: &std::path::Path) {
        let p6 = std::fs::read(pnm).unwrap();
        let mut lines = p6.splitn(4, |byte| *byte == 10);
        assert_eq!(lines.next(), Some(b"P6".as_slice()));
        let dimensions = std::str::from_utf8(lines.next().unwrap()).unwrap().split_once(' ').unwrap();
        assert_eq!(lines.next(), Some(b"255".as_slice()));
        let (width, height): (u32, u32) = (dimensions.0.parse().unwrap(), dimensions.1.parse().unwrap());
        let mut encoder = png::Encoder::new(std::fs::File::create(png_path).unwrap(), width, height);
        encoder.set_color(png::ColorType::Rgb);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().unwrap();
        writer.write_image_data(lines.next().unwrap()).unwrap();
        writer.finish().unwrap();
    }

    #[test]
    #[ignore = "needs PDF_WORKSTATION_OCR_SETUP_ROOT pointing at a scripts/setup-ocr.ps1 output root"]
    fn real_engine_creates_searchable_copies_that_pdfium_extracts() {
        let Some(setup) = crate::ocr_test_support::require_local_setup("real_engine_creates_searchable_copies_that_pdfium_extracts") else { return; };
        let _serial = crate::ocr_process::test_lock();
        let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let service = PdfService::start(root.join("resources/pdfium/bin/pdfium.dll"));
        let runner = OcrProcessRunner::new(
            setup.root.join("engine/bin/tesseract.exe"),
            setup.root.join("engine/tessdata"),
            setup.engine,
        ).unwrap();
        let coordinator = SearchableOcrCoordinator::new(service.clone(), runner);
        let folder = tempfile::tempdir().unwrap();
        let png_path = folder.path().join("known-text.png");
        smoke_png(&setup.smoke_pnm(), &png_path);
        let scan = folder.path().join("known-text-scan.pdf");
        crate::image_pdf::prepare_and_write(
            &png_path,
            &scan,
            crate::image_pdf::ImagePdfOptions {
                page_size: crate::image_pdf::ImagePdfPageSize::Letter,
                orientation: crate::image_pdf::ImagePdfOrientation::Landscape,
                margin_points: 24.0,
            },
            |_| Ok(()),
        ).unwrap();
        let cases = [
            ("known-text-scan", scan, vec!["Receipt", "#A-17:", "Coffee", "&", "Tea,", "$12.50.", "Mixed", "case:", "3rd", "Avenue;", "ready."]),
            ("repo-fixture", root.join("tests/fixtures/reportlab-plain-fields.pdf"), vec!["Independent", "AcroForm", "compatibility", "fixture"]),
        ];
        for (name, source, expected) in cases {
            let opened = tauri::async_runtime::block_on(service.open(source)).unwrap();
            let request = opened.ocr_request(0);
            let output = folder.path().join(format!("{name}-searchable.pdf"));
            tauri::async_runtime::block_on(coordinator.create(request.document_id, request.revision, 1, output.clone(), OcrCancellation::default()))
                .unwrap_or_else(|error| panic!("{name}: {error:?}"));
            let reopened = tauri::async_runtime::block_on(service.open(output)).unwrap();
            let reopened_request = reopened.ocr_request(0);
            let text = tauri::async_runtime::block_on(service.text(reopened_request.document_id, 0, reopened_request.revision)).unwrap();
            eprintln!("{name}: PDFium extracted {text:?}");
            let extracted = text.split_whitespace().collect::<std::collections::HashSet<_>>();
            for word in expected {
                assert!(extracted.contains(word), "{name}: PDFium text {text:?} lacks {word:?}");
            }
            tauri::async_runtime::block_on(service.close(request.document_id)).unwrap();
            tauri::async_runtime::block_on(service.close(reopened_request.document_id)).unwrap();
        }
    }

    /// Evidence, not a gate: each `*.png` in `SM4_SEARCHABLE_PAGES_DIR` (read
    /// only) is placed on a Letter page, then saved through the full
    /// searchable-copy path (render, engine, parse, write, reopen checks).
    #[test]
    #[ignore = "measurement: needs the OCR setup root and SM4_SEARCHABLE_PAGES_DIR"]
    fn scan_pages_save_through_the_searchable_copy_path() {
        let Some(setup) = crate::ocr_test_support::require_local_setup("scan_pages_save_through_the_searchable_copy_path") else { return; };
        let Some(input) = std::env::var_os("SM4_SEARCHABLE_PAGES_DIR") else { return; };
        let _serial = crate::ocr_process::test_lock();
        let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let service = PdfService::start(root.join("resources/pdfium/bin/pdfium.dll"));
        let runner = OcrProcessRunner::new(setup.root.join("engine/bin/tesseract.exe"), setup.root.join("engine/tessdata"), setup.engine).unwrap();
        let coordinator = SearchableOcrCoordinator::new(service.clone(), runner);
        let folder = tempfile::tempdir().unwrap();
        let mut pages = std::fs::read_dir(&input).unwrap().map(|entry| entry.unwrap().path())
            .filter(|path| path.extension().is_some_and(|extension| extension == "png")).collect::<Vec<_>>();
        pages.sort();
        let mut saved = 0;
        for png in &pages {
            let stem = png.file_stem().unwrap().to_string_lossy().into_owned();
            let scan = folder.path().join(format!("{stem}.pdf"));
            crate::image_pdf::prepare_and_write(png, &scan, crate::image_pdf::ImagePdfOptions {
                page_size: crate::image_pdf::ImagePdfPageSize::Letter,
                orientation: crate::image_pdf::ImagePdfOrientation::Auto,
                margin_points: 0.0,
            }, |_| Ok(())).unwrap();
            let opened = tauri::async_runtime::block_on(service.open(scan)).unwrap();
            let request = opened.ocr_request(0);
            let output = folder.path().join(format!("{stem}-searchable.pdf"));
            match tauri::async_runtime::block_on(coordinator.create(request.document_id, request.revision, 1, output, OcrCancellation::default())) {
                Ok(_) => { saved += 1; println!("SM4_SAVE {stem} saved"); }
                Err(error) => println!("SM4_SAVE {stem} failed: {error:?}"),
            }
            tauri::async_runtime::block_on(service.close(request.document_id)).unwrap();
        }
        println!("SM4_SAVE {saved} of {} pages saved", pages.len());
    }
}
