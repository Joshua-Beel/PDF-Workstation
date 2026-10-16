//! SM7 adversarial evidence for raster redaction.
//!
//! Every fixture is built here from literal PDF syntax; no external document is
//! read. Each vector is built twice: once with `SECRET` and once with the secret
//! replaced by spaces (the control). The redaction pipeline below mirrors the
//! `Request::RasterRedact` worker path in `service.rs` (same 150 DPI page sizes,
//! same PDFium render flags, same `bgra_to_rgb`, `redact_raster_pages` and
//! `verify_clean_raster_pdf`) on an unedited document, without the session's
//! page plan or app-owned comment overlay.
//!
//! A vector is clean when the output bytes (raw and every decompressed stream)
//! do not contain the secret in any common encoding, PDFium extracts no text
//! from the output, and the output pixels (decoded image and PDFium re-render)
//! are identical to the control run, so the secret contributed no ink.
//!
//! The on-screen viewer renders with `render_annotations(true)` and no
//! `FPDF_PRINTING`; redaction renders with `use_print_quality(true)`, which sets
//! `FPDF_PRINTING`. Tests named `documents_leak_*` assert the current leaking
//! behaviour so the evidence reruns wherever the full native suite runs: there
//! is no per-push CI, but `scripts/run-native-recovery-gates.ps1` runs every
//! test target (`cargo test --locked`) in the manual Windows validation, the
//! tag-triggered release and the manual signed-OCR workflows. When SM2 fixes the
//! leak these tests fail and must be flipped to `assert_clean`.
//! `worker_render_path_matches_this_replica` fails if `service.rs` drifts from
//! the flags, DPI or conversion copied here.

#[allow(dead_code)]
#[path = "../src/page_image.rs"]
mod page_image;
#[allow(dead_code)]
#[path = "../src/raster_redaction.rs"]
mod raster_redaction;
#[allow(dead_code)]
#[path = "../src/sanitization.rs"]
mod sanitization;

use lopdf::{Document, Object};
use page_image::PageImageFormat;
use pdfium_render::prelude::{PdfBitmapFormat, PdfColor, PdfPage, PdfRenderConfig, Pdfium};
use raster_redaction::{redact_raster_pages, RasterRedactionPage, RasterRedactionRect};
use sanitization::verify_clean_raster_pdf;
use std::path::PathBuf;
use std::sync::OnceLock;

const SECRET: &str = "SM7SECRET42";
const BOX: RasterRedactionRect = RasterRedactionRect { x: 5.0, y: 25.0, width: 160.0, height: 45.0 };
const UNDER_BOX: &str = "BT /F1 12 Tf 20 150 Td";
const OUTSIDE_BOX: &str = "BT /F1 12 Tf 20 60 Td";
const PAGE_TWO: &str = "BT /F1 12 Tf 20 100 Td (page two) Tj ET";

fn pdfium() -> &'static Pdfium {
    static PDFIUM: OnceLock<Pdfium> = OnceLock::new();
    PDFIUM.get_or_init(|| {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        Pdfium::new(Pdfium::bind_to_library(root.join("resources/pdfium/bin/pdfium.dll")).unwrap())
    })
}

fn control() -> String {
    " ".repeat(SECRET.len())
}

fn stream(dict: &str, data: &[u8]) -> Vec<u8> {
    let mut bytes = format!("<< {dict} /Length {} >>\nstream\n", data.len()).into_bytes();
    bytes.extend_from_slice(data);
    bytes.extend_from_slice(b"\nendstream");
    bytes
}

fn obj(text: impl Into<String>) -> Vec<u8> {
    text.into().into_bytes()
}

struct Assembled {
    bytes: Vec<u8>,
    startxref: usize,
    size: u32,
}

