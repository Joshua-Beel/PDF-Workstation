//! Read-only survey of every annotation in a PDF, for listing annotations the app does not edit
//! (annotation interchange design, phase 1, sections 2.1 and 5). It never fails on an annotation: a field
//! that is missing, malformed or over a limit is left out. It never resolves actions, appearance streams,
//! rich text or embedded files. Every string it returns is plain text with control and bidi-override
//! characters removed; the frontend shows it as text only.
use lopdf::{Dictionary, Document, Object};
use serde::Serialize;

/// Annotations listed per page and per document; the rest are counted in `truncated`.
pub const MAX_LISTED_PER_PAGE: usize = 4_096;
pub const MAX_LISTED: usize = 16_384;
/// Body characters kept per annotation for the list, and text bytes (bodies and labels) kept per document.
/// This bounds the list sent to the frontend; paging the list is later phase 1 work.
pub const MAX_CONTENTS_CHARS: usize = 16_384;
pub const MAX_TOTAL_TEXT_BYTES: usize = 4 * 1024 * 1024;
pub const MAX_LABEL_CHARS: usize = 256;
/// `/Annots` entries examined per page and per document, listed or not. A shared `/Annots` array or
/// annotation object is surveyed once.
pub const MAX_ENTRIES_PER_PAGE: usize = 65_536;
pub const MAX_ENTRIES_TOTAL: usize = 262_144;

const LISTED: &[&str] = &["Text", "FreeText", "Line", "Square", "Circle", "Polygon", "PolyLine", "Highlight", "Underline", "Squiggly", "StrikeOut", "Stamp", "Caret", "Ink", "FileAttachment", "Sound", "Movie", "Screen", "Redact", "RichMedia", "3D", "Watermark", "Projection"];
/// Not comments in Acrobat's list: form fields, links, popups and print marks.
const NOT_LISTED: &[&str] = &["Widget", "Link", "Popup", "PrinterMark", "TrapNet"];

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Surveyed {
    /// One of the standard subtype names, or "Other".
    pub subtype: &'static str,
    pub author: Option<String>,
    pub subject: Option<String>,
    pub contents: Option<String>,
    /// `/M` (or `/CreationDate`) as `YYYY-MM-DD HH:MM`, when it parses as a PDF date.
    pub modified: Option<String>,
    /// Has `/IRT`: a reply to another annotation.
    pub reply: bool,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Survey { pub pages: Vec<Vec<Surveyed>>, pub truncated: usize }

fn name(dict: &Dictionary, key: &[u8]) -> Option<Vec<u8>> { dict.get(key).ok().and_then(|value| value.as_name().ok()).map(<[u8]>::to_vec) }

/// PDFDocEncoding (ISO 32000-1 Annex D): bytes 0x18-0x1F, 0x80-0x9E and 0xA0 (euro) differ from Latin-1;
/// 0x7F, 0x9F and 0xAD are undefined.
fn pdfdoc(byte: u8) -> Option<char> {
    const LOW: [char; 8] = ['\u{02D8}', '\u{02C7}', '\u{02C6}', '\u{02D9}', '\u{02DD}', '\u{02DB}', '\u{02DA}', '\u{02DC}'];
    const HIGH: [char; 31] = ['\u{2022}', '\u{2020}', '\u{2021}', '\u{2026}', '\u{2014}', '\u{2013}', '\u{0192}', '\u{2044}', '\u{2039}', '\u{203A}', '\u{2212}', '\u{2030}', '\u{201E}', '\u{201C}', '\u{201D}', '\u{2018}', '\u{2019}', '\u{201A}', '\u{2122}', '\u{FB01}', '\u{FB02}', '\u{0141}', '\u{0152}', '\u{0160}', '\u{0178}', '\u{017D}', '\u{0131}', '\u{0142}', '\u{0153}', '\u{0161}', '\u{017E}'];
    match byte {
        0x18..=0x1F => Some(LOW[usize::from(byte - 0x18)]),
        0x80..=0x9E => Some(HIGH[usize::from(byte - 0x80)]),
        0xA0 => Some('\u{20AC}'),
        0x7F | 0x9F | 0xAD => None,
        byte => Some(char::from(byte)),
    }
}

/// Decodes a PDF text string (UTF-16BE or UTF-8 with BOM, else PDFDocEncoding) into at most `limit`
/// characters, dropping C0/C1 controls (newline and tab survive when `multiline`), bidi controls and
/// zero-width characters; line and paragraph separators become newlines in bodies and are dropped from
/// labels. At most `4 * limit` raw bytes after the BOM are examined, whatever is kept, so a huge or
/// control-padded string costs no more than a short one.
pub fn text(bytes: &[u8], limit: usize, multiline: bool) -> String {
    let raw_cap = limit.saturating_mul(4);
    let mut out = Output { text: String::new(), count: 0, limit, multiline, previous_cr: false };
    if let Some(rest) = bytes.strip_prefix(&[0xFE, 0xFF]) {
        let rest = &rest[..rest.len().min(raw_cap)];
        out.extend(char::decode_utf16(rest.chunks_exact(2).map(|pair| u16::from_be_bytes([pair[0], pair[1]]))).map(|c| c.unwrap_or('\u{FFFD}')));
    } else if let Some(rest) = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]) {
        out.extend(String::from_utf8_lossy(&rest[..rest.len().min(raw_cap)]).chars());
    } else {
        out.extend(bytes[..bytes.len().min(raw_cap)].iter().filter_map(|byte| pdfdoc(*byte)));
    }
    out.text.trim().to_owned()
}

