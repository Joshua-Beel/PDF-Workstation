//! Page-label editing for Save a Copy.
//!
//! The edited labels are held as the document's list of ranges (a start page, a numbering style, a prefix and
//! a first number), the same shape the PDF number tree has. Untouched documents are never rewritten: ranges
//! are written only when they differ from the source's. A copy gets one flat `/Nums` tree under `/PageLabels`
//! (sorted, page 0 covered), or no `/PageLabels` at all when the labels are the PDF default.
//!
//! Interaction with page structure (a deliberate refusal rule): ranges name physical pages, so label edits
//! are accepted only while the page order is the source order (rotation and cropping do not matter), and
//! reordering or deleting pages is refused while label edits are pending. Extracting or splitting pages from a
//! copy that has label edits recomputes the ranges for the pages kept, so each page keeps its label.
use crate::editor::PageSpec;
use crate::page_labels::{expand_ranges, Range, Style, MAX_PREFIX_BYTES, MAX_RANGES};
use lopdf::{dictionary, Dictionary, Document, Object};
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;

const MAX_UNDO: usize = 32;
const UNDO_BUDGET: usize = 16 * 1024 * 1024;
const MAX_FIRST: u64 = 1_000_000_000;

pub const STRUCTURE_REFUSAL: &str = "Page labels can be edited only while the pages are in their original order. Undo the page reordering or deleting first.";
pub const PENDING_REFUSAL: &str = "This document has unsaved page-label edits, and reordering or deleting pages would move them. Save a copy or discard the label edits first.";

#[derive(Deserialize, Debug, Clone)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LabelOp {
    /// Defines the range that starts at `start_page` (zero-based), replacing a range that already starts there.
    Set { start_page: usize, style: Option<Style>, prefix: String, first: u64 },
    /// Removes the range that starts at `start_page`; its pages continue the range before it.
    Delete { start_page: usize },
    Undo,
    Redo,
}

#[derive(Serialize, Debug)]
pub struct RangeView {
    pub start_page: usize,
    pub end_page: usize,
    pub style: Option<Style>,
    pub prefix: String,
    pub first: u64,
    pub first_label: String,
    pub last_label: String,
}

#[derive(Serialize, Debug)]
pub struct EditView {
    pub editable: bool,
    pub reason: Option<String>,
    pub version: u64,
    pub page_count: usize,
    pub ranges: Vec<RangeView>,
    pub can_undo: bool,
    pub can_redo: bool,
    pub changed: bool,
}

#[derive(Default)]
pub struct LabelEdits {
    baseline: Option<Result<Vec<Range>, String>>,
    current: Vec<Range>,
    page_count: usize,
    undo: VecDeque<Vec<Range>>,
    redo: Vec<Vec<Range>>,
    version: u64,
}

fn default_range() -> Range { Range { start: 0, prefix: String::new(), style: Some(Style::Decimal), first: 1 } }

fn snapshot_bytes(ranges: &[Range]) -> usize { ranges.iter().map(|range| std::mem::size_of::<Range>() + range.prefix.capacity()).sum::<usize>() + std::mem::size_of::<Vec<Range>>() }

/// The PDF default (decimal numbering from 1, no prefix) is the same as having no `/PageLabels`.
fn normalized(mut ranges: Vec<Range>) -> Vec<Range> {
    if ranges.len() == 1 && ranges[0] == default_range() { ranges.clear(); }
    ranges
}

pub fn encode_prefix(value: &str) -> Vec<u8> {
    if value.is_ascii() { value.as_bytes().to_vec() } else {
        let mut bytes = vec![0xfe, 0xff];
        for unit in value.encode_utf16() { bytes.extend(unit.to_be_bytes()); }
        bytes
    }
}

fn clean_prefix(prefix: &str) -> Result<String, String> {
    if prefix.chars().any(char::is_control) { return Err("A page-label prefix cannot contain control characters.".into()); }
    if prefix.chars().any(|c| matches!(c, '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}' | '\u{200B}'..='\u{200F}' | '\u{FEFF}' | '\u{2060}' | '\u{00AD}')) { return Err("A page-label prefix cannot contain invisible or text-direction control characters.".into()); }
    if encode_prefix(prefix).len() > MAX_PREFIX_BYTES { return Err("A page-label prefix is too long (at most 512 bytes once encoded; 250 characters outside basic ASCII).".into()); }
    Ok(prefix.to_owned())
}

