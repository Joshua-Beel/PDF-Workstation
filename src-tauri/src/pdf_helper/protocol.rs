//! Wire format between the app and the PDF-engine helper.
//!
//! The helper first writes [`SYNC`], then frames. A frame is a little-endian `u32` body length followed
//! by the body; every body starts with an 8-byte header: protocol version (`u16`), kind (`u16`) and
//! sequence (`u32`). Requests and replies mirror the [`crate::pdf_engine::PdfEngine`] and
//! [`crate::pdf_engine::EngineDocument`] methods one to one. A reply carries the request's kind and
//! sequence, or [`kind::ERROR`] with an error class and the engine's message.
//!
//! Every reader checks a frame's length against the largest frame the pending request allows before it
//! allocates, and every decoder bounds counts and string lengths, rejects unknown tags and requires the
//! payload to be consumed exactly. The app treats helper replies as untrusted: a document that gets code
//! running in the helper controls everything the helper writes.

use std::collections::HashMap;
use std::io::{ErrorKind, Read};

use crate::bookmark_destinations::{BookmarkDestination, DestinationMode, DestinationView};
use crate::comments::DisplayRect;
use crate::document_properties::{DocumentProperties, MetadataEntry, PageDimensions, SecurityProperties};
use crate::editor::{CropBox, PageSpec};
use crate::forms::{ChoiceInfo, ChoiceOption, FormField, RadioInfo, RadioOption};
use crate::pdf_engine::{EngineBookmark, PageSize, PageTarget, RenderMode};
use crate::text_geometry::{InspectedGeometry, PageTextGeometry, TextBounds, TextCharacter, MAX_CHARACTERS};

pub(crate) const PROTOCOL_VERSION: u16 = 2;
/// Written once by the helper before its first frame.
pub(crate) const SYNC: [u8; 8] = *b"SM5PDFH1";
pub(crate) const HEADER_LEN: usize = 8;
/// Largest request. Sources and bitmaps travel in shared sections, so requests carry only small data (the
/// largest is a form-value check with its field list).
pub(crate) const MAX_REQUEST_FRAME: usize = 64 * 1024 * 1024;
/// Replies that carry no document content: hello, init, load, close, sizes, flags, errors.
pub(crate) const MAX_CONTROL_REPLY: usize = 64 * 1024;
/// Replies that carry text or structured page data.
pub(crate) const MAX_DTO_REPLY: usize = 16 * 1024 * 1024;
/// An engine error message.
pub(crate) const MAX_MESSAGE_BYTES: usize = 16 * 1024;
/// The largest BGRA render the helper is asked for (page-image export is the largest caller, 128 MiB).
/// Note for step 5: a viewer render wider than about 13,400 px (height capped at 5,000) or any render over
/// 67 M pixels exceeds this and is refused ("larger than the PDF engine renders"), although
/// `MAX_VIEWER_WIDTH` allows 16,384. Today's callers stay below it (the viewer asks for at most 3,000 px
/// wide, print 16 M pixels, export 32 M pixels).
pub(crate) const MAX_BGRA_BYTES: usize = 256 * 1024 * 1024;
pub(crate) const MAX_VIEWER_WIDTH: i32 = 16_384;
pub(crate) const MAX_VIEWER_HEIGHT: u32 = 5_000;
pub(crate) const MAX_PAGES: usize = 65_536;
/// Documents one helper holds at once (open tabs, note copies, print snapshots and output checks).
pub(crate) const MAX_HELPER_DOCUMENTS: usize = 1_024;
/// Bookmark title bytes the helper sends at most: past the app's 1 MiB read-text budget, so the app
/// refuses an outline with too much text exactly as it does in process; the title that crosses it is cut.
pub(crate) const BOOKMARK_TITLE_BUDGET: usize = 2 * 1024 * 1024;
/// Page text larger than this is refused in the helper with the app's own too-much-text message.
pub(crate) const MAX_TEXT_REPLY_BYTES: usize = MAX_DTO_REPLY - 64 * 1024;
#[cfg(test)]
const MAX_LABEL_BYTES: usize = 64 * 1024;
const MAX_FIELD_TEXT_BYTES: usize = 64 * 1024;
const MAX_FORM_FIELDS: usize = 100_000;
const MAX_METADATA_ENTRIES: usize = 8;
const MAX_METADATA_VALUE_BYTES: usize = 4 * 4096;
const MAX_DIMENSION_GROUPS: usize = 128;
const MAX_VERSION_BYTES: usize = 16;
const MAX_PASSWORD_BYTES: usize = 64 * 1024;

pub(crate) mod kind {
    pub(crate) const HELLO: u16 = 1;
    pub(crate) const INIT: u16 = 2;
    pub(crate) const LOAD: u16 = 3;
    pub(crate) const CLOSE: u16 = 4;
    pub(crate) const PAGE_SIZES: u16 = 5;
    pub(crate) const PLANNED_PAGE_SIZE: u16 = 6;
    /// Retired in step 4 (viewer PNGs are encoded in the app); never reuse the value.
    #[allow(dead_code)]
    pub(crate) const RETIRED_VIEWER_PNG: u16 = 7;
    pub(crate) const VISIBLE_TEXT: u16 = 8;
    pub(crate) const TEXT_GEOMETRY: u16 = 9;
    pub(crate) const TEXT_GEOMETRY_WITH_SOURCE: u16 = 10;
    pub(crate) const BOOKMARKS: u16 = 11;
    pub(crate) const BOOKMARK_PAGE_VIEW: u16 = 12;
    pub(crate) const PROPERTIES: u16 = 13;
    pub(crate) const DISPLAYED_CROP: u16 = 14;
    pub(crate) const DISPLAYED_NOTE_RECTS: u16 = 15;
    pub(crate) const FORM_VALUES: u16 = 16;
    pub(crate) const SOURCE_PAGE_SIZE: u16 = 17;
    pub(crate) const THUMBNAIL_CHECK: u16 = 18;
    pub(crate) const RENDER_BGRA: u16 = 19;
    /// A viewer render in RGBA order into an app-owned section (step 4).
    pub(crate) const RENDER_VIEWER_RGBA: u16 = 21;
    #[cfg(test)]
    pub(crate) const PAGE_LABELS: u16 = 20;
    #[cfg(test)]
    pub(crate) const FAULT: u16 = 0x7f00;
    pub(crate) const ERROR: u16 = 0xffff;
}

