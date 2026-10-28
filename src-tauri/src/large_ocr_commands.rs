//! App wiring for large-document OCR jobs: the in-process page source (the
//! existing PDFium worker), the Tesseract recogniser, the one-job registry and
//! the job folder in app data. SM5's isolated render helper can replace
//! `InProcessSource` behind the same `PageSource` trait.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde::Serialize;

use crate::large_ocr_job::{
    automatic_workers, run_job, unfinished_jobs, JobControl, JobOutcome, JobProgress, JobSettings, JobStart, JobState,
    PageError, PageRecognizer, PageReport, PageSource, PauseReason, RenderedPage,
};
use crate::ocr::{OcrJobRaster, OcrJobSource, OcrPageRequest};
use crate::ocr_process::{OcrCancellation, OcrOperation, OcrProcessError, OcrProcessRunner};
use crate::searchable_pdf_stream::{hex, sha256, JobManifest, PageIssue, MANIFEST_FORMAT};
use crate::service::PdfService;

const UNAVAILABLE: &str = "OCR is not available in this build.";

struct InProcessSource {
    service: PdfService,
    document_id: u64,
    revision: u64,
    sizes: Vec<(f32, f32)>,
    operation: Arc<OcrOperation>,
    render_slot: Mutex<()>,
}

impl PageSource for InProcessSource {
    fn page_size(&self, page: u32) -> Result<(f32, f32), PageError> {
        self.sizes.get(page as usize).copied().ok_or_else(|| PageError::Job(PauseReason::Document("OCR page is out of range".into())))
    }

    fn render(&self, page: u32, cancellation: &OcrCancellation) -> Result<RenderedPage, PageError> {
        let _one_render_at_a_time = self.render_slot.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        if cancellation.is_cancelled() { return Err(PageError::Cancelled); }
        let page = u16::try_from(page).map_err(|_| PageError::Job(PauseReason::Document("OCR page is out of range".into())))?;
        let request = OcrPageRequest { document_id: self.document_id, revision: self.revision, page };
        match self.service.ocr_job_raster_blocking(request, cancellation.clone(), self.operation.worker_hold()) {
            Ok(OcrJobRaster::Raster(raster, reduced_resolution)) => Ok(RenderedPage {
                width: raster.width, height: raster.height, page_width: raster.page_width, page_height: raster.page_height,
                p6: raster.p6, reduced_resolution,
            }),
            Ok(OcrJobRaster::PageFailed(message)) => Err(PageError::Page(PageIssue::RenderFailed, message)),
            Err(_) if cancellation.is_cancelled() => Err(PageError::Cancelled),
            Err(message) => Err(PageError::Job(PauseReason::Document(message))),
        }
    }
}

struct TesseractRecognizer {
    runner: OcrProcessRunner,
    operation: Arc<OcrOperation>,
}

