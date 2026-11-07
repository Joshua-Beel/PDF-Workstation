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

/// The survey of every page, in `get_pages` order. Bounded by the limits above; never fails.
pub fn survey(document: &Document) -> Survey {
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
            let dict = match entry { Object::Reference(id) if seen_annotations.insert(*id) => document.get_dictionary(*id).ok(), Object::Reference(_) => None, Object::Dictionary(dict) => Some(dict), _ => None };
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
        let survey = survey(&document(|document, page| {
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
        let survey = survey(&document(|document, _| {
            let items: Vec<Object> = (0..MAX_LISTED_PER_PAGE + 3).map(|_| Object::Dictionary(dictionary! { "Subtype" => "Square", "Contents" => literal(&vec![b'a'; MAX_CONTENTS_CHARS + 10]) })).collect();
            document.add_object(Object::Array(items)).into()
        }));
        assert_eq!(survey.pages[0].len(), MAX_LISTED_PER_PAGE);
        assert_eq!(survey.truncated, 3);
        assert_eq!(survey.pages[0][0].contents.as_ref().unwrap().chars().count(), MAX_CONTENTS_CHARS);
    }

    #[test]
    fn a_refused_file_keeps_its_refusal_and_lists_its_annotations_view_only() {
        let foreign = document(|document, page| {
            let note = document.add_object(dictionary! { "Type" => "Annot", "Subtype" => "Text", "Rect" => vec![10.into(), 10.into(), 30.into(), 30.into()], "P" => page, "T" => literal(b"Reviewer"), "Contents" => literal(b"Check this") });
            Object::Array(vec![note.into()])
        });
        let mut bytes = Vec::new(); foreign.clone().save_to(&mut bytes).unwrap();
        let session = crate::editor::EditSession::new(bytes.clone(), 1);
        assert!(session.comments_reason().is_some(), "editing stays refused until the preserving writer lands");
        let listed = &session.view_only_annotations().unwrap().pages[0];
        assert_eq!(listed.len(), 1);
        let info = crate::comments::ViewOnlyInfo { page: 0, annotation: listed[0].clone() };
        assert_eq!(serde_json::to_value(&info).unwrap(), serde_json::json!({ "page": 0, "subtype": "Text", "author": "Reviewer", "subject": null, "contents": "Check this", "modified": null, "reply": false }));
        assert!(session.proposed_comment(Some((0, crate::editor::CropBox { left: 50.0, bottom: 50.0, right: 80.0, top: 80.0 })), None, Some("new")).is_err(), "no app comment can be added over it yet");
        let exported = session.export(None).unwrap();
        assert_eq!(survey(&Document::load_mem(&exported).unwrap()), survey(&foreign), "an unedited copy keeps the annotation");
        let supported = crate::editor::EditSession::new(std::fs::read(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("resources/welcome.pdf")).unwrap(), 6);
        assert!(supported.comments_reason().is_none() && supported.view_only_annotations().is_none(), "editable files are not surveyed");
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
        let listed = survey(&probe);
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
        let survey = survey(&document(|document, _| {
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
        let survey = survey(&document);
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
        let survey = survey(&document);
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
        let survey = survey(&document);
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
