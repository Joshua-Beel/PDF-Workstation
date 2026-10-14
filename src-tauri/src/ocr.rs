use std::sync::Arc;

use crate::ocr_process::{
    OcrCancellation, OcrOperation, OcrProcessError, OcrProcessRunner, OCR_INPUT_LIMIT,
    OCR_MAX_EDGE,
};
use crate::service::PdfService;

const OCR_DPI: u16 = 150;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct OcrPageRequest {
    pub(crate) document_id: u64,
    pub(crate) revision: u64,
    pub(crate) page: u16,
}

#[derive(Debug, Eq, PartialEq)]
pub(crate) struct OcrPageText {
    pub(crate) document_id: u64,
    pub(crate) revision: u64,
    pub(crate) page: u16,
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) text: String,
    pub(crate) peak_job_commit_bytes: usize,
}

#[derive(Debug)]
pub(crate) struct OcrPageRaster {
    pub(crate) request: OcrPageRequest,
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) page_width: f32,
    pub(crate) page_height: f32,
    pub(crate) p6: Vec<u8>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct OcrRasterDimensions {
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) bgra_bytes: usize,
    p6_bytes: usize,
}

pub(crate) struct OcrCoordinator {
    service: PdfService,
    runner: Arc<OcrProcessRunner>,
    #[cfg(test)]
    hooks: OcrCoordinatorHooks,
}

#[cfg(test)]
#[derive(Clone, Default)]
pub(crate) struct OcrCoordinatorHooks {
    pub(crate) after_raster: Option<Arc<dyn Fn() + Send + Sync>>,
    pub(crate) before_process_resume: Option<Arc<dyn Fn() + Send + Sync>>,
    pub(crate) after_process_resume: Option<Arc<dyn Fn() + Send + Sync>>,
    pub(crate) after_process: Option<Arc<dyn Fn() + Send + Sync>>,
}

impl OcrCoordinator {
    pub(crate) fn new(service: PdfService, runner: OcrProcessRunner) -> Self {
        Self {
            service,
            runner: Arc::new(runner),
            #[cfg(test)]
            hooks: OcrCoordinatorHooks::default(),
        }
    }

    #[cfg(test)]
    pub(crate) fn with_hooks(service: PdfService, runner: OcrProcessRunner, hooks: OcrCoordinatorHooks) -> Self {
        Self { service, runner: Arc::new(runner), hooks }
    }

    pub(crate) async fn recognize_page(
        &self,
        request: OcrPageRequest,
        cancellation: OcrCancellation,
    ) -> Result<OcrPageText, OcrProcessError> {
        let operation = OcrOperation::try_begin(&cancellation)?;
        let mut cancel_on_drop = CancelOnDrop::new(cancellation.clone());
        let raster = match self.service.ocr_raster(request, cancellation.clone(), operation.worker_hold()).await {
            Ok(raster) => raster,
            Err(error) => return finish(&mut cancel_on_drop, operation, Err(OcrProcessError::Document(error))),
        };
        #[cfg(test)]
        if let Some(hook) = &self.hooks.after_raster { hook(); }
        if let Err(error) = cancellation.ensure_runnable() {
            return finish(&mut cancel_on_drop, operation, Err(error));
        }
        let runner = self.runner.clone();
        #[cfg(test)]
        let before_process_resume = self.hooks.before_process_resume.clone();
        #[cfg(test)]
        let after_process_resume = self.hooks.after_process_resume.clone();
        #[cfg(test)]
        let after_process = self.hooks.after_process.clone();
        let process = tauri::async_runtime::spawn_blocking(move || {
            let receipt = (raster.request, raster.width, raster.height);
            #[cfg(test)]
            let result = runner.recognize_p6_admitted_with_hooks(
                &raster.p6,
                &operation,
                before_process_resume.as_ref().map(|hook| &**hook as &dyn Fn()),
                after_process_resume.as_ref().map(|hook| &**hook as &dyn Fn()),
            );
            #[cfg(not(test))]
            let result = runner.recognize_p6_admitted(&raster.p6, &operation);
            drop(raster);
            #[cfg(test)]
            if let Some(hook) = after_process { hook(); }
            (operation, receipt, result)
        })
        .await;
        let (operation, (raster_request, raster_width, raster_height), process) = match process {
            Ok(value) => value,
            Err(error) => {
                cancellation.cancel();
                return Err(OcrProcessError::System(format!("OCR worker failed: {error}")));
            }
        };
        let process = match process {
            Ok(output) => output,
            Err(error) => return finish(&mut cancel_on_drop, operation, Err(error)),
        };
        if let Err(error) = cancellation.ensure_runnable() {
            return finish(&mut cancel_on_drop, operation, Err(error));
        }
        if let Err(error) = self.service.validate_ocr_result(request, operation.worker_hold()).await {
            return finish(&mut cancel_on_drop, operation, Err(OcrProcessError::Document(error)));
        }
        let result = OcrPageText {
            document_id: raster_request.document_id,
            revision: raster_request.revision,
            page: raster_request.page,
            width: raster_width,
            height: raster_height,
            text: process.text,
            peak_job_commit_bytes: process.peak_job_commit_bytes,
        };
        finish(&mut cancel_on_drop, operation, Ok(result))
    }
}

