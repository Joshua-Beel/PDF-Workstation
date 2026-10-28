use super::*;
use lopdf::LoadOptions;
use std::{fmt::Write as _, time::Instant};

enum Row { N(usize), F, Zero }

const CATALOG: &str = "<</Type/Catalog/Pages 2 0 R>>";
const PAGES: &str = "<</Type/Pages/Kids[]/Count 0>>";

struct Pdf { buf: Vec<u8> }

impl Pdf {
    fn new() -> Self { Pdf { buf: b"%PDF-1.7\n%\xE2\xE3\xCF\xD3\n".to_vec() } }
    fn put(&mut self, bytes: &[u8]) -> usize { let at = self.buf.len(); self.buf.extend_from_slice(bytes); at }
    fn obj(&mut self, number: u32, body: &str) -> usize { self.put(format!("{number} 0 obj\n{body}\nendobj\n").as_bytes()) }
    fn objstm(&mut self, number: u32, members: &[(u32, &str)]) -> usize {
        let (mut head, mut data) = (String::new(), String::new());
        for (member, body) in members { write!(head, "{member} {} ", data.len()).unwrap(); data.push_str(body); data.push('\n'); }
        let content = format!("{head}{data}");
        self.put(format!("{number} 0 obj\n<</Type/ObjStm/N {}/First {}/Length {}>>\nstream\n{content}\nendstream\nendobj\n", members.len(), head.len(), content.len()).as_bytes())
    }
    fn table(&mut self, rows: &[(u32, Row)], trailer: &str) -> usize {
        let mut text = String::from("xref\n0 1\n0000000000 65535 f \n");
        for (number, row) in rows {
            let (offset, generation, kind) = match row { Row::N(offset) => (*offset, 0, 'n'), Row::F => (0, 1, 'f'), Row::Zero => (0, 0, 'n') };
            write!(text, "{number} 1\n{offset:010} {generation:05} {kind} \n").unwrap();
        }
        write!(text, "trailer\n{trailer}\n").unwrap();
        self.put(text.as_bytes())
    }
    /// An uncompressed cross-reference stream, `/W [1 4 2]`, one `/Index` pair per row: (number, type, field 2, field 3).
    fn xref_stream(&mut self, number: u32, rows: &[(u32, u8, u32, u16)], extra: &str) -> usize {
        let (mut data, mut index) = (Vec::new(), String::new());
        for (row, kind, second, third) in rows { write!(index, "{row} 1 ").unwrap(); data.push(*kind); data.extend_from_slice(&second.to_be_bytes()); data.extend_from_slice(&third.to_be_bytes()); }
        let size = rows.iter().map(|row| row.0).max().unwrap_or(0) + 1;
        let at = self.put(format!("{number} 0 obj\n<</Type/XRef/Size {size}/W[1 4 2]/Index[{index}]/Length {}{extra}>>\nstream\n", data.len()).as_bytes());
        self.buf.extend_from_slice(&data);
        self.put(b"\nendstream\nendobj\n");
        at
    }
    fn finish(mut self, start: usize) -> Vec<u8> { self.put(format!("startxref\n{start}\n%%EOF\n").as_bytes()); self.buf }
}

fn value(document: &Document, number: u32) -> Option<Vec<u8>> {
    let (_, object) = document.objects.range((number, 0)..=(number, u16::MAX)).next()?;
    Some(object.as_dict().ok()?.get(b"V").ok()?.as_str().ok()?.to_vec())
}
fn raw(bytes: &[u8]) -> Document { Document::load_mem(bytes).unwrap() }
fn load(bytes: &[u8]) -> Result<Document, String> { crate::pdf_load::load(bytes).map_err(|error| error.to_string()) }

/// A catalog that uses objects 10 and 11 when `reachable`, so a reader must resolve them.
fn catalog(reachable: bool) -> &'static str { if reachable { "<</Type/Catalog/Pages 2 0 R/Uses[10 0 R 11 0 R]>>" } else { CATALOG } }

/// Hybrid file with the `/XRefStm` in the same section as the table (no `/Prev`): the table frees 10, the stream puts it in container 6.
fn same_section_hybrid(reachable: bool) -> Vec<u8> {
    let mut pdf = Pdf::new();
    let (o1, o2) = (pdf.obj(1, catalog(reachable)), pdf.obj(2, PAGES));
    let a = pdf.objstm(5, &[(10, "<</V(A)>>")]);
    let b = pdf.objstm(6, &[(10, "<</V(B)>>")]);
    let xs = pdf.xref_stream(7, &[(10, 2, 6, 0)], "");
    let start = pdf.table(&[(1, Row::N(o1)), (2, Row::N(o2)), (5, Row::N(a)), (6, Row::N(b)), (10, Row::F)], &format!("<</Size 11/Root 1 0 R/XRefStm {xs}>>"));
    pdf.finish(start)
}

