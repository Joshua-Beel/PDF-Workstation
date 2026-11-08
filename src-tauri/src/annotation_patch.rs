//! Typed patches to other apps' Ink annotations (SM3 P2-7g, groundwork for 7c). Not wired to editing, the editing
//! plan or the recovery journal yet: 7c connects these to journalled editing (an SM5 record) and the UI.
//!
//! The rules are SM7's design ruling `sm7-ruling-annot-p2-7g-foreign-ap` (2026-11-07):
//! - Only allow-listed keys change: a move writes `/Rect` and `/InkList`; a style edit writes `/C`, `/BS` (its
//!   `/W`) and `/AP`; every patch writes `/M`. `/A`, `/AA`, `/FS`, `/EF`, `/JS`, `/OC`, `/StructParent`, `/IRT`,
//!   `/Popup`, `/P`, `/NM`, `/T` and every unknown key stay as they were (checked after each patch).
//! - A move keeps the other app's `/AP` byte-identical: its BBox maps onto the shifted `/Rect`.
//! - A colour or width edit replaces `/AP` with exactly `<< /N new >>`, a stream the app generates from typed
//!   values only. The old appearance objects are never modified or deleted; only the annotation's reference to
//!   them is dropped, and pruning removes them only when nothing else refers to them.
//!
//! For 7c (SM7 conditions 7 and 8): a style edit drops on-disk appearance bytes, so the next save of an unsigned
//! file must be a full rewrite with prune through `removes_saved_annotation` or its successor, failing closed.
//! The colour and width UI must say the stroke will be redrawn from its saved path data and that Undo restores it.
use crate::annotation_model::{read_ink_list, ink_bounds, valid_pdf_date, MAX_INK_COORDINATE, MAX_INK_WIDTH};
use crate::annotation_write::{ink_appearance, ink_list};
use crate::editor::CropBox;
use lopdf::{dictionary, Dictionary, Document, Object, ObjectId, Stream, StringFormat};

const MOVE_KEYS: &[&[u8]] = &[b"Rect", b"InkList", b"M"];
const STYLE_KEYS: &[&[u8]] = &[b"C", b"BS", b"AP", b"AS", b"M"];

/// Another app's Ink, read with the strict editable validation (SM7 condition 6): `/Subtype /Ink`, a finite
/// `/Rect` with positive size, an `/InkList` within the limits, and in-range `/C`, `/BS /W` and `/CA`.
#[derive(Debug)]
pub(crate) struct ForeignInk { pub strokes: Vec<Vec<(f32, f32)>>, pub rect: CropBox, pub color: Option<Vec<f32>>, pub width: f32, pub opacity: f32 }

fn number(document: &Document, value: &Object) -> Result<f32, String> {
    document.dereference(value).ok().and_then(|(_, value)| value.as_float().ok()).filter(|value| value.is_finite()).ok_or_else(|| "The drawing has a non-number where a number belongs.".to_owned())
}

fn numbers(document: &Document, value: &Object) -> Result<Vec<f32>, String> {
    let (_, value) = document.dereference(value).map_err(|_| "The drawing has an unreadable array.".to_owned())?;
    value.as_array().map_err(|_| "The drawing has a non-array where an array belongs.".to_owned())?.iter().map(|value| number(document, value)).collect()
}

fn border_style(document: &Document, annotation: &Dictionary) -> Result<Option<Dictionary>, String> {
    match annotation.get(b"BS") {
        Err(_) => Ok(None),
        Ok(value) => document.dereference(value).ok().and_then(|(_, value)| value.as_dict().ok().cloned()).map(Some).ok_or_else(|| "The drawing has an unreadable border style.".to_owned()),
    }
}