impl PageRecognizer for TesseractRecognizer {
    fn recognize_tsv(&self, p6: &[u8], _cancellation: &OcrCancellation) -> Result<String, PageError> {
        match self.runner.recognize_p6_tsv_background(p6, &self.operation) {
            Ok(output) => Ok(output.text),
            Err(OcrProcessError::Cancelled) => Err(PageError::Cancelled),
            Err(OcrProcessError::TimedOut) => Err(PageError::Page(PageIssue::TimedOut, "timed out".into())),
            Err(OcrProcessError::InvalidInput(message)) => Err(PageError::Page(PageIssue::EngineOutputRefused, message)),
            Err(OcrProcessError::Busy) => Err(PageError::Job(PauseReason::EngineFailing("OCR is already running.".into()))),
            Err(OcrProcessError::InvalidAsset(message) | OcrProcessError::AssetIntegrity(message)) => Err(PageError::Job(PauseReason::EngineFailing(message))),
            Err(OcrProcessError::Document(message)) => Err(PageError::Job(PauseReason::Document(message))),
            Err(error) => Err(PageError::Page(PageIssue::EngineStopped, error.to_string())),
        }
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct LargeOcrJobStatus {
    pub(crate) job: String,
    pub(crate) source_name: String,
    pub(crate) destination: String,
    pub(crate) running: bool,
    pub(crate) progress: JobProgress,
    pub(crate) pause_reason: Option<PauseReason>,
    pub(crate) report: Vec<PageReport>,
    /// Paused by an earlier app session (not by this one).
    pub(crate) interrupted: bool,
    /// The saved parts and their page ranges, when the copy was saved in parts.
    pub(crate) parts: Vec<crate::large_ocr_parts::PublishedPart>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct LargeOcrStatus {
    pub(crate) available: bool,
    pub(crate) reason: Option<String>,
    pub(crate) max_pages: u32,
    pub(crate) job: Option<LargeOcrJobStatus>,
}

struct ActiveJob {
    directory: PathBuf,
    control: JobControl,
    status: Arc<Mutex<LargeOcrJobStatus>>,
}

#[derive(Clone)]
pub(crate) struct LargeOcrJobs {
    inner: Arc<Inner>,
}

struct Inner {
    root: PathBuf,
    resource_dir: PathBuf,
    service: PdfService,
    available: bool,
    active: Mutex<Option<ActiveJob>>,
    /// The saved job from an earlier session, read once (status is polled).
    interrupted: Mutex<Option<Option<LargeOcrJobStatus>>>,
}

const STALE_FOLDER_AGE: std::time::Duration = std::time::Duration::from_secs(24 * 60 * 60);

fn is_job_folder_name(name: &str) -> bool { name.len() == 24 && name.bytes().all(|byte| byte.is_ascii_hexdigit()) }

/// Startup cleanup: job folders older than a day whose description is missing
/// or invalid are removed (never one that is only locked or unreadable right
/// now, or written by a newer app); a temporary copy left next to a
/// destination by an interrupted cross-volume publish is removed.
fn sweep(root: &Path) {
    let Ok(entries) = std::fs::read_dir(root) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        let named = entry.file_name().to_str().is_some_and(is_job_folder_name);
        let Ok(metadata) = std::fs::symlink_metadata(&path) else { continue };
        if !named || !metadata.is_dir() || metadata.file_type().is_symlink() { continue; }
        use crate::searchable_pdf_stream::ManifestState;
        match crate::searchable_pdf_stream::inspect_manifest(&path) {
            ManifestState::Valid(manifest) => remove_stale_publish_temps(&path, &manifest),
            ManifestState::Missing | ManifestState::Corrupt => {
                let old = metadata.modified().ok().and_then(|modified| modified.elapsed().ok()).is_some_and(|age| age > STALE_FOLDER_AGE);
                if old { let _ = std::fs::remove_dir_all(&path); }
            }
            ManifestState::Newer | ManifestState::Unreadable => {}
        }
    }
}

impl LargeOcrJobs {
    pub(crate) fn new(service: PdfService, resource_dir: PathBuf, root: PathBuf) -> Self {
        let available = matches!(crate::ocr_commands::configured_runner(&resource_dir), Ok(Some(_)));
        sweep(&root);
        Self { inner: Arc::new(Inner { root, resource_dir, service, available, active: Mutex::new(None), interrupted: Mutex::new(None) }) }
    }

    pub(crate) fn service(&self) -> PdfService { self.inner.service.clone() }

    pub(crate) fn status(&self) -> LargeOcrStatus {
        let job = {
            let active = self.inner.active.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            active.as_ref().map(|job| job.status.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).clone())
        };
        let job = job.or_else(|| {
            let mut cached = self.inner.interrupted.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            cached.get_or_insert_with(|| self.interrupted_job().map(|(directory, manifest)| interrupted_status(&directory, &manifest))).clone()
        });
        LargeOcrStatus {
            available: self.inner.available, reason: (!self.inner.available).then(|| UNAVAILABLE.into()),
            max_pages: crate::searchable_pdf_stream::MAX_STREAM_PAGES, job,
        }
    }

    fn forget_interrupted(&self) { *self.inner.interrupted.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = None; }

    fn interrupted_job(&self) -> Option<(PathBuf, JobManifest)> {
        unfinished_jobs(&self.inner.root).into_iter().next()
    }

    fn running(&self) -> bool {
        let active = self.inner.active.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        active.as_ref().is_some_and(|job| job.status.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).running)
    }

    /// A destination must be an absolute `.pdf` path in an existing folder,
    /// outside the app data folder, and must not exist yet.
    fn validate_destination(&self, destination: &Path) -> Result<(), String> {
        validate_destination(destination, self.inner.root.parent().unwrap_or(&self.inner.root))
    }

    /// Starts a new job for the open document. Blocking: call off the UI thread.
    pub(crate) fn start(&self, document_id: u64, revision: u64, source: OcrJobSource, destination: PathBuf) -> Result<LargeOcrStatus, String> {
        if !self.inner.available { return Err(UNAVAILABLE.into()); }
        if self.running() { return Err("OCR is already running.".into()); }
        if self.interrupted_job().is_some() { return Err("Another OCR job has saved progress. Resume or discard it first.".into()); }
        self.validate_destination(&destination)?;
        std::fs::create_dir_all(&self.inner.root).map_err(|error| format!("Could not create the OCR work folder: {error}"))?;
        let name = hex(&sha256(format!("{}|{}|{:?}", source.identity, destination.display(), std::time::SystemTime::now()).as_bytes())[..12]);
        let manifest = JobManifest {
            format: MANIFEST_FORMAT, page_count: source.page_count, source_identity: source.identity.clone(), source_name: source.source_name.clone(),
            destination: destination.to_string_lossy().into_owned(), dpi: crate::ocr::OCR_DPI, language: "eng".into(),
            engine_identity: option_env!("PDF_WORKSTATION_OCR_ENGINE_SHA256").unwrap_or("unknown").into(),
        };
        self.launch(self.inner.root.join(name), JobStart::New(manifest.clone()), &manifest, document_id, revision, source, false)
    }