struct Output { text: String, count: usize, limit: usize, multiline: bool, previous_cr: bool }
impl Output {
    fn extend(&mut self, chars: impl Iterator<Item = char>) {
        for c in chars {
            if self.count >= self.limit { break; }
            if self.multiline && c == '\n' && self.previous_cr { self.previous_cr = false; continue; }
            self.previous_cr = c == '\r';
            let c = match c { '\r' | '\u{2028}' | '\u{2029}' if self.multiline => '\n', c => c };
            let invisible = matches!(c, '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}' | '\u{200B}'..='\u{200F}' | '\u{2060}'..='\u{2064}' | '\u{061C}' | '\u{FEFF}' | '\u{2028}' | '\u{2029}');
            let control = c.is_control() && !(self.multiline && (c == '\n' || c == '\t'));
            if !invisible && !control { self.text.push(c); self.count += 1; }
        }
    }
}

/// Decoded indirect strings, so a string object shared by many annotations is decoded once. With the
/// raw-byte cap in `text`, total decoding work stays within the size of the file.
type Decoded = std::collections::HashMap<(lopdf::ObjectId, usize, bool), Option<String>>;

fn string(document: &Document, dict: &Dictionary, key: &[u8], limit: usize, multiline: bool, cache: &mut Decoded) -> Option<String> {
    let decode = |value: &Object| value.as_str().ok().map(|bytes| text(bytes, limit, multiline)).filter(|value| !value.is_empty());
    match dict.get(key).ok()? {
        Object::Reference(id) => cache.entry((*id, limit, multiline)).or_insert_with(|| document.get_object(*id).ok().and_then(decode)).clone(),
        other => decode(other),
    }
}

/// `D:YYYYMMDDHHmm...` to `YYYY-MM-DD HH:MM` (local time as written; the offset is not applied).
fn date(document: &Document, dict: &Dictionary, cache: &mut Decoded) -> Option<String> {
    let raw = [b"M".as_slice(), b"CreationDate"].iter().find_map(|key| string(document, dict, key, 64, false, cache))?;
    let digits = raw.strip_prefix("D:").unwrap_or(&raw);
    let field = |range: std::ops::Range<usize>| digits.get(range).filter(|part| part.bytes().all(|b| b.is_ascii_digit())).and_then(|part| part.parse::<u32>().ok());
    let (year, month, day) = (field(0..4)?, field(4..6).unwrap_or(1), field(6..8).unwrap_or(1));
    let (hour, minute) = (field(8..10).unwrap_or(0), field(10..12).unwrap_or(0));
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) || hour > 23 || minute > 59 { return None; }
    Some(format!("{year:04}-{month:02}-{day:02} {hour:02}:{minute:02}"))
}

