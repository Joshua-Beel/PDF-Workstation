use super::*;
use lopdf::{EncryptionState, EncryptionVersion, Permissions, SaveOptions};
use std::time::{Duration, Instant};

const PLAIN: &[u8] = include_bytes!("../tests/fixtures/reportlab-plain-fields.pdf");
const MIXED: &[u8] = include_bytes!("../tests/fixtures/reportlab-mixed-fields.pdf");
const RADIO: &[u8] = include_bytes!("../tests/fixtures/reportlab-radio-fields.pdf");
const CHOICE: &[u8] = include_bytes!("../tests/fixtures/reportlab-choice-fields.pdf");

fn save(doc: &mut Document) -> Vec<u8> { let mut bytes = Vec::new(); doc.save_to(&mut bytes).unwrap(); bytes }
fn roots(doc: &Document) -> Vec<ObjectId> { dict(doc, doc.catalog().unwrap().get(b"AcroForm").unwrap()).unwrap().get(b"Fields").unwrap().as_array().unwrap().iter().map(|value| value.as_reference().unwrap()).collect() }
fn form_id(doc: &Document) -> ObjectId { doc.catalog().unwrap().get(b"AcroForm").unwrap().as_reference().unwrap() }
fn tree_id(doc: &Document) -> ObjectId { doc.catalog().unwrap().get(b"Pages").unwrap().as_reference().unwrap() }
fn first_page(doc: &Document) -> ObjectId { *doc.get_pages().values().next().unwrap() }
fn parse(bytes: &[u8], pages: usize) -> Result<FormDocument, String> { FormDocument::parse(&EditSession::new(bytes.to_vec(), pages)) }
fn refuse(bytes: &[u8], pages: usize, case: &str) -> String {
    let source = EditSession::new(bytes.to_vec(), pages);
    let query = FormDocument::query(&source, 1);
    assert_eq!(query.status, "unsupported", "{case} must be refused as a whole");
    assert!(query.fields.is_empty(), "{case} must not list a partial field set");
    assert_eq!(source.source, bytes, "{case} must leave the source untouched");
    query.reason.unwrap()
}
fn mutate(source: &[u8], change: impl FnOnce(&mut Document)) -> Vec<u8> { let mut doc = Document::load_mem(source).unwrap(); change(&mut doc); save(&mut doc) }
fn all_text_patches(form: &FormDocument, value: &str) -> Vec<FieldValue> { form.fields.iter().map(|field| FieldValue::Text { field_id: field.dto.field_id.clone(), value: value.into() }).collect() }
fn decoded_streams(bytes: &[u8]) -> Vec<Vec<u8>> {
    let doc = Document::load_mem(bytes).unwrap();
    doc.objects.values().filter_map(|object| object.as_stream().ok()).map(|stream| stream.decompressed_content().unwrap_or_else(|_| stream.content.clone())).collect()
}
fn contains(haystack: &[u8], needle: &[u8]) -> bool { haystack.windows(needle.len()).any(|window| window == needle) }

/// Copies of the plain fixture's populated "Name" text field: field `i` sits on page `placements[i]` of `pages` pages.
fn text_form(pages: usize, placements: &[usize]) -> Vec<u8> {
    let mut doc = Document::load_mem(PLAIN).unwrap();
    let template = doc.get_dictionary(roots(&doc)[0]).unwrap().clone();
    let original_page = first_page(&doc);
    let page_template = doc.get_dictionary(original_page).unwrap().clone();
    let tree = tree_id(&doc); let form = form_id(&doc);
    let page_ids = (0..pages).map(|_| doc.new_object_id()).collect::<Vec<_>>();
    let mut annots = vec![Vec::new(); pages]; let mut fields = Vec::new();
    for (index, page) in placements.iter().enumerate() {
        let mut field = template.clone(); field.set("T", Object::string_literal(format!("Field {index}"))); field.set("P", page_ids[*page]);
        let id = doc.add_object(field); annots[*page].push(Object::Reference(id)); fields.push(Object::Reference(id));
    }
    for (id, annots) in page_ids.iter().zip(annots) { let mut page = page_template.clone(); page.set("Parent", tree); if annots.is_empty() { page.remove(b"Annots"); } else { page.set("Annots", annots); } doc.objects.insert(*id, Object::Dictionary(page)); }
    doc.objects.remove(&original_page);
    for id in roots(&doc) { doc.objects.remove(&id); }
    let tree_dict = doc.get_dictionary_mut(tree).unwrap(); tree_dict.set("Kids", page_ids.iter().map(|id| Object::Reference(*id)).collect::<Vec<_>>()); tree_dict.set("Count", pages as i64);
    doc.get_dictionary_mut(form).unwrap().set("Fields", fields);
    save(&mut doc)
}
pub(crate) fn grid(pages: usize, per_page: usize) -> Vec<u8> { text_form(pages, &(0..pages * per_page).map(|index| index / per_page).collect::<Vec<_>>()) }

