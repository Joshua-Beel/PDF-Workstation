#[path = "../src/searchable_pdf.rs"]
mod searchable_pdf;

use lopdf::{content::Content, Document, Object};
use searchable_pdf::{verify_searchable_pdf, write_searchable_pdf, SearchableRasterPage, SearchableWord, MAX_SEARCHABLE_PIXELS};

fn page(words: Vec<SearchableWord>) -> SearchableRasterPage {
    SearchableRasterPage {
        page_width: 200.0,
        page_height: 100.0,
        image_width: 2,
        image_height: 1,
        rgb: vec![255, 0, 0, 0, 255, 0],
        words,
    }
}

fn word(text: &str) -> SearchableWord {
    SearchableWord { x: 20.0, y: 10.0, width: 60.0, height: 12.0, text: text.into() }
}

#[test]
fn writes_canonical_raster_with_invisible_ascii_text() {
    let expected = vec![page(vec![word("Hello")])];
    let bytes = write_searchable_pdf(&expected).unwrap();
    verify_searchable_pdf(&bytes, &expected).unwrap();
    let document = Document::load_mem(&bytes).unwrap();
    let page_id = *document.get_pages().values().next().unwrap();
    let content = document.get_page_content(page_id);
    let operations = Content::decode(&content).unwrap().operations;
    assert_eq!(operations.iter().filter(|op| op.operator == "Do").count(), 1);
    assert_eq!(operations.iter().filter(|op| op.operator == "Tr").count(), 1);
    assert_eq!(operations.iter().filter(|op| op.operator == "Tm").count(), 1);
    let shown = operations.iter().find(|op| op.operator == "Tj").unwrap();
    assert_eq!(shown.operands[0].as_str().unwrap(), &[0, 1, 0, 2, 0, 3, 0, 3, 0, 4]);
    let render_mode = operations.iter().find(|op| op.operator == "Tr").unwrap();
    assert_eq!(render_mode.operands, vec![Object::Integer(3)]);
}

fn font_parts(document: &Document) -> (lopdf::Dictionary, lopdf::Dictionary, String, Vec<u8>, Vec<u8>) {
    let page_id = *document.get_pages().values().next().unwrap();
    let page = document.get_dictionary(page_id).unwrap();
    let fonts = page.get(b"Resources").unwrap().as_dict().unwrap().get(b"Font").unwrap().as_dict().unwrap();
    let font = document.get_dictionary(fonts.get(b"F1").unwrap().as_reference().unwrap()).unwrap().clone();
    let cid_ref = font.get(b"DescendantFonts").unwrap().as_array().unwrap()[0].as_reference().unwrap();
    let cid = document.get_dictionary(cid_ref).unwrap().clone();
    let stream = |object: &Object| {
        document.get_object(object.as_reference().unwrap()).unwrap().as_stream().unwrap().decompressed_content().unwrap()
    };
    let to_unicode = String::from_utf8(stream(font.get(b"ToUnicode").unwrap())).unwrap();
    let gid_map = stream(cid.get(b"CIDToGIDMap").unwrap());
    let descriptor = document.get_dictionary(cid.get(b"FontDescriptor").unwrap().as_reference().unwrap()).unwrap();
    let program = stream(descriptor.get(b"FontFile2").unwrap());
    (font, cid, to_unicode, gid_map, program)
}