pub(crate) fn read_foreign_ink(document: &Document, annotation: &Dictionary) -> Result<ForeignInk, String> {
    if annotation.get(b"Subtype").and_then(Object::as_name).ok() != Some(b"Ink") { return Err("Only drawings (Ink annotations) can be patched here.".into()); }
    let rect = numbers(document, annotation.get(b"Rect").map_err(|_| "The drawing has no /Rect.".to_owned())?)?;
    if rect.len() != 4 || rect.iter().any(|value| value.abs() > MAX_INK_COORDINATE) { return Err("The drawing has an invalid /Rect.".into()); }
    let rect = CropBox { left: rect[0].min(rect[2]), bottom: rect[1].min(rect[3]), right: rect[0].max(rect[2]), top: rect[1].max(rect[3]) };
    if rect.right <= rect.left || rect.top <= rect.bottom { return Err("The drawing has an empty /Rect.".into()); }
    let strokes = read_ink_list(document, annotation.get(b"InkList").map_err(|_| "The drawing has no /InkList.".to_owned())?)?;
    let color = match annotation.get(b"C") { Ok(value) => Some(numbers(document, value)?), Err(_) => None };
    if color.as_ref().is_some_and(|color| !matches!(color.len(), 0 | 1 | 3 | 4) || color.iter().any(|value| !(0.0..=1.0).contains(value))) { return Err("The drawing has an out-of-range /C.".into()); }
    let width = match border_style(document, annotation)?.and_then(|style| style.get(b"W").ok().cloned()) { Some(value) => number(document, &value)?, None => 1.0 };
    if !(0.0..=MAX_INK_WIDTH).contains(&width) { return Err("The drawing has an out-of-range /BS /W.".into()); }
    let opacity = match annotation.get(b"CA") { Ok(value) => number(document, value)?, Err(_) => 1.0 };
    if !(0.0..=1.0).contains(&opacity) { return Err("The drawing has an out-of-range /CA.".into()); }
    Ok(ForeignInk { strokes, rect, color, width, opacity })
}

fn date(now: &str) -> Result<Object, String> {
    if !valid_pdf_date(now) { return Err("The annotation has an invalid date.".into()); }
    Ok(Object::String(now.as_bytes().to_vec(), StringFormat::Literal))
}

/// After a patch, every key outside `allowed` must be exactly as before, and none may have appeared or gone.
fn only_allowed_changed(before: &Dictionary, after: &Dictionary, allowed: &[&[u8]]) -> Result<(), String> {
    let outside = |dictionary: &Dictionary| dictionary.iter().filter(|(key, _)| !allowed.contains(&key.as_slice())).map(|(key, value)| (key.clone(), format!("{value:?}"))).collect::<std::collections::BTreeMap<_, _>>();
    if outside(before) != outside(after) { return Err("A drawing patch changed a key outside its allow-list. Nothing was changed.".into()); }
    Ok(())
}

fn finish(document: &mut Document, id: ObjectId, before: &Dictionary, after: Dictionary, allowed: &[&[u8]]) -> Result<(), String> {
    only_allowed_changed(before, &after, allowed)?;
    document.objects.insert(id, Object::Dictionary(after));
    Ok(())
}

fn annotation(document: &Document, id: ObjectId) -> Result<Dictionary, String> {
    document.get_object(id).ok().and_then(|value| value.as_dict().ok().cloned()).ok_or_else(|| "The drawing is not an annotation dictionary.".to_owned())
}