#[test]
fn forms_hardening_scale_fills_the_full_budget_on_any_page_layout_and_refuses_larger_forms_whole() {
    for (pages, per_page) in [(1, 256), (256, 1), (4, 64), (16, 16)] {
        let bytes = grid(pages, per_page); let started = Instant::now();
        let parsed = parse(&bytes, pages).unwrap(); assert_eq!(parsed.fields.len(), 256);
        assert!(parsed.fields.iter().enumerate().all(|(index, field)| field.dto.page == index / per_page && field.dto.value == "Original"));
        let patches = all_text_patches(&parsed, "Filled at scale");
        let (output, fields) = parsed.prepare(&patches).unwrap();
        let elapsed = started.elapsed();
        let reparsed = parse(&output, pages).unwrap();
        assert_eq!(fields.len(), 256); assert!(reparsed.fields.iter().all(|field| field.dto.value == "Filled at scale"));
        assert!(elapsed < Duration::from_secs(20), "{pages}x{per_page} parse+prepare took {elapsed:?}");
    }
    let spread = text_form(4096, &(0..256).map(|index| index * 16).collect::<Vec<_>>());
    let parsed = parse(&spread, 4096).unwrap(); assert_eq!(parsed.fields.len(), 256); assert_eq!(parsed.fields[255].dto.page, 4080);
    for (bytes, pages, case) in [(grid(2, 129), 2, "258 fields over two pages"), (grid(1, 257), 1, "257 widgets on one page"), (grid(16, 256), 16, "4,096 fields"), (text_form(4097, &[0]), 4097, "4,097 pages")] {
        let started = Instant::now(); let reason = refuse(&bytes, pages, case);
        assert!(reason.contains("256") || reason.contains("4,096") || reason.contains("too large"), "{case}: {reason}");
        assert!(started.elapsed() < Duration::from_secs(10), "{case} refusal took {:?}", started.elapsed());
    }
}

#[test]
fn forms_hardening_every_widget_and_annotation_is_accounted_for_or_the_form_is_refused() {
    let two = grid(2, 1);
    let cases: Vec<(&str, Vec<u8>)> = vec![
        ("widget listed by two pages", mutate(&two, |doc| { let pages = doc.get_pages().values().copied().collect::<Vec<_>>(); let all = roots(doc).into_iter().map(Object::Reference).collect::<Vec<_>>(); for page in pages { doc.get_dictionary_mut(page).unwrap().set("Annots", all.clone()); } })),
        ("one indirect Annots array shared by two pages", mutate(&two, |doc| { let pages = doc.get_pages().values().copied().collect::<Vec<_>>(); let shared = doc.add_object(roots(doc).into_iter().map(Object::Reference).collect::<Vec<_>>()); for page in pages { doc.get_dictionary_mut(page).unwrap().set("Annots", shared); } })),
        ("field whose widget is on no page", mutate(&two, |doc| { let page = first_page(doc); doc.get_dictionary_mut(page).unwrap().remove(b"Annots"); })),
        ("widget outside /Fields", mutate(&two, |doc| { let form = form_id(doc); let first = roots(doc)[0]; doc.get_dictionary_mut(form).unwrap().set("Fields", vec![Object::Reference(first)]); })),
        ("non-widget annotation beside the fields", mutate(&two, |doc| { let page = first_page(doc); let note = doc.add_object(dictionary! { "Type" => "Annot", "Subtype" => "Text", "Rect" => vec![0.into(), 0.into(), 20.into(), 20.into()], "Contents" => Object::string_literal("note") }); doc.get_dictionary_mut(page).unwrap().get_mut(b"Annots").unwrap().as_array_mut().unwrap().push(Object::Reference(note)); })),
        ("widget on a page outside the page tree", mutate(&two, |doc| { let tree = tree_id(doc); let pages = doc.get_pages().values().copied().collect::<Vec<_>>(); doc.get_dictionary_mut(tree).unwrap().set("Kids", vec![Object::Reference(pages[0])]); doc.get_dictionary_mut(tree).unwrap().set("Count", 1); })),
        ("widget without /P", mutate(&two, |doc| { let id = roots(doc)[0]; doc.get_dictionary_mut(id).unwrap().remove(b"P"); })),
        ("widget /P naming the other page", mutate(&two, |doc| { let ids = roots(doc); let other = doc.get_dictionary(ids[1]).unwrap().get(b"P").unwrap().clone(); doc.get_dictionary_mut(ids[0]).unwrap().set("P", other); })),
        ("direct (unreferenced) widget in Annots", mutate(&two, |doc| { let page = first_page(doc); let id = roots(doc)[0]; let direct = doc.get_dictionary(id).unwrap().clone(); doc.get_dictionary_mut(page).unwrap().set("Annots", vec![Object::Dictionary(direct)]); })),
    ];
    for (case, bytes) in cases {
        let pages = Document::load_mem(&bytes).unwrap().get_pages().len();
        refuse(&bytes, pages, case);
    }
    let no_form = mutate(PLAIN, |doc| { let root = doc.trailer.get(b"Root").unwrap().as_reference().unwrap(); doc.get_dictionary_mut(root).unwrap().remove(b"AcroForm"); });
    refuse(&no_form, 1, "page widgets without an AcroForm");
    let notes_only = mutate(&no_form, |doc| { let page = first_page(doc); let note = doc.add_object(dictionary! { "Type" => "Annot", "Subtype" => "Text", "Rect" => vec![0.into(), 0.into(), 20.into(), 20.into()] }); doc.get_dictionary_mut(page).unwrap().set("Annots", vec![Object::Reference(note)]); });
    let parsed = parse(&notes_only, 1).unwrap(); assert!(parsed.fields.is_empty());
    assert!(parsed.prepare(&[FieldValue::Text { field_id: "field-1-0".into(), value: "x".into() }]).is_err(), "A document without fields has nothing to fill");
}

