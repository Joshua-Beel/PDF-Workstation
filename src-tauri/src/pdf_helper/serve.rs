//! The helper's request loop: PDFium in this process, driven over stdin/stdout by the app.
//!
//! Runs only in the helper process, after `harden_job_child_process` and `arm_terminating_allocator`. It
//! writes [`SYNC`] and a hello frame, binds the PDFium library it finds itself (`helper_library`) on the
//! first (init) request, which carries no payload, then answers one request at a time until stdin closes. A
//! request that does not follow the protocol ends the helper.

use std::collections::HashMap;
use std::io::{Read, Write};

use super::protocol::*;
use super::section::View;
use crate::pdf_engine::{bgra_len, EngineDocument, InProcessDocument, InProcessEngine, LoadError, PdfEngine, ProbeError, RenderError, TextError};

/// Exit codes of the helper loop. The app treats any exit as a failure of the request in flight.
pub(crate) const EXIT_CLOSED: u32 = 0;
pub(crate) const EXIT_PROTOCOL: u32 = 3;
pub(crate) const EXIT_INIT: u32 = 4;
pub(crate) const EXIT_OUTPUT: u32 = 5;

/// A reply payload, or an engine error (class and message).
pub(crate) type Reply = Result<FrameWriter, (u8, String)>;

/// Same text as the app's own refusal of an oversized page text, for pages whose text would not fit a reply.
const TOO_MUCH_TEXT: &str = "This page has too much embedded text to read or search.";

fn floor_char_boundary(text: &str, index: usize) -> usize {
    let mut index = index.min(text.len());
    while !text.is_char_boundary(index) {
        index -= 1;
    }
    index
}

const DOCUMENT_KINDS: &[u16] = &[
    kind::CLOSE,
    kind::PAGE_SIZES,
    kind::PLANNED_PAGE_SIZE,
    kind::VISIBLE_TEXT,
    kind::TEXT_GEOMETRY,
    kind::TEXT_GEOMETRY_WITH_SOURCE,
    kind::BOOKMARKS,
    kind::BOOKMARK_PAGE_VIEW,
    kind::PROPERTIES,
    kind::DISPLAYED_CROP,
    kind::DISPLAYED_NOTE_RECTS,
    kind::FORM_VALUES,
    kind::SOURCE_PAGE_SIZE,
    kind::THUMBNAIL_CHECK,
    kind::RENDER_BGRA,
    kind::RENDER_VIEWER_RGBA,
    #[cfg(test)]
    kind::PAGE_LABELS,
];

/// Serves requests until the input closes or a request is malformed. Returns the exit code.
pub(crate) fn serve(input: &mut impl Read, output: &mut impl Write) -> u32 {
    let mut hello = Vec::from(SYNC);
    let mut frame = FrameWriter::new(kind::HELLO, 0);
    frame.u32(std::process::id());
    hello.extend_from_slice(&frame.finish());
    if output.write_all(&hello).and_then(|()| output.flush()).is_err() {
        return EXIT_OUTPUT;
    }
    let body = match read_request(input) {
        Ok(body) => body,
        Err(code) => return code,
    };
    let Ok((Header { kind: kind::INIT, sequence }, reader)) = parse_body(&body) else { return EXIT_PROTOCOL };
    if reader.finish().is_err() {
        return EXIT_PROTOCOL;
    }
    let engine = match super::helper_library().and_then(InProcessEngine::bind) {
        Ok(engine) => engine,
        Err(message) => {
            send(output, Err((class::GENERAL, message)), sequence);
            return EXIT_INIT;
        }
    };
    if !send(output, Ok(FrameWriter::new(kind::INIT, sequence)), sequence) {
        return EXIT_OUTPUT;
    }
    let mut state = State {
        engine: &engine,
        documents: HashMap::new(),
        next_handle: 1,
        #[cfg(test)]
        misreply: None,
    };
    loop {
        let body = match read_request(input) {
            Ok(body) => body,
            Err(code) => return code,
        };
        let Ok((header, _)) = parse_body(&body) else { return EXIT_PROTOCOL };
        #[cfg(test)]
        let armed = state.misreply.take();
        let Ok(reply) = state.handle(header, body) else { return EXIT_PROTOCOL };
        #[cfg(test)]
        if let Some(misreply) = armed {
            if !super::faults::send_misreply(output, misreply, reply_frame(reply, header.sequence)) {
                return EXIT_OUTPUT;
            }
            continue;
        }
        if !send(output, reply, header.sequence) {
            return EXIT_OUTPUT;
        }
    }
}

