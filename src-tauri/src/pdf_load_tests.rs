use super::*;
use lopdf::{dictionary, Stream};
use std::{collections::BTreeSet, path::PathBuf, time::{Duration, Instant}};

/// A zlib stream that inflates to `prefix` followed by `len` copies of `byte` (literals, then length-258
/// distance-1 copies in one fixed-Huffman block, 13 bits per 258 output bytes), with a correct Adler-32. Built
/// directly, so a 1 GiB bomb costs about 6.8 MB here and nothing is ever inflated by the builder.
pub(crate) fn zlib_prefixed_run(prefix: &[u8], byte: u8, len: usize) -> Vec<u8> {
    assert!(len >= 1 && (len - 1) % 258 == 0, "length must be 1 + 258k");
    struct Bits { out: Vec<u8>, acc: u64, used: u32 }
    impl Bits {
        fn put(&mut self, value: u64, count: u32) { self.acc |= value << self.used; self.used += count; while self.used >= 8 { self.out.push(self.acc as u8); self.acc >>= 8; self.used -= 8; } }
        fn code(&mut self, code: u64, count: u32) { let reversed = (0..count).fold(0, |acc, bit| acc | (((code >> bit) & 1) << (count - 1 - bit))); self.put(reversed, count); }
        fn literal(&mut self, byte: u8) { if byte < 144 { self.code(0x30 + u64::from(byte), 8); } else { self.code(0x190 + u64::from(byte) - 144, 9); } }
    }
    let mut bits = Bits { out: vec![0x78, 0x01], acc: 0, used: 0 };
    bits.put(1, 1); bits.put(1, 2);
    for &literal in prefix { bits.literal(literal); }
    bits.literal(byte);
    for _ in 0..(len - 1) / 258 { bits.code(0b1100_0101, 8); bits.code(0, 5); }
    bits.code(0, 7);
    if bits.used > 0 { bits.out.push(bits.acc as u8); }
    let (mut a, mut b) = (1u128, 0u128);
    for &literal in prefix { a = (a + u128::from(literal)) % 65521; b = (b + a) % 65521; }
    let (n, c) = (len as u128, u128::from(byte));
    b = (b + n * a + c * (n * (n + 1) / 2)) % 65521;
    a = (a + n * c) % 65521;
    bits.out.extend_from_slice(&(((b as u32) << 16) | a as u32).to_be_bytes());
    bits.out
}

pub(crate) fn zlib_run(byte: u8, len: usize) -> Vec<u8> { zlib_prefixed_run(b"", byte, len) }