#[test]
fn forms_hardening_hierarchies_inheritance_and_reference_cycles_refuse_without_hanging() {
    let started = Instant::now();
    let deep = mutate(PLAIN, |doc| {
        let ids = roots(doc); let form = form_id(doc); let widget = ids[0];
        let mut child = widget; let mut root = widget;
        for level in 0..2000 { let parent = doc.add_object(dictionary! { "FT" => "Tx", "T" => Object::string_literal(format!("L{level}")), "Kids" => vec![Object::Reference(child)] }); doc.get_dictionary_mut(child).unwrap().set("Parent", parent); child = parent; root = parent; }
        doc.get_dictionary_mut(form).unwrap().set("Fields", vec![Object::Reference(root), Object::Reference(ids[1])]);
    });
    refuse(&deep, 1, "2,000-level /Kids chain");
    let cycle = mutate(PLAIN, |doc| { let ids = roots(doc); let (a, b) = (ids[0], ids[1]); doc.get_dictionary_mut(a).unwrap().set("Kids", vec![Object::Reference(b)]); doc.get_dictionary_mut(b).unwrap().set("Kids", vec![Object::Reference(a)]); doc.get_dictionary_mut(a).unwrap().set("Parent", b); doc.get_dictionary_mut(b).unwrap().set("Parent", a); });
    refuse(&cycle, 1, "cyclic /Kids and /Parent");
    let inherited = mutate(PLAIN, |doc| { let ids = roots(doc); let form = form_id(doc); let widget = doc.get_dictionary_mut(ids[0]).unwrap(); let ft = widget.remove(b"FT").unwrap(); let t = widget.remove(b"T").unwrap(); let da = widget.remove(b"DA").unwrap(); let parent = doc.add_object(dictionary! { "FT" => ft, "T" => t, "DA" => da, "Ff" => 0, "Kids" => vec![Object::Reference(ids[0])] }); doc.get_dictionary_mut(ids[0]).unwrap().set("Parent", parent); doc.get_dictionary_mut(form).unwrap().set("Fields", vec![Object::Reference(parent), Object::Reference(ids[1])]); });
    refuse(&inherited, 1, "text field inheriting /FT, /T and /DA from a parent");
    let radio_inherited = mutate(RADIO, |doc| { let parent = roots(doc)[0]; doc.get_dictionary_mut(parent).unwrap().remove(b"FT"); });
    refuse(&radio_inherited, 1, "radio group without its own /FT");
    let radio_nested = mutate(RADIO, |doc| { let parent = roots(doc)[0]; let kid = doc.get_dictionary(parent).unwrap().get(b"Kids").unwrap().as_array().unwrap()[0].as_reference().unwrap(); let grandchild = doc.get_dictionary(kid).unwrap().clone(); let grandchild = doc.add_object(grandchild); doc.get_dictionary_mut(kid).unwrap().set("Kids", vec![Object::Reference(grandchild)]); });
    refuse(&radio_nested, 1, "radio widget with its own /Kids");
    for (case, target) in [("reference cycle in /AP", b"AP".as_slice()), ("reference cycle in /V", b"V"), ("reference cycle in /Rect", b"Rect")] {
        let bytes = mutate(PLAIN, |doc| { let id = roots(doc)[0]; let a = doc.new_object_id(); let b = doc.new_object_id(); doc.objects.insert(a, Object::Reference(b)); doc.objects.insert(b, Object::Reference(a)); doc.get_dictionary_mut(id).unwrap().set(target, a); });
        refuse(&bytes, 1, case);
    }
    assert!(started.elapsed() < Duration::from_secs(20), "hierarchy refusals took {:?}", started.elapsed());
    let form_da = mutate(PLAIN, |doc| { let id = roots(doc)[0]; let form = form_id(doc); let da = doc.get_dictionary_mut(id).unwrap().remove(b"DA").unwrap(); doc.get_dictionary_mut(form).unwrap().set("DA", da); });
    let parsed = parse(&form_da, 1).unwrap(); let patches = all_text_patches(&parsed, "AcroForm DA");
    let (output, _) = parsed.prepare(&patches).unwrap();
    assert!(parse(&output, 1).unwrap().fields.iter().all(|field| field.dto.value == "AcroForm DA"), "The AcroForm-level /DA (the only supported inheritance) fills correctly");
}