fn assemble(objects: &[Vec<u8>], trailer_extra: &str) -> Assembled {
    let mut bytes = b"%PDF-1.7\n%\xE2\xE3\xCF\xD3\n".to_vec();
    let mut offsets = Vec::with_capacity(objects.len());
    for (index, body) in objects.iter().enumerate() {
        offsets.push(bytes.len());
        bytes.extend_from_slice(format!("{} 0 obj\n", index + 1).as_bytes());
        bytes.extend_from_slice(body);
        bytes.extend_from_slice(b"\nendobj\n");
    }
    let startxref = bytes.len();
    let size = objects.len() as u32 + 1;
    bytes.extend_from_slice(format!("xref\n0 {size}\n0000000000 65535 f \n").as_bytes());
    for offset in offsets {
        bytes.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    bytes.extend_from_slice(format!("trailer\n<< /Size {size} /Root 1 0 R {trailer_extra} >>\nstartxref\n{startxref}\n%%EOF\n").as_bytes());
    Assembled { bytes, startxref, size }
}

fn append_update(base: Assembled, updates: &[(u32, Vec<u8>)]) -> Vec<u8> {
    let mut bytes = base.bytes;
    let mut entries = Vec::new();
    let mut size = base.size;
    for (number, body) in updates {
        entries.push((*number, bytes.len()));
        bytes.extend_from_slice(format!("{number} 0 obj\n").as_bytes());
        bytes.extend_from_slice(body);
        bytes.extend_from_slice(b"\nendobj\n");
        size = size.max(number + 1);
    }
    let startxref = bytes.len();
    bytes.extend_from_slice(b"xref\n");
    for (number, offset) in entries {
        bytes.extend_from_slice(format!("{number} 1\n{offset:010} 00000 n \n").as_bytes());
    }
    bytes.extend_from_slice(format!("trailer\n<< /Size {size} /Root 1 0 R /Prev {} >>\nstartxref\n{startxref}\n%%EOF\n", base.startxref).as_bytes());
    bytes
}

struct PageDef {
    content: String,
    fonts: String,
    resources: String,
    extra: String,
}

fn page(content: impl Into<String>) -> PageDef {
    PageDef { content: content.into(), fonts: String::new(), resources: String::new(), extra: String::new() }
}

fn first_extra(pages: usize) -> u32 {
    4 + 2 * pages as u32
}

fn document_objects(pages: Vec<PageDef>, catalog_extra: &str, extras: Vec<Vec<u8>>) -> Vec<Vec<u8>> {
    let kids: Vec<String> = (0..pages.len()).map(|index| format!("{} 0 R", 4 + 2 * index)).collect();
    let mut objects = vec![
        obj(format!("<< /Type /Catalog /Pages 2 0 R {catalog_extra} >>")),
        obj(format!("<< /Type /Pages /Kids [{}] /Count {} >>", kids.join(" "), pages.len())),
        obj("<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>"),
    ];
    for (index, page) in pages.iter().enumerate() {
        objects.push(obj(format!(
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Resources << /Font << /F1 3 0 R {} >> {} >> /Contents {} 0 R {} >>",
            page.fonts,
            page.resources,
            5 + 2 * index,
            page.extra
        )));
        objects.push(stream("", page.content.as_bytes()));
    }
    objects.extend(extras);
    objects
}

fn document(pages: Vec<PageDef>, catalog_extra: &str, extras: Vec<Vec<u8>>, trailer_extra: &str) -> Vec<u8> {
    assemble(&document_objects(pages, catalog_extra, extras), trailer_extra).bytes
}

fn text_ap(secret: &str, width: u32, height: u32) -> Vec<u8> {
    stream(
        &format!("/Type /XObject /Subtype /Form /BBox [0 0 {width} {height}] /Resources << /Font << /F1 3 0 R >> >>"),
        format!("BT /F1 12 Tf 10 12 Td ({secret}) Tj ET").as_bytes(),
    )
}

fn render(page: &PdfPage, redaction_flags: bool) -> Vec<u8> {
    let dimensions = page_image::dimensions(page.width().value, page.height().value, 150, PageImageFormat::Png).unwrap();
    let mut config = PdfRenderConfig::new()
        .set_fixed_size(dimensions.width as i32, dimensions.height as i32)
        .set_format(PdfBitmapFormat::BGRA)
        .set_reverse_byte_order(false)
        .clear_before_rendering(true)
        .set_clear_color(PdfColor::WHITE)
        .render_annotations(true);
    if redaction_flags {
        config = config.render_form_data(true).use_print_quality(true);
    }
    let bitmap = page.render_with_config(&config).unwrap();
    page_image::bgra_to_rgb(dimensions, &bitmap.as_raw_bytes(), PageImageFormat::Png).unwrap()
}

struct Redacted {
    output: Vec<u8>,
    view: Vec<Vec<u8>>,
}

fn redact(source: &[u8], rectangles: &[RasterRedactionRect]) -> Redacted {
    let document = pdfium().load_pdf_from_byte_slice(source, None).unwrap();
    let mut pages = Vec::new();
    let mut view = Vec::new();
    for (index, page) in document.pages().iter().enumerate() {
        let (page_width, page_height) = (page.width().value, page.height().value);
        let dimensions = page_image::dimensions(page_width, page_height, 150, PageImageFormat::Png).unwrap();
        view.push(render(&page, false));
        pages.push(RasterRedactionPage {
            page_width,
            page_height,
            image_width: dimensions.width,
            image_height: dimensions.height,
            rgb: render(&page, true),
            rectangles: if index == 0 { rectangles.to_vec() } else { Vec::new() },
        });
    }
    let count = pages.len();
    let prepared = redact_raster_pages(pages).unwrap();
    verify_clean_raster_pdf(&prepared.bytes, count).unwrap();
    Redacted { output: prepared.bytes, view }
}

fn needles(secret: &str) -> Vec<Vec<u8>> {
    let utf16be: Vec<u8> = secret.encode_utf16().flat_map(|unit| unit.to_be_bytes()).collect();
    let utf16le: Vec<u8> = secret.encode_utf16().flat_map(|unit| unit.to_le_bytes()).collect();
    let hex_upper: String = secret.bytes().map(|byte| format!("{byte:02X}")).collect();
    let hex_lower = hex_upper.to_lowercase();
    vec![secret.as_bytes().to_vec(), utf16be, utf16le, hex_upper.into_bytes(), hex_lower.into_bytes()]
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|window| window == needle)
}

