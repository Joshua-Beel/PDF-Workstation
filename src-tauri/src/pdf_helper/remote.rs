//! [`RemotePdfEngine`]: the [`PdfEngine`] implementation that forwards every call to the helper process.
//!
//! Each document keeps its source bytes and password in the app, so it can be loaded again into a new
//! helper after a restart. That reload is lazy (on the document's next call) and is skipped for a
//! *suspect* document: one whose request was in flight when a helper died. A suspect document answers
//! every call with an error until [`RemoteDocument::retry`]; a second death on the same document closes
//! its engine side for good (its edits live in the app and are unaffected).

use std::borrow::Cow;
use std::cell::RefCell;
use std::collections::HashMap;
use std::ops::ControlFlow;
use std::path::PathBuf;

use super::protocol::*;
use super::supervisor::{CallError, HelperConfig, HelperFailure, Supervisor};
use crate::editor::{CropBox, PageSpec};
use crate::pdf_engine::{bgra_len, EngineBookmark, EngineDocument, LoadError, PageSize, PageTarget, PdfEngine, ProbeError, RenderError, RenderMode, RenderedPng, TextError};

pub(crate) const SUSPECT_MESSAGE: &str = "This file couldn't be displayed. Try again to reload it.";
pub(crate) const CLOSED_MESSAGE: &str = "This file couldn't be displayed: the PDF engine stopped on it twice.";
const RELOAD_CHANGED_MESSAGE: &str = "This file couldn't be displayed: it reloaded with a different page count.";
/// Helper deaths after which a document's engine side is closed for good.
const MAX_DEATHS: u8 = 2;

pub(crate) struct RemotePdfEngine {
    supervisor: RefCell<Supervisor>,
}

impl RemotePdfEngine {
    pub(crate) fn start(config: HelperConfig) -> Result<Self, String> {
        Supervisor::start(config).map(|supervisor| Self { supervisor: RefCell::new(supervisor) }).map_err(|failure| failure.message())
    }

    /// The app's helper (see [`HelperConfig::production`]).
    pub(crate) fn production(library: PathBuf) -> Result<Self, String> {
        Self::start(HelperConfig::production(library))
    }

    /// Lets a crash-looped engine start helpers again (the engine-stopped state's Retry).
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn reset_after_stop(&self) {
        self.supervisor.borrow_mut().reset_after_stop();
    }

    #[cfg(test)]
    pub(crate) fn stats(&self) -> super::supervisor::SupervisorStats {
        self.supervisor.borrow().stats()
    }

    /// Sends a test-only fault request (not tied to a document).
    #[cfg(test)]
    pub(crate) fn fault(&self, fault: super::faults::Fault, deadline: std::time::Duration) -> Result<u8, CallError> {
        self.supervisor.borrow_mut().call(kind::FAULT, 16, |writer| fault.write(writer), MAX_CONTROL_REPLY, deadline, class::ONLY_GENERAL, |reader| reader.u8())
    }

    /// Loads `source` into the running helper (starting one if needed); returns the helper generation too.
    fn load_handle(&self, source: &[u8], password: Option<&str>) -> Result<(u64, Loaded), CallError> {
        let mut supervisor = self.supervisor.borrow_mut();
        let deadline = supervisor.deadlines().load;
        let capacity = source.len().saturating_add(password.map_or(0, str::len) + 16);
        let loaded = supervisor.call(kind::LOAD, capacity, |writer| put_load(writer, password, source), MAX_CONTROL_REPLY, deadline, class::LOAD, get_loaded)?;
        let generation = supervisor.running_generation().ok_or(CallError::Failure(HelperFailure::Stopped))?;
        Ok((generation, loaded))
    }

    fn load<'a>(&'a self, source: Cow<'a, [u8]>, password: Option<&str>) -> Result<RemoteDocument<'a>, LoadError> {
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
        self.load(Cow::Owned(bytes), password)
    }
    fn load_from_slice<'a>(&'a self, bytes: &'a [u8], password: Option<&str>) -> Result<RemoteDocument<'a>, LoadError> {
        self.load(Cow::Borrowed(bytes), password)
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
    source: Cow<'a, [u8]>,
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
    /// Allows one more reload after a helper died on this document (the suspect tab's Retry). No effect
    /// once the document has been closed for good.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn retry(&self) {
        let mut state = self.state.borrow_mut();
        if state.deaths < MAX_DEATHS {
            state.suspect = false;
        }
    }

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

