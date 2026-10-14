use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde::Serialize;

use crate::ocr::{OcrCoordinator, OcrPageRequest, OcrPageText};
use crate::ocr_process::{OcrCancellation, OcrProcessError, OcrProcessRunner};
#[cfg(ocr_opt_in)]
use crate::ocr_process::OcrExecutableIdentity;
use crate::service::PdfService;
use crate::searchable_ocr_job::SearchableOcrCoordinator;

const OCR_REQUEST_LIMIT: usize = 4_096;
const OCR_DPI: u16 = 150;
const UNAVAILABLE_DEFAULT: &str = "OCR is not enabled in this source build.";
const UNAVAILABLE_RESOURCES: &str = "OCR resources could not be verified for this source build.";
const REQUEST_LIMIT_ERROR: &str = "The OCR request limit was reached. Restart the application before starting another OCR request.";

fn available_capability(searchable: bool, searchable_reason: Option<String>) -> OcrCapability {
    OcrCapability {
        available: true, reason: None, language: Some("eng".into()),
        searchable_pdf_available: searchable, searchable_pdf_reason: searchable_reason,
        searchable_pdf_dpi: searchable.then_some(150), searchable_pdf_max_pages: 32,
        searchable_pdf_max_pixels: crate::searchable_pdf::MAX_SEARCHABLE_PIXELS,
        searchable_pdf_max_words: crate::searchable_pdf::MAX_SEARCHABLE_WORDS,
        searchable_pdf_max_text_bytes: crate::searchable_pdf::MAX_SEARCHABLE_TEXT_BYTES,
        searchable_pdf_characters: searchable.then(|| "printable ASCII words only".into()),
    }
}