/// Error classes in an [`kind::ERROR`] reply: which engine error variant the message belongs to.
pub(crate) mod class {
    pub(crate) const GENERAL: u8 = 0;
    pub(crate) const LOAD_PASSWORD: u8 = 1;
    pub(crate) const LOAD_OTHER: u8 = 2;
    pub(crate) const TEXT_PAGE: u8 = 3;
    pub(crate) const TEXT_TEXT: u8 = 4;
    pub(crate) const TEXT_BOUNDS: u8 = 5;
    pub(crate) const RENDER_PAGE: u8 = 6;
    pub(crate) const RENDER_RENDER: u8 = 7;
    pub(crate) const RENDER_FORMAT: u8 = 8;
    pub(crate) const RENDER_LAYOUT: u8 = 9;
    pub(crate) const PROBE_READ: u8 = 10;
    pub(crate) const PROBE_RENDER: u8 = 11;
    pub(crate) const LOAD: &[u8] = &[GENERAL, LOAD_PASSWORD, LOAD_OTHER];
    pub(crate) const TEXT: &[u8] = &[TEXT_PAGE, TEXT_TEXT, TEXT_BOUNDS];
    pub(crate) const RENDER: &[u8] = &[RENDER_PAGE, RENDER_RENDER, RENDER_FORMAT, RENDER_LAYOUT];
    pub(crate) const PROBE: &[u8] = &[PROBE_READ, PROBE_RENDER];
    pub(crate) const ONLY_GENERAL: &[u8] = &[GENERAL];
}

/// A frame or payload that does not follow the protocol.
#[derive(Debug, Clone, Eq, PartialEq)]
pub(crate) struct Malformed(pub(crate) String);

impl Malformed {
    pub(crate) fn new(reason: impl Into<String>) -> Self {
        Self(reason.into())
    }
}

/// Builds one frame: length placeholder, header, payload.
pub(crate) struct FrameWriter {
    bytes: Vec<u8>,
}

impl FrameWriter {
    pub(crate) fn new(kind: u16, sequence: u32) -> Self {
        Self::with_capacity(kind, sequence, 64)
    }

    pub(crate) fn with_capacity(kind: u16, sequence: u32, payload: usize) -> Self {
        let mut bytes = Vec::with_capacity(4 + HEADER_LEN + payload);
        bytes.extend_from_slice(&[0; 4]);
        bytes.extend_from_slice(&PROTOCOL_VERSION.to_le_bytes());
        bytes.extend_from_slice(&kind.to_le_bytes());
        bytes.extend_from_slice(&sequence.to_le_bytes());
        Self { bytes }
    }

    pub(crate) fn u8(&mut self, value: u8) {
        self.bytes.push(value);
    }
    pub(crate) fn u16(&mut self, value: u16) {
        self.bytes.extend_from_slice(&value.to_le_bytes());
    }
    pub(crate) fn u32(&mut self, value: u32) {
        self.bytes.extend_from_slice(&value.to_le_bytes());
    }
    pub(crate) fn u64(&mut self, value: u64) {
        self.bytes.extend_from_slice(&value.to_le_bytes());
    }
    pub(crate) fn i32(&mut self, value: i32) {
        self.bytes.extend_from_slice(&value.to_le_bytes());
    }
    pub(crate) fn f32(&mut self, value: f32) {
        self.u32(value.to_bits());
    }
    pub(crate) fn f64(&mut self, value: f64) {
        self.u64(value.to_bits());
    }
    pub(crate) fn bool(&mut self, value: bool) {
        self.u8(u8::from(value));
    }
    /// A `u32` count; callers bound their collections far below `u32::MAX`.
    pub(crate) fn count(&mut self, value: usize) {
        self.u32(u32::try_from(value).unwrap_or(u32::MAX));
    }
    pub(crate) fn str(&mut self, value: &str) {
        self.count(value.len());
        self.bytes.extend_from_slice(value.as_bytes());
    }
    pub(crate) fn opt<T>(&mut self, value: Option<T>, put: impl FnOnce(&mut Self, T)) {
        match value {
            Some(value) => {
                self.u8(1);
                put(self, value);
            }
            None => self.u8(0),
        }
    }
    /// Bytes with no length prefix (the rest of the payload).
    #[cfg(test)]
    pub(crate) fn raw(&mut self, value: &[u8]) {
        self.bytes.extend_from_slice(value);
    }
    /// The finished frame. A body longer than `u32::MAX` gets the length `u32::MAX`, which every reader refuses.
    pub(crate) fn finish(mut self) -> Vec<u8> {
        let body = u32::try_from(self.bytes.len() - 4).unwrap_or(u32::MAX);
        self.bytes[..4].copy_from_slice(&body.to_le_bytes());
        self.bytes
    }
}

/// Reads a payload; every method fails instead of reading past the end.
pub(crate) struct WireReader<'a> {
    bytes: &'a [u8],
}

