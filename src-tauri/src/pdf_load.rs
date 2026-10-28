//! lopdf loading with the open-time work limits, and the linear reachability prune.
//!
//! `Document::load_mem` has no decompression or object limits: a small Flate-compressed object stream can
//! inflate without bound, and a file of tiny objects is parsed in full before any later check can refuse it.
//! Every lopdf load of a document source goes through `load`, which
//! 1. counts indirect-object headers in the raw bytes before lopdf parses anything (`MAX_LOAD_OBJECTS`);
//! 2. asks lopdf to refuse any single stream it decodes while loading (object and cross-reference streams)
//!    past `MAX_LOAD_STREAM_DECODE_BYTES`;
//! 3. measures every object stream in a load filter before lopdf decodes it: its decoded size against the
//!    per-stream and total budgets (`MAX_LOAD_DECODE_TOTAL`), its entry count against the object limit, and
//!    whether its entries overlap (lopdf parses one copy of an object per index entry, so entries that point
//!    into one large object multiply the parse work without any decompression).
//! A tripped limit makes the filter drop every later object, so nothing more is decoded or kept, and the load
//! is refused with a message that ends "Nothing was opened."
//! A successful load is then reconciled with the cross-reference chain as a conforming reader resolves it
//! (`xref_resolve::reconcile`, SM3): free and absent numbers, hybrid `/XRefStm` sections, object-stream members.
//!
//! lopdf passes the filter no context and runs it on rayon threads, so the budget lives in process-wide
//! atomics and `GATE` lets one budgeted load run at a time. The app's loads all run on the single PDF worker,
//! so the gate only serializes parallel tests.
//!
//! Not covered (see the SM5 charter): lopdf's encrypted-document loader ignores the filter, so for an
//! encrypted file only the object-count pre-check and the per-stream cap apply; and cross-reference entries
//! that share one offset make lopdf parse that object once per entry.

use crate::open_limits::{MAX_LOAD_DECODE_TOTAL, MAX_LOAD_OBJECTS, MAX_LOAD_STREAM_DECODE_BYTES};
use lopdf::{DecompressError, Document, LoadOptions, Object, ObjectId, Stream};
use std::{
    collections::HashSet,
    sync::{
        atomic::{AtomicU8, AtomicUsize, Ordering},
        Mutex, PoisonError,
    },
};

#[derive(Debug)]
pub enum LoadFailure {
    /// A work limit refused the file; the message is complete and ends "Nothing was opened."
    Limit(String),
    /// lopdf could not parse the file.
    Parse(String),
}

impl std::fmt::Display for LoadFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self { Self::Limit(message) | Self::Parse(message) => formatter.write_str(message) }
    }
}

const TRIP_NONE: u8 = 0;
const TRIP_OBJECTS: u8 = 1;
const TRIP_STREAM: u8 = 2;
const TRIP_TOTAL: u8 = 3;
const TRIP_OVERLAP: u8 = 4;

static GATE: Mutex<()> = Mutex::new(());
static OBJECTS: AtomicUsize = AtomicUsize::new(0);
static DECODED: AtomicUsize = AtomicUsize::new(0);
static TRIP: AtomicU8 = AtomicU8::new(TRIP_NONE);

fn mib(bytes: usize) -> usize { bytes / (1024 * 1024) }

fn grouped(value: usize) -> String {
    let digits = value.to_string();
    let mut out = String::new();
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index) % 3 == 0 { out.push(','); }
        out.push(digit);
    }
    out
}

fn objects_message() -> String { format!("This PDF has more than {} objects, over the limit for opening a file. Nothing was opened.", grouped(MAX_LOAD_OBJECTS)) }

fn trip_message(reason: u8) -> String {
    match reason {
        TRIP_OBJECTS => objects_message(),
        TRIP_STREAM => format!("A compressed part of this PDF expands past {} MiB, over the limit for opening a file. Nothing was opened.", mib(MAX_LOAD_STREAM_DECODE_BYTES)),
        TRIP_TOTAL => format!("The compressed parts of this PDF expand past {} MiB in total, over the limit for opening a file. Nothing was opened.", mib(MAX_LOAD_DECODE_TOTAL)),
        _ => "This PDF has a damaged object stream whose entries overlap, which would take too much work to read. Nothing was opened.".into(),
    }
}

fn is_whitespace(byte: u8) -> bool { matches!(byte, b'\0' | b'\t' | b'\n' | b'\x0c' | b'\r' | b' ') }
fn is_delimiter(byte: u8) -> bool { matches!(byte, b'(' | b')' | b'<' | b'>' | b'[' | b']' | b'{' | b'}' | b'/' | b'%') }
fn is_regular(byte: u8) -> bool { !is_whitespace(byte) && !is_delimiter(byte) }

