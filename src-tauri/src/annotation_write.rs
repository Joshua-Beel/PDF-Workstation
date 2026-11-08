//! Writing the app's annotations and their appearance streams (SM3 P2-7a, split out of `comments.rs` with no
//! behaviour change). Only pages whose app annotations changed are touched; foreign entries are kept in order.
use crate::comments::*;
use crate::editor::{CropBox, PageSpec};
use std::fmt::Write as _;

/// The appearance of a drawing (P2-7g; not called by `write` yet), for the app's own Ink and for a colour or
/// width edit of another app's Ink (SM7 ruling, conditions 1 and 3). It is built only from typed values: each
/// stroke is one path with round caps and joins, in page coordinates, so `bbox` is the annotation's `/Rect` and
/// the appearance maps onto it without scaling. `color` has 0 (nothing is stroked), 1 (gray), 3 (RGB) or 4
/// (CMYK) components in 0 to 1. The form has only `/Type /XObject /Subtype /Form /BBox /Resources`, and
/// `/Resources` holds an app-made ExtGState only when the opacity is not 1. Over 4 MiB of content is refused.
pub(crate) fn ink_appearance(strokes: &[Vec<(f32, f32)>], width: f32, color: &[f32], opacity: f32, bbox: CropBox) -> Result<(Dictionary, Vec<u8>), String> {
    validate_strokes(strokes)?;
    if !(width.is_finite() && (0.0..=MAX_INK_WIDTH).contains(&width)) { return Err("The drawing width is out of range.".into()); }
    if !matches!(color.len(), 0 | 1 | 3 | 4) || color.iter().any(|value| !(0.0..=1.0).contains(value)) { return Err("The drawing colour is out of range.".into()); }
    if !(0.0..=1.0).contains(&opacity) { return Err("The drawing opacity is out of range.".into()); }
    let mut content = String::from("q\n");
    if opacity != 1.0 { content.push_str("/GS0 gs\n"); }
    let operator = match color.len() { 1 => "G", 3 => "RG", 4 => "K", _ => "" };
    for value in color { let _ = write!(content, "{value:.6} "); }
    if !operator.is_empty() { content.push_str(operator); content.push('\n'); }
    let _ = writeln!(content, "{width:.6} w 1 J 1 j");
    for stroke in strokes {
        let (x0, y0) = stroke[0];
        let _ = writeln!(content, "{x0:.6} {y0:.6} m");
        // A single point still draws a dot: a zero-length line with round caps.
        let rest: &[(f32, f32)] = if stroke.len() == 1 { stroke } else { &stroke[1..] };
        for &(x, y) in rest { let _ = writeln!(content, "{x:.6} {y:.6} l"); }
        content.push_str(if operator.is_empty() { "n\n" } else { "S\n" });
        if content.len() > MAX_INK_APPEARANCE_BYTES { return Err("The drawing appearance would exceed 4 MiB.".into()); }
    }
    content.push_str("Q\n");
    if content.len() > MAX_INK_APPEARANCE_BYTES { return Err("The drawing appearance would exceed 4 MiB.".into()); }
    let resources = if opacity != 1.0 { dictionary! { "ExtGState" => dictionary! { "GS0" => dictionary! { "Type" => "ExtGState", "CA" => Object::Real(opacity), "ca" => Object::Real(opacity) } } } } else { dictionary! {} };
    let form = dictionary! { "Type" => "XObject", "Subtype" => "Form",
        "BBox" => vec![Object::Real(bbox.left), Object::Real(bbox.bottom), Object::Real(bbox.right), Object::Real(bbox.top)], "Resources" => resources };
    Ok((form, content.into_bytes()))
}

/// The `/InkList` of a drawing.
pub(crate) fn ink_list(strokes: &[Vec<(f32, f32)>]) -> Object {
    Object::Array(strokes.iter().map(|stroke| Object::Array(stroke.iter().flat_map(|&(x, y)| [Object::Real(x), Object::Real(y)]).collect())).collect())
}

/// A new app drawing on `page_id` (P2-7g; not called by `write` yet): the annotation and its appearance, added to
/// `document`. Acrobat-style keys: `/Subj (Pencil)`, a UUID `/NM`, `/CreationDate` and `/M`, `/BS` with the
/// width, `/C`, `/CA`. No `/T` until Joshua rules on Q1.
#[allow(dead_code, reason = "used by the pen tool in P2-7b")]
pub(crate) fn new_ink(document: &mut Document, page_id: ObjectId, id: &str, ink: &Ink, created: Option<&str>) -> Result<ObjectId, String> {
    validate_ink(ink)?;
    if !is_uuid(id) { return Err("A drawing needs a UUID annotation ID.".into()); }
    if created.is_some_and(|date| !valid_pdf_date(date)) { return Err("The annotation has an invalid date.".into()); }
    let bounds = ink_bounds(&ink.strokes, ink.width);
    let (form, content) = ink_appearance(&ink.strokes, ink.width, &ink.color, ink.opacity, bounds)?;
    let appearance = document.add_object(Stream::new(form, content));
    let [r, g, b] = ink.color;
    let mut annotation = dictionary! { "Type" => "Annot", "Subtype" => "Ink",
        "Rect" => vec![Object::Real(bounds.left), Object::Real(bounds.bottom), Object::Real(bounds.right), Object::Real(bounds.top)],
        "InkList" => ink_list(&ink.strokes), "BS" => dictionary! { "Type" => "Border", "W" => Object::Real(ink.width), "S" => "S" },
        "C" => vec![Object::Real(r), Object::Real(g), Object::Real(b)], "CA" => Object::Real(ink.opacity),
        "NM" => Object::String(id.as_bytes().to_vec(), StringFormat::Literal), "F" => 4, "P" => page_id, "AP" => dictionary! { "N" => appearance },
        "Subj" => Object::String(b"Pencil".to_vec(), StringFormat::Literal) };
    if let Some(created) = created {
        annotation.set("CreationDate", Object::String(created.as_bytes().to_vec(), StringFormat::Literal));
        annotation.set("M", Object::String(created.as_bytes().to_vec(), StringFormat::Literal));
    }
    Ok(document.add_object(Object::Dictionary(annotation)))
}
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
