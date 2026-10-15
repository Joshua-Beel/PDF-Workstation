#[path = "../src/image_edit.rs"]
mod image_edit;
#[path = "../src/text_edit.rs"]
mod text_edit;

use lopdf::{content::{Content, Operation}, dictionary, Dictionary, Document, Object, Stream};

fn text_fixture(text: &str) -> Vec<u8> {
    let mut d = Document::with_version("1.7");
    let pages = d.new_object_id();
    let font = d.add_object(dictionary! { "Type" => "Font", "Subtype" => "Type1", "BaseFont" => "Helvetica", "Encoding" => "WinAnsiEncoding" });
    let content = Content { operations: vec![
        Operation::new("BT", vec![]),
        Operation::new("Tf", vec![Object::Name(b"F1".to_vec()), 12.into()]),
        Operation::new("Tm", vec![1.into(), 0.into(), 0.into(), 1.into(), 20.into(), 50.into()]),
        Operation::new("Tj", vec![Object::string_literal(text)]),
        Operation::new("ET", vec![]),
    ] }.encode().unwrap();
    let stream = d.add_object(Stream::new(Dictionary::new(), content));
    let page = d.add_object(dictionary! { "Type" => "Page", "Parent" => pages, "MediaBox" => vec![0.into(), 0.into(), 200.into(), 100.into()], "Resources" => dictionary! { "Font" => dictionary! { "F1" => font } }, "Contents" => stream });
    d.objects.insert(pages, dictionary! { "Type" => "Pages", "Count" => 1, "Kids" => vec![Object::Reference(page)] }.into());
    let root = d.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages });
    d.trailer.set("Root", root);
    d.reference_table.cross_reference_type = lopdf::xref::XrefType::CrossReferenceTable;
    d.trailer.remove(b"Type");
    let mut bytes = Vec::new();
    d.save_to(&mut bytes).unwrap();
    bytes
}

fn image_fixture() -> Vec<u8> {
    let mut pdf = Document::with_version("1.7");
    let pages = pdf.new_object_id();
    let mut image = Stream::new(dictionary! { "Type" => "XObject", "Subtype" => "Image", "Width" => 2, "Height" => 1, "ColorSpace" => "DeviceRGB", "BitsPerComponent" => 8 }, vec![255, 0, 0, 0, 255, 0]);
    image.compress().unwrap();
    let image_id = pdf.add_object(image);
    let content = Content { operations: vec![
        Operation::new("q", vec![]),
        Operation::new("cm", vec![200.into(), 0.into(), 0.into(), 100.into(), 0.into(), 0.into()]),
        Operation::new("Do", vec![Object::Name(b"Image".to_vec())]),
        Operation::new("Q", vec![]),
    ] }.encode().unwrap();
    let contents = pdf.add_object(Stream::new(Dictionary::new(), content));
    let page = pdf.add_object(dictionary! { "Type" => "Page", "Parent" => pages, "MediaBox" => vec![0.into(), 0.into(), 200.into(), 100.into()], "Resources" => dictionary! { "XObject" => dictionary! { "Image" => image_id } }, "Contents" => contents });
    pdf.objects.insert(pages, dictionary! { "Type" => "Pages", "Count" => 1, "Kids" => vec![Object::Reference(page)] }.into());
    let catalog = pdf.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages });
    pdf.trailer.set("Root", catalog);
    let mut bytes = Vec::new();
    pdf.save_to(&mut bytes).unwrap();
    bytes
}

// Writes the strict-profile text and image fixtures that scripts/verify-native-annotate-dialogs.ps1 opens.
// Run: cargo test --test dialog_fixtures -- --ignored (with SM3_DIALOG_FIXTURE_DIR set to an existing folder).
#[test]
#[ignore]
fn write_dialog_fixtures() {
    let dir = std::path::PathBuf::from(std::env::var("SM3_DIALOG_FIXTURE_DIR").expect("set SM3_DIALOG_FIXTURE_DIR"));
    let text = text_fixture("12345");
    assert_eq!(text_edit::inspect_text_run(&text, 1).unwrap().text, "12345");
    let image = image_fixture();
    let target = image_edit::inspect_flat_raster_image(&image, 1).unwrap();
    assert_eq!((target.pixel_width, target.pixel_height), (2, 1));
    std::fs::write(dir.join("strict-text.pdf"), text).unwrap();
    std::fs::write(dir.join("strict-image.pdf"), image).unwrap();
}
