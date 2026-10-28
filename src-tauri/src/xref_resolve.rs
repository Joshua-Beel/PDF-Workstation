//! Object resolution the way a conforming reader does it, reconciled onto lopdf's load.
//!
//! lopdf 0.45 builds its cross-reference table in three ways a conforming reader (ISO 32000-1 7.5.4,
//! 7.5.6, 7.5.8.4) does not:
//! 1. it drops `f` rows, so a number a newer revision freed is read from an older revision instead;
//! 2. it reads a hybrid file's `/XRefStm` only from the newest trailer, only when that trailer has `/Prev`,
//!    and after the older section, and never for an older section;
//! 3. it adds every object-stream member whose number has no `Compressed` entry in its table, from
//!    whichever object stream holds it (and with rayon, which of two copies wins is not even fixed).
//! A conforming reader instead walks the `/Prev` chain newest first; within a section the table comes first
//! and the hybrid `/XRefStm` fills the numbers the table leaves free or absent; a free or absent number is
//! the null object, and an object-stream member is read only from the container its entry names.
//!
//! The bundled PDFium (151.0.7881) does not follow the standard everywhere either (source tests in
//! `xref_resolve_tests.rs`): it reads a `/XRefStm` only when that section's trailer also has `/Prev` (Word's
//! layout, also inside an older revision), and it keeps an object a newer revision freed. It does agree with
//! the standard that an object-stream member with a free or absent entry is null.
//!
//! `reconcile` re-walks the chain within fixed bounds (sections, rows, cross-reference stream input plus output,
//! dictionary bytes; offsets deduplicated after lopdf's ±64-byte correction), then:
//! - removes an object-stream member whose number is free or absent (the standard and PDFium agree: null);
//! - for every other disagreement (a number freed by a newer revision, a member lopdf did not take from its named
//!   container, an in-use entry at offset 0 that an object stream fills, a direct entry lopdf read from elsewhere,
//!   an offset that holds a different object number) removes the objects when nothing reachable from the trailer
//!   uses them (a saved copy prunes them anyway), and otherwise refuses the load;
//! - refuses a file whose raw bytes show object streams (`/ObjStm` or `/XRefStm`) when its chain cannot be walked
//!   or lopdf had to rebuild its table by scanning; without them every reader's scan finds the same direct objects,
//!   so lopdf's load is kept.
//! A refusal reaches the open path as `xref_resolve::AMBIGUOUS`: viewing, search, selection and OCR keep working
//! (see `editor.rs`), while every lopdf-backed feature (edits, saved copies, comments, forms, page labels) shows it.
//! The resulting `reference_table` is the standard one (in-use entries only, as lopdf keeps it).

use crate::open_limits::{MAX_LOAD_OBJECTS, MAX_LOAD_STREAM_DECODE_BYTES};
use lopdf::{xref::XrefEntry, Dictionary, Document, Object, Stream, StringFormat};
use std::collections::{BTreeMap, HashSet};

const MAX_SECTIONS: usize = 1024;
const MAX_ROWS: usize = 4 * MAX_LOAD_OBJECTS;
const MAX_XREF_STREAM_BYTES: usize = MAX_LOAD_STREAM_DECODE_BYTES;
const MAX_DEPTH: usize = 32;
const RECOVERY_WINDOW: usize = 64;
/// Trailer and cross-reference stream dictionaries are parsed within this many bytes (an unterminated string or
/// array cannot run to the end of the file).
const MAX_DICTIONARY_BYTES: usize = 64 * 1024;
/// An object header (`N G obj`) is looked for within this many bytes of its entry's offset.
const HEADER_WINDOW: usize = 64;

pub(crate) const AMBIGUOUS: &str = "PDF readers resolve this file's cross-reference data differently (its revisions, object streams or cross-reference streams disagree), so it cannot be edited or saved safely. Viewing is unaffected.";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Entry {
    Free,
    Normal { offset: u64, generation: u64 },
    Compressed { container: u64, index: u64 },
}

type Section = BTreeMap<u32, Entry>;
type Walk<T> = Result<T, &'static str>;

/// Work charged across one walk: rows, and the encoded plus decoded bytes of every cross-reference stream.
struct Budget { rows: usize, stream_bytes: usize }

