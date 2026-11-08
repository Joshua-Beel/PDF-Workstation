//! Reading the app's annotations out of a PDF: which `/Annots` entries are the app's own (editable) and which
//! are preserved untouched. The model is in `annotation_model.rs` and the writer in `annotation_write.rs`
//! (SM3 P2-7a); both are re-exported here, so callers keep using `comments::`.
use crate::editor::CropBox;
use lopdf::{Dictionary, Document, Object, ObjectId};
use std::collections::HashSet;
pub use crate::annotation_model::*;
pub use crate::annotation_write::*;

/// The page's `/Annots` array, direct or one reference deep, with the id of the array object when indirect.
/// `None` when the page has no `/Annots`; an error when it is something other than an array.
pub(crate) fn page_annots(document: &Document, page_id: ObjectId) -> Result<Option<(Option<ObjectId>, &[Object])>, String> {
    let page = document.get_dictionary(page_id).map_err(|error| error.to_string())?;
    match page.get(b"Annots") {
        Err(_) => Ok(None),
        Ok(Object::Reference(id)) => document.get_object(*id).ok().and_then(|value| value.as_array().ok()).map(|values| Some((Some(*id), values.as_slice()))).ok_or_else(|| "The PDF has a malformed annotation array. Commenting is unavailable.".into()),
        Ok(Object::Array(values)) => Ok(Some((None, values.as_slice()))),
        Ok(_) => Err("The PDF has a malformed annotation array. Commenting is unavailable.".into()),
    }
}

/// What `read_owned` found: the app's own annotations (editable) and everything else (preserved untouched).
pub struct Owned {
    /// The app's editable notes and highlights, per page in `get_pages` order, in `/Annots` order.
    pub notes: Vec<Vec<Note>>,
    /// Annotation object id of each editable note, mapped to its id and its appearance stream's id.
    pub app: std::collections::HashMap<ObjectId, (String, ObjectId)>,
    /// The popup annotation of an editable sticky note (note object id to popup object id). The note and its
    /// popup are one unit: the popup's `/Parent` points back at the note, which is not a foreign reference.
    pub popups: std::collections::HashMap<ObjectId, ObjectId>,
    /// Entries in `/Annots` that are not the app's editable annotations (other apps', links, widgets,
    /// app-made ones another object refers to or that were altered, inline or malformed entries).
    pub foreign: usize,
    /// The largest app ID number on any annotation, editable or not, so new IDs never repeat one.
    pub max_number: u64,
}

/// Where the app puts a sticky note's popup, as Acrobat does: a 180 x 120 point box at the right edge of the
/// page, its top level with the note's top, kept inside the MediaBox.
pub fn popup_rect(note: CropBox, media: CropBox) -> CropBox {
    let (width, height) = (180f32.min(media.right - media.left), 120f32.min(media.top - media.bottom));
    let top = note.top.min(media.top).max(media.bottom + height);
    CropBox { left: media.right - width, bottom: top - height, right: media.right, top }
}

/// The app's popup for the sticky note `parent`: exactly `/Type /Annot /Subtype /Popup /Parent /Rect /Open
/// false /F 28`, with the rectangle `popup_rect` gives. Anything else makes the note foreign.
pub(crate) fn app_popup(document: &Document, popup: ObjectId, parent: ObjectId, note: CropBox, media: CropBox) -> Result<(), String> {
    let dict = document.get_dictionary(popup).map_err(|error| error.to_string())?;
    keys(dict, &[b"Type", b"Subtype", b"Parent", b"Rect", b"Open", b"F"])?;
    name(dict, b"Type", b"Annot")?; name(dict, b"Subtype", b"Popup")?;
    let expected = popup_rect(note, media);
    if dict.get(b"Parent").and_then(Object::as_reference).ok() != Some(parent) || dict.get(b"Open").and_then(Object::as_bool).ok() != Some(false) || dict.get(b"F").and_then(Object::as_i64).ok() != Some(28)
        || values(dict.get(b"Rect").map_err(|error| error.to_string())?)? != [expected.left, expected.bottom, expected.right, expected.top] { return Err("The PDF has an altered note popup.".into()); }
    Ok(())
}

/// One annotation checked against the app's strict format. Any failure makes it foreign, not the file.
/// Returns the note, its appearance stream and, for a sticky note written since the popups slice, its popup.
pub(crate) fn app_note(document: &Document, page_id: ObjectId, id: ObjectId, annotation: &Dictionary) -> Result<(Note, ObjectId, Option<ObjectId>), String> {
                let kind = match annotation.get(b"Subtype").and_then(Object::as_name).ok() { Some(b"Text") => AnnotationKind::Note, Some(b"Highlight") => AnnotationKind::Highlight, _ => return Err("The PDF has a foreign annotation subtype.".into()) };
                match kind {
                    AnnotationKind::Note => { keys(annotation, &[b"Type", b"Subtype", b"Rect", b"NM", b"Contents", b"Name", b"C", b"Open", b"F", b"P", b"AP", b"Subj", b"M", b"CreationDate", b"Popup"])?; name(annotation, b"Name", b"Note")?; if annotation.get(b"Open").and_then(Object::as_bool).ok() != Some(false) { return Err("The PDF has unsupported popup state.".into()); } },
                    AnnotationKind::Highlight => keys(annotation, &[b"Type", b"Subtype", b"Rect", b"QuadPoints", b"NM", b"Contents", b"C", b"F", b"P", b"AP", b"Subj", b"M", b"CreationDate"])?
                }
                // Acrobat-style metadata the app writes since slice P2-4: optional (0.3.x and 1c files lack it),
                // but when present it must be exactly the app's form.
                if annotation.has(b"Subj") && annotation.get(b"Subj").and_then(Object::as_str).ok() != Some(subject(kind).as_bytes()) { return Err("The PDF has an altered annotation subject.".into()); }
                let date = |key: &[u8]| -> Result<Option<String>, String> {
                    let Ok(value) = annotation.get(key) else { return Ok(None) };
                    let text = value.as_str().ok().and_then(|bytes| std::str::from_utf8(bytes).ok()).filter(|text| valid_pdf_date(text)).ok_or("The PDF has an annotation date in a form the app does not write.")?;
                    Ok(Some(text.to_owned()))
                };
                let (created, modified) = (date(b"CreationDate")?, date(b"M")?);
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
                let note_id = std::str::from_utf8(id_bytes).map_err(|_| "The PDF has an invalid note ID.")?.to_owned();
                id_matches_kind(&note_id, kind)?;
                let contents = match (kind, annotation.get(b"Contents")) { (AnnotationKind::Highlight, Err(_)) => String::new(), (_, Ok(contents)) => decode_text(contents)?, _ => return Err("The note has no contents.".into()) };
                if kind == AnnotationKind::Note { validate_text(&contents)?; } else if annotation.has(b"Contents") && (contents.is_empty() || highlight_contents(Some(&contents))? != contents) { return Err("The PDF has a noncanonical empty highlight body.".into()); }
                let ap = annotation.get(b"AP").and_then(Object::as_dict).map_err(|_| "The PDF has unsupported note appearances.")?; keys(ap, &[b"N"])?;
                let appearance_id = ap.get(b"N").and_then(Object::as_reference).map_err(|_| "The PDF has unsupported note appearances.")?;
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
                let popup = match annotation.get(b"Popup") {
                    Err(_) => None,
                    Ok(value) => { let popup = value.as_reference().map_err(|_| "The PDF has an unsupported note popup.")?; app_popup(document, popup, id, bounds, media)?; Some(popup) }
                };
                Ok((Note { id: note_id, rect: bounds, contents, kind, quads, created, modified }, appearance_id, popup))
}

