//! Large-document OCR job: a coordinator, a bounded pool of recognition
//! workers and the streaming writer.
//!
//! Workers claim pages in order, render them (`PageSource`), recognise them
//! (`PageRecognizer`) and encode the page image; the coordinator writes the
//! finished pages strictly in order. A worker may only claim a page within
//! `workers + 1` pages of the next page to write, so at most that many pages
//! are in memory however long the document is. A page that fails is retried
//! once with a fresh attempt; then it keeps its image without text (recognition
//! failed) or becomes a white placeholder (rendering failed), is listed in the
//! report, and the job continues. Five failing pages in a row pause the job
//! instead of skipping hundreds of pages. Pause and cancel stop every worker
//! through the shared cancellation token, which also kills running engine
//! processes; a pause keeps the checkpoint, a cancel deletes it.
//!
//! The render and recognition stages are traits so the in-process renderer
//! used now can be replaced by SM5's isolated render helper later without
//! changing the coordinator.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use serde::Serialize;

use crate::large_ocr_parts::{Opened, Output, PublishedPart};
use crate::ocr_preprocess::{map_words_to_page, Skew};
use crate::ocr_process::OcrCancellation;
use crate::searchable_ocr::{map_words_to_display, parse_tesseract_tsv};
use crate::searchable_pdf::SearchableWord;
use crate::searchable_pdf_stream::{
    encode_image, placeholder_image, publish_noclobber, JobManifest, PageIssue, PageOutcome, StreamError, StreamPage,
    MAX_STREAM_OUTPUT_BYTES,
};

pub(crate) const MAX_JOB_WORKERS: usize = 4;
const MAX_ATTEMPTS: u32 = 2;
const BREAKER_PAGES: u32 = 5;
const PROGRESS_INTERVAL: Duration = Duration::from_millis(250);
const PROJECTION_AFTER_PAGES: u32 = 10;
/// How long a stop waits for workers. A worker stuck in a render that cannot
/// be interrupted (in-process PDFium) is left behind; its late result is
/// ignored, so pause and cancel still answer promptly.
const STOP_WAIT: Duration = Duration::from_secs(2);

/// A rendered page: P6 bytes (header included) of the white-composited raster.
pub(crate) struct RenderedPage {
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) page_width: f32,
    pub(crate) page_height: f32,
    pub(crate) p6: Vec<u8>,
    pub(crate) reduced_resolution: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", tag = "kind", content = "message")]
pub(crate) enum PauseReason {
    /// The user paused, or the app is closing.
    User,
    /// Five pages in a row failed; the engine is probably broken.
    EngineFailing(String),
    /// The source document was closed or edited, or can no longer be read.
    Document(String),
    /// Writing failed (for example the disk is full).
    Disk(String),
    /// The copy would pass 1 GiB (and saving in parts was not chosen).
    SizeLimit,
    /// The finished copy failed its final check, or could not be published.
    Publish(String),
}

#[derive(Debug)]
pub(crate) enum PageError {
    /// This page failed; retry once, then degrade the page and continue.
    Page(PageIssue, String),
    /// The whole job must pause (document closed or changed, engine unusable).
    Job(PauseReason),
    Cancelled,
}

pub(crate) trait PageSource: Send + Sync {
    fn page_size(&self, page: u32) -> Result<(f32, f32), PageError>;
    fn render(&self, page: u32, cancellation: &OcrCancellation) -> Result<RenderedPage, PageError>;
}

pub(crate) trait PageRecognizer: Send + Sync {
    /// Recognises one P6 page: Tesseract TSV in the pixels of the image the
    /// engine saw, and that image's rotation relative to the page.
    fn recognize_tsv(&self, p6: &[u8], cancellation: &OcrCancellation) -> Result<(String, Skew), PageError>;
}

const REQUEST_RUN: u8 = 0;
const REQUEST_PAUSE: u8 = 1;
const REQUEST_CANCEL: u8 = 2;

/// Shared between the job and whoever controls it (commands, tests).
#[derive(Clone)]
pub(crate) struct JobControl {
    cancellation: OcrCancellation,
    request: Arc<AtomicU8>,
    in_parts: Arc<AtomicBool>,
}

impl Default for JobControl {
    fn default() -> Self { Self { cancellation: OcrCancellation::default(), request: Arc::new(AtomicU8::new(REQUEST_RUN)), in_parts: Arc::new(AtomicBool::new(false)) } }
}

