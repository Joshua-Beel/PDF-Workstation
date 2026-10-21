use super::*;
const FIXTURE: &[u8] = include_bytes!("../tests/fixtures/reportlab-plain-fields.pdf");
fn session(bytes: Vec<u8>) -> EditSession { EditSession::new(bytes, 1) }
fn first_id(session: &EditSession) -> String { FormDocument::parse(session).unwrap().fields[0].dto.field_id.clone() }
fn fill(source: &EditSession, value: &str) -> Result<Vec<u8>, String> { let id = first_id(source); FormDocument::parse(source)?.prepare(&[FieldValue::Text { field_id: id, value: value.into() }]).map(|(bytes, _)| bytes) }

#[test]
fn latin1_text_round_trips_with_winansi_appearance_bytes_and_utf16_value() {
    let source = session(FIXTURE.to_vec()); let bytes = fill(&source, "Café ñ ©").unwrap();
    let output = session(bytes.clone()); let parsed = FormDocument::parse(&output).unwrap();
    assert_eq!(parsed.fields[0].dto.value, "Café ñ ©", "the filled copy parses again with the same value");
    let document = Document::load_mem(&bytes).unwrap(); let field = document.get_dictionary(parsed.fields[0].object).unwrap();
    let value = field.get(b"V").unwrap().as_str().unwrap(); assert_eq!(&value[..2], &[0xfe, 0xff], "non-ASCII values are stored as UTF-16BE text strings");
    let stream = document.get_object(field.get(b"AP").unwrap().as_dict().unwrap().get(b"N").unwrap().as_reference().unwrap()).unwrap().as_stream().unwrap();
    let content = Content::decode(&stream.decompressed_content().unwrap()).unwrap();
    let shown = content.operations.iter().find(|operation| operation.operator == "Tj").unwrap().operands[0].as_str().unwrap().to_vec();
    assert_eq!(shown, b"Caf\xe9 \xf1 \xa9", "the appearance uses WinAnsi bytes");
    assert_eq!(fill(&source, "plain").map(|bytes| Document::load_mem(&bytes).unwrap().get_dictionary(parsed.fields[0].object).unwrap().get(b"V").unwrap().as_str().unwrap().to_vec()).unwrap(), b"plain", "ASCII values stay byte strings");
}

#[test]
fn text_beyond_latin1_and_control_characters_are_refused() {
    let source = session(FIXTURE.to_vec());
    for value in ["世界", "€", "tab\there", "line\nbreak", "soft\u{ad}hyphen", "no\u{a0}break", "\u{85}", "emoji 😀"] { assert!(fill(&source, value).is_err(), "{value:?}"); }
    assert!(fill(&source, &"é".repeat(40)).is_err(), "the width check uses the Latin-1 metrics");
}

#[test]
fn non_ascii_is_refused_when_the_font_is_not_plain_winansi() {
    let mut document = Document::load_mem(FIXTURE).unwrap();
    for object in document.objects.values_mut() { if let Object::Dictionary(font) = object { if font.get(b"BaseFont").is_ok() { font.set("Encoding", dictionary! { "Type" => "Encoding", "BaseEncoding" => "StandardEncoding", "Differences" => vec![Object::Integer(39), Object::Name(b"quotesingle".to_vec()), Object::Integer(96), Object::Name(b"grave".to_vec())] }); } } }
    let mut bytes = Vec::new(); document.save_to(&mut bytes).unwrap(); let source = session(bytes);
    assert!(fill(&source, "plain").is_ok());
    assert!(fill(&source, "é").unwrap_err().contains("beyond ASCII"));
}

