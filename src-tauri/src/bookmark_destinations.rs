use pdfium_render::prelude::*;
use serde::Serialize;

const COORDINATE_LIMIT: f32 = 1_000_000.0;
const MIN_ZOOM: f32 = 0.1;
const MAX_ZOOM: f32 = 4.0;

#[derive(Serialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DestinationMode { Top, Xyz, Fit, FitH, FitV, FitR, FitB, FitBh, FitBv }

#[derive(Serialize, Clone, Copy, Debug, PartialEq)]
pub struct DestinationView { pub left: f32, pub bottom: f32, pub width: f32, pub height: f32, pub rotation: u16 }

#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct BookmarkDestination {
    pub mode: DestinationMode,
    pub x: Option<f32>,
    pub y: Option<f32>,
    pub rect: Option<[f32; 4]>,
    pub zoom: Option<f32>,
    pub view: Option<DestinationView>,
}

impl BookmarkDestination {
    pub fn top() -> Self { Self { mode: DestinationMode::Top, x: None, y: None, rect: None, zoom: None, view: None } }
    pub fn needs_view(&self) -> bool { self.x.is_some() || self.y.is_some() || self.rect.is_some() }
    pub fn without_position(self) -> Self { Self::top() }
}

fn coordinate(value: Option<PdfPoints>) -> Option<f32> {
    value.map(|points| points.value).filter(|value| value.is_finite()).map(|value| value.clamp(-COORDINATE_LIMIT, COORDINATE_LIMIT))
}

fn zoom(value: Option<f32>) -> Option<f32> {
    value.filter(|value| value.is_finite() && *value > 0.0).map(|value| value.clamp(MIN_ZOOM, MAX_ZOOM))
}

fn with(mode: DestinationMode, x: Option<f32>, y: Option<f32>) -> BookmarkDestination {
    BookmarkDestination { mode, x, y, rect: None, zoom: None, view: None }
}

pub fn parse(settings: Result<PdfDestinationViewSettings, PdfiumError>) -> BookmarkDestination {
    use DestinationMode::*;
    let Ok(settings) = settings else { return BookmarkDestination::top(); };
    match settings {
        PdfDestinationViewSettings::Unknown => BookmarkDestination::top(),
        PdfDestinationViewSettings::SpecificCoordinatesAndZoom(x, y, z) => BookmarkDestination { zoom: zoom(z), ..with(Xyz, coordinate(x), coordinate(y)) },
        PdfDestinationViewSettings::FitPageToWindow => with(Fit, None, None),
        PdfDestinationViewSettings::FitPageHorizontallyToWindow(y) => with(FitH, None, coordinate(y)),
        PdfDestinationViewSettings::FitPageVerticallyToWindow(x) => with(FitV, coordinate(x), None),
        PdfDestinationViewSettings::FitPageToRectangle(rect) => {
            let values = [coordinate(Some(rect.left())), coordinate(Some(rect.bottom())), coordinate(Some(rect.right())), coordinate(Some(rect.top()))];
            let [Some(left), Some(bottom), Some(right), Some(top)] = values else { return BookmarkDestination::top(); };
            BookmarkDestination { rect: Some([left.min(right), bottom.min(top), left.max(right), bottom.max(top)]), ..with(FitR, None, None) }
        }
        PdfDestinationViewSettings::FitBoundsToWindow => with(FitB, None, None),
        PdfDestinationViewSettings::FitBoundsHorizontallyToWindow(y) => with(FitBh, None, coordinate(y)),
        PdfDestinationViewSettings::FitBoundsVerticallyToWindow(x) => with(FitBv, coordinate(x), None),
    }
}