struct CancelOnDrop {
    cancellation: OcrCancellation,
    armed: bool,
}

impl CancelOnDrop {
    fn new(cancellation: OcrCancellation) -> Self { Self { cancellation, armed: true } }
    fn disarm(&mut self) { self.armed = false; }
}

impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        if self.armed { self.cancellation.cancel(); }
    }
}

fn finish<T>(
    cancel_on_drop: &mut CancelOnDrop,
    operation: OcrOperation,
    result: Result<T, OcrProcessError>,
) -> Result<T, OcrProcessError> {
    let result = operation.finish(result);
    cancel_on_drop.disarm();
    result
}

pub(crate) fn raster_dimensions(width_points: f32, height_points: f32) -> Result<OcrRasterDimensions, String> {
    if !width_points.is_finite() || !height_points.is_finite() || width_points <= 0.0 || height_points <= 0.0 {
        return Err("The page has invalid displayed dimensions for OCR.".into());
    }
    let width = (f64::from(width_points) * f64::from(OCR_DPI) / 72.0).ceil();
    let height = (f64::from(height_points) * f64::from(OCR_DPI) / 72.0).ceil();
    if !width.is_finite() || !height.is_finite() || width < 1.0 || height < 1.0
        || width > OCR_MAX_EDGE as f64 || height > OCR_MAX_EDGE as f64
    {
        return Err("The OCR image dimensions exceed the 16,384 pixel edge limit.".into());
    }
    let width = width as u32;
    let height = height as u32;
    let pixels = u64::from(width).checked_mul(u64::from(height)).ok_or("The OCR pixel count is out of range.")?;
    let bgra_bytes = pixels.checked_mul(4).ok_or("The OCR bitmap size is out of range.")?;
    let rgb_bytes = pixels.checked_mul(3).ok_or("The OCR image size is out of range.")?;
    let header_bytes = format!("P6\n{width} {height}\n255\n").len() as u64;
    let p6_bytes = header_bytes.checked_add(rgb_bytes).ok_or("The OCR input size is out of range.")?;
    if p6_bytes > OCR_INPUT_LIMIT as u64 {
        return Err("The OCR image exceeds the 16 MiB input limit.".into());
    }
    Ok(OcrRasterDimensions {
        width,
        height,
        bgra_bytes: usize::try_from(bgra_bytes).map_err(|_| "The OCR bitmap size is out of range.")?,
        p6_bytes: usize::try_from(p6_bytes).map_err(|_| "The OCR input size is out of range.")?,
    })
}

