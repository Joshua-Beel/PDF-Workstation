use pdfium_render::prelude::*;
use serde::Serialize;
use crate::editor::CropBox;

pub(crate) const MAX_CHARACTERS: usize = 20_000;
const COORDINATE_SIZE: i32 = 1_000_000;

#[derive(Debug, Serialize)]
pub struct TextBounds { pub x: f32, pub y: f32, pub width: f32, pub height: f32 }
#[derive(Debug, Serialize)]
pub struct TextCharacter { pub text: String, pub bounds: Option<TextBounds>, pub angle: u16 }
#[derive(Debug, Serialize)]
pub struct PageTextGeometry {
    pub id: u64,
    pub page: u16,
    pub revision: u64,
    pub status: &'static str,
    pub truncated: bool,
    pub characters: Vec<TextCharacter>,
    pub reason: Option<String>,
}

fn cardinal_angle(matrix: PdfMatrix) -> Option<u16> {
    let (a, b, c, d) = (matrix.a(), matrix.b(), matrix.c(), matrix.d());
    if ![a, b, c, d].iter().all(|value| value.is_finite()) { return None; }
    let x = a.hypot(b);
    let y = c.hypot(d);
    if x <= 0.0 || y <= 0.0 || a * d - b * c <= 0.0 { return None; }
    if (a * c + b * d).abs() > x * y * 0.00001 { return None; }
    let angle = b.atan2(a).to_degrees().rem_euclid(360.0);
    let nearest = (angle / 90.0).round() * 90.0;
    if (angle - nearest).abs() > 0.001 { return None; }
    Some((nearest as u16) % 360)
}

fn normalized_bounds(corners: [(i32, i32); 4]) -> Result<Option<TextBounds>, &'static str> {
    let left = corners.iter().map(|point| point.0).min().unwrap() as f32 / COORDINATE_SIZE as f32;
    let right = corners.iter().map(|point| point.0).max().unwrap() as f32 / COORDINATE_SIZE as f32;
    let top = corners.iter().map(|point| point.1).min().unwrap() as f32 / COORDINATE_SIZE as f32;
    let bottom = corners.iter().map(|point| point.1).max().unwrap() as f32 / COORDINATE_SIZE as f32;
    if right <= left || bottom <= top { return Err("Text has invalid bounds."); }
    if right <= 0.0 || bottom <= 0.0 || left >= 1.0 || top >= 1.0 { return Ok(None); }
    let x = left.max(0.0);
    let y = top.max(0.0);
    Ok(Some(TextBounds { x, y, width: right.min(1.0) - x, height: bottom.min(1.0) - y }))
}