#[test]
fn forms_hardening_oversized_options_values_names_and_appearances_refuse_quickly() {
    let started = Instant::now();
    let huge = |count: usize| mutate(CHOICE, |doc| { let id = roots(doc)[0]; doc.get_dictionary_mut(id).unwrap().set("Opt", (0..count).map(|index| Object::string_literal(format!("Option {index}"))).collect::<Vec<_>>()); });
    refuse(&huge(100_000), 1, "100,000-entry /Opt");
    let indirect = mutate(CHOICE, |doc| { let id = roots(doc)[0]; let opts = doc.add_object((0..100_000).map(|index| Object::string_literal(format!("Option {index}"))).collect::<Vec<_>>()); doc.get_dictionary_mut(id).unwrap().set("Opt", opts); });
    refuse(&indirect, 1, "indirect 100,000-entry /Opt");
    let long = "A".repeat(1024 * 1024);
    for (case, key, value) in [("1 MiB /V", b"V".as_slice(), Object::string_literal(long.clone())), ("1 MiB /DV", b"DV", Object::string_literal(long.clone())), ("1 MiB /T", b"T", Object::string_literal(long.clone())), ("4,097-byte /V", b"V", Object::string_literal("a".repeat(4097))), ("1,025-byte /T", b"T", Object::string_literal("a".repeat(1025))), ("/MaxLen 2^40", b"MaxLen", Object::Integer(1 << 40)), ("/MaxLen 0", b"MaxLen", Object::Integer(0)), ("hex UTF-16 non-ASCII /V", b"V", Object::String(vec![0xfe, 0xff, 0x00, 0xe9], lopdf::StringFormat::Hexadecimal))] {
        refuse(&mutate(PLAIN, |doc| { let id = roots(doc)[0]; doc.get_dictionary_mut(id).unwrap().set(key, value); }), 1, case);
    }
    let parsed = parse(PLAIN, 1).unwrap(); let id = parsed.fields[0].dto.field_id.clone();
    assert!(parsed.prepare(&[FieldValue::Text { field_id: id, value: long.clone() }]).is_err(), "A 1 MiB patch value is refused");
    let normal = |doc: &Document| dict(doc, doc.get_dictionary(roots(doc)[0]).unwrap().get(b"AP").unwrap()).unwrap().get(b"N").unwrap().as_reference().unwrap();
    let mut bomb = Stream::new(dictionary! {}, vec![b' '; 64 * 1024 * 1024]); bomb.compress().unwrap();
    let bomb_content = bomb.content.clone();
    let streams: Vec<(&str, Box<dyn Fn(&mut Stream)>)> = vec![
        ("FlateDecode garbage", Box::new(|stream: &mut Stream| { stream.dict.set("Filter", "FlateDecode"); stream.set_content(b"not deflate".to_vec()); })),
        ("64 MiB deflate bomb", Box::new(move |stream: &mut Stream| { stream.dict.set("Filter", "FlateDecode"); stream.set_content(bomb_content.clone()); })),
        ("DCTDecode appearance", Box::new(|stream: &mut Stream| { stream.dict.set("Filter", "DCTDecode"); })),
        ("unparsable content", Box::new(|stream: &mut Stream| { stream.dict.remove(b"Filter"); stream.set_content(b"BT /Helv 10 Tf (unterminated Tj ET".to_vec()); })),
        ("appearance with an extra operator", Box::new(|stream: &mut Stream| { let content = stream.decompressed_content().unwrap(); stream.dict.remove(b"Filter"); let mut content = content; content.extend_from_slice(b"\n0 0 1 1 re f"); stream.set_content(content); })),
    ];
    for (case, change) in streams {
        let bytes = mutate(PLAIN, |doc| { let id = normal(doc); change(doc.get_object_mut(id).unwrap().as_stream_mut().unwrap()); });
        refuse(&bytes, 1, case);
    }
    refuse(&mutate(PLAIN, |doc| { let id = roots(doc)[0]; let ap = doc.add_object(dictionary! { "N" => dictionary! {} }); doc.get_dictionary_mut(id).unwrap().set("AP", ap); }), 1, "non-stream normal appearance");
    refuse(&mutate(PLAIN, |doc| { let id = roots(doc)[0]; let n = normal(doc); doc.get_dictionary_mut(id).unwrap().set("AP", dictionary! { "N" => n, "D" => n }); }), 1, "text field with a down appearance");
    assert!(started.elapsed() < Duration::from_secs(30), "oversized refusals took {:?}", started.elapsed());
}

