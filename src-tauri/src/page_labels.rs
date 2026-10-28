use crate::editor::PageSpec;
use lopdf::{Dictionary, Document, Object, ObjectId};
use serde::{Deserialize, Serialize};
use std::collections::{HashSet, VecDeque};

const MAX_SOURCE_BYTES: usize = 64 * 1024 * 1024;
const MAX_PAGES: usize = 65_536;
const MAX_REFERENCE_DEPTH: usize = 16;
const MAX_TREE_DEPTH: usize = 16;
const MAX_TREE_NODES: usize = 1_024;
pub(crate) const MAX_RANGES: usize = 4_096;
pub(crate) const MAX_PREFIX_BYTES: usize = 512;
const MAX_LABEL_BYTES: usize = 1_024;
const MAX_OUTPUT_BYTES: usize = 1024 * 1024;
const CACHE_BUDGET: usize = 8 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum PageLabelStatus {
    Supported,
    None,
    Unavailable,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PageLabel {
    pub page: usize,
    pub label: String,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DocumentPageLabels {
    pub document_id: u64,
    pub revision: u64,
    pub status: PageLabelStatus,
    pub reason: Option<String>,
    pub labels: Vec<PageLabel>,
}

#[derive(Clone, Debug)]
pub struct SourcePageLabels {
    status: PageLabelStatus,
    reason: Option<String>,
    labels: Vec<String>,
}

impl SourcePageLabels {
    /// The labels the given ranges produce, for showing unsaved label edits. No ranges means no labels.
    pub fn from_ranges(ranges: &[Range], page_count: usize) -> Self {
        if ranges.is_empty() { return Self::none(); }
        match expand_ranges(ranges, page_count) {
            Ok(labels) => Self { status: PageLabelStatus::Supported, reason: None, labels },
            Err(reason) => Self::unavailable(reason),
        }
    }

    fn none() -> Self {
        Self { status: PageLabelStatus::None, reason: None, labels: Vec::new() }
    }

    fn unavailable(reason: impl Into<String>) -> Self {
        Self { status: PageLabelStatus::Unavailable, reason: Some(reason.into()), labels: Vec::new() }
    }

    fn weight(&self) -> usize {
        std::mem::size_of::<Self>() + self.reason.as_ref().map_or(0, String::capacity)
            + self.labels.capacity() * std::mem::size_of::<String>()
            + self.labels.iter().map(String::capacity).sum::<usize>()
    }
}

#[derive(Default)]
pub struct PageLabelCache {
    entries: VecDeque<(u64, SourcePageLabels, usize)>,
    weight: usize,
}

impl PageLabelCache {
    pub fn read(&mut self, id: u64, source: &[u8], source_pages: usize) -> SourcePageLabels {
        if let Some(index) = self.entries.iter().position(|(entry_id, _, _)| *entry_id == id) {
            let entry = self.entries.remove(index).expect("The page-label cache index exists");
            let value = entry.1.clone();
            self.entries.push_back(entry);
            return value;
        }
        let value = read_source(source, source_pages);
        let weight = value.weight();
        while self.weight.saturating_add(weight) > CACHE_BUDGET {
            if let Some((_, _, removed)) = self.entries.pop_front() { self.weight -= removed; } else { break; }
        }
        if weight <= CACHE_BUDGET {
            self.weight += weight;
            self.entries.push_back((id, value.clone(), weight));
        }
        value
    }

    pub fn close(&mut self, id: u64) {
        self.entries.retain(|(entry_id, _, _)| *entry_id != id);
        self.weight = self.entries.iter().map(|(_, _, weight)| *weight).sum();
    }
}

pub fn project(source: &SourcePageLabels, document_id: u64, revision: u64, plan: &[PageSpec]) -> DocumentPageLabels {
    let empty = |status, reason| DocumentPageLabels { document_id, revision, status, reason, labels: Vec::new() };
    if source.status != PageLabelStatus::Supported {
        return empty(source.status, source.reason.clone());
    }
    if plan.len() > MAX_PAGES {
        return empty(PageLabelStatus::Unavailable, Some("Page labels exceed the supported page-count limit.".into()));
    }
    let mut labels = Vec::with_capacity(plan.len());
    for (page, spec) in plan.iter().enumerate() {
        let Some(label) = source.labels.get(spec.source) else {
            return empty(PageLabelStatus::Unavailable, Some("The current page order does not map to the source page labels.".into()));
        };
        labels.push(PageLabel { page, label: label.clone() });
    }
    let candidate = DocumentPageLabels { document_id, revision, status: PageLabelStatus::Supported, reason: None, labels };
    if serde_json::to_vec(&candidate).map_or(true, |bytes| bytes.len() > MAX_OUTPUT_BYTES) {
        return empty(PageLabelStatus::Unavailable, Some("Page labels exceed the supported output limit.".into()));
    }
    candidate
}

fn read_source(source: &[u8], source_pages: usize) -> SourcePageLabels {
    if source.len() > MAX_SOURCE_BYTES {
        return SourcePageLabels::unavailable("Page labels are unavailable for PDF sources larger than 64 MiB.");
    }
    if source_pages == 0 || source_pages > MAX_PAGES {
        return SourcePageLabels::unavailable("Page labels are unavailable for this source page count.");
    }
    let document = match crate::pdf_load::load(source) {
        Ok(document) => document,
        Err(crate::pdf_load::LoadFailure::Parse(reason)) if reason == crate::xref_resolve::AMBIGUOUS => return SourcePageLabels::unavailable(reason),
        Err(_) => return SourcePageLabels::unavailable("The PDF page-label structure could not be read."),
    };
    if document.is_encrypted() || document.encryption_state.is_some() {
        return SourcePageLabels::unavailable("Page labels in encrypted PDFs are unavailable in this build.");
    }
    match parse_source(&document, source_pages) {
        Ok(Some(labels)) => SourcePageLabels { status: PageLabelStatus::Supported, reason: None, labels },
        Ok(None) => SourcePageLabels::none(),
        Err(reason) => SourcePageLabels::unavailable(reason),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Style { Decimal, RomanUpper, RomanLower, AlphaUpper, AlphaLower }

/// One entry of the page-label number tree: pages from `start` (zero-based) up to the next range share it.
#[derive(Clone, Debug, PartialEq)]
pub struct Range {
    pub start: usize,
    pub prefix: String,
    pub style: Option<Style>,
    pub first: u64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct Bounds { first: usize, last: usize }

#[derive(Default)]
struct TreeState {
    nodes: usize,
    ranges: Vec<Range>,
    node_references: HashSet<ObjectId>,
}

fn parse_source(document: &Document, page_count: usize) -> Result<Option<Vec<String>>, String> {
    match parse_ranges(document, page_count)? {
        Some(ranges) => expand_ranges(&ranges, page_count).map(Some),
        None => Ok(None),
    }
}

/// The document's page-label ranges through the same bounded reader the viewer uses (depth, node, range and
/// output caps, cycle and repeated-node checks, Limits checks). An empty list means the document has no labels.
pub fn read_ranges(document: &Document, page_count: usize) -> Result<Vec<Range>, String> {
    if page_count == 0 || page_count > MAX_PAGES { return Err("Page labels are unavailable for this source page count.".into()); }
    match parse_ranges(document, page_count)? {
        Some(ranges) => { expand_ranges(&ranges, page_count)?; Ok(ranges) }
        None => Ok(Vec::new()),
    }
}

fn parse_ranges(document: &Document, page_count: usize) -> Result<Option<Vec<Range>>, String> {
    let catalog = resolve(document, document.trailer.get(b"Root").map_err(|_| "The PDF catalog is missing.")?)?;
    let catalog = catalog.as_dict().map_err(|_| "The PDF catalog has an invalid type.")?;
    let root = match catalog.get(b"PageLabels") {
        Ok(root) => root,
        Err(_) => return Ok(None),
    };
    let mut state = TreeState::default();
    parse_node(document, root, 0, true, page_count, &mut state)?;
    if state.ranges.is_empty() || state.ranges[0].start != 0 {
        return Err("The page-label ranges must start at physical page 0.".into());
    }
    for pair in state.ranges.windows(2) {
        if pair[0].start >= pair[1].start { return Err("The page-label range indices must be sorted and unique.".into()); }
    }
    Ok(Some(state.ranges))
}

fn parse_node(document: &Document, value: &Object, depth: usize, root: bool, page_count: usize, state: &mut TreeState) -> Result<Bounds, String> {
    if depth > MAX_TREE_DEPTH { return Err("The page-label number tree is too deep.".into()); }
    state.nodes += 1;
    if state.nodes > MAX_TREE_NODES { return Err("The page-label number tree has too many nodes.".into()); }
    let dictionary = resolve_node(document, value, &mut state.node_references)?.as_dict().map_err(|_| "A page-label number-tree node is not a dictionary.")?;
    let nums = dictionary.get(b"Nums").ok();
    let kids = dictionary.get(b"Kids").ok();
    if nums.is_some() == kids.is_some() { return Err("Each page-label number-tree node must contain exactly one of Nums or Kids.".into()); }
    let actual = if let Some(nums) = nums {
        let nums = resolve(document, nums)?.as_array().map_err(|_| "A page-label Nums entry is not an array.")?;
        if nums.is_empty() || nums.len() % 2 != 0 { return Err("A page-label Nums array must contain key/value pairs.".into()); }
        let mut first = None;
        let mut last = None;
        for pair in nums.chunks_exact(2) {
            if state.ranges.len() >= MAX_RANGES { return Err("The PDF has too many page-label ranges.".into()); }
            let index = nonnegative_integer(resolve(document, &pair[0])?, "A page-label range index")?;
            if index >= page_count { return Err("A page-label range starts outside the source document.".into()); }
            if last.is_some_and(|previous| index <= previous) { return Err("The page-label range indices must be sorted and unique.".into()); }
            let range = parse_range(document, index, resolve(document, &pair[1])?)?;
            first.get_or_insert(index);
            last = Some(index);
            state.ranges.push(range);
        }
        Bounds { first: first.expect("A nonempty Nums array has a first key"), last: last.expect("A nonempty Nums array has a last key") }
    } else {
        let kids = resolve(document, kids.expect("Kids is present when Nums is absent"))?.as_array().map_err(|_| "A page-label Kids entry is not an array.")?;
        if kids.is_empty() { return Err("A page-label Kids array cannot be empty.".into()); }
        let mut first = None;
        let mut last = None;
        for kid in kids {
            let bounds = parse_node(document, kid, depth + 1, false, page_count, state)?;
            if last.is_some_and(|previous| bounds.first <= previous) { return Err("Page-label child ranges overlap or are out of order.".into()); }
            first.get_or_insert(bounds.first);
            last = Some(bounds.last);
        }
        Bounds { first: first.expect("A nonempty Kids array has a first range"), last: last.expect("A nonempty Kids array has a last range") }
    };
    match parse_limits(document, dictionary)? {
        Some(declared) if declared != actual => return Err("A page-label node has incorrect Limits.".into()),
        None if !root => return Err("A non-root page-label node is missing Limits.".into()),
        _ => {}
    }
    Ok(actual)
}

fn parse_limits(document: &Document, dictionary: &Dictionary) -> Result<Option<Bounds>, String> {
    let value = match dictionary.get(b"Limits") { Ok(value) => value, Err(_) => return Ok(None) };
    let values = resolve(document, value)?.as_array().map_err(|_| "A page-label Limits entry is not an array.")?;
    if values.len() != 2 { return Err("A page-label Limits entry must contain two indices.".into()); }
    let first = nonnegative_integer(resolve(document, &values[0])?, "A page-label lower limit")?;
    let last = nonnegative_integer(resolve(document, &values[1])?, "A page-label upper limit")?;
    if first > last { return Err("A page-label Limits entry is reversed.".into()); }
    Ok(Some(Bounds { first, last }))
}

fn parse_range(document: &Document, start: usize, value: &Object) -> Result<Range, String> {
    let dictionary = value.as_dict().map_err(|_| "A page-label range value is not a dictionary.")?;
    let prefix = match dictionary.get(b"P") {
        Ok(value) => decode_prefix(resolve(document, value)?)?,
        Err(_) => String::new(),
    };
    let style = match dictionary.get(b"S") {
        Ok(value) => {
            let name = resolve(document, value)?.as_name().map_err(|_| "A page-label style is not a name.")?;
            Some(match name {
                b"D" => Style::Decimal,
                b"R" => Style::RomanUpper,
                b"r" => Style::RomanLower,
                b"A" => Style::AlphaUpper,
                b"a" => Style::AlphaLower,
                _ => return Err("The PDF uses an unsupported page-label style.".into()),
            })
        }
        Err(_) => None,
    };
    if style.is_none() && !dictionary.has(b"P") { return Err("A page-label range must contain a style or prefix.".into()); }
    let first = match dictionary.get(b"St") {
        Ok(value) => {
            if style.is_none() { return Err("A prefix-only page-label range cannot contain St.".into()); }
            positive_integer(resolve(document, value)?, "A page-label St value")?
        }
        Err(_) => 1,
    };
    Ok(Range { start, prefix, style, first })
}

fn decode_prefix(value: &Object) -> Result<String, String> {
    let bytes = value.as_str().map_err(|_| "A page-label prefix is not a string.")?;
    if bytes.len() > MAX_PREFIX_BYTES { return Err("A page-label prefix exceeds 512 bytes.".into()); }
    if bytes.starts_with(b"\xfe\xff") {
        if bytes.len() % 2 != 0 { return Err("A page-label prefix has invalid UTF-16BE length.".into()); }
        let units = bytes[2..].chunks_exact(2).map(|pair| u16::from_be_bytes([pair[0], pair[1]])).collect::<Vec<_>>();
        String::from_utf16(&units).map_err(|_| "A page-label prefix has invalid UTF-16BE text.".into())
    } else if bytes.starts_with(b"\xef\xbb\xbf") {
        std::str::from_utf8(&bytes[3..]).map(str::to_owned).map_err(|_| "A page-label prefix has invalid UTF-8 text.".into())
    } else {
        let decoded = lopdf::decode_text_string(value).map_err(|_| "A page-label prefix has invalid PDFDocEncoding text.".to_owned())?;
        if decoded.chars().count() != bytes.len() { return Err("A page-label prefix contains undefined PDFDocEncoding bytes.".into()); }
        Ok(decoded)
    }
}

pub(crate) fn expand_ranges(ranges: &[Range], page_count: usize) -> Result<Vec<String>, String> {
    let mut labels = Vec::with_capacity(page_count);
    let mut total = 0usize;
    for (range_index, range) in ranges.iter().enumerate() {
        let end = ranges.get(range_index + 1).map_or(page_count, |next| next.start);
        for page in range.start..end {
            let number = range.first.checked_add((page - range.start) as u64).ok_or("A page-label number overflows.")?;
            let mut label = range.prefix.clone();
            if let Some(style) = range.style { append_number(&mut label, style, number)?; }
            if label.len() > MAX_LABEL_BYTES { return Err("A generated page label exceeds 1024 bytes.".into()); }
            total = total.checked_add(label.len()).ok_or("The page-label output size overflows.")?;
            if total > MAX_OUTPUT_BYTES { return Err("The source page labels exceed the supported output limit.".into()); }
            labels.push(label);
        }
    }
    if labels.len() != page_count { return Err("The page-label ranges do not cover every source page.".into()); }
    Ok(labels)
}

fn append_number(output: &mut String, style: Style, value: u64) -> Result<(), String> {
    if value == 0 { return Err("Page-label numbers must be positive.".into()); }
    match style {
        Style::Decimal => output.push_str(&value.to_string()),
        Style::RomanUpper | Style::RomanLower => {
            let suffix_start = output.len();
            let thousands = usize::try_from(value / 1000).map_err(|_| "A Roman page label is too large.")?;
            if output.len().saturating_add(thousands) > MAX_LABEL_BYTES { return Err("A generated page label exceeds 1024 bytes.".into()); }
            output.extend(std::iter::repeat_n('M', thousands));
            let mut rest = value % 1000;
            for (amount, text) in [(900, "CM"), (500, "D"), (400, "CD"), (100, "C"), (90, "XC"), (50, "L"), (40, "XL"), (10, "X"), (9, "IX"), (5, "V"), (4, "IV"), (1, "I")] {
                while rest >= amount { output.push_str(text); rest -= amount; }
            }
            if matches!(style, Style::RomanLower) { output[suffix_start..].make_ascii_lowercase(); }
        }
        Style::AlphaUpper | Style::AlphaLower => {
            let repeat = usize::try_from((value - 1) / 26 + 1).map_err(|_| "An alphabetic page label is too large.")?;
            if output.len().saturating_add(repeat) > MAX_LABEL_BYTES { return Err("A generated page label exceeds 1024 bytes.".into()); }
            let base = if matches!(style, Style::AlphaUpper) { b'A' } else { b'a' };
            let letter = (base + ((value - 1) % 26) as u8) as char;
            output.extend(std::iter::repeat_n(letter, repeat));
        }
    }
    Ok(())
}

fn nonnegative_integer(value: &Object, label: &str) -> Result<usize, String> {
    let value = value.as_i64().map_err(|_| format!("{label} is not an integer."))?;
    usize::try_from(value).map_err(|_| format!("{label} must be nonnegative."))
}

fn positive_integer(value: &Object, label: &str) -> Result<u64, String> {
    let value = value.as_i64().map_err(|_| format!("{label} is not an integer."))?;
    u64::try_from(value).ok().filter(|value| *value > 0).ok_or_else(|| format!("{label} must be positive."))
}

fn resolve<'a>(document: &'a Document, value: &'a Object) -> Result<&'a Object, String> {
    let mut current = value;
    let mut seen = HashSet::new();
    let mut depth = 0;
    while let Object::Reference(id) = current {
        if depth >= MAX_REFERENCE_DEPTH { return Err("A page-label reference chain is too deep.".into()); }
        if !seen.insert(*id) { return Err("A page-label reference chain contains a cycle.".into()); }
        current = document.objects.get(id).ok_or("A page-label reference is missing.")?;
        depth += 1;
    }
    Ok(current)
}

fn resolve_node<'a>(document: &'a Document, value: &'a Object, seen_nodes: &mut HashSet<ObjectId>) -> Result<&'a Object, String> {
    let mut current = value;
    let mut local = HashSet::new();
    let mut depth = 0;
    while let Object::Reference(id) = current {
        if depth >= MAX_REFERENCE_DEPTH { return Err("A page-label node reference chain is too deep.".into()); }
        if !local.insert(*id) || !seen_nodes.insert(*id) { return Err("The page-label number tree contains a cycle or repeated node.".into()); }
        current = document.objects.get(id).ok_or("A page-label node reference is missing.")?;
        depth += 1;
    }
    Ok(current)
}

#[cfg(test)]
mod tests {
    use super::*;
    use lopdf::{dictionary, StringFormat};

    fn spec(source: usize) -> PageSpec {
        PageSpec { source, turns: 0, crop: None, notes: Vec::new() }
    }

    fn direct_document(page_labels: Object) -> Document {
        let mut document = Document::with_version("1.7");
        let catalog = document.add_object(dictionary! { "Type" => "Catalog", "PageLabels" => page_labels });
        document.trailer.set("Root", catalog);
        document
    }

    #[test]
    fn fixture_covers_every_style_prefix_only_and_iso_alphabetic_numbering() {
        let source = include_bytes!("../tests/fixtures/reportlab-page-labels.pdf");
        let parsed = read_source(source, 14);
        assert_eq!(parsed.status, PageLabelStatus::Supported, "{:?}", parsed.reason);
        assert_eq!(parsed.labels, ["1", "2", "iv", "v", "I", "II", "AA", "BB", "bb", "cc", "Appendix", "Appendix", "N-5", "N-6"]);
        let projected = project(&parsed, 7, 3, &[spec(8), spec(0), spec(8), spec(13)]);
        assert_eq!(projected.labels.iter().map(|label| (label.page, label.label.as_str())).collect::<Vec<_>>(), [(0, "bb"), (1, "1"), (2, "bb"), (3, "N-6")]);
        assert_eq!(projected.document_id, 7);
        assert_eq!(projected.revision, 3);
    }

    #[test]
    fn absent_and_malformed_trees_never_return_partial_labels() {
        let mut none = Document::with_version("1.7");
        let catalog = none.add_object(dictionary! { "Type" => "Catalog" });
        none.trailer.set("Root", catalog);
        assert!(parse_source(&none, 2).unwrap().is_none());

        for nums in [
            vec![1.into(), Object::Dictionary(dictionary! { "S" => "D" })],
            vec![0.into(), Object::Dictionary(dictionary! { "S" => "D" }), 0.into(), Object::Dictionary(dictionary! { "S" => "D" })],
            vec![0.into(), Object::Dictionary(dictionary! { "S" => "D", "St" => 0 })],
            vec![0.into(), Object::Dictionary(dictionary! { "S" => "Z" })],
        ] {
            let document = direct_document(Object::Dictionary(dictionary! { "Nums" => nums }));
            assert!(parse_source(&document, 2).is_err());
        }
    }

    #[test]
    fn limits_overlap_cycles_and_reference_depth_are_rejected() {
        let left = Object::Dictionary(dictionary! { "Limits" => vec![0.into(), 1.into()], "Nums" => vec![0.into(), Object::Dictionary(dictionary! { "S" => "D" })] });
        let right = Object::Dictionary(dictionary! { "Limits" => vec![1.into(), 2.into()], "Nums" => vec![1.into(), Object::Dictionary(dictionary! { "S" => "D" })] });
        let overlap = direct_document(Object::Dictionary(dictionary! { "Kids" => vec![left, right] }));
        assert!(parse_source(&overlap, 3).unwrap_err().contains("Limits"));

        let mut cycle = Document::with_version("1.7");
        let node = cycle.new_object_id();
        cycle.objects.insert(node, Object::Dictionary(dictionary! { "Kids" => vec![Object::Reference(node)] }));
        let catalog = cycle.add_object(dictionary! { "Type" => "Catalog", "PageLabels" => node });
        cycle.trailer.set("Root", catalog);
        assert!(parse_source(&cycle, 1).unwrap_err().contains("cycle"));

        let mut deep = Document::with_version("1.7");
        let mut value = Object::Dictionary(dictionary! { "Nums" => vec![0.into(), Object::Dictionary(dictionary! { "S" => "D" })] });
        for _ in 0..=MAX_REFERENCE_DEPTH { value = Object::Reference(deep.add_object(value)); }
        let catalog = deep.add_object(dictionary! { "Type" => "Catalog", "PageLabels" => value });
        deep.trailer.set("Root", catalog);
        assert!(parse_source(&deep, 1).unwrap_err().contains("deep"));
    }

    #[test]
    fn text_decoding_is_lossless_and_generation_is_bounded() {
        let pdfdoc = Object::String(vec![b'A', 0x8b], StringFormat::Literal);
        assert_eq!(decode_prefix(&pdfdoc).unwrap(), "A‰");
        let undefined = Object::String(vec![b'A', 0x00, b'B'], StringFormat::Literal);
        assert!(decode_prefix(&undefined).unwrap_err().contains("undefined"));
        let utf16 = Object::String(vec![0xfe, 0xff, 0x03, 0xa9], StringFormat::Hexadecimal);
        assert_eq!(decode_prefix(&utf16).unwrap(), "Ω");
        let utf8 = Object::String([b"\xef\xbb\xbf".as_slice(), "é".as_bytes()].concat(), StringFormat::Hexadecimal);
        assert_eq!(decode_prefix(&utf8).unwrap(), "é");
        let invalid_utf16 = Object::String(vec![0xfe, 0xff, 0xd8, 0x00], StringFormat::Hexadecimal);
        assert!(decode_prefix(&invalid_utf16).is_err());

        let mut alpha = String::new();
        append_number(&mut alpha, Style::AlphaLower, 28).unwrap();
        assert_eq!(alpha, "bb");
        let mut roman = String::new();
        append_number(&mut roman, Style::RomanUpper, 4_000_000).unwrap_err();
    }

    #[test]
    fn source_page_range_node_and_serialized_output_caps_are_enforced() {
        assert_eq!(read_source(&vec![0; MAX_SOURCE_BYTES + 1], 1).status, PageLabelStatus::Unavailable);
        assert_eq!(read_source(include_bytes!("../tests/fixtures/reportlab-page-labels.pdf"), MAX_PAGES + 1).status, PageLabelStatus::Unavailable);

        let mut nums = Vec::new();
        for index in 0..=MAX_RANGES { nums.extend([Object::Integer(index as i64), Object::Dictionary(dictionary! { "S" => "D" })]); }
        let too_many_ranges = direct_document(Object::Dictionary(dictionary! { "Nums" => nums }));
        assert!(parse_source(&too_many_ranges, MAX_RANGES + 1).unwrap_err().contains("too many"));

        let kids = (0..MAX_TREE_NODES).map(|index| Object::Dictionary(dictionary! {
            "Limits" => vec![Object::Integer(index as i64), Object::Integer(index as i64)],
            "Nums" => vec![Object::Integer(index as i64), Object::Dictionary(dictionary! { "S" => "D" })]
        })).collect::<Vec<_>>();
        let too_many_nodes = direct_document(Object::Dictionary(dictionary! { "Kids" => kids }));
        assert!(parse_source(&too_many_nodes, MAX_TREE_NODES).unwrap_err().contains("too many nodes"));

        let source = SourcePageLabels { status: PageLabelStatus::Supported, reason: None, labels: vec![String::new(); MAX_PAGES] };
        let plan = (0..MAX_PAGES).map(spec).collect::<Vec<_>>();
        let projected = project(&source, 1, 0, &plan);
        assert_eq!(projected.status, PageLabelStatus::Unavailable);
        assert!(projected.labels.is_empty());
        assert!(projected.reason.unwrap().contains("output limit"));
    }

    #[test]
    fn encrypted_source_is_explicitly_unavailable_and_input_bytes_remain_intact() {
        let mut document = Document::load_mem(include_bytes!("../tests/fixtures/reportlab-page-labels.pdf")).unwrap();
        document.trailer.set("ID", vec![Object::string_literal("page-label-test-id"), Object::string_literal("page-label-test-id")]);
        let encryption = lopdf::EncryptionVersion::V2 { document: &document, owner_password: "owner", user_password: "labels", key_length: 128, permissions: lopdf::Permissions::all() };
        let state = lopdf::EncryptionState::try_from(encryption).unwrap();
        document.encrypt(&state).unwrap();
        let mut bytes = Vec::new();
        document.save_to(&mut bytes).unwrap();
        let original = bytes.clone();
        let parsed = read_source(&bytes, 14);
        assert_eq!(parsed.status, PageLabelStatus::Unavailable);
        assert!(parsed.reason.unwrap().contains("encrypted"));
        assert_eq!(bytes, original);
    }

    #[test]
    fn cache_is_lru_bounded_and_close_evicts_document_entries() {
        let source = include_bytes!("../tests/fixtures/reportlab-page-labels.pdf");
        let mut cache = PageLabelCache::default();
        assert_eq!(cache.read(1, source, 14).status, PageLabelStatus::Supported);
        assert_eq!(cache.entries.len(), 1);
        assert!(cache.weight <= CACHE_BUDGET);
        cache.close(1);
        assert!(cache.entries.is_empty());
        assert_eq!(cache.weight, 0);
    }
}