pub struct InspectedGeometry { pub geometry: PageTextGeometry, pub(crate) source_bounds: Vec<Option<CropBox>> }
impl InspectedGeometry {
    pub fn highlight_bounds(&self, start: usize, end: usize) -> Result<Vec<CropBox>, String> {
        if self.geometry.status != "ok" || self.geometry.truncated { return Err(self.geometry.reason.clone().unwrap_or_else(|| "This page does not support text highlights.".into())); }
        if start >= end || end > self.geometry.characters.len() { return Err("Select a valid character range on one current page.".into()); }
        let bounds = self.source_bounds[start..end].iter().flatten().copied().collect::<Vec<_>>();
        if bounds.is_empty() { return Err("The selected range has no positioned characters.".into()); }
        if bounds.len() > crate::comments::MAX_HIGHLIGHT_QUADS { return Err("A text highlight is limited to 256 positioned characters. Select a shorter range.".into()); }
        Ok(bounds)
    }
}
pub fn inspect(page: &PdfPage<'_>, id: u64, index: u16, revision: u64) -> Result<PageTextGeometry, String> {
    Ok(inspect_with_source(page, id, index, revision)?.geometry)
}
pub fn inspect_with_source(page: &PdfPage<'_>, id: u64, index: u16, revision: u64) -> Result<InspectedGeometry, String> {
    let mut result = PageTextGeometry { id, page: index, revision, status: "ok", truncated: false, characters: Vec::new(), reason: None };
    let mut source_bounds = Vec::new();
    let extraction = (|| -> Result<(), String> {
        let width = page.width().value;
        let height = page.height().value;
        if !width.is_finite() || !height.is_finite() || width <= 0.0 || height <= 0.0 { return Err("The page has invalid dimensions.".into()); }
        let rotation = page.rotation().map_err(|error| error.to_string())?.as_degrees() as u16;
        let visible = page.boundaries().bounding().map_err(|error| error.to_string())?.bounds;
        let config = PdfRenderConfig::new().set_fixed_size(COORDINATE_SIZE, COORDINATE_SIZE);
        let text = page.text().map_err(|error| error.to_string())?;
        let count = text.len().max(0) as usize;
        result.truncated = count > MAX_CHARACTERS;
        if result.truncated {
            result.reason = Some(format!("This page exceeds the selection limit of {MAX_CHARACTERS} characters."));
            return Ok(());
        }
        let chars = text.chars();
        result.characters.reserve(count.min(MAX_CHARACTERS));
        for index in 0..count.min(MAX_CHARACTERS) {
            let character = chars.get(index).map_err(|error| error.to_string())?;
            let value = character.unicode_char().ok_or("Text contains an unsupported character mapping.")?;
            if value.is_control() && !matches!(value, '\r' | '\n' | '\t') { return Err("Text contains unsupported control characters.".into()); }
            if matches!(value, '\r' | '\n' | '\t') {
                result.characters.push(TextCharacter { text: value.to_string(), bounds: None, angle: rotation });
                source_bounds.push(None);
                continue;
            }
            let source_angle = cardinal_angle(character.matrix().map_err(|error| error.to_string())?).ok_or("Selection does not support skewed, mirrored, or angled text on this page.")?;
            let angle = (rotation + 360 - source_angle) % 360;
            let bounds = character.loose_bounds().map_err(|error| error.to_string())?;
            let (left, bottom, right, top) = (bounds.left().value, bounds.bottom().value, bounds.right().value, bounds.top().value);
            if ![left, bottom, right, top].iter().all(|value| value.is_finite()) || right < left || top < bottom { return Err("Text has invalid bounds.".into()); }
            if right == left || top == bottom {
                if value.is_whitespace() { result.characters.push(TextCharacter { text: value.to_string(), bounds: None, angle }); source_bounds.push(None); continue; }
                return Err("Text has invalid bounds.".into());
            }
            let mut corners = [(0, 0); 4];
            for (position, (x, y)) in [(left, bottom), (left, top), (right, bottom), (right, top)].into_iter().enumerate() {
                corners[position] = page.points_to_pixels(PdfPoints::new(x), PdfPoints::new(y), &config).map_err(|error| error.to_string())?;
            }
            if let Some(bounds) = normalized_bounds(corners).map_err(str::to_string)? {
                result.characters.push(TextCharacter { text: value.to_string(), bounds: Some(bounds), angle });
                let clipped = CropBox { left: left.max(visible.left().value), bottom: bottom.max(visible.bottom().value), right: right.min(visible.right().value), top: top.min(visible.top().value) };
                if clipped.right <= clipped.left || clipped.top <= clipped.bottom { return Err("Text bounds do not agree with the visible page.".into()); }
                source_bounds.push(Some(clipped));
            }
        }
        Ok(())
    })();
    if let Err(reason) = extraction {
        result.status = "unsupported";
        result.characters.clear();
        source_bounds.clear();
        result.truncated = false;
        result.reason = Some(reason);
    }
    Ok(InspectedGeometry { geometry: result, source_bounds })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    #[test]
    fn text_highlights_range_indices_whitespace_unsupported_and_quad_cap() {
        let bounds = CropBox { left: 10.0, bottom: 10.0, right: 20.0, top: 30.0 };
        let mut inspected = InspectedGeometry { geometry: PageTextGeometry { id: 1, page: 0, revision: 3, status: "ok", truncated: false, reason: None, characters: (0..258).map(|index| TextCharacter { text: if index == 0 { "😀" } else if index == 1 { "\n" } else { "A" }.into(), bounds: if index == 1 { None } else { Some(TextBounds { x: 0.1, y: 0.1, width: 0.1, height: 0.1 }) }, angle: 0 }).collect() }, source_bounds: (0..258).map(|index| if index == 1 { None } else { Some(bounds) }).collect() };
        assert_eq!(inspected.highlight_bounds(0, 2).unwrap(), vec![bounds]); assert!(inspected.highlight_bounds(1, 2).is_err()); assert_eq!(inspected.highlight_bounds(0, 257).unwrap().len(), 256);
        for (start, end) in [(0, 258), (4, 3), (2, 2), (257, 259), (usize::MAX, usize::MAX)] { assert!(inspected.highlight_bounds(start, end).is_err()); }
        inspected.geometry.truncated = true; assert!(inspected.highlight_bounds(0, 1).is_err()); inspected.geometry.truncated = false; inspected.geometry.status = "unsupported"; assert!(inspected.highlight_bounds(0, 1).is_err());
    }

    pub(crate) fn fixture(rotation: i64, matrix: [f32; 6], text: &str) -> Vec<u8> {
        use lopdf::{dictionary, content::{Content, Operation}, Object, Stream};
        let mut document = lopdf::Document::with_version("1.7");
        let pages = document.new_object_id();
        let font = document.add_object(dictionary! { "Type" => "Font", "Subtype" => "Type1", "BaseFont" => "Helvetica", "Encoding" => "WinAnsiEncoding" });
        let mut operations = vec![
            Operation::new("BT", vec![]),
            Operation::new("Tf", vec![Object::Name(b"F1".to_vec()), 24.into()]),
            Operation::new("Tm", matrix.into_iter().map(Object::Real).collect()),
        ];
        for (line, text) in text.split('\n').enumerate() {
            if line > 0 { operations.push(Operation::new("Td", vec![0.into(), (-36).into()])); }
            let bytes: Vec<_> = text.chars().map(|character| { assert!((character as u32) <= 255); character as u8 }).collect();
            operations.push(Operation::new("Tj", vec![Object::string_literal(bytes)]));
        }
        operations.push(Operation::new("ET", vec![]));
        let content = Content { operations }.encode().unwrap();
        let content = document.add_object(Stream::new(dictionary! {}, content));
        let page = document.add_object(dictionary! { "Type" => "Page", "Parent" => pages, "MediaBox" => vec![0.into(), 0.into(), 400.into(), 400.into()], "CropBox" => vec![50.into(), 70.into(), 350.into(), 330.into()], "Rotate" => rotation, "Resources" => dictionary! { "Font" => dictionary! { "F1" => font } }, "Contents" => content });
        document.objects.insert(pages, dictionary! { "Type" => "Pages", "Kids" => vec![Object::Reference(page)], "Count" => 1 }.into());
        let catalog = document.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages });
        document.trailer.set("Root", catalog);
        let mut bytes = Vec::new(); document.save_to(&mut bytes).unwrap(); bytes
    }

    #[test]
    fn accepts_only_cardinal_unskewed_unmirrored_glyphs() {
        for (matrix, angle) in [
            (PdfMatrix::new(2.0, 0.0, 0.0, 3.0, 0.0, 0.0), 0),
            (PdfMatrix::new(0.0, 2.0, -3.0, 0.0, 0.0, 0.0), 90),
            (PdfMatrix::new(-2.0, 0.0, 0.0, -3.0, 0.0, 0.0), 180),
            (PdfMatrix::new(0.0, -2.0, 3.0, 0.0, 0.0, 0.0), 270),
        ] { assert_eq!(cardinal_angle(matrix), Some(angle)); }
        for matrix in [PdfMatrix::new(1.0, 0.0, 0.2, 1.0, 0.0, 0.0), PdfMatrix::new(-1.0, 0.0, 0.0, 1.0, 0.0, 0.0), PdfMatrix::new(1.0, 1.0, -1.0, 1.0, 0.0, 0.0), PdfMatrix::new(f32::NAN, 0.0, 0.0, 1.0, 0.0, 0.0)] { assert_eq!(cardinal_angle(matrix), None); }
    }

    #[test]
    fn clips_cropped_bounds_and_rejects_degenerate_bounds() {
        let bounds = normalized_bounds([(-100_000, 100_000), (-100_000, 300_000), (200_000, 100_000), (200_000, 300_000)]).unwrap().unwrap();
        assert_eq!(bounds.x, 0.0); assert_eq!(bounds.width, 0.2);
        assert!(normalized_bounds([(0, 0); 4]).is_err());
        assert!(normalized_bounds([(1_100_000, 0), (1_100_000, 100_000), (1_200_000, 0), (1_200_000, 100_000)]).unwrap().is_none());
    }
}