/// Moves another app's drawing by (`dx`, `dy`) points: `/Rect` and `/InkList` shift by the same finite delta,
/// `/AP` stays byte-identical, `/M` becomes `now`. Refused when `/Path` is present or the result leaves 1e7.
pub(crate) fn move_foreign_ink(document: &mut Document, id: ObjectId, dx: f32, dy: f32, now: &str) -> Result<(), String> {
    let before = annotation(document, id)?;
    let ink = read_foreign_ink(document, &before)?;
    if before.has(b"Path") { return Err("This drawing uses /Path, which the app does not move.".into()); }
    if !(dx.is_finite() && dy.is_finite()) { return Err("The move distance is invalid.".into()); }
    let shift = |x: f32, y: f32| (x + dx, y + dy);
    let strokes = ink.strokes.iter().map(|stroke| stroke.iter().map(|&(x, y)| shift(x, y)).collect::<Vec<_>>()).collect::<Vec<_>>();
    crate::annotation_model::validate_strokes(&strokes).map_err(|_| "The moved drawing would leave the allowed coordinate range.".to_owned())?;
    let (left, bottom) = shift(ink.rect.left, ink.rect.bottom);
    let (right, top) = shift(ink.rect.right, ink.rect.top);
    if [left, bottom, right, top].iter().any(|value| !value.is_finite() || value.abs() > MAX_INK_COORDINATE) { return Err("The moved drawing would leave the allowed coordinate range.".into()); }
    let mut after = before.clone();
    after.set("Rect", vec![Object::Real(left), Object::Real(bottom), Object::Real(right), Object::Real(top)]);
    after.set("InkList", ink_list(&strokes));
    after.set("M", date(now)?);
    finish(document, id, &before, after, MOVE_KEYS)
}

/// Keys that change how a drawing looks in ways the app's generator does not draw (SM7 condition 2): a colour or
/// width edit is refused when any is present.
const UNDRAWN_KEYS: &[&[u8]] = &[b"BE", b"Path", b"BM", b"OC"];

