//! The app's annotation model (SM3 P2-7a, split out of `comments.rs` with no behaviour change): kinds, limits,
//! IDs, dates, validation and the geometry and text helpers the reader and the writer share.
use crate::editor::{CropBox, PageSpec};
use lopdf::{Dictionary, Document, Object, ObjectId};
use serde::Serialize;
use std::collections::HashSet;

pub const MAX_NOTES: usize = 1_000;
pub const MAX_TEXT_BYTES: usize = 8 * 1024;
pub const MAX_TOTAL_BYTES: usize = 1024 * 1024;
pub const MAX_HIGHLIGHT_QUADS: usize = 256;
pub const MAX_GEOMETRY_SLOTS: usize = 4_096;
pub(crate) const PREFIX: &str = "pdf-workstation-note-";
pub(crate) const HIGHLIGHT_PREFIX: &str = "pdf-workstation-highlight-";

#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum AnnotationKind { Note, Highlight }

/// One of the app's own annotations. `created` and `modified` are PDF dates in the app's one form,
/// `D:YYYYMMDDHHmmSSZ` (UTC, so no time-zone offset is recorded). They are written as `/CreationDate` and
/// `/M`, as Acrobat does. Notes from 0.3.x and slice 1c have none: an edit sets `modified` and leaves
/// `created` unknown.
#[derive(Clone, Debug, PartialEq)]
pub struct Note { pub id: String, pub rect: CropBox, pub contents: String, pub kind: AnnotationKind, pub quads: Option<Vec<CropBox>>, pub created: Option<String>, pub modified: Option<String> }

/// Acrobat's subject for each kind, written as `/Subj` (Acrobat shows it as the comment type).
pub fn subject(kind: AnnotationKind) -> &'static str { match kind { AnnotationKind::Note => "Sticky Note", AnnotationKind::Highlight => "Highlight" } }

/// Whether `value` is a PDF date in the app's form `D:YYYYMMDDHHmmSSZ`, a real calendar date and time. The one
/// implementation lives in the recovery journal, which the standalone journal test also compiles.
pub fn valid_pdf_date(value: &str) -> bool { crate::recovery_journal::valid_annotation_date(value) }

/// Seconds since the Unix epoch as a PDF date in UTC (`D:YYYYMMDDHHmmSSZ`).
pub fn pdf_date(epoch_seconds: u64) -> String {
    let (days, seconds) = (epoch_seconds / 86_400, epoch_seconds % 86_400);
    // Civil date from days since 1970-01-01 (Howard Hinnant's algorithm, proleptic Gregorian).
    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let day_of_era = z.rem_euclid(146_097);
    let year_of_era = (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_index + 2) / 5 + 1;
    let month = if month_index < 10 { month_index + 3 } else { month_index - 9 };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    format!("D:{year:04}{month:02}{day:02}{:02}{:02}{:02}Z", seconds / 3_600, seconds % 3_600 / 60, seconds % 60)
}

