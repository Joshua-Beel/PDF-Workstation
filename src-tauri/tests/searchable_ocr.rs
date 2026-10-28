#[path = "../src/searchable_ocr.rs"]
mod searchable_ocr;
#[allow(dead_code)]
#[path = "../src/searchable_pdf.rs"]
mod searchable_pdf;

use searchable_ocr::{map_words_to_display, parse_tesseract_tsv, MAX_TSV_BYTES_PER_PAGE};

const HEADER: &str =
    "level\tpage_num\tblock_num\tpar_num\tline_num\tword_num\tleft\ttop\twidth\theight\tconf\ttext";

fn document(rows: &[&str]) -> String {
    std::iter::once(HEADER)
        .chain(rows.iter().copied())
        .collect::<Vec<_>>()
        .join("\n")
}

fn one_line(words: &[&str]) -> String {
    let mut rows = vec![
        "1\t1\t0\t0\t0\t0\t0\t0\t1000\t500\t-1\t",
        "2\t1\t1\t0\t0\t0\t0\t0\t1000\t500\t-1\t",
        "3\t1\t1\t1\t0\t0\t0\t0\t1000\t500\t-1\t",
        "4\t1\t1\t1\t1\t0\t0\t0\t1000\t500\t-1\t",
    ];
    rows.extend_from_slice(words);
    document(&rows)
}

#[test]
fn parses_decimal_confidence_and_maps_non_square_geometry() {
    let tsv = document(&[
        "1\t1\t0\t0\t0\t0\t0\t0\t1000\t500\t-1\t",
        "2\t1\t1\t0\t0\t0\t0\t0\t1000\t500\t-1\t",
        "3\t1\t1\t1\t0\t0\t0\t0\t1000\t500\t-1\t",
        "4\t1\t1\t1\t1\t0\t0\t0\t1000\t300\t-1\t",
        "5\t1\t1\t1\t1\t1\t100\t50\t200\t100\t96.125\tHello",
        "5\t1\t1\t1\t1\t2\t400\t200\t100\t50\t88\tworld!",
        "4\t1\t1\t1\t2\t0\t0\t300\t100\t100\t-1\t",
        "5\t1\t1\t1\t2\t1\t20\t300\t80\t40\t75.5\tAgain",
    ]);
    let parsed = parse_tesseract_tsv(tsv.as_bytes(), 1000, 500).unwrap();
    assert_eq!(parsed.extracted_text, "Hello world!\nAgain");
    let display = map_words_to_display(&parsed, 612.0, 792.0).unwrap();
    assert!((display[0].x - 61.2).abs() < 1e-9);
    assert!((display[0].width - 122.4).abs() < 1e-9);
    assert_eq!((parsed.words[0].top, parsed.words[0].height), (50, 100));
    assert_eq!((parsed.words[0].line_top, parsed.words[0].line_height), (0, 300));
    assert!((display[0].y - 0.0).abs() < 1e-9);
    assert!((display[0].height - 475.2).abs() < 1e-9);
    assert_eq!((parsed.words[2].line_top, parsed.words[2].line_height), (300, 100));
    assert!((display[2].y - 475.2).abs() < 1e-9);
    assert!((display[2].height - 158.4).abs() < 1e-9);
}

#[test]
fn thin_words_take_their_line_band() {
    let tsv = one_line(&[
        "5\t1\t1\t1\t1\t1\t100\t50\t200\t100\t90\tword",
        "5\t1\t1\t1\t1\t2\t320\t104\t23\t1\t90\t—",
    ]);
    let parsed = parse_tesseract_tsv(tsv.as_bytes(), 1000, 500).unwrap();
    let display = map_words_to_display(&parsed, 1000.0, 500.0).unwrap();
    assert_eq!((display[1].x, display[1].width), (320.0, 23.0));
    assert_eq!((display[1].y, display[1].height), (0.0, 500.0));
    assert_eq!((display[0].y, display[0].height), (display[1].y, display[1].height));
}

#[test]
fn permits_a_blank_page_and_crlf() {
    let parsed = parse_tesseract_tsv(
        format!("{HEADER}\r\n1\t1\t0\t0\t0\t0\t0\t0\t20\t10\t-1\t\r\n").as_bytes(),
        20,
        10,
    )
    .unwrap();
    assert!(parsed.words.is_empty());
    assert!(parsed.extracted_text.is_empty());
}

