//! The PDF engine boundary (helper step 2).
//!
//! Everything the pdf-worker in `service.rs` asks PDFium to do goes through [`PdfEngine`] (load a
//! document) and [`EngineDocument`] (everything done with a loaded document). The only
//! implementation today is [`InProcessEngine`], which runs PDFium in this process exactly as the
//! worker did before the boundary existed. A later step adds a second implementation that forwards
//! the same calls to the memory-capped helper process, so every method takes and returns owned,
//! plain data (page plans, sizes, strings, DTOs), never a PDFium handle.
//!
//! Bitmaps the caller keeps are rendered into a caller-provided buffer
//! ([`EngineDocument::render_bgra_into`]) so a shared-memory section can stand in for the buffer
//! later; the caller computes and validates the pixel size before asking.

use std::ops::ControlFlow;
use std::path::PathBuf;
use pdfium_render::prelude::*;
use serde::Serialize;
use crate::editor::PageSpec;

#[derive(Clone, Serialize)]
pub struct PageSize { pub(crate) width: f32, pub(crate) height: f32 }

/// A failed load. `Display` is the engine's own message, unchanged.
#[derive(Debug)]
pub(crate) struct LoadError { password: bool, message: String }
impl LoadError {
    /// The document is encrypted and the password (or no password) was not accepted.
    pub(crate) fn is_password(&self) -> bool { self.password }
}
impl std::fmt::Display for LoadError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { formatter.write_str(&self.message) }
}
impl From<PdfiumError> for LoadError {
    fn from(error: PdfiumError) -> Self {
        Self { password: matches!(error, PdfiumError::PdfiumLibraryInternalError(PdfiumInternalError::PasswordError)), message: error.to_string() }
    }
}

/// Which page a call addresses: a page of the current plan (source page with the plan's crop and
/// extra rotation applied for the duration of the call), or a source page as stored in the file.
#[derive(Clone, Copy)]
pub(crate) enum PageTarget<'s> { Planned(&'s PageSpec), Source(usize) }

/// A failed text read; each variant carries the engine's message unchanged.
#[derive(Debug)]
pub(crate) enum TextError { Page(String), Text(String), Bounds(String) }
impl From<TextError> for String {
    fn from(error: TextError) -> Self { match error { TextError::Page(message) | TextError::Text(message) | TextError::Bounds(message) => message } }
}

/// A viewer render, PNG-encoded, with the bitmap's pixel size.
pub(crate) struct RenderedPng { pub(crate) png: Vec<u8>, pub(crate) width: u32, pub(crate) height: u32 }

/// One outline entry as the engine reads it; the worker maps it onto the current page plan.
pub(crate) struct EngineBookmark {
    pub(crate) title: Option<String>,
    pub(crate) depth: usize,
    pub(crate) page_index: Option<usize>,
    pub(crate) destination: Option<crate::bookmark_destinations::BookmarkDestination>,
}

pub(crate) trait PdfEngine {
    type Document<'a>: EngineDocument where Self: 'a;
    fn load_from_vec(&self, bytes: Vec<u8>, password: Option<&str>) -> Result<Self::Document<'_>, LoadError>;
    fn load_from_slice<'a>(&'a self, bytes: &'a [u8], password: Option<&str>) -> Result<Self::Document<'a>, LoadError>;
}