#[test]
fn same_section_hybrid_member_is_refused_when_used_and_dropped_otherwise() {
    let bytes = same_section_hybrid(true);
    assert!(raw(&bytes).reference_table.get(10).is_none(), "lopdf ignores a /XRefStm without /Prev");
    assert_eq!(load(&bytes).unwrap_err(), AMBIGUOUS);
    let document = load(&same_section_hybrid(false)).unwrap();
    assert!(value(&document, 10).is_none());
    assert!(document.reference_table.get(10).is_none(), "a dropped number leaves the table too");
}

/// Word's layout: the full table first (members free), then an empty update section whose trailer has `/Prev` and `/XRefStm`.
#[test]
fn word_style_hybrid_loads_unchanged() {
    let mut pdf = Pdf::new();
    let (o1, o2) = (pdf.obj(1, catalog(true)), pdf.obj(2, PAGES));
    let a = pdf.objstm(5, &[(10, "<</V(A)>>"), (11, "<</V(also A)>>")]);
    let b = pdf.objstm(6, &[(10, "<</V(B)>>")]);
    let first = pdf.table(&[(1, Row::N(o1)), (2, Row::N(o2)), (5, Row::N(a)), (6, Row::N(b)), (10, Row::F), (11, Row::F)], "<</Size 12/Root 1 0 R>>");
    pdf.put(b"startxref\n0\n%%EOF\n");
    let xs = pdf.xref_stream(7, &[(10, 2, 6, 0), (11, 2, 6, 1)], "");
    let start = pdf.table(&[], &format!("<</Size 12/Root 1 0 R/Prev {first}/XRefStm {xs}>>"));
    let bytes = pdf.finish(start);
    let before = raw(&bytes);
    let after = load(&bytes).unwrap();
    assert_eq!(before.objects, after.objects);
    assert_eq!(value(&after, 10).as_deref(), Some(b"B".as_slice()));
    assert!(value(&after, 11).is_none(), "its named container does not hold 11");
}

#[test]
fn older_section_xrefstm_is_refused_when_used() {
    let mut pdf = Pdf::new();
    let (o1, o2) = (pdf.obj(1, catalog(true)), pdf.obj(2, PAGES));
    let a = pdf.objstm(5, &[(10, "<</V(A)>>")]);
    let b = pdf.objstm(6, &[(10, "<</V(B)>>")]);
    let xs = pdf.xref_stream(7, &[(10, 2, 6, 0)], "");
    let first = pdf.table(&[(1, Row::N(o1)), (2, Row::N(o2)), (5, Row::N(a)), (6, Row::N(b)), (10, Row::F)], &format!("<</Size 11/Root 1 0 R/XRefStm {xs}>>"));
    pdf.put(b"startxref\n0\n%%EOF\n");
    let o2 = pdf.obj(2, "<</Type/Pages/Kids[]/Count 0/Updated true>>");
    let start = pdf.table(&[(2, Row::N(o2))], &format!("<</Size 11/Root 1 0 R/Prev {first}>>"));
    let bytes = pdf.finish(start);
    assert!(raw(&bytes).reference_table.get(10).is_none(), "lopdf never reads an older section's /XRefStm");
    assert_eq!(load(&bytes).unwrap_err(), AMBIGUOUS);
}

fn freed_by_update(reachable: bool) -> Vec<u8> {
    let mut pdf = Pdf::new();
    let (o1, o2, o10) = (pdf.obj(1, catalog(reachable)), pdf.obj(2, PAGES), pdf.obj(10, "<</V(old secret)>>"));
    let first = pdf.table(&[(1, Row::N(o1)), (2, Row::N(o2)), (10, Row::N(o10))], "<</Size 11/Root 1 0 R>>");
    pdf.put(b"startxref\n0\n%%EOF\n");
    let start = pdf.table(&[(10, Row::F)], &format!("<</Size 11/Root 1 0 R/Prev {first}>>"));
    pdf.finish(start)
}

