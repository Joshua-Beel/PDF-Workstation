//! Named destinations: the catalog's `/Dests` dictionary and the `/Names /Dests` name tree.
//!
//! `read` walks both with a bounded, cycle-checked reader (node, name, depth and reference-hop caps; a node
//! that is the catalog, a page or a page-tree node, or that appears twice, is refused). `prune` drops the
//! names whose destination targets a removed page by writing a fresh `/Dests` dictionary, name-tree leaf and
//! `/Names` dictionary and pointing the catalog at them, so no existing object is ever rewritten.
use lopdf::{Dictionary, Document, Object, ObjectId};
use std::collections::{HashMap, HashSet};

const MAX_NODES: usize = 4096;
const MAX_NAMES: usize = 65_536;
const MAX_DEPTH: usize = 32;
const MAX_HOPS: usize = 8;
pub const REFUSAL: &str = "The named destinations use an unsupported structure, so pages cannot be removed safely.";

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

/// The page object a destination targets: `[page ...]` or `<< /D [page ...] >>`, inline or indirect.
pub fn destination_target(document: &Document, value: &Object) -> Option<ObjectId> {
    let mut value = follow(document, value)?;
    if let Object::Dictionary(dict) = value { value = follow(document, dict.get(b"D").ok()?)?; }
    match value.as_array().ok()?.first()? { Object::Reference(id) => Some(*id), _ => None }
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
    let mut nodes = 0usize;
    let mut stack: Vec<(&Object, usize)> = vec![(root, 0)];
    while let Some((object, depth)) = stack.pop() {
        nodes += 1;
        if nodes > MAX_NODES || depth > MAX_DEPTH { return Err(REFUSAL.into()); }
        let node = structure(document, object, protected, seen)?.as_dict().map_err(|_| REFUSAL)?;
        if let Ok(kids) = node.get(b"Kids") {
            let kids = structure(document, kids, protected, seen)?.as_array().map_err(|_| REFUSAL)?;
            if kids.iter().any(|kid| !matches!(kid, Object::Reference(_))) { return Err(REFUSAL.into()); }
            stack.extend(kids.iter().rev().map(|kid| (kid, depth + 1)));
        } else if let Ok(names) = node.get(b"Names") {
            let names = structure(document, names, protected, seen)?.as_array().map_err(|_| REFUSAL)?;
            if names.len() % 2 != 0 || pairs.len() + names.len() / 2 > MAX_NAMES { return Err(REFUSAL.into()); }
            for pair in names.chunks(2) {
                if !matches!(pair[0], Object::String(..)) { return Err(REFUSAL.into()); }
                pairs.push((pair[0].clone(), pair[1].clone()));
            }
        }
    }
    Ok(pairs)
}

impl NamedDests {
    /// Name to target page object. A name in the name tree wins over the same name in `/Dests`, as in PDFium.
    pub fn targets(&self, document: &Document) -> HashMap<Vec<u8>, Option<ObjectId>> {
        let mut map = HashMap::new();
        for (key, value) in self.dests.iter().flatten() { map.insert(key.clone(), destination_target(document, value)); }
        for (key, value) in self.tree.iter().flatten() { if let Object::String(key, _) = key { map.insert(key.clone(), destination_target(document, value)); } }
        map
    }
}

/// Drops every named destination that targets a page in `removed`. Returns whether anything was dropped.
pub fn prune(document: &mut Document, removed: &HashSet<ObjectId>) -> Result<bool, String> {
    let named = read(document)?;
    let dead = |value: &Object| destination_target(document, value).is_some_and(|page| removed.contains(&page));
    let dests: Option<Dictionary> = named.dests.as_ref().filter(|entries| entries.iter().any(|(_, value)| dead(value))).map(|entries| {
        let mut dict = Dictionary::new();
        for (key, value) in entries.iter().filter(|(_, value)| !dead(value)) { dict.set(key.clone(), value.clone()); }
        dict
    });
    let tree: Option<Vec<Object>> = named.tree.as_ref().filter(|pairs| pairs.iter().any(|(_, value)| dead(value))).map(|pairs| {
        pairs.iter().filter(|(_, value)| !dead(value)).flat_map(|(key, value)| [key.clone(), value.clone()]).collect()
    });
    let names = match &tree {
        Some(_) => Some(follow(document, document.catalog().map_err(|error| error.to_string())?.get(b"Names").map_err(|_| REFUSAL)?).and_then(|names| names.as_dict().ok()).ok_or(REFUSAL)?.clone()),
        None => None,
    };
    let changed = dests.is_some() || tree.is_some();
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
    Ok(changed)
}