pub(crate) trait EngineDocument {
    fn page_count(&self) -> usize;
    /// Every page's size in points; refuses empty, oversized (> 65,536 pages) and degenerate documents.
    fn page_sizes(&self) -> Result<Vec<PageSize>, String>;
    /// No security handler (not encrypted, no permission restrictions).
    fn is_unprotected(&self) -> bool;
    /// Displayed width and height in points of a planned page (crop and rotation applied).
    fn planned_page_size(&self, spec: &PageSpec) -> Result<(f32, f32), String>;
    /// The viewer render: `width` pixels wide, at most 5,000 high, annotations on, PNG-encoded.
    fn render_viewer_png(&self, spec: &PageSpec, width: i32) -> Result<RenderedPng, String>;
    /// Text inside the page's bounding box.
    fn visible_text(&self, target: PageTarget<'_>) -> Result<String, TextError>;
    fn text_geometry(&self, target: PageTarget<'_>, id: u64, index: u16, revision: u64) -> Result<crate::text_geometry::PageTextGeometry, String>;
    /// Visits up to `limit` outline entries in document order until `visit` breaks or fails.
    fn bookmarks(&self, limit: usize, visit: &mut dyn FnMut(EngineBookmark) -> Result<ControlFlow<()>, String>) -> Result<(), String>;
    /// The destination view of a planned page (for bookmarks whose destination needs one).
    fn bookmark_page_view(&self, spec: &PageSpec) -> Result<Option<crate::bookmark_destinations::DestinationView>, String>;
    fn properties(&self, source_size_bytes: usize, current_page_dimensions: &[(f32, f32)]) -> Result<crate::document_properties::DocumentProperties, String>;
    #[cfg(test)]
    fn engine_page_labels(&self) -> Vec<Option<String>>;
}

/// PDFium in this process.
pub(crate) struct InProcessEngine { pdfium: Pdfium }
impl InProcessEngine {
    pub(crate) fn bind(library: PathBuf) -> Result<Self, String> {
        Pdfium::bind_to_library(library).map(Pdfium::new).map(|pdfium| Self { pdfium }).map_err(|e| format!("PDF engine could not start: {e}"))
    }
}
impl PdfEngine for InProcessEngine {
    type Document<'a> = InProcessDocument<'a>;
    fn load_from_vec(&self, bytes: Vec<u8>, password: Option<&str>) -> Result<InProcessDocument<'_>, LoadError> {
        Ok(InProcessDocument { inner: self.pdfium.load_pdf_from_byte_vec(bytes, password)? })
    }
    fn load_from_slice<'a>(&'a self, bytes: &'a [u8], password: Option<&str>) -> Result<InProcessDocument<'a>, LoadError> {
        Ok(InProcessDocument { inner: self.pdfium.load_pdf_from_byte_slice(bytes, password)? })
    }
}

pub(crate) struct InProcessDocument<'a> { inner: PdfDocument<'a> }

// Transitional while the request groups move behind the boundary one commit at a time; removed
// once every group is migrated.
impl<'a> std::ops::Deref for InProcessDocument<'a> {
    type Target = PdfDocument<'a>;
    fn deref(&self) -> &PdfDocument<'a> { &self.inner }
}

impl InProcessDocument<'_> {
    fn with_target<T>(&self, target: PageTarget<'_>, action: impl FnOnce(&PdfPage<'_>) -> T) -> Result<T, String> {
        match target {
            PageTarget::Planned(spec) => with_planned_page(&self.inner, spec, |page| Ok(action(page))),
            PageTarget::Source(index) => { let page = self.inner.pages().get(index as i32).map_err(|error| error.to_string())?; Ok(action(&page)) }
        }
    }
}

