//! Named destinations: the catalog's `/Dests` dictionary and the `/Names /Dests` name tree.
//!
//! `read` walks both with a bounded, cycle-checked reader (node, kid, name, depth and reference-hop caps; a
//! node that is the catalog, a page or a page-tree node, or that appears twice, is refused, and so is a name
//! that appears twice in the tree, since PDFium's /Limits search could pick either). `rewrite` drops the names
//! whose destination targets a removed page and turns page-number destinations (`[3 /Fit]`, which PDFium
//! follows as a page index) into page references, by writing a fresh `/Dests` dictionary, name-tree leaf and
//! `/Names` dictionary and pointing the catalog at them, so no existing object is ever rewritten.
use lopdf::{Dictionary, Document, Object, ObjectId};
use std::collections::{HashMap, HashSet};

const MAX_NODES: usize = 4096;
const MAX_NAMES: usize = 65_536;
const MAX_DEPTH: usize = 32;
const MAX_HOPS: usize = 8;
const VIEW_MODES: [&[u8]; 8] = [b"XYZ", b"Fit", b"FitH", b"FitV", b"FitR", b"FitB", b"FitBH", b"FitBV"];
pub const REFUSAL: &str = "The named destinations use an unsupported structure, so pages cannot be removed or reordered safely.";

/// The `/Dests` dictionary entries and the name-tree pairs, in document order; `None` when absent.
pub struct NamedDests { dests: Option<Vec<(Vec<u8>, Object)>>, tree: Option<Vec<(Object, Object)>> }

fn follow<'a>(document: &'a Document, mut object: &'a Object) -> Option<&'a Object> {
    for _ in 0..MAX_HOPS {
        match object { Object::Reference(id) => object = document.objects.get(id)?, other => return Some(other) }
    }
    None
}

fn structure<'a>(document: &'a Document, object: &'a Object, protected: &HashSet<ObjectId>, seen: &mut HashSet<ObjectId>) -> Result<&'a Object, String> {
    let mut cursor = object;
    for _ in 0..MAX_HOPS {
        match cursor {
            Object::Reference(id) => {
                if protected.contains(id) || !seen.insert(*id) { return Err(REFUSAL.into()); }
                cursor = document.objects.get(id).ok_or(REFUSAL)?;
            }
            other => return Ok(other),
        }
    }
    Err(REFUSAL.into())
}

/// The destination array of `value`: `[page ...]` or `<< /D [page ...] >>`, inline or indirect.
fn destination_array<'a>(document: &'a Document, value: &'a Object) -> Option<&'a Vec<Object>> {
    let mut value = follow(document, value)?;
    if let Object::Dictionary(dict) = value { value = follow(document, dict.get(b"D").ok()?)?; }
    value.as_array().ok()
}

/// A destination array whose first element is a page number (`[3 /Fit]`), in range for `page_count`.
fn page_number(array: &[Object], page_count: usize) -> Option<usize> {
    match (array.first()?, array.get(1)?) {
        (Object::Integer(number), Object::Name(mode)) if VIEW_MODES.contains(&mode.as_slice()) => usize::try_from(*number).ok().filter(|number| *number < page_count),
        _ => None,
    }
}

/// The page object a destination targets; a page number is resolved through `pages` (source page order).
pub fn destination_target(document: &Document, value: &Object, pages: &[ObjectId]) -> Option<ObjectId> {
    let array = destination_array(document, value)?;
    match array.first()? { Object::Reference(id) => Some(*id), _ => page_number(array, pages.len()).map(|number| pages[number]) }
}

/// The source page index of a page-number destination, or `None` for any other destination.
pub fn destination_page_number(document: &Document, value: &Object, page_count: usize) -> Option<usize> {
    page_number(destination_array(document, value)?, page_count)
}