    /// Resumes the saved job if the open document is the one it was started
    /// from. `in_parts` lets it continue into further files at the size cap.
    pub(crate) fn resume(&self, document_id: u64, revision: u64, source: OcrJobSource, in_parts: bool) -> Result<LargeOcrStatus, String> {
        if self.running() { return Err("OCR is already running.".into()); }
        let (directory, manifest) = self.interrupted_job().ok_or("There is no saved OCR progress to resume.")?;
        let destination = Path::new(&manifest.destination);
        let app_data = self.inner.root.parent().unwrap_or(&self.inner.root);
        let checked = if in_parts || crate::large_ocr_parts::has_parts(&directory) { validate_destination_folder(destination, app_data) } else { validate_destination(destination, app_data) };
        checked.map_err(|error| format!("{error} Use Save elsewhere to choose another location."))?;
        if manifest.source_identity != source.identity || manifest.page_count != source.page_count {
            return Err("The document has changed since OCR started, so the saved progress can't be used.".into());
        }
        self.launch(directory, JobStart::Resume, &manifest, document_id, revision, source, in_parts)
    }

    /// "Save in parts": a running job continues into further files when it
    /// reaches the size cap; a paused job resumes that way (it needs the
    /// source document unless every page is already done).
    pub(crate) fn save_in_parts(&self, document: Option<(u64, u64, OcrJobSource)>) -> Result<LargeOcrStatus, String> {
        {
            let active = self.inner.active.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            if let Some(job) = active.as_ref() {
                let mut status = job.status.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
                if status.running {
                    job.control.save_in_parts();
                    status.progress.in_parts = true;
                    status.progress.size_warning = false;
                    drop(status);
                    drop(active);
                    return Ok(self.status());
                }
            }
        }
        let (document_id, revision, source) = document.ok_or("Open the document this OCR job was started from, then choose Save in parts again.")?;
        self.resume(document_id, revision, source, true)
    }

    /// Changes where the saved job will be published, then resumes it. When
    /// every page is already done the source document is not needed.
    pub(crate) fn relocate(&self, destination: PathBuf, document: Option<(u64, u64, OcrJobSource)>) -> Result<LargeOcrStatus, String> {
        if self.running() { return Err("OCR is already running.".into()); }
        let (directory, manifest) = self.interrupted_job().ok_or("There is no saved OCR progress to resume.")?;
        let app_data = self.inner.root.parent().unwrap_or(&self.inner.root);
        let (manifest, document_id, revision, source) = prepare_relocation(app_data, &directory, &manifest, &destination, document)?;
        let in_parts = crate::large_ocr_parts::has_parts(&directory);
        self.launch(directory, JobStart::Resume, &manifest, document_id, revision, source, in_parts)
    }

    pub(crate) fn is_running(&self) -> bool { self.running() }

    /// The saved job's destination, for the Save elsewhere dialog.
    pub(crate) fn saved_destination(&self) -> Option<PathBuf> { self.interrupted_job().map(|(_, manifest)| PathBuf::from(manifest.destination)) }

    pub(crate) fn pause(&self) {
        let active = self.inner.active.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(job) = active.as_ref() { job.control.pause(); }
    }