fn read_request(input: &mut impl Read) -> Result<Vec<u8>, u32> {
    match read_frame(input, &|| MAX_REQUEST_FRAME) {
        Ok(body) => Ok(body),
        Err(FrameError::Closed) => Err(EXIT_CLOSED),
        Err(_) => Err(EXIT_PROTOCOL),
    }
}

/// The finished reply frame: the payload frame, or an error frame carrying the request's sequence.
pub(crate) fn reply_frame(reply: Reply, sequence: u32) -> Vec<u8> {
    match reply {
        Ok(frame) => frame.finish(),
        Err((error_class, message)) => {
            let mut frame = FrameWriter::new(kind::ERROR, sequence);
            put_error(&mut frame, error_class, &message);
            frame.finish()
        }
    }
}

fn send(output: &mut impl Write, reply: Reply, sequence: u32) -> bool {
    output.write_all(&reply_frame(reply, sequence)).and_then(|()| output.flush()).is_ok()
}

/// A loaded document and the read-only view of the app's section that holds its source. Field order
/// matters: the document (which borrows the view's bytes) is dropped first.
struct HelperDocument<'e> {
    document: InProcessDocument<'e>,
    _source: View,
}

struct State<'e> {
    engine: &'e InProcessEngine,
    documents: HashMap<u32, HelperDocument<'e>>,
    next_handle: u32,
    #[cfg(test)]
    misreply: Option<super::faults::Misreply>,
}

fn general(message: impl Into<String>) -> (u8, String) {
    (class::GENERAL, message.into())
}

fn load_error(error: LoadError) -> (u8, String) {
    (if error.is_password() { class::LOAD_PASSWORD } else { class::LOAD_OTHER }, error.to_string())
}

fn text_error(error: TextError) -> (u8, String) {
    match error {
        TextError::Page(message) => (class::TEXT_PAGE, message),
        TextError::Text(message) => (class::TEXT_TEXT, message),
        TextError::Bounds(message) => (class::TEXT_BOUNDS, message),
    }
}

fn render_error(error: RenderError) -> (u8, String) {
    match error {
        RenderError::Page(message) => (class::RENDER_PAGE, message),
        RenderError::Render(message) => (class::RENDER_RENDER, message),
        RenderError::Format(message) => (class::RENDER_FORMAT, message),
        RenderError::Layout => (class::RENDER_LAYOUT, String::new()),
    }
}

fn probe_error(error: ProbeError) -> (u8, String) {
    match error {
        ProbeError::Read(message) => (class::PROBE_READ, message),
        ProbeError::Render(message) => (class::PROBE_RENDER, message),
    }
}