/// A new, inline copy of `value` with its page number replaced by `page`, or `None` when `value` is not a
/// page-number destination. The original objects are left untouched.
pub fn with_page_reference(document: &Document, value: &Object, page: ObjectId, page_count: usize) -> Option<Object> {
    let fix = |array: &Vec<Object>| -> Option<Object> {
        page_number(array, page_count)?;
        let mut array = array.clone(); array[0] = Object::Reference(page); Some(Object::Array(array))
    };
    match follow(document, value)? {
        Object::Array(array) => fix(array),
        Object::Dictionary(dict) => {
            let fixed = fix(follow(document, dict.get(b"D").ok()?)?.as_array().ok()?)?;
            let mut dict = dict.clone(); dict.set("D", fixed); Some(Object::Dictionary(dict))
        }
        _ => None,
    }
}

/// Whether any destination-shaped array still starts with an in-range page number (remote `GoToR` / `GoToE`
/// targets excepted, where a number names a page of another file). Structural copies refuse rather than
/// leave such a destination pointing at whatever page now has that index.
pub fn page_number_destination_left(document: &Document, page_count: usize) -> bool {
    fn remote(dict: &Dictionary) -> bool { dict.get(b"S").and_then(Object::as_name).is_ok_and(|name| name == b"GoToR" || name == b"GoToE") }
    let mut pending: Vec<&Object> = document.objects.values().collect();
    pending.extend(document.trailer.iter().map(|(_, value)| value));
    while let Some(object) = pending.pop() {
        match object {
            Object::Array(values) => { if page_number(values, page_count).is_some() { return true; } pending.extend(values.iter()); }
            Object::Dictionary(dict) => { let skip = remote(dict); pending.extend(dict.iter().filter(|(key, _)| !(skip && key.as_slice() == b"D")).map(|(_, value)| value)); }
            Object::Stream(stream) => pending.extend(stream.dict.iter().map(|(_, value)| value)),
            _ => {}
        }
    }
    false
}

/// Whether the catalog's `/Names` holds anything besides `/Dests` (attachments, scripts, templates...), or is
/// not a dictionary at all. Page removal still refuses those.
pub fn names_besides_dests(document: &Document, names: &Object) -> bool {
    match follow(document, names) {
        Some(Object::Dictionary(dict)) => dict.iter().any(|(key, value)| key.as_slice() != b"Dests" && !matches!(value, Object::Null)),
        Some(Object::Null) => false,
        _ => true,
    }
}

pub fn read(document: &Document) -> Result<NamedDests, String> {
    let catalog = document.catalog().map_err(|error| error.to_string())?;
    let protected = crate::outline_edit::protected_ids(document);
    let mut seen = HashSet::new();
    let dests = match catalog.get(b"Dests") {
        Err(_) | Ok(Object::Null) => None,
        Ok(value) => {
            let dict = structure(document, value, &protected, &mut seen)?.as_dict().map_err(|_| REFUSAL)?;
            if dict.len() > MAX_NAMES { return Err(REFUSAL.into()); }
            Some(dict.iter().map(|(key, value)| (key.clone(), value.clone())).collect())
        }
    };
    let tree = match catalog.get(b"Names") {
        Err(_) | Ok(Object::Null) => None,
        Ok(names) => match structure(document, names, &protected, &mut seen)?.as_dict().map_err(|_| REFUSAL)?.get(b"Dests") {
            Err(_) | Ok(Object::Null) => None,
            Ok(root) => Some(walk(document, root, &protected, &mut seen)?),
        },
    };
    Ok(NamedDests { dests, tree })
}

