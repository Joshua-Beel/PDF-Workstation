//! Bookmark (outline) editing for Save a Copy.
//!
//! The edited outline is held as a flat, pre-order list of entries (one per bookmark, with its depth), the
//! same shape the panel shows. Existing bookmarks keep their original PDF dictionary, so their destinations,
//! actions, colours and flags are never rewritten; only the tree links (`/Parent /First /Last /Prev /Next
//! /Count`) are regenerated when a copy is written, and `/Title` when the title was edited. New bookmarks get
//! a fresh dictionary with an `/XYZ` destination that keeps the viewer's zoom.
use crate::editor::PageSpec;
use lopdf::{dictionary, Document, Object, ObjectId};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet, VecDeque};

pub const MAX_ENTRIES: usize = 1000;
pub const MAX_DEPTH: usize = 32;
const MAX_TITLE_CHARS: usize = 512;
const MAX_TOTAL_TITLE_BYTES: usize = 1_048_576;
const MAX_UNDO: usize = 32;
const UNDO_BUDGET: usize = 16 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq)]
pub struct Entry {
    pub uid: u32,
    /// The existing outline item this entry keeps; `None` for a bookmark added in this session.
    pub origin: Option<ObjectId>,
    pub title: String,
    pub renamed: bool,
    pub depth: usize,
    pub open: bool,
    /// Zero-based page index in the source document, when it is known.
    pub source_page: Option<usize>,
}

#[derive(Deserialize, Debug, Clone)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum BookmarkOp {
    /// Adds a bookmark to the current (planned) page, as the next sibling after `after`, or at the end.
    Add { page: usize, title: String, after: Option<u32> },
    Rename { uid: u32, title: String },
    Delete { uid: u32 },
    MoveUp { uid: u32 },
    MoveDown { uid: u32 },
    Indent { uid: u32 },
    Outdent { uid: u32 },
    SetOpen { uid: u32, open: bool },
    Undo,
    Redo,
}

#[derive(Serialize, Debug)]
pub struct ViewEntry { pub uid: u32, pub title: String, pub depth: usize, pub page: Option<usize>, pub open: bool, pub has_children: bool, pub added: bool }

#[derive(Serialize, Debug)]
pub struct EditView {
    pub editable: bool,
    pub reason: Option<String>,
    pub version: u64,
    pub entries: Vec<ViewEntry>,
    pub can_undo: bool,
    pub can_redo: bool,
    pub changed: bool,
}

#[derive(Default)]
pub struct OutlineEdits {
    baseline: Option<Result<Vec<Entry>, String>>,
    current: Vec<Entry>,
    undo: VecDeque<Vec<Entry>>,
    redo: Vec<Vec<Entry>>,
    next_uid: u32,
    version: u64,
}

fn snapshot_bytes(entries: &[Entry]) -> usize { entries.iter().map(|entry| std::mem::size_of::<Entry>() + entry.title.capacity()).sum::<usize>() + std::mem::size_of::<Vec<Entry>>() }

fn reference(object: Option<&Object>) -> Result<Option<ObjectId>, String> {
    match object { None | Some(Object::Null) => Ok(None), Some(Object::Reference(id)) => Ok(Some(*id)), Some(_) => Err("The bookmarks use an unsupported structure.".into()) }
}

/// Subtree of entry `index`: `index..end`.
fn subtree_end(entries: &[Entry], index: usize) -> usize {
    let depth = entries[index].depth;
    let mut end = index + 1;
    while end < entries.len() && entries[end].depth > depth { end += 1; }
    end
}

fn previous_sibling(entries: &[Entry], index: usize) -> Option<usize> {
    let depth = entries[index].depth;
    let mut cursor = index;
    while cursor > 0 {
        cursor -= 1;
        if entries[cursor].depth == depth { return Some(cursor); }
        if entries[cursor].depth < depth { return None; }
    }
    None
}

fn valid_structure(entries: &[Entry]) -> Result<(), String> {
    if entries.len() > MAX_ENTRIES { return Err(format!("A document can have at most {MAX_ENTRIES} editable bookmarks.")); }
    let mut previous: Option<usize> = None;
    let mut bytes = 0usize;
    for entry in entries {
        if entry.depth >= MAX_DEPTH { return Err(format!("Bookmarks can be nested at most {MAX_DEPTH} levels deep.")); }
        if entry.depth > previous.map_or(0, |depth| depth + 1) { return Err("The bookmark tree is not well formed.".into()); }
        bytes = bytes.saturating_add(entry.title.len());
        previous = Some(entry.depth);
    }
    if bytes > MAX_TOTAL_TITLE_BYTES { return Err("Bookmarks contain too much text.".into()); }
    Ok(())
}

