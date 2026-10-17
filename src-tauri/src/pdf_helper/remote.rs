//! [`RemotePdfEngine`]: the [`PdfEngine`] implementation that forwards every call to the helper process.
//!
//! Each document keeps its source in an app-owned shared section (shared with the helper read-only) and
//! its password in the app, so it can be loaded again into a new helper after a restart. That reload is lazy (on the document's next call) and is skipped for a
//! *suspect* document: one whose request was in flight when a helper died. A suspect document answers
//! every call with an error until [`RemoteDocument::retry`]; a second death on the same document closes
//! its engine side for good (its edits live in the app and are unaffected).

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::ops::ControlFlow;

use super::protocol::*;
use super::section::{Section, View, FILE_MAP_READ, FILE_MAP_WRITE};
use super::supervisor::{CallError, HelperConfig, HelperFailure, Supervisor};
use crate::editor::{CropBox, PageSpec};
use crate::pdf_engine::{bgra_len, EngineBookmark, EngineDocument, LoadError, PageSize, PageTarget, PdfEngine, ProbeError, RenderError, RenderMode, RenderedPng, TextError};

pub(crate) const SUSPECT_MESSAGE: &str = "This file couldn't be displayed. Try again to reload it.";
pub(crate) const CLOSED_MESSAGE: &str = "This file couldn't be displayed: the PDF engine stopped on it twice.";
const RELOAD_CHANGED_MESSAGE: &str = "This file couldn't be displayed: it reloaded with a different page count.";
/// Helper deaths after which a document's engine side is closed for good.
const MAX_DEATHS: u8 = 2;
/// A render target section larger than this is released after use; smaller ones are kept for the next
/// render. 64 MiB covers the largest viewer render (3,000 x 5,000 px): recreating and faulting in a fresh
/// section per render cost 40-80 ms at 3,000 px in the T16 measurement.
const KEEP_RENDER_TARGET: usize = 64 * 1024 * 1024;

pub(crate) struct RemotePdfEngine {
    supervisor: RefCell<Supervisor>,
    /// Handles loaded in the running helper generation: a handle the helper gives out twice is refused.
    live: RefCell<(u64, HashSet<u32>)>,
    /// The app-owned section the helper renders bitmaps into, kept between renders up to `KEEP_RENDER_TARGET`.
    render_target: RefCell<Option<(Section, View)>>,
    /// Test only: kill the helper right after a document's next handle lookup, the window this guards.
    #[cfg(test)]
    kill_after_lookup: std::cell::Cell<bool>,
    /// App-private copy of the last viewer render, reused so the copy out of the shared section (SM7 L-a)
    /// lands in memory that is already faulted in. Released when it grows past `KEEP_RENDER_TARGET`.
    viewer_pixels: RefCell<Vec<u8>>,
}

impl RemotePdfEngine {
    pub(crate) fn start(config: HelperConfig) -> Result<Self, String> {
        Supervisor::start(config).map(|supervisor| Self { supervisor: RefCell::new(supervisor), live: RefCell::new((0, HashSet::new())), render_target: RefCell::new(None),
            #[cfg(test)]
            kill_after_lookup: std::cell::Cell::new(false), viewer_pixels: RefCell::new(Vec::new()) }).map_err(|failure| failure.message())
    }

    /// The app's helper (see [`HelperConfig::production`]).
    pub(crate) fn production() -> Result<Self, String> {
        Self::start(HelperConfig::production())
    }

    #[cfg(test)]
    pub(crate) fn stats(&self) -> super::supervisor::SupervisorStats {
        self.supervisor.borrow().stats()
    }

    /// Kills the helper right after a document's next handle lookup (test only).
    #[cfg(test)]
    pub(crate) fn kill_after_next_lookup(&self) {
        self.kill_after_lookup.set(true);
    }

    /// Ends the running helper as a crash would, while no request is in flight (test only).
    #[cfg(test)]
    pub(crate) fn kill_helper(&self) -> bool {
        self.supervisor.borrow_mut().kill_running_helper()
    }

    /// Makes the helper exit when its next request arrives, a crash while that request is in flight (test only).
    #[cfg(test)]
    pub(crate) fn crash_on_next_request(&self) -> bool {
        let deadline = self.supervisor.borrow().deadlines().read;
        self.fault(super::faults::Fault::ExitOnNextRequest, deadline) == Ok(1)
    }

