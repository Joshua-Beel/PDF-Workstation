use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashSet;

const MAGIC: &[u8; 8] = b"SMACREC\0";
/// The newest record version this build reads. It writes 3 only for records whose annotations carry dates or
/// a UUID ID (`record_version`), and 2 otherwise.
const VERSION: u16 = 3;
const HEADER_BYTES: usize = 78;
const DIGEST_BYTES: usize = 32;
const MAX_FRAME_BYTES: usize = 32 * 1024 * 1024;
const MAX_PAGES: usize = 65_536;
const MAX_ANNOTATIONS: usize = 1_000;
const MAX_TEXT_BYTES: usize = 8 * 1024;
const MAX_TOTAL_TEXT_BYTES: usize = 1024 * 1024;
const MAX_QUADS: usize = 256;
const MAX_TOTAL_QUADS: usize = 4_096;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct SourceIdentity { pub bytes: u64, pub sha256: [u8; 32] }

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RecoveryRect { pub left: f32, pub bottom: f32, pub right: f32, pub top: f32 }

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RecoveryAnnotationKind { Note, AreaHighlight, TextHighlight }

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RecoveryAnnotation {
    pub id: String,
    pub kind: RecoveryAnnotationKind,
    pub rect: RecoveryRect,
    pub contents: String,
    pub quads: Vec<RecoveryRect>,
    /// `/CreationDate` and `/M` of the app's annotation (SM3 P2-4), in the app's PDF date form. Absent in
    /// version 1 and 2 records and for notes that never had them; omitted when absent, so those records
    /// stay byte-identical (the canonical-encoding check).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub modified: Option<String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RecoveryPage {
    pub source: u32,
    pub turns: u8,
    pub crop: Option<RecoveryRect>,
    pub annotations: Vec<RecoveryAnnotation>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RecoveryEditState { pub pages: Vec<RecoveryPage> }

#[derive(Clone, Debug, PartialEq)]
pub struct RecoveryRecord {
    pub active: bool,
    pub generation: u64,
    pub revision: u64,
    pub current_page: u32,
    pub source_pages: u32,
    pub source: SourceIdentity,
    pub edit_state: RecoveryEditState,
}

/// The annotation date form `D:YYYYMMDDHHmmSSZ` (SM3), checked as a real calendar date: the day must exist in
/// that month (leap years included), hours 0-23, minutes and seconds 0-59. This is the only implementation;
/// `comments::valid_pdf_date` calls it. It lives here because this file is also compiled alone by
/// `tests/recovery_journal.rs`.
pub fn valid_annotation_date(value: &str) -> bool {
    let Some(digits) = value.strip_prefix("D:").and_then(|rest| rest.strip_suffix('Z')) else { return false };
    if digits.len() != 14 || !digits.bytes().all(|byte| byte.is_ascii_digit()) { return false; }
    let field = |range: std::ops::Range<usize>| digits[range].parse::<u32>().unwrap_or(u32::MAX);
    let (year, month, day) = (field(0..4), field(4..6), field(6..8));
    let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    let days = match month { 1 | 3 | 5 | 7 | 8 | 10 | 12 => 31, 4 | 6 | 9 | 11 => 30, 2 if leap => 29, 2 => 28, _ => return false };
    (1..=9999).contains(&year) && (1..=days).contains(&day) && field(8..10) <= 23 && field(10..12) <= 59 && field(12..14) <= 59
}

/// Version 3 marks a record that 0.3.x cannot read: one whose annotations carry dates or a UUID ID. Any other
/// record is written as version 2, byte-identical to what 0.3.x writes.
fn record_version(state: &RecoveryEditState) -> u16 {
    // Version 3 also when any ID is a UUID: 0.3.x reads only numbered IDs (SM7 M-1 on 684b33e).
    if state.pages.iter().flat_map(|page| &page.annotations).any(|annotation| annotation.created.is_some() || annotation.modified.is_some() || annotation_uuid(&annotation.id)) { 3 } else { 2 }
}

fn positive_rect(rect: RecoveryRect) -> bool {
    [rect.left, rect.bottom, rect.right, rect.top].iter().all(|value| value.is_finite())
        && (rect.right - rect.left).is_finite() && (rect.top - rect.bottom).is_finite()
        && rect.right > rect.left && rect.top > rect.bottom
}

fn valid_crop(rect: RecoveryRect) -> bool {
    positive_rect(rect) && rect.right - rect.left >= 1.0 && rect.top - rect.bottom >= 1.0
}

/// An annotation ID of the given kind: a random UUID v4 (lowercase, dashed; SM3 P2-4) for any kind, or the
/// numbered form with the kind's prefix (the journal's form of `comments::id_matches_kind`). `annotation_uuid`
/// is the only UUID check; `comments::is_uuid` calls it, since this file is also compiled alone by
/// `tests/recovery_journal.rs`.
fn annotation_number(id: &str, prefix: &str) -> bool {
    annotation_uuid(id) || id.strip_prefix(prefix).and_then(|suffix| suffix.parse::<u64>().ok().map(|value| (suffix, value)))
        .is_some_and(|(suffix, value)| value > 0 && suffix == value.to_string())
}
pub fn annotation_uuid(id: &str) -> bool {
    let bytes = id.as_bytes();
    bytes.len() == 36 && bytes.iter().enumerate().all(|(index, byte)| match index { 8 | 13 | 18 | 23 => *byte == b'-', 14 => *byte == b'4', 19 => matches!(byte, b'8' | b'9' | b'a' | b'b'), _ => matches!(byte, b'0'..=b'9' | b'a'..=b'f') })
}

fn union(quads: &[RecoveryRect]) -> Option<RecoveryRect> {
    let mut bounds = *quads.first()?;
    for rect in quads {
        if !positive_rect(*rect) { return None; }
        bounds.left = bounds.left.min(rect.left); bounds.bottom = bounds.bottom.min(rect.bottom);
        bounds.right = bounds.right.max(rect.right); bounds.top = bounds.top.max(rect.top);
    }
    positive_rect(bounds).then_some(bounds)
}

fn validate(record: &RecoveryRecord) -> Result<(), String> {
    if record.generation == 0 || record.source.bytes == 0 { return Err("Recovery identity is invalid.".into()); }
    let source_pages = usize::try_from(record.source_pages).map_err(|_| "Recovery page count is invalid.")?;
    if !record.active {
        if source_pages == 0 || !record.edit_state.pages.is_empty() || record.current_page != 0 { return Err("Recovery tombstone is invalid.".into()); }
        return Ok(());
    }
    if record.revision == 0 { return Err("Recovery identity is invalid.".into()); }
    if source_pages == 0 || source_pages > MAX_PAGES || record.edit_state.pages.is_empty() || record.edit_state.pages.len() > source_pages || record.current_page as usize >= record.edit_state.pages.len() { return Err("Recovery page count is invalid.".into()); }
    let mut sources = HashSet::new();
    let mut annotations = 0usize;
    let mut text_bytes = 0usize;
    let mut geometry_slots = 0usize;
    let mut ids = HashSet::new();
    for page in &record.edit_state.pages {
        if page.source as usize >= source_pages || !sources.insert(page.source) || page.turns > 3 || page.crop.is_some_and(|rect| !valid_crop(rect)) { return Err("Recovery page state is invalid.".into()); }
        annotations = annotations.checked_add(page.annotations.len()).ok_or("Recovery annotation count is invalid.")?;
        if annotations > MAX_ANNOTATIONS { return Err("Recovery annotation count is invalid.".into()); }
        for annotation in &page.annotations {
            if [&annotation.created, &annotation.modified].into_iter().flatten().any(|date| !valid_annotation_date(date)) { return Err("Recovery annotation state is invalid.".into()); }
            if !ids.insert(&annotation.id) || !positive_rect(annotation.rect) || annotation.contents.contains('\0') || annotation.contents.len() > MAX_TEXT_BYTES { return Err("Recovery annotation state is invalid.".into()); }
            text_bytes = text_bytes.checked_add(annotation.contents.len()).ok_or("Recovery annotation text is invalid.")?;
            if text_bytes > MAX_TOTAL_TEXT_BYTES { return Err("Recovery annotation text is invalid.".into()); }
            let slots = match annotation.kind {
                RecoveryAnnotationKind::Note if annotation_number(&annotation.id, "pdf-workstation-note-") && annotation.quads.is_empty() && !annotation.contents.trim().is_empty() => 1,
                RecoveryAnnotationKind::AreaHighlight if annotation_number(&annotation.id, "pdf-workstation-highlight-") && annotation.quads.is_empty() && (annotation.contents.is_empty() || !annotation.contents.trim().is_empty()) => 1,
                RecoveryAnnotationKind::TextHighlight if annotation_number(&annotation.id, "pdf-workstation-highlight-") && !annotation.quads.is_empty() && annotation.quads.len() <= MAX_QUADS && (annotation.contents.is_empty() || !annotation.contents.trim().is_empty()) && union(&annotation.quads) == Some(annotation.rect) => annotation.quads.len(),
                _ => return Err("Recovery annotation state is invalid.".into()),
            };
            geometry_slots = geometry_slots.checked_add(slots).ok_or("Recovery annotation geometry is invalid.")?;
            if geometry_slots > MAX_TOTAL_QUADS { return Err("Recovery annotation geometry is invalid.".into()); }
        }
    }
    Ok(())
}

pub fn encode_recovery(record: &RecoveryRecord) -> Result<Vec<u8>, String> {
    validate(record)?;
    let payload = serde_json::to_vec(&record.edit_state).map_err(|_| "Recovery state could not be encoded.")?;
    let total = HEADER_BYTES.checked_add(payload.len()).and_then(|value| value.checked_add(DIGEST_BYTES)).ok_or("Recovery record is too large.")?;
    if total > MAX_FRAME_BYTES || payload.len() > u32::MAX as usize { return Err("Recovery record is too large.".into()); }
    let mut output = Vec::with_capacity(total);
    output.extend_from_slice(MAGIC);
    output.extend_from_slice(&record_version(&record.edit_state).to_le_bytes());
    output.extend_from_slice(&record.generation.to_le_bytes());
    output.extend_from_slice(&record.revision.to_le_bytes());
    output.extend_from_slice(&record.current_page.to_le_bytes());
    output.extend_from_slice(&record.source_pages.to_le_bytes());
    output.extend_from_slice(&record.source.bytes.to_le_bytes());
    output.extend_from_slice(&record.source.sha256);
    output.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    output.extend_from_slice(&payload);
    output.extend_from_slice(&Sha256::digest(&output));
    Ok(output)
}

fn take<const N: usize>(bytes: &[u8], offset: &mut usize) -> Result<[u8; N], String> {
    let end = offset.checked_add(N).ok_or("Recovery record is truncated.")?;
    let value: [u8; N] = bytes.get(*offset..end).ok_or("Recovery record is truncated.")?.try_into().map_err(|_| "Recovery record is truncated.")?;
    *offset = end;
    Ok(value)
}

pub fn decode_recovery(bytes: &[u8], expected_source: SourceIdentity, prior_generation: Option<u64>, minimum_revision: u64) -> Result<RecoveryRecord, String> {
    if bytes.len() < HEADER_BYTES + DIGEST_BYTES || bytes.len() > MAX_FRAME_BYTES { return Err("Recovery record length is invalid.".into()); }
    let mut offset = 0usize;
    if &take::<8>(bytes, &mut offset)? != MAGIC { return Err("Recovery record header is invalid.".into()); }
    let version = u16::from_le_bytes(take(bytes, &mut offset)?);
    if !(1..=VERSION).contains(&version) { return Err("Recovery record version is unsupported.".into()); }
    let generation = u64::from_le_bytes(take(bytes, &mut offset)?);
    let revision = u64::from_le_bytes(take(bytes, &mut offset)?);
    let current_page = u32::from_le_bytes(take(bytes, &mut offset)?);
    let source_pages = u32::from_le_bytes(take(bytes, &mut offset)?);
    let source = SourceIdentity { bytes: u64::from_le_bytes(take(bytes, &mut offset)?), sha256: take(bytes, &mut offset)? };
    let payload_bytes = u32::from_le_bytes(take(bytes, &mut offset)?) as usize;
    let expected = HEADER_BYTES.checked_add(payload_bytes).and_then(|value| value.checked_add(DIGEST_BYTES)).ok_or("Recovery record length is invalid.")?;
    if expected != bytes.len() { return Err("Recovery record length is invalid.".into()); }
    let payload_end = HEADER_BYTES + payload_bytes;
    if Sha256::digest(&bytes[..payload_end]).as_slice() != &bytes[payload_end..] { return Err("Recovery record integrity check failed.".into()); }
    if source != expected_source { return Err("Recovery source identity does not match.".into()); }
    if prior_generation.is_some_and(|prior| generation <= prior) { return Err("Recovery generation is stale or duplicated.".into()); }
    if revision < minimum_revision { return Err("Recovery revision is stale.".into()); }
    let edit_state: RecoveryEditState = serde_json::from_slice(&bytes[HEADER_BYTES..payload_end]).map_err(|_| "Recovery state is malformed.")?;
    if serde_json::to_vec(&edit_state).map_err(|_| "Recovery state is malformed.")? != &bytes[HEADER_BYTES..payload_end] { return Err("Recovery state is not canonical.".into()); }
    // Dates and UUID IDs appear only in version 3, and a version 3 record always has one (otherwise it is written as 2).
    if version == 1 && record_version(&edit_state) == 3 || version >= 2 && version != record_version(&edit_state) { return Err("Recovery record version does not match its contents.".into()); }
    let active = if version == 1 { true } else { !edit_state.pages.is_empty() };
    let record = RecoveryRecord { active, generation, revision, current_page, source_pages, source, edit_state };
    validate(&record)?;
    Ok(record)
}