pub(crate) fn bgra_to_p6(
    dimensions: OcrRasterDimensions,
    bgra: &[u8],
    cancellation: &OcrCancellation,
) -> Result<Vec<u8>, String> {
    if bgra.len() != dimensions.bgra_bytes { return Err("Unexpected OCR bitmap layout.".into()); }
    let header = format!("P6\n{} {}\n255\n", dimensions.width, dimensions.height);
    let mut p6 = Vec::new();
    p6.try_reserve_exact(dimensions.p6_bytes).map_err(|_| "Could not allocate the bounded OCR image.".to_owned())?;
    p6.extend_from_slice(header.as_bytes());
    let row_bytes = usize::try_from(dimensions.width).map_err(|_| "The OCR row size is out of range.")?
        .checked_mul(4).ok_or("The OCR row size is out of range.")?;
    for row in bgra.chunks_exact(row_bytes) {
        if cancellation.is_cancelled() { return Err("OCR was cancelled".into()); }
        for pixel in row.chunks_exact(4) {
            let alpha = u16::from(pixel[3]);
            for channel in [pixel[2], pixel[1], pixel[0]] {
                p6.push(((u16::from(channel) * alpha + 255 * (255 - alpha) + 127) / 255) as u8);
            }
        }
    }
    if p6.len() != dimensions.p6_bytes { return Err("Unexpected OCR pixel layout.".into()); }
    Ok(p6)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};
    use std::sync::mpsc;
    use std::time::Duration;

    use crate::ocr_test_support::{local_setup, require_local_setup};

    fn root() -> PathBuf { PathBuf::from(env!("CARGO_MANIFEST_DIR")).parent().unwrap().to_path_buf() }

    fn verified_runner() -> OcrProcessRunner {
        let setup = local_setup().expect("the test must call require_local_setup first");
        let engine = setup.engine_directory();
        OcrProcessRunner::new(engine.join("bin/tesseract.exe"), engine.join("tessdata"), setup.engine).unwrap()
    }

    fn write_smoke_png(path: &Path) {
        let p6 = std::fs::read(local_setup().expect("the test must call require_local_setup first").smoke_pnm()).unwrap();
        let mut lines = p6.splitn(4, |byte| *byte == b'\n');
        assert_eq!(lines.next(), Some(b"P6".as_slice()));
        let dimensions = std::str::from_utf8(lines.next().unwrap()).unwrap().split_once(' ').unwrap();
        assert_eq!(lines.next(), Some(b"255".as_slice()));
        let width: u32 = dimensions.0.parse().unwrap(); let height: u32 = dimensions.1.parse().unwrap(); let pixels = lines.next().unwrap();
        assert_eq!(pixels.len(), width as usize * height as usize * 3);
        let file = std::fs::File::create(path).unwrap(); let mut encoder = png::Encoder::new(file, width, height);
        encoder.set_color(png::ColorType::Rgb); encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().unwrap(); writer.write_image_data(pixels).unwrap(); writer.finish().unwrap();
    }

    async fn known_edited_page(service: &PdfService, folder: &Path) -> (OcrPageRequest, PathBuf, Vec<u8>) {
        let image = folder.join("known-text.png"); write_smoke_png(&image);
        let plain = folder.join("known-plain.pdf");
        crate::image_pdf::prepare_and_write(
            &image,
            &plain,
            crate::image_pdf::ImagePdfOptions {
                page_size: crate::image_pdf::ImagePdfPageSize::Letter,
                orientation: crate::image_pdf::ImagePdfOrientation::Landscape,
                margin_points: 24.0,
            },
            |_| Ok(()),
        ).unwrap();
        let mut pdf = lopdf::Document::load(&plain).unwrap();
        let page = *pdf.get_pages().values().next().unwrap();
        pdf.get_dictionary_mut(page).unwrap().set("Rotate", 90);
        let source_path = folder.join("known-source-rotated.pdf");
        pdf.save(&source_path).unwrap();
        let source = std::fs::read(&source_path).unwrap();
        let mut info = service.open(source_path.clone()).await.unwrap();
        for _ in 0..3 {
            info = service.edit(info.ocr_request(0).document_id, crate::editor::PageEdit::Rotate { pages: vec![0], clockwise: true }, 0).await.unwrap();
        }
        info = service.crop(info.ocr_request(0).document_id, 0, info.ocr_request(0).revision, crate::service::CropRect { x: 0.01, y: 0.01, width: 0.98, height: 0.98 }, 0).await.unwrap();
        info = service.create_highlight(info.ocr_request(0).document_id, info.ocr_request(0).revision, 0, crate::service::CropRect { x: 0.02, y: 0.02, width: 0.03, height: 0.03 }, Some("Retained OCR note".into()), 0).await.unwrap();
        (info.ocr_request(0), source_path, source)
    }

    fn blocking_hook() -> (Arc<dyn Fn() + Send + Sync>, mpsc::Receiver<()>, mpsc::SyncSender<()>) {
        let (ready, ready_receiver) = mpsc::sync_channel(0);
        let (release, release_receiver) = mpsc::sync_channel(0);
        let release_receiver = std::sync::Mutex::new(release_receiver);
        let hook = Arc::new(move || {
            ready.send(()).unwrap();
            release_receiver.lock().unwrap().recv().unwrap();
        });
        (hook, ready_receiver, release)
    }

    #[test]
    fn raster_dimensions_and_white_alpha_composition_are_exact() {
        let _ocr_guard = crate::ocr_process::test_lock();
        let dimensions = raster_dimensions(612.0, 792.0).unwrap();
        assert_eq!((dimensions.width, dimensions.height), (1275, 1650));
        assert_eq!(dimensions.bgra_bytes, 8_415_000);
        assert_eq!(dimensions.p6_bytes, 6_311_267);
        let dimensions = raster_dimensions(0.96, 0.48).unwrap();
        assert_eq!((dimensions.width, dimensions.height), (2, 1));
        let p6 = bgra_to_p6(dimensions, &[0, 0, 255, 128, 255, 0, 0, 255], &OcrCancellation::default()).unwrap();
        assert_eq!(p6, b"P6\n2 1\n255\n\xff\x7f\x7f\x00\x00\xff");
        assert!(raster_dimensions(f32::NAN, 1.0).is_err());
        assert!(raster_dimensions(16_385.0 * 72.0 / 150.0, 1.0).is_err());
        assert!(raster_dimensions(10_000.0, 10_000.0).is_err());
    }

    #[test]
    #[ignore = "needs PDF_WORKSTATION_OCR_SETUP_ROOT pointing at a scripts/setup-ocr.ps1 output root"]
    fn retained_engine_recognizes_the_tagged_current_worker_snapshot() {
        if require_local_setup("retained_engine_recognizes_the_tagged_current_worker_snapshot").is_none() { return; }
        let _ocr_guard = crate::ocr_process::test_lock();
        let service = PdfService::start(root().join("src-tauri/resources/pdfium/bin/pdfium.dll"));
        let folder = tempfile::tempdir().unwrap();
        let (request, source_path, source) = tauri::async_runtime::block_on(known_edited_page(&service, folder.path()));
        let raster_cancellation = OcrCancellation::default();
        let raster_operation = OcrOperation::try_begin(&raster_cancellation).unwrap();
        let raster = tauri::async_runtime::block_on(service.ocr_raster(request, raster_cancellation, raster_operation.worker_hold())).unwrap();
        let raster = raster_operation.finish(Ok(raster)).unwrap();
        let probe = root().join("target/ocr-page-probe"); std::fs::create_dir_all(&probe).unwrap();
        std::fs::write(probe.join("current-edited-page-150dpi.pnm"), &raster.p6).unwrap();

        let coordinator = OcrCoordinator::new(service.clone(), verified_runner());
        let result = tauri::async_runtime::block_on(coordinator.recognize_page(request, OcrCancellation::default())).unwrap();
        let normalized = result.text.trim().replace("\r\n", "\n");
        assert_eq!(normalized, "Receipt #A-17: Coffee & Tea, $12.50.\nMixed case: 3rd Avenue; ready.");
        assert_eq!((result.document_id, result.revision, result.page, result.width, result.height), (request.document_id, request.revision, request.page, raster.width, raster.height));
        assert!(result.peak_job_commit_bytes <= crate::ocr_process::OCR_CHILD_COMMIT_LIMIT);
        std::fs::write(probe.join("current-edited-page-receipt.json"), serde_json::to_vec_pretty(&serde_json::json!({
            "documentId": result.document_id, "revision": result.revision, "page": result.page,
            "width": result.width, "height": result.height, "dpi": 150,
            "text": result.text, "peakJobCommitBytes": result.peak_job_commit_bytes,
        })).unwrap()).unwrap();
        assert_eq!(std::fs::read(&source_path).unwrap(), source);
        let immediate = OcrOperation::try_begin(&OcrCancellation::default()).unwrap(); immediate.finish(Ok(())).unwrap();
        tauri::async_runtime::block_on(service.close(request.document_id)).unwrap();
        println!("OCR_PAGE_PROBE p6={} receipt={}", probe.join("current-edited-page-150dpi.pnm").display(), probe.join("current-edited-page-receipt.json").display());
    }

    #[test]
    #[ignore = "needs PDF_WORKSTATION_OCR_SETUP_ROOT pointing at a scripts/setup-ocr.ps1 output root"]
    fn retained_engine_result_rechecks_stale_and_closed_documents() {
        if require_local_setup("retained_engine_result_rechecks_stale_and_closed_documents").is_none() { return; }
        let _ocr_guard = crate::ocr_process::test_lock();
        let service = PdfService::start(root().join("src-tauri/resources/pdfium/bin/pdfium.dll"));
        let folder = tempfile::tempdir().unwrap();
        let (request, _, _) = tauri::async_runtime::block_on(known_edited_page(&service, folder.path()));
        let (hook, ready, release) = blocking_hook();
        let coordinator = Arc::new(OcrCoordinator::with_hooks(service.clone(), verified_runner(), OcrCoordinatorHooks { after_process: Some(hook), ..Default::default() }));
        let task = tauri::async_runtime::spawn({ let coordinator = coordinator.clone(); async move { coordinator.recognize_page(request, OcrCancellation::default()).await } });
        ready.recv_timeout(Duration::from_secs(30)).unwrap();
        tauri::async_runtime::block_on(service.edit(request.document_id, crate::editor::PageEdit::Rotate { pages: vec![0], clockwise: true }, 0)).unwrap();
        release.send(()).unwrap();
        let error = tauri::async_runtime::block_on(task).unwrap().unwrap_err();
        assert!(matches!(error, OcrProcessError::Document(ref message) if message.contains("changed")));
        tauri::async_runtime::block_on(service.close(request.document_id)).unwrap();

        let second_folder = tempfile::tempdir().unwrap();
        let (request, _, _) = tauri::async_runtime::block_on(known_edited_page(&service, second_folder.path()));
        let (hook, ready, release) = blocking_hook();
        let coordinator = Arc::new(OcrCoordinator::with_hooks(service.clone(), verified_runner(), OcrCoordinatorHooks { after_process: Some(hook), ..Default::default() }));
        let task = tauri::async_runtime::spawn({ let coordinator = coordinator.clone(); async move { coordinator.recognize_page(request, OcrCancellation::default()).await } });
        ready.recv_timeout(Duration::from_secs(30)).unwrap();
        tauri::async_runtime::block_on(service.close(request.document_id)).unwrap();
        release.send(()).unwrap();
        let error = tauri::async_runtime::block_on(task).unwrap().unwrap_err();
        assert!(matches!(error, OcrProcessError::Document(ref message) if message.contains("closed")));
    }

    #[test]
    #[ignore = "needs PDF_WORKSTATION_OCR_SETUP_ROOT pointing at a scripts/setup-ocr.ps1 output root"]
    fn retained_engine_cancellation_and_aborted_coordinator_release_every_resource() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        if require_local_setup("retained_engine_cancellation_and_aborted_coordinator_release_every_resource").is_none() { return; }
        let _ocr_guard = crate::ocr_process::test_lock();
        let service = PdfService::start(root().join("src-tauri/resources/pdfium/bin/pdfium.dll"));
        let folder = tempfile::tempdir().unwrap();
        let (request, _, _) = tauri::async_runtime::block_on(known_edited_page(&service, folder.path()));

        let (after_raster, ready, release) = blocking_hook();
        let process_starts = Arc::new(AtomicUsize::new(0));
        let before_resume: Arc<dyn Fn() + Send + Sync> = { let process_starts = process_starts.clone(); Arc::new(move || { process_starts.fetch_add(1, Ordering::SeqCst); }) };
        let coordinator = Arc::new(OcrCoordinator::with_hooks(service.clone(), verified_runner(), OcrCoordinatorHooks { after_raster: Some(after_raster), before_process_resume: Some(before_resume), after_process_resume: None, after_process: None }));
        let cancellation = OcrCancellation::default();
        let task = tauri::async_runtime::spawn({ let coordinator = coordinator.clone(); let cancellation = cancellation.clone(); async move { coordinator.recognize_page(request, cancellation).await } });
        ready.recv_timeout(Duration::from_secs(30)).unwrap();
        assert!(cancellation.cancel());
        release.send(()).unwrap();
        assert_eq!(tauri::async_runtime::block_on(task).unwrap(), Err(OcrProcessError::Cancelled));
        assert_eq!(process_starts.load(Ordering::SeqCst), 0, "cancellation after raster must win before process resume");

        let (after_process, ready, release) = blocking_hook();
        let coordinator = Arc::new(OcrCoordinator::with_hooks(service.clone(), verified_runner(), OcrCoordinatorHooks { after_process: Some(after_process), ..Default::default() }));
        let cancellation = OcrCancellation::default();
        let task = tauri::async_runtime::spawn({ let coordinator = coordinator.clone(); let cancellation = cancellation.clone(); async move { coordinator.recognize_page(request, cancellation).await } });
        ready.recv_timeout(Duration::from_secs(30)).unwrap();
        assert!(cancellation.cancel());
        release.send(()).unwrap();
        assert_eq!(tauri::async_runtime::block_on(task).unwrap(), Err(OcrProcessError::Cancelled));

        let (after_resume, ready, release) = blocking_hook();
        let (finished, finished_receiver) = mpsc::sync_channel(0);
        let after_process: Arc<dyn Fn() + Send + Sync> = Arc::new(move || { finished.send(()).unwrap(); });
        let coordinator = Arc::new(OcrCoordinator::with_hooks(service.clone(), verified_runner(), OcrCoordinatorHooks { after_process_resume: Some(after_resume), after_process: Some(after_process), ..Default::default() }));
        let cancellation = OcrCancellation::default();
        let task = tauri::async_runtime::spawn({ let coordinator = coordinator.clone(); let cancellation = cancellation.clone(); async move { coordinator.recognize_page(request, cancellation).await } });
        ready.recv_timeout(Duration::from_secs(30)).unwrap();
        task.abort();
        assert!(tauri::async_runtime::block_on(task).is_err());
        assert!(!cancellation.cancel(), "aborting the coordinator must arm cancellation for the detached blocking child");
        assert!(matches!(OcrOperation::try_begin(&OcrCancellation::default()), Err(OcrProcessError::Busy)));
        release.send(()).unwrap();
        finished_receiver.recv_timeout(Duration::from_secs(30)).unwrap();
        let mut admitted = None;
        for _ in 0..100_000 {
            match OcrOperation::try_begin(&OcrCancellation::default()) {
                Ok(operation) => { admitted = Some(operation); break; }
                Err(OcrProcessError::Busy) => std::thread::yield_now(),
                Err(error) => panic!("unexpected admission result: {error}"),
            }
        }
        admitted.expect("detached canceled OCR must release admission after child reap").finish(Ok(())).unwrap();
        tauri::async_runtime::block_on(service.close(request.document_id)).unwrap();
    }
}