    /// This document's handle in the running helper, loading it again after a restart unless it is suspect.
    fn current_handle(&self) -> Result<u32, CallError> {
        {
            let state = self.state.borrow();
            if state.deaths >= MAX_DEATHS {
                return Err(CallError::Unavailable(CLOSED_MESSAGE));
            }
            if let Some((generation, handle)) = state.handle {
                if self.engine.supervisor.borrow().running_generation() == Some(generation) {
                    return Ok(handle);
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
                Ok(loaded.handle)
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
        let handle = self.current_handle()?;
        let result = {
            let mut supervisor = self.engine.supervisor.borrow_mut();
            let deadlines = supervisor.deadlines();
            let deadline = match deadline {
                Deadline::Render => deadlines.render,
                Deadline::Read => deadlines.read,
            };
            supervisor.call(
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

    fn page_sizes(&self) -> Result<Vec<PageSize>, String> {
        let limit = HEADER_LEN + 4 + self.page_count.min(MAX_PAGES + 1) * 8;
        let page_count = self.page_count;
        self.request(kind::PAGE_SIZES, 0, |_| {}, limit, Deadline::Read, class::ONLY_GENERAL, |reader| get_page_sizes(reader, page_count)).map_err(CallError::message)
    }

    fn is_unprotected(&self) -> bool {
        self.unprotected
    }

    fn planned_page_size(&self, spec: &PageSpec) -> Result<(f32, f32), String> {
        self.read(kind::PLANNED_PAGE_SIZE, |writer| put_spec(writer, spec), get_pair)
    }

    fn render_viewer_png(&self, spec: &PageSpec, width: i32) -> Result<RenderedPng, String> {
        if !(1..=MAX_VIEWER_WIDTH).contains(&width) {
            return Err(format!("The viewer width {width} is outside the supported range."));
        }
        let payload = |writer: &mut FrameWriter| {
            put_spec(writer, spec);
            writer.i32(width);
        };
        let (width, height, png) = self
            .request(kind::VIEWER_PNG, 64, payload, viewer_reply_limit(width), Deadline::Render, class::ONLY_GENERAL, |reader| get_png(reader, width))
            .map_err(CallError::message)?;
        Ok(RenderedPng { png, width, height })
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
            get_crop,
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
        self.read(kind::SOURCE_PAGE_SIZE, |writer| writer.u64(index as u64), get_pair)
    }

    fn thumbnail_check(&self, index: usize) -> Result<bool, ProbeError> {
        self.request(kind::THUMBNAIL_CHECK, 16, |writer| writer.u64(index as u64), MAX_CONTROL_REPLY, Deadline::Render, class::PROBE, |reader| reader.bool()).map_err(probe_error)
    }

    fn render_bgra_into(&self, target: PageTarget<'_>, mode: RenderMode, width: u32, height: u32, out: &mut [u8]) -> Result<(), RenderError> {
        let Some(length) = bgra_len(width, height).filter(|length| *length == out.len()) else { return Err(RenderError::Layout) };
        if length > MAX_BGRA_BYTES {
            return Err(RenderError::Render("The bitmap is larger than the PDF engine renders.".into()));
        }
        let payload = |writer: &mut FrameWriter| {
            put_target(writer, target);
            put_mode(writer, mode);
            writer.u32(width);
            writer.u32(height);
        };
        let decode = |reader: &mut WireReader<'_>| {
            let pixels = reader.rest();
            if pixels.len() != length {
                return Err(Malformed::new(format!("a {width}x{height} bitmap of {} bytes", pixels.len())));
            }
            out.copy_from_slice(pixels);
            Ok(())
        };
        self.request(kind::RENDER_BGRA, 64, payload, HEADER_LEN + length, Deadline::Render, class::RENDER, decode).map_err(render_error)
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
        let deadline = supervisor.deadlines().close;
        let _ = supervisor.call_running(kind::CLOSE, 4, |writer| writer.u32(handle), MAX_CONTROL_REPLY, deadline, class::ONLY_GENERAL, |_| Ok(()));
    }
}