#[test]
fn forms_hardening_duplicate_names_need_appearances_and_encryption_refuse() {
    let utf16 = mutate(PLAIN, |doc| { let ids = roots(doc); let name = text(doc.get_dictionary(ids[0]).unwrap().get(b"T").unwrap()).unwrap(); let mut encoded = vec![0xfe, 0xff]; for unit in name.encode_utf16() { encoded.extend(unit.to_be_bytes()); } doc.get_dictionary_mut(ids[1]).unwrap().set("T", Object::String(encoded, lopdf::StringFormat::Hexadecimal)); });
    refuse(&utf16, 1, "same name spelled as PDFDocEncoding and UTF-16BE");
    let mixed_name = mutate(MIXED, |doc| { let ids = roots(doc); let name = doc.get_dictionary(ids[1]).unwrap().get(b"T").unwrap().clone(); doc.get_dictionary_mut(ids[0]).unwrap().set("T", name); });
    refuse(&mixed_name, 1, "checkbox and text field sharing a name");
    let radio_name = mutate(RADIO, |doc| { let parent = roots(doc)[0]; let page = first_page(doc); let form = form_id(doc); let mut plain = Document::load_mem(PLAIN).unwrap(); plain.renumber_objects_with(doc.max_id + 1); let text_id = roots(&plain)[0]; let name = doc.get_dictionary(parent).unwrap().get(b"T").unwrap().clone(); let mut field = plain.get_dictionary(text_id).unwrap().clone(); field.set("T", name); field.set("P", page); for (id, object) in plain.objects { if id != text_id { doc.objects.entry(id).or_insert(object); } } doc.max_id = doc.objects.keys().map(|id| id.0).max().unwrap(); let field = doc.add_object(field); doc.get_dictionary_mut(page).unwrap().get_mut(b"Annots").unwrap().as_array_mut().unwrap().push(Object::Reference(field)); doc.get_dictionary_mut(form).unwrap().get_mut(b"Fields").unwrap().as_array_mut().unwrap().push(Object::Reference(field)); });
    refuse(&radio_name, 1, "radio group and text field sharing a name");
    let need = |value: Object| mutate(PLAIN, |doc| { let form = form_id(doc); let value = if matches!(value, Object::Null) { Object::Reference(doc.add_object(true)) } else { value }; doc.get_dictionary_mut(form).unwrap().set("NeedAppearances", value); });
    refuse(&need(Object::Boolean(true)), 1, "/NeedAppearances true");
    refuse(&need(Object::Null), 1, "indirect /NeedAppearances true");
    refuse(&need(Object::Integer(1)), 1, "non-boolean /NeedAppearances");
    assert!(parse(&need(Object::Boolean(false)), 1).is_ok(), "/NeedAppearances false is the supported explicit state");
    for user in ["", "user"] {
        let mut doc = Document::load_mem(PLAIN).unwrap();
        let state = EncryptionState::try_from(EncryptionVersion::V2 { document: &doc, owner_password: "owner", user_password: user, key_length: 128, permissions: Permissions::all() }).unwrap();
        doc.encrypt(&state).unwrap(); let bytes = save(&mut doc);
        let reason = refuse(&bytes, 1, "RC4-encrypted form");
        assert!(reason.contains("Encrypted") || reason.contains("cannot be parsed"), "{reason}");
    }
}