fn walk(document: &Document, root: &Object, protected: &HashSet<ObjectId>, seen: &mut HashSet<ObjectId>) -> Result<Vec<(Object, Object)>, String> {
    let mut pairs = Vec::new();
    let mut keys: HashSet<Vec<u8>> = HashSet::new();
    let mut nodes = 0usize;
    let mut stack: Vec<(&Object, usize)> = vec![(root, 0)];
    while let Some((object, depth)) = stack.pop() {
        nodes += 1;
        if nodes > MAX_NODES || depth > MAX_DEPTH { return Err(REFUSAL.into()); }
        let node = structure(document, object, protected, seen)?.as_dict().map_err(|_| REFUSAL)?;
        if let Ok(kids) = node.get(b"Kids") {
            let kids = structure(document, kids, protected, seen)?.as_array().map_err(|_| REFUSAL)?;
            if nodes + stack.len() + kids.len() > MAX_NODES || kids.iter().any(|kid| !matches!(kid, Object::Reference(_))) { return Err(REFUSAL.into()); }
            stack.extend(kids.iter().rev().map(|kid| (kid, depth + 1)));
        } else if let Ok(names) = node.get(b"Names") {
            let names = structure(document, names, protected, seen)?.as_array().map_err(|_| REFUSAL)?;
            if names.len() % 2 != 0 || pairs.len() + names.len() / 2 > MAX_NAMES { return Err(REFUSAL.into()); }
            for pair in names.chunks(2) {
                let Object::String(key, _) = &pair[0] else { return Err(REFUSAL.into()) };
                if !keys.insert(key.clone()) { return Err(REFUSAL.into()); }
                pairs.push((pair[0].clone(), pair[1].clone()));
            }
        }
    }
    Ok(pairs)
}

impl NamedDests {
    /// Name to target page object. A name in the name tree wins over the same name in `/Dests`, as in PDFium.
    pub fn targets(&self, document: &Document) -> HashMap<Vec<u8>, Option<ObjectId>> {
        let pages: Vec<ObjectId> = document.get_pages().values().copied().collect();
        let mut map = HashMap::new();
        for (key, value) in self.dests.iter().flatten() { map.insert(key.clone(), destination_target(document, value, &pages)); }
        for (key, value) in self.tree.iter().flatten() { if let Object::String(key, _) = key { map.insert(key.clone(), destination_target(document, value, &pages)); } }
        map
    }
}

/// For a structural copy whose source pages are `pages` (source order): drops every named destination that
/// targets a page in `removed`, and turns page-number destinations into references to the page they named.
pub fn rewrite(document: &mut Document, pages: &[ObjectId], removed: &HashSet<ObjectId>) -> Result<(), String> {
    let named = read(document)?;
    let updated = |value: &Object| -> Option<Option<Object>> {
        let target = destination_target(document, value, pages);
        if target.is_some_and(|page| removed.contains(&page)) { return Some(None); }
        target.and_then(|page| with_page_reference(document, value, page, pages.len())).map(Some)
    };
    let dests: Option<Dictionary> = named.dests.as_ref().filter(|entries| entries.iter().any(|(_, value)| updated(value).is_some())).map(|entries| {
        let mut dict = Dictionary::new();
        for (key, value) in entries { match updated(value) { None => dict.set(key.clone(), value.clone()), Some(Some(fixed)) => dict.set(key.clone(), fixed), Some(None) => {} } }
        dict
    });
    let tree: Option<Vec<Object>> = named.tree.as_ref().filter(|pairs| pairs.iter().any(|(_, value)| updated(value).is_some())).map(|pairs| {
        pairs.iter().filter_map(|(key, value)| match updated(value) { None => Some([key.clone(), value.clone()]), Some(Some(fixed)) => Some([key.clone(), fixed]), Some(None) => None }).flatten().collect()
    });
    let names = match &tree {
        Some(_) => Some(follow(document, document.catalog().map_err(|error| error.to_string())?.get(b"Names").map_err(|_| REFUSAL)?).and_then(|names| names.as_dict().ok()).ok_or(REFUSAL)?.clone()),
        None => None,
    };
    if let Some(dests) = dests {
        let id = document.add_object(dests);
        document.catalog_mut().map_err(|error| error.to_string())?.set("Dests", Object::Reference(id));
    }
    if let (Some(pairs), Some(mut names)) = (tree, names) {
        let leaf = document.add_object(lopdf::dictionary! { "Names" => pairs });
        names.set("Dests", Object::Reference(leaf));
        let id = document.add_object(names);
        document.catalog_mut().map_err(|error| error.to_string())?.set("Names", Object::Reference(id));
    }
    Ok(())
}
