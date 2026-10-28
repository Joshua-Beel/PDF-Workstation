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

use std::collections::{HashMap, HashSet};
use std::ops::ControlFlow;
use std::path::PathBuf;
use pdfium_render::prelude::*;
use serde::Serialize;
use crate::editor::{CropBox, PageSpec};

#[derive(Clone, Serialize)]
pub struct PageSize { pub(crate) width: f32, pub(crate) height: f32 }

/// A failed load. `Display` is the engine's own message, unchanged.
#[derive(Debug)]
pub(crate) struct LoadError { password: bool, message: String }
impl LoadError {
    /// The document is encrypted and the password (or no password) was not accepted.
    pub(crate) fn is_password(&self) -> bool { self.password }
    /// A load error reported by another engine implementation (the helper process).
    pub(crate) fn new(password: bool, message: String) -> Self { Self { password, message } }
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

/// Render flags for [`EngineDocument::render_bgra_into`]. Both clear to white first.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RenderMode {
    /// Annotations, form data and print quality (print, page-image export, OCR input).
    Print,
    /// Exactly the viewer's flags ([`EngineDocument::render_viewer_png`]): annotations and form data
    /// as displayed, no `FPDF_PRINTING`, so optional content and annotation flags resolve as on
    /// screen. Raster redaction uses it so the copy shows what the user saw and marked.
    Viewer,
    /// Page content only (output validation, pixel comparisons).
    Plain,
}

/// A failed BGRA render. `Page`: the page could not be prepared; `Render`: the engine refused the
/// render; `Format`: the bitmap format could not be read; `Layout`: the bitmap does not match the
/// requested size or format. Messages are the engine's own.
#[derive(Debug)]
pub(crate) enum RenderError { Page(String), Render(String), Format(String), Layout }

/// A failed output probe ([`EngineDocument::thumbnail_check`]).
#[derive(Debug)]
pub(crate) enum ProbeError { Read(String), Render(String) }

/// `width * height * 4`, the BGRA buffer size for a render.
pub(crate) fn bgra_len(width: u32, height: u32) -> Option<usize> {
    usize::try_from(u64::from(width) * u64::from(height) * 4).ok()
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
    /// The user's "Try again" after the engine stopped itself (helper crash loop). No-op in process.
    fn reset_after_stop(&self) {}
}

pub(crate) trait EngineDocument {
    fn page_count(&self) -> usize;
    /// Every page's size in points; refuses empty, oversized (> 65,536 pages) and degenerate documents.
    fn page_sizes(&self) -> Result<Vec<PageSize>, String>;
    /// No security handler (not encrypted, no permission restrictions).
    fn is_unprotected(&self) -> bool;
    /// PDFium's own copy/extract permission (`FPDF_GetDocPermissions` bit 5) under the security handler
    /// PDFium chose, read once at load. The text gate requires this and lopdf's view to agree (see `service.rs`).
    fn text_extraction_allowed(&self) -> bool;
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
    /// Text geometry plus the source-space bounds a text highlight needs.
    fn text_geometry_with_source(&self, target: PageTarget<'_>, id: u64, index: u16, revision: u64) -> Result<crate::text_geometry::InspectedGeometry, String>;
    /// Maps a rectangle given as fractions of the displayed planned page to a source-space crop box.
    fn displayed_crop(&self, spec: &PageSpec, rect: crate::service::CropRect) -> Result<CropBox, String>;
    /// Maps source-space rectangles to fractions of the displayed planned page; `None` where one is not visible.
    fn displayed_note_rects(&self, spec: &PageSpec, rects: &[CropBox]) -> Result<Vec<Option<crate::comments::DisplayRect>>, String>;
    /// The engine's own reading of every form field the parser found, keyed by field name, or `None`
    /// when the engine disagrees with the parser's structure.
    fn form_values(&self, fields: &[crate::forms::FormField]) -> Option<HashMap<String, Option<String>>>;
    /// Size in points of a source page as stored (no plan applied).
    fn source_page_size(&self, index: usize) -> Result<(f32, f32), String>;
    /// Renders a source page into a 64-pixel box; `Ok(false)` when the bitmap is empty.
    fn thumbnail_check(&self, index: usize) -> Result<bool, ProbeError>;
    /// Renders a page at exactly `width` x `height` pixels, BGRA, into `out`, which must be exactly
    /// [`bgra_len`] bytes (otherwise `Layout`). The engine never chooses the buffer size.
    fn render_bgra_into(&self, target: PageTarget<'_>, mode: RenderMode, width: u32, height: u32, out: &mut [u8]) -> Result<(), RenderError>;
    /// The user's "Try again" for a document the engine set aside after a failure. No-op in process.
    fn retry(&self) {}
}

/// PDFium in this process.
/// `raw` is a second binding to the same loaded library, kept only to read the raw permission bits that
/// pdfium-render does not expose (it reports bit 10, not bit 5, for revision 3 and later). It must be taken
/// before `Pdfium::new`, which refuses any later binding.
pub(crate) struct InProcessEngine { pdfium: Pdfium, raw: Box<dyn PdfiumLibraryBindings> }
impl InProcessEngine {
    pub(crate) fn bind(library: PathBuf) -> Result<Self, String> {
        let start = |error: PdfiumError| format!("PDF engine could not start: {error}");
        let raw = Pdfium::bind_to_library(&library).map_err(start)?;
        let pdfium = Pdfium::bind_to_library(library).map(Pdfium::new).map_err(start)?;
        Ok(Self { pdfium, raw })
    }
    fn document<'a>(&self, inner: PdfDocument<'a>) -> InProcessDocument<'a> {
        let handle = self.raw.get_handle_from_document(&inner);
        let bits = unsafe { self.raw.FPDF_GetDocPermissions(handle) } as u64;
        InProcessDocument { inner, text_extraction_allowed: extraction_bits_allow(bits) }
    }
}