#[test]
fn forms_hardening_object_and_xref_stream_sources_fill_correctly() {
    for source in [PLAIN, MIXED, RADIO, CHOICE] {
        let mut doc = Document::load_mem(source).unwrap(); let mut bytes = Vec::new();
        doc.save_with_options(&mut bytes, SaveOptions::builder().use_object_streams(true).use_xref_streams(true).build()).unwrap();
        assert!(contains(&bytes, b"/ObjStm") && contains(&bytes, b"/XRef"), "fixture really uses object and xref streams");
        let parsed = parse(&bytes, 1).unwrap(); let count = parsed.fields.len(); assert!(count > 0);
        let patches = parsed.fields.iter().map(|field| { let dto = &field.dto; if let Some(choice) = &dto.choice { FieldValue::Choice { field_id: dto.field_id.clone(), option_id: Some(choice.options.iter().find(|option| Some(&option.option_id) != choice.selected_option_id.as_ref()).unwrap().option_id.clone()) } } else if let Some(radio) = &dto.radio { FieldValue::Radio { field_id: dto.field_id.clone(), option_id: Some(radio.options.iter().find(|option| Some(&option.option_id) != radio.selected_option_id.as_ref()).unwrap().option_id.clone()) } } else if let Some(checked) = dto.checked { FieldValue::Checkbox { field_id: dto.field_id.clone(), checked: !checked } } else { FieldValue::Text { field_id: dto.field_id.clone(), value: "Object streams".into() } } }).collect::<Vec<_>>();
        let (output, fields) = parsed.prepare(&patches).unwrap();
        let reparsed = parse(&output, 1).unwrap(); assert_eq!(reparsed.fields.len(), count);
        for (actual, expected) in reparsed.fields.iter().zip(&fields) { assert_eq!(actual.dto.value, expected.value); assert_eq!(actual.dto.checked, expected.checked); }
    }
}

#[test]
fn forms_hardening_objects_the_lenient_loader_would_drop_refuse_the_whole_form() {
    let mut doc = Document::load_mem(PLAIN).unwrap();
    let page = first_page(&doc);
    let direct = doc.get_dictionary(page).unwrap().get(b"Resources").unwrap().clone();
    let direct = match direct { Object::Reference(id) => doc.get_object(id).unwrap().clone(), direct => direct };
    let resources = doc.add_object(direct); doc.get_dictionary_mut(page).unwrap().set("Resources", resources);
    let padding = doc.add_object(Object::string_literal("x".repeat(1_100_000)));
    let catalog = doc.trailer.get(b"Root").unwrap().as_reference().unwrap();
    doc.get_dictionary_mut(catalog).unwrap().set("PieceInfo", dictionary! { "Padding" => dictionary! { "Private" => padding } });
    let before = doc.objects.iter().filter(|(id, object)| id.0 < resources.0 && id.1 == 0 && !matches!(object, Object::Stream(_))).count();
    let mut oversized = Vec::new();
    doc.save_with_options(&mut oversized, SaveOptions::builder().use_object_streams(true).use_xref_streams(true).max_objects_per_stream(before).build()).unwrap();
    assert!(Document::load_mem(&oversized).unwrap().objects.contains_key(&resources), "An unlimited load keeps the page resources");
    let limited = Document::load_mem_with_options(&oversized, LoadOptions::with_max_decompressed_size(MAX_TEXT)).unwrap();
    assert!(!limited.objects.contains_key(&resources), "Fixture premise: the 1 MiB loader limit silently drops the oversized object stream");
    let reason = refuse(&oversized, 1, "object stream beyond the loader limit");
    assert!(reason.contains("could not be loaded"), "{reason}");

    let mut bytes = Document::load_mem(PLAIN).unwrap(); let mut info = Vec::new(); let info_object = bytes.add_object(dictionary! { "Producer" => Object::string_literal("Hardening fixture") }); bytes.trailer.set("Info", info_object); bytes.save_to(&mut info).unwrap();
    let header = format!("\n{} 0 obj", info_object.0).into_bytes(); let at = info.windows(header.len()).position(|window| window == header.as_slice()).unwrap(); info[at + header.len() - 1] = b'k';
    let lenient = Document::load_mem_with_options(&info, LoadOptions::with_max_decompressed_size(MAX_TEXT)).unwrap();
    let info_id = lenient.trailer.get(b"Info").unwrap().as_reference().unwrap();
    assert!(!lenient.objects.contains_key(&info_id), "Fixture premise: the lenient loader silently drops a malformed object");
    let reason = refuse(&info, 1, "malformed object the loader would drop");
    assert!(reason.contains("could not be loaded"), "{reason}");
}