impl JobControl {
    pub(crate) fn cancellation(&self) -> &OcrCancellation { &self.cancellation }
    /// Lets the job continue into further files instead of pausing at the size cap.
    pub(crate) fn save_in_parts(&self) { self.in_parts.store(true, Ordering::Release); }
    pub(crate) fn may_split(&self) -> bool { self.in_parts.load(Ordering::Acquire) }
    /// Stops the job and keeps its progress. Running engine processes are killed.
    pub(crate) fn pause(&self) {
        let _ = self.request.compare_exchange(REQUEST_RUN, REQUEST_PAUSE, Ordering::AcqRel, Ordering::Acquire);
        self.cancellation.cancel();
    }
    /// Stops the job and discards its progress.
    pub(crate) fn cancel(&self) {
        self.request.store(REQUEST_CANCEL, Ordering::Release);
        self.cancellation.cancel();
    }
    fn requested(&self) -> u8 { self.request.load(Ordering::Acquire) }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum JobState { Preparing, Running, Finalising, Published, Paused, Cancelled }

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct JobProgress {
    pub(crate) state: JobState,
    pub(crate) pages_done: u32,
    pub(crate) page_count: u32,
    pub(crate) workers: usize,
    pub(crate) issues: u32,
    pub(crate) bytes_written: u64,
    /// Estimated final size after the first pages, or `None` before that.
    pub(crate) projected_bytes: Option<u64>,
    /// True when the projected size is over 1 GiB and the copy is not being saved in parts.
    pub(crate) size_warning: bool,
    pub(crate) seconds_left: Option<u64>,
    /// Saving in parts was chosen, or has started.
    pub(crate) in_parts: bool,
    /// The part being written (1 for a single copy).
    pub(crate) part: u32,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PageReport {
    pub(crate) page: u32,
    pub(crate) outcome: PageOutcome,
    pub(crate) issue: PageIssue,
    pub(crate) reduced_resolution: bool,
    pub(crate) dropped_words: u32,
}

#[derive(Debug, PartialEq)]
pub(crate) enum JobOutcome {
    /// `parts` is empty for a single copy; otherwise `path` is part 1.
    Published { path: PathBuf, pages: u32, bytes: u64, report: Vec<PageReport>, parts: Vec<PublishedPart> },
    Paused { reason: PauseReason, next_page: u32, report: Vec<PageReport> },
    Cancelled,
}

pub(crate) enum JobStart {
    New(JobManifest),
    Resume,
}

pub(crate) struct JobSettings {
    pub(crate) directory: PathBuf,
    pub(crate) start: JobStart,
    pub(crate) workers: usize,
    /// Runs immediately before the finished copy (or its first part) is moved
    /// into place; an error (cancelled, paused) stops the publish.
    pub(crate) commit: Box<dyn FnOnce() -> Result<(), String> + Send>,
    /// The largest file the job may write: `MAX_STREAM_OUTPUT_BYTES` in the
    /// app, smaller in tests.
    pub(crate) size_cap: u64,
}

pub(crate) type ProgressSink = Arc<dyn Fn(&JobProgress) + Send + Sync>;

/// Live page buffers (rendered or encoded, not yet written). Tests read the
/// maximum to prove memory stays bounded by the window, not the page count.
#[cfg(test)]
pub(crate) static LIVE_PAGES: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
#[cfg(test)]
pub(crate) static MAX_LIVE_PAGES: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// When the last stop finished waiting for workers (before the checkpoint's
/// disk flushes, whose time depends on the disk, not on this code).
#[cfg(test)]
pub(crate) static WORKERS_RELEASED_AT: Mutex<Option<Instant>> = Mutex::new(None);

struct LivePage;
impl LivePage {
    fn new() -> Self {
        #[cfg(test)]
        {
            let live = LIVE_PAGES.fetch_add(1, Ordering::AcqRel) + 1;
            MAX_LIVE_PAGES.fetch_max(live, Ordering::AcqRel);
        }
        Self
    }
}
impl Drop for LivePage {
    fn drop(&mut self) {
        #[cfg(test)]
        LIVE_PAGES.fetch_sub(1, Ordering::AcqRel);
    }
}

enum WorkResult {
    Page(StreamPage, LivePage),
    Pause(PauseReason),
    Cancelled,
}

struct Shared {
    next_claim: u32,
    next_write: u32,
    finished: BTreeMap<u32, WorkResult>,
    stop: bool,
    workers_running: usize,
}

/// Picks the worker count: about half the physical cores (logical / 4 with
/// SMT), at most four, and fewer when the memory budget (half of the memory
/// available now, 256 MiB per engine process plus 256 MiB for the app's page
/// buffers) is smaller.
pub(crate) fn automatic_workers() -> usize {
    let logical = std::thread::available_parallelism().map(|count| count.get()).unwrap_or(2);
    let by_cores = (logical / 4).clamp(1, MAX_JOB_WORKERS);
    let by_memory = available_memory_bytes().map(|available| {
        let budget = available / 2;
        let per_worker = crate::ocr_process::OCR_CHILD_COMMIT_LIMIT as u64;
        (budget.saturating_sub(256 * 1024 * 1024) / per_worker).max(1) as usize
    }).unwrap_or(1);
    by_cores.min(by_memory).max(1)
}

#[repr(C)]
struct MemoryStatusEx {
    length: u32,
    memory_load: u32,
    total_physical: u64,
    available_physical: u64,
    total_page_file: u64,
    available_page_file: u64,
    total_virtual: u64,
    available_virtual: u64,
    available_extended_virtual: u64,
}

#[link(name = "kernel32")]
extern "system" {
    fn GlobalMemoryStatusEx(buffer: *mut MemoryStatusEx) -> i32;
    fn SetThreadExecutionState(flags: u32) -> u32;
}

fn available_memory_bytes() -> Option<u64> {
    let mut status = MemoryStatusEx { length: std::mem::size_of::<MemoryStatusEx>() as u32, memory_load: 0, total_physical: 0, available_physical: 0, total_page_file: 0, available_page_file: 0, total_virtual: 0, available_virtual: 0, available_extended_virtual: 0 };
    (unsafe { GlobalMemoryStatusEx(&mut status) } != 0).then_some(status.available_physical)
}

const ES_CONTINUOUS: u32 = 0x8000_0000;
const ES_SYSTEM_REQUIRED: u32 = 0x0000_0001;

/// Keeps the PC from sleeping while the job runs (the display may still turn
/// off). The request belongs to the calling thread and ends on drop.
struct KeepAwake;
impl KeepAwake {
    fn new() -> Self {
        unsafe { SetThreadExecutionState(ES_CONTINUOUS | ES_SYSTEM_REQUIRED) };
        Self
    }
}
impl Drop for KeepAwake {
    fn drop(&mut self) { unsafe { SetThreadExecutionState(ES_CONTINUOUS) }; }
}

/// The pages worth reporting; `first_page` turns a part's page index into the
/// source page index.
pub(crate) fn report_of(writer_records: &[crate::searchable_pdf_stream::PageRecord], first_page: u32) -> Vec<PageReport> {
    writer_records.iter()
        .filter(|record| record.outcome != PageOutcome::Text || record.reduced_resolution || record.dropped_words > 0)
        .map(|record| PageReport { page: first_page + record.page, outcome: record.outcome, issue: record.issue, reduced_resolution: record.reduced_resolution, dropped_words: record.dropped_words })
        .collect()
}

fn pause_for(error: StreamError) -> PauseReason {
    match error {
        StreamError::SizeLimit => PauseReason::SizeLimit,
        StreamError::Io(message) => PauseReason::Disk(message),
        other => PauseReason::Publish(other.to_string()),
    }
}

/// Runs (or resumes) a job on the calling thread until it is published,
/// paused or cancelled.
pub(crate) fn run_job(
    settings: JobSettings,
    source: Arc<dyn PageSource>,
    recognizer: Arc<dyn PageRecognizer>,
    control: JobControl,
    progress: ProgressSink,
) -> JobOutcome {
    let _awake = KeepAwake::new();
    let directory = settings.directory.clone();
    let workers = settings.workers.clamp(1, MAX_JOB_WORKERS);
    let size_cap = settings.size_cap.min(MAX_STREAM_OUTPUT_BYTES);
    let opened = match settings.start {
        JobStart::New(manifest) => Output::create(&directory, manifest, size_cap).map(Opened::Output),
        JobStart::Resume => Output::resume(&directory, size_cap),
    };
    let mut writer = match opened {
        Ok(Opened::Output(output)) => output,
        Ok(Opened::AlreadyPublished { parts, report }) => {
            let _ = std::fs::remove_dir_all(&directory);
            let pages = parts.iter().map(|part| part.pages).sum();
            let bytes = parts.iter().map(|part| part.bytes).sum();
            let path = parts.first().map(|part| PathBuf::from(&part.path)).unwrap_or_default();
            progress(&JobProgress { state: JobState::Published, pages_done: pages, page_count: pages, workers, issues: report.len() as u32, bytes_written: bytes, projected_bytes: Some(bytes), size_warning: false, seconds_left: Some(0), in_parts: true, part: parts.len() as u32 });
            return JobOutcome::Published { path, pages, bytes, report, parts };
        }
        Err(error) => {
            return JobOutcome::Paused { reason: match error { StreamError::Io(message) => PauseReason::Disk(message), other => PauseReason::Publish(other.to_string()) }, next_page: 0, report: Vec::new() };
        }
    };
    let page_count = writer.manifest().page_count;
    let destination = PathBuf::from(&writer.manifest().destination);
    let first_page = writer.next_page();
    let started = Instant::now();
    let mut last_progress: Option<Instant> = None;
    let progress_control = control.clone();
    let send_progress = |state: JobState, writer: &Output, force: bool, last: &mut Option<Instant>| {
        if !force && last.is_some_and(|at| at.elapsed() < PROGRESS_INTERVAL) { return; }
        *last = Some(Instant::now());
        let done = writer.next_page();
        let written_now = done - first_page;
        let projected = (done >= PROJECTION_AFTER_PAGES).then(|| writer.bytes_written() / u64::from(done) * u64::from(page_count));
        let seconds_left = (written_now >= 5).then(|| {
            let per_page = started.elapsed().as_secs_f64() / f64::from(written_now);
            (per_page * f64::from(page_count - done)).round() as u64
        });
        let in_parts = writer.in_parts() || progress_control.may_split();
        progress(&JobProgress {
            state, pages_done: done, page_count, workers, issues: writer.report().len() as u32,
            bytes_written: writer.bytes_written(), projected_bytes: projected,
            size_warning: !in_parts && projected.is_some_and(|bytes| bytes > size_cap), seconds_left,
            in_parts, part: writer.part(),
        });
    };
    send_progress(JobState::Running, &writer, true, &mut last_progress);

    let shared = Arc::new((Mutex::new(Shared { next_claim: first_page, next_write: first_page, finished: BTreeMap::new(), stop: false, workers_running: workers }), Condvar::new()));
    let window = workers as u32 + 1;
    let mut handles = Vec::with_capacity(workers);
    for _ in 0..workers {
        let (shared, source, recognizer, control) = (shared.clone(), source.clone(), recognizer.clone(), control.clone());
        handles.push(std::thread::Builder::new().name("ocr-job-worker".into()).spawn(move || {
            worker_loop(&shared, source.as_ref(), recognizer.as_ref(), &control, page_count, window);
        }));
    }
    if handles.iter().any(Result::is_err) {
        control.pause();
    }

    let mut consecutive_failures = 0_u32;
    let mut stop_reason: Option<PauseReason> = None;
    let (lock, signal) = &*shared;
    while writer.next_page() < page_count && stop_reason.is_none() {
        let result = {
            let mut state = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            loop {
                if let Some(result) = state.finished.remove(&writer.next_page()) { break Some(result); }
                if state.workers_running == 0 || control.requested() != REQUEST_RUN { break None; }
                state = signal.wait_timeout(state, Duration::from_millis(100)).unwrap_or_else(|poisoned| poisoned.into_inner()).0;
            }
        };
        let Some(result) = result else {
            stop_reason = Some(PauseReason::User);
            break;
        };
        match result {
            WorkResult::Page(page, live) => {
                let failed = page.outcome != PageOutcome::Text;
                if let Err(error) = writer.append(&page, control.may_split()) {
                    stop_reason = Some(pause_for(error));
                    break;
                }
                drop(live);
                consecutive_failures = if failed { consecutive_failures + 1 } else { 0 };
                {
                    let mut state = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
                    state.next_write = writer.next_page();
                }
                signal.notify_all();
                if consecutive_failures >= BREAKER_PAGES {
                    stop_reason = Some(PauseReason::EngineFailing(format!("{BREAKER_PAGES} pages in a row could not be read")));
                    break;
                }
                send_progress(JobState::Running, &writer, false, &mut last_progress);
            }
            WorkResult::Pause(reason) => { stop_reason = Some(reason); break; }
            WorkResult::Cancelled => { stop_reason = Some(PauseReason::User); break; }
        }
    }
    {
        let mut state = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        state.stop = true;
        state.finished.clear();
    }
    signal.notify_all();
    if stop_reason.is_some() { control.cancellation().cancel(); }
    {
        let deadline = Instant::now() + STOP_WAIT;
        let mut state = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        while state.workers_running > 0 && Instant::now() < deadline {
            state = signal.wait_timeout(state, Duration::from_millis(20)).unwrap_or_else(|poisoned| poisoned.into_inner()).0;
        }
    }
    #[cfg(test)]
    { *WORKERS_RELEASED_AT.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(Instant::now()); }
    for handle in handles.into_iter().flatten() {
        if handle.is_finished() { let _ = handle.join(); }
    }

    if control.requested() == REQUEST_CANCEL {
        drop(writer);
        let _ = std::fs::remove_dir_all(&directory);
        progress(&JobProgress { state: JobState::Cancelled, pages_done: 0, page_count, workers, issues: 0, bytes_written: 0, projected_bytes: None, size_warning: false, seconds_left: None, in_parts: false, part: 1 });
        return JobOutcome::Cancelled;
    }
    if let Some(reason) = stop_reason {
        let checkpoint = writer.checkpoint();
        let reason = match (reason, checkpoint) { (PauseReason::User, Err(error)) => pause_for(error), (reason, _) => reason };
        send_progress(JobState::Paused, &writer, true, &mut last_progress);
        return JobOutcome::Paused { reason, next_page: writer.next_page(), report: writer.report() };
    }

    send_progress(JobState::Finalising, &writer, true, &mut last_progress);
    if writer.in_parts() {
        return match writer.publish_parts(settings.commit) {
            Ok((parts, report)) => {
                let _ = std::fs::remove_dir_all(&directory);
                let bytes = parts.iter().map(|part| part.bytes).sum();
                progress(&JobProgress { state: JobState::Published, pages_done: page_count, page_count, workers, issues: report.len() as u32, bytes_written: bytes, projected_bytes: Some(bytes), size_warning: false, seconds_left: Some(0), in_parts: true, part: parts.len() as u32 });
                JobOutcome::Published { path: parts.first().map(|part| PathBuf::from(&part.path)).unwrap_or_default(), pages: page_count, bytes, report, parts }
            }
            Err((error, report)) => {
                if control.requested() == REQUEST_CANCEL && !crate::large_ocr_parts::any_published(&directory) {
                    let _ = std::fs::remove_dir_all(&directory);
                    return JobOutcome::Cancelled;
                }
                let reason = match error { StreamError::NotCommitted(_) => PauseReason::User, StreamError::Io(message) => PauseReason::Disk(message), other => PauseReason::Publish(other.to_string()) };
                JobOutcome::Paused { reason, next_page: page_count, report }
            }
        };
    }
    let finished = match writer.finalize_single() {
        Ok(finished) => finished,
        Err(error) => {
            let resumed = crate::searchable_pdf_stream::PartialWriter::resume(&directory).map(|writer| (writer.next_page(), report_of(writer.records(), 0))).unwrap_or((0, Vec::new()));
            return JobOutcome::Paused { reason: pause_for(error), next_page: resumed.0, report: resumed.1 };
        }
    };
    let report = report_of(&finished.records, 0);
    match publish_noclobber(&finished.path, &destination, settings.commit) {
        Ok(()) => {
            let _ = std::fs::remove_dir_all(&directory);
            progress(&JobProgress { state: JobState::Published, pages_done: page_count, page_count, workers, issues: report.len() as u32, bytes_written: finished.bytes, projected_bytes: Some(finished.bytes), size_warning: false, seconds_left: Some(0), in_parts: false, part: 1 });
            JobOutcome::Published { path: destination, pages: page_count, bytes: finished.bytes, report, parts: Vec::new() }
        }
        Err(error) => {
            let _ = crate::searchable_pdf_stream::reopen_for_resume(&finished.path);
            if control.requested() == REQUEST_CANCEL {
                let _ = std::fs::remove_dir_all(&directory);
                return JobOutcome::Cancelled;
            }
            let reason = match error { StreamError::NotCommitted(_) => PauseReason::User, other => PauseReason::Publish(other.to_string()) };
            JobOutcome::Paused { reason, next_page: page_count, report }
        }
    }
}

/// Counts a worker out even if it unwinds, so the coordinator never waits for
/// a worker that no longer exists.
struct WorkerExit<'a>(&'a (Mutex<Shared>, Condvar));
impl Drop for WorkerExit<'_> {
    fn drop(&mut self) {
        let (lock, signal) = self.0;
        lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).workers_running -= 1;
        signal.notify_all();
    }
}