fn bytes_contain_secret(bytes: &[u8]) -> bool {
    let needles = needles(SECRET);
    if needles.iter().any(|needle| contains(bytes, needle)) {
        return true;
    }
    let Ok(document) = Document::load_mem(bytes) else { return false };
    document.objects.values().any(|object| match object {
        Object::Stream(stream) => {
            let data = stream.decompressed_content().unwrap_or_else(|_| stream.content.clone());
            needles.iter().any(|needle| contains(&data, needle))
        }
        _ => false,
    })
}

fn pdfium_text(bytes: &[u8]) -> String {
    let document = pdfium().load_pdf_from_byte_slice(bytes, None).unwrap();
    document.pages().iter().map(|page| page.text().unwrap().all()).collect::<Vec<_>>().join("\n")
}

fn output_images(bytes: &[u8]) -> Vec<Vec<u8>> {
    let document = Document::load_mem(bytes).unwrap();
    document
        .get_pages()
        .values()
        .map(|page_id| {
            let page = document.get_dictionary(*page_id).unwrap();
            let image_id = page.get(b"Resources").unwrap().as_dict().unwrap().get(b"XObject").unwrap().as_dict().unwrap().get(b"Image").unwrap().as_reference().unwrap();
            document.get_object(image_id).unwrap().as_stream().unwrap().decompressed_content().unwrap()
        })
        .collect()
}

fn rerender(bytes: &[u8]) -> Vec<Vec<u8>> {
    let document = pdfium().load_pdf_from_byte_slice(bytes, None).unwrap();
    document
        .pages()
        .iter()
        .map(|page| render(&page, false))
        .collect()
}

fn differing_pixels(left: &[Vec<u8>], right: &[Vec<u8>]) -> Vec<usize> {
    assert_eq!(left.len(), right.len());
    left.iter()
        .zip(right)
        .map(|(a, b)| {
            assert_eq!(a.len(), b.len());
            a.chunks_exact(3).zip(b.chunks_exact(3)).filter(|(x, y)| x != y).count()
        })
        .collect()
}

fn assert_output_structure(bytes: &[u8]) {
    let document = Document::load_mem(bytes).unwrap();
    let catalog = document.catalog().unwrap();
    let mut keys: Vec<&[u8]> = catalog.iter().map(|(key, _)| key.as_slice()).collect();
    keys.sort();
    assert_eq!(keys, vec![b"Pages".as_slice(), b"Type".as_slice()]);
    assert!(!document.trailer.has(b"Info"));
    let forbidden: [&[u8]; 18] = [
        b"Annots", b"AcroForm", b"OCProperties", b"OC", b"Outlines", b"Names", b"Dests", b"Metadata", b"StructTreeRoot",
        b"StructParents", b"JavaScript", b"JS", b"OpenAction", b"AA", b"EmbeddedFiles", b"EF", b"Thumb", b"Font",
    ];
    for object in document.objects.values() {
        let dict = match object {
            Object::Dictionary(dict) => dict,
            Object::Stream(stream) => &stream.dict,
            _ => continue,
        };
        for key in forbidden {
            assert!(!dict.has(key), "output retains /{}", String::from_utf8_lossy(key));
        }
    }
}

#[derive(Debug)]
struct Report {
    source_has_secret: bool,
    output_bytes_leak: bool,
    output_text_leak: bool,
    output_text_empty: bool,
    output_ink: Vec<usize>,
    rerender_ink: Vec<usize>,
    view_ink: Vec<usize>,
}

fn evaluate(name: &str, build: impl Fn(&str) -> Vec<u8>, rectangles: &[RasterRedactionRect]) -> Report {
    let source = build(SECRET);
    let control_source = build(&control());
    let secret_run = redact(&source, rectangles);
    let control_run = redact(&control_source, rectangles);
    assert_output_structure(&secret_run.output);
    let output_text = pdfium_text(&secret_run.output);
    let report = Report {
        source_has_secret: bytes_contain_secret(&source) || pdfium_text(&source).contains(SECRET),
        output_bytes_leak: bytes_contain_secret(&secret_run.output),
        output_text_leak: output_text.contains(SECRET),
        output_text_empty: output_text.trim().is_empty(),
        output_ink: differing_pixels(&output_images(&secret_run.output), &output_images(&control_run.output)),
        rerender_ink: differing_pixels(&rerender(&secret_run.output), &rerender(&control_run.output)),
        view_ink: differing_pixels(&secret_run.view, &control_run.view),
    };
    eprintln!("SM7-REDACTION {name}: {report:?}");
    assert!(report.source_has_secret, "{name}: fixture does not carry the secret");
    assert!(!report.output_bytes_leak, "{name}: secret bytes survive in the output");
    assert!(!report.output_text_leak && report.output_text_empty, "{name}: output has extractable text");
    report
}

fn assert_clean(report: &Report) {
    assert!(report.output_ink.iter().all(|count| *count == 0), "secret ink survives: {report:?}");
    assert!(report.rerender_ink.iter().all(|count| *count == 0), "secret ink survives re-render: {report:?}");
}