impl EngineDocument for InProcessDocument<'_> {
    fn page_count(&self) -> usize { self.inner.pages().len() as usize }
    fn page_sizes(&self) -> Result<Vec<PageSize>, String> {
        let pages = self.inner.pages();
        if pages.is_empty() { return Err("This PDF has no pages.".into()); }
        if pages.len() > u16::MAX as i32 + 1 { return Err("This PDF exceeds the supported limit of 65,536 pages.".into()); }
        (0..pages.len()).map(|index| {
            let page = pages.get(index).map_err(|error| format!("Unable to read page {}: {error}", index + 1))?;
            let size = PageSize { width: page.width().value, height: page.height().value };
            if !size.width.is_finite() || !size.height.is_finite() || size.width <= 0.0 || size.height <= 0.0 { return Err(format!("Invalid dimensions on page {}.", index + 1)); }
            Ok(size)
        }).collect()
    }
    fn is_unprotected(&self) -> bool { matches!(self.inner.permissions().security_handler_revision(), Ok(PdfSecurityHandlerRevision::Unprotected)) }
    fn planned_page_size(&self, spec: &PageSpec) -> Result<(f32, f32), String> {
        with_planned_page(&self.inner, spec, |page| Ok((page.width().value, page.height().value)))
    }
    fn render_viewer_png(&self, spec: &PageSpec, width: i32) -> Result<RenderedPng, String> {
        let image = with_planned_page(&self.inner, spec, |page| {
            let bitmap = page.render_with_config(&PdfRenderConfig::new().set_target_width(width).set_maximum_height(5000).render_annotations(true)).map_err(|e| e.to_string())?;
            bitmap.as_image().map_err(|e| e.to_string())
        })?;
        let mut bytes = std::io::Cursor::new(Vec::new());
        image.write_to(&mut bytes, image::ImageFormat::Png).map_err(|e| e.to_string())?;
        Ok(RenderedPng { png: bytes.into_inner(), width: image.width(), height: image.height() })
    }
    fn visible_text(&self, target: PageTarget<'_>) -> Result<String, TextError> {
        self.with_target(target, |page| {
            let text = page.text().map_err(|e| TextError::Text(e.to_string()))?;
            let visible = page.boundaries().bounding().map_err(|e| TextError::Bounds(e.to_string()))?.bounds;
            Ok(text.inside_rect(visible))
        }).map_err(TextError::Page)?
    }
    fn text_geometry(&self, target: PageTarget<'_>, id: u64, index: u16, revision: u64) -> Result<crate::text_geometry::PageTextGeometry, String> {
        self.with_target(target, |page| crate::text_geometry::inspect(page, id, index, revision))?
    }
    fn bookmarks(&self, limit: usize, visit: &mut dyn FnMut(EngineBookmark) -> Result<ControlFlow<()>, String>) -> Result<(), String> {
        let mut depths = std::collections::HashMap::new();
        for bookmark in self.inner.bookmarks().iter().take(limit) {
            let depth = bookmark.parent().and_then(|parent| depths.get(&parent).copied()).map_or(0, |depth: usize| depth + 1);
            depths.insert(bookmark.clone(), depth);
            let target = bookmark.destination();
            let page_index = target.as_ref().and_then(|destination| destination.page_index().ok()).map(|source| source as usize);
            let destination = target.map(|target| crate::bookmark_destinations::parse(target.view_settings()));
            if visit(EngineBookmark { title: bookmark.title(), depth, page_index, destination })?.is_break() { break; }
        }
        Ok(())
    }
    fn bookmark_page_view(&self, spec: &PageSpec) -> Result<Option<crate::bookmark_destinations::DestinationView>, String> {
        with_planned_page(&self.inner, spec, |planned| {
            let bounds = planned.boundaries().bounding().or_else(|_| planned.boundaries().crop()).or_else(|_| planned.boundaries().media()).map(|boundary| boundary.bounds);
            let (left, bottom) = bounds.map_or((0.0, 0.0), |bounds| (bounds.left().value, bounds.bottom().value));
            Ok(planned.rotation().ok().and_then(|rotation| crate::bookmark_destinations::page_view(left, bottom, planned.width().value, planned.height().value, rotation.as_degrees())))
        })
    }
    fn properties(&self, source_size_bytes: usize, current_page_dimensions: &[(f32, f32)]) -> Result<crate::document_properties::DocumentProperties, String> {
        crate::document_properties::inspect(&self.inner, source_size_bytes, current_page_dimensions)
    }
    #[cfg(test)]
    fn engine_page_labels(&self) -> Vec<Option<String>> { self.inner.pages().iter().map(|page| page.label().map(str::to_owned)).collect() }
}

/// Runs `action` on a source page with the plan's crop and extra quarter turns applied, then
/// restores the page's own crop box and rotation.
pub(crate) fn with_planned_page<T>(document: &PdfDocument<'_>, spec: &PageSpec, action: impl FnOnce(&PdfPage<'_>) -> Result<T, String>) -> Result<T, String> {
    let mut page = document.pages().get(spec.source as i32).map_err(|error| error.to_string())?;
    let original_rotation = page.rotation().map_err(|error| error.to_string())?;
    let original_crop = if spec.crop.is_some() {
        Some(page.boundaries().crop().or_else(|_| page.boundaries().bounding()).map_err(|error| error.to_string())?.bounds)
    } else { None };
    if let Some(crop) = spec.crop { page.boundaries_mut().set_crop(PdfRect::new_from_values(crop.bottom, crop.left, crop.top, crop.right)).map_err(|error| error.to_string())?; }
    let turns = (original_rotation.as_degrees() as i32 / 90 + spec.turns).rem_euclid(4);
    page.set_rotation(match turns { 1 => PdfPageRenderRotation::Degrees90, 2 => PdfPageRenderRotation::Degrees180, 3 => PdfPageRenderRotation::Degrees270, _ => PdfPageRenderRotation::None });
    let result = action(&page);
    let restore = match original_crop { Some(crop) => page.boundaries_mut().set_crop(crop).map_err(|error| error.to_string()), None => Ok(()) };
    page.set_rotation(original_rotation);
    result.and_then(|value| restore.map(|_| value))
}
