#[path = "../src/searchable_ocr.rs"]
mod searchable_ocr;
#[path = "../src/searchable_pdf.rs"]
mod searchable_pdf;

use pdfium_render::prelude::{PdfBitmapFormat, PdfColor, PdfRenderConfig, Pdfium};
use searchable_ocr::{map_words_to_display, parse_tesseract_tsv};
use searchable_pdf::{write_searchable_pdf, SearchableRasterPage, SearchableWord};
use serde_json::Value;
use std::path::PathBuf;
use std::sync::OnceLock;

fn pdfium() -> &'static Pdfium {
    static PDFIUM: OnceLock<Pdfium> = OnceLock::new();
    PDFIUM.get_or_init(|| {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        Pdfium::bind_to_library(root.join("resources/pdfium/bin/pdfium.dll"))
            .map(Pdfium::new)
            .unwrap()
    })
}

#[test]
fn unicode_words_extract_exactly_and_stay_inside_their_boxes() {
    const HEADER: &str =
        "level\tpage_num\tblock_num\tpar_num\tline_num\tword_num\tleft\ttop\twidth\theight\tconf\ttext";
    let lines: [&[(u32, u32, &str)]; 3] = [
        &[(100, 120, "café"), (240, 160, "“naïve”"), (420, 150, "ﬁnal—x"), (590, 120, "‘ok’…")],
        &[(100, 130, "Ærø’s"), (250, 80, "𝔄😀"), (350, 110, "Łódź"), (480, 140, "€½°±×"), (640, 90, "ŒßĞİ")],
        &[(100, 300, "Straße-naïveté"), (420, 60, "a𠀋b"), (500, 180, "e\u{301}x\u{1D11E}"), (700, 200, "ﬀﬁﬂﬃﬄﬅﬆ")],
    ];
    let mut rows = vec![
        "1\t1\t0\t0\t0\t0\t0\t0\t1000\t500\t-1\t".to_string(),
        "2\t1\t1\t0\t0\t0\t50\t40\t900\t300\t-1\t".to_string(),
        "3\t1\t1\t1\t0\t0\t50\t40\t900\t300\t-1\t".to_string(),
    ];
    for (line_index, words) in lines.iter().enumerate() {
        let top = 50 + 70 * line_index as u32;
        rows.push(format!("4\t1\t1\t1\t{}\t0\t60\t{top}\t880\t50\t-1\t", line_index + 1));
        for (word_index, (left, width, text)) in words.iter().enumerate() {
            rows.push(format!("5\t1\t1\t1\t{}\t{}\t{left}\t{}\t{width}\t40\t91.5\t{text}", line_index + 1, word_index + 1, top + 5));
        }
        let (thin_top, thin_height, thin_text) = [(22, 2, "—"), (15, 10, "+"), (20, 4, "–")][line_index];
        rows.push(format!("5\t1\t1\t1\t{}\t{}\t910\t{}\t25\t{thin_height}\t80\t{thin_text}", line_index + 1, words.len() + 1, top + thin_top));
    }
    let tsv = std::iter::once(HEADER.to_string()).chain(rows).collect::<Vec<_>>().join("\n");
    let parsed = parse_tesseract_tsv(tsv.as_bytes(), 1000, 500).unwrap();
    let page_width = 480.0;
    let page_height = 240.0;
    let words = map_words_to_display(&parsed, page_width, page_height)
        .unwrap()
        .into_iter()
        .map(|word| SearchableWord { x: word.x, y: word.y, width: word.width, height: word.height, text: word.text })
        .collect::<Vec<_>>();
    let expected = SearchableRasterPage {
        page_width: page_width as f32,
        page_height: page_height as f32,
        image_width: 4,
        image_height: 2,
        rgb: vec![255; 24],
        words: words.clone(),
    };
    let bytes = write_searchable_pdf(&[expected]).unwrap();
    let pdfium = pdfium();
    let document = pdfium.load_pdf_from_byte_vec(bytes, None).unwrap();
    let page = document.pages().get(0).unwrap();
    let text = page.text().unwrap();
    let visible = page.boundaries().bounding().unwrap().bounds;
    let expected_text = parsed.extracted_text.replace('\n', "\r\n");
    assert_eq!(text.inside_rect(visible), expected_text);
    assert_eq!(text.all(), expected_text);

    let mut generated = 0;
    let mut chars = Vec::new();
    let all_chars = text.chars();
    for character in all_chars.iter() {
        if character.is_generated().unwrap() {
            generated += 1;
            continue;
        }
        chars.push(character);
    }
    let units = |character: &pdfium_render::prelude::PdfPageTextChar| {
        let value = character.unicode_value();
        match char::from_u32(value) {
            Some(decoded) => decoded.encode_utf16(&mut [0; 2]).to_vec(),
            None => vec![u16::try_from(value).unwrap()],
        }
    };
    let flat = words.iter().map(|word| word.text.clone()).collect::<String>();
    let extracted = String::from_utf16(&chars.iter().flat_map(|character| units(character)).collect::<Vec<_>>()).unwrap();
    assert_eq!(extracted, flat);
    assert!(generated > 0);
    let mut cursor = 0;
    for word in &words {
        let expected_units = word.text.encode_utf16().count();
        let left = word.x as f32;
        let right = (word.x + word.width) as f32;
        let top = (page_height - word.y) as f32;
        let bottom = (page_height - word.y - word.height) as f32;
        let mut seen = Vec::new();
        while seen.len() < expected_units {
            let character = &chars[cursor];
            let bounds = character.loose_bounds().unwrap();
            let tight = character.tight_bounds().unwrap();
            let tolerance = 0.05;
            let vertical_tolerance = 0.02 * (top - bottom);
            let label = format!("{} char {}", word.text, seen.len());
            for bounds in [&bounds, &tight] {
                assert!(bounds.left().value >= left - tolerance && bounds.right().value <= right + tolerance,
                    "{label} x {:?} outside {left}..{right}", (bounds.left().value, bounds.right().value));
                assert!(bounds.bottom().value >= bottom - vertical_tolerance && bounds.top().value <= top + vertical_tolerance,
                    "{label} y {:?} outside {bottom}..{top}", (bounds.bottom().value, bounds.top().value));
            }
            assert!(bounds.right().value > bounds.left().value && bounds.top().value > bounds.bottom().value, "{label} empty box");
            assert_eq!(character.render_mode().unwrap(), pdfium_render::prelude::PdfPageTextRenderMode::Invisible);
            seen.extend(units(character));
            cursor += 1;
        }
        assert_eq!(String::from_utf16(&seen).unwrap(), word.text);
    }
    assert_eq!(cursor, chars.len());
    let object = chars[0].text_object().unwrap();
    assert!(object.font().is_embedded().unwrap());
}

