//! Writing the app's annotations and their appearance streams (SM3 P2-7a, split out of `comments.rs` with no
//! behaviour change). Only pages whose app annotations changed are touched; foreign entries are kept in order.
use crate::comments::*;
use crate::editor::{CropBox, PageSpec};
use lopdf::{dictionary, Dictionary, Document, Object, ObjectId, Stream, StringFormat};
use std::collections::HashSet;

pub(crate) fn appearance(bounds: CropBox, kind: AnnotationKind) -> Vec<u8> {
    let (w, h) = (bounds.right - bounds.left, bounds.top - bounds.bottom);
    if kind == AnnotationKind::Highlight { return format!("q /GS0 gs 1 1 0 rg 0 0 {w:.6} {h:.6} re f Q\n").into_bytes(); }
    format!("q 1 0.85 0 rg 0 0 {w:.6} {h:.6} re f 0.25 0.2 0 RG 0.25 w {x:.6} {a:.6} m {r:.6} {a:.6} l {x:.6} {b:.6} m {r:.6} {b:.6} l S Q\n", x=w*0.2, r=w*0.8, a=h*0.65, b=h*0.4).into_bytes()
}
pub(crate) fn text_appearance(bounds: CropBox, quads: &[CropBox]) -> Vec<u8> {
    let mut content = String::from("q /GS0 gs 1 1 0 rg ");
    for rect in quads { content.push_str(&format!("{x:.6} {y:.6} {w:.6} {h:.6} re ", x=rect.left-bounds.left, y=rect.bottom-bounds.bottom, w=rect.right-rect.left, h=rect.top-rect.bottom)); }
    content.push_str("f Q\n"); content.into_bytes()
}
pub(crate) fn appearance_dict(bounds: CropBox, kind: AnnotationKind) -> Dictionary {
    let resources = if kind == AnnotationKind::Highlight { dictionary! { "ExtGState" => dictionary! { "GS0" => dictionary! { "Type" => "ExtGState", "BM" => "Multiply", "ca" => Object::Real(0.25), "CA" => Object::Real(0.25) } } } } else { dictionary! {} };
    dictionary! { "Type" => "XObject", "Subtype" => "Form", "FormType" => 1,
        "BBox" => vec![0.into(), 0.into(), Object::Real(bounds.right - bounds.left), Object::Real(bounds.top - bounds.bottom)], "Resources" => resources }
}
pub(crate) fn encode_text(value: &str) -> Object {
    let mut bytes = vec![0xfe, 0xff]; for unit in value.encode_utf16() { bytes.extend(unit.to_be_bytes()); }
    Object::String(bytes, StringFormat::Hexadecimal)
}

/// The `/Annots` entries for one app annotation: the annotation, then, for a sticky note, its popup (Acrobat
/// lists a popup right after its parent).
pub(crate) fn new_annotation(document: &mut Document, page_id: ObjectId, note: &Note) -> Result<Vec<Object>, String> {
    let id = document.new_object_id();
    let popup = if note.kind == AnnotationKind::Note {
        // Fails closed (SM7 L-1 on 8bb61c3): without a valid MediaBox the reader would not accept the note as
        // the app's own either, so writing it without a popup would gain nothing.
        let media = media_box(document, page_id).map_err(|_| "This page has no valid MediaBox, so the sticky note's popup cannot be placed. Nothing was saved.".to_owned())?;
        let area = popup_rect(note.rect, media);
        Some(document.add_object(dictionary! { "Type" => "Annot", "Subtype" => "Popup", "Parent" => id, "Rect" => vec![Object::Real(area.left), Object::Real(area.bottom), Object::Real(area.right), Object::Real(area.top)], "Open" => false, "F" => 28 }))
    } else { None };
    let content = match &note.quads { Some(quads) => text_appearance(note.rect, quads), None => appearance(note.rect, note.kind) };
    let appearance = document.add_object(Stream::new(appearance_dict(note.rect, note.kind), content));
    let mut annotation = dictionary! { "Type" => "Annot", "Subtype" => if note.kind == AnnotationKind::Note { "Text" } else { "Highlight" }, "Rect" => vec![Object::Real(note.rect.left), Object::Real(note.rect.bottom), Object::Real(note.rect.right), Object::Real(note.rect.top)],
        "NM" => Object::String(note.id.as_bytes().to_vec(), StringFormat::Literal), "F" => 4, "P" => page_id, "AP" => dictionary! { "N" => appearance },
        "Subj" => Object::String(subject(note.kind).as_bytes().to_vec(), StringFormat::Literal) };
    // No /T: the author name stays blank until Joshua rules on Q1 (design 6.4); the Windows user name is never read.
    if let Some(created) = &note.created { annotation.set("CreationDate", Object::String(created.as_bytes().to_vec(), StringFormat::Literal)); }
    if let Some(modified) = &note.modified { annotation.set("M", Object::String(modified.as_bytes().to_vec(), StringFormat::Literal)); }
    if note.kind == AnnotationKind::Note { annotation.set("Contents", encode_text(&note.contents)); annotation.set("Name", "Note"); annotation.set("C", vec![Object::Real(1.0), Object::Real(0.85), Object::Real(0.0)]); annotation.set("Open", false); }
    else { let points = note.quads.as_deref().map_or_else(|| quad(note.rect), |quads| quads.iter().flat_map(|bounds| quad(*bounds)).collect()); annotation.set("QuadPoints", points.into_iter().map(Object::Real).collect::<Vec<_>>()); annotation.set("C", vec![1.into(), 1.into(), 0.into()]); if !note.contents.is_empty() { annotation.set("Contents", encode_text(&note.contents)); } }
    if let Some(popup) = popup { annotation.set("Popup", popup); }
    document.objects.insert(id, Object::Dictionary(annotation));
    Ok([Some(id), popup].into_iter().flatten().map(Object::Reference).collect())
}