/// The numbering of every page of `ranges` as `(style, prefix, number)`; a prefix-only page has number 1.
fn per_page(ranges: &[Range], page_count: usize) -> Vec<(Option<Style>, &str, u64)> {
    let mut pages = Vec::with_capacity(page_count);
    for (index, range) in ranges.iter().enumerate() {
        let end = ranges.get(index + 1).map_or(page_count, |next| next.start);
        for page in range.start..end {
            pages.push((range.style, range.prefix.as_str(), if range.style.is_some() { range.first + (page - range.start) as u64 } else { 1 }));
        }
    }
    pages
}

/// The shortest ranges that give each listed page its numbering, in the order listed.
fn compress(pages: impl Iterator<Item = (Option<Style>, String, u64)>) -> Vec<Range> {
    let mut ranges: Vec<Range> = Vec::new();
    let mut previous: Option<(Option<Style>, String, u64)> = None;
    for (index, (style, prefix, number)) in pages.enumerate() {
        let continues = previous.as_ref().is_some_and(|(before_style, before_prefix, before_number)| *before_style == style && *before_prefix == prefix && (style.is_none() || *before_number + 1 == number));
        if !continues { ranges.push(Range { start: index, prefix: prefix.clone(), style, first: number }); }
        previous = Some((style, prefix, number));
    }
    ranges
}

fn in_source_order(plan: &[PageSpec], source_pages: usize) -> bool { plan.len() == source_pages && plan.iter().enumerate().all(|(index, spec)| spec.source == index) }

/// The ranges to write for the output pages `plan`. When the output is every source page in order, the edited
/// ranges are written as they are; otherwise they are recomputed for the pages kept.
pub fn output_ranges(ranges: &[Range], source_pages: usize, plan: &[PageSpec]) -> Result<Vec<Range>, String> {
    if in_source_order(plan, source_pages) { return Ok(normalized(ranges.to_vec())); }
    let pages = per_page(ranges, source_pages);
    if pages.len() != source_pages || plan.iter().any(|spec| spec.source >= source_pages) { return Err("The page-label ranges do not cover every source page.".into()); }
    Ok(normalized(compress(plan.iter().map(|spec| { let (style, prefix, number) = pages[spec.source]; (style, prefix.to_owned(), number) }))))
}

fn style_name(style: Style) -> &'static str {
    match style { Style::Decimal => "D", Style::RomanUpper => "R", Style::RomanLower => "r", Style::AlphaUpper => "A", Style::AlphaLower => "a" }
}

/// Writes `ranges` as the document's `/PageLabels`, or removes it when there are none.
pub fn write(document: &mut Document, ranges: &[Range]) -> Result<(), String> {
    if ranges.is_empty() {
        document.catalog_mut().map_err(|error| error.to_string())?.remove(b"PageLabels");
        return Ok(());
    }
    if ranges.len() > MAX_RANGES { return Err("The document would have too many page-label ranges.".into()); }
    if ranges[0].start != 0 || ranges.windows(2).any(|pair| pair[0].start >= pair[1].start) { return Err("The page-label ranges are not sorted from page 1.".into()); }
    let mut nums: Vec<Object> = Vec::with_capacity(ranges.len() * 2);
    for range in ranges {
        let mut value = Dictionary::new();
        if let Some(style) = range.style { value.set("S", Object::Name(style_name(style).as_bytes().to_vec())); }
        if !range.prefix.is_empty() { value.set("P", Object::string_literal(encode_prefix(&range.prefix))); }
        if range.style.is_some() && range.first != 1 { value.set("St", Object::Integer(i64::try_from(range.first).map_err(|_| "A page-label start number is too large.")?)); }
        nums.push(Object::Integer(i64::try_from(range.start).map_err(|_| "A page-label start page is too large.")?));
        nums.push(Object::Dictionary(value));
    }
    let tree = document.add_object(dictionary! { "Nums" => nums });
    document.catalog_mut().map_err(|error| error.to_string())?.set("PageLabels", Object::Reference(tree));
    Ok(())
}

impl LabelEdits {
    pub fn loaded(&self) -> bool { self.baseline.is_some() }
    pub fn load(&mut self, result: Result<Vec<Range>, String>, page_count: usize) {
        if let Ok(ranges) = &result { self.current = ranges.clone(); }
        self.page_count = page_count;
        self.baseline = Some(result);
    }
    /// Drops every pending label edit and returns to the source labels.
    pub fn discard(&mut self) {
        if !self.changed() && self.undo.is_empty() && self.redo.is_empty() { return; }
        if let Some(Ok(baseline)) = &self.baseline { self.current = baseline.clone(); }
        self.undo.clear(); self.redo.clear(); self.version += 1;
    }
    pub fn changed(&self) -> bool { matches!(&self.baseline, Some(Ok(baseline)) if *baseline != self.current) }
    pub fn pending(&self) -> Option<&[Range]> { self.changed().then_some(self.current.as_slice()) }
    /// The labels the pending edits produce, for the page list. `None` when nothing is pending.
    pub fn pending_labels(&self) -> Option<crate::page_labels::SourcePageLabels> {
        self.pending().map(|ranges| crate::page_labels::SourcePageLabels::from_ranges(ranges, self.page_count))
    }