#[test]
fn latin1_width_table_matches_the_published_helvetica_metrics_glyph_by_glyph() {
    const EXPECTED: &str = "exclamdown=333 cent=556 sterling=556 currency=556 yen=556 brokenbar=260 section=556 dieresis=333 copyright=737 ordfeminine=370 guillemotleft=556 logicalnot=584 hyphen=333 registered=737 macron=333 degree=400 plusminus=584 twosuperior=333 threesuperior=333 acute=333 mu=556 paragraph=537 periodcentered=278 cedilla=333 onesuperior=333 ordmasculine=365 guillemotright=556 onequarter=834 onehalf=834 threequarters=834 questiondown=611 Agrave=667 Aacute=667 Acircumflex=667 Atilde=667 Adieresis=667 Aring=667 AE=1000 Ccedilla=722 Egrave=667 Eacute=667 Ecircumflex=667 Edieresis=667 Igrave=278 Iacute=278 Icircumflex=278 Idieresis=278 Eth=722 Ntilde=722 Ograve=778 Oacute=778 Ocircumflex=778 Otilde=778 Odieresis=778 multiply=584 Oslash=778 Ugrave=722 Uacute=722 Ucircumflex=722 Udieresis=722 Yacute=667 Thorn=667 germandbls=611 agrave=556 aacute=556 acircumflex=556 atilde=556 adieresis=556 aring=556 ae=889 ccedilla=500 egrave=556 eacute=556 ecircumflex=556 edieresis=556 igrave=278 iacute=278 icircumflex=278 idieresis=278 eth=556 ntilde=556 ograve=556 oacute=556 ocircumflex=556 otilde=556 odieresis=556 divide=584 oslash=611 ugrave=556 uacute=556 ucircumflex=556 udieresis=556 yacute=500 thorn=556 ydieresis=500";
    assert_eq!(LATIN1_WIDTHS.len(), 96); assert_eq!(LATIN1_NAMES.len(), 95); assert_eq!(WIDTHS.len(), 95);
    let expected: Vec<(&str, u16)> = EXPECTED.split(' ').map(|pair| { let (name, width) = pair.split_once('=').unwrap(); (name, width.parse().unwrap()) }).collect();
    assert_eq!(expected.len(), 95);
    for (index, (name, width)) in expected.iter().enumerate() { assert_eq!(LATIN1_NAMES[index], *name, "glyph name at code {}", 161 + index); assert_eq!(LATIN1_WIDTHS[index + 1], *width, "{name}"); }
    assert_eq!(LATIN1_WIDTHS[0], 278, "the table starts at the no-break space slot, which is never used");
    for (character, width) in [('A', 667.0), ('i', 222.0), (' ', 278.0), ('~', 584.0), ('é', 556.0)] { assert_eq!(advance(character), width, "{character}"); }
}

const CHOICE: &[u8] = include_bytes!("../tests/fixtures/reportlab-choice-fields.pdf");
fn scrolling_list(top: usize) -> (Vec<u8>, ObjectId) {
    let mut doc = Document::load_mem(CHOICE).unwrap();
    let id = *doc.objects.iter().find(|(_, object)| object.as_dict().ok().is_some_and(|d| d.has(b"Opt") && matches!(d.get(b"Ff").ok().and_then(|f| f.as_i64().ok()).unwrap_or(0), 0 | 2))).unwrap().0;
    let form = doc.catalog().unwrap().get(b"AcroForm").unwrap().as_reference().unwrap(); let form = doc.get_dictionary(form).unwrap().clone();
    let rect = doc.get_dictionary(id).unwrap().get(b"Rect").unwrap().as_array().unwrap().iter().map(|v| v.as_float().unwrap()).collect::<Vec<_>>();
    let labels: Vec<String> = (0..10).map(|n| format!("Option {n}")).collect();
    let field = doc.get_dictionary_mut(id).unwrap();
    field.set("Opt", labels.iter().map(|label| Object::string_literal(label.as_bytes().to_vec())).collect::<Vec<_>>()); field.remove(b"V"); field.remove(b"I"); field.remove(b"DV");
    if top > 0 { field.set("TI", top as i64); } else { field.remove(b"TI"); }
    field.set("Rect", vec![Object::Real(rect[0]), Object::Real(rect[1]), Object::Real(rect[0] + 200.0), Object::Real(rect[1] + 100.0)]);
    let trial = style(&doc, &form, doc.get_dictionary(id).unwrap()).unwrap(); let height = 4.0 * trial.border + 5.4 * trial.size;
    doc.get_dictionary_mut(id).unwrap().set("Rect", vec![Object::Real(rect[0]), Object::Real(rect[1]), Object::Real(rect[0] + 200.0), Object::Real(rect[1] + height)]);
    let mut st = style(&doc, &form, doc.get_dictionary(id).unwrap()).unwrap(); st.top = top; assert_eq!(list_slots(&st), 4);
    let info = ChoiceInfo { presentation: "list", options: labels.iter().enumerate().map(|(n, label)| ChoiceOption { option_id: format!("o{n}"), label: label.clone(), export: label.clone() }).collect(), selected_option_id: None };
    let content = choice_appearance(&st, &info).unwrap().encode().unwrap();
    let normal = doc.get_dictionary(id).unwrap().get(b"AP").unwrap().as_dict().unwrap().get(b"N").unwrap().as_reference().unwrap();
    let stream = doc.get_object_mut(normal).unwrap().as_stream_mut().unwrap(); stream.dict.remove(b"Filter"); stream.set_content(content);
    stream.dict.set("BBox", vec![Object::Real(0.0), Object::Real(0.0), Object::Real(st.width), Object::Real(st.height)]);
    let mut bytes = Vec::new(); doc.save_to(&mut bytes).unwrap(); (bytes, id)
}
fn list_field(parsed: &FormDocument) -> &ParsedField { parsed.fields.iter().find(|field| field.dto.choice.as_ref().is_some_and(|choice| choice.presentation == "list")).unwrap() }