    /// Shares `section` with `access` into the helper and asks it to probe the mapping (test only).
    #[cfg(test)]
    pub(crate) fn probe_section(&self, section: &Section, access: u32) -> Result<u8, CallError> {
        let len = section.len() as u64;
        let deadline = self.supervisor.borrow().deadlines().read;
        self.supervisor.borrow_mut().call_sharing(None, section.handle(), access, kind::FAULT, 32, |writer, shared| super::faults::Fault::ProbeSection(shared, len).write(writer), MAX_CONTROL_REPLY, deadline, class::ONLY_GENERAL, |reader| reader.u8())
    }

    /// Sends a test-only fault request (not tied to a document).
    #[cfg(test)]
    pub(crate) fn fault(&self, fault: super::faults::Fault, deadline: std::time::Duration) -> Result<u8, CallError> {
        self.supervisor.borrow_mut().call(None, kind::FAULT, 16, |writer| fault.write(writer), MAX_CONTROL_REPLY, deadline, class::ONLY_GENERAL, |reader| reader.u8())
    }

    /// Loads the document in `source` into the running helper (starting one if needed), sharing the section
    /// read-only; returns the helper generation too.
    fn load_handle(&self, source: &Section, password: Option<&str>) -> Result<(u64, Loaded), CallError> {
        let mut supervisor = self.supervisor.borrow_mut();
        let deadline = supervisor.deadlines().load;
        let capacity = password.map_or(0, str::len) + 32;
        let len = source.len();
        let loaded = supervisor.call_sharing(None, source.handle(), FILE_MAP_READ, kind::LOAD, capacity, |writer, shared| put_load(writer, password, shared, len), MAX_CONTROL_REPLY, deadline, class::LOAD, get_loaded)?;
        let generation = supervisor.running_generation().ok_or(CallError::Failure(HelperFailure::Stopped))?;
        let mut live = self.live.borrow_mut();
        if live.0 != generation {
            *live = (generation, HashSet::new());
        }
        if !live.1.insert(loaded.handle) {
            return Err(supervisor.fail_malformed("the helper gave two documents the same handle"));
        }
        Ok((generation, loaded))
    }

    /// Runs `render` with a target section of at least `length` bytes and the app's view of it.
    fn with_render_target<T>(&self, length: usize, render: impl FnOnce(&Section, &View) -> T) -> Result<T, String> {
        let mut slot = self.render_target.borrow_mut();
        if slot.as_ref().is_none_or(|(section, _)| section.len() < length) {
            *slot = None;
            let section = Section::create(length)?;
            let view = section.map(false)?;
            *slot = Some((section, view));
        }
        let (section, view) = slot.as_ref().expect("render target");
        let result = render(section, view);
        if length > KEEP_RENDER_TARGET {
            *slot = None;
        }
        Ok(result)
    }

    fn load<'a>(&'a self, bytes: &[u8], password: Option<&str>) -> Result<RemoteDocument<'a>, LoadError> {
        let source = Section::from_bytes(bytes).map_err(|message| LoadError::new(false, message))?;
        let (generation, loaded) = self.load_handle(&source, password).map_err(|error| match error {
            CallError::Engine(class::LOAD_PASSWORD, message) => LoadError::new(true, message),
            other => LoadError::new(false, other.message()),
        })?;
        Ok(RemoteDocument {
            engine: self,
            source,
            password: password.map(str::to_owned),
            page_count: loaded.page_count,
            unprotected: loaded.unprotected,
            state: RefCell::new(DocState { handle: Some((generation, loaded.handle)), deaths: 0, suspect: false }),
        })
    }
}

impl PdfEngine for RemotePdfEngine {
    type Document<'a> = RemoteDocument<'a>;
    fn load_from_vec(&self, bytes: Vec<u8>, password: Option<&str>) -> Result<RemoteDocument<'_>, LoadError> {
        self.load(&bytes, password)
    }
    fn load_from_slice<'a>(&'a self, bytes: &'a [u8], password: Option<&str>) -> Result<RemoteDocument<'a>, LoadError> {
        self.load(bytes, password)
    }
    /// Lets a crash-looped engine start helpers again (the engine-stopped state's "Try again").
    fn reset_after_stop(&self) {
        self.supervisor.borrow_mut().reset_after_stop();
    }
}