#[test]
fn a_number_freed_by_a_newer_revision_is_dropped_or_refused_when_used() {
    let bytes = freed_by_update(false);
    assert!(value(&raw(&bytes), 10).is_some(), "lopdf drops f rows and reads the older copy");
    let document = load(&bytes).unwrap();
    assert!(value(&document, 10).is_none());
    assert!(document.reference_table.get(10).is_none());
    assert_eq!(load(&freed_by_update(true)).unwrap_err(), AMBIGUOUS);
}

fn stream_supplies_ten(row: Option<Row>, reachable: bool) -> Vec<u8> {
    let mut pdf = Pdf::new();
    let (o1, o2) = (pdf.obj(1, catalog(reachable)), pdf.obj(2, PAGES));
    let a = pdf.objstm(5, &[(10, "<</V(from a stream)>>")]);
    let mut rows = vec![(1, Row::N(o1)), (2, Row::N(o2)), (5, Row::N(a))];
    if let Some(row) = row { rows.push((10, row)); }
    let start = pdf.table(&rows, "<</Size 11/Root 1 0 R>>");
    pdf.finish(start)
}

#[test]
fn an_object_stream_cannot_supply_a_free_or_missing_number() {
    for row in [Some(Row::F), None] {
        let bytes = stream_supplies_ten(row, true);
        assert!(value(&raw(&bytes), 10).is_some(), "lopdf adds every member that has no compressed entry");
        assert!(value(&load(&bytes).unwrap(), 10).is_none());
    }
}

#[test]
fn an_object_stream_supplying_an_offset_zero_entry_is_refused_when_used() {
    let bytes = stream_supplies_ten(Some(Row::Zero), true);
    assert!(value(&raw(&bytes), 10).is_some());
    assert_eq!(load(&bytes).unwrap_err(), AMBIGUOUS);
    let document = load(&stream_supplies_ten(Some(Row::Zero), false)).unwrap();
    assert!(value(&document, 10).is_none());
    assert!(document.reference_table.get(10).is_none());
}

fn reason(bytes: &[u8]) -> Result<(), &'static str> {
    let mut document = crate::pdf_load::load_unreconciled(bytes).unwrap();
    reconcile_with_reason(&mut document, bytes)
}

#[test]
fn an_offset_holding_another_object_number_is_refused_when_used() {
    for reachable in [true, false] {
        let mut pdf = Pdf::new();
        let (o1, o2) = (pdf.obj(1, catalog(reachable)), pdf.obj(2, PAGES));
        let o11 = pdf.obj(11, "<</V(eleven)>>");
        let start = pdf.table(&[(1, Row::N(o1)), (2, Row::N(o2)), (10, Row::N(o11))], "<</Size 12/Root 1 0 R>>");
        let bytes = pdf.finish(start);
        if reachable { assert_eq!(reason(&bytes), Err("an entry's offset holds a different object number")); continue; }
        let document = load(&bytes).unwrap();
        assert!(value(&document, 10).is_none() && value(&document, 11).is_none());
    }
}

/// Shaped like the refused real file: Word wrote its objects into object streams that only its `/XRefStm` lists
/// (the table frees the containers too), and another tool then appended an update, so lopdf never reads that
/// stream and loads none of the containers, the page tree included.
fn word_then_update(break_stream: bool) -> Vec<u8> {
    let mut pdf = Pdf::new();
    let o1 = pdf.obj(1, CATALOG);
    let s6 = pdf.objstm(6, &[(2, "<</Type/Pages/Kids[3 0 R]/Count 1>>"), (3, "<</Type/Page/Parent 2 0 R/MediaBox[0 0 200 200]>>")]);
    let main = pdf.table(&[(1, Row::N(o1)), (2, Row::F), (3, Row::F), (6, Row::F)], "<</Size 7/Root 1 0 R>>");
    pdf.put(b"startxref\n0\n%%EOF\n");
    let extra = if break_stream { "/Filter/FlateDecode" } else { "" };
    let xs = pdf.xref_stream(7, &[(2, 2, 6, 0), (3, 2, 6, 1), (6, 1, s6 as u32, 0)], extra);
    let word = pdf.table(&[], &format!("<</Size 7/Root 1 0 R/Prev {main}/XRefStm {xs}>>"));
    pdf.put(b"startxref\n0\n%%EOF\n");
    let o1 = pdf.obj(1, "<</Type/Catalog/Pages 2 0 R/Lang(en)>>");
    let start = pdf.table(&[(1, Row::N(o1))], &format!("<</Size 7/Root 1 0 R/Prev {word}>>"));
    pdf.finish(start)
}