/// The survey of every page, in `get_pages` order, leaving out the annotation objects in `skip` (the app's
/// own editable annotations, listed separately). Bounded by the limits above; never fails.
pub fn survey(document: &Document, skip: &std::collections::HashSet<lopdf::ObjectId>) -> Survey {
    let mut result = Survey::default();
    let (mut listed, mut text_bytes, mut examined) = (0usize, 0usize, 0usize);
    let (mut seen_arrays, mut seen_annotations) = (std::collections::HashSet::new(), std::collections::HashSet::new());
    let mut cache = Decoded::new();
    for (_, page_id) in document.get_pages() {
        let mut page_list = Vec::new();
        let annots = document.get_dictionary(page_id).ok().and_then(|page| page.get(b"Annots").ok()).and_then(|value| match value {
            Object::Reference(id) if seen_arrays.insert(*id) => document.get_object(*id).ok().and_then(|value| value.as_array().ok()),
            Object::Reference(_) => None,
            other => other.as_array().ok(),
        });
        let entries = annots.map(|annots| annots.as_slice()).unwrap_or(&[]);
        let budget = MAX_ENTRIES_PER_PAGE.min(MAX_ENTRIES_TOTAL - examined);
        if entries.len() > budget { result.truncated += entries.len() - budget; }
        examined += entries.len().min(budget);
        for entry in &entries[..entries.len().min(budget)] {
            let dict = match entry { Object::Reference(id) if !skip.contains(id) && seen_annotations.insert(*id) => document.get_dictionary(*id).ok(), Object::Reference(_) => None, Object::Dictionary(dict) => Some(dict), _ => None };
            let Some(dict) = dict else { continue };
            let subtype_name = name(dict, b"Subtype");
            if subtype_name.as_deref().is_some_and(|subtype| NOT_LISTED.iter().any(|skip| skip.as_bytes() == subtype)) { continue; }
            let flags = dict.get(b"F").ok().and_then(|value| value.as_i64().ok()).unwrap_or(0);
            let state = dict.has(b"State") || dict.has(b"StateModel");
            if state && dict.has(b"IRT") { continue; }
            if flags & 2 != 0 && !dict.has(b"IRT") { continue; }
            if page_list.len() >= MAX_LISTED_PER_PAGE || listed >= MAX_LISTED { result.truncated += 1; continue; }
            let subtype = subtype_name.as_deref().and_then(|subtype| LISTED.iter().copied().find(|known| known.as_bytes() == subtype)).unwrap_or("Other");
            let mut within_budget = |key: &[u8], limit: usize, multiline: bool| {
                let value = (text_bytes < MAX_TOTAL_TEXT_BYTES).then(|| string(document, dict, key, limit, multiline, &mut cache)).flatten();
                text_bytes += value.as_ref().map_or(0, String::len);
                value
            };
            let (author, subject) = (within_budget(b"T", MAX_LABEL_CHARS, false), within_budget(b"Subj", MAX_LABEL_CHARS, false));
            let contents = within_budget(b"Contents", MAX_CONTENTS_CHARS, true);
            let modified = date(document, dict, &mut cache);
            page_list.push(Surveyed {
                subtype,
                author,
                subject,
                contents,
                modified,
                reply: dict.has(b"IRT"),
            });
            listed += 1;
        }
        result.pages.push(page_list);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use lopdf::{dictionary, Stream, StringFormat};

    fn utf16(value: &str) -> Object { let mut bytes = vec![0xFE, 0xFF]; for unit in value.encode_utf16() { bytes.extend(unit.to_be_bytes()); } Object::String(bytes, StringFormat::Hexadecimal) }
    fn literal(value: &[u8]) -> Object { Object::String(value.to_vec(), StringFormat::Literal) }
    fn survey_all(document: &Document) -> Survey { survey(document, &std::collections::HashSet::new()) }

    fn document(annots: impl FnOnce(&mut Document, lopdf::ObjectId) -> Object) -> Document {
        let mut document = Document::with_version("1.7");
        let pages = document.new_object_id();
        let page = document.new_object_id();
        let contents = document.add_object(Stream::new(dictionary! {}, Vec::new()));
        let annots = annots(&mut document, page);
        document.objects.insert(page, Object::Dictionary(dictionary! { "Type" => "Page", "Parent" => pages, "MediaBox" => vec![0.into(), 0.into(), 200.into(), 200.into()], "Contents" => contents, "Annots" => annots }));
        document.objects.insert(pages, Object::Dictionary(dictionary! { "Type" => "Pages", "Kids" => vec![page.into()], "Count" => 1 }));
        let catalog = document.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages });
        document.trailer.set("Root", catalog);
        document
    }

    #[test]
    fn surveys_acrobat_style_comments_replies_and_skips_links_widgets_popups_states_and_hidden() {
        let survey = survey_all(&document(|document, page| {
            let note = document.add_object(dictionary! { "Type" => "Annot", "Subtype" => "Text", "Rect" => vec![10.into(), 10.into(), 30.into(), 30.into()], "P" => page,
                "T" => utf16("Reviewer \u{202E}evil"), "Subj" => literal(b"Sticky Note"), "Contents" => literal(b"Line one\rLine two \x8dquoted\x8e \x93\xa0"), "M" => literal(b"D:20240305141500-06'00'"), "NM" => literal(b"0b2c") });
            let reply = document.add_object(dictionary! { "Type" => "Annot", "Subtype" => "Text", "Rect" => vec![10.into(), 10.into(), 30.into(), 30.into()], "IRT" => note, "T" => literal(b"Other"), "Contents" => utf16("Reply \u{1F600}"), "F" => 30 });
            let state = document.add_object(dictionary! { "Type" => "Annot", "Subtype" => "Text", "IRT" => note, "State" => literal(b"Accepted"), "StateModel" => literal(b"Review"), "F" => 30 });
            let popup = document.add_object(dictionary! { "Type" => "Annot", "Subtype" => "Popup", "Parent" => note });
            let link = document.add_object(dictionary! { "Type" => "Annot", "Subtype" => "Link", "A" => dictionary! { "S" => "URI", "URI" => literal(b"https://example.invalid") } });
            let widget = document.add_object(dictionary! { "Type" => "Annot", "Subtype" => "Widget", "FT" => "Tx" });
            let hidden = document.add_object(dictionary! { "Type" => "Annot", "Subtype" => "Square", "F" => 2 });
            let ink = dictionary! { "Type" => "Annot", "Subtype" => "Ink", "InkList" => vec![Object::Array(vec![0.into(), 0.into(), 5.into(), 5.into()])], "CreationDate" => literal(b"D:2023") };
            let odd = document.add_object(dictionary! { "Type" => "Annot", "Subtype" => "SomethingNew", "Contents" => literal(b"\x01\x02x") });
            Object::Array(vec![note.into(), popup.into(), reply.into(), state.into(), link.into(), widget.into(), hidden.into(), Object::Dictionary(ink), Object::Null, Object::Integer(3), odd.into()])
        }));
        assert_eq!(survey.truncated, 0);
        let page = &survey.pages[0];
        assert_eq!(page.iter().map(|item| item.subtype).collect::<Vec<_>>(), ["Text", "Text", "Ink", "Other"]);
        assert_eq!(page[0].author.as_deref(), Some("Reviewer evil"), "bidi override removed");
        assert_eq!(page[0].subject.as_deref(), Some("Sticky Note"));
        assert_eq!(page[0].contents.as_deref(), Some("Line one\nLine two \u{201C}quoted\u{201D} \u{FB01}\u{20AC}"), "PDFDocEncoding quotes, ligature and euro (not Windows-1252), CR as newline");
        assert_eq!(page[0].modified.as_deref(), Some("2024-03-05 14:15"));
        assert!(!page[0].reply && page[1].reply);
        assert_eq!(page[1].contents.as_deref(), Some("Reply \u{1F600}"));
        assert_eq!(page[2].modified.as_deref(), Some("2023-01-01 00:00"));
        assert_eq!(page[3].contents.as_deref(), Some("x"), "controls removed");
    }

    #[test]
    fn survey_reads_an_indirect_annots_array_and_bounds_what_it_lists() {
        let survey = survey_all(&document(|document, _| {
            let items: Vec<Object> = (0..MAX_LISTED_PER_PAGE + 3).map(|_| Object::Dictionary(dictionary! { "Subtype" => "Square", "Contents" => literal(&vec![b'a'; MAX_CONTENTS_CHARS + 10]) })).collect();
            document.add_object(Object::Array(items)).into()
        }));
        assert_eq!(survey.pages[0].len(), MAX_LISTED_PER_PAGE);
        assert_eq!(survey.truncated, 3);
        assert_eq!(survey.pages[0][0].contents.as_ref().unwrap().chars().count(), MAX_CONTENTS_CHARS);
    }

    /// Two pages. Page 1 has an indirect `/Annots` array with an Acrobat-style sticky note, its popup and a
    /// reply, a link, an inline square and a null entry. Page 2 has another app's highlight.
    fn acrobat_style() -> (Document, Vec<lopdf::ObjectId>) {
        let mut document = Document::with_version("1.7");
        let pages = document.new_object_id();
        let (first, second) = (document.new_object_id(), document.new_object_id());
        let contents = document.add_object(Stream::new(dictionary! {}, Vec::new()));
        let note = document.new_object_id();
        let popup = document.add_object(dictionary! { "Type" => "Annot", "Subtype" => "Popup", "Parent" => note, "Rect" => vec![120.into(), 100.into(), 190.into(), 160.into()], "Open" => false, "F" => 28 });
        document.objects.insert(note, Object::Dictionary(dictionary! { "Type" => "Annot", "Subtype" => "Text", "Rect" => vec![10.into(), 170.into(), 30.into(), 190.into()], "P" => first, "NM" => literal(b"5c1f0a52-1b7e-4c33"), "T" => literal(b"Reviewer"), "Subj" => literal(b"Sticky Note"), "Contents" => literal(b"Check this"), "M" => literal(b"D:20240305141500-06'00'"), "C" => vec![1.into(), 1.into(), 0.into()], "Name" => "Comment", "F" => 28, "Popup" => popup }));
        let reply = document.add_object(dictionary! { "Type" => "Annot", "Subtype" => "Text", "Rect" => vec![10.into(), 170.into(), 30.into(), 190.into()], "P" => first, "IRT" => note, "T" => literal(b"Second"), "Contents" => literal(b"Agreed"), "F" => 30 });
        let link = document.add_object(dictionary! { "Type" => "Annot", "Subtype" => "Link", "Rect" => vec![10.into(), 10.into(), 60.into(), 20.into()], "A" => dictionary! { "S" => "URI", "URI" => literal(b"https://example.invalid") } });
        let square = Object::Dictionary(dictionary! { "Type" => "Annot", "Subtype" => "Square", "Rect" => vec![50.into(), 50.into(), 90.into(), 90.into()], "T" => literal(b"Inline") });
        let annots = document.add_object(Object::Array(vec![note.into(), popup.into(), reply.into(), link.into(), square, Object::Null]));
        let highlight = document.add_object(dictionary! { "Type" => "Annot", "Subtype" => "Highlight", "Rect" => vec![10.into(), 10.into(), 60.into(), 20.into()], "QuadPoints" => vec![10.into(), 20.into(), 60.into(), 20.into(), 10.into(), 10.into(), 60.into(), 10.into()], "T" => literal(b"Other app"), "C" => vec![1.into(), 1.into(), 0.into()] });
        for (page, annots) in [(first, Object::Reference(annots)), (second, Object::Array(vec![highlight.into()]))] {
            document.objects.insert(page, Object::Dictionary(dictionary! { "Type" => "Page", "Parent" => pages, "MediaBox" => vec![0.into(), 0.into(), 200.into(), 200.into()], "Contents" => contents, "Annots" => annots }));
        }
        document.objects.insert(pages, Object::Dictionary(dictionary! { "Type" => "Pages", "Kids" => vec![first.into(), second.into()], "Count" => 2 }));
        let catalog = document.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages });
        document.trailer.set("Root", catalog);
        (document, vec![note, popup, reply, link, highlight])
    }

    fn fixture(name: &str) -> std::path::PathBuf { std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/annotations").join(name) }

    /// Writes the committed interchange fixtures that the pdf.js check (`scripts/pdfjs-annotations.test.ts`)
    /// reads: `app-annotations.pdf` (a sticky note with its popup, an area highlight and a two-quad text highlight,
    /// all written by the app) and `foreign-plus-app.pdf` (the Acrobat-style two-page file of these tests with
    /// one app note added). Synthetic content only. Run when the writer's format changes:
    /// `SM3_WRITE_FIXTURES=1 cargo test --bins write_interchange_fixtures -- --ignored`.
    #[test]
    #[ignore = "writes the committed fixtures: set SM3_WRITE_FIXTURES=1"]
    fn write_interchange_fixtures() {
        if std::env::var_os("SM3_WRITE_FIXTURES").is_none() { return; }
        std::fs::create_dir_all(fixture("")).unwrap();
        let mut blank = document(|_, _| Object::Array(Vec::new()));
        blank.get_dictionary_mut(blank.get_pages()[&1]).unwrap().remove(b"Annots");
        let mut bytes = Vec::new(); blank.save_to(&mut bytes).unwrap();
        let mut session = crate::editor::EditSession::new(bytes, 1);
        let next = session.proposed_comment(Some((0, crate::editor::CropBox { left: 20.0, bottom: 150.0, right: 40.0, top: 170.0 })), None, Some("App note")).unwrap(); session.commit_comments(next);
        let next = session.proposed_highlight(Some((0, crate::editor::CropBox { left: 60.0, bottom: 100.0, right: 140.0, top: 130.0 })), None, Some("Area"), false).unwrap(); session.commit_comments(next);
        let quads = vec![crate::editor::CropBox { left: 20.0, bottom: 60.0, right: 120.0, top: 72.0 }, crate::editor::CropBox { left: 20.0, bottom: 40.0, right: 80.0, top: 52.0 }];
        let next = session.proposed_text_highlight(0, quads, Some("Text")).unwrap(); session.commit_comments(next);
        std::fs::write(fixture("app-annotations.pdf"), session.export(None).unwrap()).unwrap();
        let (source, _) = acrobat_style();
        let mut bytes = Vec::new(); source.clone().save_to(&mut bytes).unwrap();
        let mut session = crate::editor::EditSession::new(bytes, 2);
        let next = session.proposed_comment(Some((0, crate::editor::CropBox { left: 100.0, bottom: 30.0, right: 130.0, top: 60.0 })), None, Some("Mine")).unwrap(); session.commit_comments(next);
        std::fs::write(fixture("foreign-plus-app.pdf"), session.export(None).unwrap()).unwrap();
    }

    /// The committed fixtures still match the writer's format: everything the app wrote reads back as its own,
    /// and every foreign entry is kept. If this fails after a deliberate format change, regenerate them.
    #[test]
    fn committed_interchange_fixtures_read_back_as_written() {
        let app = Document::load_mem(&std::fs::read(fixture("app-annotations.pdf")).unwrap()).unwrap();
        let owned = crate::comments::read_owned(&app).unwrap();
        assert_eq!((owned.app.len(), owned.popups.len(), owned.foreign), (3, 1, 0));
        assert_eq!(owned.notes[0].iter().map(|note| note.contents.as_str()).collect::<Vec<_>>(), ["App note", "Area", "Text"]);
        assert!(owned.notes[0].iter().all(|note| crate::comments::is_uuid(&note.id) && note.created.is_some()));
        let mixed = Document::load_mem(&std::fs::read(fixture("foreign-plus-app.pdf")).unwrap()).unwrap();
        let owned = crate::comments::read_owned(&mixed).unwrap();
        assert_eq!((owned.app.len(), owned.popups.len(), owned.foreign), (1, 1, 7), "page 1: note, popup, reply, link, inline square, null; page 2: highlight");
        assert_eq!(survey_all(&mixed).pages.iter().map(Vec::len).sum::<usize>(), 5, "the foreign note, reply, square and highlight, plus the app note");
    }

    fn entries(document: &Document, page: usize) -> Vec<Object> {
        let id = document.get_pages().into_values().nth(page).unwrap();
        match document.get_dictionary(id).unwrap().get(b"Annots") {
            Ok(Object::Reference(array)) => document.get_object(*array).unwrap().as_array().unwrap().clone(),
            Ok(Object::Array(values)) => values.clone(),
            _ => Vec::new(),
        }
    }

    #[test]
    fn app_comments_work_alongside_other_apps_annotations_which_are_kept_untouched() {
        let (source, foreign) = acrobat_style();
        let mut bytes = Vec::new(); source.clone().save_to(&mut bytes).unwrap();
        let source = Document::load_mem(&bytes).unwrap();
        let mut session = crate::editor::EditSession::new(bytes.clone(), 2);
        assert!(session.comments_reason().is_none(), "other apps' annotations no longer refuse commenting");
        let listed = session.view_only_annotations().unwrap();
        assert_eq!(listed.pages.iter().map(|page| page.iter().map(|item| (item.subtype, item.author.as_deref())).collect::<Vec<_>>()).collect::<Vec<_>>(), vec![vec![("Text", Some("Reviewer")), ("Text", Some("Second")), ("Square", Some("Inline"))], vec![("Highlight", Some("Other app"))]]);
        // Add a note on page 1 and save: every foreign entry stays, in order, objects unchanged; page 2 is untouched.
        let rect = crate::editor::CropBox { left: 100.0, bottom: 30.0, right: 130.0, top: 60.0 };
        let next = session.proposed_comment(Some((0, rect)), None, Some("Mine")).unwrap(); session.commit_comments(next);
        assert!(!session.removes_saved_annotation(), "adding never needs a rewrite");
        let saved = Document::load_mem(&session.export(None).unwrap()).unwrap();
        for id in &foreign { assert_eq!(saved.get_object(*id).unwrap(), source.get_object(*id).unwrap(), "foreign object {id:?} unchanged"); }
        let (before, after) = (entries(&source, 0), entries(&saved, 0));
        assert_eq!(&after[..before.len()], &before[..], "foreign entries keep their order; the inline square and null stay");
        assert_eq!(after.len(), before.len() + 2, "the new note and its popup");
        assert!(matches!(saved.get_dictionary(saved.get_pages()[&1]).unwrap().get(b"Annots").unwrap(), Object::Array(_)), "a touched page gets a direct array");
        assert_eq!(saved.get_dictionary(saved.get_pages()[&2]).unwrap().get(b"Annots").unwrap(), source.get_dictionary(source.get_pages()[&2]).unwrap().get(b"Annots").unwrap(), "an untouched page keeps its /Annots as it was");
        assert_eq!(survey_all(&saved).pages[1], survey_all(&source).pages[1]);
        // Reopen: the note is editable, the rest view only; edit then delete it; foreign entries remain exactly.
        let mut saved_bytes = Vec::new(); saved.clone().save_to(&mut saved_bytes).unwrap();
        let mut reopened = crate::editor::EditSession::new(saved_bytes, 2);
        assert_eq!(reopened.plan[0].notes.len(), 1);
        let mine = reopened.plan[0].notes[0].id.clone();
        let next = reopened.proposed_comment(None, Some(&mine), Some("Edited")).unwrap(); reopened.commit_comments(next);
        assert!(reopened.removes_saved_annotation(), "an edited saved note is rewritten in place (SM2 §11 rewrite rule)");
        let edited = Document::load_mem(&reopened.export(None).unwrap()).unwrap();
        assert_eq!(&entries(&edited, 0)[..before.len()], &before[..]);
        assert_eq!(crate::comments::read(&edited).unwrap()[0][0].contents, "Edited");
        let next = reopened.proposed_comment(None, Some(&mine), None).unwrap(); reopened.commit_comments(next);
        assert!(reopened.removes_saved_annotation());
        let deleted = Document::load_mem(&reopened.export(None).unwrap()).unwrap();
        assert_eq!(entries(&deleted, 0), before, "after deleting the app note only the original entries remain");
        for id in &foreign { assert_eq!(deleted.get_object(*id).unwrap(), source.get_object(*id).unwrap()); }
        // Page removal stays refused while other apps' annotations exist; rotation and moves keep them.
        assert!(session.apply(crate::editor::PageEdit::Delete { pages: vec![1] }).unwrap_err().contains("other apps"));
        session.apply(crate::editor::PageEdit::Move { from: 1, to: 0 }).unwrap();
        let moved = Document::load_mem(&session.export(None).unwrap()).unwrap();
        for id in &foreign { assert_eq!(moved.get_object(*id).unwrap(), source.get_object(*id).unwrap()); }
    }

    #[test]
    fn an_app_note_another_object_refers_to_becomes_foreign_and_new_ids_skip_its_number() {
        let (mut session, bytes) = { let mut session = crate::editor::EditSession::new(std::fs::read(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("resources/welcome.pdf")).unwrap(), 6);
            let next = session.proposed_comment(Some((0, crate::editor::CropBox { left: 40.0, bottom: 60.0, right: 70.0, top: 90.0 })), None, Some("Ours")).unwrap(); session.commit_comments(next);
            let bytes = session.export(None).unwrap(); (session, bytes) };
        let ours = session.plan[0].notes[0].id.clone();
        let mut document = Document::load_mem(&bytes).unwrap();
        let page = document.get_pages()[&1];
        let note = entries(&document, 0)[0].as_reference().unwrap();
        let reply = document.add_object(dictionary! { "Type" => "Annot", "Subtype" => "Text", "Rect" => vec![40.into(), 60.into(), 70.into(), 90.into()], "IRT" => note, "T" => literal(b"Acrobat user"), "Contents" => literal(b"A reply to your note") });
        document.get_dictionary_mut(page).unwrap().get_mut(b"Annots").unwrap().as_array_mut().unwrap().push(reply.into());
        let mut replied = Vec::new(); document.clone().save_to(&mut replied).unwrap();
        session = crate::editor::EditSession::new(replied, 6);
        assert!(session.comments_reason().is_none() && session.plan[0].notes.is_empty(), "a note something else depends on is kept, not edited");
        assert_eq!(session.view_only_annotations().unwrap().pages[0].len(), 2, "it is listed view only, with the reply");
        let next = session.proposed_comment(Some((0, crate::editor::CropBox { left: 100.0, bottom: 60.0, right: 130.0, top: 90.0 })), None, Some("New")).unwrap(); session.commit_comments(next);
        assert!(crate::comments::is_uuid(&session.plan[0].notes[0].id) && session.plan[0].notes[0].id != ours, "new IDs are random UUIDs, never a kept app ID");
        let saved = Document::load_mem(&session.export(None).unwrap()).unwrap();
        assert_eq!(saved.get_object(note).unwrap(), document.get_object(note).unwrap());
        assert_eq!(saved.get_object(reply).unwrap(), document.get_object(reply).unwrap());
    }

    /// 5 s in the unoptimised test build, 1 s in a release build (`cargo test --release`); release timings
    /// are recorded in the SM3 log.
    const BUDGET: std::time::Duration = std::time::Duration::from_secs(if cfg!(debug_assertions) { 5 } else { 1 });

    /// The reviewers' reproduction (214 s to open in a release build before the fix): one page, 4,096
    /// annotations whose every text field refers to one shared 1 MB string of control characters.
    #[test]
    fn reviewer_probe_4096_annotations_sharing_one_1_mb_control_string_opens_quickly() {
        let probe = document(|document, _| {
            let controls = document.add_object(Object::String(vec![0x01; 1_000_000], StringFormat::Literal));
            Object::Array((0..4_096).map(|_| document.add_object(dictionary! { "Type" => "Annot", "Subtype" => "Text", "T" => controls, "Subj" => controls, "Contents" => controls, "M" => controls, "CreationDate" => controls }).into()).collect())
        });
        let mut bytes = Vec::new(); probe.clone().save_to(&mut bytes).unwrap();
        let started = std::time::Instant::now();
        let session = crate::editor::EditSession::new(bytes, 1);
        let elapsed = started.elapsed();
        println!("reviewer probe, session open including parse: {elapsed:?}");
        assert!(elapsed < BUDGET * 4, "took {elapsed:?}");
        let started = std::time::Instant::now();
        let listed = survey_all(&probe);
        let elapsed = started.elapsed();
        println!("reviewer probe, survey alone: {elapsed:?}");
        assert!(elapsed < BUDGET, "took {elapsed:?}");
        assert_eq!(listed.pages[0].len(), 4_096);
        assert_eq!(session.view_only_annotations().unwrap().pages[0].len(), 4_096);
    }

    #[test]
    fn a_huge_utf8_label_is_decoded_from_a_bounded_prefix() {
        let mut huge = vec![0xEF, 0xBB, 0xBF];
        huge.extend("é".repeat(32 * 1024 * 1024).into_bytes());
        assert_eq!(huge.len(), 64 * 1024 * 1024 + 3);
        let started = std::time::Instant::now();
        let label = text(&huge, MAX_LABEL_CHARS, false);
        assert_eq!(label.chars().count(), MAX_LABEL_CHARS);
        let survey = survey_all(&document(|document, _| {
            let shared = document.add_object(Object::String(huge.clone(), StringFormat::Literal));
            Object::Array(vec![Object::Dictionary(dictionary! { "Subtype" => "Text", "T" => shared, "Subj" => shared, "Contents" => shared, "M" => shared })])
        }));
        let elapsed = started.elapsed();
        println!("64 MiB UTF-8 label: {elapsed:?}");
        assert!(elapsed < BUDGET, "took {elapsed:?}");
        assert_eq!(survey.pages[0][0].author.as_ref().unwrap().chars().count(), MAX_LABEL_CHARS);
        assert_eq!(survey.pages[0][0].contents.as_ref().unwrap().chars().count(), MAX_CONTENTS_CHARS);
    }

    #[test]
    fn a_shared_all_control_string_on_every_listed_annotation_stays_cheap() {
        let mut document = Document::with_version("1.7");
        let pages = document.new_object_id();
        let controls = document.add_object(Object::String(vec![0x01; 1024 * 1024], StringFormat::Literal));
        let page_count = MAX_LISTED / MAX_LISTED_PER_PAGE;
        let kids: Vec<Object> = (0..page_count).map(|_| {
            let annots: Vec<Object> = (0..MAX_LISTED_PER_PAGE).map(|_| document.add_object(dictionary! { "Subtype" => "Square", "T" => controls, "Subj" => controls, "Contents" => controls, "M" => controls, "CreationDate" => controls }).into()).collect();
            document.add_object(dictionary! { "Type" => "Page", "Parent" => pages, "MediaBox" => vec![0.into(), 0.into(), 10.into(), 10.into()], "Annots" => annots }).into()
        }).collect();
        document.objects.insert(pages, Object::Dictionary(dictionary! { "Type" => "Pages", "Kids" => kids, "Count" => page_count as i64 }));
        let catalog = document.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages });
        document.trailer.set("Root", catalog);
        let started = std::time::Instant::now();
        let survey = survey_all(&document);
        let elapsed = started.elapsed();
        println!("16,384 annotations sharing a 1 MiB control string: {elapsed:?}");
        assert!(elapsed < BUDGET, "took {elapsed:?}");
        assert_eq!(survey.pages.iter().map(Vec::len).sum::<usize>(), MAX_LISTED);
        assert!(survey.pages.iter().flatten().all(|item| item.author.is_none() && item.subject.is_none() && item.contents.is_none() && item.modified.is_none()));
        let started = std::time::Instant::now();
        assert_eq!(text(&vec![0x01; 64 * 1024 * 1024], MAX_LABEL_CHARS, false), "");
        assert!(started.elapsed() < std::time::Duration::from_millis(500), "an unshared all-control string is read only up to the raw cap");
    }

    #[test]
    fn a_large_annots_array_shared_by_many_pages_is_surveyed_once() {
        let mut document = Document::with_version("1.7");
        let pages = document.new_object_id();
        let entries: Vec<Object> = (0..MAX_ENTRIES_PER_PAGE).map(|index| document.add_object(dictionary! { "Subtype" => if index % 2 == 0 { "Link" } else { "Square" } }).into()).collect();
        let shared = document.add_object(Object::Array(entries));
        let kids: Vec<Object> = (0..100_000).map(|_| document.add_object(dictionary! { "Type" => "Page", "Parent" => pages, "MediaBox" => vec![0.into(), 0.into(), 10.into(), 10.into()], "Annots" => shared }).into()).collect();
        document.objects.insert(pages, Object::Dictionary(dictionary! { "Type" => "Pages", "Kids" => kids, "Count" => 100_000 }));
        let catalog = document.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages });
        document.trailer.set("Root", catalog);
        let started = std::time::Instant::now();
        let survey = survey_all(&document);
        let elapsed = started.elapsed();
        println!("100,000 pages sharing a 65,536-entry /Annots: {elapsed:?}");
        assert!(elapsed < BUDGET, "took {elapsed:?}");
        assert_eq!(survey.pages.len(), 100_000);
        assert_eq!(survey.pages[0].len(), MAX_LISTED_PER_PAGE, "the first page lists up to its cap");
        assert!(survey.pages[1..].iter().all(Vec::is_empty), "the shared array is not surveyed again");
        assert_eq!(survey.truncated, MAX_ENTRIES_PER_PAGE / 2 - MAX_LISTED_PER_PAGE);
    }

    #[test]
    fn distinct_annots_arrays_stop_at_the_document_wide_entry_cap() {
        let mut document = Document::with_version("1.7");
        let pages = document.new_object_id();
        let link = document.add_object(dictionary! { "Subtype" => "Link" });
        let kids: Vec<Object> = (0..8).map(|_| document.add_object(dictionary! { "Type" => "Page", "Parent" => pages, "MediaBox" => vec![0.into(), 0.into(), 10.into(), 10.into()], "Annots" => vec![Object::Reference(link); MAX_ENTRIES_PER_PAGE] }).into()).collect();
        document.objects.insert(pages, Object::Dictionary(dictionary! { "Type" => "Pages", "Kids" => kids, "Count" => 8 }));
        let catalog = document.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages });
        document.trailer.set("Root", catalog);
        let survey = survey_all(&document);
        assert_eq!(survey.truncated, 8 * MAX_ENTRIES_PER_PAGE - MAX_ENTRIES_TOTAL, "entries past the document cap are only counted");
    }

    #[test]
    fn labels_drop_invisible_and_separator_characters_and_bodies_turn_separators_into_newlines() {
        let utf8 = |value: &str| [&[0xEF, 0xBB, 0xBF][..], value.as_bytes()].concat();
        assert_eq!(text(&utf8("Na\u{200B}me\u{2060}\u{2028}X\u{2029}"), 32, false), "NameX");
        assert_eq!(text(&utf8("a\u{2028}b\u{2029}c\u{200D}"), 32, true), "a\nb\nc");
        let mut utf16 = vec![0xFE, 0xFF]; for unit in "x\u{200B}\u{2028}y".encode_utf16() { utf16.extend(unit.to_be_bytes()); }
        assert_eq!(text(&utf16, 32, false), "xy");
    }

    #[test]
    fn text_decoding_never_fails_and_strips_controls() {
        assert_eq!(text(&[0xFE, 0xFF, 0xD8, 0x00], 10, false), "\u{FFFD}", "a lone surrogate is replaced");
        assert_eq!(text(&[0xFE, 0xFF, 0x00], 10, false), "", "an odd trailing byte is dropped");
        assert_eq!(text(b"\xEF\xBB\xBFcaf\xC3\xA9", 10, false), "caf\u{e9}");
        assert_eq!(text(b"a\tb\nc", 10, false), "abc");
        assert_eq!(text(b"a\tb\nc", 10, true), "a\tb\nc");
        assert_eq!(text(b"\x7f\x9f\xad", 10, true), "");
    }
}

#[cfg(all(test, windows))]
#[path = "qpdf_check.rs"]
mod qpdf_check;