fn two_pages(first: PageDef, catalog_extra: &str, extras: Vec<Vec<u8>>, trailer_extra: &str) -> Vec<u8> {
    document(vec![first, page(PAGE_TWO)], catalog_extra, extras, trailer_extra)
}

#[test]
fn page_text_under_the_box_is_removed() {
    let report = evaluate("page_text_under_box", |s| two_pages(page(format!("{UNDER_BOX} ({s}) Tj ET")), "", vec![], ""), &[BOX]);
    assert!(report.view_ink[0] > 0, "the fixture secret must be visible before redaction");
    assert_clean(&report);
}

#[test]
fn text_covered_by_a_later_shape_outside_the_box_stays_covered() {
    let report = evaluate(
        "text_under_drawn_cover",
        |s| two_pages(page(format!("{OUTSIDE_BOX} ({s}) Tj ET 0 g 10 50 180 30 re f")), "", vec![], ""),
        &[BOX],
    );
    assert_eq!(report.view_ink, vec![0, 0]);
    assert_clean(&report);
}

#[test]
fn form_xobject_reused_on_an_unmarked_page_survives_as_visible_pixels() {
    let build = |s: &str| {
        let x = first_extra(2);
        let mut first = page("/X1 Do");
        first.resources = format!("/XObject << /X1 {x} 0 R >>");
        let mut second = page("/X1 Do");
        second.resources = first.resources.clone();
        let form = stream(
            "/Type /XObject /Subtype /Form /BBox [0 0 200 200] /Resources << /Font << /F1 3 0 R >> >>",
            format!("{UNDER_BOX} ({s}) Tj ET").as_bytes(),
        );
        document(vec![first, second], "", vec![form], "")
    };
    let report = evaluate("form_xobject_reused", build, &[BOX]);
    assert_eq!(report.output_ink[0], 0, "the marked page must be clean");
    assert!(report.output_ink[1] > 0 && report.view_ink[1] > 0, "by design: marks apply to one page, the reuse stays visible: {report:?}");
}

#[test]
fn freetext_annotation_under_the_box_is_flattened_then_removed() {
    let build = |s: &str| {
        let a = first_extra(2);
        let mut first = page("");
        first.extra = format!("/Annots [{a} 0 R]");
        let annot = obj(format!(
            "<< /Type /Annot /Subtype /FreeText /Rect [10 135 160 170] /F 4 /P 4 0 R /Contents ({s}) /DA (/Helv 12 Tf 0 g) /AP << /N {} 0 R >> >>",
            a + 1
        ));
        two_pages(first, "", vec![annot, text_ap(s, 150, 35)], "")
    };
    let report = evaluate("freetext_under_box", build, &[BOX]);
    assert!(report.view_ink[0] > 0);
    assert_clean(&report);
}

#[test]
fn text_annotation_contents_and_open_popup_outside_the_box() {
    let build = |s: &str| {
        let t = first_extra(2);
        let mut first = page("");
        first.extra = format!("/Annots [{t} 0 R {} 0 R]", t + 1);
        let text = obj(format!("<< /Type /Annot /Subtype /Text /Rect [20 20 40 40] /F 4 /P 4 0 R /T (author) /Contents ({s}) /Open true /Popup {} 0 R >>", t + 1));
        let popup = obj(format!("<< /Type /Annot /Subtype /Popup /Rect [60 10 190 80] /F 4 /P 4 0 R /Open true /Parent {t} 0 R >>"));
        two_pages(first, "", vec![text, popup], "")
    };
    let report = evaluate("text_annot_open_popup", build, &[BOX]);
    assert_eq!(report.view_ink, report.output_ink, "output must match what the viewer showed");
    assert_clean(&report);
}

fn widget(s: &str, flags: u32, rect: &str) -> Vec<u8> {
    let w = first_extra(2);
    let mut first = page("");
    first.extra = format!("/Annots [{w} 0 R]");
    let field = obj(format!(
        "<< /Type /Annot /Subtype /Widget /FT /Tx /T (f1) /V ({s}) /DA (/Helv 12 Tf 0 g) /Rect [{rect}] /F {flags} /P 4 0 R /AP << /N {} 0 R >> >>",
        w + 1
    ));
    two_pages(
        first,
        &format!("/AcroForm << /Fields [{w} 0 R] /DA (/Helv 0 Tf 0 g) /DR << /Font << /Helv 3 0 R >> >> >>"),
        vec![field, text_ap(s, 150, 35)],
        "",
    )
}

#[test]
fn form_field_value_and_appearance_under_the_box_are_removed() {
    let report = evaluate("widget_under_box", |s| widget(s, 4, "10 135 160 170"), &[BOX]);
    assert!(report.view_ink[0] > 0);
    assert_clean(&report);
}

#[test]
fn hidden_form_field_outside_the_box_is_not_rendered() {
    let report = evaluate("widget_hidden_flag", |s| widget(s, 2, "10 40 160 75"), &[BOX]);
    assert_eq!(report.view_ink, vec![0, 0]);
    assert_clean(&report);
}