/// Counts `N G obj` headers in the raw bytes, stopping once `stop_after` is passed. Linear and allocation-free.
/// It is an upper bound on the objects lopdf parses directly (by cross-reference or by its header-scanning repair),
/// give or take headers that appear inside stream data.
pub(crate) fn count_object_headers(bytes: &[u8], stop_after: usize) -> usize {
    let mut count = 0;
    let mut position = 0;
    while let Some(found) = bytes[position..].iter().position(|&byte| byte == b'o') {
        let at = position + found;
        position = at + 1;
        if bytes.get(at + 1) != Some(&b'b') || bytes.get(at + 2) != Some(&b'j') { continue; }
        if bytes.get(at + 3).is_some_and(|&byte| is_regular(byte)) { continue; }
        let mut cursor = at;
        let mut ok = true;
        for _ in 0..2 {
            let spaces = cursor;
            while cursor > 0 && is_whitespace(bytes[cursor - 1]) { cursor -= 1; }
            let digits = cursor;
            while cursor > 0 && bytes[cursor - 1].is_ascii_digit() { cursor -= 1; }
            if spaces == digits || digits == cursor { ok = false; break; }
        }
        if !ok || (cursor > 0 && is_regular(bytes[cursor - 1])) { continue; }
        count += 1;
        if count > stop_after { break; }
    }
    count
}

/// Bytes the object starting at `start` spans, by a lexer that follows strings, names, arrays and dictionaries
/// the way lopdf's parser does. Returns `None` once the span passes `allowance` bytes.
fn object_span(data: &[u8], start: usize, allowance: usize) -> Option<usize> {
    let mut position = start;
    let mut depth = 0usize;
    loop {
        if position - start > allowance { return None; }
        while position < data.len() && is_whitespace(data[position]) { position += 1; }
        if position >= data.len() { return Some(position - start); }
        match data[position] {
            b'%' => { while position < data.len() && !matches!(data[position], b'\r' | b'\n') { position += 1; } continue; }
            b'(' => {
                let mut nesting = 0usize;
                while position < data.len() {
                    match data[position] { b'\\' => position += 1, b'(' => nesting += 1, b')' => { nesting -= 1; if nesting == 0 { break; } }, _ => {} }
                    position += 1;
                }
                position += 1;
            }
            b'<' if data.get(position + 1) == Some(&b'<') => { depth += 1; position += 2; continue; }
            b'<' => { while position < data.len() && data[position] != b'>' { position += 1; } position += 1; }
            b'>' if data.get(position + 1) == Some(&b'>') && depth > 0 => { depth -= 1; position += 2; }
            b'[' => { depth += 1; position += 1; continue; }
            b']' if depth > 0 => { depth -= 1; position += 1; }
            b'/' => { position += 1; while position < data.len() && is_regular(data[position]) { position += 1; } }
            byte if is_regular(byte) => { while position < data.len() && is_regular(data[position]) { position += 1; } }
            _ => return Some(position - start),
        }
        if depth == 0 { return Some(position.min(data.len()) - start); }
    }
}

/// Measures one object stream before lopdf decodes it. `Ok` covers streams lopdf will reject on its own
/// (bad filter, missing `/First`, non-text index), which it reports and skips as it always has.
fn measure_object_stream(stream: &Stream) -> Result<(), u8> {
    let content = match stream.get_plain_content_with_limit(MAX_LOAD_STREAM_DECODE_BYTES) {
        Ok(content) => content,
        Err(lopdf::Error::Decompress(DecompressError::MemoryLimitExceeded { .. })) => return Err(TRIP_STREAM),
        Err(_) => return Ok(()),
    };
    if DECODED.fetch_add(content.len(), Ordering::Relaxed).saturating_add(content.len()) > MAX_LOAD_DECODE_TOTAL { return Err(TRIP_TOTAL); }
    let Some(first) = stream.dict.get(b"First").and_then(Object::as_i64).ok().and_then(|first| usize::try_from(first).ok()) else { return Ok(()) };
    let Some(index) = content.get(..first) else { return Ok(()) };
    let Ok(index) = std::str::from_utf8(index) else { return Ok(()) };
    let numbers: Vec<Option<u32>> = index.split_whitespace().map(|number| number.parse().ok()).collect();
    let pairs = numbers.len() / 2;
    if OBJECTS.load(Ordering::Relaxed).saturating_add(pairs) > MAX_LOAD_OBJECTS { return Err(TRIP_OBJECTS); }
    let mut allowance = content.len().saturating_sub(first).saturating_add(pairs.saturating_mul(16));
    for pair in numbers[..pairs * 2].chunks(2) {
        let (Some(_), Some(offset)) = (pair[0], pair[1]) else { continue };
        let start = first.saturating_add(offset as usize);
        if start >= content.len() { continue; }
        match object_span(&content, start, allowance) {
            Some(span) => allowance -= span.min(allowance),
            None => return Err(TRIP_OVERLAP),
        }
    }
    Ok(())
}