    /// Cancels a running job, or discards saved progress. The source is never touched.
    pub(crate) fn cancel(&self) {
        self.forget_interrupted();
        let mut active = self.inner.active.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(job) = active.as_ref() {
            let running = job.status.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).running;
            job.control.cancel();
            if !running {
                discard(&job.directory);
                *active = None;
            }
            return;
        }
        drop(active);
        if let Some((directory, _)) = self.interrupted_job() {
            if directory.parent() == Some(self.inner.root.as_path()) { discard(&directory); }
        }
    }

    /// Clears a finished job's status from the panel.
    pub(crate) fn dismiss(&self) {
        let mut active = self.inner.active.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let finished = active.as_ref().is_some_and(|job| {
            let status = job.status.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            matches!(status.progress.state, JobState::Published | JobState::Cancelled)
        });
        if finished { *active = None; }
    }

    #[allow(clippy::too_many_arguments)]
    fn launch(&self, directory: PathBuf, start: JobStart, manifest: &JobManifest, document_id: u64, revision: u64, source: OcrJobSource, in_parts: bool) -> Result<LargeOcrStatus, String> {
        let control = JobControl::default();
        if in_parts { control.save_in_parts(); }
        let operation = Arc::new(OcrOperation::try_begin(control.cancellation()).map_err(|error| match error {
            OcrProcessError::Busy if self.running() => "OCR is already running.".to_owned(),
            OcrProcessError::Busy => "OCR is still busy: a page OCR or a previous page render is still finishing. Try again in a moment.".to_owned(),
            other => other.to_string(),
        })?);
        let runner = crate::ocr_commands::configured_runner(&self.inner.resource_dir).ok().flatten().ok_or(UNAVAILABLE)?;
        let workers = automatic_workers();
        let status = Arc::new(Mutex::new(LargeOcrJobStatus {
            job: directory.file_name().unwrap_or_default().to_string_lossy().into_owned(),
            source_name: manifest.source_name.clone(), destination: manifest.destination.clone(), running: true,
            progress: JobProgress { state: JobState::Preparing, pages_done: 0, page_count: manifest.page_count, workers, issues: 0, bytes_written: 0, projected_bytes: None, size_warning: false, seconds_left: None, in_parts, part: 1 },
            pause_reason: None, report: Vec::new(), interrupted: false, parts: Vec::new(),
        }));
        let page_source: Arc<dyn PageSource> = Arc::new(InProcessSource {
            service: self.inner.service.clone(), document_id, revision, sizes: source.page_sizes, operation: operation.clone(), render_slot: Mutex::new(()),
        });
        let recognizer: Arc<dyn PageRecognizer> = Arc::new(TesseractRecognizer { runner, operation: operation.clone() });
        let commit_token = control.cancellation().clone();
        let settings = JobSettings {
            directory: directory.clone(), start, workers,
            commit: Box::new(move || commit_token.commit().map_err(|error| error.to_string())),
            size_cap: crate::searchable_pdf_stream::MAX_STREAM_OUTPUT_BYTES,
        };
        let sink_status = status.clone();
        let progress: crate::large_ocr_job::ProgressSink = Arc::new(move |progress: &JobProgress| {
            sink_status.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).progress = progress.clone();
        });
        let thread_status = status.clone();
        let thread_control = control.clone();
        {
            let mut active = self.inner.active.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            *active = Some(ActiveJob { directory: directory.clone(), control: control.clone(), status: status.clone() });
        }
        self.forget_interrupted();
        let spawned = std::thread::Builder::new().name("ocr-job".into()).spawn(move || {
            let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| run_job(settings, page_source, recognizer, thread_control, progress)));
            drop(operation);
            apply_outcome(&thread_status, outcome);
        });
        if let Err(error) = spawned {
            *self.inner.active.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = None;
            return Err(format!("Could not start OCR: {error}"));
        }
        Ok(self.status())
    }
}

/// Checks a Save elsewhere request and points the job at the new destination.
/// The open document is used only if it is the job's source; when every page
/// is done and it is not, a source with no pages stands in, so a resume that
/// has to redo a page pauses instead of rendering another document. A
/// temporary copy left next to the old destination is removed first.
fn prepare_relocation(app_data: &Path, directory: &Path, manifest: &JobManifest, destination: &Path, document: Option<(u64, u64, OcrJobSource)>) -> Result<(JobManifest, u64, u64, OcrJobSource), String> {
    if crate::large_ocr_parts::any_published(directory) {
        return Err("Some parts are already saved next to the original destination, so the rest must be saved there too. Move or rename the file that is in the way, then resume.".into());
    }
    if crate::large_ocr_parts::has_parts(directory) { validate_destination_folder(destination, app_data)?; } else { validate_destination(destination, app_data)?; }
    let all_done = crate::large_ocr_parts::saved_progress(directory, manifest) == manifest.page_count;
    let (document_id, revision, source) = match document {
        Some((document_id, revision, source)) if manifest.source_identity == source.identity && manifest.page_count == source.page_count => (document_id, revision, source),
        _ if all_done => (0, 0, OcrJobSource { page_count: manifest.page_count, page_sizes: Vec::new(), identity: manifest.source_identity.clone(), suggested_name: String::new(), source_name: manifest.source_name.clone() }),
        Some(_) => return Err("The document has changed since OCR started, so the saved progress can't be used.".into()),
        None => return Err("Open the document this OCR job was started from, then choose Save elsewhere again.".into()),
    };
    remove_stale_publish_temps(directory, manifest);
    let manifest = crate::searchable_pdf_stream::set_destination(directory, &destination.to_string_lossy()).map_err(|error| error.to_string())?;
    Ok((manifest, document_id, revision, source))
}

