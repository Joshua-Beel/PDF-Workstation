//! The isolated PDF-engine helper (SM5 helper step 3): helper mode, protocol and supervision.
//!
//! The same exe started as `pdf-workstation.exe --pdf-engine-helper` runs [`run_helper_mode`] before Tauri
//! starts: no window, PDFium only, requests on stdin and replies on stdout. The app starts it through
//! `child_job` in a hardened Job Object capped at `HELPER_MEMORY_LIMIT` and talks to it through
//! [`RemotePdfEngine`], which implements the same `PdfEngine` boundary as `InProcessEngine`.
//!
//! The helper is off by default. [`SelectedEngine`] chooses it only when [`helper_enabled`] is true: in a
//! debug build with the environment variable [`HELPER_SETTING_VARIABLE`] set to `1`. Release builds compile
//! the check to `false`, and no command or webview request can change it.
//!
//! The helper is a crash and memory boundary, not a privilege sandbox: it runs with the user's full token,
//! at the same integrity level, with file and network access. Code running in it can do what the user can.

// Release builds have no helper mode (SM7 M1): its code stays compiled but is unreachable there.
#![cfg_attr(not(debug_assertions), allow(dead_code))]

#[cfg(windows)]
mod protocol;
#[cfg(windows)]
mod remote;
#[cfg(windows)]
mod section;
#[cfg(windows)]
mod serve;
#[cfg(windows)]
mod supervisor;
#[cfg(all(test, windows))]
mod faults;
#[cfg(all(test, windows))]
mod tests;

use std::path::PathBuf;

use crate::pdf_engine::{EngineBookmark, EngineDocument, InProcessDocument, InProcessEngine, LoadError, PageSize, PageTarget, PdfEngine, ProbeError, RenderError, RenderMode, RenderedPng, TextError};

#[cfg(windows)]
pub(crate) use remote::{RemoteDocument, RemotePdfEngine};
#[cfg(windows)]
pub(crate) use supervisor::HELPER_MODE_FLAG;

/// Debug builds only: set to `1` to run PDFium in the helper process instead of in the app.
pub(crate) const HELPER_SETTING_VARIABLE: &str = "SMACROBAT_PDF_ENGINE_HELPER";

/// Whether the app uses the helper. Always `false` in release builds.
pub(crate) fn helper_enabled() -> bool {
    #[cfg(all(debug_assertions, windows))]
    {
        std::env::var_os(HELPER_SETTING_VARIABLE).is_some_and(|value| value == "1")
    }
    #[cfg(not(all(debug_assertions, windows)))]
    {
        false
    }
}

/// Whether this build has a helper mode at all: debug builds only, until the helper is enabled for release
/// (SM7 M1). `main` gates its check on the same configuration, so a release build ignores
/// `--pdf-engine-helper` and starts the app normally.
pub(crate) const HELPER_MODE_AVAILABLE: bool = cfg!(all(debug_assertions, windows));

/// Whether `main` must run the helper instead of the app: helper mode exists in this build and the mode
/// flag is the one and only argument.
#[cfg(windows)]
#[cfg_attr(not(any(test, debug_assertions)), allow(dead_code))]
pub(crate) fn helper_mode_requested(arguments: impl IntoIterator<Item = std::ffi::OsString>) -> bool {
    HELPER_MODE_AVAILABLE && is_helper_invocation(arguments)
}

/// Where the bundled PDFium sits below the resource folder, as `main` and `tauri.conf.json` have it.
#[cfg(windows)]
pub(crate) const PDFIUM_RELATIVE_PATH: &str = "resources/pdfium/bin/pdfium.dll";

/// The PDFium library the helper loads. The helper works this out itself and never takes a path from a
/// request: the bundled DLL below this exe's folder, which is the app's resource folder on Windows, so it
/// is the same file the app would load. Test builds use the source tree's copy.
#[cfg(windows)]
pub(crate) fn helper_library() -> Result<PathBuf, String> {
    #[cfg(test)]
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    #[cfg(not(test))]
    let root = std::env::current_exe()
        .map_err(|error| format!("The PDF engine helper could not locate itself: {error}"))?
        .parent()
        .ok_or("The PDF engine helper could not locate its folder.")?
        .to_path_buf();
    Ok(root.join(PDFIUM_RELATIVE_PATH))
}