/// `process_page` with a panic turned into a degraded page: a white
/// placeholder of the page's size, reported as an internal error.
fn process_page_guarded(page: u32, source: &dyn PageSource, recognizer: &dyn PageRecognizer, cancellation: &OcrCancellation) -> WorkResult {
    use std::panic::{catch_unwind, AssertUnwindSafe};
    if let Ok(result) = catch_unwind(AssertUnwindSafe(|| process_page(page, source, recognizer, cancellation))) { return result; }
    match catch_unwind(AssertUnwindSafe(|| source.page_size(page))) {
        Ok(Ok((page_width, page_height))) => WorkResult::Page(StreamPage {
            page_width, page_height, image: placeholder_image(), words: Vec::new(),
            outcome: PageOutcome::Placeholder, issue: PageIssue::InternalError, reduced_resolution: false,
        }, LivePage::new()),
        Ok(Err(PageError::Cancelled)) => WorkResult::Cancelled,
        Ok(Err(PageError::Job(reason))) => WorkResult::Pause(reason),
        _ => WorkResult::Pause(PauseReason::Publish(format!("page {} hit an internal error", page + 1))),
    }
}

fn worker_loop(shared: &(Mutex<Shared>, Condvar), source: &dyn PageSource, recognizer: &dyn PageRecognizer, control: &JobControl, page_count: u32, window: u32) {
    let _exit = WorkerExit(shared);
    let (lock, signal) = shared;
    loop {
        let page = {
            let mut state = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            loop {
                if state.stop || control.requested() != REQUEST_RUN || state.next_claim >= page_count { break None; }
                if state.next_claim < state.next_write + window {
                    let page = state.next_claim;
                    state.next_claim += 1;
                    break Some(page);
                }
                state = signal.wait_timeout(state, Duration::from_millis(100)).unwrap_or_else(|poisoned| poisoned.into_inner()).0;
            }
        };
        let Some(page) = page else { break };
        let result = process_page_guarded(page, source, recognizer, control.cancellation());
        let terminal = !matches!(result, WorkResult::Page(..));
        {
            let mut state = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            if !state.stop { state.finished.insert(page, result); }
        }
        signal.notify_all();
        if terminal { break; }
    }
}

fn process_page(page: u32, source: &dyn PageSource, recognizer: &dyn PageRecognizer, cancellation: &OcrCancellation) -> WorkResult {
    let mut last_issue = PageIssue::RenderFailed;
    let mut rendered = None;
    for _ in 0..MAX_ATTEMPTS {
        if cancellation.is_cancelled() { return WorkResult::Cancelled; }
        match source.render(page, cancellation) {
            Ok(raster) => { rendered = Some(raster); break; }
            Err(PageError::Page(issue, _)) => last_issue = issue,
            Err(PageError::Job(reason)) => return WorkResult::Pause(reason),
            Err(PageError::Cancelled) => return WorkResult::Cancelled,
        }
    }
    if cancellation.is_cancelled() { return WorkResult::Cancelled; }
    let live = LivePage::new();
    let prepared = rendered.and_then(|raster| {
        let header = format!("P6\n{} {}\n255\n", raster.width, raster.height);
        let pixels = raster.p6.strip_prefix(header.as_bytes())?;
        let image = encode_image(raster.width, raster.height, pixels).ok()?;
        Some((raster, image))
    });
    let Some((raster, image)) = prepared else {
        let _ = last_issue;
        return match source.page_size(page) {
            Ok((page_width, page_height)) => WorkResult::Page(StreamPage {
                page_width, page_height, image: placeholder_image(), words: Vec::new(),
                outcome: PageOutcome::Placeholder, issue: PageIssue::RenderFailed, reduced_resolution: false,
            }, live),
            Err(PageError::Job(reason)) => WorkResult::Pause(reason),
            Err(PageError::Cancelled) => WorkResult::Cancelled,
            Err(PageError::Page(_, message)) => WorkResult::Pause(PauseReason::Document(message)),
        };
    };
    let mut words = None;
    let mut issue = PageIssue::EngineStopped;
    for _ in 0..MAX_ATTEMPTS {
        if cancellation.is_cancelled() { return WorkResult::Cancelled; }
        match recognizer.recognize_tsv(&raster.p6, cancellation) {
            Ok((tsv, skew)) => {
                let (recognized_width, recognized_height) = skew.recognition_size(raster.width, raster.height);
                let mapped = parse_tesseract_tsv(tsv.as_bytes(), recognized_width, recognized_height)
                    .and_then(|mut parsed| {
                        map_words_to_page(&mut parsed, skew, raster.width, raster.height);
                        map_words_to_display(&parsed, f64::from(raster.page_width), f64::from(raster.page_height))
                    });
                match mapped {
                    Ok(display) => {
                        words = Some(display.into_iter().map(|word| SearchableWord { x: word.x, y: word.y, width: word.width, height: word.height, text: word.text }).collect::<Vec<_>>());
                        break;
                    }
                    Err(_) => issue = PageIssue::EngineOutputRefused,
                }
            }
            Err(PageError::Page(failure, _)) => issue = failure,
            Err(PageError::Job(reason)) => return WorkResult::Pause(reason),
            Err(PageError::Cancelled) => return WorkResult::Cancelled,
        }
    }
    if cancellation.is_cancelled() { return WorkResult::Cancelled; }
    let (outcome, issue, words) = match words {
        Some(words) => (PageOutcome::Text, PageIssue::None, words),
        None => (PageOutcome::ImageOnly, issue, Vec::new()),
    };
    WorkResult::Page(StreamPage {
        page_width: raster.page_width, page_height: raster.page_height, image, words, outcome, issue,
        reduced_resolution: raster.reduced_resolution,
    }, live)
}