fn discard(directory: &Path) {
    if let Ok(manifest) = crate::searchable_pdf_stream::read_manifest(directory) { remove_stale_publish_temps(directory, &manifest); }
    let _ = std::fs::remove_dir_all(directory);
}

/// Removes temporary copies left by an interrupted cross-volume publish of the
/// copy or of any of its parts (each part folder keeps its own marker).
fn remove_stale_publish_temps(directory: &Path, manifest: &JobManifest) {
    for folder in crate::large_ocr_parts::part_directories(directory) {
        crate::searchable_pdf_stream::remove_stale_publish_temp(&folder, manifest);
    }
}

pub(crate) fn validate_destination(destination: &Path, app_data: &Path) -> Result<(), String> {
    validate_destination_folder(destination, app_data)?;
    if destination.exists() { return Err("That file already exists. Choose a new filename; Save a Copy never overwrites an existing file.".into()); }
    Ok(())
}

/// The destination checks except "does not exist yet": a copy saved in parts
/// never writes the chosen name itself, only `<name> (part i of N).pdf`,
/// whose names are checked when the parts are published.
pub(crate) fn validate_destination_folder(destination: &Path, app_data: &Path) -> Result<(), String> {
    if !destination.is_absolute() { return Err("Choose a full path for the OCR copy.".into()); }
    if !destination.extension().is_some_and(|extension| extension.eq_ignore_ascii_case("pdf")) { return Err("The output filename must end in .pdf.".into()); }
    let parent = destination.parent().filter(|parent| parent.is_dir()).ok_or("The folder for the OCR copy does not exist.")?;
    let canonical_inside = std::fs::canonicalize(parent).ok().zip(std::fs::canonicalize(app_data).ok()).is_some_and(|(parent, app_data)| parent.starts_with(app_data));
    if parent.starts_with(app_data) || canonical_inside { return Err("Choose a folder outside PDF Workstation's app data for the OCR copy.".into()); }
    Ok(())
}

/// Records how a job ended. A panic in the job thread becomes a pause with a
/// message, so the panel never shows a job that runs forever.
fn apply_outcome(status: &Mutex<LargeOcrJobStatus>, outcome: std::thread::Result<JobOutcome>) {
    let mut status = status.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    status.running = false;
    match outcome {
        Ok(JobOutcome::Published { report, parts, .. }) => {
            status.progress.state = JobState::Published;
            status.progress.in_parts = !parts.is_empty();
            status.report = report;
            status.parts = parts;
        }
        Ok(JobOutcome::Paused { reason, next_page, report }) => {
            status.progress.state = JobState::Paused;
            status.progress.pages_done = next_page;
            status.pause_reason = Some(reason);
            status.report = report;
        }
        Ok(JobOutcome::Cancelled) => { status.progress.state = JobState::Cancelled; }
        Err(_) => {
            status.progress.state = JobState::Paused;
            status.pause_reason = Some(PauseReason::Publish("OCR stopped after an internal error. Saved pages are kept; resume or discard them.".into()));
        }
    }
}