fn trip(reason: u8) { let _ = TRIP.compare_exchange(TRIP_NONE, reason, Ordering::AcqRel, Ordering::Acquire); }

/// lopdf calls this for every directly stored object, and again for every object-stream entry it keeps.
/// For a direct object lopdf keeps `object` itself and ignores the returned value; for an entry it keeps the
/// returned value. Streams are only ever direct, so they return a placeholder; other objects return a copy.
fn budget_filter(id: ObjectId, object: &mut Object) -> Option<(ObjectId, Object)> {
    if TRIP.load(Ordering::Acquire) != TRIP_NONE { *object = Object::Null; return None; }
    if OBJECTS.fetch_add(1, Ordering::Relaxed) >= MAX_LOAD_OBJECTS { trip(TRIP_OBJECTS); *object = Object::Null; return None; }
    if let Object::Stream(stream) = object {
        if stream.dict.has_type(b"ObjStm") {
            if let Err(reason) = measure_object_stream(stream) { trip(reason); *object = Object::Null; return None; }
        }
        return Some((id, Object::Null));
    }
    Some((id, object.clone()))
}

/// Loads a document source within the open-time work limits.
pub fn load(bytes: &[u8]) -> Result<Document, LoadFailure> {
    if count_object_headers(bytes, MAX_LOAD_OBJECTS) > MAX_LOAD_OBJECTS { return Err(LoadFailure::Limit(objects_message())); }
    let _gate = GATE.lock().unwrap_or_else(PoisonError::into_inner);
    OBJECTS.store(0, Ordering::Relaxed);
    DECODED.store(0, Ordering::Relaxed);
    TRIP.store(TRIP_NONE, Ordering::Release);
    let options = LoadOptions { filter: Some(budget_filter), max_decompressed_size: Some(MAX_LOAD_STREAM_DECODE_BYTES), ..Default::default() };
    let loaded = Document::load_mem_with_options(bytes, options);
    let reason = TRIP.swap(TRIP_NONE, Ordering::AcqRel);
    if reason != TRIP_NONE { return Err(LoadFailure::Limit(trip_message(reason))); }
    let mut document = loaded.map_err(|error| LoadFailure::Parse(error.to_string()))?;
    if document.objects.len() > MAX_LOAD_OBJECTS { return Err(LoadFailure::Limit(objects_message())); }
    crate::xref_resolve::reconcile(&mut document, bytes).map_err(LoadFailure::Parse)?;
    Ok(document)
}

/// The open paths: `Err` refuses the open; `Ok(Err)` is a parse failure the session records, as before.
pub fn load_for_open(bytes: &[u8]) -> Result<Result<Document, String>, String> {
    match load(bytes) {
        Ok(document) => Ok(Ok(document)),
        Err(LoadFailure::Limit(message)) => Err(message),
        Err(LoadFailure::Parse(message)) => Ok(Err(message)),
    }
}

/// Every object reachable from the trailer, following references through arrays, dictionaries and stream
/// dictionaries. Linear: a HashSet visited set (lopdf's `traverse_objects` uses `Vec::contains`, O(n²)).
pub(crate) fn reachable(document: &Document) -> HashSet<ObjectId> {
    fn references(value: &Object, out: &mut Vec<ObjectId>) {
        match value { Object::Reference(id) => out.push(*id), Object::Array(values) => for value in values { references(value, out); }, Object::Dictionary(dict) => for (_, value) in dict { references(value, out); }, Object::Stream(stream) => for (_, value) in &stream.dict { references(value, out); }, _ => {} }
    }
    let mut pending = Vec::new(); for (_, value) in &document.trailer { references(value, &mut pending); }
    let mut live = HashSet::new();
    while let Some(id) = pending.pop() { if live.insert(id) { if let Some(value) = document.objects.get(&id) { references(value, &mut pending); } } }
    live
}

/// Drops every object not reachable from the trailer: the same object set as lopdf's `prune_objects`, in linear time.
pub(crate) fn prune_unreachable(document: &mut Document) {
    let live = reachable(document);
    document.objects.retain(|id, _| live.contains(id));
}

#[cfg(test)]
#[path = "pdf_load_tests.rs"]
pub(crate) mod tests;