/// Whether this process was started as the helper: the mode flag is the one and only argument.
#[cfg(windows)]
pub(crate) fn is_helper_invocation(arguments: impl IntoIterator<Item = std::ffi::OsString>) -> bool {
    let mut arguments = arguments.into_iter().skip(1);
    arguments.next().is_some_and(|flag| flag == HELPER_MODE_FLAG) && arguments.next().is_none()
}

/// Exit code when the helper's stdin or stdout is not a pipe (someone ran the exe with the flag by hand).
#[cfg(windows)]
pub(crate) const EXIT_NOT_A_PIPE: u32 = 2;

/// The helper process: hardened, terminating allocator armed, then the request loop. Never returns.
#[cfg(windows)]
pub(crate) fn run_helper_mode() -> ! {
    crate::child_job::harden_job_child_process();
    crate::child_job::arm_terminating_allocator();
    let code = serve_standard_streams();
    std::process::exit(code as i32)
}

/// Serves on the inherited stdin/stdout, which must be pipes.
#[cfg(windows)]
pub(crate) fn serve_standard_streams() -> u32 {
    use std::mem::ManuallyDrop;
    use std::os::windows::io::{AsRawHandle, FromRawHandle};
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::Storage::FileSystem::{GetFileType, FILE_TYPE_PIPE};
    let input = std::io::stdin().as_raw_handle();
    let output = std::io::stdout().as_raw_handle();
    let is_pipe = |handle: std::os::windows::io::RawHandle| !handle.is_null() && unsafe { GetFileType(HANDLE(handle)) } == FILE_TYPE_PIPE;
    if !is_pipe(input) || !is_pipe(output) {
        return EXIT_NOT_A_PIPE;
    }
    let mut input = ManuallyDrop::new(unsafe { std::fs::File::from_raw_handle(input) });
    let mut output = ManuallyDrop::new(unsafe { std::fs::File::from_raw_handle(output) });
    serve::serve(&mut *input, &mut *output)
}

/// The engine the pdf-worker uses: in process (the default), or the helper when [`helper_enabled`].
pub(crate) enum SelectedEngine {
    InProcess(InProcessEngine),
    #[cfg(windows)]
    Remote(RemotePdfEngine),
}

impl SelectedEngine {
    pub(crate) fn bind(library: PathBuf) -> Result<Self, String> {
        #[cfg(windows)]
        if helper_enabled() {
            let same = |path: &std::path::Path| std::fs::canonicalize(path).ok();
            if same(&helper_library()?).is_none() || same(&helper_library()?) != same(&library) {
                return Err("PDF engine could not start: the helper would load a different PDFium library than the app.".into());
            }
            return RemotePdfEngine::production().map(Self::Remote);
        }
        InProcessEngine::bind(library).map(Self::InProcess)
    }
}

#[cfg(test)]
impl SelectedEngine {
    /// Which engine the pdf-worker is using (test only).
    pub(crate) fn test_kind(&self) -> &'static str {
        match self {
            Self::InProcess(_) => "in-process",
            #[cfg(windows)]
            Self::Remote(_) => "helper",
        }
    }

    /// Ends the running helper as a crash would; `false` in process (test only).
    pub(crate) fn test_kill_helper(&self) -> bool {
        match self {
            Self::InProcess(_) => false,
            #[cfg(windows)]
            Self::Remote(engine) => engine.kill_helper(),
        }
    }
}

pub(crate) enum SelectedDocument<'a> {
    InProcess(InProcessDocument<'a>),
    #[cfg(windows)]
    Remote(RemoteDocument<'a>),
}

impl PdfEngine for SelectedEngine {
    type Document<'a> = SelectedDocument<'a>;
    fn load_from_vec(&self, bytes: Vec<u8>, password: Option<&str>) -> Result<SelectedDocument<'_>, LoadError> {
        match self {
            Self::InProcess(engine) => engine.load_from_vec(bytes, password).map(SelectedDocument::InProcess),
            #[cfg(windows)]
            Self::Remote(engine) => engine.load_from_vec(bytes, password).map(SelectedDocument::Remote),
        }
    }
    fn load_from_slice<'a>(&'a self, bytes: &'a [u8], password: Option<&str>) -> Result<SelectedDocument<'a>, LoadError> {
        match self {
            Self::InProcess(engine) => engine.load_from_slice(bytes, password).map(SelectedDocument::InProcess),
            #[cfg(windows)]
            Self::Remote(engine) => engine.load_from_slice(bytes, password).map(SelectedDocument::Remote),
        }
    }
    fn reset_after_stop(&self) {
        match self {
            Self::InProcess(engine) => engine.reset_after_stop(),
            #[cfg(windows)]
            Self::Remote(engine) => engine.reset_after_stop(),
        }
    }
}