/// The app number in an annotation's `/NM`, when it carries the app's prefix (valid or not otherwise).
pub(crate) fn app_number(document: &Document, entry: &Object) -> Option<u64> {
    let dict = match entry { Object::Reference(id) => document.get_dictionary(*id).ok()?, Object::Dictionary(dict) => dict, _ => return None };
    let bytes = match dict.get(b"NM").ok()? { Object::Reference(id) => document.get_object(*id).ok()?, value => value }.as_str().ok()?;
    if bytes.len() > HIGHLIGHT_PREFIX.len() + 20 { return None; }
    number(std::str::from_utf8(bytes).ok()?).ok()
}

/// Reads every page's annotations. The app's own notes and highlights (the strict format of `app_note`,
/// referenced once from one page's `/Annots`, with an appearance no other annotation shares, a unique
/// `/NM`, and no other object referring to either) are editable. Everything else is foreign: it is counted
/// and left exactly as it is by `write`. Only document-level conditions refuse: encryption, signatures,
/// an unsupported version, forms, tagged structure, page labels, threads, a malformed `/Annots`, or more
/// app annotations than the app's limits.
pub fn read_owned(document: &Document) -> Result<Owned, String> {
    if document.is_encrypted() || document.encryption_state.is_some() { return Err("Encrypted PDFs cannot be commented on in this build.".into()); }
    let catalog = document.catalog().map_err(|error| error.to_string())?;
    effective_version(document)?;
    if catalog.has(b"Perms") || document.objects.values().any(|object| object.as_dict().is_ok_and(|dict| dict.has(b"ByteRange") || dict.get(b"Type").is_ok_and(|value| value.as_name().ok() == Some(b"Sig")))) { return Err("Signed or certified PDFs cannot be commented on in this build.".into()); }
    if [b"AcroForm".as_slice(), b"StructTreeRoot", b"PageLabels", b"Threads"].iter().any(|key| catalog.has(key)) { return Err("Commenting on forms, tagged PDFs, page labels, or article threads is not supported in this build.".into()); }
    let pages = document.get_pages().into_values().collect::<Vec<_>>();
    // (page index, annotation id, note, appearance id, indirect /Annots array it came from, popup)
    let mut candidates: Vec<(usize, ObjectId, Note, ObjectId, Option<ObjectId>, Option<ObjectId>)> = Vec::new();
    // The page each referenced /Annots entry was first seen on, so a note's popup must sit on the same page.
    let mut entry_page: std::collections::HashMap<ObjectId, usize> = std::collections::HashMap::new();
    let (mut entries_total, mut max_number) = (0usize, 0u64);
    let mut reference_counts: std::collections::HashMap<ObjectId, usize> = std::collections::HashMap::new();
    // How many pages use each indirect /Annots array. A shared array is walked once; its entries are on
    // several pages, so none of them is editable.
    let mut array_pages: std::collections::HashMap<ObjectId, usize> = std::collections::HashMap::new();
    let mut inline_entries: Vec<&Object> = Vec::new();
    for (index, page_id) in pages.iter().enumerate() {
        let Some((array_id, entries)) = page_annots(document, *page_id)? else { continue };
        if let Some(array_id) = array_id {
            let uses = array_pages.entry(array_id).or_insert(0); *uses += 1;
            if *uses > 1 { continue; }
        }
        entries_total += entries.len();
        if entries_total > crate::annotations::MAX_ENTRIES_TOTAL { return Err("The PDF has more annotation entries than this build edits (262,144). Annotations are shown but commenting is unavailable.".into()); }
        for entry in entries {
            if let Some(value) = app_number(document, entry) { max_number = max_number.max(value); }
            let Object::Reference(id) = entry else { inline_entries.push(entry); continue };
            *reference_counts.entry(*id).or_insert(0) += 1;
            entry_page.entry(*id).or_insert(index);
            if reference_counts[id] > 1 { continue; }
            if let Ok((note, appearance, popup)) = document.get_dictionary(*id).map_err(|error| error.to_string()).and_then(|dict| app_note(document, *page_id, *id, dict)) {
                candidates.push((index, *id, note, appearance, array_id, popup));
            }
        }
    }
    // Keep a candidate only if nothing else could depend on it: one `/Annots` reference, from an array no
    // other page or object uses, a unique `/NM`, an appearance of its own, and no reference to either from
    // any other object, inline annotation or the trailer.
    // A popup must be listed exactly once, on its note's page, and belong to one note only.
    let mut names = std::collections::HashMap::new();
    let mut appearances = std::collections::HashMap::new();
    let mut popup_uses = std::collections::HashMap::new();
    for (_, _, note, appearance, _, popup) in &candidates { *names.entry(note.id.clone()).or_insert(0usize) += 1; *appearances.entry(*appearance).or_insert(0usize) += 1; if let Some(popup) = popup { *popup_uses.entry(*popup).or_insert(0usize) += 1; } }
    let note_ids: HashSet<ObjectId> = candidates.iter().map(|(_, id, ..)| *id).collect();
    candidates.retain(|(index, id, note, appearance, array, popup)| reference_counts[id] == 1 && names[&note.id] == 1 && appearances[appearance] == 1 && !reference_counts.contains_key(appearance) && array.is_none_or(|array| array_pages[&array] == 1)
        && popup.is_none_or(|popup| popup_uses[&popup] == 1 && reference_counts.get(&popup) == Some(&1) && entry_page.get(&popup) == Some(index) && !appearances.contains_key(&popup) && !note_ids.contains(&popup)));
    let mut targets: HashSet<ObjectId> = candidates.iter().flat_map(|(_, id, _, appearance, _, popup)| [Some(*id), Some(*appearance), *popup]).flatten().collect();
    targets.extend(candidates.iter().filter_map(|(_, _, _, _, array, _)| *array));
    let page_set: HashSet<ObjectId> = pages.iter().copied().collect();
    let mut referenced = HashSet::new();
    for (id, object) in &document.objects {
        if targets.contains(id) { continue; }
        if page_set.contains(id) {
            if let Ok(page) = object.as_dict() { for (key, value) in page.iter() { if key != b"Annots" { collect_references(value, &targets, &mut referenced); } } }
        } else if array_pages.contains_key(id) {
            // Another page's /Annots array: its reference entries are counted above, but an inline
            // dictionary in it can still point at a candidate.
            if let Ok(values) = object.as_array() { for value in values.iter().filter(|value| !matches!(value, Object::Reference(_))) { collect_references(value, &targets, &mut referenced); } }
        } else { collect_references(object, &targets, &mut referenced); }
    }
    for entry in inline_entries { collect_references(entry, &targets, &mut referenced); }
    collect_references(&Object::Dictionary(document.trailer.clone()), &targets, &mut referenced);
    // The popups' own `/Parent` links back to their notes are skipped above (they are targets); any other
    // reference to a note, its appearance, its popup or its /Annots array keeps the unit foreign.
    candidates.retain(|(_, id, _, appearance, array, popup)| !referenced.contains(id) && !referenced.contains(appearance) && array.is_none_or(|array| !referenced.contains(&array)) && popup.is_none_or(|popup| !referenced.contains(&popup)));
    let (mut count, mut text_bytes, mut geometry_slots) = (0usize, 0usize, 0usize);
    let mut notes = vec![Vec::new(); pages.len()];
    let mut app = std::collections::HashMap::new();
    let mut popups = std::collections::HashMap::new();
    for (index, id, note, appearance, _, popup) in candidates {
        count += 1; text_bytes += note.contents.len(); geometry_slots += note.quads.as_ref().map_or(1, Vec::len);
        if geometry_slots > MAX_GEOMETRY_SLOTS { return Err("The PDF exceeds the 4,096 annotation geometry-slot limit. Annotations are unavailable.".into()); }
        if count > MAX_NOTES || text_bytes > MAX_TOTAL_BYTES { return Err("The PDF exceeds the 1,000-note or 1 MiB total note-text limit.".into()); }
        app.insert(id, (note.id.clone(), appearance));
        if let Some(popup) = popup { popups.insert(id, popup); }
        notes[index].push(note);
    }
    Ok(Owned { notes, foreign: entries_total - app.len() - popups.len(), app, popups, max_number })
}