#[test]
fn rejects_wrong_header_and_malformed_rows() {
    assert!(parse_tesseract_tsv(b"wrong", 10, 10).is_err());
    assert!(parse_tesseract_tsv(format!("{HEADER}\n1\t1").as_bytes(), 10, 10).is_err());
    assert!(parse_tesseract_tsv(format!("{HEADER}\n").as_bytes(), 10, 10).is_err());
    assert!(parse_tesseract_tsv(format!("{HEADER}\n\n").as_bytes(), 10, 10).is_err());
}

#[test]
fn accepts_unicode_word_text_including_non_bmp() {
    for text in ["café", "“quoted”", "naïve—final…", "Ærø’s", "Łódź", "𝔄𝔅", "a😀b"] {
        let row = format!("5\t1\t1\t1\t1\t1\t0\t0\t5\t5\t90\t{text}");
        let tsv = one_line(&[&row]);
        let parsed = parse_tesseract_tsv(tsv.as_bytes(), 1000, 500).unwrap();
        assert_eq!(parsed.words[0].text, text);
        assert_eq!(parsed.extracted_text, text);
    }
}

#[test]
fn expands_latin_ligatures_before_limits_and_oracle_text() {
    let row = "5\t1\t1\t1\t1\t1\t0\t0\t5\t5\t90\tﬀﬁﬂﬃﬄﬅﬆ";
    let parsed = parse_tesseract_tsv(one_line(&[row]).as_bytes(), 1000, 500).unwrap();
    assert_eq!(parsed.words[0].text, "fffiflffifflſtst");
    assert_eq!(parsed.extracted_text, "fffiflffifflſtst");
    let max = searchable_pdf::MAX_SEARCHABLE_WORD_CHARS;
    let fits = format!("5\t1\t1\t1\t1\t1\t0\t0\t5\t5\t90\t{}", "ﬁ".repeat(max / 2));
    assert!(parse_tesseract_tsv(one_line(&[&fits]).as_bytes(), 1000, 500).is_ok());
    let over = format!("5\t1\t1\t1\t1\t1\t0\t0\t5\t5\t90\t{}", "ﬃ".repeat(max / 3 + 1));
    assert!(parse_tesseract_tsv(one_line(&[&over]).as_bytes(), 1000, 500).is_err());
}

#[test]
fn rejects_control_whitespace_invisible_and_oversize_word_text() {
    let long = "é".repeat(searchable_pdf::MAX_SEARCHABLE_WORD_CHARS + 1);
    for text in [
        "two words", "bad\tfield", "nb\u{a0}sp", "bell\u{7}", "c1\u{85}", "zero\u{200b}width",
        "bidi\u{202e}x", "bom\u{feff}", "soft\u{ad}hyphen", "bad\u{fffd}", "pua\u{e000}",
        "non\u{fffe}", "plane16\u{10fffd}", "ideo\u{3000}space", long.as_str(),
    ] {
        let row = format!("5\t1\t1\t1\t1\t1\t0\t0\t5\t5\t90\t{text}");
        let tsv = one_line(&[&row]);
        assert!(parse_tesseract_tsv(tsv.as_bytes(), 1000, 500).is_err(), "{text:?}");
    }
    let exact = "é".repeat(searchable_pdf::MAX_SEARCHABLE_WORD_CHARS);
    let row = format!("5\t1\t1\t1\t1\t1\t0\t0\t5\t5\t90\t{exact}");
    assert!(parse_tesseract_tsv(one_line(&[&row]).as_bytes(), 1000, 500).is_ok());
}

#[test]
fn rejects_boxes_outside_raster_and_integer_overflow() {
    for row in [
        "5\t1\t1\t1\t1\t1\t999\t0\t2\t1\t90\tA",
        "5\t1\t1\t1\t1\t1\t4294967295\t0\t2\t1\t90\tA",
        "5\t1\t1\t1\t1\t1\t0\t499\t1\t2\t90\tA",
    ] {
        let tsv = one_line(&[row]);
        assert!(parse_tesseract_tsv(tsv.as_bytes(), 1000, 500).is_err());
    }
}