impl Budget {
    fn rows(&mut self, count: usize) -> Walk<()> {
        self.rows = self.rows.checked_add(count).filter(|rows| *rows <= MAX_ROWS).ok_or("too many cross-reference rows")?;
        Ok(())
    }
    fn stream_bytes(&mut self, count: usize) -> Walk<()> {
        self.stream_bytes = self.stream_bytes.checked_add(count).filter(|bytes| *bytes <= MAX_XREF_STREAM_BYTES).ok_or("cross-reference streams exceed the decode budget")?;
        Ok(())
    }
    fn stream_allowance(&self) -> usize { MAX_XREF_STREAM_BYTES - self.stream_bytes }
}

fn dictionary(buf: &[u8], pos: usize) -> Walk<(Dictionary, usize)> {
    let window = &buf[..buf.len().min(pos.saturating_add(MAX_DICTIONARY_BYTES))];
    match object(window, pos, 0)? { (Object::Dictionary(dict), end) => Ok((dict, end)), _ => Err("expected a dictionary") }
}

/// The object number and generation written at `offset`, looked for only within `HEADER_WINDOW` bytes.
fn header_at(buf: &[u8], offset: usize) -> Option<(u64, u64)> {
    let window = buf.get(..buf.len().min(offset.saturating_add(HEADER_WINDOW)))?;
    let (number, pos) = uint(window, skip_ws(window, offset)).ok()?;
    let (generation, pos) = uint(window, skip_ws(window, pos)).ok()?;
    keyword(window, skip_ws(window, pos), b"obj").then_some((number, generation))
}

fn is_whitespace(byte: u8) -> bool { matches!(byte, b'\0' | b'\t' | b'\n' | b'\x0c' | b'\r' | b' ') }
fn is_delimiter(byte: u8) -> bool { matches!(byte, b'(' | b')' | b'<' | b'>' | b'[' | b']' | b'{' | b'}' | b'/' | b'%') }
fn is_regular(byte: u8) -> bool { !is_whitespace(byte) && !is_delimiter(byte) }

fn skip_ws(buf: &[u8], mut pos: usize) -> usize {
    while let Some(&byte) = buf.get(pos) {
        if is_whitespace(byte) { pos += 1; } else if byte == b'%' { while buf.get(pos).is_some_and(|byte| !matches!(byte, b'\r' | b'\n')) { pos += 1; } } else { break; }
    }
    pos
}

fn keyword(buf: &[u8], pos: usize, word: &[u8]) -> bool {
    buf.get(pos..).is_some_and(|rest| rest.starts_with(word)) && buf.get(pos + word.len()).is_none_or(|byte| !is_regular(*byte))
}

fn uint(buf: &[u8], pos: usize) -> Walk<(u64, usize)> {
    let mut end = pos; let mut value: u64 = 0;
    while let Some(digit) = buf.get(end).filter(|byte| byte.is_ascii_digit()) {
        value = value.checked_mul(10).and_then(|value| value.checked_add(u64::from(digit - b'0'))).ok_or("number too large")?;
        end += 1;
    }
    if end == pos { return Err("expected a number"); }
    Ok((value, end))
}

fn obj_header(buf: &[u8], pos: usize) -> Walk<usize> {
    let (_, pos) = uint(buf, pos)?;
    let pos = skip_ws(buf, pos);
    let (_, pos) = uint(buf, pos)?;
    let pos = skip_ws(buf, pos);
    if !keyword(buf, pos, b"obj") { return Err("expected obj"); }
    Ok(pos + 3)
}

fn name(buf: &[u8], mut pos: usize) -> Walk<(Vec<u8>, usize)> {
    pos += 1;
    let mut out = Vec::new();
    while let Some(&byte) = buf.get(pos).filter(|byte| is_regular(**byte)) {
        if byte == b'#' {
            let hex = buf.get(pos + 1..pos + 3).and_then(|hex| std::str::from_utf8(hex).ok()).and_then(|hex| u8::from_str_radix(hex, 16).ok());
            match hex { Some(value) => { out.push(value); pos += 3; } None => { out.push(byte); pos += 1; } }
        } else { out.push(byte); pos += 1; }
    }
    Ok((out, pos))
}

fn number(buf: &[u8], pos: usize) -> Walk<(Object, usize)> {
    let mut end = pos;
    if matches!(buf.get(end), Some(b'+' | b'-')) { end += 1; }
    let digits = end;
    while buf.get(end).is_some_and(|byte| byte.is_ascii_digit() || *byte == b'.') { end += 1; }
    if end == digits { return Err("expected a number"); }
    let text = std::str::from_utf8(&buf[pos..end]).map_err(|_| "bad number")?;
    if text.contains('.') { return Ok((Object::Real(text.parse::<f32>().map_err(|_| "bad number")?), end)); }
    Ok((Object::Integer(text.parse::<i64>().map_err(|_| "bad number")?), end))
}