/// The app's editable notes and highlights per page (see `read_owned`).
pub fn read(document: &Document) -> Result<Vec<Vec<Note>>, String> { read_owned(document).map(|owned| owned.notes) }

pub(crate) fn collect_references(value: &Object, owned: &HashSet<ObjectId>, found: &mut HashSet<ObjectId>) {
    let mut remaining = vec![value];
    while let Some(value) = remaining.pop() {
        match value {
            Object::Reference(id) if owned.contains(id) => { found.insert(*id); }
            Object::Array(values) => remaining.extend(values),
            Object::Dictionary(values) => remaining.extend(values.iter().map(|(_, value)| value)),
            Object::Stream(value) => remaining.extend(value.dict.iter().map(|(_, value)| value)),
            _ => (),
        }
    }
}


#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor::{EditSession, PageEdit, PageSpec};
    use lopdf::{dictionary, Stream};
    fn sample() -> Vec<u8> { std::fs::read(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("resources/welcome.pdf")).unwrap() }
    /// A one-page PDF with the given MediaBox.
    fn page_with_media_box(media: [f32; 4]) -> Vec<u8> {
        let mut document = Document::with_version("1.7");
        let pages = document.new_object_id();
        let contents = document.add_object(Stream::new(dictionary! {}, Vec::new()));
        let page = document.add_object(dictionary! { "Type" => "Page", "Parent" => pages, "MediaBox" => media.iter().map(|value| Object::Real(*value)).collect::<Vec<_>>(), "Contents" => contents, "Resources" => dictionary! {} });
        document.objects.insert(pages, Object::Dictionary(dictionary! { "Type" => "Pages", "Kids" => vec![page.into()], "Count" => 1 }));
        let catalog = document.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages });
        document.trailer.set("Root", catalog);
        let mut bytes = Vec::new(); document.save_to(&mut bytes).unwrap(); bytes
    }
    #[test]
    fn popup_rectangles_round_trip_on_a_fractional_media_box() {
        // A4 in points (595.276 x 841.89) with a fractional origin and a note at fractional coordinates: the popup
        // rectangle computed from the MediaBox must survive save and reload exactly, or the note would turn foreign.
        for media in [[0.0, 0.0, 595.276, 841.89], [12.345, 6.789, 607.621, 848.679], [0.0, 0.0, 150.5, 99.25]] {
            let mut session = EditSession::new(page_with_media_box(media), 1);
            let rect = CropBox { left: media[0] + 10.125, bottom: media[1] + 20.333, right: media[0] + 31.75, top: media[1] + 41.9 };
            let next = session.proposed_comment(Some((0, rect)), None, Some("Fractional")).unwrap(); session.commit_comments(next);
            let saved = session.export(None).unwrap();
            let owned = read_owned(&Document::load_mem(&saved).unwrap()).unwrap();
            assert_eq!((owned.app.len(), owned.popups.len(), owned.foreign), (1, 1, 0), "MediaBox {media:?}");
            let reopened = read_owned(&Document::load_mem(&EditSession::new(saved, 1).export(None).unwrap()).unwrap()).unwrap();
            assert_eq!(reopened.popups.len(), 1, "a second save keeps the pair intact: {media:?}");
        }
    }
    #[test]
    fn a_sticky_note_and_its_acrobat_style_popup_are_one_editable_unit() {
        let mut session = EditSession::new(sample(), 6);
        let rect = CropBox { left: 40.0, bottom: 60.0, right: 70.0, top: 90.0 };
        let next = session.proposed_comment(Some((0, rect)), None, Some("With popup")).unwrap(); session.commit_comments(next);
        let next = session.proposed_highlight(Some((0, CropBox { left: 100.0, bottom: 200.0, right: 180.0, top: 240.0 })), None, None, false).unwrap(); session.commit_comments(next);
        let output = Document::load_mem(&session.export(None).unwrap()).unwrap();
        let page = output.get_pages()[&1];
        let annots = output.get_dictionary(page).unwrap().get(b"Annots").unwrap().as_array().unwrap().clone();
        assert_eq!(annots.len(), 3, "note, its popup right after it, then the highlight (no popup)");
        let (note, popup) = (annots[0].as_reference().unwrap(), annots[1].as_reference().unwrap());
        assert_eq!(output.get_dictionary(note).unwrap().get(b"Popup").unwrap().as_reference().unwrap(), popup);
        let popup_dict = output.get_dictionary(popup).unwrap();
        assert_eq!(popup_dict.get(b"Subtype").unwrap().as_name().unwrap(), b"Popup");
        assert_eq!(popup_dict.get(b"Parent").unwrap().as_reference().unwrap(), note);
        assert_eq!(popup_dict.get(b"Open").unwrap().as_bool().unwrap(), false);
        assert_eq!(popup_dict.get(b"F").unwrap().as_i64().unwrap(), 28);
        let media = media_box(&output, page).unwrap();
        let area = values(popup_dict.get(b"Rect").unwrap()).unwrap();
        assert_eq!(area[2], media.right, "at the right edge of the page");
        assert_eq!(area[3], rect.top.max(media.bottom + 120.0), "level with the note's top, raised only to stay on the page");
        assert_eq!(area[3] - area[1], 120.0); assert_eq!(area[2] - area[0], 180.0);
        let high = popup_rect(CropBox { left: 40.0, bottom: 700.0, right: 70.0, top: 730.0 }, media);
        assert_eq!((high.top, high.bottom), (730.0, 610.0), "a note high on the page keeps its top");
        let owned = read_owned(&output).unwrap();
        assert_eq!(owned.notes[0], session.plan[0].notes);
        assert_eq!(owned.popups.get(&note), Some(&popup));
        assert_eq!(owned.foreign, 0, "the popup is part of the app's unit, so page delete is not blocked");
        // Editing regenerates the pair; deleting removes both.
        let mut reopened = EditSession::new(session.export(None).unwrap(), 6);
        let id = reopened.plan[0].notes[0].id.clone();
        let next = reopened.proposed_comment(None, Some(&id), Some("Edited")).unwrap(); reopened.commit_comments(next);
        let edited = Document::load_mem(&reopened.export(None).unwrap()).unwrap();
        assert_eq!(edited.get_dictionary(edited.get_pages()[&1]).unwrap().get(b"Annots").unwrap().as_array().unwrap().len(), 3);
        assert_eq!(read_owned(&edited).unwrap().popups.len(), 1);
        let next = reopened.proposed_comment(None, Some(&id), None).unwrap(); reopened.commit_comments(next);
        let deleted = Document::load_mem(&reopened.export(None).unwrap()).unwrap();
        let left = deleted.get_dictionary(deleted.get_pages()[&1]).unwrap().get(b"Annots").unwrap().as_array().unwrap().clone();
        assert_eq!(left.len(), 1, "only the highlight remains");
        assert!(deleted.objects.values().all(|object| object.as_dict().map_or(true, |dict| dict.get(b"Subtype").and_then(Object::as_name).ok() != Some(b"Popup"))), "the old popup is gone from the copy");
        assert!(reopened.apply(PageEdit::Delete { pages: vec![5] }).is_ok(), "an app-only file still allows page delete");
        // A popup in any other form, or one another object also uses, keeps the note foreign; both are kept.
        let alter = |change: &dyn Fn(&mut Document)| { let mut altered = output.clone(); change(&mut altered); altered };
        let cases: Vec<(&str, Document)> = vec![
            ("open popup", alter(&|doc| { doc.get_dictionary_mut(popup).unwrap().set("Open", true); })),
            ("moved popup", alter(&|doc| { doc.get_dictionary_mut(popup).unwrap().set("Rect", vec![0.into(), 0.into(), 10.into(), 10.into()]); })),
            ("extra key", alter(&|doc| { doc.get_dictionary_mut(popup).unwrap().set("Contents", Object::string_literal("x")); })),
            ("other parent", alter(&|doc| { let other = annots[2].as_reference().unwrap(); doc.get_dictionary_mut(popup).unwrap().set("Parent", other); })),
            ("popup missing from /Annots", alter(&|doc| { doc.get_dictionary_mut(page).unwrap().get_mut(b"Annots").unwrap().as_array_mut().unwrap().remove(1); })),
            ("popup listed twice", alter(&|doc| { doc.get_dictionary_mut(page).unwrap().get_mut(b"Annots").unwrap().as_array_mut().unwrap().push(Object::Reference(popup)); })),
            ("popup referenced elsewhere", alter(&|doc| { let info = doc.add_object(dictionary! { "Popup" => popup }); doc.trailer.set("Info", info); })),
        ];
        for (case, document) in cases {
            let owned = read_owned(&document).unwrap();
            assert!(!owned.app.contains_key(&note) && owned.popups.is_empty(), "{case}: the note is foreign");
            assert!(preserved(&document, note), "{case}: the note is kept as it was");
        }
        // A 0.3.x note without a popup still reads; when its page is rewritten it gains one.
        let mut legacy = output.clone();
        legacy.get_dictionary_mut(note).unwrap().remove(b"Popup");
        legacy.get_dictionary_mut(page).unwrap().get_mut(b"Annots").unwrap().as_array_mut().unwrap().remove(1);
        legacy.objects.remove(&popup);
        let owned = read_owned(&legacy).unwrap();
        assert!(owned.app.contains_key(&note) && owned.popups.is_empty());
    }
    #[test]
    fn new_annotation_ids_are_random_uuids_and_numbered_ids_still_read() {
        let ids: HashSet<String> = (0..1_000).map(|_| new_id().unwrap()).collect();
        assert_eq!(ids.len(), 1_000);
        assert!(ids.iter().all(|id| is_uuid(id) && crate::recovery_journal::annotation_uuid(id)));
        for (value, uuid) in [("5c1f0a52-1b7e-4c33-8a2b-0123456789ab", true), ("5C1F0A52-1B7E-4C33-8A2B-0123456789AB", false), ("5c1f0a52-1b7e-1c33-8a2b-0123456789ab", false), ("5c1f0a52-1b7e-4c33-7a2b-0123456789ab", false), ("5c1f0a521b7e4c338a2b0123456789ab", false), ("", false)] {
            assert_eq!(is_uuid(value), uuid, "{value}"); assert_eq!(crate::recovery_journal::annotation_uuid(value), uuid, "the journal's copy agrees: {value}");
        }
        assert!(id_matches_kind(&new_id().unwrap(), AnnotationKind::Note).is_ok() && id_matches_kind(&new_id().unwrap(), AnnotationKind::Highlight).is_ok());
        assert!(id_matches_kind(&id(3), AnnotationKind::Note).is_ok() && id_matches_kind(&id(3), AnnotationKind::Highlight).is_err(), "a numbered ID still carries its kind");
        // A new note is written with its UUID as /NM and reads back editable; a numbered 0.3.x note keeps its ID.
        let mut session = EditSession::new(sample(), 6);
        let next = session.proposed_comment(Some((0, CropBox { left: 40.0, bottom: 60.0, right: 70.0, top: 90.0 })), None, Some("Random")).unwrap(); session.commit_comments(next);
        let fresh = session.plan[0].notes[0].id.clone(); assert!(is_uuid(&fresh));
        let output = Document::load_mem(&session.export(None).unwrap()).unwrap();
        assert_eq!(read(&output).unwrap()[0][0].id, fresh);
        let mut legacy = output.clone();
        let annotation = legacy.get_dictionary(legacy.get_pages()[&1]).unwrap().get(b"Annots").unwrap().as_array().unwrap()[0].as_reference().unwrap();
        legacy.get_dictionary_mut(annotation).unwrap().set("NM", Object::string_literal(id(7)));
        assert_eq!(read(&legacy).unwrap()[0][0].id, id(7));
        legacy.get_dictionary_mut(annotation).unwrap().set("NM", Object::string_literal(highlight_id(7)));
        assert!(preserved(&legacy, annotation), "a numbered ID of the wrong kind makes the note foreign");
    }
    #[test]
    fn pdf_dates_are_utc_civil_dates_in_the_app_form() {
        assert_eq!(pdf_date(0), "D:19700101000000Z");
        assert_eq!(pdf_date(951_782_400), "D:20000229000000Z", "leap day in a century leap year");
        assert_eq!(pdf_date(1_762_473_599), "D:20251106235959Z");
        assert_eq!(pdf_date(4_102_444_800), "D:21000101000000Z");
        assert!(valid_pdf_date(&pdf_date_now()));
        for bad in ["D:20251106235959", "D:2025110623595Z", "D:20251306000000Z", "D:20251100000000Z", "D:20251106240000Z", "D:20251106236000Z", "20251106235959Z", "D:2025110623595xZ", "D:20251106235959-06'00'"] { assert!(!valid_pdf_date(bad), "{bad}"); }
        // One implementation (in the journal); real calendar days only.
        for (value, valid) in [("D:20240229120000Z", true), ("D:20230229120000Z", false), ("D:21000229120000Z", false), ("D:20000229120000Z", true), ("D:20250431000000Z", false), ("D:20250430000000Z", true), ("D:20250132000000Z", false), ("D:20251106240000Z", false), ("D:20251106235960Z", false), ("D:00001106235959Z", false), ("D:99991231235959Z", true), ("", false)] {
            assert_eq!(valid_pdf_date(value), valid, "{value}");
            assert_eq!(crate::recovery_journal::valid_annotation_date(value), valid, "{value}");
        }
    }
    #[test]
    fn new_app_annotations_carry_acrobat_style_dates_and_subject_but_no_author() {
        let mut session = EditSession::new(sample(), 6);
        let rect = CropBox { left: 40.0, bottom: 60.0, right: 70.0, top: 90.0 };
        let next = session.proposed_comment(Some((0, rect)), None, Some("Dated")).unwrap(); session.commit_comments(next);
        let next = session.proposed_highlight(Some((0, CropBox { left: 100.0, bottom: 200.0, right: 180.0, top: 240.0 })), None, None, false).unwrap(); session.commit_comments(next);
        let (note, highlight) = (session.plan[0].notes[0].clone(), session.plan[0].notes[1].clone());
        assert!(note.created.is_some() && note.created == note.modified && highlight.created.is_some());
        let output = Document::load_mem(&session.export(None).unwrap()).unwrap();
        let page = output.get_pages()[&1];
        for (index, expected) in [(0, "Sticky Note"), (2, "Highlight")] {
            let id = output.get_dictionary(page).unwrap().get(b"Annots").unwrap().as_array().unwrap()[index].as_reference().unwrap();
            let dict = output.get_dictionary(id).unwrap();
            assert_eq!(dict.get(b"Subj").unwrap().as_str().unwrap(), expected.as_bytes());
            assert!(valid_pdf_date(std::str::from_utf8(dict.get(b"CreationDate").unwrap().as_str().unwrap()).unwrap()) && dict.has(b"M"));
            assert!(!dict.has(b"T"), "the author stays blank until Joshua rules on Q1");
        }
        assert_eq!(read(&output).unwrap()[0], session.plan[0].notes, "dates and subject round-trip");
        // An unchanged edit keeps /M; a real edit changes /M and keeps /CreationDate.
        let revision = session.revision;
        let next = session.proposed_comment(None, Some(&note.id), Some("Dated")).unwrap(); session.commit_comments(next);
        assert_eq!(session.revision, revision);
        let mut edited = session.proposed_comment(None, Some(&note.id), Some("Changed")).unwrap();
        assert_eq!(edited[0].notes[0].created, note.created);
        assert!(edited[0].notes[0].modified.as_deref() >= note.modified.as_deref());
        edited[0].notes[0].modified = Some("D:20991231235959Z".into());
        session.commit_comments(edited);
        let output = Document::load_mem(&session.export(None).unwrap()).unwrap();
        assert_eq!(read(&output).unwrap()[0][0].modified.as_deref(), Some("D:20991231235959Z"));
        // A date or subject in any other form makes the annotation foreign (kept, not edited).
        let id = output.get_dictionary(page).unwrap().get(b"Annots").unwrap().as_array().unwrap()[0].as_reference().unwrap();
        for (key, value) in [("M", "D:20240305141500-06'00'"), ("CreationDate", "yesterday"), ("Subj", "Comment")] {
            let mut altered = output.clone(); altered.get_dictionary_mut(id).unwrap().set(key, Object::string_literal(value));
            assert!(preserved(&altered, id), "{key} = {value}");
        }
        // A 0.3.x note without dates still reads; editing it sets only /M.
        let (_, dated) = annotated();
        let mut legacy = Document::load_mem(&dated).unwrap();
        let first = legacy.get_dictionary(legacy.get_pages()[&1]).unwrap().get(b"Annots").unwrap().as_array().unwrap()[0].as_reference().unwrap();
        for key in [b"M".as_slice(), b"CreationDate", b"Subj"] { legacy.get_dictionary_mut(first).unwrap().remove(key); }
        let mut legacy_bytes = Vec::new(); legacy.save_to(&mut legacy_bytes).unwrap();
        let mut old = EditSession::new(legacy_bytes, 6);
        assert!(old.plan[0].notes[0].created.is_none());
        let id = old.plan[0].notes[0].id.clone();
        let next = old.proposed_comment(None, Some(&id), Some("Edited later")).unwrap(); old.commit_comments(next);
        assert!(old.plan[0].notes[0].created.is_none() && old.plan[0].notes[0].modified.is_some());
    }
    #[test]
    fn recovery_records_keep_annotation_dates_and_dateless_records_stay_canonical() {
        use crate::recovery_journal::*;
        let annotation = |created: Option<&str>| RecoveryAnnotation { id: id(1), kind: RecoveryAnnotationKind::Note, rect: RecoveryRect { left: 40.0, bottom: 60.0, right: 70.0, top: 90.0 }, contents: "x".into(), quads: Vec::new(), created: created.map(str::to_owned), modified: created.map(str::to_owned) };
        let record = |annotation: RecoveryAnnotation| RecoveryRecord { active: true, generation: 1, revision: 1, current_page: 0, source_pages: 1, source: SourceIdentity { bytes: 10, sha256: [7; 32] }, edit_state: RecoveryEditState { pages: vec![RecoveryPage { source: 0, turns: 0, crop: None, annotations: vec![annotation] }] } };
        for created in [None, Some("D:20251106235959Z")] {
            let original = record(annotation(created));
            let bytes = encode_recovery(&original).unwrap();
            assert_eq!(decode_recovery(&bytes, original.source, None, 0).unwrap(), original);
            assert_eq!(bytes.windows(9).any(|window| window == b"\"created\""), created.is_some(), "dateless records are unchanged from version 2");
            let version = u16::from_le_bytes([bytes[8], bytes[9]]);
            assert_eq!(version, if created.is_some() { 3 } else { 2 }, "dated records are version 3, dateless ones stay 2");
            // The other version with the same contents is refused (re-stamped and re-digested).
            let mut swapped = bytes.clone();
            swapped[8..10].copy_from_slice(&(if version == 3 { 2u16 } else { 3u16 }).to_le_bytes());
            let body = swapped.len() - 32; let digest = <sha2::Sha256 as sha2::Digest>::digest(&swapped[..body]); swapped[body..].copy_from_slice(&digest);
            assert!(decode_recovery(&swapped, original.source, None, 0).unwrap_err().contains("does not match its contents"));
        }
        assert!(encode_recovery(&record(annotation(Some("not a date")))).is_err());
        assert!(encode_recovery(&record(annotation(Some("D:20250230000000Z")))).is_err(), "no February 30th");
        // SM7 L-1: a generator failure refuses the edit and leaves the session as it was; nothing panics.
        let mut session = EditSession::new(sample(), 6);
        let before = session.plan.clone();
        FAIL_RANDOM.with(|fail| fail.set(true));
        let refused = session.proposed_comment(Some((0, CropBox { left: 40.0, bottom: 60.0, right: 70.0, top: 90.0 })), None, Some("x"));
        let refused_highlight = session.proposed_text_highlight(0, vec![CropBox { left: 40.0, bottom: 60.0, right: 70.0, top: 90.0 }], None);
        FAIL_RANDOM.with(|fail| fail.set(false));
        assert!(refused.unwrap_err().contains("random number generator") && refused_highlight.unwrap_err().contains("random number generator"));
        assert_eq!(session.plan, before);
        let next = session.proposed_comment(Some((0, CropBox { left: 40.0, bottom: 60.0, right: 70.0, top: 90.0 })), None, Some("x")).unwrap(); session.commit_comments(next);
        // SM7 M-1: a dateless note with a UUID ID is unreadable by 0.3.x too, so it is version 3; restamped as 2, refused.
        let mut uuid_note = annotation(None); uuid_note.id = new_id().unwrap();
        let original = record(uuid_note);
        let bytes = encode_recovery(&original).unwrap();
        assert_eq!(u16::from_le_bytes([bytes[8], bytes[9]]), 3);
        assert_eq!(decode_recovery(&bytes, original.source, None, 0).unwrap(), original);
        for older in [1u16, 2] {
            let mut restamped = bytes.clone(); restamped[8..10].copy_from_slice(&older.to_le_bytes());
            let body = restamped.len() - 32; let digest = <sha2::Sha256 as sha2::Digest>::digest(&restamped[..body]); restamped[body..].copy_from_slice(&digest);
            assert!(decode_recovery(&restamped, original.source, None, 0).unwrap_err().contains("does not match its contents"), "version {older}");
        }
    }
    #[test]
    fn quads_are_written_in_acrobat_order_and_both_orders_read_back() {
        let (session, _) = annotated();
        let mut session = session;
        let quads = vec![CropBox { left: 40.0, bottom: 300.0, right: 80.0, top: 315.0 }, CropBox { left: 40.0, bottom: 270.0, right: 65.0, top: 285.0 }];
        let next = session.proposed_text_highlight(0, quads.clone(), None).unwrap(); session.commit_comments(next);
        let area = CropBox { left: 200.0, bottom: 220.0, right: 250.0, top: 260.0 };
        let next = session.proposed_highlight(Some((0, area)), None, None, false).unwrap(); session.commit_comments(next);
        let output = Document::load_mem(&session.export(None).unwrap()).unwrap();
        let page = output.get_pages()[&1];
        let annots = output.get_dictionary(page).unwrap().get(b"Annots").unwrap().as_array().unwrap().clone();
        let points = |id: ObjectId| values(output.get_dictionary(id).unwrap().get(b"QuadPoints").unwrap()).unwrap();
        let (text, area_id) = (annots[annots.len() - 2].as_reference().unwrap(), annots[annots.len() - 1].as_reference().unwrap());
        assert_eq!(points(text), vec![40.0, 315.0, 80.0, 315.0, 40.0, 300.0, 80.0, 300.0, 40.0, 285.0, 65.0, 285.0, 40.0, 270.0, 65.0, 270.0], "TL, TR, BL, BR per quad");
        assert_eq!(points(area_id), vec![200.0, 260.0, 250.0, 260.0, 200.0, 220.0, 250.0, 220.0]);
        let expected = read(&output).unwrap();
        assert_eq!(expected[0], session.plan[0].notes);
        // A file from 0.3.x: the same annotations with the old counter-clockwise order still read identically.
        let mut legacy = output.clone();
        for id in [text, area_id] {
            let old: Vec<Object> = points(id).chunks_exact(8).flat_map(|p| { let r = quad_box(p).unwrap(); legacy_quad(r) }).map(Object::Real).collect();
            legacy.get_dictionary_mut(id).unwrap().set("QuadPoints", old);
        }
        assert_eq!(read(&legacy).unwrap(), expected);
        // Mixed orders within one annotation are fine; any other permutation or a rotated quad is not ours.
        let mut mixed = output.clone();
        let mut both = legacy_quad(quads[0]); both.extend(quad(quads[1]));
        mixed.get_dictionary_mut(text).unwrap().set("QuadPoints", both.into_iter().map(Object::Real).collect::<Vec<_>>());
        assert_eq!(read(&mixed).unwrap(), expected);
        let q = quads[0];
        for wrong in [vec![q.right, q.top, q.left, q.top, q.right, q.bottom, q.left, q.bottom], vec![q.left, q.top, q.right, q.top + 1.0, q.left, q.bottom, q.right, q.bottom]] {
            let mut altered = output.clone();
            let mut points = wrong; points.extend(quad(quads[1]));
            altered.get_dictionary_mut(text).unwrap().set("QuadPoints", points.into_iter().map(Object::Real).collect::<Vec<_>>());
            assert!(preserved(&altered, text), "an altered quad makes the highlight foreign: kept, not edited");
        }
    }
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
        let page = output.get_pages()[&1]; let annotation = output.get_dictionary(page).unwrap().get(b"Annots").unwrap().as_array().unwrap()[3].as_reference().unwrap();
        let ap = output.get_dictionary(annotation).unwrap().get(b"AP").unwrap().as_dict().unwrap().get(b"N").unwrap().as_reference().unwrap();
        let content = &output.get_object(ap).unwrap().as_stream().unwrap().content;
        assert_eq!(std::str::from_utf8(content).unwrap().matches(" re ").count(), 4); assert_eq!(std::str::from_utf8(content).unwrap().matches("f Q").count(), 1, "Overlapping quads use exactly one nonzero-winding fill");
        for values in [vec![0.into(); 7], vec![0.into(); 8 * (MAX_HIGHLIGHT_QUADS + 1)], vec![Object::Real(f32::NAN); 8]] { let mut altered = output.clone(); altered.get_dictionary_mut(annotation).unwrap().set("QuadPoints", values); assert!(preserved(&altered, annotation)); }
        let mut altered = output.clone(); altered.get_object_mut(ap).unwrap().as_stream_mut().unwrap().content.extend(b" f"); assert!(preserved(&altered, annotation));
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
        let mut duplicate = output.get_dictionary(original).unwrap().clone(); duplicate.set("NM", Object::string_literal(highlight_id(999))); let duplicate = output.add_object(duplicate); output.get_dictionary_mut(page).unwrap().get_mut(b"Annots").unwrap().as_array_mut().unwrap().push(Object::Reference(duplicate)); assert!(preserved(&output, original) && preserved(&output, duplicate), "two annotations sharing one appearance are both kept as foreign, so they cannot push the app past its limits");
        session.apply(PageEdit::Undo).unwrap(); assert!(session.dirty()); let next = session.proposed_text_highlight(0, vec![bounds], None).unwrap(); session.commit_comments(next); assert!(!session.can_redo()); assert!(is_uuid(&session.plan[0].notes.last().unwrap().id) && session.plan[0].notes.last().unwrap().id != saved[0].notes.last().unwrap().id); session.apply(PageEdit::Undo).unwrap(); session.apply(PageEdit::Redo).unwrap(); assert!(validate_plan(&session.plan).is_ok()); assert_eq!(session.source, source);
    }
    /// The annotation object `id` is not editable, and adding a note on its page keeps it byte for byte
    /// (same object, same place among the page's other `/Annots` entries).
    fn preserved(document: &Document, id: ObjectId) -> bool {
        let Ok(owned) = read_owned(document) else { return false };
        if owned.app.contains_key(&id) { return false; }
        let page = document.get_pages().into_values().position(|page| page_annots(document, page).ok().flatten().is_some_and(|(_, entries)| entries.contains(&Object::Reference(id)))).unwrap();
        let mut plan: Vec<PageSpec> = owned.notes.iter().enumerate().map(|(source, notes)| PageSpec { source, turns: 0, crop: None, notes: notes.clone() }).collect();
        plan[page].notes.push(Note { id: self::id(owned.max_number + 1), rect: CropBox { left: 40.0, bottom: 60.0, right: 70.0, top: 90.0 }, contents: "added".into(), kind: AnnotationKind::Note, quads: None, created: None, modified: None });
        let before = document.get_object(id).unwrap().clone();
        let entries = |document: &Document| page_annots(document, document.get_pages().into_values().nth(page).unwrap()).unwrap().unwrap().1.to_vec();
        let position = entries(document).iter().position(|entry| entry == &Object::Reference(id)).unwrap();
        let mut written = document.clone();
        write(&mut written, &plan).unwrap();
        // Compared through Debug so a NaN value (not equal to itself) still counts as unchanged.
        written.get_object(id).ok().map(|object| format!("{object:?}")) == Some(format!("{before:?}")) && entries(&written).iter().position(|entry| entry == &Object::Reference(id)) == Some(position)
    }
    /// `pages` pages, each with `/Annots` taken from `annots(page_index)`.
    fn many_pages(pages: usize, mut annots: impl FnMut(&mut Document, usize) -> Object, extra: impl FnOnce(&mut Document)) -> Document {
        let mut document = Document::with_version("1.7");
        let tree = document.new_object_id();
        let kids: Vec<Object> = (0..pages).map(|index| { let annots = annots(&mut document, index); document.add_object(dictionary! { "Type" => "Page", "Parent" => tree, "MediaBox" => vec![0.into(), 0.into(), 200.into(), 200.into()], "Annots" => annots }).into() }).collect();
        document.objects.insert(tree, Object::Dictionary(dictionary! { "Type" => "Pages", "Kids" => kids, "Count" => pages as i64 }));
        let catalog = document.add_object(dictionary! { "Type" => "Catalog", "Pages" => tree });
        document.trailer.set("Root", catalog);
        extra(&mut document);
        document
    }

    /// Times `read_owned`, a session open (parse excluded) and a write that adds one note on the first page.
    fn timed_open_and_write(document: &Document, pages: usize) -> ([std::time::Duration; 3], Document) {
        let started = std::time::Instant::now();
        read_owned(document).unwrap();
        let read = started.elapsed();
        let started = std::time::Instant::now();
        let session = EditSession::opened(Vec::<u8>::new(), Ok(document.clone()), pages, None);
        let open = started.elapsed();
        assert!(session.comments_reason().is_none());
        let mut plan: Vec<PageSpec> = (0..pages).map(|source| PageSpec { source, turns: 0, crop: None, notes: Vec::new() }).collect();
        plan[0].notes.push(Note { id: id(1), rect: CropBox { left: 40.0, bottom: 60.0, right: 70.0, top: 90.0 }, contents: "added".into(), kind: AnnotationKind::Note, quads: None, created: None, modified: None });
        let mut written = document.clone();
        let started = std::time::Instant::now();
        write(&mut written, &plan).unwrap();
        ([read, open, started.elapsed()], written)
    }

    /// The shared-array cost is compared with the same 100,000 pages carrying an empty `/Annots` each, timed
    /// in the same run, so machine load and build profile cancel out. Reading the shared array once adds about
    /// 65,536 entries of work (measured ratio about 1.5-3x). The per-page rescan SM7 found would be about
    /// 65,536 times the baseline per page. The 10x limit fails on any per-page walk of the shared array,
    /// whatever the machine.
    #[test]
    fn a_shared_annots_array_is_read_once_and_open_and_write_stay_fast() {
        const PAGES: usize = 100_000;
        let baseline = many_pages(PAGES, |_, _| Object::Array(Vec::new()), |_| {});
        let mut shared = None;
        let document = many_pages(PAGES, |document, _| {
            let id = *shared.get_or_insert_with(|| { let entries: Vec<Object> = (0..65_536).map(|_| document.add_object(dictionary! { "Type" => "Annot", "Subtype" => "Link", "Rect" => vec![0.into(), 0.into(), 1.into(), 1.into()] }).into()).collect(); document.add_object(Object::Array(entries)) });
            Object::Reference(id)
        }, |_| {});
        let owned = read_owned(&document).unwrap();
        assert!(owned.app.is_empty() && owned.foreign == 65_536, "the shared array is walked once");
        let (base, _) = timed_open_and_write(&baseline, PAGES);
        let (times, written) = timed_open_and_write(&document, PAGES);
        println!("100,000 pages: empty /Annots each {base:?}; one shared 65,536-entry /Annots {times:?} (read_owned, open, write)");
        // An absolute ceiling as well, so a regression that slows the baseline too fails instead of hanging CI.
        let ceiling = std::time::Duration::from_secs(if cfg!(debug_assertions) { 300 } else { 60 });
        for (step, (shared, base)) in ["read_owned", "open", "write"].iter().zip(times.iter().zip(base.iter())) {
            assert!(*shared < *base * 10 + std::time::Duration::from_millis(200), "{step}: {shared:?} against a baseline of {base:?}");
            assert!(*shared < ceiling && *base < ceiling, "{step}: {shared:?} (baseline {base:?}) is over the {ceiling:?} ceiling");
        }
        assert!(matches!(written.get_dictionary(written.get_pages()[&2]).unwrap().get(b"Annots").unwrap(), Object::Reference(_)), "other pages keep the shared array");
        assert_eq!(page_annots(&written, written.get_pages()[&1]).unwrap().unwrap().1.len(), 65_538, "the shared entries plus the new note and its popup");
    }

    #[test]
    fn more_entries_than_the_document_cap_refuse_commenting_but_still_open_and_list() {
        let document = many_pages(5, |document, _| { let link = document.add_object(dictionary! { "Type" => "Annot", "Subtype" => "Square", "Rect" => vec![0.into(), 0.into(), 1.into(), 1.into()] }); Object::Array(vec![Object::Reference(link); 65_536]) }, |_| {});
        assert!(read_owned(&document).err().unwrap().contains("262,144"));
        let session = EditSession::opened(Vec::<u8>::new(), Ok(document), 5, None);
        assert!(session.comments_reason().unwrap().contains("262,144"));
        assert!(session.view_only_annotations().unwrap().pages.iter().map(Vec::len).sum::<usize>() > 0, "the bounded survey still lists them");
    }

    #[test]
    fn an_inline_reply_or_another_referrer_of_the_annots_array_keeps_an_app_note_view_only() {
        let (_, bytes) = annotated();
        let base = Document::load_mem(&bytes).unwrap();
        let page = base.get_pages()[&1];
        let note = base.get_dictionary(page).unwrap().get(b"Annots").unwrap().as_array().unwrap()[0].as_reference().unwrap();
        assert!(read_owned(&base).unwrap().app.contains_key(&note));
        // An inline reply (/IRT) or inline popup (/Parent) in /Annots pointing at the note, in a direct and in an
        // indirect array: the note stays foreign, so the inline entry never points at a deleted object.
        for (key, subtype) in [("IRT", "Text"), ("Parent", "Popup")] {
            let mut inline = base.clone();
            inline.get_dictionary_mut(page).unwrap().get_mut(b"Annots").unwrap().as_array_mut().unwrap().push(Object::Dictionary(dictionary! { "Type" => "Annot", "Subtype" => subtype, key => note }));
            assert!(preserved(&inline, note), "direct array, inline /{key}");
            let entries = inline.get_dictionary(page).unwrap().get(b"Annots").unwrap().clone();
            let array = inline.add_object(entries);
            inline.get_dictionary_mut(page).unwrap().set("Annots", array);
            assert!(preserved(&inline, note), "indirect array, inline /{key}");
        }
        // An indirect /NM on another annotation still counts toward the highest app ID.
        let mut indirect_name = base.clone();
        let name = indirect_name.add_object(Object::string_literal(id(4_242)));
        let other = indirect_name.add_object(dictionary! { "Type" => "Annot", "Subtype" => "Square", "NM" => name });
        indirect_name.get_dictionary_mut(page).unwrap().get_mut(b"Annots").unwrap().as_array_mut().unwrap().push(other.into());
        assert_eq!(read_owned(&indirect_name).unwrap().max_number, 4_242);
        // An indirect /Annots array that another object also refers to.
        let mut shared = base.clone();
        let entries = shared.get_dictionary(page).unwrap().get(b"Annots").unwrap().clone();
        let array = shared.add_object(entries);
        shared.get_dictionary_mut(page).unwrap().set("Annots", array);
        assert!(read_owned(&shared).unwrap().app.contains_key(&note), "an indirect array used by one page only is fine");
        let info = shared.add_object(dictionary! { "Annotations" => array });
        shared.trailer.set("Info", info);
        assert!(preserved(&shared, note), "an /Annots array another object refers to keeps its entries foreign");
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
        let page = output.get_pages()[&1]; let annotation = output.get_dictionary(page).unwrap().get(b"Annots").unwrap().as_array().unwrap()[2].as_reference().unwrap();
        for key in [b"AA".as_slice(), b"A", b"Popup", b"Name", b"Open", b"Unknown"] { let mut altered = output.clone(); altered.get_dictionary_mut(annotation).unwrap().set(key, Object::Null); assert!(preserved(&altered, annotation)); }
        let mut altered = output.clone(); altered.get_dictionary_mut(annotation).unwrap().set("QuadPoints", vec![0.into(); 8]); assert!(preserved(&altered, annotation));
        let ap = output.get_dictionary(annotation).unwrap().get(b"AP").unwrap().as_dict().unwrap().get(b"N").unwrap().as_reference().unwrap();
        let mut altered = output.clone(); let resources = altered.get_object_mut(ap).unwrap().as_stream_mut().unwrap().dict.get_mut(b"Resources").unwrap().as_dict_mut().unwrap(); resources.get_mut(b"ExtGState").unwrap().as_dict_mut().unwrap().get_mut(b"GS0").unwrap().as_dict_mut().unwrap().set("ca", Object::Real(0.5)); assert!(preserved(&altered, annotation));
        let mut altered = output.clone(); altered.get_object_mut(ap).unwrap().as_stream_mut().unwrap().content.push(b' '); assert!(preserved(&altered, annotation));
        assert_eq!(session.source, legacy);
    }
    #[test]
    fn highlights_optional_body_caps_history_and_cross_kind_ids_are_consistent() {
        let mut session = EditSession::new(sample(), 6); let bounds = CropBox { left: 100.0, bottom: 200.0, right: 180.0, top: 240.0 };
        let next = session.proposed_highlight(Some((0, bounds)), None, Some(" \n\t"), false).unwrap(); session.commit_comments(next); let first = session.plan[0].notes[0].id.clone(); assert!(session.plan[0].notes[0].contents.is_empty());
        let revision = session.revision; let next = session.proposed_highlight(None, Some(&first), None, false).unwrap(); session.commit_comments(next); assert_eq!(session.revision, revision);
        assert!(session.proposed_comment(None, Some(&first), Some("Wrong kind")).is_err()); assert!(session.proposed_comment(None, Some(&first), None).is_err());
        session.apply(PageEdit::Undo).unwrap(); let next = session.proposed_comment(Some((0, bounds)), None, Some("Note")).unwrap(); session.commit_comments(next); let note = session.plan[0].notes[0].id.clone(); assert!(is_uuid(&note) && note != first); assert!(!session.can_redo()); assert!(session.proposed_highlight(None, Some(&note), None, true).is_err());
        let next = session.proposed_highlight(Some((0, bounds)), None, None, false).unwrap(); session.commit_comments(next); let highlight = session.plan[0].notes[1].id.clone(); session.mark_saved();
        let next = session.proposed_highlight(None, Some(&highlight), Some("Body é 😀"), false).unwrap(); session.commit_comments(next); assert!(session.dirty()); session.apply(PageEdit::Undo).unwrap(); assert!(!session.dirty()); session.apply(PageEdit::Redo).unwrap(); assert!(session.dirty());
        assert!(highlight_contents(Some("\0")).is_err()); assert!(highlight_contents(Some(&"x".repeat(MAX_TEXT_BYTES + 1))).is_err());
        let next = session.proposed_comment(None, Some(&note), None).unwrap(); session.commit_comments(next); let next = session.proposed_highlight(None, Some(&highlight), None, true).unwrap(); session.commit_comments(next); let output = Document::load_mem(&session.export(None).unwrap()).unwrap(); assert!(output.get_pages().values().all(|page| !output.get_dictionary(*page).unwrap().has(b"Annots"))); assert!(read(&output).unwrap().iter().all(Vec::is_empty));
        session.plan[0].notes = (1..=MAX_NOTES).map(|value| Note { quads: None, created: None, modified: None, id: if value % 2 == 0 { highlight_id(value as u64) } else { id(value as u64) }, kind: if value % 2 == 0 { AnnotationKind::Highlight } else { AnnotationKind::Note }, rect: bounds, contents: "x".into() }).collect(); assert!(validate_plan(&session.plan).is_ok()); session.plan[0].notes.push(Note { quads: None, created: None, modified: None, id: highlight_id(1001), kind: AnnotationKind::Highlight, rect: bounds, contents: String::new() }); assert!(validate_plan(&session.plan).unwrap_err().contains("1,000"));
        session.plan[0].notes = (1..=128).map(|value| Note { quads: None, created: None, modified: None, id: if value % 2 == 0 { highlight_id(value) } else { id(value) }, kind: if value % 2 == 0 { AnnotationKind::Highlight } else { AnnotationKind::Note }, rect: bounds, contents: "x".repeat(MAX_TEXT_BYTES) }).collect(); assert!(validate_plan(&session.plan).is_ok()); session.plan[0].notes.push(Note { quads: None, created: None, modified: None, id: highlight_id(129), kind: AnnotationKind::Highlight, rect: bounds, contents: "x".into() }); assert!(validate_plan(&session.plan).unwrap_err().contains("1 MiB"));
    }
    #[test]
    fn comments_unicode_owned_schema_reopens_and_foreign_links_or_appearance_refuse() {
        let (session, bytes) = annotated(); let document = Document::load_mem(&bytes).unwrap();
        let notes = read(&document).unwrap(); assert_eq!(notes[0], session.plan[0].notes);
        assert_eq!(session.source, sample());
        let page = document.get_pages()[&1]; let annotation = document.get_dictionary(page).unwrap().get(b"Annots").unwrap().as_array().unwrap()[0].as_reference().unwrap();
        for key in [b"AA".as_slice(), b"A", b"Popup", b"Unknown"] {
            let mut altered = document.clone(); altered.get_dictionary_mut(annotation).unwrap().set(key, Object::Null); assert!(preserved(&altered, annotation));
        }
        let mut altered = document.clone(); let ap = altered.get_dictionary(annotation).unwrap().get(b"AP").unwrap().as_dict().unwrap().get(b"N").unwrap().as_reference().unwrap();
        altered.get_object_mut(ap).unwrap().as_stream_mut().unwrap().content.push(b' '); assert!(preserved(&altered, annotation));
        let mut altered = document.clone(); altered.get_dictionary_mut(page).unwrap().get_mut(b"Annots").unwrap().as_array_mut().unwrap().push(Object::Reference(annotation)); assert!(preserved(&altered, annotation));
        let mut altered = document.clone(); let dependency = altered.add_object(dictionary! { "Unrelated" => annotation }); altered.trailer.set("Info", dependency); assert!(preserved(&altered, annotation));
        let mut empty = Document::load_mem(&sample()).unwrap(); let page = empty.get_pages()[&1]; empty.get_dictionary_mut(page).unwrap().set("Annots", Vec::<Object>::new());
        let original_page = empty.get_dictionary(page).unwrap().clone(); assert!(read(&empty).unwrap().iter().all(Vec::is_empty), "an empty /Annots is simply no annotations"); assert_eq!(empty.get_dictionary(page).unwrap(), &original_page);
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
        session.plan[0].notes = (1..=MAX_NOTES).map(|value| Note { quads: None, created: None, modified: None, id: id(value as u64), rect: bounds, contents: "x".into(), kind: AnnotationKind::Note }).collect(); assert!(validate_plan(&session.plan).is_ok());
        session.plan[0].notes.push(Note { quads: None, created: None, modified: None, id: id(MAX_NOTES as u64 + 1), rect: bounds, contents: "x".into(), kind: AnnotationKind::Note }); assert!(validate_plan(&session.plan).unwrap_err().contains("1,000"));
        session.plan[0].notes = (1..=128).map(|value| Note { quads: None, created: None, modified: None, id: id(value), rect: bounds, contents: "x".repeat(MAX_TEXT_BYTES), kind: AnnotationKind::Note }).collect(); assert!(validate_plan(&session.plan).is_ok());
        session.plan[0].notes.push(Note { quads: None, created: None, modified: None, id: id(129), rect: bounds, contents: "x".into(), kind: AnnotationKind::Note }); assert!(validate_plan(&session.plan).unwrap_err().contains("1 MiB"));
        session.plan[0].notes = vec![Note { quads: None, created: None, modified: None, id: id(1), rect: bounds, contents: "x".into(), kind: AnnotationKind::Note }; 2]; assert!(validate_plan(&session.plan).unwrap_err().contains("duplicate"));
        let (_, bytes) = annotated(); let mut pdf = Document::load_mem(&bytes).unwrap(); let page = pdf.get_pages()[&1];
        let original = pdf.get_dictionary(page).unwrap().get(b"Annots").unwrap().as_array().unwrap()[0].as_reference().unwrap();
        let duplicate = pdf.add_object(pdf.get_object(original).unwrap().clone()); pdf.get_dictionary_mut(page).unwrap().get_mut(b"Annots").unwrap().as_array_mut().unwrap().push(Object::Reference(duplicate)); assert!(preserved(&pdf, original) && preserved(&pdf, duplicate), "a duplicated /NM makes both copies foreign");
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

#[cfg(test)]
#[path = "annotation_lockdown_tests.rs"]
mod lockdown_tests;