#[test]
fn rejects_invalid_confidence_and_unhandled_structural_text() {
    for row in [
        "5\t1\t1\t1\t1\t1\t0\t0\t1\t1\tNaN\tA",
        "5\t1\t1\t1\t1\t1\t0\t0\t1\t1\t101\tA",
        "5\t1\t1\t1\t1\t1\t0\t0\t1\t1\t-1\tA",
        "4\t1\t1\t1\t1\t0\t0\t0\t1\t1\t-1\thidden",
        "4\t1\t1\t1\t1\t0\t0\t0\t1\t1\t0\t",
    ] {
        let tsv = one_line(&[row]);
        assert!(parse_tesseract_tsv(tsv.as_bytes(), 1000, 500).is_err());
    }
}

#[test]
fn rejects_ambiguous_or_duplicate_word_order() {
    for second in [
        "5\t1\t1\t1\t1\t1\t2\t0\t1\t1\t90\tB",
        "5\t1\t1\t1\t0\t2\t2\t0\t1\t1\t90\tB",
        "5\t1\t1\t0\t2\t1\t2\t0\t1\t1\t90\tB",
    ] {
        let tsv = one_line(&["5\t1\t1\t1\t1\t1\t0\t0\t1\t1\t90\tA", second]);
        assert!(parse_tesseract_tsv(tsv.as_bytes(), 1000, 500).is_err());
    }
}

#[test]
fn enforces_input_limit_before_parsing() {
    let oversized = vec![0xff; MAX_TSV_BYTES_PER_PAGE + 1];
    assert!(parse_tesseract_tsv(&oversized, 10, 10).is_err());
    assert!(parse_tesseract_tsv(&[0xff], 10, 10).is_err());
}

#[test]
fn rejects_invalid_display_dimensions() {
    let tsv = document(&["1\t1\t0\t0\t0\t0\t0\t0\t10\t10\t-1\t"]);
    let parsed = parse_tesseract_tsv(tsv.as_bytes(), 10, 10).unwrap();
    for dimensions in [(0.0, 1.0), (1.0, -1.0), (f64::NAN, 1.0)] {
        assert!(map_words_to_display(&parsed, dimensions.0, dimensions.1).is_err());
    }
}

#[test]
fn rejects_missing_mismatched_or_duplicate_hierarchy() {
    let cases = [
        vec!["5\t1\t1\t1\t1\t1\t0\t0\t1\t1\t90\tA"],
        vec![
            "1\t1\t0\t0\t0\t0\t0\t0\t1000\t500\t-1\t",
            "3\t1\t1\t1\t0\t0\t0\t0\t100\t100\t-1\t",
        ],
        vec![
            "1\t1\t0\t0\t0\t0\t0\t0\t1000\t500\t-1\t",
            "2\t1\t1\t0\t0\t0\t0\t0\t100\t100\t-1\t",
            "2\t1\t1\t0\t0\t0\t0\t0\t100\t100\t-1\t",
        ],
        vec![
            "1\t1\t0\t0\t0\t0\t0\t0\t1000\t500\t-1\t",
            "2\t1\t1\t0\t0\t0\t0\t0\t100\t100\t-1\t",
            "3\t1\t2\t1\t0\t0\t0\t0\t100\t100\t-1\t",
        ],
    ];
    for rows in cases {
        let tsv = document(&rows);
        assert!(parse_tesseract_tsv(tsv.as_bytes(), 1000, 500).is_err());
    }
}

#[test]
fn rejects_every_incomplete_terminal_hierarchy() {
    let rows = [
        "1\t1\t0\t0\t0\t0\t0\t0\t1000\t500\t-1\t",
        "2\t1\t1\t0\t0\t0\t0\t0\t1000\t500\t-1\t",
        "3\t1\t1\t1\t0\t0\t0\t0\t1000\t500\t-1\t",
        "4\t1\t1\t1\t1\t0\t0\t0\t1000\t500\t-1\t",
    ];
    assert!(parse_tesseract_tsv(HEADER.as_bytes(), 1000, 500).is_err());
    for end in 2..=rows.len() {
        let tsv = document(&rows[..end]);
        assert!(parse_tesseract_tsv(tsv.as_bytes(), 1000, 500).is_err());
    }
}