/// `/P` bits are numbered from 1 (PDF 32000-1 table 22), so bit 5 is mask 0x10 and bit 10 is mask 0x200.
/// Bit 5 (0x10, copy or extract text and graphics), the bit lopdf's `COPYABLE` reads, has that meaning in every
/// standard handler revision (R2-R6), so the revision (`FPDF_GetSecurityHandlerRevision`) does not change the rule:
/// without an `/Encrypt` dictionary (revision -1) or when the owner password unlocked the file PDFium reports every
/// bit; otherwise the bits are the `/P` of the `/Encrypt` dictionary PDFium resolved. Bit 10 (0x200, extraction for
/// accessibility, R3+), which pdfium-render's `can_extract_text_and_graphics` reads, is not enough.
pub(crate) fn extraction_bits_allow(bits: u64) -> bool { bits & 0x10 != 0 }

impl PdfEngine for InProcessEngine {
    type Document<'a> = InProcessDocument<'a>;
    fn load_from_vec(&self, bytes: Vec<u8>, password: Option<&str>) -> Result<InProcessDocument<'_>, LoadError> {
        Ok(self.document(self.pdfium.load_pdf_from_byte_vec(bytes, password)?))
    }
    fn load_from_slice<'a>(&'a self, bytes: &'a [u8], password: Option<&str>) -> Result<InProcessDocument<'a>, LoadError> {
        Ok(self.document(self.pdfium.load_pdf_from_byte_slice(bytes, password)?))
    }
}

/// A document loaded in this process. Its PDFium handle is private to this module: the worker can
/// reach it only through [`EngineDocument`].
pub(crate) struct InProcessDocument<'a> { inner: PdfDocument<'a>, text_extraction_allowed: bool }

