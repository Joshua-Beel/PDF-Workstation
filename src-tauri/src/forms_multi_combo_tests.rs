use super::*;
const CHOICE: &[u8] = include_bytes!("../tests/fixtures/reportlab-choice-fields.pdf");
fn session(bytes: Vec<u8>) -> EditSession { EditSession::new(bytes, 1) }
fn labels() -> Vec<String> { (0..10).map(|n| format!("Option {n}")).collect() }
fn flags(doc: &Document, id: ObjectId) -> i64 { doc.get_dictionary(id).unwrap().get(b"Ff").ok().and_then(|f| f.as_i64().ok()).unwrap_or(0) }
fn choice_ids(doc: &Document) -> (ObjectId, ObjectId) {
    let mut list = None; let mut dropdown = None;
    for (id, object) in &doc.objects { if let Ok(field) = object.as_dict() { if field.has(b"Opt") { if flags(doc, *id) & 131072 != 0 { dropdown = Some(*id); } else { list = Some(*id); } } } }
    (list.unwrap(), dropdown.unwrap())
}
fn rebuild(doc: &mut Document, id: ObjectId, info: &ChoiceInfo, top: usize) {
    let form = doc.catalog().unwrap().get(b"AcroForm").unwrap().as_reference().unwrap(); let form = doc.get_dictionary(form).unwrap().clone();
    let mut st = style(doc, &form, doc.get_dictionary(id).unwrap()).unwrap(); st.top = top;
    let content = choice_appearance(&st, info).unwrap().encode().unwrap();
    let normal = doc.get_dictionary(id).unwrap().get(b"AP").unwrap().as_dict().unwrap().get(b"N").unwrap().as_reference().unwrap();
    let stream = doc.get_object_mut(normal).unwrap().as_stream_mut().unwrap(); stream.dict.remove(b"Filter"); stream.set_content(content);
    stream.dict.set("BBox", vec![Object::Real(0.0), Object::Real(0.0), Object::Real(st.width), Object::Real(st.height)]);
}
fn options(labels: &[String]) -> Vec<ChoiceOption> { labels.iter().enumerate().map(|(n, label)| ChoiceOption { option_id: format!("o{n}"), label: label.clone(), export: label.clone() }).collect() }
fn size_list(doc: &mut Document, id: ObjectId) {
    let form = doc.catalog().unwrap().get(b"AcroForm").unwrap().as_reference().unwrap(); let form = doc.get_dictionary(form).unwrap().clone();
    let rect = doc.get_dictionary(id).unwrap().get(b"Rect").unwrap().as_array().unwrap().iter().map(|v| v.as_float().unwrap()).collect::<Vec<_>>();
    doc.get_dictionary_mut(id).unwrap().set("Rect", vec![Object::Real(rect[0]), Object::Real(rect[1]), Object::Real(rect[0] + 200.0), Object::Real(rect[1] + 100.0)]);
    let trial = style(doc, &form, doc.get_dictionary(id).unwrap()).unwrap();
    doc.get_dictionary_mut(id).unwrap().set("Rect", vec![Object::Real(rect[0]), Object::Real(rect[1]), Object::Real(rect[0] + 200.0), Object::Real(rect[1] + 4.0 * trial.border + 5.4 * trial.size)]);
}
pub(crate) fn multi_source(selected: &[usize]) -> (Vec<u8>, ObjectId) {
    let mut doc = Document::load_mem(CHOICE).unwrap(); let (id, _) = choice_ids(&doc); let labels = labels();
    let field = doc.get_dictionary_mut(id).unwrap();
    field.set("Opt", labels.iter().map(|label| Object::string_literal(label.as_bytes().to_vec())).collect::<Vec<_>>()); field.set("Ff", 2097152i64); field.remove(b"DV"); field.remove(b"TI");
    match selected.len() { 0 => { field.remove(b"V"); field.remove(b"I"); }, 1 => { field.set("V", Object::string_literal(labels[selected[0]].as_bytes().to_vec())); field.set("I", vec![Object::Integer(selected[0] as i64)]); }, _ => { field.set("V", selected.iter().map(|i| Object::string_literal(labels[*i].as_bytes().to_vec())).collect::<Vec<_>>()); field.set("I", selected.iter().map(|i| Object::Integer(*i as i64)).collect::<Vec<_>>()); } }
    size_list(&mut doc, id);
    let info = ChoiceInfo { presentation: "list", options: options(&labels), selected_option_id: None, multiple: true, editable: false, selected_option_ids: selected.iter().map(|i| format!("o{i}")).collect(), text: String::new() };
    rebuild(&mut doc, id, &info, 0);
    let mut bytes = Vec::new(); doc.save_to(&mut bytes).unwrap(); (bytes, id)
}
pub(crate) fn combo_source(custom: &str) -> (Vec<u8>, ObjectId, Vec<ChoiceOption>) {
    let mut doc = Document::load_mem(CHOICE).unwrap(); let (_, id) = choice_ids(&doc);
    let parsed = FormDocument::parse(&session(CHOICE.to_vec())).unwrap();
    let source = parsed.fields.iter().find(|field| field.object == id).unwrap().dto.choice.clone().unwrap();
    let flag = flags(&doc, id) | 262144; let field = doc.get_dictionary_mut(id).unwrap();
    field.set("Ff", flag); field.remove(b"I"); if custom.is_empty() { field.remove(b"V"); } else { field.set("V", pdf_text(custom)); }
    let info = ChoiceInfo { presentation: "dropdown", options: source.options.clone(), selected_option_id: None, multiple: false, editable: true, selected_option_ids: Vec::new(), text: custom.to_string() };
    rebuild(&mut doc, id, &info, 0);
    let mut bytes = Vec::new(); doc.save_to(&mut bytes).unwrap(); (bytes, id, source.options)
}
fn field_of<'a>(parsed: &'a FormDocument, id: ObjectId) -> &'a ParsedField { parsed.fields.iter().find(|field| field.object == id).unwrap() }
fn fill(source: &EditSession, patch: FieldValue) -> Result<Vec<u8>, String> { FormDocument::parse(source)?.prepare(&[patch]).map(|(bytes, _)| bytes) }
fn read(bytes: &[u8], id: ObjectId, key: &[u8]) -> Option<Object> { Document::load_mem(bytes).unwrap().get_dictionary(id).unwrap().get(key).ok().cloned() }