pub fn page_view(left: f32, bottom: f32, display_width: f32, display_height: f32, rotation_degrees: f32) -> Option<DestinationView> {
    if ![left, bottom, display_width, display_height, rotation_degrees].iter().all(|value| value.is_finite()) { return None; }
    if display_width <= 0.0 || display_height <= 0.0 || display_width > COORDINATE_LIMIT || display_height > COORDINATE_LIMIT { return None; }
    let rotation = (((rotation_degrees.round() as i64) % 360 + 360) % 360) as u16;
    if rotation % 90 != 0 { return None; }
    let (width, height) = if rotation % 180 == 0 { (display_width, display_height) } else { (display_height, display_width) };
    Some(DestinationView { left: left.clamp(-COORDINATE_LIMIT, COORDINATE_LIMIT), bottom: bottom.clamp(-COORDINATE_LIMIT, COORDINATE_LIMIT), width, height, rotation })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn p(value: f32) -> PdfPoints { PdfPoints::new(value) }

    #[test]
    fn every_view_mode_maps_to_its_own_mode_and_keeps_only_the_values_it_has() {
        let xyz = parse(Ok(PdfDestinationViewSettings::SpecificCoordinatesAndZoom(Some(p(10.0)), None, Some(1.5))));
        assert_eq!((xyz.mode, xyz.x, xyz.y, xyz.zoom), (DestinationMode::Xyz, Some(10.0), None, Some(1.5)));
        assert_eq!(parse(Ok(PdfDestinationViewSettings::FitPageToWindow)), with(DestinationMode::Fit, None, None));
        assert_eq!(parse(Ok(PdfDestinationViewSettings::FitPageHorizontallyToWindow(Some(p(700.0))))), with(DestinationMode::FitH, None, Some(700.0)));
        assert_eq!(parse(Ok(PdfDestinationViewSettings::FitPageVerticallyToWindow(Some(p(20.0))))), with(DestinationMode::FitV, Some(20.0), None));
        assert_eq!(parse(Ok(PdfDestinationViewSettings::FitBoundsToWindow)).mode, DestinationMode::FitB);
        assert_eq!(parse(Ok(PdfDestinationViewSettings::FitBoundsHorizontallyToWindow(None))), with(DestinationMode::FitBh, None, None));
        assert_eq!(parse(Ok(PdfDestinationViewSettings::FitBoundsVerticallyToWindow(Some(p(5.0))))), with(DestinationMode::FitBv, Some(5.0), None));
        let rect = parse(Ok(PdfDestinationViewSettings::FitPageToRectangle(PdfRect::new_from_values(400.0, 300.0, 100.0, 50.0))));
        assert_eq!((rect.mode, rect.rect), (DestinationMode::FitR, Some([50.0, 100.0, 300.0, 400.0])));
        assert!(rect.needs_view() && !parse(Ok(PdfDestinationViewSettings::FitPageToWindow)).needs_view());
    }

    #[test]
    fn malformed_values_clamp_or_fall_back_to_the_page_top() {
        assert_eq!(parse(Ok(PdfDestinationViewSettings::Unknown)), BookmarkDestination::top());
        assert_eq!(parse(Err(PdfiumError::PdfDestinationViewInvalidParameters)), BookmarkDestination::top());
        let huge = parse(Ok(PdfDestinationViewSettings::SpecificCoordinatesAndZoom(Some(p(f32::MAX)), Some(p(f32::NEG_INFINITY)), Some(f32::MAX))));
        assert_eq!((huge.x, huge.y, huge.zoom), (Some(1_000_000.0), None, Some(4.0)));
        for bad in [0.0, -2.0, f32::NAN, f32::INFINITY] {
            assert_eq!(parse(Ok(PdfDestinationViewSettings::SpecificCoordinatesAndZoom(None, None, Some(bad)))).zoom, None, "zoom {bad}");
        }
        assert_eq!(parse(Ok(PdfDestinationViewSettings::SpecificCoordinatesAndZoom(None, None, Some(0.0001)))).zoom, Some(0.1));
        assert_eq!(parse(Ok(PdfDestinationViewSettings::FitPageHorizontallyToWindow(Some(p(f32::NAN))))), with(DestinationMode::FitH, None, None));
        let nan_rect = parse(Ok(PdfDestinationViewSettings::FitPageToRectangle(PdfRect::new_from_values(f32::NAN, 0.0, 10.0, 10.0))));
        assert_eq!(nan_rect, BookmarkDestination::top());
    }

    #[test]
    fn page_view_swaps_dimensions_for_rotated_pages_and_rejects_bad_geometry() {
        let upright = page_view(10.0, 20.0, 612.0, 792.0, 0.0).unwrap();
        assert_eq!((upright.width, upright.height, upright.rotation), (612.0, 792.0, 0));
        let turned = page_view(0.0, 0.0, 792.0, 612.0, 90.0).unwrap();
        assert_eq!((turned.width, turned.height, turned.rotation), (612.0, 792.0, 90));
        assert_eq!(page_view(0.0, 0.0, 612.0, 792.0, -90.0).unwrap().rotation, 270);
        assert_eq!(page_view(0.0, 0.0, 612.0, 792.0, 540.0).unwrap().rotation, 180);
        for bad in [page_view(f32::NAN, 0.0, 612.0, 792.0, 0.0), page_view(0.0, 0.0, 0.0, 792.0, 0.0), page_view(0.0, 0.0, 612.0, -1.0, 0.0), page_view(0.0, 0.0, 612.0, 792.0, 45.0), page_view(0.0, 0.0, 2.0e6, 792.0, 0.0)] {
            assert!(bad.is_none());
        }
    }
}