#[test]
fn forms_hardening_replaced_values_leave_no_stale_appearance_in_the_copy() {
    let mut doc = Document::load_mem(PLAIN).unwrap(); let ids = roots(&doc);
    let secret = "Prior secret 123";
    let parsed = parse(PLAIN, 1).unwrap(); let style = parsed.fields[0].style.clone().unwrap();
    let ap = doc.add_object(Stream::new(dictionary! { "Type" => "XObject", "Subtype" => "Form", "BBox" => real(&[0.0, 0.0, style.width, style.height]), "Resources" => dictionary! { "Font" => dictionary! { style.font.as_bytes() => style.font_object.clone() } } }, appearance(&style, secret).encode().unwrap()));
    let field = doc.get_dictionary_mut(ids[0]).unwrap(); field.set("V", Object::string_literal(secret)); field.set("AP", dictionary! { "N" => ap });
    let source = save(&mut doc);
    assert!(decoded_streams(&source).iter().any(|stream| contains(stream, secret.as_bytes())));
    for replacement in ["", "Replacement"] {
        let parsed = parse(&source, 1).unwrap(); let id = parsed.fields[0].dto.field_id.clone();
        let (output, _) = parsed.prepare(&[FieldValue::Text { field_id: id, value: replacement.into() }]).unwrap();
        assert!(!contains(&output, secret.as_bytes()) && decoded_streams(&output).iter().all(|stream| !contains(stream, secret.as_bytes())), "Replacing {secret:?} with {replacement:?} must not leave the old appearance in the copy");
        assert_eq!(parse(&output, 1).unwrap().fields[0].dto.value, replacement);
    }
    let shared = grid(1, 2);
    let parsed = parse(&shared, 1).unwrap(); let first = parsed.fields[0].dto.field_id.clone();
    let shared_ap = dict(&parsed.document, parsed.document.get_dictionary(parsed.fields[1].object).unwrap().get(b"AP").unwrap()).unwrap().get(b"N").unwrap().as_reference().unwrap();
    let (output, _) = parsed.prepare(&[FieldValue::Text { field_id: first, value: "Only the first".into() }]).unwrap();
    let reparsed = parse(&output, 1).unwrap();
    assert_eq!(reparsed.fields[1].dto.value, "Original"); assert!(reparsed.document.objects.contains_key(&shared_ap), "An appearance still used by an unchanged field is kept");
    let choice = parse(CHOICE, 1).unwrap(); let old_ap = dict(&choice.document, choice.document.get_dictionary(choice.fields[0].object).unwrap().get(b"AP").unwrap()).unwrap().get(b"N").unwrap().as_reference().unwrap();
    let patches = choice.fields.iter().map(|field| FieldValue::Choice { field_id: field.dto.field_id.clone(), option_id: None }).collect::<Vec<_>>();
    let (output, _) = choice.prepare(&patches).unwrap();
    assert!(!Document::load_mem(&output).unwrap().objects.contains_key(&old_ap), "A cleared choice drops its replaced selected-value appearance");

    let mut update = lopdf::IncrementalDocument::create_from(source.clone(), Document::load_mem(&source).unwrap());
    let original = Document::load_mem(PLAIN).unwrap().get_dictionary(ids[0]).unwrap().clone();
    update.new_document.objects.insert(ids[0], Object::Dictionary(original));
    let mut incremental = Vec::new(); update.save_to(&mut incremental).unwrap();
    assert!(decoded_streams(&incremental).iter().any(|stream| contains(stream, secret.as_bytes())), "Fixture premise: the superseded revision still carries the old appearance");
    let parsed = parse(&incremental, 1).unwrap(); assert_eq!(parsed.fields[0].dto.value, "Original");
    let id = parsed.fields[1].dto.field_id.clone();
    let (output, _) = parsed.prepare(&[FieldValue::Text { field_id: id, value: "Other field".into() }]).unwrap();
    assert!(!contains(&output, secret.as_bytes()) && decoded_streams(&output).iter().all(|stream| !contains(stream, secret.as_bytes())), "The rewritten copy does not carry a superseded incremental revision");
}