#[test]
fn noview_print_form_field_outside_the_box_is_not_rendered() {
    // Unlike the FreeText case below, PDFium's form-field draw path skips this widget in
    // both the viewer and the FPDF_PRINTING redaction render.
    let report = evaluate("widget_noview_print", |s| widget(s, 36, "10 40 160 75"), &[BOX]);
    assert_eq!(report.view_ink, vec![0, 0], "the field is invisible in the viewer");
    assert_clean(&report);
}

#[test]
fn documents_leak_noview_print_annotation_hidden_on_screen_is_printed_into_output() {
    let build = |s: &str| {
        let a = first_extra(2);
        let mut first = page("");
        first.extra = format!("/Annots [{a} 0 R]");
        let annot = obj(format!(
            "<< /Type /Annot /Subtype /FreeText /Rect [10 40 160 75] /F 36 /P 4 0 R /Contents ({s}) /DA (/Helv 12 Tf 0 g) /AP << /N {} 0 R >> >>",
            a + 1
        ));
        two_pages(first, "", vec![annot, text_ap(s, 150, 35)], "")
    };
    let report = evaluate("annot_noview_print", build, &[BOX]);
    assert_eq!(report.view_ink, vec![0, 0], "the annotation is invisible in the viewer");
    // LEAK (SM7 2026-10-14, R-1): redaction renders with FPDF_PRINTING, so PDFium draws
    // annotations by the Print flag and ignores NoView; the viewer does the reverse.
    // Flip to assert_clean once fixed.
    assert!(report.output_ink[0] > 0 && report.rerender_ink[0] > 0, "{report:?}");
}

#[test]
fn info_dictionary_and_xmp_metadata_are_dropped() {
    let build = |s: &str| {
        let m = first_extra(2);
        let xmp = format!("<?xpacket begin=\"\" id=\"W5M0MpCehiHzreSzNTczkc9d\"?><x:xmpmeta xmlns:x=\"adobe:ns:meta/\"><rdf:RDF xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\"><rdf:Description xmlns:dc=\"http://purl.org/dc/elements/1.1/\"><dc:title>{s}</dc:title></rdf:Description></rdf:RDF></x:xmpmeta><?xpacket end=\"w\"?>");
        two_pages(
            page("BT /F1 12 Tf 20 100 Td (cover) Tj ET"),
            &format!("/Metadata {m} 0 R"),
            vec![stream("/Type /Metadata /Subtype /XML", xmp.as_bytes()), obj(format!("<< /Title ({s}) /Author ({s}) /Subject ({s}) >>"))],
            &format!("/Info {} 0 R", m + 1),
        )
    };
    assert_clean(&evaluate("info_and_xmp", build, &[BOX]));
}

#[test]
fn outline_titles_and_named_destinations_are_dropped() {
    let build = |s: &str| {
        let o = first_extra(2);
        two_pages(
            page("BT /F1 12 Tf 20 100 Td (cover) Tj ET"),
            &format!("/Outlines {o} 0 R /PageMode /UseOutlines /Names << /Dests << /Names [({s}) [4 0 R /Fit]] >> >> /Dests << /{} [6 0 R /Fit] >>", s.replace(' ', "_")),
            vec![
                obj(format!("<< /Type /Outlines /First {} 0 R /Last {} 0 R /Count 1 >>", o + 1, o + 1)),
                obj(format!("<< /Title ({s}) /Parent {o} 0 R /Dest [4 0 R /Fit] >>")),
            ],
            "",
        )
    };
    assert_clean(&evaluate("outline_and_dests", build, &[BOX]));
}

#[test]
fn embedded_files_and_file_attachment_annotations_are_dropped() {
    let build = |s: &str| {
        let a = first_extra(2);
        let mut first = page("BT /F1 12 Tf 20 100 Td (cover) Tj ET");
        first.extra = format!("/Annots [{a} 0 R]");
        two_pages(
            first,
            &format!("/Names << /EmbeddedFiles << /Names [({s}) {} 0 R] >> >>", a + 1),
            vec![
                obj(format!("<< /Type /Annot /Subtype /FileAttachment /Rect [20 20 40 40] /F 4 /P 4 0 R /Contents ({s}) /FS {} 0 R >>", a + 1)),
                obj(format!("<< /Type /Filespec /F ({s}.txt) /UF ({s}.txt) /Desc ({s}) /EF << /F {} 0 R >> >>", a + 2)),
                stream("/Type /EmbeddedFile", s.as_bytes()),
            ],
            "",
        )
    };
    assert_clean(&evaluate("embedded_file", build, &[BOX]));
}

#[test]
fn document_and_page_javascript_is_dropped_and_not_executed() {
    let build = |s: &str| {
        let j = first_extra(2);
        let mut first = page("BT /F1 12 Tf 20 100 Td (cover) Tj ET");
        first.extra = format!("/AA << /O << /S /JavaScript /JS (var p = \"{s}\";) >> >>");
        two_pages(
            first,
            &format!("/OpenAction << /S /JavaScript /JS (var s = \"{s}\";) >> /Names << /JavaScript << /Names [(doc) {j} 0 R] >> >>"),
            vec![obj(format!("<< /S /JavaScript /JS {} 0 R >>", j + 1)), stream("", format!("var t = '{s}';").as_bytes())],
            "",
        )
    };
    assert_clean(&evaluate("javascript", build, &[BOX]));
}