/// One direct object, as much of the PDF syntax as trailer and cross-reference stream dictionaries use.
fn object(buf: &[u8], pos: usize, depth: usize) -> Walk<(Object, usize)> {
    if depth > MAX_DEPTH { return Err("nested too deeply"); }
    let pos = skip_ws(buf, pos);
    match *buf.get(pos).ok_or("unexpected end")? {
        b'<' if buf.get(pos + 1) == Some(&b'<') => {
            let mut dict = Dictionary::new(); let mut pos = pos + 2;
            loop {
                pos = skip_ws(buf, pos);
                if buf.get(pos..).is_some_and(|rest| rest.starts_with(b">>")) { return Ok((Object::Dictionary(dict), pos + 2)); }
                if buf.get(pos) != Some(&b'/') { return Err("expected a name"); }
                let (key, next) = name(buf, pos)?;
                let (value, next) = object(buf, next, depth + 1)?;
                dict.set(key, value); pos = next;
            }
        }
        b'<' => {
            let end = buf[pos..].iter().position(|byte| *byte == b'>').ok_or("unterminated hex string")? + pos;
            let digits: Vec<u8> = buf[pos + 1..end].iter().copied().filter(|byte| !is_whitespace(*byte)).collect();
            let bytes = digits.chunks(2).map(|pair| { let text = std::str::from_utf8(pair).map_err(|_| "bad hex")?; u8::from_str_radix(text, 16).map(|value| if pair.len() == 1 { value << 4 } else { value }).map_err(|_| "bad hex") }).collect::<Walk<Vec<u8>>>()?;
            Ok((Object::String(bytes, StringFormat::Hexadecimal), end + 1))
        }
        b'(' => {
            let (mut level, mut end) = (0usize, pos);
            loop {
                match *buf.get(end).ok_or("unterminated string")? { b'\\' => end += 1, b'(' => level += 1, b')' => { level -= 1; if level == 0 { break; } } _ => {} }
                end += 1;
            }
            Ok((Object::String(buf[pos + 1..end].to_vec(), StringFormat::Literal), end + 1))
        }
        b'[' => {
            let mut items = Vec::new(); let mut pos = pos + 1;
            loop {
                pos = skip_ws(buf, pos);
                if buf.get(pos) == Some(&b']') { return Ok((Object::Array(items), pos + 1)); }
                let (item, next) = object(buf, pos, depth + 1)?;
                items.push(item); pos = next;
            }
        }
        b'/' => { let (value, end) = name(buf, pos)?; Ok((Object::Name(value), end)) }
        b'0'..=b'9' | b'+' | b'-' | b'.' => {
            let (value, end) = number(buf, pos)?;
            if let Object::Integer(id) = value {
                let reference = (|| {
                    let (generation, after) = uint(buf, skip_ws(buf, end)).ok()?;
                    let after = skip_ws(buf, after);
                    keyword(buf, after, b"R").then_some((u32::try_from(id).ok()?, u16::try_from(generation).ok()?, after + 1))
                })();
                if let Some((id, generation, after)) = reference { return Ok((Object::Reference((id, generation)), after)); }
            }
            Ok((value, end))
        }
        _ if keyword(buf, pos, b"true") => Ok((Object::Boolean(true), pos + 4)),
        _ if keyword(buf, pos, b"false") => Ok((Object::Boolean(false), pos + 5)),
        _ if keyword(buf, pos, b"null") => Ok((Object::Null, pos + 4)),
        _ => Err("unexpected token"),
    }
}

fn int(dict: &Dictionary, key: &[u8]) -> Option<i64> { dict.get(key).ok().and_then(|value| value.as_i64().ok()) }

/// lopdf's lenient offset correction, mirrored so both walks start each section at the same place.
fn correct(buf: &[u8], offset: usize) -> usize {
    let Some(rest) = buf.get(offset..) else { return offset };
    if rest.starts_with(b"xref") || obj_header(buf, offset).is_ok() { return offset; }
    let (start, end) = (offset.saturating_sub(RECOVERY_WINDOW), buf.len().min(offset + RECOVERY_WINDOW));
    (start..end.saturating_sub(4)).filter(|&pos| buf[pos..].starts_with(b"xref") && !(pos >= 5 && &buf[pos - 5..pos] == b"start")).min_by_key(|pos| pos.abs_diff(offset)).unwrap_or(offset)
}