#[test]
fn a_multi_select_list_parses_fills_clears_and_round_trips() {
    let (bytes, id) = multi_source(&[1, 3]); let source = session(bytes);
    let parsed = FormDocument::parse(&source).unwrap(); let choice = field_of(&parsed, id).dto.choice.as_ref().unwrap();
    assert!(choice.multiple && !choice.editable); let ids: Vec<String> = choice.options.iter().map(|option| option.option_id.clone()).collect(); assert_eq!(choice.selected_option_ids, [ids[1].clone(), ids[3].clone()]); assert_eq!(field_of(&parsed, id).dto.value, "Option 1\nOption 3");
    let field_id = field_of(&parsed, id).dto.field_id.clone();
    let choose = |picks: &[usize]| FieldValue::Choices { field_id: field_id.clone(), option_ids: picks.iter().map(|pick| if *pick < 10 { ids[*pick].clone() } else { "missing".to_string() }).collect() };
    let three = fill(&source, choose(&[7, 0, 3])).unwrap();
    let again = FormDocument::parse(&session(three.clone())).unwrap(); assert_eq!(field_of(&again, id).dto.choice.as_ref().unwrap().selected_option_ids, [ids[0].clone(), ids[3].clone(), ids[7].clone()], "kept in option order");
    assert_eq!(read(&three, id, b"I").unwrap().as_array().unwrap().iter().map(|v| v.as_i64().unwrap()).collect::<Vec<_>>(), [0, 3, 7]);
    assert_eq!(read(&three, id, b"V").unwrap().as_array().unwrap().len(), 3);
    let one = fill(&source, choose(&[3])).unwrap(); assert_eq!(read(&one, id, b"V").unwrap().as_str().unwrap(), b"Option 3"); assert_eq!(read(&one, id, b"I").unwrap().as_array().unwrap().len(), 1);
    assert!(FormDocument::parse(&session(one)).is_ok());
    let none = fill(&source, choose(&[])).unwrap(); assert!(read(&none, id, b"V").is_none() && read(&none, id, b"I").is_none());
    assert!(field_of(&FormDocument::parse(&session(none)).unwrap(), id).dto.choice.as_ref().unwrap().selected_option_ids.is_empty());
    for bad in [choose(&[99]), choose(&[1, 1]), FieldValue::Choice { field_id: field_id.clone(), option_id: Some(ids[1].clone()) }, FieldValue::Choice { field_id: field_id.clone(), option_id: None }, FieldValue::Combo { field_id: field_id.clone(), text: "x".into() }] { assert!(fill(&source, bad).is_err()); }
    assert!(fill(&source, choose(&[1, 3])).is_err(), "no change is refused");
}