/// Lists unfinished job folders under `root` (each holds a manifest).
pub(crate) fn unfinished_jobs(root: &Path) -> Vec<(PathBuf, JobManifest)> {
    let Ok(entries) = std::fs::read_dir(root) else { return Vec::new() };
    let mut jobs = entries.flatten()
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
        .filter_map(|entry| crate::searchable_pdf_stream::read_manifest(&entry.path()).ok().map(|manifest| (entry.path(), manifest)))
        .collect::<Vec<_>>();
    jobs.sort_by(|left, right| left.0.cmp(&right.0));
    jobs
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::searchable_pdf_stream::tests::manifest;
    use crate::searchable_pdf_stream::PartialWriter;
    use std::sync::atomic::{AtomicU32, AtomicUsize};

    /// Synthetic pages: a 200 x 260 pixel raster (gray, bilevel or colour by
    /// page) on a 96 x 124.8 pt page (150 DPI would be 200 x 260).
    struct FakeSource {
        fail_render: Vec<u32>,
        render_calls: AtomicU32,
        stop_document_at: Option<u32>,
        panic_at: Option<u32>,
        hang_at: Option<u32>,
    }

    impl FakeSource {
        fn new() -> Self { Self { fail_render: Vec::new(), render_calls: AtomicU32::new(0), stop_document_at: None, panic_at: None, hang_at: None } }
    }

    fn raster(page: u32) -> Vec<u8> {
        let (width, height) = (200_u32, 260_u32);
        let mut p6 = format!("P6\n{width} {height}\n255\n").into_bytes();
        for y in 0..height {
            for x in 0..width {
                let value = ((x + 2 * y + page * 13) % 251) as u8;
                if y == 0 && x < 32 {
                    p6.extend_from_slice(&[if page >> x & 1 == 1 { 0 } else { 255 }; 3]);
                    continue;
                }
                match page % 3 {
                    0 => p6.extend_from_slice(&[if (x / 8 + y / 8 + page) % 2 == 0 { 0 } else { 255 }; 3]),
                    1 => p6.extend_from_slice(&[value; 3]),
                    _ => p6.extend_from_slice(&[value, value / 2, 255 - value]),
                }
            }
        }
        p6
    }

    impl PageSource for FakeSource {
        fn page_size(&self, _page: u32) -> Result<(f32, f32), PageError> { Ok((96.0, 124.8)) }
        fn render(&self, page: u32, cancellation: &OcrCancellation) -> Result<RenderedPage, PageError> {
            self.render_calls.fetch_add(1, Ordering::AcqRel);
            if cancellation.is_cancelled() { return Err(PageError::Cancelled); }
            if self.stop_document_at == Some(page) { return Err(PageError::Job(PauseReason::Document("Document changed. Start OCR again.".into()))); }
            if self.fail_render.contains(&page) { return Err(PageError::Page(PageIssue::RenderFailed, "render crashed".into())); }
            if self.panic_at == Some(page) { panic!("simulated bug while rendering page {page}"); }
            if self.hang_at == Some(page) { std::thread::sleep(Duration::from_secs(8)); }
            Ok(RenderedPage { width: 200, height: 260, page_width: 96.0, page_height: 124.8, p6: raster(page), reduced_resolution: page % 97 == 5 })
        }
    }

    #[derive(Clone, Copy, PartialEq)]
    enum Failure { Timeout, Crash, Garbage, FirstAttemptOnly }

    struct FakeEngine {
        failures: Vec<(u32, Failure)>,
        attempts: Mutex<std::collections::HashMap<u32, u32>>,
        calls: AtomicU32,
        delay: Duration,
        hang_after_calls: Option<u32>,
        broken: bool,
    }

    impl FakeEngine {
        fn new() -> Self { Self { failures: Vec::new(), attempts: Mutex::new(Default::default()), calls: AtomicU32::new(0), delay: Duration::ZERO, hang_after_calls: None, broken: false } }
    }

    fn page_of(p6: &[u8]) -> u32 {
        let header = "P6
200 260
255
".len();
        (0..32).filter(|bit| p6[header + 3 * bit] == 0).fold(0, |page, bit| page | 1 << bit)
    }

    fn tsv(page: u32) -> String {
        let header = "level\tpage_num\tblock_num\tpar_num\tline_num\tword_num\tleft\ttop\twidth\theight\tconf\ttext";
        let words = [format!("Page{}", page + 1), ["café", "naïve", "“ok”", "Straße", "€½"][page as usize % 5].to_string(), format!("ref{:x}", page.wrapping_mul(2_654_435_761))];
        let mut rows = vec![
            header.to_string(),
            "1\t1\t0\t0\t0\t0\t0\t0\t200\t260\t-1\t".into(),
            "2\t1\t1\t0\t0\t0\t10\t10\t180\t30\t-1\t".into(),
            "3\t1\t1\t1\t0\t0\t10\t10\t180\t30\t-1\t".into(),
            "4\t1\t1\t1\t1\t0\t10\t10\t180\t30\t-1\t".into(),
        ];
        for (index, word) in words.iter().enumerate() {
            rows.push(format!("5\t1\t1\t1\t1\t{}\t{}\t12\t50\t20\t91\t{word}", index + 1, 10 + index * 60));
        }
        rows.join("\n")
    }

    impl PageRecognizer for FakeEngine {
        fn recognize_tsv(&self, p6: &[u8], cancellation: &OcrCancellation) -> Result<(String, Skew), PageError> {
            let call = self.calls.fetch_add(1, Ordering::AcqRel) + 1;
            if self.broken { return Err(PageError::Page(PageIssue::EngineStopped, "the engine failed to start".into())); }
            let page = page_of(p6);
            let attempt = { let mut attempts = self.attempts.lock().unwrap(); let entry = attempts.entry(page).or_default(); *entry += 1; *entry };
            let deadline = Instant::now() + self.delay;
            while Instant::now() < deadline || self.hang_after_calls.is_some_and(|limit| call > limit) {
                if cancellation.is_cancelled() { return Err(PageError::Cancelled); }
                std::thread::sleep(Duration::from_millis(1));
            }
            match self.failures.iter().find(|(failing, _)| *failing == page).map(|(_, failure)| *failure) {
                Some(Failure::Timeout) => Err(PageError::Page(PageIssue::TimedOut, "timed out".into())),
                Some(Failure::Crash) => Err(PageError::Page(PageIssue::EngineStopped, "access violation".into())),
                Some(Failure::Garbage) => Ok(("not a tsv".into(), Skew::NONE)),
                Some(Failure::FirstAttemptOnly) if attempt == 1 => Err(PageError::Page(PageIssue::TimedOut, "timed out".into())),
                _ => Ok((tsv(page), Skew::NONE)),
            }
        }
    }

    fn settings(directory: &Path, pages: u32, destination: &Path, workers: usize, resume: bool) -> JobSettings {
        JobSettings {
            directory: directory.to_path_buf(),
            start: if resume { JobStart::Resume } else { JobStart::New(manifest(pages, destination)) },
            workers,
            commit: Box::new(|| Ok(())),
            size_cap: MAX_STREAM_OUTPUT_BYTES,
        }
    }

    fn silent() -> ProgressSink { Arc::new(|_: &JobProgress| {}) }

    fn when_calls_reach(engine: Arc<FakeEngine>, calls: u32, action: impl FnOnce() + Send + 'static) -> std::thread::JoinHandle<()> {
        std::thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(120);
            while engine.calls.load(Ordering::Acquire) < calls && Instant::now() < deadline { std::thread::sleep(Duration::from_millis(1)); }
            action();
        })
    }

    fn run(directory: &Path, pages: u32, destination: &Path, workers: usize, source: FakeSource, engine: FakeEngine) -> JobOutcome {
        run_job(settings(directory, pages, destination, workers, false), Arc::new(source), Arc::new(engine), JobControl::default(), silent())
    }

    #[test]
    fn a_multi_hundred_page_job_completes_with_bounded_pages_in_memory() {
        let _serial = JOB_TEST_LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let folder = tempfile::tempdir().unwrap();
        let destination = folder.path().join("big-searchable.pdf");
        MAX_LIVE_PAGES.store(0, Ordering::Release);
        let updates = Arc::new(AtomicUsize::new(0));
        let counter = updates.clone();
        let progress: ProgressSink = Arc::new(move |_: &JobProgress| { counter.fetch_add(1, Ordering::AcqRel); });
        let outcome = run_job(settings(&folder.path().join("job"), 400, &destination, 3, false), Arc::new(FakeSource::new()), Arc::new(FakeEngine::new()), JobControl::default(), progress);
        let JobOutcome::Published { pages, report, .. } = outcome else { panic!("{outcome:?}") };
        assert_eq!(pages, 400);
        assert!(report.iter().all(|entry| entry.outcome == PageOutcome::Text && entry.reduced_resolution), "{report:?}");
        assert_eq!(report.iter().map(|entry| entry.page).collect::<Vec<_>>(), vec![5, 102, 199, 296, 393], "pages rendered at reduced resolution are listed");
        let peak = MAX_LIVE_PAGES.load(Ordering::Acquire);
        assert!((1..=3 + 1).contains(&peak), "at most workers + 1 pages in memory, saw {peak}");
        assert!(updates.load(Ordering::Acquire) >= 3);
        assert!(!folder.path().join("job").exists(), "the job folder is removed after publishing");
        let document = lopdf::Document::load(&destination).unwrap();
        assert_eq!(document.get_pages().len(), 400);
    }

    pub(crate) static JOB_TEST_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn output_does_not_depend_on_worker_count() {
        let _serial = JOB_TEST_LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let folder = tempfile::tempdir().unwrap();
        let mut outputs = Vec::new();
        for workers in [1, 4] {
            let destination = folder.path().join(format!("w{workers}.pdf"));
            let outcome = run(&folder.path().join(format!("job{workers}")), 60, &destination, workers, FakeSource::new(), FakeEngine::new());
            assert!(matches!(outcome, JobOutcome::Published { .. }), "{outcome:?}");
            outputs.push(std::fs::read(destination).unwrap());
        }
        assert!(outputs[0] == outputs[1]);
    }

    #[test]
    fn failing_pages_are_degraded_reported_and_the_job_continues() {
        let _serial = JOB_TEST_LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let folder = tempfile::tempdir().unwrap();
        let destination = folder.path().join("out.pdf");
        let mut source = FakeSource::new();
        source.fail_render = vec![40];
        let mut engine = FakeEngine::new();
        engine.failures = vec![(7, Failure::Timeout), (8, Failure::Crash), (90, Failure::Garbage), (120, Failure::FirstAttemptOnly)];
        let outcome = run(&folder.path().join("job"), 150, &destination, 2, source, engine);
        let JobOutcome::Published { report, .. } = outcome else { panic!("{outcome:?}") };
        let failures = report.iter().filter(|entry| !entry.reduced_resolution).map(|entry| (entry.page, entry.outcome, entry.issue)).collect::<Vec<_>>();
        assert_eq!(failures, vec![
            (7, PageOutcome::ImageOnly, PageIssue::TimedOut),
            (8, PageOutcome::ImageOnly, PageIssue::EngineStopped),
            (40, PageOutcome::Placeholder, PageIssue::RenderFailed),
            (90, PageOutcome::ImageOnly, PageIssue::EngineOutputRefused),
        ], "page 120 succeeded on its retry");
        let document = lopdf::Document::load(&destination).unwrap();
        assert_eq!(document.get_pages().len(), 150);
    }

    #[test]
    fn five_failures_in_a_row_pause_the_job_and_resume_finishes_it() {
        let _serial = JOB_TEST_LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let folder = tempfile::tempdir().unwrap();
        let destination = folder.path().join("out.pdf");
        let directory = folder.path().join("job");
        let mut engine = FakeEngine::new();
        engine.failures = (30..40).map(|page| (page, Failure::Crash)).collect();
        let outcome = run(&directory, 80, &destination, 2, FakeSource::new(), engine);
        let JobOutcome::Paused { reason: PauseReason::EngineFailing(_), next_page, .. } = outcome else { panic!("{outcome:?}") };
        assert_eq!(next_page, 35);
        assert!(!destination.exists());
        let outcome = run_job(settings(&directory, 80, &destination, 2, true), Arc::new(FakeSource::new()), Arc::new(FakeEngine::new()), JobControl::default(), silent());
        let JobOutcome::Published { report, .. } = outcome else { panic!("{outcome:?}") };
        assert_eq!(report.iter().filter(|entry| entry.outcome == PageOutcome::ImageOnly).count(), 5);
    }

    #[test]
    fn cancel_mid_job_stops_workers_and_leaves_nothing() {
        let _serial = JOB_TEST_LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let folder = tempfile::tempdir().unwrap();
        let destination = folder.path().join("out.pdf");
        let directory = folder.path().join("job");
        let mut engine = FakeEngine::new();
        engine.delay = Duration::from_millis(5);
        engine.hang_after_calls = Some(60);
        let engine = Arc::new(engine);
        let control = JobControl::default();
        let watcher = when_calls_reach(engine.clone(), 40, { let control = control.clone(); move || control.cancel() });
        let started = Instant::now();
        let outcome = run_job(settings(&directory, 300, &destination, 3, false), Arc::new(FakeSource::new()), engine, control, silent());
        watcher.join().unwrap();
        assert_eq!(outcome, JobOutcome::Cancelled);
        assert!(started.elapsed() < Duration::from_secs(60));
        assert!(!directory.exists() && !destination.exists());
    }

    #[test]
    fn pause_then_resume_and_a_simulated_crash_both_give_the_clean_output() {
        let _serial = JOB_TEST_LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let folder = tempfile::tempdir().unwrap();
        let clean = folder.path().join("clean.pdf");
        assert!(matches!(run(&folder.path().join("clean-job"), 120, &clean, 2, FakeSource::new(), FakeEngine::new()), JobOutcome::Published { .. }));
        let clean = std::fs::read(clean).unwrap();

        let destination = folder.path().join("paused.pdf");
        let directory = folder.path().join("paused-job");
        let control = JobControl::default();
        let mut engine = FakeEngine::new();
        engine.delay = Duration::from_millis(2);
        engine.hang_after_calls = Some(70);
        let engine = Arc::new(engine);
        let watcher = when_calls_reach(engine.clone(), 60, { let control = control.clone(); move || control.pause() });
        let outcome = run_job(settings(&directory, 120, &destination, 2, false), Arc::new(FakeSource::new()), engine, control, silent());
        watcher.join().unwrap();
        let JobOutcome::Paused { reason: PauseReason::User, next_page, .. } = outcome else { panic!("{outcome:?}") };
        assert!((50..=70).contains(&next_page), "{next_page}");
        assert!(!destination.exists());
        assert!(matches!(run_job(settings(&directory, 120, &destination, 3, true), Arc::new(FakeSource::new()), Arc::new(FakeEngine::new()), JobControl::default(), silent()), JobOutcome::Published { .. }));
        assert!(std::fs::read(&destination).unwrap() == clean);

        // Crash: stop without a checkpoint (the app is killed), then tear the
        // last journal record and part bytes as a power loss could.
        let destination = folder.path().join("crashed.pdf");
        let directory = folder.path().join("crashed-job");
        let mut writer = PartialWriter::create(&directory, manifest(120, &destination)).unwrap();
        let source = FakeSource::new();
        let engine = FakeEngine::new();
        for page in 0..70 {
            let WorkResult::Page(page, _) = process_page(page, &source, &engine, &OcrCancellation::default()) else { panic!() };
            writer.append(&page).unwrap();
            if writer.next_page() == 60 { writer.checkpoint().unwrap(); }
        }
        drop(writer);
        let journal = directory.join(crate::searchable_pdf_stream::JOURNAL_FILE);
        let length = std::fs::metadata(&journal).unwrap().len();
        std::fs::OpenOptions::new().write(true).open(&journal).unwrap().set_len(length - 3).unwrap();
        let outcome = run_job(settings(&directory, 120, &destination, 2, true), Arc::new(FakeSource::new()), Arc::new(FakeEngine::new()), JobControl::default(), silent());
        assert!(matches!(outcome, JobOutcome::Published { .. }), "{outcome:?}");
        assert!(std::fs::read(&destination).unwrap() == clean);
    }

    #[test]
    fn a_document_change_pauses_instead_of_degrading_pages() {
        let _serial = JOB_TEST_LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let folder = tempfile::tempdir().unwrap();
        let mut source = FakeSource::new();
        source.stop_document_at = Some(25);
        let outcome = run(&folder.path().join("job"), 60, &folder.path().join("out.pdf"), 2, source, FakeEngine::new());
        let JobOutcome::Paused { reason: PauseReason::Document(_), next_page, report } = outcome else { panic!("{outcome:?}") };
        assert_eq!(next_page, 25);
        assert!(report.iter().all(|entry| entry.outcome == PageOutcome::Text));
    }

    #[test]
    fn a_hanging_engine_is_stopped_by_pause_within_a_second() {
        let _serial = JOB_TEST_LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let folder = tempfile::tempdir().unwrap();
        let mut engine = FakeEngine::new();
        engine.hang_after_calls = Some(10);
        let engine = Arc::new(engine);
        let control = JobControl::default();
        let paused_at = Arc::new(Mutex::new(None::<Instant>));
        let watcher = when_calls_reach(engine.clone(), 12, {
            let (control, paused_at) = (control.clone(), paused_at.clone());
            move || { std::thread::sleep(Duration::from_millis(200)); *paused_at.lock().unwrap() = Some(Instant::now()); control.pause(); }
        });
        let outcome = run_job(settings(&folder.path().join("job"), 100, &folder.path().join("out.pdf"), 2, false), Arc::new(FakeSource::new()), engine, control, silent());
        let stopped = Instant::now();
        watcher.join().unwrap();
        assert!(matches!(outcome, JobOutcome::Paused { reason: PauseReason::User, next_page: 10, .. }), "{outcome:?}");
        let paused = paused_at.lock().unwrap().unwrap();
        assert!(stopped.saturating_duration_since(paused) < Duration::from_secs(1), "{:?}", stopped.saturating_duration_since(paused));
    }

    #[test]
    fn a_full_disk_pauses_and_resume_completes() {
        let _serial = JOB_TEST_LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let folder = tempfile::tempdir().unwrap();
        let destination = folder.path().join("out.pdf");
        let directory = folder.path().join("job");
        let mut writer = PartialWriter::create(&directory, manifest(40, &destination)).unwrap();
        let (source, engine) = (FakeSource::new(), FakeEngine::new());
        writer.fail_write_at = Some(20_000);
        let mut paused = None;
        for page in 0..40 {
            let WorkResult::Page(page, _) = process_page(page, &source, &engine, &OcrCancellation::default()) else { panic!() };
            if let Err(error) = writer.append(&page) { paused = Some(pause_for(error)); break; }
        }
        assert!(matches!(paused, Some(PauseReason::Disk(_))));
        writer.checkpoint().unwrap();
        drop(writer);
        let outcome = run_job(settings(&directory, 40, &destination, 2, true), Arc::new(FakeSource::new()), Arc::new(FakeEngine::new()), JobControl::default(), silent());
        assert!(matches!(outcome, JobOutcome::Published { .. }), "{outcome:?}");
    }

    #[test]
    fn an_existing_destination_pauses_and_keeps_the_work() {
        let _serial = JOB_TEST_LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let folder = tempfile::tempdir().unwrap();
        let destination = folder.path().join("out.pdf");
        let directory = folder.path().join("job");
        let blocker = destination.clone();
        let mut job = settings(&directory, 20, &destination, 2, false);
        job.commit = Box::new(move || { std::fs::write(&blocker, b"someone else").unwrap(); Ok(()) });
        let outcome = run_job(job, Arc::new(FakeSource::new()), Arc::new(FakeEngine::new()), JobControl::default(), silent());
        assert!(matches!(outcome, JobOutcome::Paused { reason: PauseReason::Publish(_), next_page: 20, .. }), "{outcome:?}");
        assert_eq!(std::fs::read(&destination).unwrap(), b"someone else");
        std::fs::remove_file(&destination).unwrap();
        let outcome = run_job(settings(&directory, 20, &destination, 2, true), Arc::new(FakeSource::new()), Arc::new(FakeEngine::new()), JobControl::default(), silent());
        assert!(matches!(outcome, JobOutcome::Published { .. }), "{outcome:?}");
    }

    #[test]
    fn a_panicking_page_becomes_a_reported_blank_page_and_the_job_finishes() {
        let _serial = JOB_TEST_LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let folder = tempfile::tempdir().unwrap();
        let mut source = FakeSource::new();
        source.panic_at = Some(17);
        let outcome = run(&folder.path().join("job"), 40, &folder.path().join("out.pdf"), 3, source, FakeEngine::new());
        let JobOutcome::Published { report, .. } = outcome else { panic!("{outcome:?}") };
        let failures = report.iter().filter(|entry| !entry.reduced_resolution).map(|entry| (entry.page, entry.outcome, entry.issue)).collect::<Vec<_>>();
        assert_eq!(failures, vec![(17, PageOutcome::Placeholder, PageIssue::InternalError)]);
    }

    #[test]
    fn pause_answers_promptly_even_when_a_render_cannot_be_interrupted() {
        let _serial = JOB_TEST_LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let folder = tempfile::tempdir().unwrap();
        let mut source = FakeSource::new();
        source.hang_at = Some(6);
        *WORKERS_RELEASED_AT.lock().unwrap() = None;
        let engine = Arc::new(FakeEngine::new());
        let control = JobControl::default();
        let paused_at = Arc::new(Mutex::new(None::<Instant>));
        let watcher = when_calls_reach(engine.clone(), 6, {
            let (control, paused_at) = (control.clone(), paused_at.clone());
            move || { std::thread::sleep(Duration::from_millis(300)); *paused_at.lock().unwrap() = Some(Instant::now()); control.pause(); }
        });
        let outcome = run_job(settings(&folder.path().join("job"), 30, &folder.path().join("out.pdf"), 1, false), Arc::new(source), engine, control, silent());
        watcher.join().unwrap();
        assert!(matches!(outcome, JobOutcome::Paused { reason: PauseReason::User, next_page: 6, .. }), "{outcome:?}");
        let released = WORKERS_RELEASED_AT.lock().unwrap().expect("the stop recorded when it released the workers");
        let waited = released.saturating_duration_since(paused_at.lock().unwrap().unwrap());
        // The coordinator notices the pause within its 100 ms wait, then waits
        // at most STOP_WAIT for the stuck worker; 400 ms covers scheduling.
        assert!(waited >= STOP_WAIT && waited < STOP_WAIT + Duration::from_millis(400), "{waited:?}");
    }

    const SPLIT_PAGES: u32 = 60;
    /// The 60 synthetic pages make a copy of about 86 KB, so this cap splits
    /// it into several parts.
    const SPLIT_CAP: u64 = 25_000;

    fn split_settings(directory: &Path, destination: &Path, resume: bool) -> JobSettings {
        let mut job = settings(directory, SPLIT_PAGES, destination, 2, resume);
        job.size_cap = SPLIT_CAP;
        job
    }

    fn in_parts() -> JobControl {
        let control = JobControl::default();
        control.save_in_parts();
        control
    }

    fn run_in_parts(directory: &Path, destination: &Path, resume: bool) -> JobOutcome {
        run_job(split_settings(directory, destination, resume), Arc::new(FakeSource::new()), Arc::new(FakeEngine::new()), in_parts(), silent())
    }

    fn published_parts(outcome: JobOutcome) -> Vec<PublishedPart> {
        let JobOutcome::Published { parts, pages, .. } = outcome else { panic!("{outcome:?}") };
        assert_eq!(pages, SPLIT_PAGES);
        parts
    }

    fn contents(parts: &[PublishedPart]) -> Vec<Vec<u8>> { parts.iter().map(|part| std::fs::read(&part.path).unwrap()).collect() }

    /// A clean run in parts, kept for byte comparisons.
    fn clean_parts(folder: &Path) -> (Vec<PublishedPart>, Vec<Vec<u8>>) {
        let parts = published_parts(run_in_parts(&folder.join("clean-job"), &folder.join("clean.pdf"), false));
        let bytes = contents(&parts);
        (parts, bytes)
    }

    #[test]
    fn a_small_cap_saves_valid_parts_with_page_ranges_and_text() {
        let _pdfium = crate::ocr_process::test_lock();
        let _serial = JOB_TEST_LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let folder = tempfile::tempdir().unwrap();
        let destination = folder.path().join("big scan.pdf");
        let directory = folder.path().join("job");
        let mut source = FakeSource::new();
        source.fail_render = vec![33];
        let outcome = run_job(split_settings(&directory, &destination, false), Arc::new(source), Arc::new(FakeEngine::new()), in_parts(), silent());
        let JobOutcome::Published { parts, report, bytes, path, .. } = outcome else { panic!("{outcome:?}") };
        let count = parts.len();
        eprintln!("bytes {bytes} parts: {:?}", parts.iter().map(|part| (part.first_page, part.pages, part.bytes)).collect::<Vec<_>>());
        assert!(count >= 3, "{count} parts");
        assert_eq!(path, folder.path().join(format!("big scan (part 1 of {count}).pdf")));
        assert!(!destination.exists(), "the chosen name itself is not written");
        assert!(!directory.exists(), "the job folder is removed after publishing");
        assert_eq!(bytes, parts.iter().map(|part| part.bytes).sum::<u64>());
        assert!(report.iter().any(|entry| entry.page == 33 && entry.outcome == PageOutcome::Placeholder), "report pages are source pages: {report:?}");
        let service = crate::service::PdfService::start(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("resources/pdfium/bin/pdfium.dll"));
        let mut next = 0;
        for (index, part) in parts.iter().enumerate() {
            assert_eq!(part.path, folder.path().join(format!("big scan (part {} of {count}).pdf", index + 1)).to_string_lossy());
            assert_eq!(part.first_page, next, "parts are contiguous");
            next += part.pages;
            let file = std::fs::read(&part.path).unwrap();
            assert_eq!(file.len() as u64, part.bytes);
            assert!(part.bytes <= SPLIT_CAP, "part {} is {} bytes", index + 1, part.bytes);
            assert_eq!(lopdf::Document::load_mem(&file).unwrap().get_pages().len(), part.pages as usize);
            let opened = tauri::async_runtime::block_on(service.open(PathBuf::from(&part.path))).unwrap();
            let request = opened.ocr_request(0);
            for local in [0, part.pages - 1] {
                let source_page = part.first_page + local;
                let text = tauri::async_runtime::block_on(service.text(request.document_id, local as u16, request.revision)).unwrap();
                if source_page == 33 { assert!(!text.contains("Page34"), "{text:?}"); continue; }
                assert!(text.contains(&format!("Page{}", source_page + 1)), "part {} page {local}: {text:?}", index + 1);
            }
            tauri::async_runtime::block_on(service.close(request.document_id)).unwrap();
        }
        assert_eq!(next, SPLIT_PAGES);
    }

    #[test]
    fn the_cap_pauses_a_single_copy_and_save_in_parts_continues_it() {
        let _serial = JOB_TEST_LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let folder = tempfile::tempdir().unwrap();
        let (clean, clean_bytes) = clean_parts(folder.path());
        for part in &clean { std::fs::remove_file(&part.path).unwrap(); }
        let destination = folder.path().join("clean.pdf");
        let directory = folder.path().join("job");
        let outcome = run_job(split_settings(&directory, &destination, false), Arc::new(FakeSource::new()), Arc::new(FakeEngine::new()), JobControl::default(), silent());
        let JobOutcome::Paused { reason: PauseReason::SizeLimit, next_page, .. } = outcome else { panic!("{outcome:?}") };
        assert_eq!(next_page, clean[0].pages, "the single copy stops where part 1 ends");
        assert!(!directory.join(crate::large_ocr_parts::PARTS_FILE).exists());
        let resumed = published_parts(run_in_parts(&directory, &destination, true));
        assert_eq!(resumed, clean);
        assert!(contents(&resumed) == clean_bytes);
    }

    #[test]
    fn a_crash_mid_part_two_resumes_to_identical_parts() {
        let _serial = JOB_TEST_LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let folder = tempfile::tempdir().unwrap();
        let (clean, clean_bytes) = clean_parts(folder.path());
        for part in &clean { std::fs::remove_file(&part.path).unwrap(); }
        let destination = folder.path().join("clean.pdf");
        let directory = folder.path().join("job");
        let part_two = clean[1].first_page..clean[1].first_page + clean[1].pages;
        let target = part_two.start + clean[1].pages / 2;
        let mut engine = FakeEngine::new();
        engine.delay = Duration::from_millis(2);
        engine.hang_after_calls = Some(target + 1);
        let engine = Arc::new(engine);
        let control = in_parts();
        let watcher = when_calls_reach(engine.clone(), target, { let control = control.clone(); move || control.pause() });
        let outcome = run_job(split_settings(&directory, &destination, false), Arc::new(FakeSource::new()), engine, control, silent());
        watcher.join().unwrap();
        let JobOutcome::Paused { reason: PauseReason::User, next_page, .. } = outcome else { panic!("{outcome:?}") };
        assert!(part_two.contains(&next_page) && next_page > part_two.start, "paused at {next_page}, part 2 is {part_two:?}");
        assert!(directory.join(crate::large_ocr_parts::PARTS_FILE).exists() && directory.join("part-2").is_dir());
        // A power loss: the last journal record of part 2 is torn.
        let journal = directory.join("part-2").join(crate::searchable_pdf_stream::JOURNAL_FILE);
        let length = std::fs::metadata(&journal).unwrap().len();
        std::fs::OpenOptions::new().write(true).open(&journal).unwrap().set_len(length - 3).unwrap();
        assert_eq!(crate::large_ocr_parts::saved_progress(&directory, &crate::searchable_pdf_stream::read_manifest(&directory).unwrap()), next_page - 1);
        let resumed = published_parts(run_in_parts(&directory, &destination, true));
        assert_eq!(resumed, clean);
        assert!(contents(&resumed) == clean_bytes, "resumed parts differ from the uninterrupted run");
    }

    #[test]
    fn existing_part_names_are_never_overwritten() {
        let _serial = JOB_TEST_LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let folder = tempfile::tempdir().unwrap();
        let (clean, clean_bytes) = clean_parts(folder.path());
        for part in &clean { std::fs::remove_file(&part.path).unwrap(); }
        let destination = folder.path().join("clean.pdf");
        let names = clean.iter().map(|part| PathBuf::from(&part.path)).collect::<Vec<_>>();

        // A name taken before publishing: nothing is published.
        let directory = folder.path().join("job-a");
        std::fs::write(&names[1], b"someone else's part 2").unwrap();
        let outcome = run_in_parts(&directory, &destination, false);
        let JobOutcome::Paused { reason: PauseReason::Publish(message), next_page, .. } = outcome else { panic!("{outcome:?}") };
        assert_eq!(next_page, SPLIT_PAGES);
        assert!(message.contains("already exists"), "{message}");
        assert!(!names[0].exists(), "no part is published while a name is taken");
        assert_eq!(std::fs::read(&names[1]).unwrap(), b"someone else's part 2");
        std::fs::remove_file(&names[1]).unwrap();
        assert!(contents(&published_parts(run_in_parts(&directory, &destination, true))) == clean_bytes);
        for name in &names { std::fs::remove_file(name).unwrap(); }

        // A name taken between the check and the move: part 1 is saved, part 2 is not, and resume finishes the set.
        let directory = folder.path().join("job-b");
        let blocker = names[1].clone();
        let mut job = split_settings(&directory, &destination, false);
        job.commit = Box::new(move || { std::fs::write(&blocker, b"raced").unwrap(); Ok(()) });
        let outcome = run_job(job, Arc::new(FakeSource::new()), Arc::new(FakeEngine::new()), in_parts(), silent());
        assert!(matches!(outcome, JobOutcome::Paused { reason: PauseReason::Publish(_), next_page: SPLIT_PAGES, .. }), "{outcome:?}");
        assert!(std::fs::read(&names[0]).unwrap() == clean_bytes[0]);
        assert_eq!(std::fs::read(&names[1]).unwrap(), b"raced");
        assert!(crate::large_ocr_parts::any_published(&directory));
        std::fs::remove_file(&names[1]).unwrap();
        let resumed = published_parts(run_in_parts(&directory, &destination, true));
        assert_eq!(resumed, clean);
        assert!(contents(&resumed) == clean_bytes);
    }

    #[test]
    fn every_part_is_checked_from_disk_before_any_is_published() {
        let _serial = JOB_TEST_LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let folder = tempfile::tempdir().unwrap();
        let destination = folder.path().join("out.pdf");
        let directory = folder.path().join("job");
        let mut job = split_settings(&directory, &destination, false);
        job.commit = Box::new(|| Err("OCR was paused".into()));
        let outcome = run_job(job, Arc::new(FakeSource::new()), Arc::new(FakeEngine::new()), in_parts(), silent());
        assert!(matches!(outcome, JobOutcome::Paused { reason: PauseReason::User, next_page: SPLIT_PAGES, .. }), "{outcome:?}");
        let part_two = directory.join("part-2");
        let records = crate::searchable_pdf_stream::saved_records(&part_two);
        let part_file = part_two.join(crate::searchable_pdf_stream::PART_FILE);
        let mut bytes = std::fs::read(&part_file).unwrap();
        assert!(bytes.starts_with(b"%PDW-PARTIAL"), "a refused publish turns the checked parts back into partial copies");
        let image = records[0].end_offset() as usize;
        let at = image + bytes[image..].windows(7).position(|window| window == b"stream\n").unwrap() + 7 + 10;
        bytes[at] ^= 0x40;
        std::fs::write(&part_file, bytes).unwrap();
        let outcome = run_in_parts(&directory, &destination, true);
        let JobOutcome::Paused { reason: PauseReason::Publish(message), .. } = outcome else { panic!("{outcome:?}") };
        assert!(message.contains("final check"), "{message}");
        let published = std::fs::read_dir(folder.path()).unwrap().flatten().filter(|entry| entry.file_name().to_string_lossy().ends_with(".pdf")).count();
        assert_eq!(published, 0, "nothing is published when any part fails its check");
    }

    /// Writes every page with the publish refused, so every part is on disk
    /// and partial; returns the job's parts record.
    fn all_pages_unpublished(directory: &Path, destination: &Path) -> crate::large_ocr_parts::PartsState {
        let mut job = split_settings(directory, destination, false);
        job.commit = Box::new(|| Err("OCR was paused".into()));
        let outcome = run_job(job, Arc::new(FakeSource::new()), Arc::new(FakeEngine::new()), in_parts(), silent());
        assert!(matches!(outcome, JobOutcome::Paused { reason: PauseReason::User, next_page: SPLIT_PAGES, .. }), "{outcome:?}");
        let manifest = crate::searchable_pdf_stream::read_manifest(directory).unwrap();
        crate::large_ocr_parts::load_state(directory, &manifest).unwrap().unwrap()
    }

    #[test]
    fn a_crash_between_moving_a_part_and_recording_it_is_recognised() {
        let _serial = JOB_TEST_LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let folder = tempfile::tempdir().unwrap();
        let (clean, clean_bytes) = clean_parts(folder.path());
        for part in &clean { std::fs::remove_file(&part.path).unwrap(); }
        let destination = folder.path().join("clean.pdf");
        let first = PathBuf::from(&clean[0].path);
        for (case, job) in [("moved", "job-a"), ("copied", "job-b"), ("someone else's", "job-c")] {
            let directory = folder.path().join(job);
            let mut state = all_pages_unpublished(&directory, &destination);
            let finished = PartialWriter::resume(&directory).unwrap().finalize_pages(state.closed[0]).unwrap();
            match case {
                "moved" => std::fs::rename(&finished.path, &first).unwrap(),
                "copied" => { std::fs::copy(&finished.path, &first).unwrap(); }
                _ => std::fs::write(&first, vec![b'x'; finished.bytes as usize]).unwrap(),
            }
            state.publishing = Some(finished.bytes);
            std::fs::write(directory.join(crate::large_ocr_parts::PARTS_FILE), serde_json::to_vec(&state).unwrap()).unwrap();
            let outcome = run_in_parts(&directory, &destination, true);
            if case == "someone else's" {
                let JobOutcome::Paused { reason: PauseReason::Publish(message), .. } = outcome else { panic!("{outcome:?}") };
                assert!(message.contains("already exists"), "{message}");
                assert_eq!(std::fs::read(&first).unwrap(), vec![b'x'; finished.bytes as usize], "a same-size file with other contents is not ours and is kept");
                continue;
            }
            let parts = published_parts(outcome);
            assert_eq!(parts, clean, "{case}");
            assert!(contents(&parts) == clean_bytes, "{case}");
            assert!(!directory.exists(), "{case}: the job folder, including a copied source, is removed");
            for part in &parts { std::fs::remove_file(&part.path).unwrap(); }
        }
    }

    #[test]
    fn too_little_free_space_pauses_before_a_new_part() {
        let _serial = JOB_TEST_LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let folder = tempfile::tempdir().unwrap();
        let destination = folder.path().join("out.pdf");
        let directory = folder.path().join("job");
        *crate::large_ocr_parts::FAKE_FREE_BYTES.lock().unwrap() = Some(2 * SPLIT_CAP - 1);
        let outcome = run_in_parts(&directory, &destination, false);
        *crate::large_ocr_parts::FAKE_FREE_BYTES.lock().unwrap() = None;
        let JobOutcome::Paused { reason: PauseReason::Disk(message), next_page, .. } = outcome else { panic!("{outcome:?}") };
        assert!(message.contains("free on the drive"), "{message}");
        assert!(next_page > 0 && !directory.join(crate::large_ocr_parts::PARTS_FILE).exists() && !directory.join("part-2").exists());
        assert!(published_parts(run_in_parts(&directory, &destination, true)).len() > 1);
    }

    #[test]
    fn a_part_folder_that_is_a_junction_is_never_followed() {
        let _serial = JOB_TEST_LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let folder = tempfile::tempdir().unwrap();
        let destination = folder.path().join("out.pdf");
        let directory = folder.path().join("job");
        let state = all_pages_unpublished(&directory, &destination);
        let elsewhere = folder.path().join("elsewhere");
        std::fs::create_dir(&elsewhere).unwrap();
        std::fs::write(elsewhere.join("keep.txt"), b"keep").unwrap();
        let open_part = directory.join(format!("part-{}", state.closed.len() + 1));
        std::fs::remove_dir_all(&open_part).unwrap();
        let made = crate::child_job::test_output(std::process::Command::new("cmd").args(["/C", "mklink", "/J"]).arg(&open_part).arg(&elsewhere)).unwrap();
        assert!(made.status.success(), "{}", String::from_utf8_lossy(&made.stderr));
        let outcome = run_in_parts(&directory, &destination, true);
        let JobOutcome::Paused { reason: PauseReason::Publish(message), .. } = outcome else { panic!("{outcome:?}") };
        assert!(message.contains("not a plain folder"), "{message}");
        assert_eq!(std::fs::read(elsewhere.join("keep.txt")).unwrap(), b"keep");
        assert_eq!(std::fs::read_dir(&elsewhere).unwrap().count(), 1, "nothing was written through the junction");
        std::fs::remove_dir(&open_part).unwrap();
    }

    #[test]
    fn automatic_worker_count_is_between_one_and_four() {
        let workers = automatic_workers();
        assert!((1..=MAX_JOB_WORKERS).contains(&workers));
    }

    #[repr(C)]
    struct ProcessMemoryCounters { cb: u32, page_faults: u32, peak_working_set: usize, working_set: usize, quota_peak_paged: usize, quota_paged: usize, quota_peak_non_paged: usize, quota_non_paged: usize, pagefile_usage: usize, peak_pagefile_usage: usize }

    #[link(name = "kernel32")]
    extern "system" {
        fn GetCurrentProcess() -> isize;
        fn K32GetProcessMemoryInfo(process: isize, counters: *mut ProcessMemoryCounters, size: u32) -> i32;
    }

    fn private_bytes() -> u64 {
        let mut counters = ProcessMemoryCounters { cb: std::mem::size_of::<ProcessMemoryCounters>() as u32, page_faults: 0, peak_working_set: 0, working_set: 0, quota_peak_paged: 0, quota_paged: 0, quota_peak_non_paged: 0, quota_non_paged: 0, pagefile_usage: 0, peak_pagefile_usage: 0 };
        assert!(unsafe { K32GetProcessMemoryInfo(GetCurrentProcess(), &mut counters, counters.cb) } != 0);
        counters.pagefile_usage as u64
    }

    /// Letter pages at 150 DPI (1275 x 1650 gray, about 6.3 MB of RGB each).
    struct LetterSource;
    impl PageSource for LetterSource {
        fn page_size(&self, _page: u32) -> Result<(f32, f32), PageError> { Ok((612.0, 792.0)) }
        fn render(&self, page: u32, _cancellation: &OcrCancellation) -> Result<RenderedPage, PageError> {
            let (width, height) = (1275_u32, 1650_u32);
            let mut p6 = format!("P6\n{width} {height}\n255\n").into_bytes();
            p6.reserve((width * height * 3) as usize);
            for y in 0..height {
                for x in 0..width {
                    let ink = (y / 24) % 2 == 0 && ((x * 7 + y * 3 + page * 31) % 23) < 9;
                    let value = if ink { 30 + ((x + page) % 40) as u8 } else { 250 };
                    p6.extend_from_slice(&[value; 3]);
                }
            }
            Ok(RenderedPage { width, height, page_width: 612.0, page_height: 792.0, p6, reduced_resolution: false })
        }
    }

    struct LetterEngine;
    impl PageRecognizer for LetterEngine {
        fn recognize_tsv(&self, _p6: &[u8], _cancellation: &OcrCancellation) -> Result<(String, Skew), PageError> {
            let mut rows = vec![
                "level\tpage_num\tblock_num\tpar_num\tline_num\tword_num\tleft\ttop\twidth\theight\tconf\ttext".to_string(),
                "1\t1\t0\t0\t0\t0\t0\t0\t1275\t1650\t-1\t".into(),
                "2\t1\t1\t0\t0\t0\t100\t100\t1000\t1400\t-1\t".into(),
                "3\t1\t1\t1\t0\t0\t100\t100\t1000\t1400\t-1\t".into(),
            ];
            for line in 0..40 {
                rows.push(format!("4\t1\t1\t1\t{}\t0\t100\t{}\t1000\t30\t-1\t", line + 1, 100 + line * 34));
                for word in 0..10 { rows.push(format!("5\t1\t1\t1\t{}\t{}\t{}\t{}\t90\t30\t90\tword{}", line + 1, word + 1, 100 + word * 100, 100 + line * 34, line * 10 + word)); }
            }
            Ok((rows.join("\n"), Skew::NONE))
        }
    }

    /// A measurement, not a gate in the normal suite. Run alone in release:
    /// `cargo test --release --bins private_bytes_stay_flat -- --ignored --nocapture`.
    #[test]
    #[ignore = "measurement: run alone in release"]
    fn private_bytes_stay_flat_on_a_large_job() {
        let _serial = JOB_TEST_LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let pages = std::env::var("SM4_LARGE_PAGES").ok().and_then(|value| value.parse().ok()).unwrap_or(1_000_u32);
        let folder = tempfile::tempdir().unwrap();
        let destination = folder.path().join("letter.pdf");
        let samples = Arc::new(Mutex::new(Vec::<(u32, u64)>::new()));
        let sink = samples.clone();
        let progress: ProgressSink = Arc::new(move |progress: &JobProgress| { sink.lock().unwrap().push((progress.pages_done, private_bytes())); });
        let started = Instant::now();
        let outcome = run_job(settings(&folder.path().join("job"), pages, &destination, 4, false), Arc::new(LetterSource), Arc::new(LetterEngine), JobControl::default(), progress);
        let elapsed = started.elapsed();
        let JobOutcome::Published { bytes, .. } = outcome else { panic!("{outcome:?}") };
        let samples = samples.lock().unwrap();
        let at_100 = samples.iter().find(|(done, _)| *done >= 100).unwrap().1;
        let peak = samples.iter().map(|(_, bytes)| *bytes).max().unwrap();
        let last = samples.iter().filter(|(done, _)| *done < pages).last().unwrap().1;
        println!("pages={pages} elapsed={:.1}s output={bytes} private_at_100={at_100} private_last={last} private_peak={peak} samples={}", elapsed.as_secs_f64(), samples.len());
        assert!(last.saturating_sub(at_100) <= 64 * 1024 * 1024, "private bytes grew by {}", last.saturating_sub(at_100));
    }
}