impl InProcessDocument<'_> {
    fn with_target<T>(&self, target: PageTarget<'_>, action: impl FnOnce(&PdfPage<'_>) -> T) -> Result<T, String> {
        match target {
            PageTarget::Planned(spec) => with_planned_page(&self.inner, spec, |page| Ok(action(page))),
            PageTarget::Source(index) => { let page = self.inner.pages().get(index as i32).map_err(|error| format!("Unable to read page {}: {error}", index + 1))?; Ok(action(&page)) }
        }
    }

    /// A viewer render of a planned page at exactly `width` x `height` with the viewer's flags (annotations
    /// and form data as displayed), written in RGBA order (`FPDF_REVERSE_BYTE_ORDER`, as the viewer render's
    /// own bitmap is), into `out` of exactly [`bgra_len`] bytes. The helper renders viewer pages this way into
    /// an app-owned section so the app can PNG-encode them without a channel swap.
    pub(crate) fn render_viewer_rgba_into(&self, spec: &PageSpec, width: u32, height: u32, out: &mut [u8]) -> Result<(), RenderError> {
        if bgra_len(width, height) != Some(out.len()) { return Err(RenderError::Layout); }
        with_planned_page(&self.inner, spec, |page| {
            let settings = PdfRenderConfig::new().set_fixed_size(width as i32, height as i32).set_format(PdfBitmapFormat::BGRA).set_reverse_byte_order(true).clear_before_rendering(true).set_clear_color(PdfColor::WHITE).render_annotations(true);
            let rendered = PdfBitmap::from_bytes(width as i32, height as i32, PdfBitmapFormat::BGRA, out)
                .map_err(|error| RenderError::Render(error.to_string()))
                .and_then(|mut bitmap| page.render_into_bitmap_with_config(&mut bitmap, &settings).map(|()| (bitmap.width(), bitmap.height())).map_err(|error| RenderError::Render(error.to_string())));
            Ok(rendered.and_then(|size| if size == (width as i32, height as i32) { Ok(()) } else { Err(RenderError::Layout) }))
        }).map_err(RenderError::Page)?
    }
}