#[test]
fn rejects_children_outside_parent_boxes() {
    let tsv = document(&[
        "1\t1\t0\t0\t0\t0\t0\t0\t1000\t500\t-1\t",
        "2\t1\t1\t0\t0\t0\t0\t0\t100\t100\t-1\t",
        "3\t1\t1\t1\t0\t0\t90\t90\t20\t20\t-1\t",
    ]);
    assert!(parse_tesseract_tsv(tsv.as_bytes(), 1000, 500).is_err());
}

#[test]
fn forged_pages_cannot_produce_invalid_display_geometry() {
    use searchable_ocr::{OcrWord, ParsedTsvPage};

    let word = OcrWord {
        block: 1,
        paragraph: 1,
        line: 1,
        word: 1,
        left: 9,
        top: 0,
        width: 2,
        height: 1,
        line_top: 0,
        line_height: 1,
        confidence: 90.0,
        text: "A".into(),
    };
    let zero = ParsedTsvPage {
        raster_width: 0,
        raster_height: 10,
        words: vec![],
        extracted_text: String::new(),
    };
    assert!(map_words_to_display(&zero, 100.0, 100.0).is_err());
    let outside = ParsedTsvPage {
        raster_width: 10,
        raster_height: 10,
        words: vec![word.clone()],
        extracted_text: "A".into(),
    };
    assert!(map_words_to_display(&outside, 100.0, 100.0).is_err());
    let inside = OcrWord { left: 0, ..word };
    let page = |word: OcrWord| ParsedTsvPage { raster_width: 100, raster_height: 100, words: vec![word], extracted_text: "A".into() };
    assert!(map_words_to_display(&page(inside.clone()), 100.0, 100.0).is_ok());
    for bad in [
        OcrWord { line_height: 0, ..inside.clone() },
        OcrWord { top: 20, height: 1, line_top: 0, line_height: 2, ..inside.clone() },
        OcrWord { top: 0, line_top: 12, line_height: 3, ..inside.clone() },
        OcrWord { line_top: 95, line_height: 6, top: 95, ..inside.clone() },
        OcrWord { line_top: u32::MAX, line_height: 2, ..inside.clone() },
        OcrWord { text: "a\u{200b}".into(), ..inside.clone() },
    ] {
        assert!(map_words_to_display(&page(bad), 100.0, 100.0).is_err());
    }
}

#[test]
fn tolerates_rounding_overhang_seen_in_real_engine_output() {
    let tsv = document(&[
        "1\t1\t0\t0\t0\t0\t0\t0\t1000\t500\t-1\t",
        "2\t1\t1\t0\t0\t0\t10\t10\t400\t100\t-1\t",
        "3\t1\t1\t1\t0\t0\t10\t10\t400\t100\t-1\t",
        "4\t1\t1\t1\t1\t0\t10\t14\t400\t100\t-1\t",
        "5\t1\t1\t1\t1\t1\t10\t17\t100\t96\t90\tHello",
        "5\t1\t1\t1\t1\t2\t120\t14\t100\t104\t90\tthere",
    ]);
    let parsed = parse_tesseract_tsv(tsv.as_bytes(), 1000, 500).unwrap();
    assert_eq!(parsed.extracted_text, "Hello there");
    let mapped = map_words_to_display(&parsed, 1000.0, 500.0).unwrap();
    assert_eq!(mapped.len(), 2);
}