fn clean_title(title: &str) -> Result<String, String> {
    let title = title.trim();
    if title.is_empty() { return Err("Enter a bookmark title.".into()); }
    if title.chars().count() > MAX_TITLE_CHARS { return Err(format!("A bookmark title can have at most {MAX_TITLE_CHARS} characters.")); }
    if title.chars().any(char::is_control) { return Err("A bookmark title cannot contain control characters.".into()); }
    if title.chars().any(|c| matches!(c, '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}' | '\u{200B}'..='\u{200F}' | '\u{FEFF}' | '\u{2060}' | '\u{00AD}')) { return Err("A bookmark title cannot contain invisible or text-direction control characters.".into()); }
    Ok(title.to_owned())
}

fn pdf_text(value: &str) -> Object {
    if value.is_ascii() { Object::string_literal(value.as_bytes().to_vec()) } else {
        let mut bytes = vec![0xfe, 0xff]; for unit in value.encode_utf16() { bytes.extend(unit.to_be_bytes()); }
        Object::string_literal(bytes)
    }
}

fn destination_page(document: &Document, dict: &lopdf::Dictionary, pages: &HashMap<ObjectId, usize>) -> Option<usize> {
    let array_page = |object: &Object| -> Option<usize> {
        let (_, object) = document.dereference(object).ok()?;
        match object.as_array().ok()?.first()? { Object::Reference(id) => pages.get(id).copied(), _ => None }
    };
    if let Ok(dest) = dict.get(b"Dest") { return array_page(dest); }
    let (_, action) = document.dereference(dict.get(b"A").ok()?).ok()?;
    let action = action.as_dict().ok()?;
    if action.get(b"S").ok()?.as_name().ok()? != b"GoTo" { return None; }
    array_page(action.get(b"D").ok()?)
}

/// The catalog and every page-tree node and page: objects the outline must never alias.
fn protected_ids(document: &Document) -> HashSet<ObjectId> {
    let mut ids = HashSet::new();
    if let Ok(Object::Reference(root)) = document.trailer.get(b"Root") { ids.insert(*root); }
    ids.extend(document.get_pages().values().copied());
    let mut queue: Vec<ObjectId> = match document.catalog().ok().and_then(|catalog| catalog.get(b"Pages").ok()) { Some(Object::Reference(id)) => vec![*id], _ => Vec::new() };
    let mut visited = HashSet::new();
    while let Some(id) = queue.pop() {
        if visited.len() > 200_000 || !visited.insert(id) { continue; }
        ids.insert(id);
        if let Ok(kids) = document.get_dictionary(id).and_then(|dict| dict.get(b"Kids")).and_then(|kids| kids.as_array()) {
            for kid in kids { if let Object::Reference(kid) = kid { queue.push(*kid); } }
        }
    }
    ids
}