/// Now, as a PDF date in UTC.
pub fn pdf_date_now() -> String { pdf_date(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |elapsed| elapsed.as_secs())) }
#[derive(Clone, Debug, Serialize)]
pub struct DisplayRect { pub x: f64, pub y: f64, pub width: f64, pub height: f64 }
#[derive(Serialize)]
pub struct CommentInfo { pub id: String, pub page: usize, pub rect: Option<DisplayRect>, pub contents: String }
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CommentList { pub document_id: u64, pub revision: u64, pub status: &'static str, pub reason: Option<String>, pub notes: Vec<CommentInfo> }
#[derive(Serialize)]
pub struct AnnotationInfo { pub id: String, pub kind: AnnotationKind, pub page: usize, pub rect: Option<DisplayRect>, pub contents: Option<String>, pub quads: Option<Vec<DisplayRect>> }
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AnnotationList { pub document_id: u64, pub revision: u64, pub status: &'static str, pub reason: Option<String>, pub annotations: Vec<AnnotationInfo>, pub view_only: Vec<ViewOnlyInfo>, pub view_only_truncated: usize }
/// An annotation the app shows and preserves but does not edit (another app's, or a link-free
/// annotation the strict format does not cover).
#[derive(Serialize)]
pub struct ViewOnlyInfo { pub page: usize, #[serde(flatten)] pub annotation: crate::annotations::Surveyed }

pub fn highlight_contents(contents: Option<&str>) -> Result<String, String> {
    let contents = contents.unwrap_or("");
    if contents.contains('\0') || contents.len() > MAX_TEXT_BYTES { return Err("Highlight text must be NUL-free and limited to 8 KiB of UTF-8.".into()); }
    Ok(if contents.trim().is_empty() { String::new() } else { contents.to_owned() })
}

pub fn validate_text(contents: &str) -> Result<(), String> {
    if contents.trim().is_empty() { return Err("Enter some text for the note.".into()); }
    if contents.contains('\0') { return Err("Notes cannot contain a null character.".into()); }
    if contents.len() > MAX_TEXT_BYTES { return Err("Each note is limited to 8 KiB of UTF-8 text.".into()); }
    Ok(())
}
pub fn validate_plan(plan: &[PageSpec]) -> Result<(), String> {
    let mut count = 0usize; let mut total = 0usize; let mut slots = 0usize; let mut ids = HashSet::new();
    for note in plan.iter().flat_map(|page| &page.notes) {
        if [&note.created, &note.modified].into_iter().flatten().any(|date| !valid_pdf_date(date)) { return Err("The annotation has an invalid date.".into()); }
        if let Some(quads) = &note.quads {
            if note.kind != AnnotationKind::Highlight || highlight_union(quads)? != note.rect { return Err("The annotation has invalid text-highlight quadrilaterals.".into()); }
        }
        match note.kind { AnnotationKind::Note => validate_text(&note.contents)?, AnnotationKind::Highlight => { if highlight_contents(Some(&note.contents))? != note.contents { return Err("The highlight has a noncanonical empty body.".into()); } } }
        id_matches_kind(&note.id, note.kind)?;
        if !ids.insert(&note.id) { return Err("The PDF contains duplicate note IDs. Commenting is unavailable.".into()); }
        count += 1; total += note.contents.len();
        slots += note.quads.as_ref().map_or(1, Vec::len);
    }
    if count > MAX_NOTES { return Err("This document is limited to 1,000 notes and highlights in total.".into()); }
    if total > MAX_TOTAL_BYTES { return Err("This document is limited to 1 MiB of total annotation text.".into()); }
    if slots > MAX_GEOMETRY_SLOTS { return Err("This document is limited to 4,096 annotation geometry slots; each text-highlight character uses one slot and each note or area highlight uses one.".into()); }
    Ok(())
}
pub fn number(id: &str) -> Result<u64, String> {
    let suffix = id.strip_prefix(PREFIX).or_else(|| id.strip_prefix(HIGHLIGHT_PREFIX)).ok_or("The PDF has a foreign annotation. Commenting is unavailable.")?;
    let value = suffix.parse::<u64>().map_err(|_| "The PDF has an invalid note ID.")?;
    if value == 0 || suffix != value.to_string() { return Err("The PDF has an invalid note ID.".into()); }
    Ok(value)
}
/// The numbered IDs of 0.3.x and slice 1c; the app no longer creates them, but tests build legacy files with them.
#[cfg(test)]
pub fn id(value: u64) -> String { format!("{PREFIX}{value}") }
#[cfg(test)]
pub fn highlight_id(value: u64) -> String { format!("{HIGHLIGHT_PREFIX}{value}") }

/// Whether `id` is a random UUID (version 4, lowercase, dashed), the `/NM` form the app writes since P2-4
/// part two, as Acrobat does.
/// The one implementation lives in the recovery journal, which the standalone journal test also compiles.
pub fn is_uuid(id: &str) -> bool { crate::recovery_journal::annotation_uuid(id) }

/// An app annotation ID: a random UUID (new) or the numbered `pdf-workstation-note-N` /
/// `pdf-workstation-highlight-N` form of 0.3.x and slice 1c, whose prefix must match the kind.
pub fn id_matches_kind(id: &str, kind: AnnotationKind) -> Result<(), String> {
    if is_uuid(id) { return Ok(()); }
    number(id)?;
    if id.starts_with(PREFIX) != (kind == AnnotationKind::Note) { return Err("The annotation ID does not match its kind.".into()); }
    Ok(())
}

/// A new random UUID (version 4) for a new annotation's `/NM`, from the operating system's CSPRNG. Never
/// derived from the machine, the user or the time (SM7 E3). A generator failure is an error, never a panic:
/// the PDF worker serves every open document and has no unwind guard (SM7 L-1 on 684b33e).
pub fn new_id() -> Result<String, String> {
    let mut random = [0u8; 16];
    #[cfg(test)]
    if FAIL_RANDOM.with(std::cell::Cell::get) { return Err(RANDOM_FAILED.into()); }
    fill_random(&mut random)?;
    random[6] = (random[6] & 0x0f) | 0x40; random[8] = (random[8] & 0x3f) | 0x80;
    let hex = random.iter().map(|byte| format!("{byte:02x}")).collect::<String>();
    Ok(format!("{}-{}-{}-{}-{}", &hex[0..8], &hex[8..12], &hex[12..16], &hex[16..20], &hex[20..32]))
}
#[cfg(test)]
thread_local! { pub(crate) static FAIL_RANDOM: std::cell::Cell<bool> = const { std::cell::Cell::new(false) }; }
pub(crate) const RANDOM_FAILED: &str = "The system random number generator failed, so no annotation ID could be made. Nothing was changed.";
#[cfg(windows)]
pub(crate) fn fill_random(buffer: &mut [u8]) -> Result<(), String> {
    use windows::Win32::Security::Cryptography::{BCryptGenRandom, BCRYPT_USE_SYSTEM_PREFERRED_RNG};
    let status = unsafe { BCryptGenRandom(None, buffer, BCRYPT_USE_SYSTEM_PREFERRED_RNG) };
    if status.is_ok() { Ok(()) } else { Err(RANDOM_FAILED.into()) }
}
#[cfg(not(windows))]
pub(crate) fn fill_random(buffer: &mut [u8]) -> Result<(), String> {
    use std::io::Read;
    std::fs::File::open("/dev/urandom").and_then(|mut file| file.read_exact(buffer)).map_err(|_| RANDOM_FAILED.to_owned())
}
/// QuadPoints in the order Acrobat writes and PDFium's appearance generator reads: top-left, top-right,
/// bottom-left, bottom-right (measured on 204 real markup quads, 0 in any other order).
pub(crate) fn quad(bounds: CropBox) -> Vec<f32> { vec![bounds.left, bounds.top, bounds.right, bounds.top, bounds.left, bounds.bottom, bounds.right, bounds.bottom] }
/// The counter-clockwise order (bottom-left, bottom-right, top-right, top-left) this app wrote through 0.3.x.
/// PDFium draws nothing for it once `/AP` is gone, so it is read but never written.
pub(crate) fn legacy_quad(bounds: CropBox) -> Vec<f32> { vec![bounds.left, bounds.bottom, bounds.right, bounds.bottom, bounds.right, bounds.top, bounds.left, bounds.top] }
/// The axis-aligned box of one quad, if its 8 numbers are that box in either order the app accepts.
pub(crate) fn quad_box(points: &[f32]) -> Option<CropBox> {
    let xs = [points[0], points[2], points[4], points[6]]; let ys = [points[1], points[3], points[5], points[7]];
    let rect = CropBox { left: xs.iter().copied().fold(f32::INFINITY, f32::min), bottom: ys.iter().copied().fold(f32::INFINITY, f32::min), right: xs.iter().copied().fold(f32::NEG_INFINITY, f32::max), top: ys.iter().copied().fold(f32::NEG_INFINITY, f32::max) };
    (points == quad(rect).as_slice() || points == legacy_quad(rect).as_slice()).then_some(rect)
}
pub fn highlight_union(quads: &[CropBox]) -> Result<CropBox, String> {
    if quads.is_empty() || quads.len() > MAX_HIGHLIGHT_QUADS { return Err("A text highlight needs 1 through 256 positioned characters.".into()); }
    let mut bounds = quads[0];
    for rect in quads {
        if ![rect.left, rect.bottom, rect.right, rect.top].iter().all(|value| value.is_finite()) || !((rect.right - rect.left).is_finite() && (rect.top - rect.bottom).is_finite()) || rect.right <= rect.left || rect.top <= rect.bottom { return Err("The text highlight has invalid bounds.".into()); }
        bounds.left = bounds.left.min(rect.left); bounds.bottom = bounds.bottom.min(rect.bottom); bounds.right = bounds.right.max(rect.right); bounds.top = bounds.top.max(rect.top);
    }
    if !(bounds.right - bounds.left).is_finite() || !(bounds.top - bounds.bottom).is_finite() { return Err("The text highlight has invalid total bounds.".into()); }
    Ok(bounds)
}
pub(crate) fn read_quads(value: &Object, bounds: CropBox) -> Result<Vec<CropBox>, String> {
    let count = value.as_array().map_err(|error| error.to_string())?.len();
    if count == 0 || count % 8 != 0 || count > MAX_HIGHLIGHT_QUADS * 8 { return Err("The PDF has invalid highlight quadrilaterals.".into()); }
    let values = values(value)?;
    if values.is_empty() || values.len() % 8 != 0 || values.len() > MAX_HIGHLIGHT_QUADS * 8 { return Err("The PDF has invalid highlight quadrilaterals.".into()); }
    let quads = values.chunks_exact(8).map(|points| quad_box(points).ok_or_else(|| "The PDF has altered highlight quadrilaterals.".to_owned())).collect::<Result<Vec<_>, String>>()?;
    if highlight_union(&quads)? != bounds { return Err("The highlight bounds do not match its quadrilaterals.".into()); }
    Ok(quads)
}
pub(crate) fn keys(value: &Dictionary, allowed: &[&[u8]]) -> Result<(), String> {
    if value.iter().any(|(key, _)| !allowed.contains(&key.as_slice())) { return Err("The PDF has an unknown annotation or appearance feature. Commenting is unavailable.".into()); }
    Ok(())
}
pub(crate) fn name(value: &Dictionary, key: &[u8], expected: &[u8]) -> Result<(), String> {
    if value.get(key).and_then(Object::as_name).ok() != Some(expected) { return Err("The PDF has an unsupported annotation schema. Commenting is unavailable.".into()); }
    Ok(())
}
pub(crate) fn values(value: &Object) -> Result<Vec<f32>, String> {
    value.as_array().map_err(|error| error.to_string())?.iter().map(|value| value.as_float().map_err(|error| error.to_string())).collect()
}
pub(crate) fn rect(value: &Object) -> Result<CropBox, String> {
    let value = values(value)?;
    if value.len() != 4 || !value.iter().all(|value| value.is_finite()) || !(value[2] - value[0]).is_finite() || !(value[3] - value[1]).is_finite() || value[2] - value[0] < 1.0 || value[3] - value[1] < 1.0 { return Err("The PDF has invalid note bounds. Commenting is unavailable.".into()); }
    Ok(CropBox { left: value[0], bottom: value[1], right: value[2], top: value[3] })
}
pub(crate) fn highlight_rect(value: &Object) -> Result<CropBox, String> {
    let value = values(value)?;
    if value.len() != 4 { return Err("The PDF has invalid highlight bounds.".into()); }
    let bounds = CropBox { left: value[0], bottom: value[1], right: value[2], top: value[3] }; highlight_union(&[bounds])?; Ok(bounds)
}
pub(crate) fn media_box(document: &Document, mut page: ObjectId) -> Result<CropBox, String> {
    let mut visited = HashSet::new();
    loop {
        if !visited.insert(page) { return Err("The PDF has a cyclic page boundary inheritance.".into()); }
        let dictionary = document.get_dictionary(page).map_err(|error| error.to_string())?;
        if let Ok(value) = dictionary.get(b"MediaBox") {
            let value = document.dereference(value).map_err(|error| error.to_string())?.1.as_array().map_err(|error| error.to_string())?;
            let values = value.iter().map(|value| document.dereference(value).map(|(_, value)| value.clone()).map_err(|error| error.to_string())).collect::<Result<Vec<_>, _>>()?;
            return rect(&Object::Array(values));
        }
        page = dictionary.get(b"Parent").and_then(Object::as_reference).map_err(|_| "The note page has no supported MediaBox.")?;
    }
}
pub(crate) fn decode_text(value: &Object) -> Result<String, String> {
    let bytes = value.as_str().map_err(|error| error.to_string())?;
    if bytes.len() > MAX_TEXT_BYTES * 2 + 2 { return Err("Each note is limited to 8 KiB of UTF-8 text.".into()); }
    if bytes.len() < 2 || bytes[..2] != [0xfe, 0xff] || bytes.len() % 2 != 0 { return Err("The PDF has unsupported note text encoding.".into()); }
    let units = bytes[2..].chunks_exact(2).map(|pair| u16::from_be_bytes([pair[0], pair[1]])).collect::<Vec<_>>();
    String::from_utf16(&units).map_err(|_| "The PDF has invalid Unicode note text.".into())
}
pub(crate) fn effective_version(document: &Document) -> Result<(u8, u8), String> {
    let parse = |value: &str| match value {
        "1.0" => Ok((1, 0)), "1.1" => Ok((1, 1)), "1.2" => Ok((1, 2)), "1.3" => Ok((1, 3)),
        "1.4" => Ok((1, 4)), "1.5" => Ok((1, 5)), "1.6" => Ok((1, 6)), "1.7" => Ok((1, 7)), "2.0" => Ok((2, 0)),
        _ => Err("Commenting on this PDF version is not supported in this build.".to_owned()),
    };
    let header = parse(&document.version)?;
    let catalog = document.catalog().map_err(|error| error.to_string())?;
    let declared = catalog.get(b"Version").ok().map(|value| {
        let value = document.dereference(value).map_err(|error| error.to_string())?.1.as_name().map_err(|error| error.to_string())?;
        parse(std::str::from_utf8(value).map_err(|error| error.to_string())?)
    }).transpose()?;
    Ok(declared.map_or(header, |declared| declared.max(header)))
}

/// Ink limits (P2-7g; design 5.2): paths and points per annotation, points per document, the largest
/// coordinate magnitude, the stroke width range and the largest generated appearance (SM7 ruling condition 1).
pub const MAX_INK_STROKES: usize = 1_024;
pub const MAX_INK_POINTS: usize = 131_072;
#[allow(dead_code, reason = "used by the pen tool in P2-7b")]
pub const MAX_INK_DOCUMENT_POINTS: usize = 4 * 1024 * 1024;
pub const MAX_INK_COORDINATE: f32 = 1.0e7;
pub const MAX_INK_WIDTH: f32 = 100.0;
#[allow(dead_code, reason = "used by the pen tool in P2-7b")]
pub const MIN_APP_INK_WIDTH: f32 = 0.1;
pub const MAX_INK_APPEARANCE_BYTES: usize = 4 * 1024 * 1024;

/// A freehand drawing the app makes (`/Subtype /Ink`): strokes of page-space points, one width, one RGB colour
/// and one opacity. P2-7g groundwork: not part of `Note` or the editing plan, so nothing journalled changes.
#[allow(dead_code, reason = "used by the pen tool in P2-7b")]
#[derive(Clone, Debug, PartialEq)]
pub struct Ink { pub strokes: Vec<Vec<(f32, f32)>>, pub width: f32, pub color: [f32; 3], pub opacity: f32 }

/// Checks a stroke list against the limits: 1 to 1,024 strokes, each with at least one point, at most 131,072
/// points, every coordinate finite and within 1e7.
pub fn validate_strokes(strokes: &[Vec<(f32, f32)>]) -> Result<(), String> {
    if strokes.is_empty() || strokes.len() > MAX_INK_STROKES { return Err("A drawing needs 1 through 1,024 strokes.".into()); }
    let mut points = 0usize;
    for stroke in strokes {
        if stroke.is_empty() { return Err("A drawing stroke has no points.".into()); }
        points += stroke.len();
        if points > MAX_INK_POINTS { return Err("A drawing is limited to 131,072 points.".into()); }
        if stroke.iter().any(|&(x, y)| !(x.is_finite() && y.is_finite() && x.abs() <= MAX_INK_COORDINATE && y.abs() <= MAX_INK_COORDINATE)) { return Err("A drawing has an invalid point.".into()); }
    }
    Ok(())
}

/// Checks an app drawing: its strokes, a width from 0.1 to 100 points, colour components and an opacity in 0 to 1
/// (opacity above 0).
#[allow(dead_code, reason = "used by the pen tool in P2-7b")]
pub fn validate_ink(ink: &Ink) -> Result<(), String> {
    validate_strokes(&ink.strokes)?;
    if !(MIN_APP_INK_WIDTH..=MAX_INK_WIDTH).contains(&ink.width) { return Err("The drawing width is out of range.".into()); }
    if ink.color.iter().any(|value| !(0.0..=1.0).contains(value)) || !(ink.opacity > 0.0 && ink.opacity <= 1.0) { return Err("The drawing colour or opacity is out of range.".into()); }
    Ok(())
}

/// The box around every point, grown by half the width (at least half a point) on each side so round caps and
/// joins fit. The strokes must have passed `validate_strokes`.
pub fn ink_bounds(strokes: &[Vec<(f32, f32)>], width: f32) -> CropBox {
    let (mut left, mut bottom, mut right, mut top) = (f32::INFINITY, f32::INFINITY, f32::NEG_INFINITY, f32::NEG_INFINITY);
    for &(x, y) in strokes.iter().flatten() { left = left.min(x); bottom = bottom.min(y); right = right.max(x); top = top.max(y); }
    let grow = (width / 2.0).max(0.5);
    CropBox { left: left - grow, bottom: bottom - grow, right: right + grow, top: top + grow }
}

/// Reads an `/InkList` (an array of arrays of numbers, x and y alternating, direct or by reference) and checks it
/// with `validate_strokes`. Used on other apps' Ink, so every malformed shape is an error, never a panic.
pub fn read_ink_list(document: &Document, value: &Object) -> Result<Vec<Vec<(f32, f32)>>, String> {
    let resolve = |value: &Object| document.dereference(value).map(|(_, value)| value.clone()).map_err(|_| "The drawing has an unreadable stroke list.".to_owned());
    let list = resolve(value)?;
    let list = list.as_array().map_err(|_| "The drawing stroke list is not an array.".to_owned())?;
    if list.is_empty() || list.len() > MAX_INK_STROKES { return Err("A drawing needs 1 through 1,024 strokes.".into()); }
    let mut strokes = Vec::with_capacity(list.len());
    let mut total = 0usize;
    for stroke in list {
        let stroke = resolve(stroke)?;
        let numbers = stroke.as_array().map_err(|_| "A drawing stroke is not an array.".to_owned())?;
        if numbers.is_empty() || numbers.len() % 2 != 0 { return Err("A drawing stroke has an odd or empty coordinate list.".into()); }
        total += numbers.len() / 2;
        if total > MAX_INK_POINTS { return Err("A drawing is limited to 131,072 points.".into()); }
        let values = numbers.iter().map(|value| value.as_float().map_err(|_| "A drawing stroke has a non-number.".to_owned())).collect::<Result<Vec<f32>, String>>()?;
        strokes.push(values.chunks_exact(2).map(|pair| (pair[0], pair[1])).collect::<Vec<_>>());
    }
    validate_strokes(&strokes)?;
    Ok(strokes)
}