/// Changes the colour and/or width of another app's drawing (SM7 ruling (a)). `/AP` becomes exactly
/// `<< /N new >>`, a new stream generated from the typed `/InkList`, colour, width and `/CA`, with the drawing's
/// `/Rect` as its BBox; the old appearance objects are left as they are. Refused when a key in `UNDRAWN_KEYS` is
/// present, the border style is not solid, `/ca` differs from `/CA`, or the wider stroke would not fit `/Rect`.
pub(crate) fn restyle_foreign_ink(document: &mut Document, id: ObjectId, color: Option<&[f32]>, width: Option<f32>, now: &str) -> Result<(), String> {
    let before = annotation(document, id)?;
    let ink = read_foreign_ink(document, &before)?;
    if color.is_none() && width.is_none() { return Err("Nothing to change.".into()); }
    if let Some(key) = UNDRAWN_KEYS.iter().find(|key| before.has(key)) { return Err(format!("This drawing uses /{}, which the app cannot redraw, so its colour and width stay as they are.", String::from_utf8_lossy(key))); }
    let style = border_style(document, &before)?;
    if style.as_ref().and_then(|style| style.get(b"S").ok()).is_some_and(|value| value.as_name().ok() != Some(b"S")) { return Err("This drawing has a non-solid border style, which the app cannot redraw.".into()); }
    if let Ok(value) = before.get(b"ca") { if (number(document, value)? - ink.opacity).abs() > f32::EPSILON { return Err("This drawing has different stroke and fill opacity, which the app cannot redraw.".into()); } }
    let color = match color { Some(color) => color.to_vec(), None => ink.color.clone().ok_or("This drawing has no colour to redraw it with.")? };
    if !matches!(color.len(), 0 | 1 | 3 | 4) || color.iter().any(|value| !(0.0..=1.0).contains(value)) { return Err("The new colour is out of range.".into()); }
    let width = width.unwrap_or(ink.width);
    if !(width.is_finite() && (0.0..=MAX_INK_WIDTH).contains(&width)) { return Err("The new width is out of range.".into()); }
    let needed = ink_bounds(&ink.strokes, width);
    if needed.left < ink.rect.left || needed.bottom < ink.rect.bottom || needed.right > ink.rect.right || needed.top > ink.rect.top {
        return Err("The wider stroke would not fit the drawing's rectangle, which this edit does not change.".into());
    }
    let (form, content) = ink_appearance(&ink.strokes, width, &color, ink.opacity, ink.rect)?;
    let appearance = document.add_object(Stream::new(form, content));
    let mut after = before.clone();
    after.set("C", color.iter().map(|value| Object::Real(*value)).collect::<Vec<_>>());
    let mut border = style.unwrap_or_else(|| dictionary! { "Type" => "Border", "S" => "S" });
    border.set("W", Object::Real(width));
    after.set("BS", border);
    after.set("AP", dictionary! { "N" => appearance });
    // The appearance state names a state of the old /AP; it goes with it (SM7 condition 3).
    after.remove(b"AS");
    after.set("M", date(now)?);
    finish(document, id, &before, after, STYLE_KEYS)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    const NOW: &str = "D:20261107120000Z";

    /// A page with one foreign Ink in Acrobat's shape, an appearance stream of its own, action and unknown keys,
    /// and appearance states. Returns the document, the annotation id and its appearance stream id.
    pub(crate) fn foreign_ink(extra: impl FnOnce(&mut Dictionary)) -> (Document, ObjectId, ObjectId) {
        let mut document = Document::with_version("1.7");
        let pages = document.new_object_id();
        let stream = document.add_object(Stream::new(dictionary! { "Type" => "XObject", "Subtype" => "Form", "BBox" => vec![0.into(), 0.into(), 100.into(), 60.into()], "Matrix" => vec![1.into(), 0.into(), 0.into(), 1.into(), 0.into(), 0.into()] }, b"0 0 1 RG 3 w 10 10 m 90 50 l S".to_vec()));
        let mut ink = dictionary! { "Type" => "Annot", "Subtype" => "Ink", "Rect" => vec![100.into(), 200.into(), 200.into(), 260.into()],
            "InkList" => vec![Object::Array(vec![110.into(), 210.into(), 150.into(), 240.into(), 190.into(), 250.into()]), Object::Array(vec![120.into(), 220.into()])],
            "BS" => dictionary! { "W" => 3, "S" => "S" }, "C" => vec![0.into(), 0.into(), 1.into()], "CA" => Object::Real(0.8), "F" => 4,
            "NM" => Object::string_literal("foreign-ink-1"), "T" => Object::string_literal("Reviewer"), "M" => Object::string_literal("D:20240101000000+01'00'"),
            "AP" => dictionary! { "N" => stream, "D" => stream }, "AS" => "Off", "A" => dictionary! { "S" => "URI", "URI" => Object::string_literal("https://example.invalid/") },
            "StructParent" => 7, "XAppPrivate" => Object::string_literal("kept") };
        extra(&mut ink);
        let id = document.add_object(ink);
        let page = document.add_object(dictionary! { "Type" => "Page", "Parent" => pages, "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()], "Annots" => vec![id.into()] });
        document.objects.insert(pages, Object::Dictionary(dictionary! { "Type" => "Pages", "Kids" => vec![page.into()], "Count" => 1 }));
        let catalog = document.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages });
        document.trailer.set("Root", catalog);
        (document, id, stream)
    }

    fn get(document: &Document, id: ObjectId) -> Dictionary { document.get_dictionary(id).unwrap().clone() }

    fn references(value: &Object, found: &mut Vec<ObjectId>) {
        match value {
            Object::Reference(id) => found.push(*id),
            Object::Array(values) => values.iter().for_each(|value| references(value, found)),
            Object::Dictionary(values) => values.iter().for_each(|(_, value)| references(value, found)),
            _ => (),
        }
    }

    #[test]
    fn a_move_shifts_rect_and_ink_list_and_keeps_the_appearance_byte_identical() {
        let (mut document, id, stream) = foreign_ink(|_| {});
        let (before, old_stream) = (get(&document, id), document.get_object(stream).unwrap().clone());
        move_foreign_ink(&mut document, id, 5.0, -10.0, NOW).unwrap();
        let after = get(&document, id);
        let ink = read_foreign_ink(&document, &after).unwrap();
        assert_eq!(ink.rect, CropBox { left: 105.0, bottom: 190.0, right: 205.0, top: 250.0 });
        assert_eq!(ink.strokes, vec![vec![(115.0, 200.0), (155.0, 230.0), (195.0, 240.0)], vec![(125.0, 210.0)]]);
        assert_eq!(format!("{:?}", after.get(b"AP").unwrap()), format!("{:?}", before.get(b"AP").unwrap()));
        assert_eq!(format!("{:?}", document.get_object(stream).unwrap()), format!("{old_stream:?}"));
        assert_eq!(after.get(b"M").unwrap().as_str().unwrap(), NOW.as_bytes());
        for key in [b"A".as_slice(), b"T", b"NM", b"StructParent", b"XAppPrivate", b"AS", b"BS", b"C", b"CA"] { assert_eq!(format!("{:?}", after.get(key).unwrap()), format!("{:?}", before.get(key).unwrap()), "{}", String::from_utf8_lossy(key)); }
    }

    #[test]
    fn a_move_refuses_path_non_finite_or_out_of_range_results_and_changes_nothing() {
        let (mut document, id, _) = foreign_ink(|ink| ink.set("Path", vec![Object::Array(vec![1.into(), 2.into()])]));
        let before = get(&document, id);
        assert!(move_foreign_ink(&mut document, id, 1.0, 1.0, NOW).is_err());
        let (mut plain, plain_id, _) = foreign_ink(|_| {});
        for (dx, dy) in [(f32::NAN, 0.0), (0.0, f32::INFINITY), (2.0e7, 0.0)] { assert!(move_foreign_ink(&mut plain, plain_id, dx, dy, NOW).is_err(), "{dx} {dy}"); }
        assert!(move_foreign_ink(&mut plain, plain_id, 1.0, 1.0, "yesterday").is_err());
        assert_eq!(format!("{:?}", get(&document, id)), format!("{before:?}"));
    }

    #[test]
    fn a_style_edit_writes_a_new_fixed_key_appearance_and_leaves_the_old_stream_untouched() {
        let (mut document, id, stream) = foreign_ink(|_| {});
        let (before, old_stream) = (get(&document, id), document.get_object(stream).unwrap().clone());
        restyle_foreign_ink(&mut document, id, Some(&[1.0, 0.0, 0.0]), Some(2.0), NOW).unwrap();
        let after = get(&document, id);
        let ap = after.get(b"AP").unwrap().as_dict().unwrap();
        assert_eq!(ap.iter().map(|(key, _)| key.clone()).collect::<Vec<_>>(), vec![b"N".to_vec()], "AP is exactly << /N new >>: /D, /R and /AS go with the old dictionary");
        let new = ap.get(b"N").unwrap().as_reference().unwrap();
        assert_ne!(new, stream);
        let form = &document.get_object(new).unwrap().as_stream().unwrap().dict;
        let mut keys = form.iter().map(|(key, _)| String::from_utf8_lossy(key).into_owned()).collect::<Vec<_>>(); keys.sort();
        assert_eq!(keys, ["BBox", "Length", "Resources", "Subtype", "Type"], "only the fixed keys SM7 lists");
        assert_eq!(format!("{:?}", document.get_object(stream).unwrap()), format!("{old_stream:?}"), "the old stream is never modified");
        let mut found = Vec::new();
        references(&Object::Dictionary(after.clone()), &mut found);
        assert!(!found.contains(&stream), "the patched annotation no longer refers to the old stream");
        let ink = read_foreign_ink(&document, &after).unwrap();
        assert_eq!((ink.color, ink.width, ink.opacity), (Some(vec![1.0, 0.0, 0.0]), 2.0, 0.8));
        assert!(before.has(b"AS") && !after.has(b"AS"), "/AS names a state of the old appearance and goes with it");
        for key in [b"A".as_slice(), b"T", b"NM", b"StructParent", b"XAppPrivate", b"InkList", b"Rect", b"CA"] { assert_eq!(format!("{:?}", after.get(key).unwrap()), format!("{:?}", before.get(key).unwrap()), "{}", String::from_utf8_lossy(key)); }
    }

    #[test]
    fn every_undrawn_key_refuses_a_style_edit_but_not_a_move() {
        let cases: Vec<(&str, Box<dyn Fn(&mut Dictionary)>)> = vec![
            ("BE", Box::new(|ink: &mut Dictionary| ink.set("BE", dictionary! { "S" => "C", "I" => 1 }))),
            ("Path", Box::new(|ink: &mut Dictionary| ink.set("Path", vec![Object::Array(vec![1.into(), 2.into()])]))),
            ("BM", Box::new(|ink: &mut Dictionary| ink.set("BM", "Multiply"))),
            ("OC", Box::new(|ink: &mut Dictionary| ink.set("OC", dictionary! { "Type" => "OCG" }))),
            ("dashed", Box::new(|ink: &mut Dictionary| ink.set("BS", dictionary! { "W" => 3, "S" => "D", "D" => vec![3.into()] }))),
            ("beveled", Box::new(|ink: &mut Dictionary| ink.set("BS", dictionary! { "W" => 3, "S" => "B" }))),
            ("ca", Box::new(|ink: &mut Dictionary| ink.set("ca", Object::Real(0.3)))),
        ];
        for (label, extra) in cases {
            let (mut document, id, _) = foreign_ink(|ink| extra(ink));
            let before = format!("{:?}", get(&document, id));
            assert!(restyle_foreign_ink(&mut document, id, Some(&[1.0, 0.0, 0.0]), None, NOW).is_err(), "{label}");
            assert_eq!(format!("{:?}", get(&document, id)), before, "{label}: nothing changed");
            if label != "Path" { assert!(move_foreign_ink(&mut document, id, 1.0, 1.0, NOW).is_ok(), "{label}: a move is exempt"); }
        }
        let (mut same, id, _) = foreign_ink(|ink| ink.set("ca", Object::Real(0.8)));
        assert!(restyle_foreign_ink(&mut same, id, None, Some(2.0), NOW).is_ok(), "a /ca equal to /CA is drawn");
    }

    #[test]
    fn out_of_range_values_refuse() {
        let cases: Vec<(&str, Box<dyn Fn(&mut Dictionary)>)> = vec![
            ("C with 2 components", Box::new(|ink: &mut Dictionary| ink.set("C", vec![0.into(), 1.into()]))),
            ("C above 1", Box::new(|ink: &mut Dictionary| ink.set("C", vec![Object::Real(1.5), 0.into(), 0.into()]))),
            ("W negative", Box::new(|ink: &mut Dictionary| ink.set("BS", dictionary! { "W" => -1, "S" => "S" }))),
            ("W over 100", Box::new(|ink: &mut Dictionary| ink.set("BS", dictionary! { "W" => 101, "S" => "S" }))),
            ("CA above 1", Box::new(|ink: &mut Dictionary| ink.set("CA", Object::Real(1.5)))),
            ("odd InkList", Box::new(|ink: &mut Dictionary| ink.set("InkList", vec![Object::Array(vec![1.into(), 2.into(), 3.into()])]))),
            ("empty InkList", Box::new(|ink: &mut Dictionary| ink.set("InkList", Vec::<Object>::new()))),
            ("huge coordinate", Box::new(|ink: &mut Dictionary| ink.set("InkList", vec![Object::Array(vec![Object::Real(3.0e7), 2.into()])]))),
            ("text in InkList", Box::new(|ink: &mut Dictionary| ink.set("InkList", vec![Object::Array(vec![Object::string_literal("x"), 2.into()])]))),
            ("not Ink", Box::new(|ink: &mut Dictionary| ink.set("Subtype", "Square"))),
        ];
        for (label, extra) in cases {
            let (mut document, id, _) = foreign_ink(|ink| extra(ink));
            assert!(restyle_foreign_ink(&mut document, id, Some(&[0.0]), None, NOW).is_err(), "{label}");
            assert!(move_foreign_ink(&mut document, id, 1.0, 0.0, NOW).is_err(), "{label}");
        }
        let (mut document, id, _) = foreign_ink(|_| {});
        for (color, width) in [(Some(vec![0.0, 1.0]), None), (Some(vec![2.0]), None), (None, Some(-1.0)), (None, Some(f32::NAN)), (None, Some(101.0)), (None, Some(30.0))] {
            assert!(restyle_foreign_ink(&mut document, id, color.as_deref(), width, NOW).is_err(), "{color:?} {width:?}");
        }
        let points = (0..131_073).map(|index| Object::Real(index as f32 % 50.0 + 110.0)).collect::<Vec<_>>();
        let (mut large, id, _) = foreign_ink(move |ink| ink.set("InkList", vec![Object::Array(points.iter().cloned().chain(points.iter().cloned()).collect())]));
        assert!(restyle_foreign_ink(&mut large, id, Some(&[0.0]), None, NOW).is_err(), "over 131,072 points");
    }

    #[test]
    fn a_shared_old_appearance_survives_pruning_and_an_unshared_one_does_not() {
        let (mut document, id, stream) = foreign_ink(|_| {});
        let other = document.add_object(dictionary! { "Type" => "XObject", "Subtype" => "Form", "Shared" => stream });
        let catalog = document.trailer.get(b"Root").unwrap().as_reference().unwrap();
        document.get_dictionary_mut(catalog).unwrap().set("XShared", other);
        restyle_foreign_ink(&mut document, id, Some(&[0.5]), None, NOW).unwrap();
        document.prune_objects();
        assert!(document.get_object(stream).is_ok(), "another object still refers to the old stream");
        let (mut alone, alone_id, alone_stream) = foreign_ink(|_| {});
        restyle_foreign_ink(&mut alone, alone_id, Some(&[0.5]), None, NOW).unwrap();
        alone.prune_objects();
        assert!(alone.get_object(alone_stream).is_err(), "nothing refers to it any more, so pruning drops it");
    }

    #[test]
    fn the_allow_list_check_refuses_a_change_outside_it() {
        let mut before = dictionary! { "Rect" => 1, "A" => 2 };
        let mut after = before.clone();
        after.set("Rect", 3);
        assert!(only_allowed_changed(&before, &after, MOVE_KEYS).is_ok());
        after.set("A", 4);
        assert!(only_allowed_changed(&before, &after, MOVE_KEYS).is_err());
        before.set("Popup", 5);
        assert!(only_allowed_changed(&before, &after, MOVE_KEYS).is_err(), "a key that disappears is a change too");
    }

    #[test]
    fn the_app_drawing_builder_writes_acrobat_style_keys_and_no_author() {
        let (mut document, _, _) = foreign_ink(|_| {});
        let page = document.get_pages()[&1];
        let ink = crate::annotation_model::Ink { strokes: vec![vec![(10.0, 10.0), (40.0, 30.0)], vec![(50.0, 50.0)]], width: 2.0, color: [0.0, 0.4, 1.0], opacity: 0.5 };
        let id = crate::annotation_write::new_ink(&mut document, page, "0b4c6a3e-2f1d-4c8b-9a7e-5d3f2e1c0b9a", &ink, Some(NOW)).unwrap();
        let annotation = get(&document, id);
        assert!(!annotation.has(b"T"));
        assert_eq!(annotation.get(b"Subj").unwrap().as_str().unwrap(), b"Pencil");
        let read = read_foreign_ink(&document, &annotation).unwrap();
        assert_eq!((read.strokes, read.width, read.opacity), (ink.strokes.clone(), 2.0, 0.5));
        assert_eq!(read.rect, ink_bounds(&ink.strokes, 2.0));
        for bad in [crate::annotation_model::Ink { strokes: vec![], ..ink.clone() }, crate::annotation_model::Ink { width: 0.0, ..ink.clone() }, crate::annotation_model::Ink { opacity: 0.0, ..ink.clone() }, crate::annotation_model::Ink { color: [1.1, 0.0, 0.0], ..ink.clone() }] {
            assert!(crate::annotation_write::new_ink(&mut document, page, "0b4c6a3e-2f1d-4c8b-9a7e-5d3f2e1c0b9a", &bad, None).is_err());
        }
        assert!(crate::annotation_write::new_ink(&mut document, page, "pdf-workstation-note-1", &ink, None).is_err(), "new drawings get UUIDs");
    }
}