/// Reads the whole outline with a bounded, cycle-checked walk. A malformed outline (cycle, shared item,
/// wrong object types, more than 1,000 entries, more than 32 levels) is refused rather than guessed at.
/// A wrong `/Count` is not an error: counts are regenerated when a copy is written.
pub fn read(document: &Document) -> Result<Vec<Entry>, String> {
    let catalog = document.catalog().map_err(|error| error.to_string())?;
    let (root, root_id) = match catalog.get(b"Outlines") {
        Err(_) | Ok(Object::Null) => return Ok(Vec::new()),
        Ok(Object::Reference(id)) => (document.get_dictionary(*id).map_err(|_| "The bookmark list is not a dictionary.".to_owned())?, Some(*id)),
        Ok(Object::Dictionary(dict)) => (dict, None),
        Ok(_) => return Err("The bookmarks use an unsupported structure.".into()),
    };
    let protected = protected_ids(document);
    if root_id.is_some_and(|id| protected.contains(&id)) { return Err("The bookmark list points at the page tree or catalog, so the bookmarks cannot be edited safely.".into()); }
    if let Ok(kind) = root.get(b"Type") { if kind.as_name().ok() != Some(b"Outlines".as_slice()) { return Err("The bookmark list has an unexpected type, so the bookmarks cannot be edited safely.".into()); } }
    let pages: HashMap<ObjectId, usize> = document.get_pages().values().enumerate().map(|(index, id)| (*id, index)).collect();
    let mut entries: Vec<Entry> = Vec::new();
    let mut seen = HashSet::new();
    let mut bytes = 0usize;
    let mut stack: Vec<Option<ObjectId>> = vec![reference(root.get(b"First").ok())?];
    let mut parents: Vec<Option<ObjectId>> = vec![root_id];
    while let Some(top) = stack.last().copied() {
        let Some(id) = top else { stack.pop(); parents.pop(); continue };
        if entries.len() >= MAX_ENTRIES { return Err(format!("This document has more than {MAX_ENTRIES} bookmarks, which is more than can be edited.")); }
        if !seen.insert(id) { return Err("The bookmarks contain a cycle or a shared item, so they cannot be edited safely.".into()); }
        if protected.contains(&id) { return Err("A bookmark points at the page tree or catalog, so the bookmarks cannot be edited safely.".into()); }
        let dict = document.get_dictionary(id).map_err(|_| "A bookmark is not a dictionary.".to_owned())?;
        if let Ok(kind) = dict.get(b"Type") { if kind.as_name().ok() != Some(b"Outline".as_slice()) { return Err("A bookmark has an unexpected type, so the bookmarks cannot be edited safely.".into()); } }
        if let Some(expected) = parents[stack.len() - 1] {
            match dict.get(b"Parent") { Ok(Object::Reference(parent)) if *parent == expected => {} Err(_) => {} _ => return Err("A bookmark's parent link does not match the outline tree, so the bookmarks cannot be edited safely.".into()) }
        }
        let depth = stack.len() - 1;
        if depth >= MAX_DEPTH { return Err(format!("The bookmarks are nested more than {MAX_DEPTH} levels deep, so they cannot be edited.")); }
        let last = stack.len() - 1;
        stack[last] = reference(dict.get(b"Next").ok())?;
        let first = reference(dict.get(b"First").ok())?;
        let title = dict.get(b"Title").ok().and_then(|value| document.dereference(value).ok()).and_then(|(_, value)| lopdf::decode_text_string(value).ok()).filter(|title| !title.is_empty()).unwrap_or_else(|| "Untitled bookmark".into());
        bytes = bytes.saturating_add(title.len());
        if bytes > MAX_TOTAL_TITLE_BYTES { return Err("Bookmarks contain too much text.".into()); }
        let count = dict.get(b"Count").ok().and_then(|value| document.dereference(value).ok()).and_then(|(_, value)| value.as_i64().ok()).unwrap_or(0);
        entries.push(Entry { uid: entries.len() as u32, origin: Some(id), title, renamed: false, depth, open: count > 0, source_page: destination_page(document, dict, &pages) });
        if first.is_some() { stack.push(first); parents.push(Some(id)); }
    }
    refuse_aliased_items(document, &seen, root_id)?;
    Ok(entries)
}

/// An outline item may be referenced only by the outline's own tree links. Anything reachable from the trailer
/// that points at one (an AcroForm field, the `/Info` dictionary, an annotation or action, a name tree node)
/// means the item is really another object, which a save would then rewrite. One linear walk over the objects
/// reachable from the trailer; unreachable leftovers (for example deleted bookmarks) are ignored.
fn refuse_aliased_items(document: &Document, items: &HashSet<ObjectId>, root_id: Option<ObjectId>) -> Result<(), String> {
    const LINKS: [&[u8]; 5] = [b"Parent", b"First", b"Last", b"Prev", b"Next"];
    let refusal = || "A bookmark is also used by something else in the document, so the bookmarks cannot be edited safely.".to_owned();
    let catalog = document.trailer.get(b"Root").ok().and_then(|root| root.as_reference().ok());
    let mut visited: HashSet<ObjectId> = HashSet::new();
    let mut pending: Vec<&Object> = document.trailer.iter().map(|(_, value)| value).collect();
    while let Some(object) = pending.pop() {
        match object {
            Object::Reference(id) => {
                if items.contains(id) { return Err(refusal()); }
                if Some(*id) == root_id || !visited.insert(*id) { continue; }
                let Some(target) = document.objects.get(id) else { continue };
                match target {
                    Object::Dictionary(dict) if Some(*id) == catalog => pending.extend(dict.iter().filter(|(key, _)| key.as_slice() != b"Outlines").map(|(_, value)| value)),
                    other => pending.push(other),
                }
            }
            Object::Array(values) => pending.extend(values.iter()),
            Object::Dictionary(dict) => pending.extend(dict.iter().map(|(_, value)| value)),
            Object::Stream(stream) => pending.extend(stream.dict.iter().map(|(_, value)| value)),
            _ => {}
        }
    }
    for id in items {
        if let Some(Object::Dictionary(dict)) = document.objects.get(id) {
            if dict.iter().any(|(key, value)| !LINKS.contains(&key.as_slice()) && matches!(value, Object::Reference(target) if items.contains(target))) { return Err(refusal()); }
        }
    }
    Ok(())
}