#[test]
fn unicode_words_use_a_type0_identity_font_with_to_unicode_for_every_code() {
    let expected = vec![page(vec![word("café"), word("ﬁ“x”—"), word("é𝔄😀")])];
    let bytes = write_searchable_pdf(&expected).unwrap();
    verify_searchable_pdf(&bytes, &expected).unwrap();
    let document = Document::load_mem(&bytes).unwrap();
    let (font, cid, to_unicode, gid_map, program) = font_parts(&document);
    assert_eq!(font.get(b"Subtype").unwrap().as_name().unwrap(), b"Type0");
    assert_eq!(font.get(b"Encoding").unwrap().as_name().unwrap(), b"Identity-H");
    assert_eq!(cid.get(b"Subtype").unwrap().as_name().unwrap(), b"CIDFontType2");
    assert_eq!(cid.get(b"DW").unwrap().as_i64().unwrap(), 500);
    for line in [
        "<0001> <0063>", "<0004> <00E9>", "<0005> <FB01>", "<0006> <201C>", "<0007> <0078>",
        "<0008> <201D>", "<0009> <2014>", "<000A> <D835DD04>", "<000B> <D83DDE00>",
    ] {
        assert!(to_unicode.contains(line), "{line} missing from {to_unicode}");
    }
    assert!(to_unicode.contains("11 beginbfchar"));
    assert_eq!(gid_map.len(), 24);
    assert_eq!(&gid_map[..4], &[0, 0, 0, 1]);
    assert!(gid_map[2..].chunks(2).all(|pair| pair == [0, 1]));
    assert_eq!(&program[..4], &[0, 1, 0, 0]);
    assert!(program.len() < 1024, "{}", program.len());
    let checksum = program.chunks(4).fold(0_u32, |sum, chunk| {
        let mut word = [0_u8; 4];
        word[..chunk.len()].copy_from_slice(chunk);
        sum.wrapping_add(u32::from_be_bytes(word))
    });
    assert_eq!(checksum, 0xB1B0_AFBA);
    let page_id = *document.get_pages().values().next().unwrap();
    let operations = Content::decode(&document.get_page_content(page_id)).unwrap().operations;
    let shown = operations.iter().filter(|op| op.operator == "Tj")
        .map(|op| op.operands[0].as_str().unwrap().to_vec()).collect::<Vec<_>>();
    assert_eq!(shown[2], vec![0, 4, 0, 10, 0, 11]);
}

#[test]
fn bmp_and_non_bmp_characters_round_trip_through_many_cmap_blocks() {
    let words = (0..250_u32)
        .map(|index| word(&char::from_u32(0x00C0 + index).unwrap().to_string()))
        .chain([word("\u{1F600}"), word("\u{20000}")])
        .collect::<Vec<_>>();
    let words = words.into_iter()
        .filter(|item| item.text.chars().all(searchable_pdf::is_supported_word_char))
        .collect::<Vec<_>>();
    let count = words.len();
    let bytes = write_searchable_pdf(&[page(words)]).unwrap();
    let (_, _, to_unicode, gid_map, _) = font_parts(&Document::load_mem(&bytes).unwrap());
    assert_eq!(to_unicode.matches("beginbfchar").count(), count.div_ceil(100));
    assert_eq!(to_unicode.lines().filter(|line| line.starts_with('<') && !line.starts_with("<0000> <FFFF>")).count(), count);
    assert!(to_unicode.contains("<D83DDE00>"));
    assert!(to_unicode.contains("<D840DC00>"));
    assert_eq!(gid_map.len(), (count + 1) * 2);
}

#[test]
fn refuses_oversize_words_and_too_many_distinct_characters() {
    let long = "é".repeat(searchable_pdf::MAX_SEARCHABLE_WORD_CHARS + 1);
    assert!(write_searchable_pdf(&[page(vec![word(&long)])]).is_err());
    let exact = "é".repeat(searchable_pdf::MAX_SEARCHABLE_WORD_CHARS);
    assert!(write_searchable_pdf(&[page(vec![word(&exact)])]).is_ok());
    let distinct = (0x4E00_u32..)
        .filter_map(char::from_u32)
        .take(searchable_pdf::MAX_SEARCHABLE_DISTINCT_CHARS + 1)
        .collect::<Vec<_>>();
    let words = distinct.chunks(100).map(|chunk| word(&chunk.iter().collect::<String>())).collect::<Vec<_>>();
    assert!(write_searchable_pdf(&[page(words.clone())]).is_err());
    let mut fits = words;
    let last = fits.pop().unwrap();
    let mut trimmed = last.text.chars().collect::<Vec<_>>();
    trimmed.pop();
    fits.push(word(&trimmed.into_iter().collect::<String>()));
    assert!(write_searchable_pdf(&[page(fits)]).is_ok());
}