impl EngineDocument for InProcessDocument<'_> {
    fn page_count(&self) -> usize { self.inner.pages().len() as usize }
    fn page_sizes(&self) -> Result<Vec<PageSize>, String> {
        let pages = self.inner.pages();
        if pages.is_empty() { return Err("This PDF has no pages.".into()); }
        if pages.len() > u16::MAX as i32 + 1 { return Err("This PDF exceeds the supported limit of 65,536 pages.".into()); }
        // FPDF_GetPageSizeByIndexF reads each page's size (rotation applied) without loading the page.
        (0..pages.len()).map(|index| {
            let page = pages.page_size(index).map_err(|error| format!("Unable to read page {}: {error}", index + 1))?;
            let size = PageSize { width: page.width().value, height: page.height().value };
            if !size.width.is_finite() || !size.height.is_finite() || size.width <= 0.0 || size.height <= 0.0 { return Err(format!("Invalid dimensions on page {}.", index + 1)); }
            Ok(size)
        }).collect()
    }
    fn is_unprotected(&self) -> bool { matches!(self.inner.permissions().security_handler_revision(), Ok(PdfSecurityHandlerRevision::Unprotected)) }
    fn text_extraction_allowed(&self) -> bool { self.text_extraction_allowed }
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
        let mut depths = HashMap::new();
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
    fn text_geometry_with_source(&self, target: PageTarget<'_>, id: u64, index: u16, revision: u64) -> Result<crate::text_geometry::InspectedGeometry, String> {
        self.with_target(target, |page| crate::text_geometry::inspect_with_source(page, id, index, revision))?
    }
    fn displayed_crop(&self, spec: &PageSpec, rect: crate::service::CropRect) -> Result<CropBox, String> {
        with_planned_page(&self.inner, spec, |page| displayed_crop(page, rect))
    }
    fn displayed_note_rects(&self, spec: &PageSpec, rects: &[CropBox]) -> Result<Vec<Option<crate::comments::DisplayRect>>, String> {
        with_planned_page(&self.inner, spec, |page| rects.iter().map(|rect| displayed_note_rect(page, *rect)).collect())
    }
    fn form_values(&self, fields: &[crate::forms::FormField]) -> Option<HashMap<String, Option<String>>> {
        let document = &self.inner;
        document.form()?;
        let expected=fields.iter().map(|field|(field.name.as_str(),field)).collect::<HashMap<_,_>>();
        let mut values=HashMap::new();
        let mut radio_indices:HashMap<&str,HashSet<usize>>=HashMap::new();
        for page in document.pages().iter() {
            for annotation in page.annotations().iter() {
                if let Some(field)=annotation.as_form_field() {
                    let name=field.name()?;let expected=*expected.get(name.as_str())?;
                    if let Some(choice)=&expected.choice {
                        let(options,combo_value)=if choice.presentation=="dropdown"{let actual=field.as_combo_box_field()?;if actual.has_editable_text_box()!=choice.editable||actual.is_multiselect(){return None;}(actual.options(),Some(actual.value()))}else{let actual=field.as_list_box_field()?;if actual.is_multiselect()!=choice.multiple{return None;}(actual.options(),None)};
                        if options.len()!=choice.options.len(){return None;}
                        for(index,option)in choice.options.iter().enumerate(){let actual=options.get(index).ok()?;if actual.index()!=index||actual.label().map(String::as_str)!=Some(option.label.as_str())||actual.is_set()!=(if choice.multiple{choice.selected_option_ids.contains(&option.option_id)}else{choice.selected_option_id.as_ref()==Some(&option.option_id)}){return None;}}
                        // The value comes from what the engine reports as selected, not from the expected field. PDFium's combo API reports the
                        // selected option's label; typed text that matches no option cannot be read through it, so a custom combo value must read
                        // as nothing selected and its text is the one value taken from the expected field.
                        let selected:Vec<&crate::forms::ChoiceOption>=choice.options.iter().enumerate().filter(|(index,_)|options.get(*index).ok().is_some_and(|actual|actual.is_set())).map(|(_,option)|option).collect();
                        if let Some(read)=combo_value{if read.as_deref()!=selected.first().map(|option|option.label.as_str()){return None;}}
                        let value=if selected.is_empty()&&!choice.text.is_empty(){expected.value.clone()}else{selected.iter().map(|option|option.export.as_str()).collect::<Vec<_>>().join("\n")};
                        if values.insert(name,Some(value)).is_some(){return None;}continue;
                    }
                    if let Some(radio)=&expected.radio {
                        let actual=field.as_radio_button_field()?;let index=actual.index_in_group() as usize;let option=radio.options.get(index)?;
                        if !radio_indices.entry(expected.name.as_str()).or_default().insert(index){return None;}
                        let group_value=actual.group_value();let matches=if radio.selected_option_id.is_none(){group_value.as_deref().is_none_or(|value|value.is_empty()||value=="Off")}else{group_value.as_deref()==Some(expected.value.as_str())};
                        // The convenience fallback reports Off == Off as checked for blank groups.
                        // Their canonical parser proof requires every AS Off; verify the engine's
                        // blank group value and full child bijection instead. Selected groups still
                        // require every engine checked state to agree with the selected child.
                        if !matches || radio.selected_option_id.is_some() && actual.is_checked().ok()? != (radio.selected_option_id.as_ref()==Some(&option.option_id)){return None;}
                        values.insert(name,Some(expected.value.clone()));continue;
                    }
                    let value=if expected.checked.is_some() {
                        // pdfium-render 0.9.4 is_checked() recognizes only Yes. Read the actual
                        // engine value and compare with the exact state validated in the source AP.
                        let actual=field.as_checkbox_field()?.group_value()?;
                        let checked=if actual=="Off" {false} else if actual==*expected.on_state.as_ref()? {true} else {return None;};
                        Some(checked.to_string())
                    } else {field.as_text_field()?.value()};
                    if values.insert(name,value).is_some() {return None;}
                }
            }
        }
        if fields.iter().filter_map(|field|field.radio.as_ref().map(|radio|(field.name.as_str(),radio.options.len()))).any(|(name,count)|radio_indices.get(name).is_none_or(|indices|indices.len()!=count)){return None;}
        Some(values)
    }
    fn source_page_size(&self, index: usize) -> Result<(f32, f32), String> {
        let page = self.inner.pages().get(index as i32).map_err(|error| error.to_string())?;
        Ok((page.width().value, page.height().value))
    }
    fn thumbnail_check(&self, index: usize) -> Result<bool, ProbeError> {
        let page = self.inner.pages().get(index as i32).map_err(|error| ProbeError::Read(error.to_string()))?;
        let bitmap = page.render_with_config(&PdfRenderConfig::new().set_target_width(64).set_maximum_height(64)).map_err(|error| ProbeError::Render(error.to_string()))?;
        Ok(bitmap.width() > 0 && bitmap.height() > 0)
    }
    fn render_bgra_into(&self, target: PageTarget<'_>, mode: RenderMode, width: u32, height: u32, out: &mut [u8]) -> Result<(), RenderError> {
        if bgra_len(width, height) != Some(out.len()) { return Err(RenderError::Layout); }
        self.with_target(target, |page| {
            let config = PdfRenderConfig::new().set_fixed_size(width as i32, height as i32).set_format(PdfBitmapFormat::BGRA).set_reverse_byte_order(false).clear_before_rendering(true).set_clear_color(PdfColor::WHITE);
            let config = match mode { RenderMode::Print => config.render_annotations(true).render_form_data(true).use_print_quality(true), RenderMode::Viewer => config.render_annotations(true), RenderMode::Plain => config };
            let mut bitmap = PdfBitmap::from_bytes(width as i32, height as i32, PdfBitmapFormat::BGRA, out).map_err(|error| RenderError::Render(error.to_string()))?;
            page.render_into_bitmap_with_config(&mut bitmap, &config).map_err(|error| RenderError::Render(error.to_string()))?;
            if bitmap.width() != width as i32 || bitmap.height() != height as i32 { return Err(RenderError::Layout); }
            if bitmap.format().map_err(|error| RenderError::Format(error.to_string()))? != PdfBitmapFormat::BGRA { return Err(RenderError::Layout); }
            Ok(())
        }).map_err(RenderError::Page)?
    }
}