fn interrupted_status(directory: &Path, manifest: &JobManifest) -> LargeOcrJobStatus {
    let pages_done = crate::large_ocr_parts::saved_progress(directory, manifest);
    let parts = crate::large_ocr_parts::load_state(directory, manifest).ok().flatten();
    LargeOcrJobStatus {
        job: directory.file_name().unwrap_or_default().to_string_lossy().into_owned(),
        source_name: manifest.source_name.clone(), destination: manifest.destination.clone(), running: false,
        progress: JobProgress {
            state: JobState::Paused, pages_done, page_count: manifest.page_count, workers: 0, issues: 0, bytes_written: 0, projected_bytes: None, size_warning: false, seconds_left: None,
            in_parts: parts.is_some(), part: parts.map_or(1, |state| state.closed.len() as u32 + 1),
        },
        pause_reason: Some(PauseReason::User), report: Vec::new(), interrupted: true, parts: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::large_ocr_job::PageReport;
    use lopdf::{dictionary, Document, Object, Stream};

    /// A text PDF generated in code: every page carries its number and a short
    /// paragraph, so the real engine has something to read on each page.
    pub(crate) fn synthetic_text_pdf(pages: u32) -> Vec<u8> {
        let mut document = Document::with_version("1.7");
        let pages_id = document.new_object_id();
        let font = document.add_object(dictionary! { "Type" => "Font", "Subtype" => "Type1", "BaseFont" => "Helvetica" });
        let mut kids = Vec::new();
        for page in 0..pages {
            let lines = [
                format!("Page {} of {pages}", page + 1),
                "The quick brown fox jumps over the lazy dog.".to_string(),
                format!("Reference number {}", 100_000 + page * 7),
            ];
            let mut content = String::from("BT\n/F1 16 Tf\n72 700 Td\n20 TL\n");
            for line in lines { content.push_str(&format!("({line}) Tj\nT*\n")); }
            content.push_str("ET\n");
            let content = document.add_object(Stream::new(dictionary! {}, content.into_bytes()));
            let id = document.add_object(dictionary! {
                "Type" => "Page", "Parent" => pages_id, "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
                "Resources" => dictionary! { "Font" => dictionary! { "F1" => font } }, "Contents" => content,
            });
            kids.push(Object::Reference(id));
        }
        document.objects.insert(pages_id, dictionary! { "Type" => "Pages", "Count" => pages as i64, "Kids" => kids }.into());
        let catalog = document.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages_id });
        document.trailer.set("Root", catalog);
        let mut bytes = Vec::new();
        document.save_to(&mut bytes).unwrap();
        bytes
    }

    #[test]
    fn a_panic_in_the_job_thread_ends_as_a_pause_not_a_running_job() {
        let status = Mutex::new(LargeOcrJobStatus {
            job: "a".repeat(24), source_name: "big.pdf".into(), destination: "E:/x.pdf".into(), running: true,
            progress: JobProgress { state: JobState::Running, pages_done: 3, page_count: 9, workers: 1, issues: 0, bytes_written: 0, projected_bytes: None, size_warning: false, seconds_left: None, in_parts: false, part: 1 },
            pause_reason: None, report: Vec::new(), interrupted: false, parts: Vec::new(),
        });
        let outcome = std::panic::catch_unwind(|| -> JobOutcome { panic!("simulated job bug") });
        apply_outcome(&status, outcome);
        let status = status.into_inner().unwrap();
        assert!(!status.running);
        assert_eq!(status.progress.state, JobState::Paused);
        assert!(matches!(status.pause_reason, Some(PauseReason::Publish(_))));
    }

    #[test]
    fn destinations_must_be_new_absolute_pdfs_outside_app_data() {
        let folder = tempfile::tempdir().unwrap();
        let app_data = folder.path().join("appdata");
        std::fs::create_dir_all(app_data.join("ocr-jobs")).unwrap();
        assert!(validate_destination(&folder.path().join("copy.pdf"), &app_data).is_ok());
        assert!(validate_destination(Path::new("copy.pdf"), &app_data).is_err());
        assert!(validate_destination(&folder.path().join("copy.txt"), &app_data).is_err());
        assert!(validate_destination(&folder.path().join("missing").join("copy.pdf"), &app_data).is_err());
        assert!(validate_destination(&app_data.join("ocr-jobs").join("copy.pdf"), &app_data).is_err());
        std::fs::write(folder.path().join("taken.pdf"), b"x").unwrap();
        assert!(validate_destination(&folder.path().join("taken.pdf"), &app_data).is_err());
    }

    #[test]
    fn startup_sweep_removes_only_old_missing_or_invalid_job_folders() {
        use std::os::windows::fs::OpenOptionsExt;
        let folder = tempfile::tempdir().unwrap();
        let root = folder.path().join("ocr-jobs");
        let fresh = root.join("b".repeat(24));
        let missing = root.join("c".repeat(24));
        let corrupt = root.join("d".repeat(24));
        let newer = root.join("e".repeat(24));
        let locked = root.join("f".repeat(24));
        let foreign = root.join("keep-me");
        for path in [&fresh, &missing, &corrupt, &newer, &locked, &foreign] { std::fs::create_dir_all(path).unwrap(); }
        std::fs::write(corrupt.join(crate::searchable_pdf_stream::MANIFEST_FILE), b"{not json").unwrap();
        std::fs::write(newer.join(crate::searchable_pdf_stream::MANIFEST_FILE), br#"{"format": 2, "somethingNew": true}"#).unwrap();
        std::fs::write(locked.join(crate::searchable_pdf_stream::MANIFEST_FILE), b"{}").unwrap();
        let day_ago = std::time::SystemTime::now() - std::time::Duration::from_secs(25 * 60 * 60);
        let age = |path: &Path| { std::fs::OpenOptions::new().write(true).custom_flags(0x0200_0000).open(path).unwrap().set_modified(day_ago).unwrap(); };
        for path in [&missing, &corrupt, &newer, &locked, &foreign] { age(path); }
        let hold = std::fs::OpenOptions::new().read(true).share_mode(0).open(locked.join(crate::searchable_pdf_stream::MANIFEST_FILE)).unwrap();
        sweep(&root);
        drop(hold);
        assert!(fresh.exists(), "younger than a day");
        assert!(!missing.exists() && !corrupt.exists(), "missing or invalid descriptions are removed");
        assert!(newer.exists(), "a newer app's job is kept");
        assert!(locked.exists(), "a description that cannot be read right now is kept");
        assert!(foreign.exists(), "only job-named folders are touched");
    }

    #[test]
    fn relocation_uses_only_the_matching_document_and_clears_the_old_temp_copy() {
        use crate::searchable_pdf_stream::{PartialWriter, PUBLISH_TEMP_MARKER};
        let folder = tempfile::tempdir().unwrap();
        let app_data = folder.path().join("appdata");
        let old_folder = folder.path().join("old");
        std::fs::create_dir_all(&old_folder).unwrap();
        let directory = app_data.join("ocr-jobs").join("a".repeat(24));
        std::fs::create_dir_all(directory.parent().unwrap()).unwrap();
        let mut manifest = crate::searchable_pdf_stream::tests::manifest(2, &old_folder.join("copy.pdf"));
        manifest.source_identity = "11".repeat(64);
        let mut writer = PartialWriter::create(&directory, manifest.clone()).unwrap();
        writer.append(&crate::searchable_pdf_stream::tests::synthetic_page(0)).unwrap();
        writer.checkpoint().unwrap();
        drop(writer);
        let source = |identity: &str| OcrJobSource { page_count: 2, page_sizes: vec![(612.0, 792.0); 2], identity: identity.into(), suggested_name: String::new(), source_name: "s.pdf".into() };
        let new_destination = folder.path().join("new.pdf");
        assert!(prepare_relocation(&app_data, &directory, &manifest, &new_destination, None).is_err(), "pages remain, so the source is needed");
        assert!(prepare_relocation(&app_data, &directory, &manifest, &new_destination, Some((5, 1, source(&"22".repeat(64))))).is_err(), "another document is refused");
        assert!(prepare_relocation(&app_data, &directory, &manifest, &app_data.join("x.pdf"), Some((5, 1, source(&manifest.source_identity)))).is_err());
        let leftover = old_folder.join(".tmpOLD1");
        std::fs::write(&leftover, b"partial copy").unwrap();
        std::fs::write(directory.join(PUBLISH_TEMP_MARKER), leftover.to_string_lossy().as_bytes()).unwrap();
        let (moved, document_id, _, _) = prepare_relocation(&app_data, &directory, &manifest, &new_destination, Some((5, 1, source(&manifest.source_identity)))).unwrap();
        assert_eq!((document_id, moved.destination.as_str()), (5, new_destination.to_string_lossy().as_ref()));
        assert!(!leftover.exists(), "the temporary copy next to the old destination is removed");

        let mut writer = PartialWriter::resume(&directory).unwrap();
        writer.append(&crate::searchable_pdf_stream::tests::synthetic_page(1)).unwrap();
        writer.checkpoint().unwrap();
        drop(writer);
        let other = folder.path().join("other.pdf");
        let (_, document_id, _, stand_in) = prepare_relocation(&app_data, &directory, &moved, &other, Some((7, 1, source(&"33".repeat(64))))).unwrap();
        assert_eq!((document_id, stand_in.page_sizes.len()), (0, 0), "all pages done: another open document is never used");
    }

    #[test]
    fn a_copy_in_parts_ignores_its_base_name_and_stays_put_once_a_part_is_saved() {
        use crate::searchable_pdf_stream::PartialWriter;
        let folder = tempfile::tempdir().unwrap();
        let app_data = folder.path().join("appdata");
        let directory = app_data.join("ocr-jobs").join("a".repeat(24));
        std::fs::create_dir_all(directory.parent().unwrap()).unwrap();
        let base = folder.path().join("copy.pdf");
        std::fs::write(&base, b"an unrelated file with the chosen name").unwrap();
        assert!(validate_destination(&base, &app_data).is_err());
        assert!(validate_destination_folder(&base, &app_data).is_ok(), "parts never write the chosen name itself");
        let manifest = crate::searchable_pdf_stream::tests::manifest(3, &base);
        let mut writer = PartialWriter::create(&directory, manifest.clone()).unwrap();
        writer.append(&crate::searchable_pdf_stream::tests::synthetic_page(0)).unwrap();
        writer.checkpoint().unwrap();
        drop(writer);
        std::fs::write(directory.join(crate::large_ocr_parts::PARTS_FILE), br#"{"format":1,"closed":[1],"published":0,"publishing":null}"#).unwrap();
        let source = OcrJobSource { page_count: 3, page_sizes: vec![(612.0, 792.0); 3], identity: manifest.source_identity.clone(), suggested_name: String::new(), source_name: "s.pdf".into() };
        let elsewhere = folder.path().join("elsewhere.pdf");
        assert!(prepare_relocation(&app_data, &directory, &manifest, &elsewhere, Some((5, 1, source.clone()))).is_ok());
        std::fs::write(directory.join(crate::large_ocr_parts::PARTS_FILE), br#"{"format":1,"closed":[1],"published":1,"publishing":null}"#).unwrap();
        let refused = prepare_relocation(&app_data, &directory, &manifest, &folder.path().join("third.pdf"), Some((5, 1, source))).unwrap_err();
        assert!(refused.contains("already saved"), "{refused}");
        let interrupted = interrupted_status(&directory, &crate::searchable_pdf_stream::read_manifest(&directory).unwrap());
        assert!(interrupted.progress.in_parts);
        assert_eq!((interrupted.progress.part, interrupted.progress.pages_done), (2, 1));
    }

    /// End to end with the real engine and the in-process renderer. Needs
    /// `PDF_WORKSTATION_OCR_SETUP_ROOT`; `SM4_REAL_PAGES` sets the page count.
    #[test]
    #[ignore = "needs PDF_WORKSTATION_OCR_SETUP_ROOT pointing at a scripts/setup-ocr.ps1 output root"]
    fn real_engine_large_job_publishes_a_searchable_copy() {
        let Some(setup) = crate::ocr_test_support::require_local_setup("real_engine_large_job_publishes_a_searchable_copy") else { return; };
        let _serial = crate::ocr_process::test_lock();
        let _jobs = crate::large_ocr_job::tests::JOB_TEST_LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let pages = std::env::var("SM4_REAL_PAGES").ok().and_then(|value| value.parse().ok()).unwrap_or(120_u32);
        let folder = tempfile::tempdir().unwrap();
        let source_path = folder.path().join("synthetic-text.pdf");
        std::fs::write(&source_path, synthetic_text_pdf(pages)).unwrap();
        let service = PdfService::start(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("resources/pdfium/bin/pdfium.dll"));
        let opened = tauri::async_runtime::block_on(service.open(source_path)).unwrap();
        let request = opened.ocr_request(0);
        let source = service.ocr_job_source_blocking(request.document_id, request.revision).unwrap();
        assert_eq!(source.page_count, pages);
        let control = JobControl::default();
        let operation = Arc::new(OcrOperation::try_begin(control.cancellation()).unwrap());
        let runner = OcrProcessRunner::new(setup.root.join("engine/bin/tesseract.exe"), setup.root.join("engine/tessdata"), setup.engine).unwrap();
        let destination = folder.path().join("synthetic-text-searchable.pdf");
        let manifest = JobManifest {
            format: MANIFEST_FORMAT, page_count: pages, source_identity: source.identity.clone(), source_name: source.source_name.clone(),
            destination: destination.to_string_lossy().into_owned(), dpi: 150, language: "eng".into(), engine_identity: "test".into(),
        };
        let workers = automatic_workers();
        let token = control.cancellation().clone();
        let settings = JobSettings { directory: folder.path().join("job"), start: JobStart::New(manifest), workers, commit: Box::new(move || token.commit().map_err(|error| error.to_string())), size_cap: crate::searchable_pdf_stream::MAX_STREAM_OUTPUT_BYTES };
        let page_source = Arc::new(InProcessSource { service: service.clone(), document_id: request.document_id, revision: request.revision, sizes: source.page_sizes, operation: operation.clone(), render_slot: Mutex::new(()) });
        let recognizer = Arc::new(TesseractRecognizer { runner, operation: operation.clone() });
        let started = std::time::Instant::now();
        let outcome = run_job(settings, page_source, recognizer, control, Arc::new(|_: &JobProgress| {}));
        let elapsed = started.elapsed();
        drop(operation);
        let JobOutcome::Published { bytes, report, .. } = outcome else { panic!("{outcome:?}") };
        eprintln!("real engine: pages={pages} workers={workers} elapsed={:.1}s pages_per_min={:.1} output={bytes} report={report:?}", elapsed.as_secs_f64(), f64::from(pages) * 60.0 / elapsed.as_secs_f64());
        assert!(report.iter().all(|entry: &PageReport| entry.outcome == crate::searchable_pdf_stream::PageOutcome::Text), "{report:?}");
        let reopened = tauri::async_runtime::block_on(service.open(destination)).unwrap();
        let reopened_request = reopened.ocr_request(0);
        let mut matched = 0;
        for page in [0, pages / 2, pages - 1] {
            let text = tauri::async_runtime::block_on(service.text(reopened_request.document_id, page as u16, reopened_request.revision)).unwrap();
            if text.contains(&format!("Page {} of {pages}", page + 1)) { matched += 1; }
            assert!(text.contains("quick brown fox"), "page {page}: {text:?}");
        }
        eprintln!("page-number lines read exactly on {matched} of 3 sampled pages");
    }
}