struct DocState {
    /// Helper generation and document handle while loaded in the running helper.
    handle: Option<(u64, u32)>,
    deaths: u8,
    suspect: bool,
}

/// A document loaded in the helper. Page count and protection are read once at load.
pub(crate) struct RemoteDocument<'a> {
    engine: &'a RemotePdfEngine,
    source: Section,
    password: Option<String>,
    page_count: usize,
    unprotected: bool,
    state: RefCell<DocState>,
}

#[derive(Clone, Copy)]
enum Deadline {
    Render,
    Read,
}

impl RemoteDocument<'_> {
    #[cfg(test)]
    pub(crate) fn is_suspect(&self) -> bool {
        self.state.borrow().suspect
    }

    /// Sends a test-only fault request as if it were this document's request.
    #[cfg(test)]
    pub(crate) fn inject(&self, fault: super::faults::Fault, deadline: std::time::Duration) -> Result<u8, CallError> {
        self.current_handle()?;
        let result = self.engine.fault(fault, deadline);
        if let Err(CallError::Failure(failure)) = &result {
            self.blame(failure);
        }
        result
    }

    fn blame(&self, failure: &HelperFailure) {
        if failure.ended_helper() {
            let mut state = self.state.borrow_mut();
            state.deaths = state.deaths.saturating_add(1);
            state.suspect = true;
            state.handle = None;
        }
    }

    /// This document's handle in a helper that is still the running one when the request is sent: if the
    /// helper was replaced after the lookup (it died in between), the document is loaded into the new
    /// helper and looked up again, once. The generation goes into the call, which refuses a mismatch.
    fn fresh_handle(&self) -> Result<(u64, u32), CallError> {
        for _ in 0..2 {
            let (generation, handle) = self.current_handle()?;
            #[cfg(test)]
            if self.engine.kill_after_lookup.replace(false) {
                self.engine.kill_helper();
            }
            if self.engine.supervisor.borrow_mut().ensure_running().map_err(CallError::Failure)? == generation {
                return Ok((generation, handle));
            }
        }
        Err(CallError::StaleGeneration)
    }

    /// This document's helper generation and handle, loading it again after a restart unless it is suspect.
    fn current_handle(&self) -> Result<(u64, u32), CallError> {
        self.engine.supervisor.borrow_mut().drop_if_exited();
        {
            let state = self.state.borrow();
            if state.deaths >= MAX_DEATHS {
                return Err(CallError::Unavailable(CLOSED_MESSAGE));
            }
            if let Some((generation, handle)) = state.handle {
                if self.engine.supervisor.borrow().running_generation() == Some(generation) {
                    return Ok((generation, handle));
                }
            }
            if state.suspect {
                return Err(CallError::Unavailable(SUSPECT_MESSAGE));
            }
        }
        match self.engine.load_handle(&self.source, self.password.as_deref()) {
            Ok((generation, loaded)) => {
                let mut state = self.state.borrow_mut();
                if loaded.page_count != self.page_count || loaded.unprotected != self.unprotected {
                    state.suspect = true;
                    state.deaths = MAX_DEATHS;
                    return Err(CallError::Unavailable(RELOAD_CHANGED_MESSAGE));
                }
                state.handle = Some((generation, loaded.handle));
                Ok((generation, loaded.handle))
            }
            Err(CallError::Failure(failure)) => {
                self.blame(&failure);
                Err(CallError::Failure(failure))
            }
            Err(other) => Err(other),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn request<T>(
        &self,
        request_kind: u16,
        capacity: usize,
        payload: impl FnOnce(&mut FrameWriter),
        reply_limit: usize,
        deadline: Deadline,
        allowed: &[u8],
        decode: impl FnOnce(&mut WireReader<'_>) -> Result<T, Malformed>,
    ) -> Result<T, CallError> {
        let (generation, handle) = self.fresh_handle()?;
        let result = {
            let mut supervisor = self.engine.supervisor.borrow_mut();
            let deadlines = supervisor.deadlines();
            let deadline = match deadline {
                Deadline::Render => deadlines.render,
                Deadline::Read => deadlines.read,
            };
            supervisor.call(
                Some(generation),
                request_kind,
                capacity.saturating_add(4),
                |writer| {
                    writer.u32(handle);
                    payload(writer);
                },
                reply_limit,
                deadline,
                allowed,
                decode,
            )
        };
        if let Err(CallError::Failure(failure)) = &result {
            self.blame(failure);
        }
        result
    }

    /// As `request`, sharing `handle` with `access` into the helper for this request.
    #[allow(clippy::too_many_arguments)]
    fn request_sharing<T>(
        &self,
        handle: windows::Win32::Foundation::HANDLE,
        access: u32,
        request_kind: u16,
        payload: impl FnOnce(&mut FrameWriter, u64),
        deadline: Deadline,
        allowed: &[u8],
        decode: impl FnOnce(&mut WireReader<'_>) -> Result<T, Malformed>,
    ) -> Result<T, CallError> {
        let (generation, document) = self.fresh_handle()?;
        let result = {
            let mut supervisor = self.engine.supervisor.borrow_mut();
            let deadlines = supervisor.deadlines();
            let deadline = match deadline {
                Deadline::Render => deadlines.render,
                Deadline::Read => deadlines.read,
            };
            supervisor.call_sharing(
                Some(generation),
                handle,
                access,
                request_kind,
                96,
                |writer, shared| {
                    writer.u32(document);
                    payload(writer, shared);
                },
                MAX_CONTROL_REPLY,
                deadline,
                allowed,
                decode,
            )
        };
        if let Err(CallError::Failure(failure)) = &result {
            self.blame(failure);
        }
        result
    }

    fn read<T>(&self, request_kind: u16, payload: impl FnOnce(&mut FrameWriter), decode: impl FnOnce(&mut WireReader<'_>) -> Result<T, Malformed>) -> Result<T, String> {
        self.request(request_kind, 64, payload, MAX_DTO_REPLY, Deadline::Read, class::ONLY_GENERAL, decode).map_err(CallError::message)
    }
}

fn text_error(error: CallError) -> TextError {
    match error {
        CallError::Engine(class::TEXT_TEXT, message) => TextError::Text(message),
        CallError::Engine(class::TEXT_BOUNDS, message) => TextError::Bounds(message),
        other => TextError::Page(other.message()),
    }
}

fn render_error(error: CallError) -> RenderError {
    match error {
        CallError::Engine(class::RENDER_PAGE, message) => RenderError::Page(message),
        CallError::Engine(class::RENDER_FORMAT, message) => RenderError::Format(message),
        CallError::Engine(class::RENDER_LAYOUT, _) => RenderError::Layout,
        other => RenderError::Render(other.message()),
    }
}

fn probe_error(error: CallError) -> ProbeError {
    match error {
        CallError::Engine(class::PROBE_RENDER, message) => ProbeError::Render(message),
        other => ProbeError::Read(other.message()),
    }
}

impl EngineDocument for RemoteDocument<'_> {
    fn page_count(&self) -> usize {
        self.page_count
    }

    /// Allows one more reload after a helper died on this document (the page's "Try again"). No effect
    /// once the document has been closed for good.
    fn retry(&self) {
        let mut state = self.state.borrow_mut();
        if state.deaths < MAX_DEATHS {
            state.suspect = false;
        }
    }

    fn page_sizes(&self) -> Result<Vec<PageSize>, String> {
        let limit = HEADER_LEN + 4 + self.page_count.min(MAX_PAGES + 1) * 8;
        let page_count = self.page_count;
        self.request(kind::PAGE_SIZES, 0, |_| {}, limit, Deadline::Read, class::ONLY_GENERAL, |reader| get_page_sizes(reader, page_count)).map_err(CallError::message)
    }

    fn is_unprotected(&self) -> bool {
        self.unprotected
    }

    fn planned_page_size(&self, spec: &PageSpec) -> Result<(f32, f32), String> {
        self.read(kind::PLANNED_PAGE_SIZE, |writer| put_spec(writer, spec), get_page_size)
    }

    /// The app sizes the bitmap from the planned page size (as pdfium-render would), the helper renders it
    /// with the viewer's flags into an app-owned section, and the app encodes the PNG: no helper-made image
    /// reaches the webview.
    fn render_viewer_png(&self, spec: &PageSpec, width: i32) -> Result<RenderedPng, String> {
        if !(1..=MAX_VIEWER_WIDTH).contains(&width) {
            return Err(format!("The viewer width {width} is outside the supported range."));
        }
        let (page_width, page_height) = self.planned_page_size(spec)?;
        let (pixels_wide, pixels_high) = viewer_size(page_width, page_height, width).ok_or("The page has no area to render.")?;
        // Copy out of the shared section before encoding (SM7 L-a): the helper keeps write access to the
        // section, so the encoder must read pixels it cannot change underneath it.
        let mut rgba = self.engine.viewer_pixels.borrow_mut();
        rgba.clear();
        let copy_out = |pixels: &[u8]| rgba.extend_from_slice(pixels);
        let request = |writer: &mut FrameWriter, shared: u64| {
            put_spec(writer, spec);
            writer.u32(pixels_wide);
            writer.u32(pixels_high);
            writer.u64(shared);
        };
        self.render_into_target(kind::RENDER_VIEWER_RGBA, request, pixels_wide, pixels_high, copy_out).map_err(|error| match error {
            RenderError::Page(message) | RenderError::Render(message) | RenderError::Format(message) => message,
            RenderError::Layout => "Unexpected viewer bitmap layout.".to_owned(),
        })?;
        use image::ImageEncoder;
        let mut png = Vec::new();
        let encoded = image::codecs::png::PngEncoder::new(&mut png).write_image(&rgba, pixels_wide, pixels_high, image::ExtendedColorType::Rgba8).map_err(|error| error.to_string());
        if rgba.capacity() > KEEP_RENDER_TARGET {
            *rgba = Vec::new();
        }
        encoded?;
        Ok(RenderedPng { png, width: pixels_wide, height: pixels_high })
    }

    fn visible_text(&self, target: PageTarget<'_>) -> Result<String, TextError> {
        self.request(kind::VISIBLE_TEXT, 64, |writer| put_target(writer, target), MAX_DTO_REPLY, Deadline::Read, class::TEXT, get_text).map_err(text_error)
    }

    fn text_geometry(&self, target: PageTarget<'_>, id: u64, index: u16, revision: u64) -> Result<crate::text_geometry::PageTextGeometry, String> {
        let expected = GeometryRequest { id, index, revision };
        self.read(
            kind::TEXT_GEOMETRY,
            |writer| {
                put_target(writer, target);
                writer.u64(id);
                writer.u16(index);
                writer.u64(revision);
            },
            |reader| get_geometry(reader, expected),
        )
    }

    fn bookmarks(&self, limit: usize, visit: &mut dyn FnMut(EngineBookmark) -> Result<ControlFlow<()>, String>) -> Result<(), String> {
        let limit = u32::try_from(limit).unwrap_or(u32::MAX) as usize;
        let entries = self.read(kind::BOOKMARKS, |writer| writer.count(limit), |reader| get_bookmarks(reader, limit))?;
        for entry in entries {
            if visit(entry)?.is_break() {
                break;
            }
        }
        Ok(())
    }

    fn bookmark_page_view(&self, spec: &PageSpec) -> Result<Option<crate::bookmark_destinations::DestinationView>, String> {
        self.read(kind::BOOKMARK_PAGE_VIEW, |writer| put_spec(writer, spec), |reader| reader.opt(get_view))
    }

    fn properties(&self, source_size_bytes: usize, current_page_dimensions: &[(f32, f32)]) -> Result<crate::document_properties::DocumentProperties, String> {
        let pages = current_page_dimensions.len();
        self.read(
            kind::PROPERTIES,
            |writer| put_properties_request(writer, source_size_bytes, current_page_dimensions),
            |reader| get_properties(reader, source_size_bytes, pages),
        )
    }

    #[cfg(test)]
    fn engine_page_labels(&self) -> Vec<Option<String>> {
        let page_count = self.page_count;
        self.read(kind::PAGE_LABELS, |_| {}, |reader| get_labels(reader, page_count)).unwrap_or_default()
    }

    fn text_geometry_with_source(&self, target: PageTarget<'_>, id: u64, index: u16, revision: u64) -> Result<crate::text_geometry::InspectedGeometry, String> {
        let expected = GeometryRequest { id, index, revision };
        self.read(
            kind::TEXT_GEOMETRY_WITH_SOURCE,
            |writer| {
                put_target(writer, target);
                writer.u64(id);
                writer.u16(index);
                writer.u64(revision);
            },
            |reader| get_inspected(reader, expected),
        )
    }

    fn displayed_crop(&self, spec: &PageSpec, rect: crate::service::CropRect) -> Result<CropBox, String> {
        self.read(
            kind::DISPLAYED_CROP,
            |writer| {
                put_spec(writer, spec);
                put_crop_rect(writer, rect);
            },
            get_finite_crop,
        )
    }

    fn displayed_note_rects(&self, spec: &PageSpec, rects: &[CropBox]) -> Result<Vec<Option<crate::comments::DisplayRect>>, String> {
        if rects.len() > crate::comments::MAX_GEOMETRY_SLOTS {
            return Err("Too many note rectangles for one page.".into());
        }
        let expected = rects.len();
        self.read(
            kind::DISPLAYED_NOTE_RECTS,
            |writer| {
                put_spec(writer, spec);
                put_crops(writer, rects);
            },
            |reader| get_display_rects(reader, expected),
        )
    }

    fn form_values(&self, fields: &[crate::forms::FormField]) -> Option<HashMap<String, Option<String>>> {
        self.read(kind::FORM_VALUES, |writer| put_form_fields(writer, fields), |reader| get_form_values(reader, fields)).ok().flatten()
    }

    fn source_page_size(&self, index: usize) -> Result<(f32, f32), String> {
        self.read(kind::SOURCE_PAGE_SIZE, |writer| writer.u64(index as u64), get_page_size)
    }

    fn thumbnail_check(&self, index: usize) -> Result<bool, ProbeError> {
        self.request(kind::THUMBNAIL_CHECK, 16, |writer| writer.u64(index as u64), MAX_CONTROL_REPLY, Deadline::Render, class::PROBE, |reader| reader.bool()).map_err(probe_error)
    }

    fn render_bgra_into(&self, target: PageTarget<'_>, mode: RenderMode, width: u32, height: u32, out: &mut [u8]) -> Result<(), RenderError> {
        if bgra_len(width, height) != Some(out.len()) {
            return Err(RenderError::Layout);
        }
        let request = |writer: &mut FrameWriter, shared: u64| {
            put_target(writer, target);
            put_mode(writer, mode);
            writer.u32(width);
            writer.u32(height);
            writer.u64(shared);
        };
        self.render_into_target(kind::RENDER_BGRA, request, width, height, |pixels| out.copy_from_slice(pixels))
    }
}

impl RemoteDocument<'_> {
    /// Has the helper render `width` x `height` BGRA into the engine's app-owned target section, then hands
    /// the pixels (only after a successful reply) to `consume`.
    fn render_into_target(&self, request_kind: u16, request: impl FnOnce(&mut FrameWriter, u64), width: u32, height: u32, consume: impl FnOnce(&[u8])) -> Result<(), RenderError> {
        let Some(length) = bgra_len(width, height).filter(|length| *length > 0) else { return Err(RenderError::Layout) };
        if length > MAX_BGRA_BYTES {
            return Err(RenderError::Render("The bitmap is larger than the PDF engine renders.".into()));
        }
        let rendered = self.engine.with_render_target(length, |section, view| {
            let result = self.request_sharing(section.handle(), FILE_MAP_READ | FILE_MAP_WRITE, request_kind, request, Deadline::Render, class::RENDER, |_| Ok(()));
            if result.is_ok() {
                consume(&view.as_slice()[..length]);
            }
            result
        });
        rendered.map_err(RenderError::Render)?.map_err(render_error)
    }
}

impl Drop for RemoteDocument<'_> {
    /// Closes the helper's copy if it is still loaded in the running helper; never starts a helper.
    fn drop(&mut self) {
        let Some((generation, handle)) = self.state.get_mut().handle else { return };
        let Ok(mut supervisor) = self.engine.supervisor.try_borrow_mut() else { return };
        if supervisor.running_generation() != Some(generation) {
            return;
        }
        if let Ok(mut live) = self.engine.live.try_borrow_mut() {
            live.1.remove(&handle);
        }
        let deadline = supervisor.deadlines().close;
        let _ = supervisor.call_running(kind::CLOSE, 4, |writer| writer.u32(handle), MAX_CONTROL_REPLY, deadline, class::ONLY_GENERAL, |_| Ok(()));
    }
}