#[test]
fn a_multi_select_source_with_a_wrong_index_array_or_value_is_refused() {
    let (bytes, id) = multi_source(&[1, 3]);
    let mut doc = Document::load_mem(&bytes).unwrap(); doc.get_dictionary_mut(id).unwrap().set("I", vec![Object::Integer(3), Object::Integer(1)]); let mut bad = Vec::new(); doc.save_to(&mut bad).unwrap();
    assert_eq!(FormDocument::query(&session(bad), 1).status, "unsupported", "indices must be ascending and match the values");
    let mut doc = Document::load_mem(&bytes).unwrap(); doc.get_dictionary_mut(id).unwrap().set("V", Object::string_literal(b"Option 9".to_vec())); let mut bad = Vec::new(); doc.save_to(&mut bad).unwrap();
    assert_eq!(FormDocument::query(&session(bad), 1).status, "unsupported");
    let (blank, id) = multi_source(&[]); assert!(field_of(&FormDocument::parse(&session(blank)).unwrap(), id).dto.choice.as_ref().unwrap().selected_option_ids.is_empty());
}

#[test]
fn an_editable_combo_keeps_typed_text_and_selects_listed_options() {
    let (bytes, id, options) = combo_source("Typed \u{e9} text"); let source = session(bytes);
    let parsed = FormDocument::parse(&source).unwrap(); let field = field_of(&parsed, id); let choice = field.dto.choice.as_ref().unwrap();
    assert!(choice.editable && !choice.multiple && choice.selected_option_id.is_none()); assert_eq!(choice.text, "Typed \u{e9} text"); assert_eq!(field.dto.value, "Typed \u{e9} text");
    let field_id = field.dto.field_id.clone(); let combo = |text: &str| FieldValue::Combo { field_id: field_id.clone(), text: text.into() };
    let typed = fill(&source, combo("Brand new")).unwrap(); assert_eq!(read(&typed, id, b"V").unwrap().as_str().unwrap(), b"Brand new"); assert!(read(&typed, id, b"I").is_none());
    let reparsed = FormDocument::parse(&session(typed)).unwrap(); assert_eq!(field_of(&reparsed, id).dto.choice.as_ref().unwrap().text, "Brand new");
    let accent = fill(&source, combo("Caf\u{e9} \u{f1}")).unwrap(); assert_eq!(field_of(&FormDocument::parse(&session(accent)).unwrap(), id).dto.value, "Caf\u{e9} \u{f1}");
    let by_label = fill(&source, combo(&options[1].label)).unwrap(); let by_label = FormDocument::parse(&session(by_label)).unwrap(); let chosen = field_of(&by_label, id).dto.choice.as_ref().unwrap();
    assert_eq!(chosen.selected_option_id.as_ref().unwrap(), &options[1].option_id); assert!(chosen.text.is_empty(), "typing a listed label picks that option");
    let picked = fill(&source, FieldValue::Choice { field_id: field_id.clone(), option_id: Some(options[0].option_id.clone()) }).unwrap(); assert!(field_of(&FormDocument::parse(&session(picked)).unwrap(), id).dto.choice.as_ref().unwrap().text.is_empty());
    let cleared = fill(&source, combo("")).unwrap(); assert!(read(&cleared, id, b"V").is_none()); assert!(FormDocument::parse(&session(cleared)).is_ok());
    for bad in ["\u{4e16}\u{754c}", "tab\there", &"W".repeat(60)] { assert!(fill(&source, combo(bad)).is_err(), "{bad:?}"); }
    assert!(fill(&source, FieldValue::Choices { field_id: field_id.clone(), option_ids: vec![] }).is_err());
}