#[test]
fn still_refuses_line_and_word_boxes_far_outside_their_parents() {
    let far_line = document(&[
        "1\t1\t0\t0\t0\t0\t0\t0\t1000\t500\t-1\t",
        "2\t1\t1\t0\t0\t0\t10\t10\t400\t100\t-1\t",
        "3\t1\t1\t1\t0\t0\t10\t10\t400\t100\t-1\t",
        "4\t1\t1\t1\t1\t0\t10\t19\t400\t100\t-1\t",
    ]);
    assert!(parse_tesseract_tsv(far_line.as_bytes(), 1000, 500).is_err());
    // A word that shares no pixel row with its line is refused, above or below.
    for word in ["5\t1\t1\t1\t1\t1\t0\t121\t10\t29\t90\tA", "5\t1\t1\t1\t1\t1\t0\t71\t10\t29\t90\tA"] {
        let rows = [
            "1\t1\t0\t0\t0\t0\t0\t0\t1000\t500\t-1\t",
            "2\t1\t1\t0\t0\t0\t0\t0\t1000\t500\t-1\t",
            "3\t1\t1\t1\t0\t0\t0\t0\t1000\t500\t-1\t",
            "4\t1\t1\t1\t1\t0\t0\t100\t400\t20\t-1\t",
            word,
        ];
        assert!(parse_tesseract_tsv(document(&rows).as_bytes(), 1000, 500).is_err(), "{word}");
    }
}

/// Shapes seen in real engine output on scanned pages (synthetic text): the
/// line box is the text row, so a tall first letter or a stray mark starts
/// left of it and above or below it.
#[test]
fn accepts_words_that_overhang_their_line_box_and_keeps_them_in_its_band() {
    let rows = [
        "1\t1\t0\t0\t0\t0\t0\t0\t1000\t500\t-1\t",
        "2\t1\t1\t0\t0\t0\t150\t100\t800\t200\t-1\t",
        "3\t1\t1\t1\t0\t0\t150\t100\t800\t200\t-1\t",
        "4\t1\t1\t1\t1\t0\t150\t120\t600\t30\t-1\t",
        "5\t1\t1\t1\t1\t1\t12\t103\t110\t40\t61.5\tSample",
        "5\t1\t1\t1\t1\t2\t200\t125\t60\t20\t90\tthe",
        "5\t1\t1\t1\t1\t3\t700\t130\t88\t32\t40\tpage,",
    ];
    let parsed = parse_tesseract_tsv(document(&rows).as_bytes(), 1000, 500).unwrap();
    assert_eq!(parsed.extracted_text, "Sample the page,");
    let (first, middle, last) = (&parsed.words[0], &parsed.words[1], &parsed.words[2]);
    for word in [first, middle, last] {
        assert_eq!((word.line_top, word.line_height), (120, 30), "one band per line keeps the line together on extraction");
    }
    let mapped = map_words_to_display(&parsed, 500.0, 250.0).unwrap();
    assert_eq!((mapped[0].x, mapped[0].y, mapped[0].height), (6.0, 60.0, 15.0));
    let mut detached = parsed.clone();
    detached.words[1].top = 160;
    assert!(map_words_to_display(&detached, 500.0, 250.0).is_err(), "a word below its band is refused at mapping too");
}

#[test]
fn trims_an_engine_space_at_the_edge_of_a_word_and_skips_space_only_words() {
    let tsv = one_line(&[
        "5\t1\t1\t1\t1\t1\t10\t10\t30\t20\t18.3\tone",
        "5\t1\t1\t1\t1\t2\t50\t10\t60\t20\t60.6\t quick,",
        "5\t1\t1\t1\t1\t3\t120\t10\t5\t20\t30\t ",
        "5\t1\t1\t1\t1\t4\t130\t10\t20\t20\t75\tas ",
    ]);
    let parsed = parse_tesseract_tsv(tsv.as_bytes(), 1000, 500).unwrap();
    assert_eq!(parsed.extracted_text, "one quick, as");
    assert_eq!(parsed.words.len(), 3);
    for text in [" two words", "\u{85}nel", "tab\t", "\u{a0}nbsp"] {
        let row = format!("5\t1\t1\t1\t1\t1\t0\t0\t5\t5\t90\t{text}");
        assert!(parse_tesseract_tsv(one_line(&[&row]).as_bytes(), 1000, 500).is_err(), "{text:?}");
    }
    let negative = one_line(&["5\t1\t1\t1\t1\t1\t0\t0\t5\t5\t-1\t "]);
    assert!(parse_tesseract_tsv(negative.as_bytes(), 1000, 500).is_err());
    let outside = one_line(&["5\t1\t1\t1\t1\t1\t995\t0\t6\t5\t90\t "]);
    assert!(parse_tesseract_tsv(outside.as_bytes(), 1000, 500).is_err());
}