/// Writes the entries as the document's outline. `page_object` maps a source page index to its page object,
/// or `None` when that page is not in the output; a bookmark added in this session whose page is not in the
/// output is left out and its children move up one level (an existing bookmark is never dropped this way).
pub fn write(document: &mut Document, entries: &[Entry], page_object: &dyn Fn(usize) -> Option<ObjectId>) -> Result<(), String> {
    valid_structure(entries)?;
    struct Kept<'a> { entry: &'a Entry, depth: usize, page: Option<ObjectId> }
    let mut kept: Vec<Kept> = Vec::with_capacity(entries.len());
    let mut skipped: Vec<usize> = Vec::new();
    for entry in entries {
        while skipped.last().is_some_and(|depth| *depth >= entry.depth) { skipped.pop(); }
        let page = entry.source_page.and_then(page_object);
        if entry.origin.is_none() && page.is_none() { skipped.push(entry.depth); continue; }
        kept.push(Kept { entry, depth: entry.depth - skipped.len(), page });
    }
    let existing_root = match document.catalog().map_err(|error| error.to_string())?.get(b"Outlines") {
        Err(_) | Ok(Object::Null) => None,
        Ok(Object::Reference(id)) => Some(Ok(*id)),
        Ok(Object::Dictionary(dict)) => Some(Err(dict.clone())),
        Ok(_) => return Err("The bookmarks use an unsupported structure.".into()),
    };
    if kept.is_empty() {
        document.catalog_mut().map_err(|error| error.to_string())?.remove(b"Outlines");
        return Ok(());
    }
    let count = kept.len();
    let (mut parent, mut first_child, mut last_child, mut previous, mut next) = (vec![None; count], vec![None; count], vec![None; count], vec![None; count], vec![None::<usize>; count]);
    let (mut root_first, mut root_last) = (None, None);
    let mut ancestors: Vec<usize> = Vec::new();
    for index in 0..count {
        let depth = kept[index].depth;
        ancestors.truncate(depth);
        if ancestors.len() != depth { return Err("The bookmark tree is not well formed.".into()); }
        parent[index] = ancestors.last().copied();
        let before = match parent[index] { Some(p) => last_child[p], None => root_last };
        previous[index] = before;
        if let Some(before) = before { next[before] = Some(index); }
        match parent[index] {
            Some(p) => { if first_child[p].is_none() { first_child[p] = Some(index); } last_child[p] = Some(index); }
            None => { if root_first.is_none() { root_first = Some(index); } root_last = Some(index); }
        }
        ancestors.push(index);
    }
    let mut visible = vec![0i64; count];
    let mut root_visible = 0i64;
    for index in (0..count).rev() {
        let contribution = 1 + if kept[index].entry.open { visible[index] } else { 0 };
        match parent[index] { Some(p) => visible[p] += contribution, None => root_visible += contribution }
    }
    let root_id = match &existing_root { Some(Ok(id)) => *id, _ => document.new_object_id() };
    let ids: Vec<ObjectId> = kept.iter().map(|item| item.entry.origin.unwrap_or_else(|| document.new_object_id())).collect();
    for index in 0..count {
        let item = &kept[index];
        let mut dict = match item.entry.origin {
            Some(origin) => document.get_dictionary(origin).map_err(|_| "A bookmark is no longer a dictionary.".to_owned())?.clone(),
            None => {
                let page = item.page.ok_or("A new bookmark has no page.")?;
                dictionary! { "Dest" => vec![Object::Reference(page), Object::Name(b"XYZ".to_vec()), Object::Null, Object::Null, Object::Null] }
            }
        };
        if item.entry.origin.is_none() || item.entry.renamed { dict.set("Title", pdf_text(&item.entry.title)); }
        dict.set("Parent", Object::Reference(parent[index].map_or(root_id, |p| ids[p])));
        for (key, link) in [("First", first_child[index]), ("Last", last_child[index]), ("Prev", previous[index]), ("Next", next[index])] {
            match link { Some(target) => dict.set(key, Object::Reference(ids[target])), None => { dict.remove(key.as_bytes()); } }
        }
        match first_child[index] {
            Some(_) => dict.set("Count", if item.entry.open { visible[index] } else { -visible[index] }),
            None => { dict.remove(b"Count"); }
        }
        document.objects.insert(ids[index], Object::Dictionary(dict));
    }
    let mut root = match existing_root {
        Some(Ok(id)) => document.get_dictionary(id).map_err(|_| "The bookmark list is not a dictionary.".to_owned())?.clone(),
        Some(Err(dict)) => dict,
        None => dictionary! { "Type" => "Outlines" },
    };
    root.set("First", Object::Reference(ids[root_first.ok_or("The bookmark tree is empty.")?]));
    root.set("Last", Object::Reference(ids[root_last.ok_or("The bookmark tree is empty.")?]));
    root.set("Count", root_visible);
    document.objects.insert(root_id, Object::Dictionary(root));
    document.catalog_mut().map_err(|error| error.to_string())?.set("Outlines", Object::Reference(root_id));
    Ok(())
}

