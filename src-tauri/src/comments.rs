use crate::editor::{CropBox, PageSpec};
use lopdf::{dictionary, Dictionary, Document, Object, ObjectId, Stream, StringFormat};
use serde::Serialize;
use std::collections::HashSet;

pub const MAX_NOTES: usize = 1_000;
pub const MAX_TEXT_BYTES: usize = 8 * 1024;
pub const MAX_TOTAL_BYTES: usize = 1024 * 1024;
pub const MAX_HIGHLIGHT_QUADS: usize = 256;
pub const MAX_GEOMETRY_SLOTS: usize = 4_096;
const PREFIX: &str = "pdf-workstation-note-";
const HIGHLIGHT_PREFIX: &str = "pdf-workstation-highlight-";

#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum AnnotationKind { Note, Highlight }

#[derive(Clone, Debug, PartialEq)]
pub struct Note { pub id: String, pub rect: CropBox, pub contents: String, pub kind: AnnotationKind, pub quads: Option<Vec<CropBox>> }
#[derive(Clone, Debug, Serialize)]
pub struct DisplayRect { pub x: f64, pub y: f64, pub width: f64, pub height: f64 }
#[derive(Serialize)]
pub struct CommentInfo { pub id: String, pub page: usize, pub rect: Option<DisplayRect>, pub contents: String }
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CommentList { pub document_id: u64, pub revision: u64, pub status: &'static str, pub reason: Option<String>, pub notes: Vec<CommentInfo> }
#[derive(Serialize)]
pub struct AnnotationInfo { pub id: String, pub kind: AnnotationKind, pub page: usize, pub rect: Option<DisplayRect>, pub contents: Option<String>, pub quads: Option<Vec<DisplayRect>> }
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AnnotationList { pub document_id: u64, pub revision: u64, pub status: &'static str, pub reason: Option<String>, pub annotations: Vec<AnnotationInfo> }

pub fn highlight_contents(contents: Option<&str>) -> Result<String, String> {
    let contents = contents.unwrap_or("");
    if contents.contains('\0') || contents.len() > MAX_TEXT_BYTES { return Err("Highlight text must be NUL-free and limited to 8 KiB of UTF-8.".into()); }
    Ok(if contents.trim().is_empty() { String::new() } else { contents.to_owned() })
}

pub fn validate_text(contents: &str) -> Result<(), String> {
    if contents.trim().is_empty() { return Err("Enter some text for the note.".into()); }
    if contents.contains('\0') { return Err("Notes cannot contain a null character.".into()); }
    if contents.len() > MAX_TEXT_BYTES { return Err("Each note is limited to 8 KiB of UTF-8 text.".into()); }
    Ok(())
}
pub fn validate_plan(plan: &[PageSpec]) -> Result<(), String> {
    let mut count = 0usize; let mut total = 0usize; let mut slots = 0usize; let mut ids = HashSet::new();
    for note in plan.iter().flat_map(|page| &page.notes) {
        if let Some(quads) = &note.quads {
            if note.kind != AnnotationKind::Highlight || highlight_union(quads)? != note.rect { return Err("The annotation has invalid text-highlight quadrilaterals.".into()); }
        }
        match note.kind { AnnotationKind::Note => validate_text(&note.contents)?, AnnotationKind::Highlight => { if highlight_contents(Some(&note.contents))? != note.contents { return Err("The highlight has a noncanonical empty body.".into()); } } }
        number(&note.id)?;
        if note.id.starts_with(PREFIX) != (note.kind == AnnotationKind::Note) { return Err("The annotation ID does not match its kind.".into()); }
        if !ids.insert(&note.id) { return Err("The PDF contains duplicate note IDs. Commenting is unavailable.".into()); }
        count += 1; total += note.contents.len();
        slots += note.quads.as_ref().map_or(1, Vec::len);
    }
    if count > MAX_NOTES { return Err("This document is limited to 1,000 notes and highlights in total.".into()); }
    if total > MAX_TOTAL_BYTES { return Err("This document is limited to 1 MiB of total annotation text.".into()); }
    if slots > MAX_GEOMETRY_SLOTS { return Err("This document is limited to 4,096 annotation geometry slots; each text-highlight character uses one slot and each note or area highlight uses one.".into()); }
    Ok(())
}
pub fn number(id: &str) -> Result<u64, String> {
    let suffix = id.strip_prefix(PREFIX).or_else(|| id.strip_prefix(HIGHLIGHT_PREFIX)).ok_or("The PDF has a foreign annotation. Commenting is unavailable.")?;
    let value = suffix.parse::<u64>().map_err(|_| "The PDF has an invalid note ID.")?;
    if value == 0 || suffix != value.to_string() { return Err("The PDF has an invalid note ID.".into()); }
    Ok(value)
}
pub fn id(value: u64) -> String { format!("{PREFIX}{value}") }
pub fn highlight_id(value: u64) -> String { format!("{HIGHLIGHT_PREFIX}{value}") }
fn quad(bounds: CropBox) -> Vec<f32> { vec![bounds.left, bounds.bottom, bounds.right, bounds.bottom, bounds.right, bounds.top, bounds.left, bounds.top] }
pub fn highlight_union(quads: &[CropBox]) -> Result<CropBox, String> {
    if quads.is_empty() || quads.len() > MAX_HIGHLIGHT_QUADS { return Err("A text highlight needs 1 through 256 positioned characters.".into()); }
    let mut bounds = quads[0];
    for rect in quads {
        if ![rect.left, rect.bottom, rect.right, rect.top].iter().all(|value| value.is_finite()) || !((rect.right - rect.left).is_finite() && (rect.top - rect.bottom).is_finite()) || rect.right <= rect.left || rect.top <= rect.bottom { return Err("The text highlight has invalid bounds.".into()); }
        bounds.left = bounds.left.min(rect.left); bounds.bottom = bounds.bottom.min(rect.bottom); bounds.right = bounds.right.max(rect.right); bounds.top = bounds.top.max(rect.top);
    }
    if !(bounds.right - bounds.left).is_finite() || !(bounds.top - bounds.bottom).is_finite() { return Err("The text highlight has invalid total bounds.".into()); }
    Ok(bounds)
}
fn read_quads(value: &Object, bounds: CropBox) -> Result<Vec<CropBox>, String> {
    let count = value.as_array().map_err(|error| error.to_string())?.len();
    if count == 0 || count % 8 != 0 || count > MAX_HIGHLIGHT_QUADS * 8 { return Err("The PDF has invalid highlight quadrilaterals.".into()); }
    let values = values(value)?;
    if values.is_empty() || values.len() % 8 != 0 || values.len() > MAX_HIGHLIGHT_QUADS * 8 { return Err("The PDF has invalid highlight quadrilaterals.".into()); }
    let quads = values.chunks_exact(8).map(|points| {
        let rect = CropBox { left: points[0], bottom: points[1], right: points[2], top: points[5] };
        if points != quad(rect) { return Err("The PDF has altered highlight quadrilaterals.".into()); } Ok(rect)
    }).collect::<Result<Vec<_>, String>>()?;
    if highlight_union(&quads)? != bounds { return Err("The highlight bounds do not match its quadrilaterals.".into()); }
    Ok(quads)
}
fn keys(value: &Dictionary, allowed: &[&[u8]]) -> Result<(), String> {
    if value.iter().any(|(key, _)| !allowed.contains(&key.as_slice())) { return Err("The PDF has an unknown annotation or appearance feature. Commenting is unavailable.".into()); }
    Ok(())
}
fn name(value: &Dictionary, key: &[u8], expected: &[u8]) -> Result<(), String> {
    if value.get(key).and_then(Object::as_name).ok() != Some(expected) { return Err("The PDF has an unsupported annotation schema. Commenting is unavailable.".into()); }
    Ok(())
}
fn values(value: &Object) -> Result<Vec<f32>, String> {
    value.as_array().map_err(|error| error.to_string())?.iter().map(|value| value.as_float().map_err(|error| error.to_string())).collect()
}
fn rect(value: &Object) -> Result<CropBox, String> {
    let value = values(value)?;
    if value.len() != 4 || !value.iter().all(|value| value.is_finite()) || !(value[2] - value[0]).is_finite() || !(value[3] - value[1]).is_finite() || value[2] - value[0] < 1.0 || value[3] - value[1] < 1.0 { return Err("The PDF has invalid note bounds. Commenting is unavailable.".into()); }
    Ok(CropBox { left: value[0], bottom: value[1], right: value[2], top: value[3] })
}
fn highlight_rect(value: &Object) -> Result<CropBox, String> {
    let value = values(value)?;
    if value.len() != 4 { return Err("The PDF has invalid highlight bounds.".into()); }
    let bounds = CropBox { left: value[0], bottom: value[1], right: value[2], top: value[3] }; highlight_union(&[bounds])?; Ok(bounds)
}
fn media_box(document: &Document, mut page: ObjectId) -> Result<CropBox, String> {
    let mut visited = HashSet::new();
    loop {
        if !visited.insert(page) { return Err("The PDF has a cyclic page boundary inheritance.".into()); }
        let dictionary = document.get_dictionary(page).map_err(|error| error.to_string())?;
        if let Ok(value) = dictionary.get(b"MediaBox") {
            let value = document.dereference(value).map_err(|error| error.to_string())?.1.as_array().map_err(|error| error.to_string())?;
            let values = value.iter().map(|value| document.dereference(value).map(|(_, value)| value.clone()).map_err(|error| error.to_string())).collect::<Result<Vec<_>, _>>()?;
            return rect(&Object::Array(values));
        }
        page = dictionary.get(b"Parent").and_then(Object::as_reference).map_err(|_| "The note page has no supported MediaBox.")?;
    }
}
fn appearance(bounds: CropBox, kind: AnnotationKind) -> Vec<u8> {
    let (w, h) = (bounds.right - bounds.left, bounds.top - bounds.bottom);
    if kind == AnnotationKind::Highlight { return format!("q /GS0 gs 1 1 0 rg 0 0 {w:.6} {h:.6} re f Q\n").into_bytes(); }
    format!("q 1 0.85 0 rg 0 0 {w:.6} {h:.6} re f 0.25 0.2 0 RG 0.25 w {x:.6} {a:.6} m {r:.6} {a:.6} l {x:.6} {b:.6} m {r:.6} {b:.6} l S Q\n", x=w*0.2, r=w*0.8, a=h*0.65, b=h*0.4).into_bytes()
}
fn text_appearance(bounds: CropBox, quads: &[CropBox]) -> Vec<u8> {
    let mut content = String::from("q /GS0 gs 1 1 0 rg ");
    for rect in quads { content.push_str(&format!("{x:.6} {y:.6} {w:.6} {h:.6} re ", x=rect.left-bounds.left, y=rect.bottom-bounds.bottom, w=rect.right-rect.left, h=rect.top-rect.bottom)); }
    content.push_str("f Q\n"); content.into_bytes()
}
fn appearance_dict(bounds: CropBox, kind: AnnotationKind) -> Dictionary {
    let resources = if kind == AnnotationKind::Highlight { dictionary! { "ExtGState" => dictionary! { "GS0" => dictionary! { "Type" => "ExtGState", "BM" => "Multiply", "ca" => Object::Real(0.25), "CA" => Object::Real(0.25) } } } } else { dictionary! {} };
    dictionary! { "Type" => "XObject", "Subtype" => "Form", "FormType" => 1,
        "BBox" => vec![0.into(), 0.into(), Object::Real(bounds.right - bounds.left), Object::Real(bounds.top - bounds.bottom)], "Resources" => resources }
}
fn decode_text(value: &Object) -> Result<String, String> {
    let bytes = value.as_str().map_err(|error| error.to_string())?;
    if bytes.len() > MAX_TEXT_BYTES * 2 + 2 { return Err("Each note is limited to 8 KiB of UTF-8 text.".into()); }
    if bytes.len() < 2 || bytes[..2] != [0xfe, 0xff] || bytes.len() % 2 != 0 { return Err("The PDF has unsupported note text encoding.".into()); }
    let units = bytes[2..].chunks_exact(2).map(|pair| u16::from_be_bytes([pair[0], pair[1]])).collect::<Vec<_>>();
    String::from_utf16(&units).map_err(|_| "The PDF has invalid Unicode note text.".into())
}
fn encode_text(value: &str) -> Object {
    let mut bytes = vec![0xfe, 0xff]; for unit in value.encode_utf16() { bytes.extend(unit.to_be_bytes()); }
    Object::String(bytes, StringFormat::Hexadecimal)
}
fn effective_version(document: &Document) -> Result<(u8, u8), String> {
    let parse = |value: &str| match value {
        "1.0" => Ok((1, 0)), "1.1" => Ok((1, 1)), "1.2" => Ok((1, 2)), "1.3" => Ok((1, 3)),
        "1.4" => Ok((1, 4)), "1.5" => Ok((1, 5)), "1.6" => Ok((1, 6)), "1.7" => Ok((1, 7)), "2.0" => Ok((2, 0)),
        _ => Err("Commenting on this PDF version is not supported in this build.".to_owned()),
    };
    let header = parse(&document.version)?;
    let catalog = document.catalog().map_err(|error| error.to_string())?;
    let declared = catalog.get(b"Version").ok().map(|value| {
        let value = document.dereference(value).map_err(|error| error.to_string())?.1.as_name().map_err(|error| error.to_string())?;
        parse(std::str::from_utf8(value).map_err(|error| error.to_string())?)
    }).transpose()?;
    Ok(declared.map_or(header, |declared| declared.max(header)))
}

pub fn read(document: &Document) -> Result<Vec<Vec<Note>>, String> {
    if document.is_encrypted() || document.encryption_state.is_some() { return Err("Encrypted PDFs cannot be commented on in this build.".into()); }
    let catalog = document.catalog().map_err(|error| error.to_string())?;
    effective_version(document)?;
    if catalog.has(b"Perms") || document.objects.values().any(|object| object.as_dict().is_ok_and(|dict| dict.has(b"ByteRange") || dict.get(b"Type").is_ok_and(|value| value.as_name().ok() == Some(b"Sig")))) { return Err("Signed or certified PDFs cannot be commented on in this build.".into()); }
    if [b"AcroForm".as_slice(), b"StructTreeRoot", b"PageLabels", b"Threads"].iter().any(|key| catalog.has(key)) { return Err("Commenting on forms, tagged PDFs, page labels, or article threads is not supported in this build.".into()); }
    let pages = document.get_pages(); let page_ids = pages.values().copied().collect::<HashSet<_>>(); let mut result = Vec::with_capacity(pages.len()); let mut appearance_ids = HashSet::new();
    let mut annotation_ids = HashSet::new(); let mut note_ids = HashSet::new(); let mut count = 0usize; let mut text_bytes = 0usize; let mut geometry_slots = 0usize;
    for page_id in pages.into_values() {
        let page = document.get_dictionary(page_id).map_err(|error| error.to_string())?;
        let mut notes = Vec::new();
        if let Ok(annots) = page.get(b"Annots") {
            let annots = annots.as_array().map_err(|_| "The PDF has unsupported indirect or malformed annotations.")?;
            if annots.is_empty() { return Err("The PDF has an unsupported empty annotation array. Commenting is unavailable.".into()); }
            for annotation in annots {
                let annotation_id = annotation.as_reference().map_err(|_| "The PDF has unsupported inline annotations.")?;
                if !annotation_ids.insert(annotation_id) { return Err("The PDF repeats an annotation object. Commenting is unavailable.".into()); }
                let annotation = document.get_dictionary(annotation_id).map_err(|error| error.to_string())?;
                let kind = match annotation.get(b"Subtype").and_then(Object::as_name).ok() { Some(b"Text") => AnnotationKind::Note, Some(b"Highlight") => AnnotationKind::Highlight, _ => return Err("The PDF has a foreign annotation subtype.".into()) };
                match kind {
                    AnnotationKind::Note => { keys(annotation, &[b"Type", b"Subtype", b"Rect", b"NM", b"Contents", b"Name", b"C", b"Open", b"F", b"P", b"AP"])?; name(annotation, b"Name", b"Note")?; if annotation.get(b"Open").and_then(Object::as_bool).ok() != Some(false) { return Err("The PDF has unsupported popup state.".into()); } },
                    AnnotationKind::Highlight => keys(annotation, &[b"Type", b"Subtype", b"Rect", b"QuadPoints", b"NM", b"Contents", b"C", b"F", b"P", b"AP"])?
                }
                name(annotation, b"Type", b"Annot")?;
                if annotation.get(b"P").and_then(Object::as_reference).ok() != Some(page_id) || annotation.get(b"F").and_then(Object::as_i64).ok() != Some(4) { return Err("The PDF has unsupported annotation links or flags.".into()); }
                let expected_color = if kind == AnnotationKind::Note { [1.0, 0.85, 0.0] } else { [1.0, 1.0, 0.0] };
                if values(annotation.get(b"C").map_err(|error| error.to_string())?)? != expected_color { return Err("The PDF has an altered annotation color.".into()); }
                let bounds_value = annotation.get(b"Rect").map_err(|error| error.to_string())?;
                let bounds = if kind == AnnotationKind::Note { rect(bounds_value)? } else { highlight_rect(bounds_value)? };
                let media = media_box(document, page_id)?;
                if bounds.left < media.left || bounds.bottom < media.bottom || bounds.right > media.right || bounds.top > media.top { return Err("The PDF has annotation bounds outside its MediaBox. Commenting is unavailable.".into()); }
                let mut quads = if kind == AnnotationKind::Highlight { Some(read_quads(annotation.get(b"QuadPoints").map_err(|error| error.to_string())?, bounds)?) } else { None };
                let id_bytes = annotation.get(b"NM").and_then(Object::as_str).map_err(|error| error.to_string())?;
                if id_bytes.len() > HIGHLIGHT_PREFIX.len() + 20 { return Err("The PDF has an invalid note ID.".into()); }
                let note_id = std::str::from_utf8(id_bytes).map_err(|_| "The PDF has an invalid note ID.")?.to_owned(); number(&note_id)?;
                if note_id.starts_with(PREFIX) != (kind == AnnotationKind::Note) { return Err("The PDF has an annotation ID with the wrong kind.".into()); }
                if !note_ids.insert(note_id.clone()) { return Err("The PDF contains duplicate note IDs. Commenting is unavailable.".into()); }
                let contents = match (kind, annotation.get(b"Contents")) { (AnnotationKind::Highlight, Err(_)) => String::new(), (_, Ok(contents)) => decode_text(contents)?, _ => return Err("The note has no contents.".into()) };
                if kind == AnnotationKind::Note { validate_text(&contents)?; } else if annotation.has(b"Contents") && (contents.is_empty() || highlight_contents(Some(&contents))? != contents) { return Err("The PDF has a noncanonical empty highlight body.".into()); }
                let ap = annotation.get(b"AP").and_then(Object::as_dict).map_err(|_| "The PDF has unsupported note appearances.")?; keys(ap, &[b"N"])?;
                let appearance_id = ap.get(b"N").and_then(Object::as_reference).map_err(|_| "The PDF has unsupported note appearances.")?; appearance_ids.insert(appearance_id);
                let stream = document.get_object(appearance_id).and_then(Object::as_stream).map_err(|error| error.to_string())?;
                keys(&stream.dict, &[b"Type", b"Subtype", b"FormType", b"BBox", b"Resources", b"Length"])?;
                let expected = appearance_dict(bounds, kind);
                for (key, value) in expected.iter() {
                    let matches = if key == b"BBox" { stream.dict.get(key).ok().and_then(|actual| values(actual).ok()) == values(value).ok() } else { stream.dict.get(key).ok() == Some(value) };
                    if !matches { return Err("The PDF has an altered note appearance schema.".into()); }
                }
                let legacy = quads.as_ref().is_none_or(|quads| quads.as_slice() == [bounds]) && stream.content == appearance(bounds, kind);
                if legacy { rect(bounds_value)?; quads = None; }
                else if quads.as_ref().is_none_or(|quads| stream.content != text_appearance(bounds, quads)) { return Err("The PDF has an altered annotation appearance.".into()); }
                if stream.dict.get(b"Length").is_ok_and(|value| value.as_i64().ok() != Some(stream.content.len() as i64)) { return Err("The PDF has an altered note appearance.".into()); }
                count += 1; text_bytes += contents.len();
                geometry_slots += quads.as_ref().map_or(1, Vec::len);
                if geometry_slots > MAX_GEOMETRY_SLOTS { return Err("The PDF exceeds the 4,096 annotation geometry-slot limit. Annotations are unavailable.".into()); }
                if count > MAX_NOTES || text_bytes > MAX_TOTAL_BYTES { return Err("The PDF exceeds the 1,000-note or 1 MiB total note-text limit.".into()); }
                notes.push(Note { id: note_id, rect: bounds, contents, kind, quads });
            }
        }
        result.push(notes);
    }
    let owned = annotation_ids.union(&appearance_ids).copied().collect::<HashSet<_>>();
    for (id, object) in &document.objects {
        if owned.contains(id) { continue; }
        if page_ids.contains(id) {
            for (key, value) in object.as_dict().map_err(|error| error.to_string())?.iter() {
                if key != b"Annots" && references_owned(value, &owned) { return Err("An unrelated PDF object references a note. Commenting is unavailable to preserve it.".into()); }
            }
        } else if references_owned(object, &owned) { return Err("An unrelated PDF object references a note. Commenting is unavailable to preserve it.".into()); }
    }
    if references_owned(&Object::Dictionary(document.trailer.clone()), &owned) { return Err("The PDF trailer references a note. Commenting is unavailable to preserve it.".into()); }
    Ok(result)
}
fn references_owned(value: &Object, owned: &HashSet<ObjectId>) -> bool {
    let mut remaining = vec![value];
    while let Some(value) = remaining.pop() {
        match value {
            Object::Reference(id) if owned.contains(id) => return true,
            Object::Array(values) => remaining.extend(values),
            Object::Dictionary(values) => remaining.extend(values.iter().map(|(_, value)| value)),
            Object::Stream(value) => remaining.extend(value.dict.iter().map(|(_, value)| value)),
            _ => (),
        }
    }
    false
}

pub fn write(document: &mut Document, plan: &[PageSpec]) -> Result<(), String> {
    read(document)?; validate_plan(plan)?;
    let version = effective_version(document)?.max((1, 4)); document.version = format!("{}.{}", version.0, version.1);
    let page_ids = document.get_pages().into_values().collect::<Vec<ObjectId>>();
    let mut old_objects = HashSet::new();
    for page_id in &page_ids {
        if let Ok(annots) = document.get_dictionary(*page_id).map_err(|error| error.to_string())?.get(b"Annots") {
            for annotation in annots.as_array().map_err(|error| error.to_string())? {
                let id = annotation.as_reference().map_err(|error| error.to_string())?;
                let ap = document.get_dictionary(id).map_err(|error| error.to_string())?.get(b"AP").and_then(Object::as_dict).map_err(|error| error.to_string())?.get(b"N").and_then(Object::as_reference).map_err(|error| error.to_string())?;
                old_objects.insert(id); old_objects.insert(ap);
            }
        }
    }
    for id in &page_ids { document.get_dictionary_mut(*id).map_err(|error| error.to_string())?.remove(b"Annots"); }
    for id in old_objects { document.objects.remove(&id); }
    for spec in plan {
        let page_id = *page_ids.get(spec.source).ok_or("Source page mapping is invalid.")?; let mut annots = Vec::new();
        for note in &spec.notes {
            let content = match &note.quads { Some(quads) => text_appearance(note.rect, quads), None => appearance(note.rect, note.kind) };
            let appearance = document.add_object(Stream::new(appearance_dict(note.rect, note.kind), content));
            let mut annotation = dictionary! { "Type" => "Annot", "Subtype" => if note.kind == AnnotationKind::Note { "Text" } else { "Highlight" }, "Rect" => vec![Object::Real(note.rect.left), Object::Real(note.rect.bottom), Object::Real(note.rect.right), Object::Real(note.rect.top)],
                "NM" => Object::String(note.id.as_bytes().to_vec(), StringFormat::Literal), "F" => 4, "P" => page_id, "AP" => dictionary! { "N" => appearance } };
            if note.kind == AnnotationKind::Note { annotation.set("Contents", encode_text(&note.contents)); annotation.set("Name", "Note"); annotation.set("C", vec![Object::Real(1.0), Object::Real(0.85), Object::Real(0.0)]); annotation.set("Open", false); }
            else { let points = note.quads.as_deref().map_or_else(|| quad(note.rect), |quads| quads.iter().flat_map(|bounds| quad(*bounds)).collect()); annotation.set("QuadPoints", points.into_iter().map(Object::Real).collect::<Vec<_>>()); annotation.set("C", vec![1.into(), 1.into(), 0.into()]); if !note.contents.is_empty() { annotation.set("Contents", encode_text(&note.contents)); } }
            let annotation = document.add_object(annotation);
            annots.push(Object::Reference(annotation));
        }
        if !annots.is_empty() { document.get_dictionary_mut(page_id).map_err(|error| error.to_string())?.set("Annots", annots); }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor::{EditSession, PageEdit};
    fn sample() -> Vec<u8> { std::fs::read(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("resources/welcome.pdf")).unwrap() }
    #[test]
    fn text_highlights_compound_appearance_old_schemas_unicode_and_quad_guards() {
        let (mut session, _) = annotated();
        let area = CropBox { left: 200.0, bottom: 220.0, right: 250.0, top: 260.0 };
        let next = session.proposed_highlight(Some((0, area)), None, None, false).unwrap(); session.commit_comments(next);
        let first = CropBox { left: 40.0, bottom: 300.0, right: 80.0, top: 315.0 };
        let second = CropBox { left: 55.0, bottom: 300.0, right: 95.0, top: 315.0 };
        let third = CropBox { left: 40.0, bottom: 270.0, right: 65.0, top: 285.0 };
        let quads = vec![first, second, first, third];
        let next = session.proposed_text_highlight(0, quads.clone(), Some("  é 漢字 😀\n ")).unwrap(); session.commit_comments(next);
        let output = Document::load_mem(&session.export(None).unwrap()).unwrap(); assert_eq!(read(&output).unwrap()[0], session.plan[0].notes);
        assert!(session.plan[0].notes[0].quads.is_none() && session.plan[0].notes[1].quads.is_none());
        let page = output.get_pages()[&1]; let annotation = output.get_dictionary(page).unwrap().get(b"Annots").unwrap().as_array().unwrap()[2].as_reference().unwrap();
        let ap = output.get_dictionary(annotation).unwrap().get(b"AP").unwrap().as_dict().unwrap().get(b"N").unwrap().as_reference().unwrap();
        let content = &output.get_object(ap).unwrap().as_stream().unwrap().content;
        assert_eq!(std::str::from_utf8(content).unwrap().matches(" re ").count(), 4); assert_eq!(std::str::from_utf8(content).unwrap().matches("f Q").count(), 1, "Overlapping quads use exactly one nonzero-winding fill");
        for values in [vec![0.into(); 7], vec![0.into(); 8 * (MAX_HIGHLIGHT_QUADS + 1)], vec![Object::Real(f32::NAN); 8]] { let mut altered = output.clone(); altered.get_dictionary_mut(annotation).unwrap().set("QuadPoints", values); assert!(read(&altered).is_err()); }
        let mut altered = output.clone(); altered.get_object_mut(ap).unwrap().as_stream_mut().unwrap().content.extend(b" f"); assert!(read(&altered).is_err());
        assert!(session.proposed_text_highlight(0, vec![], None).is_err()); assert!(session.proposed_text_highlight(0, vec![first; MAX_HIGHLIGHT_QUADS + 1], None).is_err());
        let next = session.proposed_text_highlight(0, vec![first; MAX_HIGHLIGHT_QUADS], None).unwrap(); assert_eq!(next[0].notes.last().unwrap().quads.as_ref().unwrap().len(), 256);
        let small = CropBox { left: 40.0, bottom: 100.0, right: 40.1, top: 100.2 }; let next = session.proposed_text_highlight(0, vec![small], None).unwrap(); session.commit_comments(next);
        let output = Document::load_mem(&session.export(None).unwrap()).unwrap(); assert_eq!(read(&output).unwrap()[0], session.plan[0].notes, "Positive glyph bounds need not meet the area's one-point minimum");
    }
    #[test]
    fn text_highlights_mixed_geometry_budget_strict_import_undo_and_atomic_failure() {
        let source = sample(); let mut session = EditSession::new(source.clone(), 6); let bounds = CropBox { left: 100.0, bottom: 200.0, right: 180.0, top: 240.0 };
        for _ in 0..15 { let next = session.proposed_text_highlight(0, vec![bounds; 256], None).unwrap(); session.commit_comments(next); }
        let next = session.proposed_text_highlight(0, vec![bounds; 254], None).unwrap(); session.commit_comments(next);
        let next = session.proposed_comment(Some((0, bounds)), None, Some("One note slot é 😀")).unwrap(); session.commit_comments(next);
        let next = session.proposed_highlight(Some((0, bounds)), None, None, false).unwrap(); session.commit_comments(next); session.mark_saved();
        let saved = session.plan.clone(); let revision = session.revision;
        assert!(validate_plan(&saved).is_ok()); assert!(session.proposed_text_highlight(0, vec![bounds], None).unwrap_err().contains("4,096")); assert_eq!(session.plan, saved); assert_eq!(session.revision, revision); assert!(!session.dirty());
        let bytes = session.export(None).unwrap(); let mut output = Document::load_mem(&bytes).unwrap(); assert_eq!(read(&output).unwrap()[0], saved[0].notes);
        let page = output.get_pages()[&1]; let original = output.get_dictionary(page).unwrap().get(b"Annots").unwrap().as_array().unwrap()[0].as_reference().unwrap();
        let mut duplicate = output.get_dictionary(original).unwrap().clone(); duplicate.set("NM", Object::string_literal(highlight_id(999))); let duplicate = output.add_object(duplicate); output.get_dictionary_mut(page).unwrap().get_mut(b"Annots").unwrap().as_array_mut().unwrap().push(Object::Reference(duplicate)); assert!(read(&output).unwrap_err().contains("4,096"));
        session.apply(PageEdit::Undo).unwrap(); assert!(session.dirty()); let next = session.proposed_text_highlight(0, vec![bounds], None).unwrap(); session.commit_comments(next); assert!(!session.can_redo()); assert!(number(&session.plan[0].notes.last().unwrap().id).unwrap() > number(&saved[0].notes.last().unwrap().id).unwrap()); session.apply(PageEdit::Undo).unwrap(); session.apply(PageEdit::Redo).unwrap(); assert!(validate_plan(&session.plan).is_ok()); assert_eq!(session.source, source);
    }
    fn annotated() -> (EditSession, Vec<u8>) {
        let source = sample(); let mut session = EditSession::new(source.clone(), 6);
        let plan = session.proposed_comment(Some((0, CropBox { left: 40.0, bottom: 60.0, right: 70.0, top: 90.0 })), None, Some("Unicode é 漢字 😀\nSecond line")).unwrap();
        session.commit_comments(plan); let bytes = session.export(None).unwrap(); (session, bytes)
    }
    #[test]
    fn highlights_mixed_legacy_notes_unicode_schema_and_appearance_are_strict() {
        let (original, legacy) = annotated(); let mut session = EditSession::new(legacy.clone(), 6);
        assert_eq!(session.plan[0].notes[0], original.plan[0].notes[0]);
        let bounds = CropBox { left: 100.0, bottom: 200.0, right: 180.0, top: 240.0 };
        let body = "  Unicode é 漢字 😀\n ";
        let plan = session.proposed_highlight(Some((0, bounds)), None, Some(body), false).unwrap(); session.commit_comments(plan);
        let output = Document::load_mem(&session.export(None).unwrap()).unwrap(); let imported = read(&output).unwrap(); assert_eq!(imported[0], session.plan[0].notes); assert_eq!(imported[0][1].contents, body); assert_eq!(imported[0][0], original.plan[0].notes[0]);
        let page = output.get_pages()[&1]; let annotation = output.get_dictionary(page).unwrap().get(b"Annots").unwrap().as_array().unwrap()[1].as_reference().unwrap();
        for key in [b"AA".as_slice(), b"A", b"Popup", b"Name", b"Open", b"Unknown"] { let mut altered = output.clone(); altered.get_dictionary_mut(annotation).unwrap().set(key, Object::Null); assert!(read(&altered).is_err()); }
        let mut altered = output.clone(); altered.get_dictionary_mut(annotation).unwrap().set("QuadPoints", vec![0.into(); 8]); assert!(read(&altered).is_err());
        let ap = output.get_dictionary(annotation).unwrap().get(b"AP").unwrap().as_dict().unwrap().get(b"N").unwrap().as_reference().unwrap();
        let mut altered = output.clone(); let resources = altered.get_object_mut(ap).unwrap().as_stream_mut().unwrap().dict.get_mut(b"Resources").unwrap().as_dict_mut().unwrap(); resources.get_mut(b"ExtGState").unwrap().as_dict_mut().unwrap().get_mut(b"GS0").unwrap().as_dict_mut().unwrap().set("ca", Object::Real(0.5)); assert!(read(&altered).is_err());
        let mut altered = output.clone(); altered.get_object_mut(ap).unwrap().as_stream_mut().unwrap().content.push(b' '); assert!(read(&altered).is_err());
        assert_eq!(session.source, legacy);
    }
    #[test]
    fn highlights_optional_body_caps_history_and_cross_kind_ids_are_consistent() {
        let mut session = EditSession::new(sample(), 6); let bounds = CropBox { left: 100.0, bottom: 200.0, right: 180.0, top: 240.0 };
        let next = session.proposed_highlight(Some((0, bounds)), None, Some(" \n\t"), false).unwrap(); session.commit_comments(next); let first = session.plan[0].notes[0].id.clone(); assert!(session.plan[0].notes[0].contents.is_empty());
        let revision = session.revision; let next = session.proposed_highlight(None, Some(&first), None, false).unwrap(); session.commit_comments(next); assert_eq!(session.revision, revision);
        assert!(session.proposed_comment(None, Some(&first), Some("Wrong kind")).is_err()); assert!(session.proposed_comment(None, Some(&first), None).is_err());
        session.apply(PageEdit::Undo).unwrap(); let next = session.proposed_comment(Some((0, bounds)), None, Some("Note")).unwrap(); session.commit_comments(next); let note = session.plan[0].notes[0].id.clone(); assert!(number(&note).unwrap() > number(&first).unwrap()); assert!(!session.can_redo()); assert!(session.proposed_highlight(None, Some(&note), None, true).is_err());
        let next = session.proposed_highlight(Some((0, bounds)), None, None, false).unwrap(); session.commit_comments(next); let highlight = session.plan[0].notes[1].id.clone(); session.mark_saved();
        let next = session.proposed_highlight(None, Some(&highlight), Some("Body é 😀"), false).unwrap(); session.commit_comments(next); assert!(session.dirty()); session.apply(PageEdit::Undo).unwrap(); assert!(!session.dirty()); session.apply(PageEdit::Redo).unwrap(); assert!(session.dirty());
        assert!(highlight_contents(Some("\0")).is_err()); assert!(highlight_contents(Some(&"x".repeat(MAX_TEXT_BYTES + 1))).is_err());
        let next = session.proposed_comment(None, Some(&note), None).unwrap(); session.commit_comments(next); let next = session.proposed_highlight(None, Some(&highlight), None, true).unwrap(); session.commit_comments(next); let output = Document::load_mem(&session.export(None).unwrap()).unwrap(); assert!(output.get_pages().values().all(|page| !output.get_dictionary(*page).unwrap().has(b"Annots"))); assert!(read(&output).unwrap().iter().all(Vec::is_empty));
        session.plan[0].notes = (1..=MAX_NOTES).map(|value| Note { quads: None, id: if value % 2 == 0 { highlight_id(value as u64) } else { id(value as u64) }, kind: if value % 2 == 0 { AnnotationKind::Highlight } else { AnnotationKind::Note }, rect: bounds, contents: "x".into() }).collect(); assert!(validate_plan(&session.plan).is_ok()); session.plan[0].notes.push(Note { quads: None, id: highlight_id(1001), kind: AnnotationKind::Highlight, rect: bounds, contents: String::new() }); assert!(validate_plan(&session.plan).unwrap_err().contains("1,000"));
        session.plan[0].notes = (1..=128).map(|value| Note { quads: None, id: if value % 2 == 0 { highlight_id(value) } else { id(value) }, kind: if value % 2 == 0 { AnnotationKind::Highlight } else { AnnotationKind::Note }, rect: bounds, contents: "x".repeat(MAX_TEXT_BYTES) }).collect(); assert!(validate_plan(&session.plan).is_ok()); session.plan[0].notes.push(Note { quads: None, id: highlight_id(129), kind: AnnotationKind::Highlight, rect: bounds, contents: "x".into() }); assert!(validate_plan(&session.plan).unwrap_err().contains("1 MiB"));
    }
    #[test]
    fn comments_unicode_owned_schema_reopens_and_foreign_links_or_appearance_refuse() {
        let (session, bytes) = annotated(); let document = Document::load_mem(&bytes).unwrap();
        let notes = read(&document).unwrap(); assert_eq!(notes[0], session.plan[0].notes);
        assert_eq!(session.source, sample());
        let page = document.get_pages()[&1]; let annotation = document.get_dictionary(page).unwrap().get(b"Annots").unwrap().as_array().unwrap()[0].as_reference().unwrap();
        for key in [b"AA".as_slice(), b"A", b"Popup", b"Unknown"] {
            let mut altered = document.clone(); altered.get_dictionary_mut(annotation).unwrap().set(key, Object::Null); assert!(read(&altered).is_err());
        }
        let mut altered = document.clone(); let ap = altered.get_dictionary(annotation).unwrap().get(b"AP").unwrap().as_dict().unwrap().get(b"N").unwrap().as_reference().unwrap();
        altered.get_object_mut(ap).unwrap().as_stream_mut().unwrap().content.push(b' '); assert!(read(&altered).is_err());
        let mut altered = document.clone(); altered.get_dictionary_mut(page).unwrap().get_mut(b"Annots").unwrap().as_array_mut().unwrap().push(Object::Reference(annotation)); assert!(read(&altered).is_err());
        let mut altered = document.clone(); let dependency = altered.add_object(dictionary! { "Unrelated" => annotation }); altered.trailer.set("Info", dependency); assert!(read(&altered).is_err());
        let mut empty = Document::load_mem(&sample()).unwrap(); let page = empty.get_pages()[&1]; empty.get_dictionary_mut(page).unwrap().set("Annots", Vec::<Object>::new());
        let original_page = empty.get_dictionary(page).unwrap().clone(); assert!(read(&empty).unwrap_err().contains("empty annotation array")); assert_eq!(empty.get_dictionary(page).unwrap(), &original_page);
    }
    #[test]
    fn comments_undo_redo_saved_baseline_noops_and_source_remain_consistent() {
        let (mut session, bytes) = annotated(); let note = session.plan[0].notes[0].clone();
        session.mark_saved(); assert!(!session.dirty()); let revision = session.revision;
        let plan = session.proposed_comment(None, Some(&note.id), Some(&note.contents)).unwrap(); session.commit_comments(plan); assert_eq!(session.revision, revision);
        let plan = session.proposed_comment(None, Some(&note.id), Some("Changed")).unwrap(); session.commit_comments(plan); assert!(session.dirty());
        session.apply(PageEdit::Undo).unwrap(); assert!(!session.dirty()); assert_eq!(session.plan[0].notes[0], note);
        session.apply(PageEdit::Redo).unwrap(); assert_eq!(session.plan[0].notes[0].contents, "Changed");
        session.apply(PageEdit::Undo).unwrap(); let plan = session.proposed_comment(None, Some(&note.id), None).unwrap(); session.commit_comments(plan); assert!(!session.can_redo());
        let output = Document::load_mem(&session.export(None).unwrap()).unwrap(); assert!(read(&output).unwrap().iter().all(Vec::is_empty));
        assert!(output.get_pages().values().all(|page| !output.get_dictionary(*page).unwrap().has(b"Annots")));
        let reopened = EditSession::new(bytes, 6); assert_eq!(reopened.plan[0].notes[0], note); assert!(!reopened.dirty());
        assert!(session.proposed_comment(None, Some("missing"), Some("changed")).is_err()); assert_eq!(session.source, sample());
    }
    #[test]
    fn comments_text_count_total_caps_and_duplicate_ids_are_explicit() {
        assert!(validate_text(" \n\t").is_err()); assert!(validate_text("x\0y").is_err());
        assert!(validate_text(&"é".repeat(MAX_TEXT_BYTES / 2)).is_ok()); assert!(validate_text(&"é".repeat(MAX_TEXT_BYTES / 2 + 1)).is_err());
        let mut session = EditSession::new(sample(), 6);
        let bounds = CropBox { left: 40.0, bottom: 60.0, right: 70.0, top: 90.0 };
        session.plan[0].notes = (1..=MAX_NOTES).map(|value| Note { quads: None, id: id(value as u64), rect: bounds, contents: "x".into(), kind: AnnotationKind::Note }).collect(); assert!(validate_plan(&session.plan).is_ok());
        session.plan[0].notes.push(Note { quads: None, id: id(MAX_NOTES as u64 + 1), rect: bounds, contents: "x".into(), kind: AnnotationKind::Note }); assert!(validate_plan(&session.plan).unwrap_err().contains("1,000"));
        session.plan[0].notes = (1..=128).map(|value| Note { quads: None, id: id(value), rect: bounds, contents: "x".repeat(MAX_TEXT_BYTES), kind: AnnotationKind::Note }).collect(); assert!(validate_plan(&session.plan).is_ok());
        session.plan[0].notes.push(Note { quads: None, id: id(129), rect: bounds, contents: "x".into(), kind: AnnotationKind::Note }); assert!(validate_plan(&session.plan).unwrap_err().contains("1 MiB"));
        session.plan[0].notes = vec![Note { quads: None, id: id(1), rect: bounds, contents: "x".into(), kind: AnnotationKind::Note }; 2]; assert!(validate_plan(&session.plan).unwrap_err().contains("duplicate"));
        let (_, bytes) = annotated(); let mut pdf = Document::load_mem(&bytes).unwrap(); let page = pdf.get_pages()[&1];
        let original = pdf.get_dictionary(page).unwrap().get(b"Annots").unwrap().as_array().unwrap()[0].as_reference().unwrap();
        let duplicate = pdf.add_object(pdf.get_object(original).unwrap().clone()); pdf.get_dictionary_mut(page).unwrap().get_mut(b"Annots").unwrap().as_array_mut().unwrap().push(Object::Reference(duplicate)); assert!(read(&pdf).unwrap_err().contains("duplicate"));
    }
    #[test]
    fn comments_pdf_version_floor_and_newer_catalog_version_survive_last_note_removal() {
        for (header, declared, expected) in [("1.0", None, "1.4"), ("1.2", Some("1.7"), "1.7"), ("2.0", Some("1.7"), "2.0")] {
            let mut pdf = Document::load_mem(&sample()).unwrap(); pdf.version = header.into(); if let Some(declared) = declared { pdf.catalog_mut().unwrap().set("Version", declared); }
            let original_catalog_version = pdf.catalog().unwrap().get(b"Version").ok().cloned(); let mut source = Vec::new(); pdf.save_to(&mut source).unwrap();
            let mut session = EditSession::new(source.clone(), 6); let plan = session.proposed_comment(Some((0, CropBox { left: 40.0, bottom: 60.0, right: 70.0, top: 90.0 })), None, Some("Note")).unwrap(); session.commit_comments(plan);
            let output = Document::load_mem(&session.export(None).unwrap()).unwrap(); assert_eq!(output.version, expected); assert_eq!(output.catalog().unwrap().get(b"Version").ok().cloned(), original_catalog_version);
            let mut reopened = EditSession::new(session.export(None).unwrap(), 6); let note_id = reopened.plan[0].notes[0].id.clone(); let plan = reopened.proposed_comment(None, Some(&note_id), None).unwrap(); reopened.commit_comments(plan);
            let output = Document::load_mem(&reopened.export(None).unwrap()).unwrap(); assert_eq!(output.version, expected); assert!(read(&output).unwrap().iter().all(Vec::is_empty));
            reopened.mark_saved(); let revision = reopened.revision; assert!(!reopened.dirty()); reopened.apply(PageEdit::Move { from: 0, to: 0 }).unwrap(); assert_eq!(reopened.revision, revision); assert_eq!(Document::load_mem(&reopened.export(None).unwrap()).unwrap().version, expected); assert!(reopened.proposed_comment(None, Some(&note_id), None).is_err()); assert_eq!(reopened.revision, revision); assert!(!reopened.dirty()); assert_eq!(session.source, source);
        }
    }

    #[test]
    #[ignore]
    fn write_dialog_annotation_fixture() {
        let dir = std::path::PathBuf::from(std::env::var("SM3_DIALOG_FIXTURE_DIR").expect("set SM3_DIALOG_FIXTURE_DIR"));
        let mut session = EditSession::new(sample(), 6);
        let next = session.proposed_comment(Some((0, CropBox { left: 40.0, bottom: 60.0, right: 70.0, top: 90.0 })), None, Some("Dialog verifier note")).unwrap(); session.commit_comments(next);
        let next = session.proposed_highlight(Some((0, CropBox { left: 300.0, bottom: 200.0, right: 400.0, top: 240.0 })), None, Some("Dialog verifier area"), false).unwrap(); session.commit_comments(next);
        let next = session.proposed_text_highlight(0, vec![CropBox { left: 54.0, bottom: 640.0, right: 250.0, top: 668.0 }], Some("Dialog verifier text")).unwrap(); session.commit_comments(next);
        let bytes = session.export(None).unwrap();
        assert_eq!(read(&Document::load_mem(&bytes).unwrap()).unwrap()[0].len(), 3);
        std::fs::write(dir.join("annotated.pdf"), bytes).unwrap();
    }
}