fn optional_content(s: &str, ocg_extra: &str, config_extra: &str) -> Vec<u8> {
    let o = first_extra(2);
    let mut first = page(format!("/OC /oc1 BDC {OUTSIDE_BOX} ({s}) Tj ET EMC"));
    first.resources = format!("/Properties << /oc1 {o} 0 R >>");
    two_pages(
        first,
        &format!("/OCProperties << /OCGs [{o} 0 R] /D << /Order [{o} 0 R] /OFF [{o} 0 R] {config_extra} >> >>"),
        vec![obj(format!("<< /Type /OCG /Name (Layer) {ocg_extra} >>"))],
        "",
    )
}

fn covered_by_screen_only_annotation(s: &str, subtype: &str, ap_content: &str, annot_extra: &str) -> Vec<u8> {
    let a = first_extra(2);
    let mut first = page(format!("{OUTSIDE_BOX} ({s}) Tj ET"));
    first.extra = format!("/Annots [{a} 0 R]");
    let annot = obj(format!(
        "<< /Type /Annot /Subtype /{subtype} /Rect [10 50 190 80] /F 0 /P 4 0 R {annot_extra} /AP << /N {} 0 R >> >>",
        a + 1
    ));
    let ap = stream(
        "/Type /XObject /Subtype /Form /BBox [0 0 180 30] /Resources << /Font << /F1 3 0 R >> >>",
        ap_content.as_bytes(),
    );
    two_pages(first, "", vec![annot, ap], "")
}

#[test]
fn documents_leak_opaque_square_annotation_without_print_flag_uncovers_text_in_output() {
    let report = evaluate(
        "cover_square_no_print",
        |s| covered_by_screen_only_annotation(s, "Square", "0 g 0 0 180 30 re f", "/IC [0 0 0] /C [0 0 0]"),
        &[BOX],
    );
    assert_eq!(report.view_ink, vec![0, 0], "the secret is covered in the viewer");
    // LEAK (SM7 2026-10-14, S-1, reverse of R-1): the cover has no Print flag, so the
    // FPDF_PRINTING redaction render drops it and the text it covered on screen is
    // rasterised, unmarked because it looked covered. Flip to assert_clean once fixed.
    assert!(report.output_ink[0] > 0 && report.rerender_ink[0] > 0, "{report:?}");
}

#[test]
fn documents_leak_opaque_freetext_annotation_without_print_flag_uncovers_text_in_output() {
    let report = evaluate(
        "cover_freetext_no_print",
        |s| covered_by_screen_only_annotation(s, "FreeText", "1 g 0 0 180 30 re f BT 0 g /F1 12 Tf 10 10 Td (approved) Tj ET", "/Contents (approved) /DA (/Helv 12 Tf 0 g)"),
        &[BOX],
    );
    assert_eq!(report.view_ink, vec![0, 0], "the secret is covered in the viewer");
    // LEAK (SM7 2026-10-14, S-1): same cause as the Square cover. Flip to assert_clean once fixed.
    assert!(report.output_ink[0] > 0 && report.rerender_ink[0] > 0, "{report:?}");
}

#[test]
fn documents_leak_view_only_optional_content_cover_uncovers_text_in_output() {
    let build = |s: &str| {
        let o = first_extra(2);
        let mut first = page(format!("{OUTSIDE_BOX} ({s}) Tj ET /OC /cover BDC 0 g 10 50 180 30 re f EMC"));
        first.resources = format!("/Properties << /cover {o} 0 R >>");
        two_pages(
            first,
            &format!("/OCProperties << /OCGs [{o} 0 R] /D << /Order [{o} 0 R] /ON [{o} 0 R] /AS [<< /Event /Print /OCGs [{o} 0 R] /Category [/Print] >> << /Event /View /OCGs [{o} 0 R] /Category [/View] >>] >> >>"),
            vec![obj("<< /Type /OCG /Name (Cover) /Usage << /Print << /PrintState /OFF >> /View << /ViewState /ON >> >> >>")],
            "",
        )
    };
    let report = evaluate("cover_ocg_view_only", build, &[BOX]);
    assert_eq!(report.view_ink, vec![0, 0], "the secret is covered in the viewer");
    // LEAK (SM7 2026-10-14, S-1): FPDF_PRINTING evaluates the cover layer's Print usage
    // (/PrintState /OFF), so the cover vanishes and the text is rasterised. Flip to assert_clean once fixed.
    assert!(report.output_ink[0] > 0 && report.rerender_ink[0] > 0, "{report:?}");
}