fn table(buf: &[u8], pos: usize, budget: &mut Budget) -> Walk<(Section, Dictionary)> {
    let mut section = Section::new(); let mut pos = pos + 4;
    loop {
        pos = skip_ws(buf, pos);
        if keyword(buf, pos, b"trailer") { break; }
        let (first, next) = uint(buf, pos)?;
        let (count, next) = uint(buf, skip_ws(buf, next))?;
        let count = usize::try_from(count).map_err(|_| "bad count")?;
        if count > buf.len().saturating_sub(next) / 6 { return Err("subsection count exceeds the file"); }
        budget.rows(count)?;
        pos = next;
        for index in 0..count {
            let (offset, next) = uint(buf, skip_ws(buf, pos))?;
            let (generation, next) = uint(buf, skip_ws(buf, next))?;
            let next = skip_ws(buf, next);
            let kind = *buf.get(next).ok_or("unexpected end")?;
            if !matches!(kind, b'n' | b'f') || buf.get(next + 1).is_some_and(|byte| is_regular(*byte)) { return Err("bad cross-reference row"); }
            let number = first.checked_add(index as u64).and_then(|number| u32::try_from(number).ok()).ok_or("object number out of range")?;
            pos = next + 1;
            // lopdf skips an in-use row whose generation does not fit (writers emit `0000000000 65536 n`); so does this walk.
            if kind == b'n' && generation > u64::from(u16::MAX) { continue; }
            section.insert(number, if kind == b'n' { Entry::Normal { offset, generation } } else { Entry::Free });
        }
    }
    Ok((section, dictionary(buf, pos + 7)?.0))
}

fn field(bytes: &[u8]) -> u64 { bytes.iter().fold(0u64, |value, byte| (value << 8) | u64::from(*byte)) }

fn xref_stream(buf: &[u8], pos: usize, budget: &mut Budget) -> Walk<(Section, Dictionary)> {
    let pos = obj_header(buf, pos)?;
    let (dict, pos) = dictionary(buf, pos)?;
    if dict.get(b"Type").and_then(Object::as_name).ok() != Some(b"XRef") { return Err("not a cross-reference stream"); }
    let pos = skip_ws(buf, pos);
    if !keyword(buf, pos, b"stream") { return Err("expected stream"); }
    let rest = &buf[pos + 6..];
    let start = pos + 6 + if rest.starts_with(b"\r\n") { 2 } else if rest.starts_with(b"\n") || rest.starts_with(b"\r") { 1 } else { 0 };
    let length = match dict.get(b"Length") { Ok(Object::Integer(length)) => usize::try_from(*length).map_err(|_| "bad length")?, _ => return Err("xref stream length is not a direct integer") };
    let data = buf.get(start..start.checked_add(length).ok_or("bad length")?).ok_or("xref stream past the end")?;
    budget.stream_bytes(length)?;
    let allowance = budget.stream_allowance();
    if allowance == 0 { return Err("cross-reference streams exceed the decode budget"); }
    let decoded = Stream::new(dict.clone(), data.to_vec()).get_plain_content_with_limit(allowance).map_err(|_| "xref stream cannot be decoded")?;
    budget.stream_bytes(decoded.len())?;
    let widths = match dict.get(b"W") { Ok(Object::Array(values)) if values.len() == 3 => values.iter().map(|value| value.as_i64().ok().and_then(|width| usize::try_from(width).ok()).filter(|width| *width <= 8)).collect::<Option<Vec<usize>>>().ok_or("bad /W")?, _ => return Err("bad /W") };
    let width = widths.iter().sum::<usize>();
    if width == 0 { return Err("bad /W"); }
    let size = int(&dict, b"Size").and_then(|size| u64::try_from(size).ok()).ok_or("bad /Size")?;
    let index = match dict.get(b"Index") {
        Ok(Object::Array(values)) if values.len() % 2 == 0 => values.iter().map(|value| value.as_i64().ok().and_then(|value| u64::try_from(value).ok())).collect::<Option<Vec<u64>>>().ok_or("bad /Index")?,
        Ok(_) => return Err("bad /Index"),
        Err(_) => vec![0, size],
    };
    let total = index.chunks(2).try_fold(0u64, |total, pair| total.checked_add(pair[1])).ok_or("bad /Index")?;
    if total > (decoded.len() / width) as u64 { return Err("xref stream /Index exceeds its data"); }
    budget.rows(total as usize)?;
    let mut section = Section::new(); let mut rows = decoded.chunks_exact(width);
    for pair in index.chunks(2) {
        for number in 0..pair[1] {
            let row = rows.next().ok_or("xref stream too short")?;
            let (kind, rest) = row.split_at(widths[0]);
            let (second, third) = rest.split_at(widths[1]);
            let kind = if widths[0] == 0 { 1 } else { field(kind) };
            let number = pair[0].checked_add(number).and_then(|number| u32::try_from(number).ok()).ok_or("object number out of range")?;
            let entry = match kind {
                1 => Entry::Normal { offset: field(second), generation: if widths[2] == 0 { 0 } else { field(third) } },
                2 => Entry::Compressed { container: field(second), index: field(third) },
                _ => Entry::Free,
            };
            section.insert(number, entry);
        }
    }
    Ok((section, dict))
}