#[test]
fn a_word_file_with_a_later_update_is_refused_but_stays_readable() {
    let bytes = word_then_update(false);
    let before = raw(&bytes);
    assert!(!before.objects.values().any(|object| matches!(object, Object::Stream(stream) if stream.dict.has_type(b"ObjStm"))), "lopdf loads none of the containers");
    assert_eq!(reason(&bytes), Err("a used member lopdf did not take from its named container"));
    assert_eq!(load(&bytes).unwrap_err(), AMBIGUOUS);
    // A broken /XRefStm: the walk fails, and lopdf's load (no object streams in it) is still refused from the raw bytes.
    assert_eq!(reason(&word_then_update(true)), Err("chain cannot be walked in a file with object streams"));

    let session = crate::editor::EditSession::new(bytes.clone(), 1);
    assert_eq!(session.text_extraction_permission(), Ok(()), "search, selection and OCR read PDFium's text");
    assert_eq!(session.comments_reason(), Some(AMBIGUOUS));
    let labels = crate::page_labels::PageLabelCache::default().read(1, &bytes, 1);
    assert_eq!(crate::page_labels::project(&labels, 1, 0, &session.plan).reason.as_deref(), Some(AMBIGUOUS));
}

#[test]
fn a_rebuilt_table_is_kept_only_without_object_streams() {
    for with_stream in [false, true] {
        let mut pdf = Pdf::new();
        let (o1, o2) = (pdf.obj(1, CATALOG), pdf.obj(2, PAGES));
        let mut rows = vec![(1, Row::N(o1)), (2, Row::N(o2))];
        if with_stream { rows.push((5, Row::N(pdf.objstm(5, &[(10, "<</V(A)>>")])))); }
        pdf.table(&rows, "<</Size 11/Root 1 0 R>>");
        let bytes = pdf.finish(999_999);
        assert_eq!(crate::pdf_load::load_unreconciled(&bytes).unwrap().xref_start, 0, "lopdf rebuilt the table");
        assert_eq!(reason(&bytes).is_err(), with_stream);
    }
}

#[test]
fn repeated_and_oversized_cross_reference_streams_are_charged_once_and_capped() {
    // 1,000 sections all point their /XRefStm at one 42 KB stream (42 MB if each were charged): it is parsed once, so the walk stays in budget.
    let mut pdf = Pdf::new();
    let (o1, o2) = (pdf.obj(1, CATALOG), pdf.obj(2, PAGES));
    let rows: Vec<(u32, u8, u32, u16)> = (100..100 + 6_000).map(|number| (number, 0, 0, 0)).collect();
    let xs = pdf.xref_stream(7, &rows, "");
    let mut prev = pdf.table(&[(1, Row::N(o1)), (2, Row::N(o2))], "<</Size 3/Root 1 0 R>>");
    for _ in 0..1_000 { prev = pdf.table(&[], &format!("<</Size 3/Root 1 0 R/Prev {prev}/XRefStm {xs}>>")); }
    let bytes = pdf.finish(prev);
    let started = Instant::now();
    assert!(conforming_entries(&bytes, prev).is_ok());
    assert!(started.elapsed().as_secs() < 20);

    // A stream whose encoded length alone passes the total is refused before it is copied or decoded.
    let mut pdf = Pdf::new();
    let (o1, o2) = (pdf.obj(1, CATALOG), pdf.obj(2, PAGES));
    let at = pdf.put(format!("7 0 obj\n<</Type/XRef/Size 3/W[1 4 2]/Length {}>>\nstream\n", MAX_XREF_STREAM_BYTES + 7).as_bytes());
    pdf.buf.resize(pdf.buf.len() + MAX_XREF_STREAM_BYTES + 7, 0);
    pdf.put(b"\nendstream\nendobj\n");
    let start = pdf.table(&[(1, Row::N(o1)), (2, Row::N(o2))], &format!("<</Size 3/Root 1 0 R/XRefStm {at}>>"));
    let bytes = pdf.finish(start);
    assert_eq!(conforming_entries(&bytes, start).unwrap_err(), "cross-reference streams exceed the decode budget");
}

