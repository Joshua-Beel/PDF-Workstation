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
/// Body characters kept per annotation for the list, and body bytes surveyed per document (this bounds the
/// list sent to the frontend; paging the list is later phase 1 work).
pub const MAX_CONTENTS_CHARS: usize = 16_384;
pub const MAX_TOTAL_CONTENTS_BYTES: usize = 4 * 1024 * 1024;
pub const MAX_LABEL_CHARS: usize = 256;
/// `/Annots` entries examined per page, listed or not.
pub const MAX_ENTRIES_PER_PAGE: usize = 65_536;

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
/// characters, dropping C0/C1 controls (newline and tab survive when `multiline`) and bidi overrides.
pub fn text(bytes: &[u8], limit: usize, multiline: bool) -> String {
    let keep = |c: char| !matches!(c, '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}' | '\u{200E}' | '\u{200F}' | '\u{061C}' | '\u{FEFF}')
        && (!c.is_control() || (multiline && (c == '\n' || c == '\t')));
    let normal = |c: char| if c == '\r' && multiline { '\n' } else { c };
    let chars: Box<dyn Iterator<Item = char>> = if let Some(rest) = bytes.strip_prefix(&[0xFE, 0xFF]) {
        Box::new(char::decode_utf16(rest.chunks_exact(2).map(|pair| u16::from_be_bytes([pair[0], pair[1]]))).map(|c| c.unwrap_or('\u{FFFD}')))
    } else if let Some(rest) = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]) {
        Box::new(String::from_utf8_lossy(rest).chars().collect::<Vec<_>>().into_iter())
    } else {
        Box::new(bytes.iter().filter_map(|byte| pdfdoc(*byte)))
    };
    let (mut out, mut count, mut previous_cr) = (String::new(), 0usize, false);
    for c in chars {
        if count >= limit { break; }
        if multiline && c == '\n' && previous_cr { previous_cr = false; continue; }
        previous_cr = c == '\r';
        let c = normal(c);
        if keep(c) { out.push(c); count += 1; }
    }
    out.trim().to_owned()
}

fn string(document: &Document, dict: &Dictionary, key: &[u8], limit: usize, multiline: bool) -> Option<String> {
    let value = dict.get(key).ok()?;
    let value = match value { Object::Reference(id) => document.get_object(*id).ok()?, other => other };
    let value = text(value.as_str().ok()?, limit, multiline);
    (!value.is_empty()).then_some(value)
}

/// `D:YYYYMMDDHHmm...` to `YYYY-MM-DD HH:MM` (local time as written; the offset is not applied).
fn date(document: &Document, dict: &Dictionary) -> Option<String> {
    let raw = [b"M".as_slice(), b"CreationDate"].iter().find_map(|key| string(document, dict, key, 64, false))?;
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
    let (mut listed, mut contents_bytes) = (0usize, 0usize);
    for (_, page_id) in document.get_pages() {
        let mut page_list = Vec::new();
        let annots = document.get_dictionary(page_id).ok().and_then(|page| page.get(b"Annots").ok()).and_then(|value| match value {
            Object::Reference(id) => document.get_object(*id).ok().and_then(|value| value.as_array().ok()),
            other => other.as_array().ok(),
        });
        for entry in annots.map(|annots| annots.as_slice()).unwrap_or(&[]).iter().take(MAX_ENTRIES_PER_PAGE) {
            let dict = match entry { Object::Reference(id) => document.get_dictionary(*id).ok(), Object::Dictionary(dict) => Some(dict), _ => None };
            let Some(dict) = dict else { continue };
            let subtype_name = name(dict, b"Subtype");
            if subtype_name.as_deref().is_some_and(|subtype| NOT_LISTED.iter().any(|skip| skip.as_bytes() == subtype)) { continue; }
            let flags = dict.get(b"F").ok().and_then(|value| value.as_i64().ok()).unwrap_or(0);
            let state = dict.has(b"State") || dict.has(b"StateModel");
            if state && dict.has(b"IRT") { continue; }
            if flags & 2 != 0 && !dict.has(b"IRT") { continue; }
            if page_list.len() >= MAX_LISTED_PER_PAGE || listed >= MAX_LISTED { result.truncated += 1; continue; }
            let subtype = subtype_name.as_deref().and_then(|subtype| LISTED.iter().copied().find(|known| known.as_bytes() == subtype)).unwrap_or("Other");
            let contents = (contents_bytes < MAX_TOTAL_CONTENTS_BYTES).then(|| string(document, dict, b"Contents", MAX_CONTENTS_CHARS, true)).flatten();
            contents_bytes += contents.as_ref().map_or(0, String::len);
            page_list.push(Surveyed {
                subtype,
                author: string(document, dict, b"T", MAX_LABEL_CHARS, false),
                subject: string(document, dict, b"Subj", MAX_LABEL_CHARS, false),
                contents,
                modified: date(document, dict),
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