#[test]
fn a_noneditable_dropdown_refuses_typed_text_and_a_blank_editable_combo_parses() {
    let parsed = FormDocument::parse(&session(CHOICE.to_vec())).unwrap();
    let dropdown = parsed.fields.iter().find(|field| field.dto.choice.as_ref().is_some_and(|choice| choice.presentation == "dropdown")).unwrap();
    assert!(FormDocument::parse(&session(CHOICE.to_vec())).unwrap().prepare(&[FieldValue::Combo { field_id: dropdown.dto.field_id.clone(), text: "x".into() }]).is_err());
    let (bytes, id, _) = combo_source(""); assert!(field_of(&FormDocument::parse(&session(bytes)).unwrap(), id).dto.choice.as_ref().unwrap().editable);
}

#[test]
fn unsupported_choice_flag_combinations_are_still_refused() {
    for extra in [524288i64, 33554432, 4194304, 1] {
        let mut doc = Document::load_mem(CHOICE).unwrap(); let (_, id) = choice_ids(&doc); let flag = flags(&doc, id) | extra; doc.get_dictionary_mut(id).unwrap().set("Ff", flag);
        let mut bytes = Vec::new(); doc.save_to(&mut bytes).unwrap(); assert_eq!(FormDocument::query(&session(bytes), 1).status, "unsupported", "{extra}");
    }
    let mut doc = Document::load_mem(CHOICE).unwrap(); let (id, _) = choice_ids(&doc); doc.get_dictionary_mut(id).unwrap().set("Ff", 2097152i64 | 131072);
    let mut bytes = Vec::new(); doc.save_to(&mut bytes).unwrap(); assert_eq!(FormDocument::query(&session(bytes), 1).status, "unsupported", "multi-select combo");
}

#[test]
fn typed_combo_text_matches_labels_only_never_another_options_export() {
    let (bytes, id, options) = combo_source(""); let (label0, label1) = (options[0].label.clone(), options[1].label.clone());
    let mut doc = Document::load_mem(&bytes).unwrap();
    doc.get_dictionary_mut(id).unwrap().get_mut(b"Opt").unwrap().as_array_mut().unwrap()[0] = Object::Array(vec![Object::string_literal(label1.clone().into_bytes()), Object::string_literal(label0.clone().into_bytes())]);
    let mut crafted = Vec::new(); doc.save_to(&mut crafted).unwrap(); let source = session(crafted);
    let parsed = FormDocument::parse(&source).unwrap(); let field_id = field_of(&parsed, id).dto.field_id.clone();
    let filled = fill(&source, FieldValue::Combo { field_id: field_id.clone(), text: label1.clone() }).unwrap();
    let again = FormDocument::parse(&session(filled.clone())).unwrap(); let choice = field_of(&again, id).dto.choice.as_ref().unwrap();
    assert_eq!(choice.options.iter().find(|option| Some(&option.option_id) == choice.selected_option_id.as_ref()).unwrap().label, label1, "typing a label selects the option with that label, not the option whose export equals it");
    assert_eq!(read(&filled, id, b"V").unwrap().as_str().unwrap(), options[1].export.as_bytes(), "the stored value is option 1's own export");
    assert!(fill(&source, FieldValue::Combo { field_id, text: options[1].export.clone() }).unwrap_err().contains("stored value"), "text equal to an export but to no label is refused rather than guessed");
}