impl<'e> State<'e> {
    /// The reply to one request; `Err` only when the request itself is malformed.
    fn handle(&mut self, header: Header, body: Vec<u8>) -> Result<Reply, Malformed> {
        let Header { kind: request_kind, sequence } = header;
        let (_, mut reader) = parse_body(&body)?;
        if request_kind == kind::LOAD {
            let (password, section, len) = get_load(&mut reader)?;
            reader.finish()?;
            return Ok(self.load(section, len, password.as_deref(), sequence));
        }
        #[cfg(test)]
        if request_kind == kind::FAULT {
            let fault = super::faults::Fault::read(&mut reader)?;
            reader.finish()?;
            return Ok(super::faults::run(fault, &mut self.misreply, sequence));
        }
        if !DOCUMENT_KINDS.contains(&request_kind) {
            return Err(Malformed::new(format!("unknown request kind {request_kind}")));
        }
        let handle = reader.u32()?;
        let mut out = FrameWriter::new(request_kind, sequence);
        if request_kind == kind::CLOSE {
            reader.finish()?;
            self.documents.remove(&handle);
            return Ok(Ok(out));
        }
        let Some(document) = self.documents.get(&handle).map(|loaded| &loaded.document) else {
            return Ok(Err(general("The PDF engine has no such document.")));
        };
        let result: Result<(), (u8, String)> = match request_kind {
            kind::PAGE_SIZES => {
                reader.finish()?;
                document.page_sizes().map(|sizes| put_page_sizes(&mut out, &sizes)).map_err(general)
            }
            kind::PLANNED_PAGE_SIZE => {
                let spec = get_spec(&mut reader)?;
                reader.finish()?;
                document.planned_page_size(&spec).map(|pair| put_pair(&mut out, pair)).map_err(general)
            }
            kind::VISIBLE_TEXT => {
                let target = get_target(&mut reader)?;
                reader.finish()?;
                match document.visible_text(target.as_target()) {
                    Ok(text) if text.len() > MAX_TEXT_REPLY_BYTES => Err((class::TEXT_TEXT, TOO_MUCH_TEXT.into())),
                    Ok(text) => Ok(put_text(&mut out, &text)),
                    Err(error) => Err(text_error(error)),
                }
            }
            kind::TEXT_GEOMETRY | kind::TEXT_GEOMETRY_WITH_SOURCE => {
                let target = get_target(&mut reader)?;
                let (id, index, revision) = (reader.u64()?, reader.u16()?, reader.u64()?);
                reader.finish()?;
                if request_kind == kind::TEXT_GEOMETRY {
                    document.text_geometry(target.as_target(), id, index, revision).map(|geometry| put_geometry(&mut out, &geometry)).map_err(general)
                } else {
                    document.text_geometry_with_source(target.as_target(), id, index, revision).map(|inspected| put_inspected(&mut out, &inspected)).map_err(general)
                }
            }
            kind::BOOKMARKS => {
                let limit = reader.u32()? as usize;
                reader.finish()?;
                let mut bookmarks = Vec::new();
                let mut title_bytes = 0_usize;
                let visited = document.bookmarks(limit, &mut |mut bookmark| {
                    if let Some(title) = &mut bookmark.title {
                        let room = BOOKMARK_TITLE_BUDGET + 1 - title_bytes;
                        if title.len() > room {
                            title.truncate(floor_char_boundary(title, room));
                        }
                        title_bytes += title.len();
                    }
                    bookmarks.push(bookmark);
                    Ok(if title_bytes > BOOKMARK_TITLE_BUDGET { std::ops::ControlFlow::Break(()) } else { std::ops::ControlFlow::Continue(()) })
                });
                visited.map(|()| put_bookmarks(&mut out, &bookmarks)).map_err(general)
            }
            kind::BOOKMARK_PAGE_VIEW => {
                let spec = get_spec(&mut reader)?;
                reader.finish()?;
                document.bookmark_page_view(&spec).map(|view| out.opt(view, put_view)).map_err(general)
            }
            kind::PROPERTIES => {
                let (size, dimensions) = get_properties_request(&mut reader)?;
                reader.finish()?;
                document.properties(size, &dimensions).map(|properties| put_properties(&mut out, &properties)).map_err(general)
            }
            kind::DISPLAYED_CROP => {
                let spec = get_spec(&mut reader)?;
                let rect = get_crop_rect(&mut reader)?;
                reader.finish()?;
                document.displayed_crop(&spec, rect).map(|crop| put_crop(&mut out, crop)).map_err(general)
            }
            kind::DISPLAYED_NOTE_RECTS => {
                let spec = get_spec(&mut reader)?;
                let rects = get_crops(&mut reader)?;
                reader.finish()?;
                document.displayed_note_rects(&spec, &rects).map(|rects| put_display_rects(&mut out, &rects)).map_err(general)
            }
            kind::FORM_VALUES => {
                let fields = get_form_fields(&mut reader)?;
                reader.finish()?;
                put_form_values(&mut out, document.form_values(&fields).as_ref());
                Ok(())
            }
            kind::SOURCE_PAGE_SIZE => {
                let index = reader.usize()?;
                reader.finish()?;
                document.source_page_size(index).map(|pair| put_pair(&mut out, pair)).map_err(general)
            }
            kind::THUMBNAIL_CHECK => {
                let index = reader.usize()?;
                reader.finish()?;
                document.thumbnail_check(index).map(|valid| out.bool(valid)).map_err(probe_error)
            }
            kind::RENDER_BGRA => {
                let target = get_target(&mut reader)?;
                let mode = get_mode(&mut reader)?;
                let (width, height, section) = (reader.u32()?, reader.u32()?, reader.u64()?);
                reader.finish()?;
                let length = bgra_len(width, height).filter(|length| (1..=MAX_BGRA_BYTES).contains(length));
                let target_view = length.ok_or_else(|| "The bitmap size is out of range.".to_owned()).and_then(|length| unsafe { View::map_passed(section, length, true) });
                match target_view {
                    Ok(mut view) => document.render_bgra_into(target.as_target(), mode, width, height, view.as_mut_slice()).map_err(render_error),
                    Err(message) => Err((class::RENDER_RENDER, message)),
                }
            }
            kind::RENDER_VIEWER_RGBA => {
                let spec = get_spec(&mut reader)?;
                let (width, height, section) = (reader.u32()?, reader.u32()?, reader.u64()?);
                reader.finish()?;
                let length = bgra_len(width, height).filter(|length| (1..=MAX_BGRA_BYTES).contains(length));
                let target_view = length.ok_or_else(|| "The bitmap size is out of range.".to_owned()).and_then(|length| unsafe { View::map_passed(section, length, true) });
                match target_view {
                    Ok(mut view) => document.render_viewer_rgba_into(&spec, width, height, view.as_mut_slice()).map_err(render_error),
                    Err(message) => Err((class::RENDER_RENDER, message)),
                }
            }
            #[cfg(test)]
            kind::PAGE_LABELS => {
                reader.finish()?;
                put_labels(&mut out, &document.engine_page_labels());
                Ok(())
            }
            other => return Err(Malformed::new(format!("unknown request kind {other}"))),
        };
        Ok(result.map(|()| out))
    }

    /// Maps the app's read-only source section (closing the passed handle either way) and loads the
    /// document over the mapping, without copying the source into this process.
    fn load(&mut self, section: u64, len: usize, password: Option<&str>, sequence: u32) -> Reply {
        let source = unsafe { View::map_passed(section, len, false) }.map_err(|message| (class::LOAD_OTHER, message))?;
        #[cfg(test)]
        super::faults::exit_if_crash_on_load(source.as_slice());
        if self.documents.len() >= MAX_HELPER_DOCUMENTS {
            return Err(general("The PDF engine has too many documents open."));
        }
        let bytes: &'e [u8] = unsafe { source.detached_slice() };
        let document = self.engine.load_from_slice(bytes, password).map_err(load_error)?;
        let handle = self.next_handle;
        self.next_handle = self.next_handle.checked_add(1).unwrap_or(1);
        let loaded = Loaded { handle, page_count: document.page_count(), unprotected: document.is_unprotected() };
        self.documents.insert(handle, HelperDocument { document, _source: source });
        let mut out = FrameWriter::new(kind::LOAD, sequence);
        put_loaded(&mut out, &loaded);
        Ok(out)
    }
}