fn section(buf: &[u8], offset: usize, budget: &mut Budget) -> Walk<(Section, Dictionary)> {
    let offset = correct(buf, offset);
    if buf.get(offset..).is_some_and(|rest| rest.starts_with(b"xref")) { table(buf, offset, budget) } else { xref_stream(buf, offset, budget) }
}

fn offset(value: i64, buf: &[u8]) -> Walk<usize> { usize::try_from(value).ok().filter(|offset| *offset <= buf.len()).ok_or("offset out of range") }

/// The cross-reference entries a conforming reader uses, newest section first. `buf` starts at `%PDF-`.
pub(crate) fn conforming_entries(buf: &[u8], start: usize) -> Walk<Section> {
    let mut entries = Section::new(); let mut budget = Budget { rows: 0, stream_bytes: 0 };
    // Keyed on the corrected offset each section is parsed at, so nearby pointers cannot re-parse one section; a
    // `/XRefStm` already merged adds nothing new (its entries are already in place, newer first), so it is skipped.
    let (mut seen, mut seen_streams) = (HashSet::new(), HashSet::new());
    let mut next = Some(start);
    while let Some(at) = next {
        let at = correct(buf, at);
        if !seen.insert(at) { break; }
        if seen.len() > MAX_SECTIONS { return Err("too many cross-reference sections"); }
        let (mut current, trailer) = section(buf, at, &mut budget)?;
        let hybrid_at = int(&trailer, b"XRefStm").filter(|_| buf.get(at..).is_some_and(|rest| rest.starts_with(b"xref")));
        if let Some(stream_at) = hybrid_at {
            let stream_at = correct(buf, offset(stream_at, buf)?);
            if seen_streams.insert(stream_at) {
                if seen_streams.len() > MAX_SECTIONS { return Err("too many cross-reference sections"); }
                let (hybrid, _) = xref_stream(buf, stream_at, &mut budget)?;
                for (number, entry) in hybrid {
                    let slot = current.entry(number).or_insert(Entry::Free);
                    if matches!(slot, Entry::Free | Entry::Normal { offset: 0, .. }) { *slot = entry; }
                }
            }
        }
        for (number, entry) in current { entries.entry(number).or_insert(entry); }
        next = prev(&trailer, buf)?;
    }
    Ok(entries)
}