#[test]
fn a_list_box_with_more_options_than_rows_scrolls_to_the_selected_option() {
    let (bytes, id) = scrolling_list(0); let source = session(bytes);
    let parsed = FormDocument::parse(&source).unwrap(); let field = list_field(&parsed); let choice = field.dto.choice.as_ref().unwrap(); assert_eq!(choice.options.len(), 10);
    let (field_id, near, far) = (field.dto.field_id.clone(), choice.options[1].option_id.clone(), choice.options[7].option_id.clone());
    let fill = |option: &str| FormDocument::parse(&source).unwrap().prepare(&[FieldValue::Choice { field_id: field_id.clone(), option_id: Some(option.to_string()) }]).unwrap().0;
    let top_of = |bytes: &[u8]| Document::load_mem(bytes).unwrap().get_dictionary(id).unwrap().get(b"TI").ok().and_then(|v| v.as_i64().ok());
    let visible = fill(&near); assert_eq!(top_of(&visible), Some(0), "an option already in view does not scroll");
    let scrolled = fill(&far); assert_eq!(top_of(&scrolled), Some(6), "an option out of view scrolls just far enough to be the last pages' top");
    let reparsed = FormDocument::parse(&session(scrolled)).unwrap(); let choice = list_field(&reparsed).dto.choice.as_ref().unwrap();
    assert_eq!(choice.options.iter().find(|o| Some(&o.option_id) == choice.selected_option_id.as_ref()).unwrap().label, "Option 7");
    let cleared = FormDocument::parse(&session(fill(&far))).unwrap().prepare(&[FieldValue::Choice { field_id: field_id.clone(), option_id: None }]).unwrap().0;
    assert!(FormDocument::parse(&session(cleared)).unwrap().fields.iter().all(|f| f.dto.choice.as_ref().is_none_or(|c| c.presentation != "list" || c.selected_option_id.is_none())));
}

#[test]
fn a_source_scroll_position_is_validated_against_its_appearance() {
    let (bytes, _) = scrolling_list(3); let parsed = FormDocument::parse(&session(bytes)).unwrap(); assert_eq!(list_field(&parsed).style.as_ref().unwrap().top, 3);
    let (bytes, id) = scrolling_list(0); let mut doc = Document::load_mem(&bytes).unwrap(); doc.get_dictionary_mut(id).unwrap().set("TI", 9); let mut bad = Vec::new(); doc.save_to(&mut bad).unwrap();
    assert_eq!(FormDocument::query(&session(bad), 1).status, "unsupported", "a top index past the last full page is refused");
    let (bytes, id) = scrolling_list(0); let mut doc = Document::load_mem(&bytes).unwrap(); doc.get_dictionary_mut(id).unwrap().set("TI", 2); let mut mismatch = Vec::new(); doc.save_to(&mut mismatch).unwrap();
    assert_eq!(FormDocument::query(&session(mismatch), 1).status, "unsupported", "a scroll position that disagrees with the drawn rows is refused");
}

#[test]
fn quotes_parentheses_and_backslashes_mix_with_latin1_characters() {
    let source = session(FIXTURE.to_vec()); let value = r"a(b)\é)(";
    let bytes = fill(&source, value).unwrap(); let parsed = FormDocument::parse(&session(bytes.clone())).unwrap(); assert_eq!(parsed.fields[0].dto.value, value);
    let document = Document::load_mem(&bytes).unwrap(); let field = document.get_dictionary(parsed.fields[0].object).unwrap();
    let stream = document.get_object(field.get(b"AP").unwrap().as_dict().unwrap().get(b"N").unwrap().as_reference().unwrap()).unwrap().as_stream().unwrap();
    let content = Content::decode(&stream.decompressed_content().unwrap()).unwrap();
    assert_eq!(content.operations.iter().find(|operation| operation.operator == "Tj").unwrap().operands[0].as_str().unwrap(), b"a(b)\\\xe9)(");
}