fn unavailable_capability(reason: &str) -> OcrCapability {
    OcrCapability {
        available: false, reason: Some(reason.into()), language: None,
        searchable_pdf_available: false, searchable_pdf_reason: Some(reason.into()), searchable_pdf_dpi: None,
        searchable_pdf_max_pages: 32, searchable_pdf_max_pixels: crate::searchable_pdf::MAX_SEARCHABLE_PIXELS,
        searchable_pdf_max_words: crate::searchable_pdf::MAX_SEARCHABLE_WORDS,
        searchable_pdf_max_text_bytes: crate::searchable_pdf::MAX_SEARCHABLE_TEXT_BYTES,
        searchable_pdf_characters: None,
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct OcrCapability {
    available: bool,
    reason: Option<String>,
    language: Option<String>,
    searchable_pdf_available: bool,
    searchable_pdf_reason: Option<String>,
    searchable_pdf_dpi: Option<u16>,
    searchable_pdf_max_pages: usize,
    searchable_pdf_max_pixels: u64,
    searchable_pdf_max_words: usize,
    searchable_pdf_max_text_bytes: usize,
    searchable_pdf_characters: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct OcrReceipt {
    status: &'static str,
    request_id: String,
    document_id: u64,
    revision: u64,
    page: u16,
    dpi: u16,
    language: &'static str,
    width: u32,
    height: u32,
    text: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct OcrCancelAck {
    request_id: String,
    status: &'static str,
}

#[derive(Clone)]
pub(crate) struct OcrCommands {
    inner: Arc<OcrCommandsInner>,
}

struct OcrCommandsInner {
    coordinator: Option<Arc<OcrCoordinator>>,
    searchable: Option<Arc<SearchableOcrCoordinator>>,
    capability: OcrCapability,
    registry: Mutex<RequestRegistry>,
    #[cfg(test)]
    completion_hook: Option<Arc<dyn Fn() + Send + Sync>>,
}

#[derive(Default)]
struct RequestRegistry {
    requests: HashMap<String, RequestState>,
    active: Option<String>,
}

enum RequestState {
    Active(OcrCancellation),
    Cancelled,
    Finished,
}

impl OcrCommands {
    pub(crate) fn new(service: PdfService, resource_dir: PathBuf) -> Self {
        let (coordinator, capability) = match configured_runner(&resource_dir) {
            Ok(Some(runner)) => {
                let searchable_runner = configured_runner(&resource_dir).ok().flatten();
                let searchable_available = searchable_runner.is_some();
                let searchable_reason = (!searchable_available).then(|| UNAVAILABLE_RESOURCES.into());
                let searchable = searchable_runner.map(|runner| Arc::new(SearchableOcrCoordinator::new(service.clone(), runner)));
                return Self { inner: Arc::new(OcrCommandsInner {
                    coordinator: Some(Arc::new(OcrCoordinator::new(service, runner))), searchable,
                    capability: available_capability(searchable_available, searchable_reason), registry: Mutex::new(RequestRegistry::default()),
                    #[cfg(test)] completion_hook: None,
                }) };
            }
            Ok(None) => (
                None,
                unavailable_capability(UNAVAILABLE_DEFAULT),
            ),
            Err(()) => (
                None,
                unavailable_capability(UNAVAILABLE_RESOURCES),
            ),
        };
        Self {
            inner: Arc::new(OcrCommandsInner {
                coordinator,
                searchable: None,
                capability,
                registry: Mutex::new(RequestRegistry::default()),
                #[cfg(test)]
                completion_hook: None,
            }),
        }
    }

    #[cfg(all(test, ocr_opt_in))]
    fn with_coordinator(coordinator: OcrCoordinator, completion_hook: Option<Arc<dyn Fn() + Send + Sync>>) -> Self {
        Self {
            inner: Arc::new(OcrCommandsInner {
                coordinator: Some(Arc::new(coordinator)),
                searchable: None,
                capability: available_capability(false, Some(UNAVAILABLE_RESOURCES.into())),
                registry: Mutex::new(RequestRegistry::default()),
                completion_hook,
            }),
        }
    }

    pub(crate) fn capability(&self) -> OcrCapability {
        self.inner.capability.clone()
    }

    pub(crate) async fn recognize(
        &self,
        request_id: String,
        document_id: u64,
        revision: u64,
        page: u16,
    ) -> Result<OcrReceipt, String> {
        validate_request_id(&request_id)?;
        let coordinator = self.inner.coordinator.clone().ok_or_else(|| {
            self.inner.capability.reason.clone().unwrap_or_else(|| UNAVAILABLE_DEFAULT.into())
        })?;
        let cancellation = self.begin(&request_id)?;
        let mut reply_drop = ReplyDropCancellation::new(cancellation.clone());
        let cleanup = ActiveRequestCleanup { inner: self.inner.clone(), request_id: request_id.clone() };
        let request = OcrPageRequest { document_id, revision, page };
        let task = tauri::async_runtime::spawn(async move {
            let _cleanup = cleanup;
            coordinator.recognize_page(request, cancellation).await
        });
        let result = task.await.map_err(|_| "OCR failed internally.".to_owned())?;
        reply_drop.disarm();
        result.map(|result| receipt(request_id, result)).map_err(public_error)
    }

    pub(crate) async fn create_searchable(
        &self, request_id: String, document_id: u64, revision: u64, page_count: usize, path: PathBuf,
    ) -> Result<crate::service::SavedCopy, String> {
        validate_uuid(&request_id)?;
        let coordinator = self.inner.searchable.clone().ok_or_else(|| self.inner.capability.searchable_pdf_reason.clone().unwrap_or_else(|| UNAVAILABLE_DEFAULT.into()))?;
        let cancellation = self.begin(&request_id)?;
        let mut reply_drop = ReplyDropCancellation::new(cancellation.clone());
        let cleanup = ActiveRequestCleanup { inner: self.inner.clone(), request_id };
        let task = tauri::async_runtime::spawn(async move { let _cleanup = cleanup; coordinator.create(document_id, revision, page_count, path, cancellation).await });
        let result = task.await.map_err(|_| "OCR failed internally.".to_owned())?;
        reply_drop.disarm();
        result.map_err(public_error)
    }

    pub(crate) fn cancel(&self, request_id: String) -> Result<OcrCancelAck, String> {
        validate_request_id(&request_id)?;
        let mut registry = self.inner.registry.lock().map_err(|_| "OCR request state is unavailable.".to_owned())?;
        let status = match registry.requests.get_mut(&request_id) {
            Some(RequestState::Active(cancellation)) => {
                if cancellation.cancel() {
                    *registry.requests.get_mut(&request_id).expect("active OCR request disappeared") = RequestState::Cancelled;
                    "cancelled"
                } else if cancellation.is_completed() {
                    *registry.requests.get_mut(&request_id).expect("active OCR request disappeared") = RequestState::Finished;
                    "finished"
                } else {
                    "already_cancelled"
                }
            }
            Some(RequestState::Cancelled) => "already_cancelled",
            Some(RequestState::Finished) => "finished",
            None => {
                ensure_registry_capacity(&registry)?;
                registry.requests.insert(request_id.clone(), RequestState::Cancelled);
                "cancelled"
            }
        };
        Ok(OcrCancelAck { request_id, status })
    }

    pub(crate) fn cancel_active(&self) {
        let Ok(mut registry) = self.inner.registry.lock() else { return; };
        let Some(request_id) = registry.active.clone() else { return; };
        if let Some(RequestState::Active(cancellation)) = registry.requests.get_mut(&request_id) {
            let state = if cancellation.cancel() || cancellation.is_cancelled() {
                RequestState::Cancelled
            } else {
                RequestState::Finished
            };
            registry.requests.insert(request_id, state);
        }
    }

    fn begin(&self, request_id: &str) -> Result<OcrCancellation, String> {
        let mut registry = self.inner.registry.lock().map_err(|_| "OCR request state is unavailable.".to_owned())?;
        if let Some(state) = registry.requests.get(request_id) {
            return Err(match state {
                RequestState::Active(_) => "This OCR request is already running.",
                RequestState::Cancelled => "This OCR request was already cancelled.",
                RequestState::Finished => "This OCR request was already completed.",
            }.into());
        }
        if registry.active.is_some() { return Err("OCR is already running.".into()); }
        ensure_registry_capacity(&registry)?;
        let cancellation = OcrCancellation::default();
        registry.requests.insert(request_id.into(), RequestState::Active(cancellation.clone()));
        registry.active = Some(request_id.into());
        Ok(cancellation)
    }
}

impl OcrCommandsInner {
    fn complete(&self, request_id: &str) {
        let Ok(mut registry) = self.registry.lock() else { return; };
        if registry.active.as_deref() == Some(request_id) { registry.active = None; }
        if let Some(RequestState::Active(cancellation)) = registry.requests.get(request_id) {
            let state = if cancellation.is_cancelled() { RequestState::Cancelled } else { RequestState::Finished };
            registry.requests.insert(request_id.into(), state);
        }
        drop(registry);
        #[cfg(test)]
        if let Some(hook) = &self.completion_hook { hook(); }
    }
}

struct ActiveRequestCleanup {
    inner: Arc<OcrCommandsInner>,
    request_id: String,
}

impl Drop for ActiveRequestCleanup {
    fn drop(&mut self) { self.inner.complete(&self.request_id); }
}

struct ReplyDropCancellation {
    cancellation: OcrCancellation,
    armed: bool,
}

impl ReplyDropCancellation {
    fn new(cancellation: OcrCancellation) -> Self { Self { cancellation, armed: true } }
    fn disarm(&mut self) { self.armed = false; }
}

impl Drop for ReplyDropCancellation {
    fn drop(&mut self) {
        if self.armed { self.cancellation.cancel(); }
    }
}

fn receipt(request_id: String, result: OcrPageText) -> OcrReceipt {
    let status = if result.text.trim().is_empty() { "no_text" } else { "recognized" };
    OcrReceipt {
        status,
        request_id,
        document_id: result.document_id,
        revision: result.revision,
        page: result.page,
        dpi: OCR_DPI,
        language: "eng",
        width: result.width,
        height: result.height,
        text: result.text,
    }
}

fn public_error(error: OcrProcessError) -> String {
    match error {
        OcrProcessError::Busy => "OCR is already running.".into(),
        OcrProcessError::Cancelled => "OCR was cancelled.".into(),
        OcrProcessError::TimedOut => "OCR exceeded its time limit.".into(),
        OcrProcessError::OutputLimit(_) => "OCR produced too much text.".into(),
        OcrProcessError::InvalidUtf8(_) => "OCR returned invalid text.".into(),
        OcrProcessError::Document(message) => message,
        OcrProcessError::InvalidInput(_) => "The current page could not be prepared for OCR.".into(),
        OcrProcessError::InvalidAsset(_)
        | OcrProcessError::AssetIntegrity(_)
        | OcrProcessError::UnexpectedStderr(_)
        | OcrProcessError::ChildExit { .. }
        | OcrProcessError::System(_) => "OCR failed internally.".into(),
    }
}

fn ensure_registry_capacity(registry: &RequestRegistry) -> Result<(), String> {
    if registry.requests.len() >= OCR_REQUEST_LIMIT { Err(REQUEST_LIMIT_ERROR.into()) } else { Ok(()) }
}

fn validate_request_id(request_id: &str) -> Result<(), String> {
    if request_id.is_empty() || request_id.len() > 64
        || !request_id.bytes().all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
    {
        return Err("OCR request ID must contain 1 to 64 ASCII letters, numbers, underscores, or hyphens.".into());
    }
    Ok(())
}

fn validate_uuid(value: &str) -> Result<(), String> {
    let bytes = value.as_bytes();
    let valid = bytes.len() == 36
        && [8, 13, 18, 23].into_iter().all(|index| bytes[index] == b'-')
        && bytes.iter().enumerate().all(|(index, byte)| [8, 13, 18, 23].contains(&index) || byte.is_ascii_hexdigit())
        && bytes[14] == b'4'
        && matches!(bytes[19].to_ascii_lowercase(), b'8' | b'9' | b'a' | b'b');
    if !valid { return Err("Searchable OCR request ID must be a fresh UUID v4.".into()); }
    Ok(())
}

#[cfg(ocr_opt_in)]
fn configured_runner(resource_dir: &Path) -> Result<Option<OcrProcessRunner>, ()> {
    let relative = option_env!("PDF_WORKSTATION_OCR_RESOURCE_RELATIVE").ok_or(())?;
    let bytes = option_env!("PDF_WORKSTATION_OCR_ENGINE_BYTES").ok_or(())?.parse::<u64>().map_err(|_| ())?;
    let sha256 = parse_sha256(option_env!("PDF_WORKSTATION_OCR_ENGINE_SHA256").ok_or(())?).ok_or(())?;
    let root = resource_dir.join(relative);
    OcrProcessRunner::new(
        root.join("bin/tesseract.exe"),
        root.join("tessdata"),
        OcrExecutableIdentity { bytes, sha256 },
    ).map(Some).map_err(|_| ())
}

#[cfg(not(ocr_opt_in))]
fn configured_runner(_resource_dir: &Path) -> Result<Option<OcrProcessRunner>, ()> { Ok(None) }

#[cfg(ocr_opt_in)]
fn parse_sha256(value: &str) -> Option<[u8; 32]> {
    if value.len() != 64 { return None; }
    let mut output = [0u8; 32];
    for (index, chunk) in value.as_bytes().chunks_exact(2).enumerate() {
        let text = std::str::from_utf8(chunk).ok()?;
        output[index] = u8::from_str_radix(text, 16).ok()?;
    }
    Some(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(ocr_opt_in)]
    use crate::ocr::OcrCoordinatorHooks;
    #[cfg(ocr_opt_in)]
    use std::path::PathBuf;
    #[cfg(ocr_opt_in)]
    use std::sync::mpsc;
    #[cfg(ocr_opt_in)]
    use std::time::Duration;

    fn unavailable() -> OcrCommands {
        OcrCommands {
            inner: Arc::new(OcrCommandsInner {
                coordinator: None,
                searchable: None,
                capability: unavailable_capability(UNAVAILABLE_DEFAULT),
                registry: Mutex::new(RequestRegistry::default()),
                completion_hook: None,
            }),
        }
    }

    #[test]
    fn cancel_before_start_tombstones_replays_and_is_idempotent() {
        let commands = unavailable();
        assert_eq!(commands.cancel("request-a".into()).unwrap().status, "cancelled");
        assert_eq!(commands.cancel("request-a".into()).unwrap().status, "already_cancelled");
        assert_eq!(commands.begin("request-a").err().unwrap(), "This OCR request was already cancelled.");
        assert!(commands.inner.registry.lock().unwrap().active.is_none());
    }

    #[test]
    fn active_busy_cancel_and_completion_keep_request_ownership() {
        let commands = unavailable();
        let cancellation = commands.begin("request-a").unwrap();
        assert_eq!(commands.begin("request-b").err().unwrap(), "OCR is already running.");
        assert_eq!(commands.cancel("request-b".into()).unwrap().status, "cancelled");
        assert!(!cancellation.is_cancelled());
        assert_eq!(commands.cancel("request-a".into()).unwrap().status, "cancelled");
        assert!(cancellation.is_cancelled());
        commands.inner.complete("request-a");
        assert_eq!(commands.cancel("request-a".into()).unwrap().status, "already_cancelled");
        assert_eq!(commands.begin("request-a").err().unwrap(), "This OCR request was already cancelled.");
        assert_eq!(commands.begin("request-b").err().unwrap(), "This OCR request was already cancelled.");

        let operation = commands.begin("request-c").unwrap();
        commands.inner.complete("request-c");
        assert!(!operation.is_cancelled());
        assert_eq!(commands.cancel("request-c".into()).unwrap().status, "finished");
        assert_eq!(commands.begin("request-c").err().unwrap(), "This OCR request was already completed.");
    }

    #[test]
    fn request_registry_is_bounded_without_eviction() {
        let commands = unavailable();
        for index in 0..OCR_REQUEST_LIMIT {
            assert_eq!(commands.cancel(format!("request-{index}" )).unwrap().status, "cancelled");
        }
        assert_eq!(commands.cancel("one-too-many".into()).unwrap_err(), REQUEST_LIMIT_ERROR);
        assert_eq!(commands.cancel("request-0".into()).unwrap().status, "already_cancelled");
        assert_eq!(commands.begin("one-too-many").err().unwrap(), REQUEST_LIMIT_ERROR);
    }

    #[test]
    fn request_ids_are_bounded_ascii_tokens() {
        for valid in ["a", "request_1", "550e8400-e29b-41d4-a716-446655440000", &"x".repeat(64)] {
            assert!(validate_request_id(valid).is_ok());
        }
        for invalid in ["", "contains space", "../path", "é", &"x".repeat(65)] {
            assert!(validate_request_id(invalid).is_err());
        }
    }

    #[test]
    fn searchable_requests_require_uuid_v4_ids() {
        for valid in [
            "550e8400-e29b-41d4-a716-446655440000",
            "550E8400-E29B-41D4-B716-446655440000",
        ] {
            assert!(validate_uuid(valid).is_ok());
        }
        for invalid in [
            "request_1",
            "550e8400-e29b-11d4-a716-446655440000",
            "550e8400-e29b-41d4-c716-446655440000",
            "550e8400e29b41d4a716446655440000",
        ] {
            assert!(validate_uuid(invalid).is_err());
        }
    }

    #[test]
    fn public_errors_do_not_disclose_child_or_asset_details() {
        assert_eq!(public_error(OcrProcessError::ChildExit { code: 9, stderr: "C:\\secret\\model".into() }), "OCR failed internally.");
        assert_eq!(public_error(OcrProcessError::AssetIntegrity("C:\\secret\\engine".into())), "OCR failed internally.");
        assert_eq!(public_error(OcrProcessError::UnexpectedStderr("private".into())), "OCR failed internally.");
    }

    #[cfg(not(ocr_opt_in))]
    #[test]
    fn default_compile_ignores_stale_opt_in_resources() {
        let resource_dir = tempfile::tempdir().unwrap();
        let stale = resource_dir.path().join("resources/ocr/stale/bin");
        std::fs::create_dir_all(&stale).unwrap();
        std::fs::write(stale.join("tesseract.exe"), b"stale opt-in resource").unwrap();
        assert!(configured_runner(resource_dir.path()).unwrap().is_none());
    }

    #[cfg(ocr_opt_in)]
    fn root() -> PathBuf { PathBuf::from(env!("CARGO_MANIFEST_DIR")).parent().unwrap().to_path_buf() }

    #[cfg(ocr_opt_in)]
    fn profile_dir() -> PathBuf {
        let exe = std::env::current_exe().unwrap().canonicalize().unwrap();
        let deps = exe.parent().unwrap();
        assert_eq!(deps.file_name().and_then(|name| name.to_str()), Some("deps"), "the test binary is expected under <target>/<profile>/deps");
        deps.parent().unwrap().to_path_buf()
    }

    #[cfg(ocr_opt_in)]
    fn runtime_runner() -> OcrProcessRunner {
        configured_runner(&profile_dir()).unwrap().expect("opt-in OCR resources")
    }

    #[cfg(ocr_opt_in)]
    fn write_smoke_png(path: &Path) {
        let setup = crate::ocr_test_support::local_setup().expect("set PDF_WORKSTATION_OCR_SETUP_ROOT to the setup root used for this opt-in build");
        let p6 = std::fs::read(setup.smoke_pnm()).unwrap();
        let mut lines = p6.splitn(4, |byte| *byte == b'\n');
        assert_eq!(lines.next(), Some(b"P6".as_slice()));
        let dimensions = std::str::from_utf8(lines.next().unwrap()).unwrap().split_once(' ').unwrap();
        assert_eq!(lines.next(), Some(b"255".as_slice()));
        let width: u32 = dimensions.0.parse().unwrap();
        let height: u32 = dimensions.1.parse().unwrap();
        let pixels = lines.next().unwrap();
        assert_eq!(pixels.len(), width as usize * height as usize * 3);
        let file = std::fs::File::create(path).unwrap();
        let mut encoder = png::Encoder::new(file, width, height);
        encoder.set_color(png::ColorType::Rgb);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().unwrap();
        writer.write_image_data(pixels).unwrap();
        writer.finish().unwrap();
    }

    #[cfg(ocr_opt_in)]
    async fn known_page(service: &PdfService, folder: &Path) -> (OcrPageRequest, PathBuf, Vec<u8>) {
        let image = folder.join("known-text.png");
        write_smoke_png(&image);
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
        info = service.create_highlight(info.ocr_request(0).document_id, info.ocr_request(0).revision, 0, crate::service::CropRect { x: 0.02, y: 0.02, width: 0.03, height: 0.03 }, Some("Command OCR note".into()), 0).await.unwrap();
        (info.ocr_request(0), source_path, source)
    }

    #[cfg(ocr_opt_in)]
    fn blocking_hook() -> (Arc<dyn Fn() + Send + Sync>, mpsc::Receiver<()>, mpsc::SyncSender<()>) {
        let (ready, ready_receiver) = mpsc::sync_channel(0);
        let (release, release_receiver) = mpsc::sync_channel(0);
        let release_receiver = Mutex::new(release_receiver);
        let hook = Arc::new(move || {
            ready.send(()).unwrap();
            release_receiver.lock().unwrap().recv().unwrap();
        });
        (hook, ready_receiver, release)
    }

    #[cfg(ocr_opt_in)]
    fn completion_hook() -> (Arc<dyn Fn() + Send + Sync>, mpsc::Receiver<()>) {
        let (finished, receiver) = mpsc::sync_channel(1);
        (Arc::new(move || { finished.send(()).unwrap(); }), receiver)
    }

    #[cfg(ocr_opt_in)]
    #[test]
    #[ignore = "uses opt-in resources produced by scripts/setup-ocr.ps1"]
    fn retained_command_recognizes_exact_owned_page_and_preserves_source() {
        let _ocr_guard = crate::ocr_process::test_lock();
        let service = PdfService::start(root().join("src-tauri/resources/pdfium/bin/pdfium.dll"));
        let folder = tempfile::tempdir().unwrap();
        let (request, source_path, source) = tauri::async_runtime::block_on(known_page(&service, folder.path()));
        let commands = OcrCommands::new(service.clone(), profile_dir());
        assert_eq!(commands.capability(), available_capability(true, None));
        let receipt = tauri::async_runtime::block_on(commands.recognize(
            "command-exact".into(), request.document_id, request.revision, request.page,
        )).unwrap();
        assert_eq!((receipt.request_id.as_str(), receipt.document_id, receipt.revision, receipt.page, receipt.dpi, receipt.language),
            ("command-exact", request.document_id, request.revision, request.page, 150, "eng"));
        assert_eq!(receipt.status, "recognized");
        assert_eq!(receipt.text.trim().replace("\r\n", "\n"), "Receipt #A-17: Coffee & Tea, $12.50.\nMixed case: 3rd Avenue; ready.");
        assert_eq!(std::fs::read(&source_path).unwrap(), source);
        assert_eq!(commands.cancel("command-exact".into()).unwrap().status, "finished");
        tauri::async_runtime::block_on(service.close(request.document_id)).unwrap();
    }

    #[cfg(ocr_opt_in)]
    #[test]
    #[ignore = "uses opt-in resources produced by scripts/setup-ocr.ps1"]
    fn retained_command_stale_and_closed_results_are_terminal_and_release_registry() {
        let _ocr_guard = crate::ocr_process::test_lock();
        for close in [false, true] {
            let service = PdfService::start(root().join("src-tauri/resources/pdfium/bin/pdfium.dll"));
            let folder = tempfile::tempdir().unwrap();
            let (request, _, _) = tauri::async_runtime::block_on(known_page(&service, folder.path()));
            let (after_process, ready, release) = blocking_hook();
            let (completed, completed_receiver) = completion_hook();
            let coordinator = OcrCoordinator::with_hooks(service.clone(), runtime_runner(), OcrCoordinatorHooks { after_process: Some(after_process), ..Default::default() });
            let commands = OcrCommands::with_coordinator(coordinator, Some(completed));
            let request_id = if close { "command-close" } else { "command-stale" };
            let task = tauri::async_runtime::spawn({
                let commands = commands.clone();
                async move { commands.recognize(request_id.into(), request.document_id, request.revision, request.page).await }
            });
            ready.recv_timeout(Duration::from_secs(30)).unwrap();
            if close {
                tauri::async_runtime::block_on(service.close(request.document_id)).unwrap();
            } else {
                tauri::async_runtime::block_on(service.edit(request.document_id, crate::editor::PageEdit::Rotate { pages: vec![0], clockwise: true }, 0)).unwrap();
            }
            release.send(()).unwrap();
            let error = tauri::async_runtime::block_on(task).unwrap().unwrap_err();
            assert!(error.contains(if close { "closed" } else { "changed" }));
            completed_receiver.recv_timeout(Duration::from_secs(30)).unwrap();
            assert_eq!(commands.cancel(request_id.into()).unwrap().status, "finished");
            let next = commands.begin(if close { "after-close" } else { "after-stale" }).unwrap();
            commands.inner.complete(if close { "after-close" } else { "after-stale" });
            assert!(!next.is_cancelled());
            if !close { tauri::async_runtime::block_on(service.close(request.document_id)).unwrap(); }
        }
    }

    #[cfg(ocr_opt_in)]
    #[test]
    #[ignore = "uses opt-in resources produced by scripts/setup-ocr.ps1"]
    fn retained_command_drop_and_window_cancel_hold_busy_until_child_cleanup() {
        let _ocr_guard = crate::ocr_process::test_lock();
        for drop_reply in [true, false] {
            let service = PdfService::start(root().join("src-tauri/resources/pdfium/bin/pdfium.dll"));
            let folder = tempfile::tempdir().unwrap();
            let (request, _, _) = tauri::async_runtime::block_on(known_page(&service, folder.path()));
            let (after_resume, ready, release) = blocking_hook();
            let (completed, completed_receiver) = completion_hook();
            let coordinator = OcrCoordinator::with_hooks(service.clone(), runtime_runner(), OcrCoordinatorHooks { after_process_resume: Some(after_resume), ..Default::default() });
            let commands = OcrCommands::with_coordinator(coordinator, Some(completed));
            let request_id = if drop_reply { "command-drop" } else { "command-window-close" };
            let mut task = tauri::async_runtime::spawn({
                let commands = commands.clone();
                async move { commands.recognize(request_id.into(), request.document_id, request.revision, request.page).await }
            });
            ready.recv_timeout(Duration::from_secs(30)).unwrap();
            if drop_reply {
                task.abort();
                assert!(tauri::async_runtime::block_on(&mut task).is_err());
            } else {
                commands.cancel_active();
            }
            assert_eq!(commands.begin("blocked-during-drain").err().unwrap(), "OCR is already running.");
            release.send(()).unwrap();
            if !drop_reply {
                assert_eq!(tauri::async_runtime::block_on(&mut task).unwrap().unwrap_err(), "OCR was cancelled.");
            }
            completed_receiver.recv_timeout(Duration::from_secs(30)).unwrap();
            let next = commands.begin("after-drain").unwrap();
            commands.inner.complete("after-drain");
            assert!(!next.is_cancelled());
            assert_eq!(commands.cancel(request_id.into()).unwrap().status, "already_cancelled");
            tauri::async_runtime::block_on(service.close(request.document_id)).unwrap();
        }
    }

    #[cfg(ocr_opt_in)]
    fn runtime_resource_relative() -> PathBuf {
        PathBuf::from(option_env!("PDF_WORKSTATION_OCR_RESOURCE_RELATIVE").unwrap())
    }

    #[cfg(ocr_opt_in)]
    fn copy_runtime_resources(destination_root: &Path) {
        let source = profile_dir().join(runtime_resource_relative());
        let destination = destination_root.join(runtime_resource_relative());
        std::fs::create_dir_all(destination.join("bin")).unwrap();
        std::fs::create_dir_all(destination.join("tessdata")).unwrap();
        std::fs::copy(source.join("bin/tesseract.exe"), destination.join("bin/tesseract.exe")).unwrap();
        std::fs::copy(source.join("tessdata/eng.traineddata"), destination.join("tessdata/eng.traineddata")).unwrap();
    }

    #[cfg(ocr_opt_in)]
    fn create_junction(link: &Path, target: &Path) {
        let output = std::process::Command::new("cmd.exe")
            .args(["/d", "/c", "mklink", "/J"])
            .arg(link)
            .arg(target)
            .output()
            .unwrap();
        assert!(output.status.success(), "mklink failed: {}", String::from_utf8_lossy(&output.stderr));
    }

    #[cfg(ocr_opt_in)]
    #[test]
    #[ignore = "uses opt-in resources produced by scripts/setup-ocr.ps1"]
    fn retained_runtime_missing_tampered_and_reparse_resources_are_unavailable_without_path_disclosure() {
        let _ocr_guard = crate::ocr_process::test_lock();
        let service = PdfService::start(root().join("src-tauri/resources/pdfium/bin/pdfium.dll"));
        let missing = tempfile::tempdir().unwrap();
        let commands = OcrCommands::new(service.clone(), missing.path().to_path_buf());
        assert_eq!(commands.capability(), unavailable_capability(UNAVAILABLE_RESOURCES));

        let tampered = tempfile::tempdir().unwrap();
        copy_runtime_resources(tampered.path());
        let engine = tampered.path().join(runtime_resource_relative()).join("bin/tesseract.exe");
        let mut bytes = std::fs::read(&engine).unwrap();
        let last = bytes.len() - 1;
        bytes[last] ^= 1;
        std::fs::write(&engine, bytes).unwrap();
        let commands = OcrCommands::new(service.clone(), tampered.path().to_path_buf());
        assert_eq!(commands.capability(), unavailable_capability(UNAVAILABLE_RESOURCES));

        let linked = tempfile::tempdir().unwrap();
        let actual_resources = profile_dir().join("resources").canonicalize().unwrap();
        let link = linked.path().join("resources");
        create_junction(&link, &actual_resources);
        let commands = OcrCommands::new(service, linked.path().to_path_buf());
        assert_eq!(commands.capability(), unavailable_capability(UNAVAILABLE_RESOURCES));
        assert!(!commands.capability().reason.unwrap().contains(actual_resources.to_string_lossy().as_ref()));
        std::fs::remove_dir(&link).unwrap();
    }
}