#[test]
fn trailers_are_parsed_in_a_bounded_window_and_nearby_offsets_parse_once() {
    let mut text = b"<</Size 3/Note(".to_vec();
    text.resize(text.len() + 2 * MAX_DICTIONARY_BYTES, b'a');
    assert_eq!(dictionary(&text, 0).unwrap_err(), "unterminated string");
    // A /Prev two bytes before its own section is corrected to that same section, which is not parsed again.
    let mut pdf = Pdf::new();
    let (o1, o2) = (pdf.obj(1, CATALOG), pdf.obj(2, PAGES));
    let at = pdf.buf.len();
    pdf.table(&[(1, Row::N(o1)), (2, Row::N(o2))], &format!("<</Size 3/Root 1 0 R/Prev {:010}>>", at - 2));
    let bytes = pdf.finish(at);
    assert_eq!(correct(&bytes, at - 2), at);
    assert_eq!(conforming_entries(&bytes, at).unwrap().len(), 3);
}

#[test]
fn a_direct_entry_lopdf_never_read_is_refused() {
    let mut pdf = Pdf::new();
    let (o1, o2, o11) = (pdf.obj(1, "<</Type/Catalog/Pages 2 0 R/Uses 11 0 R>>"), pdf.obj(2, PAGES), pdf.obj(11, "<</V(direct)>>"));
    let a = pdf.objstm(5, &[(10, "<</V(A)>>")]);
    let xs = pdf.xref_stream(7, &[(10, 2, 5, 0), (11, 1, o11 as u32, 0)], "");
    let first = pdf.table(&[(1, Row::N(o1)), (2, Row::N(o2)), (5, Row::N(a))], &format!("<</Size 12/Root 1 0 R/XRefStm {xs}>>"));
    pdf.put(b"startxref\n0\n%%EOF\n");
    let o2 = pdf.obj(2, PAGES);
    let start = pdf.table(&[(2, Row::N(o2))], &format!("<</Size 12/Root 1 0 R/Prev {first}>>"));
    let bytes = pdf.finish(start);
    assert!(value(&raw(&bytes), 11).is_none());
    assert_eq!(reason(&bytes), Err("a direct entry lopdf did not read from the same place"));
    assert_eq!(load(&bytes).unwrap_err(), AMBIGUOUS);
}

#[test]
fn prev_cycles_and_section_floods_are_bounded() {
    let mut pdf = Pdf::new();
    let (o1, o2) = (pdf.obj(1, CATALOG), pdf.obj(2, PAGES));
    let first_at = pdf.buf.len();
    let second_at = first_at + pdf.clone_len_of_table(&[(1, Row::N(o1)), (2, Row::N(o2))], &format!("<</Size 3/Root 1 0 R/Prev {:010}>>", 0));
    pdf.table(&[(1, Row::N(o1)), (2, Row::N(o2))], &format!("<</Size 3/Root 1 0 R/Prev {second_at:010}>>"));
    pdf.table(&[(1, Row::N(o1)), (2, Row::N(o2))], &format!("<</Size 3/Root 1 0 R/Prev {first_at:010}>>"));
    let cycle = pdf.finish(second_at);
    let started = Instant::now();
    assert!(load(&cycle).is_ok());
    assert_eq!(conforming_entries(&cycle, second_at).unwrap().len(), 3);

    for with_stream in [false, true] {
        let mut pdf = Pdf::new();
        let (o1, o2) = (pdf.obj(1, CATALOG), pdf.obj(2, PAGES));
        let mut rows = vec![(1, Row::N(o1)), (2, Row::N(o2))];
        if with_stream { let a = pdf.objstm(5, &[(10, "<</V(A)>>")]); rows.push((5, Row::N(a))); rows.push((10, Row::F)); }
        let mut prev: Option<usize> = None;
        for _ in 0..MAX_SECTIONS + 8 {
            let trailer = match prev { Some(prev) => format!("<</Size 11/Root 1 0 R/Prev {prev}>>"), None => "<</Size 11/Root 1 0 R>>".into() };
            prev = Some(pdf.table(&rows, &trailer));
        }
        let bytes = pdf.finish(prev.unwrap());
        assert!(conforming_entries(&bytes, prev.unwrap()).is_err());
        assert_eq!(load(&bytes).err().as_deref(), with_stream.then_some(AMBIGUOUS), "with_stream={with_stream}");
    }

    let mut pdf = Pdf::new();
    let (o1, o2) = (pdf.obj(1, CATALOG), pdf.obj(2, PAGES));
    let a = pdf.objstm(5, &[(10, "<</V(A)>>")]);
    let xs = pdf.put(b"7 0 obj\n<</Type/XRef/Size 11/W[1 4 2]/Index[0 4000000000]/Length 7>>\nstream\n\x02\x00\x00\x00\x05\x00\x00\nendstream\nendobj\n");
    let start = pdf.table(&[(1, Row::N(o1)), (2, Row::N(o2)), (5, Row::N(a))], &format!("<</Size 11/Root 1 0 R/XRefStm {xs}>>"));
    let bytes = pdf.finish(start);
    assert_eq!(load(&bytes).unwrap_err(), AMBIGUOUS);
    assert!(started.elapsed().as_secs() < 20, "bounded walks took {:?}", started.elapsed());
}