fn displayed_note_rect(page: &PdfPage<'_>, rect: CropBox) -> Result<Option<crate::comments::DisplayRect>, String> {
    const SIZE: i32 = 1_000_000;
    let config = PdfRenderConfig::new().set_fixed_size(SIZE, SIZE); let mut points = Vec::with_capacity(4);
    for (x, y) in [(rect.left, rect.bottom), (rect.left, rect.top), (rect.right, rect.bottom), (rect.right, rect.top)] {
        points.push(page.points_to_pixels(PdfPoints::new(x), PdfPoints::new(y), &config).map_err(|error| error.to_string())?);
    }
    let x = (points.iter().map(|point| point.0).min().unwrap() as f64 / SIZE as f64).max(0.0);
    let y = (points.iter().map(|point| point.1).min().unwrap() as f64 / SIZE as f64).max(0.0);
    let right = (points.iter().map(|point| point.0).max().unwrap() as f64 / SIZE as f64).min(1.0);
    let bottom = (points.iter().map(|point| point.1).max().unwrap() as f64 / SIZE as f64).min(1.0);
    Ok((right > x && bottom > y).then_some(crate::comments::DisplayRect { x, y, width: right - x, height: bottom - y }))
}

fn displayed_crop(page: &PdfPage<'_>, rect: crate::service::CropRect) -> Result<CropBox, String> {
    if ![rect.x, rect.y, rect.width, rect.height].iter().all(|value| value.is_finite()) || rect.x < 0.0 || rect.y < 0.0 || rect.width <= 0.0 || rect.height <= 0.0 || rect.x + rect.width > 1.0 + 1e-12 || rect.y + rect.height > 1.0 + 1e-12 {
        return Err("Choose a nonempty crop rectangle within the current page.".into());
    }
    let bounds = page.boundaries().bounding().map_err(|error| error.to_string())?.bounds;
    let visible = CropBox { left: bounds.left().value, bottom: bounds.bottom().value, right: bounds.right().value, top: bounds.top().value };
    if rect.x == 0.0 && rect.y == 0.0 && rect.width == 1.0 && rect.height == 1.0 { visible.validate_within(visible)?; return Ok(visible); }
    const SIZE: i32 = 1_000_000;
    let config = PdfRenderConfig::new().set_fixed_size(SIZE, SIZE);
    // DeviceToPage takes integer pixels. Interpolate its page corners in f64 so a one-point crop is not rounded to a smaller pixel cell.
    let mut page_corners = [(0.0_f64, 0.0_f64); 4];
    for (index, (x, y)) in [(0, 0), (SIZE, 0), (0, SIZE), (SIZE, SIZE)].into_iter().enumerate() {
        let (x, y) = page.pixels_to_points(x, y, &config).map_err(|error| error.to_string())?;
        if !x.value.is_finite() || !y.value.is_finite() { return Err("The crop could not be mapped to the source page.".into()); }
        page_corners[index] = (f64::from(x.value), f64::from(y.value));
    }
    let mut crop = CropBox { left: f32::INFINITY, bottom: f32::INFINITY, right: f32::NEG_INFINITY, top: f32::NEG_INFINITY };
    for (x, y) in [(rect.x, rect.y), (rect.x + rect.width, rect.y), (rect.x, rect.y + rect.height), (rect.x + rect.width, rect.y + rect.height)] {
        let (x, y) = (x.min(1.0), y.min(1.0));
        let weights = [(1.0 - x) * (1.0 - y), x * (1.0 - y), (1.0 - x) * y, x * y];
        let point = weights.into_iter().zip(page_corners).fold((0.0, 0.0), |(x, y), (weight, (corner_x, corner_y))| (x + weight * corner_x, y + weight * corner_y));
        crop.left = crop.left.min(point.0 as f32); crop.bottom = crop.bottom.min(point.1 as f32); crop.right = crop.right.max(point.0 as f32); crop.top = crop.top.max(point.1 as f32);
    }
    crop.left = crop.left.max(visible.left); crop.bottom = crop.bottom.max(visible.bottom); crop.right = crop.right.min(visible.right); crop.top = crop.top.min(visible.top);
    crop.validate_within(visible)?;
    Ok(crop)
}