/// A PDF whose catalog lives in a Flate object stream that goes on to inflate to about `len` bytes, found through
/// a cross-reference stream, so any reader must decode that stream to open the file at all.
fn catalog_in_bomb(len: usize) -> Vec<u8> {
    let catalog = b"1 0 << /Type /Catalog /Pages 2 0 R >>
";
    let container = stream_object("/Type /ObjStm /N 1 /First 4 /Filter /FlateDecode", &zlib_prefixed_run(catalog, b' ', len));
    let mut out = b"%PDF-1.7
".to_vec();
    let mut offsets = Vec::new();
    for (number, body) in [(2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()), (3, b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>".to_vec()), (4, container)] {
        offsets.push(out.len());
        out.extend_from_slice(format!("{number} 0 obj
").as_bytes()); out.extend_from_slice(&body); out.extend_from_slice(b"
endobj
");
    }
    let xref_offset = out.len();
    let mut rows = vec![[0u8, 0, 0, 0, 0, 255, 255], [2, 0, 0, 0, 4, 0, 0]];
    for offset in offsets.iter().copied().chain([xref_offset]) { let o = (offset as u32).to_be_bytes(); rows.push([1, o[0], o[1], o[2], o[3], 0, 0]); }
    let table: Vec<u8> = rows.concat();
    out.extend_from_slice(b"5 0 obj
");
    out.extend_from_slice(&stream_object("/Type /XRef /Size 6 /W [1 4 2] /Root 1 0 R", &table));
    out.extend_from_slice(format!("
endobj
startxref
{xref_offset}
%%EOF
").as_bytes());
    out
}

fn base_document() -> Document {
    let mut document = Document::with_version("1.7");
    let pages = document.new_object_id();
    let page = document.add_object(dictionary! { "Type" => "Page", "Parent" => pages, "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()] });
    document.objects.insert(pages, dictionary! { "Type" => "Pages", "Kids" => vec![page.into()], "Count" => 1 }.into());
    let catalog = document.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages });
    document.trailer.set("Root", catalog);
    document
}

/// A one-page PDF whose objects 4.. are `extra` (each a complete object body), written directly with a classic
/// cross-reference table. lopdf's own writer drops object streams on save, so these fixtures cannot come from it.
fn raw_pdf(extra: impl IntoIterator<Item = Vec<u8>>) -> Vec<u8> {
    let mut bodies: Vec<Vec<u8>> = ["<< /Type /Catalog /Pages 2 0 R >>", "<< /Type /Pages /Kids [3 0 R] /Count 1 >>", "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"].map(|body| body.as_bytes().to_vec()).into();
    bodies.extend(extra);
    let mut out = b"%PDF-1.7
".to_vec();
    let mut offsets = Vec::with_capacity(bodies.len());
    for (index, body) in bodies.iter().enumerate() {
        offsets.push(out.len());
        out.extend_from_slice(format!("{} 0 obj
", index + 1).as_bytes()); out.extend_from_slice(body); out.extend_from_slice(b"
endobj
");
    }
    let xref = out.len();
    out.extend_from_slice(format!("xref
0 {}
0000000000 65535 f 
", bodies.len() + 1).as_bytes());
    for offset in offsets { out.extend_from_slice(format!("{offset:010} 00000 n 
").as_bytes()); }
    out.extend_from_slice(format!("trailer
<< /Size {} /Root 1 0 R >>
startxref
{xref}
%%EOF
", bodies.len() + 1).as_bytes());
    out
}

fn stream_object(dict: &str, content: &[u8]) -> Vec<u8> {
    let mut body = format!("<< {dict} /Length {} >>
stream
", content.len()).into_bytes();
    body.extend_from_slice(content); body.extend_from_slice(b"
endstream");
    body
}

/// A one-page PDF plus `count` Flate object streams, each inflating to `decoded` bytes.
pub(crate) fn object_stream_bomb(count: usize, decoded: usize) -> Vec<u8> {
    let compressed = zlib_run(b'0', decoded);
    raw_pdf((0..count).map(|_| stream_object("/Type /ObjStm /N 1 /First 4 /Filter /FlateDecode", &compressed)))
}

/// A one-page PDF plus an uncompressed object stream with `pairs` index entries that all point at one array of `width` numbers.
fn overlapping_object_stream(pairs: usize, width: usize) -> Vec<u8> {
    let index: String = (0..pairs).map(|number| format!("{} 0 ", 1000 + number)).collect();
    let body = format!("[{}]", "0 ".repeat(width));
    raw_pdf([stream_object(&format!("/Type /ObjStm /N {pairs} /First {}", index.len()), format!("{index}{body}").as_bytes())])
}

/// A one-page PDF with `count` objects in all, written directly (no lopdf) so a million objects build quickly.
pub(crate) fn many_objects(count: usize) -> Vec<u8> {
    let mut out = b"%PDF-1.7\n".to_vec();
    let mut offsets = Vec::with_capacity(count);
    let bodies = ["<< /Type /Catalog /Pages 2 0 R >>", "<< /Type /Pages /Kids [3 0 R] /Count 1 >>", "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"];
    for number in 1..=count {
        offsets.push(out.len());
        let body = bodies.get(number - 1).copied().unwrap_or("null");
        out.extend_from_slice(format!("{number} 0 obj\n{body}\nendobj\n").as_bytes());
    }
    let xref = out.len();
    out.extend_from_slice(format!("xref\n0 {}\n0000000000 65535 f \n", count + 1).as_bytes());
    for offset in offsets { out.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes()); }
    out.extend_from_slice(format!("trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n", count + 1).as_bytes());
    out
}

fn limit(result: Result<Document, LoadFailure>) -> String {
    match result { Err(LoadFailure::Limit(message)) => message, Err(LoadFailure::Parse(message)) => panic!("expected a limit refusal, got a parse error: {message}"), Ok(_) => panic!("expected a limit refusal, the file loaded") }
}

fn fixtures() -> Vec<(String, Vec<u8>)> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let mut paths = vec![root.join("resources/welcome.pdf")];
    for folder in [root.join("tests/fixtures"), root.join("../test-corpus")] {
        for entry in std::fs::read_dir(folder).unwrap() { let path = entry.unwrap().path(); if path.extension().is_some_and(|ext| ext == "pdf") { paths.push(path); } }
    }
    paths.into_iter().map(|path| (path.display().to_string(), std::fs::read(&path).unwrap())).collect()
}

fn with_object_streams(bytes: &[u8]) -> Vec<u8> {
    let mut document = Document::load_mem(bytes).unwrap();
    let mut out = Vec::new();
    document.save_with_options(&mut out, lopdf::SaveOptions::builder().use_object_streams(true).use_xref_streams(true).build()).unwrap();
    assert!(out.windows(7).any(|window| window == b"/ObjStm") && out.windows(5).any(|window| window == b"/XRef"), "the re-save must use object and cross-reference streams");
    out
}

#[test]
fn header_count_matches_indirect_objects_only() {
    assert_eq!(count_object_headers(b"1 0 obj null endobj 12  3\nobj<<>>endobj", 10), 2);
    assert_eq!(count_object_headers(b"x1 0 obj 1 0 objx 1 obj 0 obj 10 0 obj", 10), 1);
    assert_eq!(count_object_headers(&many_objects(500), usize::MAX), 500);
    assert_eq!(count_object_headers(&many_objects(500), 10), 11, "the count stops just past the limit");
}

#[test]
fn flate_bomb_object_stream_is_refused_at_the_per_stream_limit() {
    let bomb = object_stream_bomb(1, 1 + 258 * 4_161_536);
    assert!(bomb.len() < 8 * 1024 * 1024, "the 1 GiB bomb is {} bytes on the wire", bomb.len());
    let started = Instant::now();
    let message = limit(load(&bomb));
    assert!(message.contains("expands past 32 MiB") && message.ends_with("Nothing was opened."), "{message}");
    assert!(started.elapsed() < Duration::from_secs(30), "refused in {:?}", started.elapsed());
}

#[test]
fn catalog_inside_a_bomb_is_refused_and_the_same_layout_loads_when_small() {
    let small = load(&catalog_in_bomb(1 + 258 * 100)).unwrap();
    assert_eq!(small.get_pages().len(), 1, "the generated object stream and cross-reference stream are valid");
    let message = limit(load(&catalog_in_bomb(1 + 258 * 4_161_536)));
    assert!(message.contains("expands past 32 MiB"), "{message}");
}

#[test]
fn many_object_streams_are_refused_at_the_total_decode_limit() {
    let decoded = 1 + 258 * 32_512;
    let streams = MAX_LOAD_DECODE_TOTAL / decoded + 2;
    let message = limit(load(&object_stream_bomb(streams, decoded)));
    assert!(message.contains("256 MiB in total") && message.ends_with("Nothing was opened."), "{message}");
    assert!(load(&object_stream_bomb(4, decoded)).is_ok(), "streams within both budgets still load");
}

#[test]
fn many_objects_are_refused_before_lopdf_parses_them() {
    let bytes = many_objects(MAX_LOAD_OBJECTS + 1);
    let started = Instant::now();
    let message = limit(load(&bytes));
    assert!(message.contains("more than 1,000,000 objects") && message.ends_with("Nothing was opened."), "{message}");
    assert!(started.elapsed() < Duration::from_secs(10), "refused in {:?}", started.elapsed());
    assert!(load(&many_objects(5_000)).is_ok());
}

#[test]
fn object_stream_entries_count_toward_the_object_limit() {
    let message = limit(load(&overlapping_object_stream(MAX_LOAD_OBJECTS, 1)));
    assert!(message.contains("more than 1,000,000 objects"), "{message}");
}

#[test]
fn overlapping_object_stream_entries_are_refused() {
    let started = Instant::now();
    let message = limit(load(&overlapping_object_stream(20_000, 20_000)));
    assert!(message.contains("entries overlap") && message.ends_with("Nothing was opened."), "{message}");
    assert!(started.elapsed() < Duration::from_secs(10), "refused in {:?}", started.elapsed());
}

#[test]
fn a_limit_refusal_does_not_leak_into_the_next_load() {
    let _ = limit(load(&object_stream_bomb(1, 1 + 258 * 200_000)));
    let (_, welcome) = fixtures().remove(0);
    assert!(load(&welcome).is_ok());
}

#[test]
fn legitimate_fixtures_load_the_same_objects_as_lopdf() {
    for (name, bytes) in fixtures() {
        if Document::load_mem(&bytes).is_err() { assert!(matches!(load(&bytes), Err(LoadFailure::Parse(_))), "{name}"); continue; }
        for (variant, bytes) in [("as saved", bytes.clone()), ("re-saved with object and xref streams", with_object_streams(&bytes))] {
            let expected = Document::load_mem(&bytes).unwrap_or_else(|error| panic!("{name} {variant}: {error}"));
            let actual = load(&bytes).unwrap_or_else(|error| panic!("{name} {variant}: {error}"));
            assert_eq!(actual.objects, expected.objects, "{name} {variant}");
            assert_eq!(actual.trailer, expected.trailer, "{name} {variant}");
        }
    }
}

#[test]
fn prune_keeps_the_same_objects_as_lopdf() {
    let mut cases: Vec<(String, Document)> = fixtures().into_iter().filter_map(|(name, bytes)| Document::load_mem(&bytes).ok().map(|document| (name, document))).collect();
    let mut orphans = base_document();
    let a = orphans.new_object_id(); let b = orphans.new_object_id();
    orphans.objects.insert(a, dictionary! { "Next" => b }.into());
    orphans.objects.insert(b, vec![Object::Reference(a), Object::Reference((9999, 0))].into());
    let page = *orphans.get_pages().values().next().unwrap();
    let shared = orphans.add_object(Stream::new(dictionary! { "Self" => page }, b"q Q".to_vec()));
    orphans.get_dictionary_mut(page).unwrap().set("Contents", shared);
    orphans.add_object(dictionary! { "Orphan" => shared });
    cases.push(("orphans and cycles".into(), orphans));
    for (name, document) in cases {
        let mut ours = document.clone(); let mut theirs = document;
        prune_unreachable(&mut ours); theirs.prune_objects();
        assert_eq!(ours.objects.keys().collect::<BTreeSet<_>>(), theirs.objects.keys().collect::<BTreeSet<_>>(), "{name}");
    }
}

pub(crate) fn chain_document(count: usize) -> Document {
    let mut document = base_document();
    let page = *document.get_pages().values().next().unwrap();
    let mut next = Object::Null;
    for _ in 0..count { next = document.add_object(dictionary! { "Next" => next }).into(); }
    document.get_dictionary_mut(page).unwrap().set("Chain", next);
    for _ in 0..count / 10 { document.add_object(dictionary! { "Orphan" => true }); }
    document
}

#[test]
fn prune_is_linear_at_scale() {
    let mut document = chain_document(200_000);
    let started = Instant::now();
    prune_unreachable(&mut document);
    let elapsed = started.elapsed();
    assert_eq!(document.objects.len(), 200_000 + 3);
    assert!(elapsed < Duration::from_secs(10), "200,000-object prune took {elapsed:?}");
}

/// Measurement probe, not part of the suite. `SM5_PROBE` = `before` (lopdf as on master) or `after` (this
/// module); `SM5_PROBE_CASE` = `bomb`, `streams`, `objects`, `objects-under`, `overlap`, `prune-<n>`, or a file path.
/// Prints elapsed time; peak memory is read from outside the process.
#[test]
#[ignore]
fn sm5_load_probe() {
    let mode = std::env::var("SM5_PROBE").unwrap_or_else(|_| "after".into());
    let case = std::env::var("SM5_PROBE_CASE").unwrap_or_else(|_| "bomb".into());
    if let Some(count) = case.strip_prefix("prune-") {
        let mut document = chain_document(count.parse().unwrap());
        let started = Instant::now();
        if mode == "before" { document.prune_objects(); } else { prune_unreachable(&mut document); }
        println!("SM5_PROBE {mode} {case}: {:?}, {} objects kept", started.elapsed(), document.objects.len());
        return;
    }
    let bytes = match case.as_str() {
        "bomb" => object_stream_bomb(1, 1 + 258 * 4_161_536),
        "streams" => object_stream_bomb(40, 1 + 258 * 32_512),
        "objects" => many_objects(MAX_LOAD_OBJECTS + 1),
        "objects-under" => many_objects(MAX_LOAD_OBJECTS - 1),
        "overlap" => overlapping_object_stream(3_000, 3_000),
        "catalog-bomb" => catalog_in_bomb(1 + 258 * 4_161_536),
        path => std::fs::read(path).unwrap(),
    };
    if mode == "pdfium" {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let pdfium = pdfium_render::prelude::Pdfium::new(pdfium_render::prelude::Pdfium::bind_to_library(root.join("resources/pdfium/bin/pdfium.dll")).unwrap());
        let started = Instant::now();
        let outcome = pdfium.load_pdf_from_byte_vec(bytes.clone(), None).map(|document| document.pages().len()).map_err(|error| error.to_string());
        println!("SM5_PROBE pdfium {case}: {} bytes, {:?}, pages {:?}", bytes.len(), started.elapsed(), outcome);
        return;
    }
    let started = Instant::now();
    let outcome = if mode == "before" { Document::load_mem(&bytes).map(|document| document.objects.len()).map_err(|error| error.to_string()) } else { load(&bytes).map(|document| document.objects.len()).map_err(|error| error.to_string()) };
    println!("SM5_PROBE {mode} {}: {} bytes, {:?}, {:?}", if case.contains(['/', '\\']) { "file" } else { case.as_str() }, bytes.len(), started.elapsed(), outcome);
}