impl Pdf {
    fn clone_len_of_table(&self, rows: &[(u32, Row)], trailer: &str) -> usize { let mut copy = Pdf { buf: Vec::new() }; copy.table(rows, trailer); copy.buf.len() }
}

/// A one-page file whose page lists annotation 10; `layout` adds the objects and cross-reference sections and returns startxref.
fn annotated(layout: impl FnOnce(&mut Pdf, Vec<(u32, Row)>) -> usize) -> Vec<u8> {
    let mut pdf = Pdf::new();
    let o1 = pdf.obj(1, CATALOG);
    let o2 = pdf.obj(2, "<</Type/Pages/Kids[3 0 R]/Count 1>>");
    let o3 = pdf.obj(3, "<</Type/Page/Parent 2 0 R/MediaBox[0 0 200 200]/Annots[10 0 R]>>");
    let start = layout(&mut pdf, vec![(1, Row::N(o1)), (2, Row::N(o2)), (3, Row::N(o3))]);
    pdf.finish(start)
}
fn note(text: &str) -> String { format!("<</Type/Annot/Subtype/Text/Rect[10 10 30 30]/Contents({text})>>") }
fn contents(document: &Document) -> Option<String> {
    let (_, object) = document.objects.range((10, 0)..=(10, u16::MAX)).next()?;
    Some(String::from_utf8(object.as_dict().ok()?.get(b"Contents").ok()?.as_str().ok()?.to_vec()).unwrap())
}