#[test]
fn worker_render_path_matches_this_replica() {
    let source = std::fs::read_to_string(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/service.rs")).unwrap();
    let engine = std::fs::read_to_string(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/pdf_engine.rs")).unwrap();
    let start = source.find("Request::RasterRedact(id, revision, page, rectangles, path, reply) =>").expect("RasterRedact worker arm");
    let end = start + source[start..].find("redact_raster_pages(raster_pages)").expect("redact_raster_pages call");
    let worker = &source[start..end];
    for expected in [
        "let document = note_document(engine, original, session, &mut redaction_note_documents, id)?;",
        "document.render_bgra_into(PageTarget::Planned(&raster.spec), RenderMode::Print, raster.dimensions.width, raster.dimensions.height, &mut bgra)",
        "crate::page_image::bgra_to_rgb(raster.dimensions, &bgra, PageImageFormat::Png)?",
        "rectangles: if index == usize::from(page) { rectangles.clone() } else { Vec::new() },",
    ] {
        assert!(worker.contains(expected), "RasterRedact drifted from the replica: {expected}");
    }
    let plan_start = source.find("fn checked_raster_redaction_request(").expect("plan function");
    let plan = &source[plan_start..plan_start + source[plan_start..].find("Ok(RasterRedactionPlan { pages })").expect("plan end")];
    assert!(plan.contains("crate::page_image::dimensions(page_width, page_height, 150, PageImageFormat::Png)?"), "redaction DPI drifted from 150");
    assert!(
        engine.contains(".set_format(PdfBitmapFormat::BGRA).set_reverse_byte_order(false).clear_before_rendering(true).set_clear_color(PdfColor::WHITE);")
            && engine.contains("RenderMode::Print => config.render_annotations(true).render_form_data(true).use_print_quality(true)"),
        "the engine's print render flags drifted from the replica"
    );
    assert!(
        engine.contains("page.render_with_config(&PdfRenderConfig::new().set_target_width(width).set_maximum_height(5000).render_annotations(true))"),
        "the viewer render flags drifted; the view_ink assertions model them"
    );
}

#[test]
fn hidden_optional_content_layer_is_not_rendered() {
    let report = evaluate("ocg_off", |s| optional_content(s, "", ""), &[BOX]);
    assert_eq!(report.view_ink, vec![0, 0]);
    assert_clean(&report);
}

#[test]
fn documents_leak_print_only_optional_content_hidden_on_screen_is_printed_into_output() {
    let build = |s: &str| {
        let o = first_extra(2);
        optional_content(
            s,
            "/Usage << /Print << /PrintState /ON >> /View << /ViewState /OFF >> >>",
            &format!("/AS [<< /Event /Print /OCGs [{o} 0 R] /Category [/Print] >> << /Event /View /OCGs [{o} 0 R] /Category [/View] >>]"),
        )
    };
    let report = evaluate("ocg_print_only", build, &[BOX]);
    assert_eq!(report.view_ink, vec![0, 0], "the layer is off in the viewer");
    // LEAK (SM7 2026-10-14): FPDF_PRINTING switches PDFium's optional-content usage to Print,
    // so a layer that is off on screen but /PrintState /ON is rasterised outside every mark.
    assert!(report.output_ink[0] > 0 && report.rerender_ink[0] > 0, "{report:?}");
}

#[test]
fn page_thumbnail_is_dropped() {
    let build = |s: &str| {
        let t = first_extra(2);
        let mut first = page("BT /F1 12 Tf 20 100 Td (cover) Tj ET");
        first.extra = format!("/Thumb {t} 0 R");
        two_pages(first, "", vec![stream(&format!("/Width {} /Height 1 /ColorSpace /DeviceGray /BitsPerComponent 8", s.len()), s.as_bytes())], "")
    };
    assert_clean(&evaluate("page_thumb", build, &[BOX]));
}

#[test]
fn earlier_incremental_revision_is_not_carried_forward() {
    let build = |s: &str| {
        let base = assemble(&document_objects(vec![page(format!("{OUTSIDE_BOX} ({s}) Tj ET")), page(PAGE_TWO)], "", vec![]), "");
        let content = base.size;
        append_update(
            base,
            &[
                (content, stream("", b"BT /F1 12 Tf 20 100 Td (revised) Tj ET")),
                (4, obj(format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Resources << /Font << /F1 3 0 R >> >> /Contents {content} 0 R >>"))),
            ],
        )
    };
    let report = evaluate("incremental_old_revision", build, &[BOX]);
    assert_eq!(report.view_ink, vec![0, 0], "only the latest revision is displayed");
    assert_clean(&report);
}

#[test]
fn structure_tree_alt_and_actual_text_are_dropped() {
    let build = |s: &str| {
        let r = first_extra(2);
        let mut first = page(format!("/Span << /ActualText ({s}) /MCID 0 >> BDC {OUTSIDE_BOX} (harmless) Tj ET EMC"));
        first.extra = "/StructParents 0".into();
        two_pages(
            first,
            &format!("/MarkInfo << /Marked true >> /StructTreeRoot {r} 0 R"),
            vec![
                obj(format!("<< /Type /StructTreeRoot /K {} 0 R /ParentTree {} 0 R >>", r + 1, r + 2)),
                obj(format!("<< /Type /StructElem /S /Span /P {r} 0 R /Pg 4 0 R /K 0 /Alt ({s}) /ActualText ({s}) >>")),
                obj(format!("<< /Nums [0 [{} 0 R]] >>", r + 1)),
            ],
            "",
        )
    };
    assert_clean(&evaluate("struct_tree_alt_actualtext", build, &[BOX]));
}

#[test]
fn tounicode_only_secret_is_dropped() {
    let build = |s: &str| {
        let f = first_extra(2);
        let chars: String = s
            .chars()
            .enumerate()
            .map(|(index, ch)| format!("<{:02X}> <{:04X}>\n", 0x61 + index, ch as u32))
            .collect();
        let cmap = format!(
            "/CIDInit /ProcSet findresource begin\n12 dict begin\nbegincmap\n/CIDSystemInfo << /Registry (Adobe) /Ordering (UCS) /Supplement 0 >> def\n/CMapName /Adobe-Identity-UCS def\n/CMapType 2 def\n1 begincodespacerange\n<00> <FF>\nendcodespacerange\n{} beginbfchar\n{chars}endbfchar\nendcmap\nCMapName currentdict /CMapResource defineresource pop\nend\nend",
            s.len()
        );
        let mut first = page("BT /F2 12 Tf 20 60 Td (abcdefghijk) Tj ET");
        first.fonts = format!("/F2 {f} 0 R");
        two_pages(first, "", vec![obj(format!("<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /ToUnicode {} 0 R >>", f + 1)), stream("", cmap.as_bytes())], "")
    };
    let source = build(SECRET);
    assert!(!bytes_contain_secret(&source), "the secret must exist only through ToUnicode");
    assert!(pdfium_text(&source).contains(SECRET), "PDFium must extract the ToUnicode secret from the source");
    assert_clean(&evaluate("tounicode_only", build, &[BOX]));
}

#[test]
fn invisible_render_mode_text_is_not_rendered() {
    let report = evaluate("tr3_invisible", |s| two_pages(page(format!("BT 3 Tr /F1 12 Tf 20 60 Td ({s}) Tj ET")), "", vec![], ""), &[BOX]);
    assert_eq!(report.view_ink, vec![0, 0]);
    assert_clean(&report);
}

#[test]
fn white_on_white_text_leaves_no_ink() {
    let report = evaluate("white_on_white", |s| two_pages(page(format!("BT 1 g /F1 12 Tf 20 60 Td ({s}) Tj ET")), "", vec![], ""), &[BOX]);
    assert_eq!(report.view_ink, vec![0, 0]);
    assert_clean(&report);
}

#[test]
fn tiny_text_outside_the_box_survives_as_the_same_pixels_the_viewer_showed() {
    let report = evaluate("tiny_1pt", |s| two_pages(page(format!("BT /F1 1 Tf 20 60 Td ({s}) Tj ET")), "", vec![], ""), &[BOX]);
    // Residual, not hidden: 1 pt text is a few 150 DPI pixels, also drawn on screen.
    assert!(report.view_ink[0] > 0 && report.output_ink[0] > 0, "{report:?}");
    assert!(report.output_ink[0] < 200, "{report:?}");
}

#[test]
fn rotated_page_box_in_displayed_coordinates_covers_the_secret() {
    let build = |s: &str| {
        let mut first = page(format!("{UNDER_BOX} ({s}) Tj ET"));
        first.extra = "/Rotate 90".into();
        two_pages(first, "", vec![], "")
    };
    let rect = RasterRedactionRect { x: 135.0, y: 10.0, width: 35.0, height: 110.0 };
    let report = evaluate("rotate_90", build, &[rect]);
    assert!(report.view_ink[0] > 0);
    assert_clean(&report);
}

#[test]
fn cropped_page_box_in_displayed_coordinates_covers_the_secret() {
    let build = |s: &str| {
        let mut first = page(format!("BT /F1 12 Tf 60 150 Td ({s}) Tj ET"));
        first.extra = "/CropBox [50 50 200 200]".into();
        two_pages(first, "", vec![], "")
    };
    let rect = RasterRedactionRect { x: 5.0, y: 30.0, width: 120.0, height: 35.0 };
    let report = evaluate("cropbox_offset", build, &[rect]);
    assert!(report.view_ink[0] > 0);
    assert_clean(&report);
}

#[test]
fn unmarked_pages_are_rasterised_and_document_channels_are_absent() {
    let source = two_pages(page(format!("{UNDER_BOX} ({SECRET}) Tj ET")), "", vec![], "");
    let run = redact(&source, &[BOX]);
    let document = Document::load_mem(&run.output).unwrap();
    assert_eq!(document.get_pages().len(), 2);
    for page_number in 1..=2 {
        assert_eq!(document.extract_text(&[page_number]).unwrap(), "");
    }
    assert!(pdfium_text(&run.output).trim().is_empty());
    assert_output_structure(&run.output);
    let images = output_images(&run.output);
    let source_view = run.view[1].clone();
    assert_eq!(images[1], source_view, "the unmarked page is a full-page raster of its rendering");
}