#[cfg(test)]
#[path = "outline_edit_unit_tests.rs"]
pub(crate) mod tests;

impl OutlineEdits {
    pub fn loaded(&self) -> bool { self.baseline.is_some() }
    pub fn load(&mut self, result: Result<Vec<Entry>, String>) {
        if let Ok(entries) = &result { self.current = entries.clone(); self.next_uid = entries.len() as u32; }
        self.baseline = Some(result);
    }
    /// Drops every pending bookmark edit and returns to the source outline.
    pub fn discard(&mut self) {
        if let Some(Ok(baseline)) = &self.baseline { self.current = baseline.clone(); }
        self.undo.clear(); self.redo.clear(); self.version += 1;
    }
    pub fn changed(&self) -> bool { matches!(&self.baseline, Some(Ok(baseline)) if *baseline != self.current) }
    /// The entries to write into a copy, when they differ from the source outline.
    pub fn pending(&self) -> Option<&[Entry]> { self.changed().then_some(self.current.as_slice()) }
    fn find(&self, uid: u32) -> Result<usize, String> { self.current.iter().position(|entry| entry.uid == uid).ok_or_else(|| "That bookmark no longer exists.".to_owned()) }

    pub fn view(&self, plan: &[PageSpec]) -> EditView {
        let (editable, reason) = match &self.baseline { Some(Ok(_)) => (true, None), Some(Err(reason)) => (false, Some(reason.clone())), None => (false, Some("Bookmarks have not been loaded.".into())) };
        let mut position = HashMap::new();
        for (planned, spec) in plan.iter().enumerate() { position.entry(spec.source).or_insert(planned); }
        let entries = if editable { self.current.iter().enumerate().map(|(index, entry)| ViewEntry {
            uid: entry.uid, title: entry.title.clone(), depth: entry.depth, page: entry.source_page.and_then(|source| position.get(&source).copied()),
            open: entry.open, has_children: self.current.get(index + 1).is_some_and(|following| following.depth > entry.depth), added: entry.origin.is_none(),
        }).collect() } else { Vec::new() };
        EditView { editable, reason, version: self.version, entries, can_undo: !self.undo.is_empty(), can_redo: !self.redo.is_empty(), changed: self.changed() }
    }