#[test]
fn deterministic_fixtures_reopen_with_exact_pixels_and_pdfium_text() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let fixture_root = root.join("tests/fixtures/searchable-ocr");
    let oracle: Value = serde_json::from_slice(
        &std::fs::read(fixture_root.join("oracle.json")).unwrap(),
    )
    .unwrap();
    let pdfium = pdfium();

    for fixture in oracle["fixtures"].as_array().unwrap() {
        let name = fixture["name"].as_str().unwrap();
        let width = fixture["width"].as_u64().unwrap() as u32;
        let height = fixture["height"].as_u64().unwrap() as u32;
        let dpi = fixture["dpi"].as_u64().unwrap() as f64;
        let rgb = image::load_from_memory(
            &std::fs::read(fixture_root.join(format!("{name}.png"))).unwrap(),
        )
        .unwrap()
        .to_rgb8()
        .into_raw();
        let parsed = parse_tesseract_tsv(
            fixture["tsv"].as_str().unwrap().as_bytes(),
            width,
            height,
        )
        .unwrap();
        let page_width = f64::from(width) * 72.0 / dpi;
        let page_height = f64::from(height) * 72.0 / dpi;
        let words = map_words_to_display(&parsed, page_width, page_height)
            .unwrap()
            .into_iter()
            .map(|word| SearchableWord {
                x: word.x,
                y: word.y,
                width: word.width,
                height: word.height,
                text: word.text,
            })
            .collect();
        let expected = SearchableRasterPage {
            page_width: page_width as f32,
            page_height: page_height as f32,
            image_width: width,
            image_height: height,
            rgb,
            words,
        };
        let bytes = write_searchable_pdf(&[expected.clone()]).unwrap();
        let document = pdfium.load_pdf_from_byte_vec(bytes, None).unwrap();
        let page = document.pages().get(0).unwrap();
        let bitmap = page
            .render_with_config(
                &PdfRenderConfig::new()
                    .set_fixed_size(width as i32, height as i32)
                    .set_format(PdfBitmapFormat::BGRA)
                    .set_reverse_byte_order(false)
                    .clear_before_rendering(true)
                    .set_clear_color(PdfColor::WHITE),
            )
            .unwrap();
        let bgra = bitmap.as_raw_bytes();
        assert!(bgra.chunks_exact(4).all(|pixel| pixel[3] == 255), "{name}");
        let reopened_rgb = bgra
            .chunks_exact(4)
            .flat_map(|pixel| [pixel[2], pixel[1], pixel[0]])
            .collect::<Vec<_>>();
        assert_eq!(reopened_rgb, expected.rgb, "{name} pixels");
        let text = page.text().unwrap();
        let visible = page.boundaries().bounding().unwrap().bounds;
        let expected_text = parsed.extracted_text.replace('\n', "\r\n");
        assert_eq!(text.inside_rect(visible), expected_text, "{name} text");
    }
}