/// Runs `action` on a source page with the plan's crop and extra quarter turns applied, then
/// restores the page's own crop box and rotation.
fn with_planned_page<T>(document: &PdfDocument<'_>, spec: &PageSpec, action: impl FnOnce(&PdfPage<'_>) -> Result<T, String>) -> Result<T, String> {
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

#[cfg(test)]
mod tests {
    use super::*;

    const CHILD: &str = "SM5_ENGINE_EQUIVALENCE_CHILD";
    const NAME: &str = "pdf_engine::tests::render_bgra_into_matches_render_with_config";

    /// The caller-buffer render must be byte-identical to the render the worker used before the
    /// boundary (`render_with_config` into a PDFium-owned bitmap, then `as_raw_bytes`), for both
    /// flag sets, every quarter turn, with and without a crop. Runs in a child process so this test
    /// binary binds PDFium only once per process.
    #[test]
    fn render_bgra_into_matches_render_with_config() {
        if std::env::var_os(CHILD).is_none() {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", NAME, "--nocapture", "--test-threads=1"]).env(CHILD, "1").output().unwrap();
            let stdout = String::from_utf8_lossy(&output.stdout);
            assert!(output.status.success() && stdout.contains("engine-equivalence-ok"), "child failed:\n{stdout}\n{}", String::from_utf8_lossy(&output.stderr));
            return;
        }
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let engine = InProcessEngine::bind(root.join("resources/pdfium/bin/pdfium.dll")).unwrap();
        let files = [root.join("resources/welcome.pdf"), root.join("../test-corpus/synthetic-text-1500.pdf"), root.join("../test-corpus/synthetic-scan-98.pdf"), root.join("tests/fixtures/reportlab-mixed-fields.pdf")];
        let mut compared = 0;
        for path in files {
            let document = engine.load_from_vec(std::fs::read(&path).unwrap(), None).unwrap();
            for source in 0..document.page_count().min(2) {
                let (width, height) = document.source_page_size(source).unwrap();
                let crop = CropBox { left: width * 0.1, bottom: height * 0.2, right: width * 0.8, top: height * 0.9 };
                for turns in 0..4 {
                    for crop in [None, Some(crop)] {
                        let spec = PageSpec { source, turns, crop, notes: Vec::new() };
                        let viewer = document.render_viewer_png(&spec, 613).unwrap();
                        let viewer_rgba = image::load_from_memory(&viewer.png).unwrap().to_rgba8();
                        let mut redaction = vec![0; bgra_len(viewer.width, viewer.height).unwrap()];
                        document.render_bgra_into(PageTarget::Planned(&spec), RenderMode::Viewer, viewer.width, viewer.height, &mut redaction).unwrap();
                        assert!(redaction.chunks_exact(4).zip(viewer_rgba.as_raw().chunks_exact(4)).all(|(bgra, rgba)| bgra[0] == rgba[2] && bgra[1] == rgba[1] && bgra[2] == rgba[0]), "{} page {source} turns {turns} crop {} viewer mode differs from the viewer render", path.display(), crop.is_some());
                        for mode in [RenderMode::Print, RenderMode::Viewer, RenderMode::Plain] {
                            let (pixel_width, pixel_height) = (613_u32, 457_u32);
                            let config = PdfRenderConfig::new().set_fixed_size(pixel_width as i32, pixel_height as i32).set_format(PdfBitmapFormat::BGRA).set_reverse_byte_order(false).clear_before_rendering(true).set_clear_color(PdfColor::WHITE);
                            let config = match mode { RenderMode::Print => config.render_annotations(true).render_form_data(true).use_print_quality(true), RenderMode::Viewer => config.render_annotations(true), RenderMode::Plain => config };
                            let before = with_planned_page(&document.inner, &spec, |page| Ok(page.render_with_config(&config).unwrap().as_raw_bytes())).unwrap();
                            let mut after = vec![0; bgra_len(pixel_width, pixel_height).unwrap()];
                            document.render_bgra_into(PageTarget::Planned(&spec), mode, pixel_width, pixel_height, &mut after).unwrap();
                            assert!(before == after, "{} page {source} turns {turns} crop {} {mode:?} differs", path.display(), crop.is_some());
                            compared += 1;
                        }
                    }
                }
            }
        }
        let mut wrong = vec![0; 16];
        assert!(matches!(document_layout_error(&engine, &root, &mut wrong), Err(RenderError::Layout)));
        println!("engine-equivalence-ok compared={compared}");
    }

    fn document_layout_error(engine: &InProcessEngine, root: &std::path::Path, out: &mut [u8]) -> Result<(), RenderError> {
        let document = engine.load_from_vec(std::fs::read(root.join("resources/welcome.pdf")).unwrap(), None).unwrap();
        document.render_bgra_into(PageTarget::Source(0), RenderMode::Plain, 10, 10, out)
    }

    const SIZES_CHILD: &str = "SM5_PAGE_SIZES_CHILD";
    const SIZES_NAME: &str = "pdf_engine::tests::page_sizes_by_index_match_loading_every_page";
    /// Optional: a text file listing more PDFs (one path per line, read only) to compare as well.
    const SIZES_EXTRA: &str = "SM5_PAGE_SIZES_EXTRA";

    /// One page per case: inherited MediaBox and /Rotate, every quarter turn (and -90), CropBox with and
    /// without rotation, offset and reversed MediaBox, CropBox past the MediaBox, /UserUnit, and a
    /// non-quarter /Rotate.
    fn edge_pages() -> Vec<u8> {
        use lopdf::{dictionary, Document, Object, Stream};
        let mut document = Document::with_version("1.7");
        let pages = document.new_object_id();
        let content = document.add_object(Stream::new(dictionary! {}, b"0 0 m 100 100 l S".to_vec()));
        let boxed = |values: [i64; 4]| Object::Array(values.into_iter().map(Object::Integer).collect());
        let cases: Vec<Vec<(&str, Object)>> = vec![
            vec![],
            vec![("MediaBox", boxed([0, 0, 612, 792])), ("Rotate", Object::Integer(0))],
            vec![("Rotate", Object::Integer(180))],
            vec![("Rotate", Object::Integer(270))],
            vec![("Rotate", Object::Integer(-90))],
            vec![("MediaBox", boxed([0, 0, 612, 792])), ("CropBox", boxed([50, 60, 400, 500])), ("Rotate", Object::Integer(0))],
            vec![("CropBox", boxed([50, 60, 400, 500]))],
            vec![("MediaBox", boxed([100, 200, 712, 992])), ("Rotate", Object::Integer(0))],
            vec![("MediaBox", boxed([612, 792, 0, 0])), ("Rotate", Object::Integer(0))],
            vec![("MediaBox", boxed([0, 0, 300, 300])), ("CropBox", boxed([-50, -50, 500, 500])), ("Rotate", Object::Integer(0))],
            vec![("UserUnit", Object::Real(2.0)), ("Rotate", Object::Integer(0))],
            vec![("Rotate", Object::Integer(45))],
        ];
        let kids = cases
            .into_iter()
            .map(|extra| {
                let mut page = dictionary! { "Type" => "Page", "Parent" => pages, "Contents" => content };
                for (key, value) in extra {
                    page.set(key, value);
                }
                Object::Reference(document.add_object(page))
            })
            .collect::<Vec<_>>();
        let count = kids.len() as i64;
        document.objects.insert(pages, Object::Dictionary(dictionary! { "Type" => "Pages", "Kids" => kids, "Count" => count, "MediaBox" => boxed([0, 0, 600, 800]), "Rotate" => 90 }));
        let catalog = document.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages });
        document.trailer.set("Root", catalog);
        let mut bytes = Vec::new();
        document.save_to(&mut bytes).unwrap();
        bytes
    }

    /// `page_sizes` reads sizes with `FPDF_GetPageSizeByIndexF` instead of loading every page. It must give
    /// exactly (bit for bit) what loading each page and reading its width and height gave, on every fixture
    /// PDF and the constructed edge pages (plus any PDFs listed in `SM5_PAGE_SIZES_EXTRA`). Runs in a child
    /// process so this test binary binds PDFium only once per process.
    #[test]
    fn page_sizes_by_index_match_loading_every_page() {
        if std::env::var_os(SIZES_CHILD).is_none() {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", SIZES_NAME, "--nocapture", "--test-threads=1"]).env(SIZES_CHILD, "1").output().unwrap();
            let stdout = String::from_utf8_lossy(&output.stdout);
            assert!(output.status.success() && stdout.contains("page-sizes-identical"), "child failed:\n{stdout}\n{}", String::from_utf8_lossy(&output.stderr));
            if let Some(line) = stdout.lines().find(|line| line.contains("page-sizes-identical")) { println!("{line}"); }
            return;
        }
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let engine = InProcessEngine::bind(root.join("resources/pdfium/bin/pdfium.dll")).unwrap();
        let mut sources: Vec<(String, Vec<u8>)> = vec![("edge pages".into(), edge_pages())];
        let mut paths = vec![root.join("resources/welcome.pdf")];
        for folder in [root.join("../test-corpus"), root.join("tests/fixtures"), root.join("tests/fixtures/searchable-ocr")] {
            if let Ok(entries) = std::fs::read_dir(folder) {
                let mut found = entries.filter_map(|entry| entry.ok().map(|entry| entry.path())).filter(|path| path.extension().is_some_and(|extension| extension.eq_ignore_ascii_case("pdf"))).collect::<Vec<_>>();
                found.sort();
                paths.extend(found);
            }
        }
        let extra = std::env::var_os(SIZES_EXTRA).map(|list| std::fs::read_to_string(list).unwrap()).unwrap_or_default();
        let extra_paths = extra.lines().map(str::trim).filter(|line| !line.is_empty()).map(PathBuf::from).collect::<Vec<_>>();
        let fixture_count = paths.len();
        paths.extend(extra_paths.iter().cloned());
        for (index, path) in paths.iter().enumerate() {
            let label = if index < fixture_count { path.display().to_string() } else { format!("extra file {}", index - fixture_count + 1) };
            sources.push((label, std::fs::read(path).unwrap()));
        }
        let (mut documents, mut pages, mut unloadable) = (0, 0, 0);
        for (label, bytes) in sources {
            let Ok(document) = engine.load_from_vec(bytes, None) else { unloadable += 1; continue };
            let legacy = {
                let all = document.inner.pages();
                (0..all.len()).map(|index| all.get(index).map(|page| (page.width().value.to_bits(), page.height().value.to_bits())).map_err(|error| error.to_string())).collect::<Result<Vec<_>, _>>()
            };
            let current = document.page_sizes().map(|sizes| sizes.iter().map(|size| (size.width.to_bits(), size.height.to_bits())).collect::<Vec<_>>());
            match (&legacy, &current) {
                (Ok(legacy), Ok(current)) => assert_eq!(legacy, current, "{label}: page sizes differ"),
                (Ok(legacy), Err(error)) => assert!(legacy.iter().any(|&(width, height)| { let (width, height) = (f32::from_bits(width), f32::from_bits(height)); !width.is_finite() || !height.is_finite() || width <= 0.0 || height <= 0.0 }), "{label}: refused only now: {error}"),
                (Err(error), _) => panic!("{label}: a page the old path could not load ({error}); decide how to treat it"),
            }
            documents += 1;
            pages += legacy.map_or(0, |sizes| sizes.len());
        }
        assert!(documents >= 10, "{documents} documents compared");
        println!("page-sizes-identical documents={documents} pages={pages} extra_files={} unloadable={unloadable}", extra_paths.len());
    }
}