    pub fn apply(&mut self, version: u64, op: BookmarkOp, plan: &[PageSpec]) -> Result<(), String> {
        if let Some(Err(reason)) = &self.baseline { return Err(reason.clone()); }
        if self.baseline.is_none() { return Err("Bookmarks have not been loaded.".into()); }
        if version != self.version { return Err("The bookmarks changed. Reload them and try again.".into()); }
        match op {
            BookmarkOp::Undo => { let previous = self.undo.pop_back().ok_or("Nothing to undo.")?; let current = std::mem::replace(&mut self.current, previous); self.redo.push(current); self.version += 1; return Ok(()); }
            BookmarkOp::Redo => { let next = self.redo.pop().ok_or("Nothing to redo.")?; let current = std::mem::replace(&mut self.current, next); self.undo.push_back(current); self.version += 1; return Ok(()); }
            _ => {}
        }
        let mut next = self.current.clone();
        let mut next_uid = self.next_uid;
        match op {
            BookmarkOp::Add { page, title, after } => {
                let source_page = plan.get(page).ok_or("Page is out of range.")?.source;
                let title = clean_title(&title)?;
                let (at, depth) = match after { Some(uid) => { let index = self.find(uid)?; (subtree_end(&next, index), next[index].depth) } None => (next.len(), 0) };
                if next.len() >= MAX_ENTRIES { return Err(format!("A document can have at most {MAX_ENTRIES} editable bookmarks.")); }
                next.insert(at, Entry { uid: next_uid, origin: None, title, renamed: false, depth, open: true, source_page: Some(source_page) });
                next_uid = next_uid.checked_add(1).ok_or("Too many bookmark edits in this session.")?;
            }
            BookmarkOp::Rename { uid, title } => {
                let index = self.find(uid)?; let title = clean_title(&title)?;
                if next[index].title != title { next[index].title = title; next[index].renamed = true; }
            }
            BookmarkOp::Delete { uid } => { let index = self.find(uid)?; let end = subtree_end(&next, index); next.drain(index..end); }
            BookmarkOp::MoveUp { uid } => {
                let index = self.find(uid)?; let end = subtree_end(&next, index);
                let before = previous_sibling(&next, index).ok_or("This bookmark is already first at its level.")?;
                next[before..end].rotate_left(index - before);
            }
            BookmarkOp::MoveDown { uid } => {
                let index = self.find(uid)?; let end = subtree_end(&next, index);
                if end >= next.len() || next[end].depth != next[index].depth { return Err("This bookmark is already last at its level.".into()); }
                let after = subtree_end(&next, end);
                next[index..after].rotate_left(end - index);
            }
            BookmarkOp::Indent { uid } => {
                let index = self.find(uid)?; let end = subtree_end(&next, index);
                let parent = previous_sibling(&next, index).ok_or("There is no bookmark above at this level to nest under.")?;
                if next[index..end].iter().any(|entry| entry.depth + 1 >= MAX_DEPTH) { return Err(format!("Bookmarks can be nested at most {MAX_DEPTH} levels deep.")); }
                for entry in &mut next[index..end] { entry.depth += 1; }
                next[parent].open = true;
            }
            BookmarkOp::Outdent { uid } => {
                let index = self.find(uid)?;
                if next[index].depth == 0 { return Err("This bookmark is already at the top level.".into()); }
                let end = subtree_end(&next, index);
                let parent = (0..index).rev().find(|candidate| next[*candidate].depth + 1 == next[index].depth).ok_or("The bookmark tree is not well formed.")?;
                let parent_end = subtree_end(&next, parent);
                let mut block: Vec<Entry> = next.drain(index..end).collect();
                for entry in &mut block { entry.depth -= 1; }
                let at = parent_end - block.len();
                for (offset, entry) in block.into_iter().enumerate() { next.insert(at + offset, entry); }
            }
            BookmarkOp::SetOpen { uid, open } => {
                let index = self.find(uid)?;
                if next.get(index + 1).is_some_and(|following| following.depth > next[index].depth) { next[index].open = open; }
            }
            BookmarkOp::Undo | BookmarkOp::Redo => unreachable!(),
        }
        valid_structure(&next)?;
        if next == self.current { return Ok(()); }
        let previous = std::mem::replace(&mut self.current, next);
        self.next_uid = next_uid;
        self.undo.push_back(previous);
        self.redo.clear();
        while self.undo.len() > MAX_UNDO || self.undo.iter().map(|snapshot| snapshot_bytes(snapshot)).sum::<usize>() > UNDO_BUDGET { self.undo.pop_front(); }
        self.version += 1;
        Ok(())
    }
}