fn prev(trailer: &Dictionary, buf: &[u8]) -> Walk<Option<usize>> {
    match trailer.get(b"Prev") { Ok(Object::Integer(prev)) => Ok(Some(offset(*prev, buf)?)), _ => Ok(None) }
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool { haystack.windows(needle.len()).any(|window| window == needle) }

fn remove_number(document: &mut Document, number: u32) {
    let ids: Vec<_> = document.objects.range((number, 0)..=(number, u16::MAX)).map(|(id, _)| *id).collect();
    for id in ids { document.objects.remove(&id); }
}

/// The text-extraction permission of a source whose load `reconcile` refused, so search, selection and OCR (which
/// read PDFium's text) keep working. It goes through the same check as any open (`encryption_state` first: lopdf
/// removes `/Encrypt` from the trailer once the empty user password decrypts the file), on lopdf's unreconciled load.
pub(crate) fn refused_source_permission(source: &[u8]) -> Result<(), String> {
    let document = crate::pdf_load::load_unreconciled(source).map_err(|_| format!("The PDF extraction permission could not be verified. {AMBIGUOUS}"))?;
    crate::editor::source_text_extraction_permission(&document)
}

/// Makes a loaded document resolve objects as a conforming reader does, or refuses it (see the module notes).
pub(crate) fn reconcile(document: &mut Document, source: &[u8]) -> Result<(), String> {
    reconcile_with_reason(document, source).map_err(|_| AMBIGUOUS.to_string())
}

/// `reconcile`, with the rule that refused the load (for the tests and the real-file probe).
pub(crate) fn reconcile_with_reason(document: &mut Document, source: &[u8]) -> Result<(), &'static str> {
    let header = source.windows(5).position(|window| window == b"%PDF-").unwrap_or(0);
    let buf = &source[header..];
    // Judged from the raw bytes: lopdf may have loaded none of the object streams it was meant to.
    let may_compress = contains(buf, b"/ObjStm") || contains(buf, b"/XRefStm");
    // lopdf rebuilt its table by scanning for object headers. Without object streams every reader's scan finds the
    // same direct objects; with them, which copy of a member wins is each reader's own guess.
    if document.xref_start == 0 { return if may_compress { Err("rebuilt table in a file with object streams") } else { Ok(()) }; }
    let entries = match conforming_entries(buf, document.xref_start) {
        Ok(entries) => entries,
        Err(_) if !may_compress => return Ok(()),
        Err(_) => return Err("chain cannot be walked in a file with object streams"),
    };
    let live: HashSet<u32> = crate::pdf_load::reachable(document).into_iter().map(|(number, _)| number).collect();
    let has = |document: &Document, number: u32| document.objects.range((number, 0)..=(number, u16::MAX)).next().is_some();
    // Each disagreement is refused when a reachable number is involved; otherwise the numbers are dropped (a saved
    // copy prunes unreachable objects anyway) and left out of the resulting table.
    let mut drop = HashSet::new();
    for (&number, entry) in &entries {
        let known = document.reference_table.entries.get(&number);
        match *entry {
            Entry::Free => {}
            Entry::Normal { offset: 0, .. } => if has(document, number) {
                if live.contains(&number) { return Err("an object stream fills an offset-0 entry"); }
                drop.insert(number);
            },
            Entry::Normal { offset, generation } => {
                if !matches!(known, Some(XrefEntry::Normal { offset: o, generation: g }) if u64::from(*o) == offset && u64::from(*g) == generation) {
                    if live.contains(&number) { return Err("a direct entry lopdf did not read from the same place"); }
                    drop.insert(number);
                    continue;
                }
                // lopdf keys a direct object by the header it finds at the offset, not by the entry's number.
                let found = usize::try_from(offset).ok().and_then(|offset| header_at(buf, offset));
                if found != Some((u64::from(number), generation)) {
                    let other = found.and_then(|(found, _)| u32::try_from(found).ok());
                    if live.contains(&number) || other.is_some_and(|other| live.contains(&other)) { return Err("an entry's offset holds a different object number"); }
                    drop.insert(number);
                    if let Some(other) = other { drop.insert(other); }
                }
            }
            Entry::Compressed { container, .. } => if !matches!(known, Some(XrefEntry::Compressed { container: c, .. }) if u64::from(*c) == container) {
                if live.contains(&number) { return Err("a used member lopdf did not take from its named container"); }
                drop.insert(number);
            },
        }
    }
    for &(number, _) in document.objects.keys() {
        if !matches!(entries.get(&number), None | Some(Entry::Free)) { continue; }
        let revived = matches!(document.reference_table.entries.get(&number), Some(XrefEntry::Normal { offset, .. }) if *offset != 0) || matches!(document.reference_table.entries.get(&number), Some(XrefEntry::Compressed { .. }));
        if revived && live.contains(&number) { return Err("a used object a newer revision freed"); }
        drop.insert(number);
    }
    for &number in &drop { remove_number(document, number); }
    let mut table = BTreeMap::new();
    for (number, entry) in entries {
        let converted = match entry {
            _ if drop.contains(&number) => continue,
            Entry::Free => continue,
            Entry::Normal { offset, generation } => XrefEntry::Normal { offset: u32::try_from(offset).map_err(|_| "entry out of range")?, generation: u16::try_from(generation).map_err(|_| "entry out of range")? },
            Entry::Compressed { container, index } => XrefEntry::Compressed { container: u32::try_from(container).map_err(|_| "entry out of range")?, index: u16::try_from(index).map_err(|_| "entry out of range")? },
        };
        table.insert(number, converted);
    }
    document.reference_table.size = table.keys().next_back().map_or(1, |number| number.saturating_add(1));
    document.reference_table.entries = table;
    if let Some(&(highest, _)) = document.objects.keys().next_back() { document.max_id = document.max_id.max(highest); }
    Ok(())
}

#[cfg(test)]
#[path = "xref_resolve_tests.rs"]
mod tests;
