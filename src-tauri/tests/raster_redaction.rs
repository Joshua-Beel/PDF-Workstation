#[path = "../src/raster_redaction.rs"]
mod raster_redaction;
#[path = "../src/sanitization.rs"]
mod sanitization;

use lopdf::Document;
use raster_redaction::{redact_raster_pages, validate_displayed_rectangles, RasterRedactionPage, RasterRedactionRect};
use sanitization::{build_clean_raster_pdf, verify_clean_raster_pdf, CleanRasterPage};

fn page(rectangles: Vec<RasterRedactionRect>) -> RasterRedactionPage {
    RasterRedactionPage {
        page_width: 4.0,
        page_height: 4.0,
        image_width: 4,
        image_height: 4,
        rgb: (0u8..48).collect(),
        rectangles,
    }
}

fn decoded_image(bytes: &[u8]) -> Vec<u8> {
    let document = Document::load_mem(bytes).unwrap();
    let page_id = *document.get_pages().values().next().unwrap();
    let page = document.get_dictionary(page_id).unwrap();
    let image_id = page
        .get(b"Resources")
        .unwrap()
        .as_dict()
        .unwrap()
        .get(b"XObject")
        .unwrap()
        .as_dict()
        .unwrap()
        .get(b"Image")
        .unwrap()
        .as_reference()
        .unwrap();
    document
        .get_object(image_id)
        .unwrap()
        .as_stream()
        .unwrap()
        .decompressed_content()
        .unwrap()
}

#[test]
fn edge_marks_reach_the_last_row_and_column_whatever_the_pixel_size() {
    // Security check for the 150 DPI rounding fix: marks map through the bitmap's own pixel size, so a
    // mark on the page edge blackens the last row and column both at the old size (1275 x 1651 for
    // Letter) and at the corrected size (1275 x 1650); no strip at the edge is left uncovered.
    for (image_width, image_height) in [(1_275u32, 1_651u32), (1_275, 1_650)] {
        let pixels = (image_width * image_height) as usize;
        let page = RasterRedactionPage {
            page_width: 612.0,
            page_height: 792.0,
            image_width,
            image_height,
            rgb: vec![255; pixels * 3],
            rectangles: vec![RasterRedactionRect { x: 600.0, y: 780.0, width: 12.0, height: 12.0 }],
        };
        let image = decoded_image(&redact_raster_pages(vec![page]).unwrap().bytes);
        let at = |x: u32, y: u32| &image[((y * image_width + x) * 3) as usize..((y * image_width + x) * 3 + 3) as usize];
        assert_eq!(at(image_width - 1, image_height - 1), [0, 0, 0]);
        assert_eq!(at(image_width - 1, image_height - 25), [0, 0, 0]);
        assert_eq!(at(image_width - 25, image_height - 1), [0, 0, 0]);
        assert_eq!(at(0, 0), [255, 255, 255]);
    }
}

#[test]
fn writer_is_exact_bounded_and_self_verified() {
    let bytes = build_clean_raster_pdf(vec![CleanRasterPage {
        page_width: 4.0,
        page_height: 4.0,
        image_width: 2,
        image_height: 1,
        rgb: vec![1, 2, 3, 4, 5, 6],
    }])
    .unwrap();
    assert_eq!(verify_clean_raster_pdf(&bytes, 1).unwrap().total_pixels, 2);
    assert_eq!(decoded_image(&bytes), vec![1, 2, 3, 4, 5, 6]);
    assert!(build_clean_raster_pdf(vec![]).is_err());
    assert!(build_clean_raster_pdf(vec![CleanRasterPage {
        page_width: 4.0,
        page_height: 4.0,
        image_width: 2,
        image_height: 1,
        rgb: vec![0; 5],
    }])
    .unwrap_err()
    .contains("byte count"));
}

#[test]
fn redaction_conservatively_blacks_only_mapped_pixels_and_counts_union() {
    let original = page(vec![
        RasterRedactionRect {
            x: 0.5,
            y: 0.5,
            width: 1.0,
            height: 1.0,
        },
        RasterRedactionRect {
            x: 1.0,
            y: 1.0,
            width: 2.0,
            height: 2.0,
        },
    ]);
    let original_rgb = original.rgb.clone();
    let result = redact_raster_pages(vec![original]).unwrap();
    assert_eq!(
        (result.pages, result.rectangles, result.covered_pixels),
        (1, 2, 7)
    );
    verify_clean_raster_pdf(&result.bytes, 1).unwrap();
    let rgb = decoded_image(&result.bytes);
    for pixel in 0..16 {
        let x = pixel % 4;
        let y = pixel / 4;
        let covered = (x < 2 && y < 2) || ((1..3).contains(&x) && (1..3).contains(&y));
        assert_eq!(
            &rgb[pixel * 3..pixel * 3 + 3],
            if covered {
                &[0, 0, 0]
            } else {
                &original_rgb[pixel * 3..pixel * 3 + 3]
            }
        );
    }
    let document = Document::load_mem(&result.bytes).unwrap();
    assert_eq!(document.extract_text(&[1]).unwrap(), "");
    assert!(!result.bytes.windows(6).any(|window| window == b"SECRET"));
}

#[test]
fn invalid_rectangles_and_unredacted_requests_fail_without_output() {
    assert!(validate_displayed_rectangles(4.0, 4.0, 4, 4, &[]).is_err());
    assert!(redact_raster_pages(vec![page(vec![])])
        .unwrap_err()
        .contains("at least one"));
    for rect in [
        RasterRedactionRect {
            x: -1.0,
            y: 0.0,
            width: 1.0,
            height: 1.0,
        },
        RasterRedactionRect {
            x: 3.5,
            y: 0.0,
            width: 1.0,
            height: 1.0,
        },
        RasterRedactionRect {
            x: 0.0,
            y: 0.0,
            width: 0.0,
            height: 1.0,
        },
        RasterRedactionRect {
            x: f32::NAN,
            y: 0.0,
            width: 1.0,
            height: 1.0,
        },
    ] {
        assert!(redact_raster_pages(vec![page(vec![rect])]).is_err());
    }
    let mut malformed = page(vec![RasterRedactionRect {
        x: 0.0,
        y: 0.0,
        width: 1.0,
        height: 1.0,
    }]);
    malformed.rgb.pop();
    assert!(redact_raster_pages(vec![malformed])
        .unwrap_err()
        .contains("byte count"));
}

#[test]
fn displayed_page_contract_maps_non_square_pixels_without_axis_flip() {
    let page = RasterRedactionPage {
        page_width: 200.0,
        page_height: 100.0,
        image_width: 4,
        image_height: 4,
        rgb: vec![255; 48],
        rectangles: vec![RasterRedactionRect {
            x: 100.0,
            y: 0.0,
            width: 100.0,
            height: 25.0,
        }],
    };
    let result = redact_raster_pages(vec![page]).unwrap();
    assert_eq!(result.covered_pixels, 2);
    let rgb = decoded_image(&result.bytes);
    assert_eq!(&rgb[0..6], &[255; 6]);
    assert_eq!(&rgb[6..12], &[0; 6]);
    assert_eq!(&rgb[12..], &[255; 36]);
}