/// Writes the planned app annotations. Only pages whose app annotations changed are touched. On such a
/// page every other `/Annots` entry (other apps' annotations, links, widgets, inline or malformed entries)
/// is kept, in order; each kept app annotation is regenerated in its place, deleted ones are dropped with
/// their appearance, and new ones are appended. A page left with no entries loses its `/Annots`. An
/// indirect `/Annots` array is replaced by a direct one on a touched page; the old array object is left for
/// `prune_unreachable`. Foreign objects are never modified.
pub fn write(document: &mut Document, plan: &[PageSpec]) -> Result<(), String> {
    let owned = read_owned(document)?; validate_plan(plan)?;
    let version = effective_version(document)?.max((1, 4)); document.version = format!("{}.{}", version.0, version.1);
    let page_ids = document.get_pages().into_values().collect::<Vec<ObjectId>>();
    let mut planned: std::collections::HashMap<usize, &[Note]> = std::collections::HashMap::new();
    for spec in plan {
        if spec.source >= page_ids.len() { return Err("Source page mapping is invalid.".into()); }
        if planned.insert(spec.source, &spec.notes).is_some() { return Err("Source page mapping is invalid.".into()); }
    }
    for (index, page_id) in page_ids.iter().copied().enumerate() {
        let Some(notes) = planned.get(&index).copied() else { continue };
        if notes == owned.notes[index].as_slice() { continue; }
        let entries = page_annots(document, page_id)?.map(|(_, entries)| entries.to_vec()).unwrap_or_default();
        let mut emitted = HashSet::new();
        let mut output = Vec::with_capacity(entries.len() + 2 * notes.len());
        let app_popups: HashSet<ObjectId> = owned.popups.values().copied().collect();
        for entry in entries {
            // An app note's popup is regenerated with its note, so its old entry is dropped here.
            if let Object::Reference(id) = &entry { if app_popups.contains(id) { document.objects.remove(id); continue; } }
            let app = match &entry { Object::Reference(id) => owned.app.get(id).cloned().map(|app| (*id, app)), _ => None };
            let Some((id, (note_id, appearance))) = app else { output.push(entry); continue };
            document.objects.remove(&id); document.objects.remove(&appearance);
            if let Some(popup) = owned.popups.get(&id) { document.objects.remove(popup); }
            if let Some(note) = notes.iter().find(|note| note.id == note_id) {
                output.extend(new_annotation(document, page_id, note)?);
                emitted.insert(note_id);
            }
        }
        for note in notes.iter().filter(|note| !emitted.contains(&note.id)) { output.extend(new_annotation(document, page_id, note)?); }
        let page = document.get_dictionary_mut(page_id).map_err(|error| error.to_string())?;
        if output.is_empty() { page.remove(b"Annots"); } else { page.set("Annots", output); }
    }
    Ok(())
}
