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
    for value in ["世界", "€", "tab\there", "line\nbreak", "soft\u{ad}hyphen", "\u{85}", "emoji 😀"] { assert!(fill(&source, value).is_err(), "{value:?}"); }
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
fn latin1_width_table_matches_the_published_helvetica_metrics() {
    assert_eq!(LATIN1_WIDTHS.len(), 96);
    for (character, width) in [('é', 556.0), ('É', 667.0), ('ß', 611.0), ('Æ', 1000.0), ('æ', 889.0), ('©', 737.0), ('ÿ', 500.0), ('\u{a0}', 278.0), ('A', 667.0)] { assert_eq!(advance(character), width, "{character}"); }
}