fn font_with_encoding(encoding: Object) -> Vec<u8> {
    let mut document = Document::load_mem(FIXTURE).unwrap();
    for object in document.objects.values_mut() { if let Object::Dictionary(font) = object { if font.get(b"BaseFont").is_ok() { font.set("Encoding", encoding.clone()); } } }
    let mut bytes = Vec::new(); document.save_to(&mut bytes).unwrap(); bytes
}
fn differences(items: Vec<Object>) -> Object { Object::Dictionary(dictionary! { "Type" => "Encoding", "BaseEncoding" => "WinAnsiEncoding", "Differences" => items }) }

#[test]
fn a_winansi_font_defined_through_differences_is_accepted_unless_it_remaps_a_latin1_code() {
    let plain = differences(vec![Object::Integer(39), Object::Name(b"quotesingle".to_vec()), Object::Integer(96), Object::Name(b"grave".to_vec())]);
    assert!(fill(&session(font_with_encoding(plain)), "é").is_ok(), "WinAnsi base with only the ASCII quote entries");
    let remapped = differences(vec![Object::Integer(39), Object::Name(b"quotesingle".to_vec()), Object::Integer(96), Object::Name(b"grave".to_vec()), Object::Integer(233), Object::Name(b"Euro".to_vec())]);
    let source = session(font_with_encoding(remapped)); assert!(fill(&source, "plain").is_ok()); assert!(fill(&source, "é").unwrap_err().contains("beyond ASCII"), "code 233 no longer shows eacute");
    let restated = differences(vec![Object::Integer(39), Object::Name(b"quotesingle".to_vec()), Object::Integer(96), Object::Name(b"grave".to_vec()), Object::Integer(233), Object::Name(b"eacute".to_vec())]);
    assert!(fill(&session(font_with_encoding(restated)), "é").is_ok(), "a difference that restates the glyph is fine");
}

#[test]
fn encoding_differences_with_an_overflowing_code_are_refused_without_panicking() {
    let mut document = Document::default();
    let encoding = dictionary! { "BaseEncoding" => "WinAnsiEncoding", "Differences" => vec![Object::Integer(i64::MAX), Object::Name(b"A".to_vec()), Object::Name(b"B".to_vec())] };
    let font = document.add_object(dictionary! { "Type" => "Font", "Subtype" => "Type1", "BaseFont" => "Helvetica", "Encoding" => encoding });
    assert!(!full_winansi(&document, &Object::Reference(font)));
}

#[test]
fn a_list_box_scrolls_back_up_when_the_choice_is_above_the_visible_rows() {
    let (bytes, id) = scrolling_list(6); let source = session(bytes);
    let parsed = FormDocument::parse(&source).unwrap(); let field = list_field(&parsed); assert_eq!(field.style.as_ref().unwrap().top, 6);
    let (field_id, option) = (field.dto.field_id.clone(), field.dto.choice.as_ref().unwrap().options[1].option_id.clone());
    let filled = FormDocument::parse(&source).unwrap().prepare(&[FieldValue::Choice { field_id, option_id: Some(option) }]).unwrap().0;
    assert_eq!(Document::load_mem(&filled).unwrap().get_dictionary(id).unwrap().get(b"TI").unwrap().as_i64().unwrap(), 1, "index 1 above the top row 6 becomes the new top");
    let reparsed = FormDocument::parse(&session(filled)).unwrap(); assert_eq!(list_field(&reparsed).style.as_ref().unwrap().top, 1);
}

#[test]
fn list_box_labels_are_drawn_with_the_same_byte_encoding_as_other_text() {
    let (bytes, _) = scrolling_list(0); let parsed = FormDocument::parse(&session(bytes)).unwrap();
    let field = list_field(&parsed); let content = choice_appearance(field.style.as_ref().unwrap(), field.dto.choice.as_ref().unwrap()).unwrap();
    assert_eq!(content.operations.iter().find(|operation| operation.operator == "Tj").unwrap().operands[0].as_str().unwrap(), b"Option 0");
    assert_eq!(win_bytes("Caf\u{e9}"), b"Caf\xe9");
}