/// Each case names annotation 10 as the standard resolves it. Where the load is kept, it must match PDFium and the
/// standard; where it is refused, PDFium or lopdf must disagree with the standard (an edit would have to guess).
/// Runs in a child process, as `pdf_engine`'s PDFium test does, so this test binary binds PDFium only once per process.
#[test]
fn kept_loads_match_pdfium_and_refusals_are_real_disagreements() {
    use pdfium_render::prelude::*;
    const CHILD: &str = "SM3_XREF_PDFIUM_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "xref_resolve::tests::kept_loads_match_pdfium_and_refusals_are_real_disagreements", "--nocapture", "--test-threads=1"]).env(CHILD, "1").output().unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(output.status.success() && stdout.contains("xref-pdfium-agreement-ok"), "child failed:
{stdout}
{}", String::from_utf8_lossy(&output.stderr));
        return;
    }
    let cases: Vec<(&str, Option<&str>, Vec<u8>)> = vec![
        ("same-section hybrid, two containers", Some("B"), annotated(|pdf, mut rows| {
            let (a, b) = (pdf.objstm(5, &[(10, note("A").as_str())]), pdf.objstm(6, &[(10, note("B").as_str())]));
            let xs = pdf.xref_stream(7, &[(10, 2, 6, 0)], "");
            rows.extend([(5, Row::N(a)), (6, Row::N(b)), (10, Row::F)]);
            pdf.table(&rows, &format!("<</Size 11/Root 1 0 R/XRefStm {xs}>>"))
        })),
        ("same-section hybrid, one container, no table row", Some("B"), annotated(|pdf, mut rows| {
            let b = pdf.objstm(6, &[(10, note("B").as_str())]);
            let xs = pdf.xref_stream(7, &[(10, 2, 6, 0)], "");
            rows.push((6, Row::N(b)));
            pdf.table(&rows, &format!("<</Size 11/Root 1 0 R/XRefStm {xs}>>"))
        })),
        ("word style: empty update section carries /XRefStm", Some("B"), annotated(|pdf, mut rows| {
            let (a, b) = (pdf.objstm(5, &[(10, note("A").as_str())]), pdf.objstm(6, &[(10, note("B").as_str())]));
            rows.extend([(5, Row::N(a)), (6, Row::N(b)), (10, Row::F)]);
            let first = pdf.table(&rows, "<</Size 11/Root 1 0 R>>");
            pdf.put(b"startxref\n0\n%%EOF\n");
            let xs = pdf.xref_stream(7, &[(10, 2, 6, 0)], "");
            pdf.table(&[], &format!("<</Size 11/Root 1 0 R/Prev {first}/XRefStm {xs}>>"))
        })),
        ("word file with a later update: containers listed only in the older /XRefStm", Some("B"), annotated(|pdf, mut rows| {
            let b = pdf.objstm(6, &[(10, note("B").as_str())]);
            rows.extend([(6, Row::F), (10, Row::F)]);
            let main = pdf.table(&rows, "<</Size 11/Root 1 0 R>>");
            pdf.put(b"startxref
0
%%EOF
");
            let xs = pdf.xref_stream(7, &[(6, 1, b as u32, 0), (10, 2, 6, 0)], "");
            let word = pdf.table(&[], &format!("<</Size 11/Root 1 0 R/Prev {main}/XRefStm {xs}>>"));
            pdf.put(b"startxref
0
%%EOF
");
            let o1 = pdf.obj(1, CATALOG);
            pdf.table(&[(1, Row::N(o1))], &format!("<</Size 11/Root 1 0 R/Prev {word}>>"))
        })),
        ("older section /XRefStm", Some("B"), annotated(|pdf, mut rows| {
            let (a, b) = (pdf.objstm(5, &[(10, note("A").as_str())]), pdf.objstm(6, &[(10, note("B").as_str())]));
            let xs = pdf.xref_stream(7, &[(10, 2, 6, 0)], "");
            rows.extend([(5, Row::N(a)), (6, Row::N(b)), (10, Row::F)]);
            let first = pdf.table(&rows, &format!("<</Size 11/Root 1 0 R/XRefStm {xs}>>"));
            pdf.put(b"startxref\n0\n%%EOF\n");
            let o1 = pdf.obj(1, CATALOG);
            pdf.table(&[(1, Row::N(o1))], &format!("<</Size 11/Root 1 0 R/Prev {first}>>"))
        })),
        ("freed by a newer revision", None, annotated(|pdf, mut rows| {
            rows.push((10, Row::N(pdf.obj(10, note("old").as_str()))));
            let first = pdf.table(&rows, "<</Size 11/Root 1 0 R>>");
            pdf.put(b"startxref\n0\n%%EOF\n");
            pdf.table(&[(10, Row::F)], &format!("<</Size 11/Root 1 0 R/Prev {first}>>"))
        })),
        ("object stream member with a free entry", None, annotated(|pdf, mut rows| {
            rows.extend([(5, Row::N(pdf.objstm(5, &[(10, note("stream").as_str())]))), (10, Row::F)]);
            pdf.table(&rows, "<</Size 11/Root 1 0 R>>")
        })),
        ("object stream member with no entry", None, annotated(|pdf, mut rows| {
            rows.push((5, Row::N(pdf.objstm(5, &[(10, note("stream").as_str())]))));
            pdf.table(&rows, "<</Size 11/Root 1 0 R>>")
        })),
    ];
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let pdfium = Pdfium::new(Pdfium::bind_to_library(root.join("resources/pdfium/bin/pdfium.dll")).unwrap());
    let mut kept = 0;
    for (case, standard, bytes) in cases {
        let shown = {
            let document = pdfium.load_pdf_from_byte_vec(bytes.clone(), None).unwrap();
            let page = document.pages().get(0).unwrap();
            let annotations = page.annotations();
            annotations.iter().next().and_then(|annotation| annotation.contents())
        };
        match load(&bytes) {
            Ok(document) => {
                kept += 1;
                assert_eq!(contents(&document), shown, "{case}: kept; lopdf before reconcile read {:?}", contents(&raw(&bytes)));
                assert_eq!(shown.as_deref(), standard, "{case}");
            }
            Err(error) => {
                assert_eq!(error, AMBIGUOUS, "{case}");
                let lopdf = contents(&raw(&bytes));
                assert!(shown.as_deref() != standard || lopdf.as_deref() != standard, "{case}: refused although PDFium and lopdf both follow the standard");
                println!("refused {case}: standard {standard:?}, PDFium {shown:?}, lopdf {lopdf:?}");
            }
        }
    }
    assert_eq!(kept, 3);
    println!("xref-pdfium-agreement-ok");
}

#[test]
fn trailer_dictionaries_parse_the_syntax_writers_use() {
    let text = b"<</Size 12/Root 1 0 R/Info 3 0 R/ID[<0A1b2C><3d4E>]/Prev 116/XRefStm 9/Note(a (nested) \\) string)/N#61me true/R -1.5/Z null/D<</A[1 2[3]]>>>>";
    let (Object::Dictionary(dict), end) = object(text, 0, 0).unwrap() else { panic!("not a dictionary") };
    assert_eq!(end, text.len());
    assert_eq!(int(&dict, b"Prev"), Some(116));
    assert_eq!(int(&dict, b"XRefStm"), Some(9));
    assert_eq!(dict.get(b"Root").unwrap(), &Object::Reference((1, 0)));
    assert!(dict.has(b"Name"));
    let deep = format!("{}{}", "[".repeat(MAX_DEPTH + 2), "]".repeat(MAX_DEPTH + 2));
    assert!(object(deep.as_bytes(), 0, 0).is_err());
}

#[test]
fn repo_fixtures_load_unchanged() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests").join("fixtures");
    let mut checked = 0;
    for entry in std::fs::read_dir(&dir).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("pdf") { continue; }
        let bytes = std::fs::read(&path).unwrap();
        let Ok(before) = Document::load_mem(&bytes) else { continue };
        if before.trailer.get(b"Encrypt").is_ok() { continue; }
        let after = load(&bytes).unwrap_or_else(|error| panic!("{}: {error}", path.display()));
        assert_eq!(before.objects, after.objects, "{}", path.display());
        checked += 1;
    }
    assert!(checked > 0);
}