#[test]
fn canonical_oracle_rejects_appended_or_mutated_bytes() {
    let expected = vec![page(vec![word("A")])];
    let bytes = write_searchable_pdf(&expected).unwrap();
    let mut appended = bytes.clone();
    appended.extend_from_slice(b"\nSECRET");
    assert!(verify_searchable_pdf(&appended, &expected).is_err());
    let mut mutated = bytes;
    let position = mutated.windows(3).position(|window| window == b"1.7").unwrap();
    mutated[position + 2] = b'6';
    assert!(verify_searchable_pdf(&mutated, &expected).is_err());
}

#[test]
fn refuses_controls_spaces_invisible_characters_and_invalid_geometry() {
    for text in ["", "two words", "bad\ntext", "nul\u{0}", "del\u{7f}", "rtl\u{202e}", "zw\u{200d}j", "pua\u{f8ff}", "bad\u{fffd}", "non\u{1fffe}"] {
        assert!(write_searchable_pdf(&[page(vec![word(text)])]).is_err());
    }
    for bad in [
        SearchableWord { x: -1.0, ..word("A") },
        SearchableWord { width: 0.0, ..word("A") },
        SearchableWord { x: 190.0, width: 20.0, ..word("A") },
        SearchableWord { y: f64::NAN, ..word("A") },
        SearchableWord { width: f64::MIN_POSITIVE, ..word("A") },
        SearchableWord { height: f64::MIN_POSITIVE, ..word("A") },
    ] {
        assert!(write_searchable_pdf(&[page(vec![bad])]).is_err());
    }
}

#[test]
fn validates_rgb_before_compression_and_aggregate_pixels() {
    let mut bad_rgb = page(vec![]);
    bad_rgb.rgb.pop();
    assert!(write_searchable_pdf(&[bad_rgb]).is_err());
    let too_many = SearchableRasterPage {
        page_width: 1.0,
        page_height: 1.0,
        image_width: (MAX_SEARCHABLE_PIXELS + 1) as u32,
        image_height: 1,
        rgb: vec![],
        words: vec![],
    };
    assert!(write_searchable_pdf(&[too_many]).is_err());
}

#[test]
fn oracle_binds_expected_words_and_pixels() {
    let expected = vec![page(vec![word("A")])];
    let bytes = write_searchable_pdf(&expected).unwrap();
    let changed_word = vec![page(vec![word("B")])];
    assert!(verify_searchable_pdf(&bytes, &changed_word).is_err());
    let mut changed_pixel = expected;
    changed_pixel[0].rgb[0] = 0;
    assert!(verify_searchable_pdf(&bytes, &changed_pixel).is_err());
}

#[test]
fn refuses_format_characters_the_extractor_would_hide() {
    for text in ["ar\u{600}x", "ar\u{605}x", "ar\u{6dd}x", "sy\u{70f}x", "br\u{110bd}x", "eg\u{13430}x", "eg\u{1343f}x", "ar\u{890}x", "ar\u{891}x", "ar\u{8e2}x", "ku\u{110cd}x"] {
        assert!(write_searchable_pdf(&[page(vec![word(text)])]).is_err(), "{text:?}");
    }
    assert!(write_searchable_pdf(&[page(vec![word("ar\u{606}x")])]).is_ok());
}

#[test]
fn embedded_font_reports_a_real_hhea_x_max_extent() {
    let bytes = write_searchable_pdf(&[page(vec![word("A")])]).unwrap();
    let document = Document::load_mem(&bytes).unwrap();
    let (_, _, _, _, program) = font_parts(&document);
    let tables = u16::from_be_bytes([program[4], program[5]]) as usize;
    let record = (0..tables).map(|i| 12 + i * 16).find(|&at| &program[at..at + 4] == b"hhea").unwrap();
    let offset = u32::from_be_bytes(program[record + 8..record + 12].try_into().unwrap()) as usize;
    assert_eq!(u16::from_be_bytes([program[offset + 16], program[offset + 17]]), 500);
}