    pub fn view(&self, plan: &[PageSpec]) -> EditView {
        let order = in_source_order(plan, self.page_count);
        let (mut editable, mut reason) = match &self.baseline { Some(Ok(_)) => (true, None), Some(Err(reason)) => (false, Some(reason.clone())), None => (false, Some("Page labels have not been loaded.".into())) };
        if editable && !order && !self.changed() { editable = false; reason = Some(STRUCTURE_REFUSAL.into()); }
        let labels = expand_ranges(&self.current, self.page_count).unwrap_or_default();
        let ranges = if editable || self.changed() {
            self.current.iter().enumerate().map(|(index, range)| {
                let end_page = self.current.get(index + 1).map_or(self.page_count, |next| next.start).saturating_sub(1);
                RangeView { start_page: range.start, end_page, style: range.style, prefix: range.prefix.clone(), first: range.first, first_label: labels.get(range.start).cloned().unwrap_or_default(), last_label: labels.get(end_page).cloned().unwrap_or_default() }
            }).collect()
        } else { Vec::new() };
        EditView { editable, reason, version: self.version, page_count: self.page_count, ranges, can_undo: !self.undo.is_empty(), can_redo: !self.redo.is_empty(), changed: self.changed() }
    }

    pub fn apply(&mut self, version: u64, op: LabelOp, plan: &[PageSpec]) -> Result<(), String> {
        if let Some(Err(reason)) = &self.baseline { return Err(reason.clone()); }
        if self.baseline.is_none() { return Err("Page labels have not been loaded.".into()); }
        if version != self.version { return Err("The page labels changed. Reload them and try again.".into()); }
        if !in_source_order(plan, self.page_count) { return Err(STRUCTURE_REFUSAL.into()); }
        match op {
            LabelOp::Undo => { let previous = self.undo.pop_back().ok_or("Nothing to undo.")?; let current = std::mem::replace(&mut self.current, previous); self.redo.push(current); self.version += 1; return Ok(()); }
            LabelOp::Redo => { let next = self.redo.pop().ok_or("Nothing to redo.")?; let current = std::mem::replace(&mut self.current, next); self.undo.push_back(current); self.version += 1; return Ok(()); }
            _ => {}
        }
        let mut next = self.current.clone();
        match op {
            LabelOp::Set { start_page, style, prefix, first } => {
                if start_page >= self.page_count { return Err("Page is out of range.".into()); }
                let prefix = clean_prefix(&prefix)?;
                let first = match style {
                    None => { if prefix.is_empty() { return Err("Choose a numbering style or enter a prefix.".into()); } 1 }
                    Some(_) => { if first == 0 || first > MAX_FIRST { return Err(format!("The first number must be between 1 and {MAX_FIRST}.")); } first }
                };
                if next.is_empty() && start_page > 0 { next.push(default_range()); }
                let range = Range { start: start_page, prefix, style, first };
                match next.binary_search_by_key(&start_page, |existing| existing.start) {
                    Ok(position) => next[position] = range,
                    Err(position) => {
                        if next.len() >= MAX_RANGES { return Err(format!("A document can have at most {MAX_RANGES} page-label ranges.")); }
                        next.insert(position, range);
                    }
                }
            }
            LabelOp::Delete { start_page } => {
                let position = next.binary_search_by_key(&start_page, |existing| existing.start).map_err(|_| "No page-label range starts on that page.".to_owned())?;
                if position == 0 {
                    if next.len() == 1 { next.clear(); } else { next[0] = default_range(); }
                } else { next.remove(position); }
            }
            LabelOp::Undo | LabelOp::Redo => unreachable!(),
        }
        let next = normalized(next);
        if !next.is_empty() { expand_ranges(&next, self.page_count)?; }
        if next == self.current { return Ok(()); }
        let previous = std::mem::replace(&mut self.current, next);
        self.undo.push_back(previous);
        self.redo.clear();
        while self.undo.len() > MAX_UNDO || self.undo.iter().map(|snapshot| snapshot_bytes(snapshot)).sum::<usize>() > UNDO_BUDGET { self.undo.pop_front(); }
        self.version += 1;
        Ok(())
    }
}

/// True when the plan has the source's pages in the source's order (the only plan label edits accept).
pub fn plan_in_source_order(plan: &[PageSpec], source_pages: usize) -> bool { in_source_order(plan, source_pages) }

#[cfg(test)]
#[path = "page_label_edit_tests.rs"]
mod tests;