/// Read-only measurement over a folder of real PDFs: `SM3_XREF_PROBE_DIR=<folder> cargo test --bins xref_probe -- --ignored --nocapture`.
/// Prints counts only, never file names.
#[test]
#[ignore]
fn xref_probe_real_files() {
    let Ok(root) = std::env::var("SM3_XREF_PROBE_DIR") else { return };
    let mut pending = vec![std::path::PathBuf::from(root)];
    let (mut files, mut loaded, mut with_streams, mut hybrid, mut refused, mut changed, mut skipped_walk) = (0, 0, 0, 0, 0, 0, 0);
    let (mut removed, mut replaced, mut removed_live, mut removed_free) = (0usize, 0usize, 0usize, 0usize);
    let mut reasons = BTreeMap::new();
    let started = Instant::now();
    while let Some(dir) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() { pending.push(path); continue; }
            if !path.extension().and_then(|ext| ext.to_str()).is_some_and(|ext| ext.eq_ignore_ascii_case("pdf")) { continue; }
            files += 1;
            let Ok(bytes) = std::fs::read(&path) else { continue };
            if bytes.len() > 512 * 1024 * 1024 { continue; }
            let options = LoadOptions { max_decompressed_size: Some(MAX_LOAD_STREAM_DECODE_BYTES), ..Default::default() };
            let Ok(before) = Document::load_mem_with_options(&bytes, options) else { continue };
            loaded += 1;
            let streams = before.objects.values().any(|object| matches!(object, Object::Stream(stream) if stream.dict.has_type(b"ObjStm")));
            if streams { with_streams += 1; }
            if bytes.windows(8).any(|window| window == b"/XRefStm") { hybrid += 1; }
            let header = bytes.windows(5).position(|window| window == b"%PDF-").unwrap_or(0);
            let walk = if before.xref_start != 0 { conforming_entries(&bytes[header..], before.xref_start) } else { Err("lopdf reconstructed") };
            if let Err(reason) = &walk { skipped_walk += 1; *reasons.entry(*reason).or_insert(0usize) += 1; }
            let mut after = before.clone();
            match reconcile_with_reason(&mut after, &bytes) {
                Err(reason) => { refused += 1; println!("refused: {reason}; object streams {streams}"); }
                Ok(()) if after.objects != before.objects => {
                    changed += 1;
                    let live = crate::pdf_load::reachable(&before);
                    let entries = walk.as_ref().unwrap();
                    for id in before.objects.keys().filter(|id| !after.objects.contains_key(id)) {
                        removed += 1;
                        if live.contains(id) { removed_live += 1; }
                        if matches!(entries.get(&id.0), Some(Entry::Free)) { removed_free += 1; }
                    }
                    replaced += after.objects.iter().filter(|(id, object)| before.objects.get(id).is_some_and(|old| old != *object)).count();
                }
                Ok(()) => {}
            }
        }
    }
    println!("xref probe: {files} files, {loaded} loaded by lopdf, {with_streams} with object streams, {hybrid} with /XRefStm, {skipped_walk} chain not walkable, {refused} refused, {changed} changed ({removed} objects removed, {replaced} replaced; {removed_live} of the removed reachable, {removed_free} with a free entry), {:?}", started.elapsed());
    println!("xref probe walk failures: {reasons:?}");
}