#[test]
#[ignore = "manual measurement probe: cargo test --bins -- --ignored forms_hardening_probe --nocapture"]
fn forms_hardening_probe_measures_scale() {
    for count in [16usize, 32, 64, 128, 256] {
        let bytes = grid(1, count); let mut best = Duration::MAX;
        for _ in 0..3 { let started = Instant::now(); let parsed = parse(&bytes, 1).unwrap(); let patches = all_text_patches(&parsed, "Measured"); parsed.prepare(&patches).unwrap(); best = best.min(started.elapsed()); }
        eprintln!("PROBE fields={count} source_bytes={} parse+prepare_best_of_3={best:?} per_field={:?}", bytes.len(), best / count as u32);
    }
    for pages in [512usize, 1024, 2048, 4096] {
        let bytes = text_form(pages, &[0]); let started = Instant::now(); parse(&bytes, pages).unwrap();
        eprintln!("PROBE pages={pages} source_bytes={} parse={:?}", bytes.len(), started.elapsed());
    }
}

fn append_update(base: &[u8], objects: &[(u32, Vec<u8>)]) -> Vec<u8> {
    let doc = Document::load_mem(base).unwrap();
    let text = String::from_utf8_lossy(base); let prev: usize = text[text.rfind("startxref").unwrap() + 9..].split_whitespace().next().unwrap().parse().unwrap();
    let mut out = base.to_vec(); out.push(b'\n'); let mut offsets = Vec::new();
    for (id, body) in objects { offsets.push((*id, out.len())); out.extend(format!("{id} 0 obj\n").as_bytes()); out.extend(body); out.extend(b"\nendobj\n"); }
    let xref = out.len(); out.extend(b"xref\n");
    for (id, offset) in &offsets { out.extend(format!("{id} 1\n{offset:010} 00000 n \n").as_bytes()); }
    let size = offsets.iter().map(|(id, _)| id + 1).max().unwrap().max(doc.max_id + 1); let root = doc.trailer.get(b"Root").unwrap().as_reference().unwrap();
    out.extend(format!("trailer\n<< /Size {size} /Root {} {} R /Prev {prev} >>\nstartxref\n{xref}\n%%EOF\n", root.0, root.1).as_bytes());
    out
}

#[test]
#[ignore = "manual measurement probe: set SM3_PROBE_DIR, then cargo test --bins -- --ignored forms_hardening_probe_writes --nocapture"]
fn forms_hardening_probe_writes_load_fixtures() {
    let folder = std::path::PathBuf::from(std::env::var("SM3_PROBE_DIR").unwrap());
    for streams in [64usize, 256, 1024] {
        let base = Document::load_mem(PLAIN).unwrap(); let first = base.max_id + 1;
        let objects = (0..streams).map(|index| { let mut content = format!("{:<16}", format!("{} 0", 800_000 + index)).into_bytes(); content.extend(b"null"); content.extend(std::iter::repeat_n(b' ', 1_000_000)); let mut stream = Stream::new(dictionary! {}, content); stream.compress().unwrap(); let mut body = format!("<< /Type /ObjStm /N 1 /First 16 /Filter /FlateDecode /Length {} >>\nstream\n", stream.content.len()).into_bytes(); body.extend(&stream.content); body.extend(b"\nendstream"); (first + index as u32, body) }).collect::<Vec<_>>();
        let bytes = append_update(PLAIN, &objects);
        let started = Instant::now(); let loaded = Document::load_mem_with_options(&bytes, LoadOptions::with_max_decompressed_size(MAX_TEXT)).unwrap();
        eprintln!("PROBE object_streams={streams} source_bytes={} decompressed_bytes~{} lopdf_load={:?} objects={}", bytes.len(), streams * 1_000_020, started.elapsed(), loaded.objects.len());
        std::fs::write(folder.join(format!("objstm-{streams}.pdf")), bytes).unwrap();
    }
    for objects in [50_001usize, 800_000] {
        let mut doc = Document::load_mem(PLAIN).unwrap(); for index in 0..objects { doc.add_object(Object::Integer(index as i64)); }
        std::fs::write(folder.join(format!("objects-{objects}.pdf")), save(&mut doc)).unwrap();
    }
}

#[test]
#[ignore = "manual measurement probe: set SM3_PROBE_FILE, then cargo test --bins -- --ignored forms_hardening_probe_parses --nocapture"]
fn forms_hardening_probe_parses_one_file() {
    let path = std::env::var("SM3_PROBE_FILE").unwrap(); let bytes = std::fs::read(&path).unwrap();
    let started = Instant::now(); let result = parse(&bytes, 1).map(|form| form.fields.len());
    eprintln!("PROBE file={path} source_bytes={} result={result:?} elapsed={:?}", bytes.len(), started.elapsed());
}