macro_rules! dispatch {
    ($self:ident, $document:ident => $call:expr) => {
        match $self {
            SelectedDocument::InProcess($document) => $call,
            #[cfg(windows)]
            SelectedDocument::Remote($document) => $call,
        }
    };
}

impl EngineDocument for SelectedDocument<'_> {
    fn page_count(&self) -> usize {
        dispatch!(self, document => document.page_count())
    }
    fn page_sizes(&self) -> Result<Vec<PageSize>, String> {
        dispatch!(self, document => document.page_sizes())
    }
    fn is_unprotected(&self) -> bool {
        dispatch!(self, document => document.is_unprotected())
    }
    fn planned_page_size(&self, spec: &crate::editor::PageSpec) -> Result<(f32, f32), String> {
        dispatch!(self, document => document.planned_page_size(spec))
    }
    fn render_viewer_png(&self, spec: &crate::editor::PageSpec, width: i32) -> Result<RenderedPng, String> {
        dispatch!(self, document => document.render_viewer_png(spec, width))
    }
    fn visible_text(&self, target: PageTarget<'_>) -> Result<String, TextError> {
        dispatch!(self, document => document.visible_text(target))
    }
    fn text_geometry(&self, target: PageTarget<'_>, id: u64, index: u16, revision: u64) -> Result<crate::text_geometry::PageTextGeometry, String> {
        dispatch!(self, document => document.text_geometry(target, id, index, revision))
    }
    fn bookmarks(&self, limit: usize, visit: &mut dyn FnMut(EngineBookmark) -> Result<std::ops::ControlFlow<()>, String>) -> Result<(), String> {
        dispatch!(self, document => document.bookmarks(limit, visit))
    }
    fn bookmark_page_view(&self, spec: &crate::editor::PageSpec) -> Result<Option<crate::bookmark_destinations::DestinationView>, String> {
        dispatch!(self, document => document.bookmark_page_view(spec))
    }
    fn properties(&self, source_size_bytes: usize, current_page_dimensions: &[(f32, f32)]) -> Result<crate::document_properties::DocumentProperties, String> {
        dispatch!(self, document => document.properties(source_size_bytes, current_page_dimensions))
    }
    #[cfg(test)]
    fn engine_page_labels(&self) -> Vec<Option<String>> {
        dispatch!(self, document => document.engine_page_labels())
    }
    fn text_geometry_with_source(&self, target: PageTarget<'_>, id: u64, index: u16, revision: u64) -> Result<crate::text_geometry::InspectedGeometry, String> {
        dispatch!(self, document => document.text_geometry_with_source(target, id, index, revision))
    }
    fn displayed_crop(&self, spec: &crate::editor::PageSpec, rect: crate::service::CropRect) -> Result<crate::editor::CropBox, String> {
        dispatch!(self, document => document.displayed_crop(spec, rect))
    }
    fn displayed_note_rects(&self, spec: &crate::editor::PageSpec, rects: &[crate::editor::CropBox]) -> Result<Vec<Option<crate::comments::DisplayRect>>, String> {
        dispatch!(self, document => document.displayed_note_rects(spec, rects))
    }
    fn form_values(&self, fields: &[crate::forms::FormField]) -> Option<std::collections::HashMap<String, Option<String>>> {
        dispatch!(self, document => document.form_values(fields))
    }
    fn source_page_size(&self, index: usize) -> Result<(f32, f32), String> {
        dispatch!(self, document => document.source_page_size(index))
    }
    fn thumbnail_check(&self, index: usize) -> Result<bool, ProbeError> {
        dispatch!(self, document => document.thumbnail_check(index))
    }
    fn render_bgra_into(&self, target: PageTarget<'_>, mode: RenderMode, width: u32, height: u32, out: &mut [u8]) -> Result<(), RenderError> {
        dispatch!(self, document => document.render_bgra_into(target, mode, width, height, out))
    }
    fn retry(&self) {
        dispatch!(self, document => document.retry())
    }
}