impl<'a> WireReader<'a> {
    pub(crate) fn new(bytes: &'a [u8]) -> Self {
        Self { bytes }
    }
    fn take(&mut self, count: usize) -> Result<&'a [u8], Malformed> {
        if count > self.bytes.len() {
            return Err(Malformed::new("payload ends early"));
        }
        let (head, tail) = self.bytes.split_at(count);
        self.bytes = tail;
        Ok(head)
    }
    pub(crate) fn remaining(&self) -> usize {
        self.bytes.len()
    }
    pub(crate) fn u8(&mut self) -> Result<u8, Malformed> {
        Ok(self.take(1)?[0])
    }
    pub(crate) fn u16(&mut self) -> Result<u16, Malformed> {
        Ok(u16::from_le_bytes(self.take(2)?.try_into().unwrap()))
    }
    pub(crate) fn u32(&mut self) -> Result<u32, Malformed> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
    pub(crate) fn u64(&mut self) -> Result<u64, Malformed> {
        Ok(u64::from_le_bytes(self.take(8)?.try_into().unwrap()))
    }
    pub(crate) fn i32(&mut self) -> Result<i32, Malformed> {
        Ok(i32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
    pub(crate) fn f32(&mut self) -> Result<f32, Malformed> {
        Ok(f32::from_bits(self.u32()?))
    }
    pub(crate) fn f64(&mut self) -> Result<f64, Malformed> {
        Ok(f64::from_bits(self.u64()?))
    }
    pub(crate) fn finite_f32(&mut self) -> Result<f32, Malformed> {
        let value = self.f32()?;
        if value.is_finite() { Ok(value) } else { Err(Malformed::new("non-finite number")) }
    }
    pub(crate) fn finite_f64(&mut self) -> Result<f64, Malformed> {
        let value = self.f64()?;
        if value.is_finite() { Ok(value) } else { Err(Malformed::new("non-finite number")) }
    }
    pub(crate) fn bool(&mut self) -> Result<bool, Malformed> {
        match self.u8()? {
            0 => Ok(false),
            1 => Ok(true),
            other => Err(Malformed::new(format!("invalid boolean {other}"))),
        }
    }
    pub(crate) fn usize(&mut self) -> Result<usize, Malformed> {
        usize::try_from(self.u64()?).map_err(|_| Malformed::new("index out of range"))
    }
    /// A count of at most `max` items, each at least `min_item_bytes` long, that fits in what is left.
    pub(crate) fn count(&mut self, max: usize, min_item_bytes: usize) -> Result<usize, Malformed> {
        let count = self.u32()? as usize;
        if count > max {
            return Err(Malformed::new(format!("count {count} exceeds {max}")));
        }
        if count.saturating_mul(min_item_bytes.max(1)) > self.remaining() {
            return Err(Malformed::new("count exceeds the payload"));
        }
        Ok(count)
    }
    pub(crate) fn str(&mut self, max: usize) -> Result<String, Malformed> {
        let length = self.u32()? as usize;
        if length > max {
            return Err(Malformed::new(format!("string of {length} bytes exceeds {max}")));
        }
        let bytes = self.take(length)?;
        String::from_utf8(bytes.to_vec()).map_err(|_| Malformed::new("string is not UTF-8"))
    }
    pub(crate) fn opt<T>(&mut self, get: impl FnOnce(&mut Self) -> Result<T, Malformed>) -> Result<Option<T>, Malformed> {
        match self.u8()? {
            0 => Ok(None),
            1 => get(self).map(Some),
            other => Err(Malformed::new(format!("invalid option tag {other}"))),
        }
    }
    pub(crate) fn finish(self) -> Result<(), Malformed> {
        if self.bytes.is_empty() { Ok(()) } else { Err(Malformed::new(format!("{} trailing bytes", self.bytes.len()))) }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Header {
    pub(crate) kind: u16,
    pub(crate) sequence: u32,
}

/// Splits a frame body into its header and payload; the version must match exactly.
pub(crate) fn parse_body(body: &[u8]) -> Result<(Header, WireReader<'_>), Malformed> {
    if body.len() < HEADER_LEN {
        return Err(Malformed::new("frame shorter than its header"));
    }
    let mut reader = WireReader::new(body);
    let version = reader.u16()?;
    if version != PROTOCOL_VERSION {
        return Err(Malformed::new(format!("protocol version {version}, expected {PROTOCOL_VERSION}")));
    }
    let kind = reader.u16()?;
    let sequence = reader.u32()?;
    Ok((Header { kind, sequence }, reader))
}

/// Why a frame could not be read.
#[derive(Debug)]
pub(crate) enum FrameError {
    /// The stream ended cleanly before a new frame.
    Closed,
    /// The stream ended inside a frame.
    Truncated,
    /// The length prefix is shorter than a header or longer than the limit; nothing was allocated.
    BadLength(u64),
    Io,
}

/// Reads one frame body of at most `limit()` bytes. The limit is read once the length prefix has arrived
/// (it belongs to the request that reply answers), and the body is allocated only after the check.
pub(crate) fn read_frame(input: &mut impl Read, limit: &dyn Fn() -> usize) -> Result<Vec<u8>, FrameError> {
    let mut prefix = [0_u8; 4];
    let mut filled = 0;
    while filled < prefix.len() {
        match input.read(&mut prefix[filled..]) {
            Ok(0) if filled == 0 => return Err(FrameError::Closed),
            Ok(0) => return Err(FrameError::Truncated),
            Ok(count) => filled += count,
            Err(error) if error.kind() == ErrorKind::Interrupted => {}
            Err(error) if error.kind() == ErrorKind::BrokenPipe => return Err(if filled == 0 { FrameError::Closed } else { FrameError::Truncated }),
            Err(_) => return Err(FrameError::Io),
        }
    }
    let length = u32::from_le_bytes(prefix) as usize;
    if length < HEADER_LEN || length > limit() {
        return Err(FrameError::BadLength(length as u64));
    }
    let mut body = vec![0_u8; length];
    input.read_exact(&mut body).map_err(|error| match error.kind() {
        ErrorKind::UnexpectedEof | ErrorKind::BrokenPipe => FrameError::Truncated,
        _ => FrameError::Io,
    })?;
    Ok(body)
}

// Requests (written by the app, read by the helper).

pub(crate) fn put_spec(writer: &mut FrameWriter, spec: &PageSpec) {
    writer.u64(spec.source as u64);
    writer.i32(spec.turns);
    writer.opt(spec.crop, put_crop);
}

/// The page plan entry without its notes: the engine applies only the source page, crop and turns.
pub(crate) fn get_spec(reader: &mut WireReader<'_>) -> Result<PageSpec, Malformed> {
    Ok(PageSpec { source: reader.usize()?, turns: reader.i32()?, crop: reader.opt(get_crop)?, notes: Vec::new() })
}

pub(crate) fn put_crop(writer: &mut FrameWriter, crop: CropBox) {
    for value in [crop.left, crop.bottom, crop.right, crop.top] {
        writer.f32(value);
    }
}

pub(crate) fn get_crop(reader: &mut WireReader<'_>) -> Result<CropBox, Malformed> {
    Ok(CropBox { left: reader.f32()?, bottom: reader.f32()?, right: reader.f32()?, top: reader.f32()? })
}

/// A [`PageTarget`] the helper owns.
pub(crate) enum OwnedTarget {
    Planned(PageSpec),
    Source(usize),
}

impl OwnedTarget {
    pub(crate) fn as_target(&self) -> PageTarget<'_> {
        match self {
            Self::Planned(spec) => PageTarget::Planned(spec),
            Self::Source(index) => PageTarget::Source(*index),
        }
    }
}

pub(crate) fn put_target(writer: &mut FrameWriter, target: PageTarget<'_>) {
    match target {
        PageTarget::Planned(spec) => {
            writer.u8(0);
            put_spec(writer, spec);
        }
        PageTarget::Source(index) => {
            writer.u8(1);
            writer.u64(index as u64);
        }
    }
}

pub(crate) fn get_target(reader: &mut WireReader<'_>) -> Result<OwnedTarget, Malformed> {
    match reader.u8()? {
        0 => Ok(OwnedTarget::Planned(get_spec(reader)?)),
        1 => Ok(OwnedTarget::Source(reader.usize()?)),
        other => Err(Malformed::new(format!("invalid page target {other}"))),
    }
}

pub(crate) fn put_mode(writer: &mut FrameWriter, mode: RenderMode) {
    writer.u8(match mode {
        RenderMode::Print => 0,
        RenderMode::Plain => 1,
        RenderMode::Viewer => 2,
    });
}

/// Wire values are fixed once assigned: 0 Print, 1 Plain, 2 Viewer. Anything else is refused.
pub(crate) fn get_mode(reader: &mut WireReader<'_>) -> Result<RenderMode, Malformed> {
    match reader.u8()? {
        0 => Ok(RenderMode::Print),
        1 => Ok(RenderMode::Plain),
        2 => Ok(RenderMode::Viewer),
        other => Err(Malformed::new(format!("invalid render mode {other}"))),
    }
}

/// A load names the source by a read-only section handle (its value in the helper) and the source length.
pub(crate) fn put_load(writer: &mut FrameWriter, password: Option<&str>, section: u64, len: usize) {
    writer.opt(password, |writer, password| writer.str(password));
    writer.u64(section);
    writer.u64(len as u64);
}

pub(crate) fn get_load(reader: &mut WireReader<'_>) -> Result<(Option<String>, u64, usize), Malformed> {
    let password = reader.opt(|reader| reader.str(MAX_PASSWORD_BYTES))?;
    let section = reader.u64()?;
    let len = reader.usize()?;
    if len == 0 || len as u64 > crate::open_limits::MAX_OPEN_SOURCE_BYTES {
        return Err(Malformed::new("source length out of range"));
    }
    Ok((password, section, len))
}


pub(crate) fn put_form_fields(writer: &mut FrameWriter, fields: &[FormField]) {
    writer.count(fields.len());
    for field in fields {
        writer.str(&field.name);
        writer.str(&field.value);
        writer.opt(field.checked, FrameWriter::bool);
        writer.opt(field.on_state.as_deref(), FrameWriter::str);
        writer.opt(field.radio.as_ref(), |writer, radio| {
            writer.count(radio.options.len());
            for option in &radio.options {
                writer.str(&option.option_id);
                writer.str(&option.label);
            }
            writer.opt(radio.selected_option_id.as_deref(), FrameWriter::str);
        });
        writer.opt(field.choice.as_ref(), |writer, choice| {
            writer.u8(u8::from(choice.presentation == "dropdown"));
            writer.count(choice.options.len());
            for option in &choice.options {
                writer.str(&option.option_id);
                writer.str(&option.label);
                writer.str(&option.export);
            }
            writer.opt(choice.selected_option_id.as_deref(), FrameWriter::str);
            writer.str(&choice.text);
            writer.bool(choice.multiple);
            writer.bool(choice.editable);
            writer.count(choice.selected_option_ids.len());
            for id in &choice.selected_option_ids {
                writer.str(id);
            }
        });
    }
}

/// The form fields as the engine's form check reads them. Identifiers the check does not read
/// (field id, page, length limit, export values, object ids) are left empty.
pub(crate) fn get_form_fields(reader: &mut WireReader<'_>) -> Result<Vec<FormField>, Malformed> {
    let text = |reader: &mut WireReader<'_>| reader.str(MAX_FIELD_TEXT_BYTES);
    let count = reader.count(MAX_FORM_FIELDS, 11)?;
    let mut fields = Vec::with_capacity(count);
    for _ in 0..count {
        let name = text(reader)?;
        let value = text(reader)?;
        let checked = reader.opt(WireReader::bool)?;
        let on_state = reader.opt(text)?;
        let radio = reader.opt(|reader| {
            let count = reader.count(MAX_FORM_FIELDS, 8)?;
            let options = (0..count)
                .map(|_| Ok(RadioOption { option_id: text(reader)?, label: text(reader)?, object: (0, 0) }))
                .collect::<Result<Vec<_>, Malformed>>()?;
            Ok(RadioInfo { options, selected_option_id: reader.opt(text)? })
        })?;
        let choice = reader.opt(|reader| {
            let presentation = if reader.bool()? { "dropdown" } else { "list" };
            let count = reader.count(MAX_FORM_FIELDS, 12)?;
            let options = (0..count)
                .map(|_| Ok(ChoiceOption { option_id: text(reader)?, label: text(reader)?, export: text(reader)? }))
                .collect::<Result<Vec<_>, Malformed>>()?;
            let selected_option_id = reader.opt(text)?;
            let typed = text(reader)?;
            let multiple = reader.bool()?;
            let editable = reader.bool()?;
            let selected = reader.count(MAX_FORM_FIELDS, 4)?;
            let selected_option_ids = (0..selected).map(|_| text(reader)).collect::<Result<Vec<_>, Malformed>>()?;
            Ok(ChoiceInfo { presentation, options, selected_option_id, multiple, editable, selected_option_ids, text: typed })
        })?;
        fields.push(FormField { field_id: String::new(), name, page: 0, value, max_length: None, checked, on_state, radio, choice });
    }
    Ok(fields)
}

// Replies (written by the helper, validated by the app).

pub(crate) fn put_error(writer: &mut FrameWriter, error_class: u8, message: &str) {
    writer.u8(error_class);
    writer.str(truncate(message, MAX_MESSAGE_BYTES));
}

pub(crate) fn get_error(reader: &mut WireReader<'_>, allowed: &[u8]) -> Result<(u8, String), Malformed> {
    let error_class = reader.u8()?;
    if !allowed.contains(&error_class) {
        return Err(Malformed::new(format!("error class {error_class} does not fit this request")));
    }
    Ok((error_class, reader.str(MAX_MESSAGE_BYTES)?))
}

fn truncate(message: &str, max: usize) -> &str {
    if message.len() <= max {
        return message;
    }
    let mut end = max;
    while !message.is_char_boundary(end) {
        end -= 1;
    }
    &message[..end]
}

/// Load reply: helper document handle, page count, whether the document has no security handler, and
/// PDFium's copy/extract permission (version 2).
pub(crate) struct Loaded {
    pub(crate) handle: u32,
    pub(crate) page_count: usize,
    pub(crate) unprotected: bool,
    pub(crate) text_extraction_allowed: bool,
}

pub(crate) fn put_loaded(writer: &mut FrameWriter, loaded: &Loaded) {
    writer.u32(loaded.handle);
    writer.u64(loaded.page_count as u64);
    writer.bool(loaded.unprotected);
    writer.bool(loaded.text_extraction_allowed);
}

pub(crate) fn get_loaded(reader: &mut WireReader<'_>) -> Result<Loaded, Malformed> {
    let handle = reader.u32()?;
    let page_count = reader.usize()?;
    if page_count > i32::MAX as usize {
        return Err(Malformed::new("page count out of range"));
    }
    Ok(Loaded { handle, page_count, unprotected: reader.bool()?, text_extraction_allowed: reader.bool()? })
}

pub(crate) fn put_page_sizes(writer: &mut FrameWriter, sizes: &[PageSize]) {
    writer.count(sizes.len());
    for size in sizes {
        writer.f32(size.width);
        writer.f32(size.height);
    }
}

/// Page sizes must match the page count reported at load and be finite and positive, as the engine
/// itself requires before it answers.
pub(crate) fn get_page_sizes(reader: &mut WireReader<'_>, page_count: usize) -> Result<Vec<PageSize>, Malformed> {
    let count = reader.count(MAX_PAGES, 8)?;
    if count != page_count {
        return Err(Malformed::new(format!("{count} page sizes for {page_count} pages")));
    }
    (0..count)
        .map(|_| {
            let size = PageSize { width: reader.finite_f32()?, height: reader.finite_f32()? };
            if size.width <= 0.0 || size.height <= 0.0 { Err(Malformed::new("page size not positive")) } else { Ok(size) }
        })
        .collect()
}

pub(crate) fn put_pair(writer: &mut FrameWriter, (width, height): (f32, f32)) {
    writer.f32(width);
    writer.f32(height);
}

/// A page size from the helper (planned or source page): both finite and positive, as every page PDFium
/// can render has; anything else is a malformed reply.
pub(crate) fn get_page_size(reader: &mut WireReader<'_>) -> Result<(f32, f32), Malformed> {
    let (width, height) = (reader.finite_f32()?, reader.finite_f32()?);
    if width > 0.0 && height > 0.0 { Ok((width, height)) } else { Err(Malformed::new("page size not positive")) }
}

/// A crop box from the helper: every coordinate finite.
pub(crate) fn get_finite_crop(reader: &mut WireReader<'_>) -> Result<CropBox, Malformed> {
    Ok(CropBox { left: reader.finite_f32()?, bottom: reader.finite_f32()?, right: reader.finite_f32()?, top: reader.finite_f32()? })
}

pub(crate) fn get_pair(reader: &mut WireReader<'_>) -> Result<(f32, f32), Malformed> {
    Ok((reader.f32()?, reader.f32()?))
}

/// The pixel size of a viewer render of a page `page_width` x `page_height` points wide at `width`
/// pixels, at most 5,000 high: the same arithmetic pdfium-render 0.9.4 applies for
/// `set_target_width(width).set_maximum_height(5000)` (`PdfRenderConfig::apply_to_page`), so the app can
/// size the bitmap before asking. `None` when the result is empty.
pub(crate) fn viewer_size(page_width: f32, page_height: f32, width: i32) -> Option<(u32, u32)> {
    let mut scale = width as f32 / page_width;
    let maximum = MAX_VIEWER_HEIGHT as f32;
    if page_height * scale > maximum {
        scale = maximum / page_height;
    }
    let (pixels_wide, pixels_high) = ((page_width * scale).round() as i32, (page_height * scale).round() as i32);
    (pixels_wide > 0 && pixels_high > 0).then_some((pixels_wide as u32, pixels_high as u32))
}

pub(crate) fn put_text(writer: &mut FrameWriter, text: &str) {
    writer.str(text);
}

pub(crate) fn get_text(reader: &mut WireReader<'_>) -> Result<String, Malformed> {
    reader.str(MAX_TEXT_REPLY_BYTES)
}

/// What a geometry reply must echo.
#[derive(Clone, Copy)]
pub(crate) struct GeometryRequest {
    pub(crate) id: u64,
    pub(crate) index: u16,
    pub(crate) revision: u64,
}

pub(crate) fn put_geometry(writer: &mut FrameWriter, geometry: &PageTextGeometry) {
    writer.u64(geometry.id);
    writer.u16(geometry.page);
    writer.u64(geometry.revision);
    writer.bool(geometry.status == "ok");
    writer.bool(geometry.truncated);
    writer.count(geometry.characters.len());
    for character in &geometry.characters {
        writer.str(&character.text);
        writer.opt(character.bounds.as_ref(), |writer, bounds| {
            for value in [bounds.x, bounds.y, bounds.width, bounds.height] {
                writer.f32(value);
            }
        });
        writer.u16(character.angle);
    }
    writer.opt(geometry.reason.as_deref(), FrameWriter::str);
}

pub(crate) fn get_geometry(reader: &mut WireReader<'_>, expected: GeometryRequest) -> Result<PageTextGeometry, Malformed> {
    let (id, page, revision) = (reader.u64()?, reader.u16()?, reader.u64()?);
    if (id, page, revision) != (expected.id, expected.index, expected.revision) {
        return Err(Malformed::new("text geometry for another page or revision"));
    }
    let status = if reader.bool()? { "ok" } else { "unsupported" };
    let truncated = reader.bool()?;
    let count = reader.count(MAX_CHARACTERS, 8)?;
    let mut characters = Vec::with_capacity(count);
    for _ in 0..count {
        let text = reader.str(4)?;
        if text.chars().count() != 1 {
            return Err(Malformed::new("a text character is not one character"));
        }
        let bounds = reader.opt(|reader| Ok(TextBounds { x: reader.finite_f32()?, y: reader.finite_f32()?, width: reader.finite_f32()?, height: reader.finite_f32()? }))?;
        let angle = reader.u16()?;
        if angle >= 360 {
            return Err(Malformed::new("text angle out of range"));
        }
        characters.push(TextCharacter { text, bounds, angle });
    }
    let reason = reader.opt(|reader| reader.str(MAX_MESSAGE_BYTES))?;
    Ok(PageTextGeometry { id, page, revision, status, truncated, characters, reason })
}

pub(crate) fn put_inspected(writer: &mut FrameWriter, inspected: &InspectedGeometry) {
    put_geometry(writer, &inspected.geometry);
    writer.count(inspected.source_bounds.len());
    for bounds in &inspected.source_bounds {
        writer.opt(*bounds, put_crop);
    }
}

/// Text geometry with source-space bounds: one bounds entry per character, as the engine produces them.
pub(crate) fn get_inspected(reader: &mut WireReader<'_>, expected: GeometryRequest) -> Result<InspectedGeometry, Malformed> {
    let geometry = get_geometry(reader, expected)?;
    let count = reader.count(MAX_CHARACTERS, 1)?;
    if count != geometry.characters.len() {
        return Err(Malformed::new("source bounds do not match the characters"));
    }
    let source_bounds = (0..count)
        .map(|_| reader.opt(|reader| Ok(CropBox { left: reader.finite_f32()?, bottom: reader.finite_f32()?, right: reader.finite_f32()?, top: reader.finite_f32()? })))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(InspectedGeometry { geometry, source_bounds })
}

fn mode_code(mode: DestinationMode) -> u8 {
    match mode {
        DestinationMode::Top => 0,
        DestinationMode::Xyz => 1,
        DestinationMode::Fit => 2,
        DestinationMode::FitH => 3,
        DestinationMode::FitV => 4,
        DestinationMode::FitR => 5,
        DestinationMode::FitB => 6,
        DestinationMode::FitBh => 7,
        DestinationMode::FitBv => 8,
    }
}

fn mode_from_code(code: u8) -> Result<DestinationMode, Malformed> {
    Ok(match code {
        0 => DestinationMode::Top,
        1 => DestinationMode::Xyz,
        2 => DestinationMode::Fit,
        3 => DestinationMode::FitH,
        4 => DestinationMode::FitV,
        5 => DestinationMode::FitR,
        6 => DestinationMode::FitB,
        7 => DestinationMode::FitBh,
        8 => DestinationMode::FitBv,
        other => return Err(Malformed::new(format!("invalid destination mode {other}"))),
    })
}

pub(crate) fn put_view(writer: &mut FrameWriter, view: DestinationView) {
    for value in [view.left, view.bottom, view.width, view.height] {
        writer.f32(value);
    }
    writer.u16(view.rotation);
}

/// A destination view from the helper: every coordinate finite.
pub(crate) fn get_view(reader: &mut WireReader<'_>) -> Result<DestinationView, Malformed> {
    Ok(DestinationView { left: reader.finite_f32()?, bottom: reader.finite_f32()?, width: reader.finite_f32()?, height: reader.finite_f32()?, rotation: reader.u16()? })
}

pub(crate) fn put_bookmarks(writer: &mut FrameWriter, bookmarks: &[EngineBookmark]) {
    writer.count(bookmarks.len());
    for bookmark in bookmarks {
        writer.opt(bookmark.title.as_deref(), FrameWriter::str);
        writer.u64(bookmark.depth as u64);
        writer.opt(bookmark.page_index, |writer, index| writer.u64(index as u64));
        writer.opt(bookmark.destination.as_ref(), |writer, destination| {
            writer.u8(mode_code(destination.mode));
            writer.opt(destination.x, FrameWriter::f32);
            writer.opt(destination.y, FrameWriter::f32);
            writer.opt(destination.rect, |writer, rect| rect.into_iter().for_each(|value| writer.f32(value)));
            writer.opt(destination.zoom, FrameWriter::f32);
            writer.opt(destination.view, put_view);
        });
    }
}

/// At most `limit` outline entries, each no deeper than the entries before it allow.
pub(crate) fn get_bookmarks(reader: &mut WireReader<'_>, limit: usize) -> Result<Vec<EngineBookmark>, Malformed> {
    let count = reader.count(limit, 11)?;
    let mut bookmarks = Vec::with_capacity(count);
    let mut title_bytes = 0_usize;
    for index in 0..count {
        let title = reader.opt(|reader| reader.str(BOOKMARK_TITLE_BUDGET + 1))?;
        title_bytes += title.as_ref().map_or(0, String::len);
        if title_bytes > BOOKMARK_TITLE_BUDGET + 1 {
            return Err(Malformed::new("bookmark titles exceed their budget"));
        }
        let depth = reader.usize()?;
        if depth > index {
            return Err(Malformed::new("bookmark deeper than its position allows"));
        }
        let page_index = reader.opt(WireReader::usize)?;
        let destination = reader.opt(|reader| {
            Ok(BookmarkDestination {
                mode: mode_from_code(reader.u8()?)?,
                x: reader.opt(WireReader::f32)?,
                y: reader.opt(WireReader::f32)?,
                rect: reader.opt(|reader| Ok([reader.f32()?, reader.f32()?, reader.f32()?, reader.f32()?]))?,
                zoom: reader.opt(WireReader::f32)?,
                view: reader.opt(get_view)?,
            })
        })?;
        bookmarks.push(EngineBookmark { title, depth, page_index, destination });
    }
    Ok(bookmarks)
}

pub(crate) fn put_properties_request(writer: &mut FrameWriter, source_size_bytes: usize, dimensions: &[(f32, f32)]) {
    writer.u64(source_size_bytes as u64);
    writer.count(dimensions.len());
    for &pair in dimensions {
        put_pair(writer, pair);
    }
}

pub(crate) fn get_properties_request(reader: &mut WireReader<'_>) -> Result<(usize, Vec<(f32, f32)>), Malformed> {
    let size = reader.usize()?;
    let count = reader.count(MAX_PAGES, 8)?;
    let dimensions = (0..count).map(|_| get_pair(reader)).collect::<Result<Vec<_>, _>>()?;
    Ok((size, dimensions))
}

const METADATA_NAMES: [&str; 8] = ["title", "author", "subject", "keywords", "creator", "producer", "creation_date_raw", "modification_date_raw"];

pub(crate) fn put_properties(writer: &mut FrameWriter, properties: &DocumentProperties) {
    writer.opt(properties.version.as_deref(), FrameWriter::str);
    writer.u64(properties.source_size_bytes as u64);
    writer.u64(properties.page_count as u64);
    writer.count(properties.metadata.len());
    for entry in &properties.metadata {
        writer.u8(METADATA_NAMES.iter().position(|name| *name == entry.name).unwrap_or(u8::MAX as usize) as u8);
        writer.str(&entry.value);
        writer.bool(entry.truncated);
    }
    writer.count(properties.page_dimensions.len());
    for group in &properties.page_dimensions {
        writer.f32(group.width_points);
        writer.f32(group.height_points);
        writer.u64(group.count as u64);
    }
    writer.bool(properties.page_dimensions_truncated);
    let security = &properties.security;
    writer.opt(security.encrypted, FrameWriter::bool);
    writer.opt(security.handler_revision, FrameWriter::u8);
    for flag in [security.print_high_quality, security.print_low_quality_only, security.modify_contents, security.assemble_document, security.fill_existing_forms] {
        writer.opt(flag, FrameWriter::bool);
    }
    writer.opt(properties.reported_signature_count, FrameWriter::u16);
}

/// Properties must echo the request's size and page count, with dimension groups that add up to it.
pub(crate) fn get_properties(reader: &mut WireReader<'_>, source_size_bytes: usize, page_count: usize) -> Result<DocumentProperties, Malformed> {
    let version = reader.opt(|reader| reader.str(MAX_VERSION_BYTES))?;
    if reader.usize()? != source_size_bytes || reader.usize()? != page_count {
        return Err(Malformed::new("properties for another source size or page count"));
    }
    let count = reader.count(MAX_METADATA_ENTRIES, 6)?;
    let mut metadata = Vec::with_capacity(count);
    for _ in 0..count {
        let name = *METADATA_NAMES.get(reader.u8()? as usize).ok_or_else(|| Malformed::new("unknown metadata name"))?;
        metadata.push(MetadataEntry { name, value: reader.str(MAX_METADATA_VALUE_BYTES)?, truncated: reader.bool()? });
    }
    let count = reader.count(MAX_DIMENSION_GROUPS, 16)?;
    let mut page_dimensions = Vec::with_capacity(count);
    let mut counted = 0_usize;
    for _ in 0..count {
        let group = PageDimensions { width_points: reader.finite_f32()?, height_points: reader.finite_f32()?, count: reader.usize()? };
        counted = counted.saturating_add(group.count);
        page_dimensions.push(group);
    }
    let page_dimensions_truncated = reader.bool()?;
    if counted > page_count || (!page_dimensions_truncated && counted != page_count) {
        return Err(Malformed::new("page dimension groups do not add up to the page count"));
    }
    let encrypted = reader.opt(WireReader::bool)?;
    let handler_revision = reader.opt(WireReader::u8)?;
    let mut flags = [None; 5];
    for flag in &mut flags {
        *flag = reader.opt(WireReader::bool)?;
    }
    let [print_high_quality, print_low_quality_only, modify_contents, assemble_document, fill_existing_forms] = flags;
    Ok(DocumentProperties {
        version,
        source_size_bytes,
        page_count,
        metadata,
        page_dimensions,
        page_dimensions_truncated,
        security: SecurityProperties { encrypted, handler_revision, print_high_quality, print_low_quality_only, modify_contents, assemble_document, fill_existing_forms },
        reported_signature_count: reader.opt(WireReader::u16)?,
        signature_validation: "not_performed",
    })
}

pub(crate) fn put_crop_rect(writer: &mut FrameWriter, rect: crate::service::CropRect) {
    for value in [rect.x, rect.y, rect.width, rect.height] {
        writer.f64(value);
    }
}

pub(crate) fn get_crop_rect(reader: &mut WireReader<'_>) -> Result<crate::service::CropRect, Malformed> {
    Ok(crate::service::CropRect { x: reader.f64()?, y: reader.f64()?, width: reader.f64()?, height: reader.f64()? })
}

pub(crate) fn put_crops(writer: &mut FrameWriter, rects: &[CropBox]) {
    writer.count(rects.len());
    for rect in rects {
        put_crop(writer, *rect);
    }
}

pub(crate) fn get_crops(reader: &mut WireReader<'_>) -> Result<Vec<CropBox>, Malformed> {
    let count = reader.count(crate::comments::MAX_GEOMETRY_SLOTS, 16)?;
    (0..count).map(|_| get_crop(reader)).collect()
}

pub(crate) fn put_display_rects(writer: &mut FrameWriter, rects: &[Option<DisplayRect>]) {
    writer.count(rects.len());
    for rect in rects {
        writer.opt(rect.as_ref(), |writer, rect| {
            for value in [rect.x, rect.y, rect.width, rect.height] {
                writer.f64(value);
            }
        });
    }
}

/// One entry per requested rectangle, each inside the unit square.
pub(crate) fn get_display_rects(reader: &mut WireReader<'_>, expected: usize) -> Result<Vec<Option<DisplayRect>>, Malformed> {
    let count = reader.count(expected, 1)?;
    if count != expected {
        return Err(Malformed::new("display rectangles do not match the request"));
    }
    (0..count)
        .map(|_| {
            reader.opt(|reader| {
                let rect = DisplayRect { x: reader.finite_f64()?, y: reader.finite_f64()?, width: reader.finite_f64()?, height: reader.finite_f64()? };
                let inside = rect.x >= 0.0 && rect.y >= 0.0 && rect.width > 0.0 && rect.height > 0.0 && rect.x + rect.width <= 1.0 + 1e-9 && rect.y + rect.height <= 1.0 + 1e-9;
                if inside { Ok(rect) } else { Err(Malformed::new("display rectangle outside the page")) }
            })
        })
        .collect()
}

pub(crate) fn put_form_values(writer: &mut FrameWriter, values: Option<&HashMap<String, Option<String>>>) {
    writer.opt(values, |writer, values| {
        let mut entries = values.iter().collect::<Vec<_>>();
        entries.sort();
        writer.count(entries.len());
        for (name, value) in entries {
            writer.str(name);
            writer.opt(value.as_deref(), FrameWriter::str);
        }
    });
}

/// The engine's form values: only names the request asked about, each at most once.
pub(crate) fn get_form_values(reader: &mut WireReader<'_>, fields: &[FormField]) -> Result<Option<HashMap<String, Option<String>>>, Malformed> {
    reader.opt(|reader| {
        let count = reader.count(fields.len(), 5)?;
        let mut values = HashMap::with_capacity(count);
        for _ in 0..count {
            let name = reader.str(MAX_FIELD_TEXT_BYTES)?;
            if !fields.iter().any(|field| field.name == name) {
                return Err(Malformed::new("a form value for a field that was not asked about"));
            }
            let value = reader.opt(|reader| reader.str(MAX_FIELD_TEXT_BYTES))?;
            if values.insert(name, value).is_some() {
                return Err(Malformed::new("a form field reported twice"));
            }
        }
        Ok(values)
    })
}

#[cfg(test)]
pub(crate) fn put_labels(writer: &mut FrameWriter, labels: &[Option<String>]) {
    writer.count(labels.len());
    for label in labels {
        writer.opt(label.as_deref(), FrameWriter::str);
    }
}

#[cfg(test)]
pub(crate) fn get_labels(reader: &mut WireReader<'_>, page_count: usize) -> Result<Vec<Option<String>>, Malformed> {
    let count = reader.count(page_count, 1)?;
    (0..count).map(|_| reader.opt(|reader| reader.str(MAX_LABEL_BYTES))).collect()
}
