//! Clean-up of the OCR recognition input. It runs on a copy of the rendered
//! page; the page image a searchable copy keeps is never altered. Each step is
//! linear in the pixel count, works on one byte per pixel and checks
//! cancellation every row band, so time and memory are bounded by the 16 MiB
//! P6 input cap. Measured choices are in `docs/ocr-accuracy.md`.

use crate::searchable_ocr::ParsedTsvPage;

pub(crate) const MAX_DESKEW_DEGREES: f64 = 5.0;
/// Below this the engine's own line finder copes, and resampling only blurs.
pub(crate) const MIN_DESKEW_DEGREES: f64 = 1.0;
const DESKEW_SAMPLE_LIMIT: usize = 400_000;
const BACKGROUND_BLOCK: usize = 48;
const BAND_ROWS: usize = 64;

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct PreprocessOptions {
    pub(crate) flatten_background: bool,
    pub(crate) deskew: bool,
}

impl PreprocessOptions {
    pub(crate) const PRODUCTION: Self = Self { flatten_background: true, deskew: true };
    #[cfg(test)]
    pub(crate) const NONE: Self = Self { flatten_background: false, deskew: false };
}

/// Below this estimated paper level a region is a photo, a dark border or a
/// negative, and stretching it to white would only amplify noise.
const DARK_LEVEL_FLOOR: f32 = 64.0;

/// The rotation applied to the recognition image. The rotated page is drawn
/// on a canvas large enough to hold all of it, so no corner is cut off. A
/// point `q` on that canvas came from `R(radians) * (q - canvas centre) +
/// page centre` in the rendered page, so recognised boxes map back exactly.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Skew {
    radians: f64,
    sin: f64,
    cos: f64,
    page_centre: (f64, f64),
    canvas_centre: (f64, f64),
    canvas: Option<(u32, u32)>,
}

impl Skew {
    pub(crate) const NONE: Self = Self {
        radians: 0.0, sin: 0.0, cos: 1.0, page_centre: (0.0, 0.0), canvas_centre: (0.0, 0.0), canvas: None,
    };

    /// `None` when the padded canvas would not fit the engine's input limits.
    fn for_page(degrees: f64, width: usize, height: usize) -> Option<Self> {
        let radians = degrees.to_radians();
        let (sin, cos) = radians.sin_cos();
        let (w, h) = (width as f64, height as f64);
        let canvas_width = (w * cos + h * sin.abs()).ceil() as u64;
        let canvas_height = (w * sin.abs() + h * cos).ceil() as u64;
        let header = format!("P6\n{canvas_width} {canvas_height}\n255\n").len() as u64;
        let edge = crate::ocr_process::OCR_MAX_EDGE as u64;
        if canvas_width > edge || canvas_height > edge
            || header + canvas_width * canvas_height * 3 > crate::ocr_process::OCR_INPUT_LIMIT as u64
        {
            return None;
        }
        Some(Self {
            radians, sin, cos,
            page_centre: (w / 2.0, h / 2.0),
            canvas_centre: (canvas_width as f64 / 2.0, canvas_height as f64 / 2.0),
            canvas: Some((canvas_width as u32, canvas_height as u32)),
        })
    }

    /// A rotation about the centre of an unpadded canvas, for drawing tilted
    /// test pages.
    #[cfg(test)]
    fn same_canvas(degrees: f64, width: usize, height: usize) -> Self {
        let (sin, cos) = degrees.to_radians().sin_cos();
        let centre = (width as f64 / 2.0, height as f64 / 2.0);
        Self { radians: degrees.to_radians(), sin, cos, page_centre: centre, canvas_centre: centre, canvas: Some((width as u32, height as u32)) }
    }

    pub(crate) fn is_identity(&self) -> bool { self.canvas.is_none() }
    pub(crate) fn degrees(&self) -> f64 { self.radians.to_degrees() }

    /// The size of the image the engine saw, for parsing its TSV.
    pub(crate) fn recognition_size(&self, raster_width: u32, raster_height: u32) -> (u32, u32) {
        self.canvas.unwrap_or((raster_width, raster_height))
    }

    pub(crate) fn to_page(&self, x: f64, y: f64) -> (f64, f64) {
        let (dx, dy) = (x - self.canvas_centre.0, y - self.canvas_centre.1);
        (self.cos * dx - self.sin * dy + self.page_centre.0, self.sin * dx + self.cos * dy + self.page_centre.1)
    }

    /// The page-pixel bounding box of a recognition-image rectangle, clamped
    /// to the raster: (left, top, right, bottom).
    fn box_to_page(&self, left: u32, top: u32, right: u32, bottom: u32, width: u32, height: u32) -> (u32, u32, u32, u32) {
        let corners = [(left, top), (right, top), (left, bottom), (right, bottom)]
            .map(|(x, y)| self.to_page(f64::from(x), f64::from(y)));
        let clamp = |value: f64, limit: u32| value.clamp(0.0, f64::from(limit)) as u32;
        let min_x = corners.iter().map(|c| c.0).fold(f64::MAX, f64::min).floor();
        let max_x = corners.iter().map(|c| c.0).fold(f64::MIN, f64::max).ceil();
        let min_y = corners.iter().map(|c| c.1).fold(f64::MAX, f64::min).floor();
        let max_y = corners.iter().map(|c| c.1).fold(f64::MIN, f64::max).ceil();
        let (l, r) = (clamp(min_x, width - 1), clamp(max_x, width));
        let (t, b) = (clamp(min_y, height - 1), clamp(max_y, height));
        (l, t, r.max(l + 1), b.max(t + 1))
    }
}

pub(crate) struct Prepared {
    pub(crate) p6: Vec<u8>,
    pub(crate) skew: Skew,
}

struct Gray {
    width: usize,
    height: usize,
    pixels: Vec<u8>,
}

fn check(is_cancelled: &dyn Fn() -> bool, row: usize) -> Result<(), String> {
    if row % BAND_ROWS == 0 && is_cancelled() { Err("OCR was cancelled".into()) } else { Ok(()) }
}

/// Grayscale, background flattening and (when the page is skewed by 1 to 5
/// degrees) deskew onto a padded canvas. Returns the recognition P6 and the
/// rotation it applied; without a rotation the P6 has the page's size.
pub(crate) fn preprocess_p6(p6: &[u8], options: PreprocessOptions, is_cancelled: &dyn Fn() -> bool) -> Result<Prepared, String> {
    let (width, height, rgb) = split_p6(p6)?;
    let mut gray = to_gray(width, height, rgb, is_cancelled)?;
    if options.flatten_background { flatten_background(&mut gray, is_cancelled)?; }
    let mut skew = Skew::NONE;
    if options.deskew {
        let degrees = estimate_skew_degrees(&gray, is_cancelled)?;
        if degrees.abs() >= MIN_DESKEW_DEGREES {
            if let Some(rotation) = Skew::for_page(degrees, width, height) {
                skew = rotation;
                gray = rotate(&gray, skew, is_cancelled)?;
            }
        }
    }
    Ok(Prepared { p6: to_p6(&gray)?, skew })
}

/// Moves words recognised on the deskewed canvas (parsed at
/// `skew.recognition_size`) back onto the rendered page of `width` x `height`
/// pixels. A word keeps its own bounding box; its line band becomes the part
/// of its line under the word, so a tilted line's words stay on it. Boxes are
/// clamped to the page.
pub(crate) fn map_words_to_page(page: &mut ParsedTsvPage, skew: Skew, width: u32, height: u32) {
    if skew.is_identity() { return; }
    page.raster_width = width;
    page.raster_height = height;
    for word in &mut page.words {
        let right = word.left + word.width;
        let (l, t, r, b) = skew.box_to_page(word.left, word.top, right, word.top + word.height, width, height);
        let (_, band_top, _, band_bottom) = skew.box_to_page(word.left, word.line_top.min(word.top), right, (word.line_top + word.line_height).max(word.top + word.height), width, height);
        word.left = l;
        word.top = t;
        word.width = r - l;
        word.height = b - t;
        word.line_top = band_top.min(t);
        word.line_height = band_bottom.max(b) - word.line_top;
    }
}

fn split_p6(p6: &[u8]) -> Result<(usize, usize, &[u8]), String> {
    let mut parts = p6.splitn(4, |byte| *byte == b'\n');
    if parts.next() != Some(b"P6".as_slice()) { return Err("OCR input must use the exact P6 format".into()); }
    let dimensions = parts.next().and_then(|value| std::str::from_utf8(value).ok()).ok_or("OCR P6 dimensions are missing")?;
    if parts.next() != Some(b"255".as_slice()) { return Err("OCR P6 maximum value must be 255".into()); }
    let pixels = parts.next().ok_or("OCR P6 pixel bytes are missing")?;
    let mut numbers = dimensions.split(' ').map(|value| value.parse::<usize>().ok().filter(|n| *n > 0));
    let (Some(Some(width)), Some(Some(height)), None) = (numbers.next(), numbers.next(), numbers.next()) else {
        return Err("OCR P6 dimensions are invalid".into());
    };
    let expected = width.checked_mul(height).and_then(|n| n.checked_mul(3)).ok_or("OCR P6 dimensions overflow")?;
    if pixels.len() != expected { return Err("OCR P6 pixel count does not match its dimensions".into()); }
    Ok((width, height, pixels))
}

fn alloc(len: usize, fill: u8) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    out.try_reserve_exact(len).map_err(|_| "Could not allocate the bounded OCR image.".to_owned())?;
    out.resize(len, fill);
    Ok(out)
}

fn to_gray(width: usize, height: usize, rgb: &[u8], is_cancelled: &dyn Fn() -> bool) -> Result<Gray, String> {
    let mut pixels = alloc(width * height, 0)?;
    for (row, (out, input)) in pixels.chunks_exact_mut(width).zip(rgb.chunks_exact(width * 3)).enumerate() {
        check(is_cancelled, row)?;
        for (value, pixel) in out.iter_mut().zip(input.chunks_exact(3)) {
            *value = ((299 * u32::from(pixel[0]) + 587 * u32::from(pixel[1]) + 114 * u32::from(pixel[2]) + 500) / 1000) as u8;
        }
    }
    Ok(Gray { width, height, pixels })
}

fn to_p6(gray: &Gray) -> Result<Vec<u8>, String> {
    let header = format!("P6\n{} {}\n255\n", gray.width, gray.height);
    let mut out = Vec::new();
    out.try_reserve_exact(header.len() + gray.pixels.len() * 3).map_err(|_| "Could not allocate the bounded OCR image.".to_owned())?;
    out.extend_from_slice(header.as_bytes());
    for value in &gray.pixels { out.extend_from_slice(&[*value; 3]); }
    Ok(out)
}

fn percentile(counts: &[u64; 256], total: u64, fraction: f64) -> u8 {
    let target = (total as f64 * fraction) as u64;
    let mut seen = 0;
    for (value, count) in counts.iter().enumerate() {
        seen += count;
        if seen > target { return value as u8; }
    }
    255
}

/// Divides out uneven lighting: the paper level is the 90th percentile of each
/// 48-pixel block, widened by a 3x3 block maximum, interpolated bilinearly, and
/// every pixel is scaled so that paper becomes white.
fn flatten_background(gray: &mut Gray, is_cancelled: &dyn Fn() -> bool) -> Result<(), String> {
    let columns = gray.width.div_ceil(BACKGROUND_BLOCK);
    let rows = gray.height.div_ceil(BACKGROUND_BLOCK);
    let mut levels = vec![0_f32; columns * rows];
    for by in 0..rows {
        check(is_cancelled, by * BAND_ROWS)?;
        for bx in 0..columns {
            let mut counts = [0_u64; 256];
            let mut total = 0;
            for y in by * BACKGROUND_BLOCK..((by + 1) * BACKGROUND_BLOCK).min(gray.height) {
                let row = &gray.pixels[y * gray.width..(y + 1) * gray.width];
                for value in &row[bx * BACKGROUND_BLOCK..((bx + 1) * BACKGROUND_BLOCK).min(gray.width)] {
                    counts[usize::from(*value)] += 1;
                    total += 1;
                }
            }
            levels[by * columns + bx] = f32::from(percentile(&counts, total, 0.9));
        }
    }
    let smoothed = (0..levels.len()).map(|index| {
        let (bx, by) = (index % columns, index / columns);
        let mut best = 0_f32;
        for y in by.saturating_sub(1)..=(by + 1).min(rows - 1) {
            for x in bx.saturating_sub(1)..=(bx + 1).min(columns - 1) {
                best = best.max(levels[y * columns + x]);
            }
        }
        best
    }).collect::<Vec<_>>();
    let level_at = |x: usize, y: usize| smoothed[y.min(rows - 1) * columns + x.min(columns - 1)];
    let block = BACKGROUND_BLOCK as f32;
    for y in 0..gray.height {
        check(is_cancelled, y)?;
        let fy = ((y as f32 + 0.5) / block - 0.5).max(0.0);
        let (y0, ty) = (fy as usize, fy.fract());
        for x in 0..gray.width {
            let fx = ((x as f32 + 0.5) / block - 0.5).max(0.0);
            let (x0, tx) = (fx as usize, fx.fract());
            let top = level_at(x0, y0) * (1.0 - tx) + level_at(x0 + 1, y0) * tx;
            let bottom = level_at(x0, y0 + 1) * (1.0 - tx) + level_at(x0 + 1, y0 + 1) * tx;
            let level = top * (1.0 - ty) + bottom * ty;
            if level < DARK_LEVEL_FLOOR { continue; }
            let value = &mut gray.pixels[y * gray.width + x];
            *value = (f32::from(*value) * 255.0 / level).round().min(255.0) as u8;
        }
    }
    Ok(())
}

fn otsu(pixels: &[u8]) -> u8 {
    let mut counts = [0_u64; 256];
    for value in pixels { counts[usize::from(*value)] += 1; }
    let total = pixels.len() as f64;
    let sum: f64 = counts.iter().enumerate().map(|(value, count)| value as f64 * *count as f64).sum();
    let (mut weight_back, mut sum_back, mut best, mut threshold) = (0.0, 0.0, -1.0, 127_u8);
    for (value, count) in counts.iter().enumerate() {
        weight_back += *count as f64;
        if weight_back == 0.0 { continue; }
        let weight_fore = total - weight_back;
        if weight_fore == 0.0 { break; }
        sum_back += value as f64 * *count as f64;
        let between = weight_back * weight_fore * (sum_back / weight_back - (sum - sum_back) / weight_fore).powi(2);
        if between > best { best = between; threshold = value as u8; }
    }
    threshold
}

/// Projection-profile skew estimate on the Otsu ink of a reduced image: the
/// angle within ±5 degrees whose rotated row histogram is sharpest. A best
/// angle on the search limit is treated as no reliable estimate.
fn estimate_skew_degrees(gray: &Gray, is_cancelled: &dyn Fn() -> bool) -> Result<f64, String> {
    let threshold = otsu(&gray.pixels);
    let step = gray.width.max(gray.height).div_ceil(1000).max(1);
    let mut points = Vec::new();
    for (index, y) in (0..gray.height).step_by(step).enumerate() {
        check(is_cancelled, index)?;
        for x in (0..gray.width).step_by(step) {
            if gray.pixels[y * gray.width + x] <= threshold { points.push(((x / step) as f32, (y / step) as f32)); }
        }
    }
    let reduced_width = gray.width.div_ceil(step) as f32;
    let reduced_height = gray.height.div_ceil(step) as f32;
    if points.len() < 200 || points.len() as f32 > 0.5 * reduced_width * reduced_height { return Ok(0.0); }
    if points.len() > DESKEW_SAMPLE_LIMIT {
        let keep = points.len().div_ceil(DESKEW_SAMPLE_LIMIT);
        points = points.into_iter().step_by(keep).collect();
    }
    let (centre_x, centre_y) = (reduced_width / 2.0, reduced_height / 2.0);
    let bins = reduced_width.hypot(reduced_height) as usize + 2;
    let mut profile = vec![0_u32; bins];
    let mut score = |degrees: f64| -> f64 {
        profile.iter_mut().for_each(|value| *value = 0);
        let (sin, cos) = degrees.to_radians().sin_cos();
        let (sin, cos, offset) = (sin as f32, cos as f32, bins as f32 / 2.0);
        for (x, y) in &points {
            let row = -sin * (x - centre_x) + cos * (y - centre_y) + offset;
            if row >= 0.0 && (row as usize) < bins { profile[row as usize] += 1; }
        }
        profile.iter().map(|count| f64::from(*count).powi(2)).sum()
    };
    let mut search = |from: f64, to: f64, increment: f64| -> Result<f64, String> {
        if is_cancelled() { return Err("OCR was cancelled".into()); }
        let (mut best_score, mut best_angle) = (f64::MIN, 0.0_f64);
        let steps = ((to - from) / increment).round() as i32;
        for index in 0..=steps {
            let angle = from + f64::from(index) * increment;
            let value = score(angle);
            if value > best_score || (value == best_score && angle.abs() < best_angle.abs()) { (best_score, best_angle) = (value, angle); }
        }
        Ok(best_angle)
    };
    let coarse = search(-MAX_DESKEW_DEGREES, MAX_DESKEW_DEGREES, 0.25)?;
    if coarse.abs() >= MAX_DESKEW_DEGREES { return Ok(0.0); }
    let fine = search(coarse - 0.25, coarse + 0.25, 0.05)?;
    // A fine angle that runs into the limit is as unreliable as a coarse one.
    if fine.abs() > MAX_DESKEW_DEGREES - 0.125 { return Ok(0.0); }
    Ok(fine)
}

/// Bilinear resample through `skew` onto its canvas; areas from outside the
/// page are white.
fn rotate(gray: &Gray, skew: Skew, is_cancelled: &dyn Fn() -> bool) -> Result<Gray, String> {
    let (width, height) = (gray.width, gray.height);
    let (canvas_width, canvas_height) = skew.canvas.map_or((width, height), |(w, h)| (w as usize, h as usize));
    let mut out = alloc(canvas_width * canvas_height, 255)?;
    let sample = |x: isize, y: isize| -> f64 {
        if x < 0 || y < 0 || x as usize >= width || y as usize >= height { 255.0 } else { f64::from(gray.pixels[y as usize * width + x as usize]) }
    };
    for y in 0..canvas_height {
        check(is_cancelled, y)?;
        for x in 0..canvas_width {
            let (sx, sy) = skew.to_page(x as f64 + 0.5, y as f64 + 0.5);
            let (sx, sy) = (sx - 0.5, sy - 0.5);
            let (fx, fy) = (sx.floor(), sy.floor());
            let (tx, ty) = (sx - fx, sy - fy);
            let (x0, y0) = (fx as isize, fy as isize);
            let top = sample(x0, y0) * (1.0 - tx) + sample(x0 + 1, y0) * tx;
            let bottom = sample(x0, y0 + 1) * (1.0 - tx) + sample(x0 + 1, y0 + 1) * tx;
            out[y * canvas_width + x] = (top * (1.0 - ty) + bottom * ty).round().clamp(0.0, 255.0) as u8;
        }
    }
    Ok(Gray { width: canvas_width, height: canvas_height, pixels: out })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::searchable_ocr::{map_words_to_display, parse_tesseract_tsv};

    fn never() -> bool { false }

    fn p6(width: usize, height: usize, mut pixel: impl FnMut(usize, usize) -> u8) -> Vec<u8> {
        let mut out = format!("P6\n{width} {height}\n255\n").into_bytes();
        for y in 0..height { for x in 0..width { out.extend_from_slice(&[pixel(x, y); 3]); } }
        out
    }

    fn gray_of(p6: &[u8]) -> (usize, usize, Vec<u8>) {
        let (width, height, rgb) = split_p6(p6).unwrap();
        (width, height, rgb.chunks_exact(3).map(|pixel| pixel[0]).collect())
    }

    /// Text-like rows of dashes, rotated by `degrees` about the centre.
    fn skewed_lines(width: usize, height: usize, degrees: f64) -> Vec<u8> {
        let skew = Skew::same_canvas(-degrees, width, height);
        p6(width, height, |x, y| {
            let (sx, sy) = skew.to_page(x as f64 + 0.5, y as f64 + 0.5);
            let inside = sx > 60.0 && sx < width as f64 - 60.0 && sy > 60.0 && sy < height as f64 - 60.0;
            let ink = inside && (sy as usize % 36) < 12 && (sx as usize % 22) < 16;
            if ink { 20 } else { 235 }
        })
    }

    #[test]
    fn skew_estimate_recovers_the_applied_angle() {
        for degrees in [-3.0, -1.5, 2.2, 4.0] {
            let input = skewed_lines(900, 1200, degrees);
            let (width, height, pixels) = gray_of(&input);
            let estimate = estimate_skew_degrees(&Gray { width, height, pixels }, &never).unwrap();
            assert!((estimate - degrees).abs() <= 0.1, "applied {degrees}, estimated {estimate}");
        }
        let (width, height, pixels) = gray_of(&skewed_lines(900, 1200, 0.4));
        let prepared = preprocess_p6(&skewed_lines(900, 1200, 0.4), PreprocessOptions::PRODUCTION, &never).unwrap();
        assert!(prepared.skew.is_identity(), "a 0.4 degree tilt is left alone");
        assert_eq!(split_p6(&prepared.p6).unwrap().0, width);
        assert_eq!(pixels.len(), width * height);
    }

    #[test]
    fn deskewed_rows_are_level_on_a_canvas_that_holds_the_whole_page() {
        let input = skewed_lines(900, 1200, 3.0);
        let prepared = preprocess_p6(&input, PreprocessOptions::PRODUCTION, &never).unwrap();
        assert!((prepared.skew.degrees() - 3.0).abs() <= 0.1, "{}", prepared.skew.degrees());
        let (width, height, pixels) = gray_of(&prepared.p6);
        assert_eq!((width as u32, height as u32), prepared.skew.recognition_size(900, 1200));
        assert!(width > 900 && height > 1200);
        let row_ink = |pixels: &[u8], width: usize| -> f64 {
            let rows = pixels.chunks_exact(width).map(|row| row.iter().filter(|v| **v < 128).count() as f64).collect::<Vec<_>>();
            rows.iter().map(|v| v * v).sum::<f64>()
        };
        let (_, _, before) = gray_of(&input);
        assert!(row_ink(&pixels, width) > 1.5 * row_ink(&before, 900), "row profile should sharpen after deskew");
    }

    /// Stamps and page numbers sit in the corners; rotating onto a page-sized
    /// canvas used to cut them off.
    #[test]
    fn deskew_keeps_marks_in_every_corner() {
        let (width, height) = (900, 1200);
        let tilt = Skew::same_canvas(-3.0, width, height);
        let input = p6(width, height, |x, y| {
            let corner = (x < 24 || x >= width - 24) && (y < 24 || y >= height - 24);
            let (sx, sy) = tilt.to_page(x as f64 + 0.5, y as f64 + 0.5);
            let text = sx > 60.0 && sx < width as f64 - 60.0 && sy > 60.0 && sy < height as f64 - 60.0
                && (sy as usize % 36) < 12 && (sx as usize % 22) < 16;
            if corner || text { 20 } else { 235 }
        });
        let prepared = preprocess_p6(&input, PreprocessOptions::PRODUCTION, &never).unwrap();
        assert!((prepared.skew.degrees() - 3.0).abs() <= 0.1, "{}", prepared.skew.degrees());
        let (canvas_width, canvas_height, pixels) = gray_of(&prepared.p6);
        for (cx, cy) in [(12.0, 12.0), (width as f64 - 12.0, 12.0), (12.0, height as f64 - 12.0), (width as f64 - 12.0, height as f64 - 12.0)] {
            // Find the canvas pixel that samples this page point.
            let found = (0..canvas_height).flat_map(|y| (0..canvas_width).map(move |x| (x, y))).find(|(x, y)| {
                let (px, py) = prepared.skew.to_page(*x as f64 + 0.5, *y as f64 + 0.5);
                (px - cx).abs() < 0.75 && (py - cy).abs() < 0.75
            });
            let (x, y) = found.unwrap_or_else(|| panic!("corner ({cx}, {cy}) has no place on the canvas"));
            assert!(pixels[y * canvas_width + x] < 128, "corner ({cx}, {cy}) mark lost");
        }
    }

    /// A photo or black scan border must not be stretched into speckle.
    #[test]
    fn flattening_leaves_dark_regions_dark() {
        let input = p6(600, 400, |x, y| {
            if x < 300 { (15 + (x * 7 + y * 13) % 20) as u8 } else if (100..110).contains(&y) && x % 20 < 12 { 40 } else { 220 }
        });
        let prepared = preprocess_p6(&input, PreprocessOptions { flatten_background: true, deskew: false }, &never).unwrap();
        let (width, _, pixels) = gray_of(&prepared.p6);
        let dark = (0..400).flat_map(|y| (0..200).map(move |x| (x, y))).map(|(x, y)| u32::from(pixels[y * width + x])).collect::<Vec<_>>();
        let mean = dark.iter().sum::<u32>() / dark.len() as u32;
        let max = *dark.iter().max().unwrap();
        assert!(mean < 40 && max < 60, "dark region mean {mean} max {max}");
        assert!(pixels[300 * width + 500] >= 245, "paper still flattened");
    }

    #[test]
    fn degenerate_pages_preprocess_without_failing() {
        for (width, height) in [(1, 1), (1, 700), (700, 1), (2, 2)] {
            for fill in [0_u8, 255] {
                let input = p6(width, height, |_, _| fill);
                let prepared = preprocess_p6(&input, PreprocessOptions::PRODUCTION, &never).unwrap();
                assert!(prepared.skew.is_identity(), "{width}x{height} {fill}");
                assert_eq!(gray_of(&prepared.p6).0, width);
            }
        }
        let black = preprocess_p6(&p6(400, 300, |_, _| 0), PreprocessOptions::PRODUCTION, &never).unwrap();
        assert!(gray_of(&black.p6).2.iter().all(|value| *value == 0), "an all-black page stays black");
        let white = preprocess_p6(&p6(400, 300, |_, _| 255), PreprocessOptions::PRODUCTION, &never).unwrap();
        assert!(gray_of(&white.p6).2.iter().all(|value| *value == 255));
    }

    #[test]
    fn flattening_lifts_a_dark_uneven_background_to_white_and_keeps_ink_dark() {
        let input = p6(600, 400, |x, y| {
            let paper = 90 + (x * 120 / 600) as u8;
            if (100..110).contains(&y) && x % 20 < 12 { paper / 4 } else { paper }
        });
        let prepared = preprocess_p6(&input, PreprocessOptions { flatten_background: true, deskew: false }, &never).unwrap();
        let (width, _, pixels) = gray_of(&prepared.p6);
        let paper = [pixels[300 * width + 10], pixels[300 * width + 590], pixels[20 * width + 300]];
        assert!(paper.iter().all(|value| *value >= 200), "paper {paper:?}");
        let ink = pixels[105 * width + 5];
        assert!(ink <= 80, "ink {ink}");
    }

    #[test]
    fn identity_options_only_convert_to_gray() {
        let input = p6(3, 2, |x, y| (x * 40 + y * 7) as u8);
        let prepared = preprocess_p6(&input, PreprocessOptions::NONE, &never).unwrap();
        assert_eq!(prepared.p6, input);
        assert!(prepared.skew.is_identity());
    }

    #[test]
    fn cancellation_stops_preprocessing() {
        let input = skewed_lines(300, 300, 2.0);
        assert_eq!(preprocess_p6(&input, PreprocessOptions::PRODUCTION, &|| true).err().as_deref(), Some("OCR was cancelled"));
    }

    #[test]
    fn rejects_inputs_that_are_not_exact_p6() {
        assert!(preprocess_p6(b"P5\n1 1\n255\n\x00", PreprocessOptions::PRODUCTION, &never).is_err());
        assert!(preprocess_p6(b"P6\n2 1\n255\n\x00\x00\x00", PreprocessOptions::PRODUCTION, &never).is_err());
        assert!(preprocess_p6(b"P6\n0 1\n255\n", PreprocessOptions::PRODUCTION, &never).is_err());
    }

    /// A dark block drawn on a page tilted by 2.5 degrees is found, level, in
    /// the deskewed recognition image; mapping that box back lands on the
    /// block as drawn on the page.
    #[test]
    fn recognised_geometry_maps_back_onto_the_page() {
        let (width, height) = (1000_usize, 1300_usize);
        let block = (620.0, 300.0, 820.0, 330.0);
        let tilt = Skew::same_canvas(-2.5, width, height);
        let input = p6(width, height, |x, y| {
            let (sx, sy) = tilt.to_page(x as f64 + 0.5, y as f64 + 0.5);
            let clear = (250.0..380.0).contains(&sy);
            let text_row = !clear && sy > 80.0 && sy < 1220.0 && (sy as usize % 40) < 14 && sx > 80.0 && sx < 920.0 && (sx as usize % 25) < 18;
            let target = sx >= block.0 && sx < block.2 && sy >= block.1 && sy < block.3;
            if target || text_row { 0 } else { 255 }
        });
        let mut drawn = (usize::MAX, usize::MAX, 0, 0);
        for y in 0..height {
            for x in 0..width {
                let (sx, sy) = tilt.to_page(x as f64 + 0.5, y as f64 + 0.5);
                if sx >= block.0 && sx < block.2 && sy >= block.1 && sy < block.3 {
                    drawn = (drawn.0.min(x), drawn.1.min(y), drawn.2.max(x + 1), drawn.3.max(y + 1));
                }
            }
        }
        let prepared = preprocess_p6(&input, PreprocessOptions::PRODUCTION, &never).unwrap();
        assert!((prepared.skew.degrees() - 2.5).abs() <= 0.1, "{}", prepared.skew.degrees());
        let (canvas_width, canvas_height, recognition) = gray_of(&prepared.p6);
        assert!(canvas_width > width && canvas_height > height, "rotated onto a padded canvas");
        let (pad_x, pad_y) = ((canvas_width - width) / 2, (canvas_height - height) / 2);
        let bounds = |pixels: &[u8], window: (usize, usize, usize, usize)| {
            let mut found = (usize::MAX, usize::MAX, 0, 0);
            for y in window.1..window.3 {
                for x in window.0..window.2 {
                    if pixels[y * canvas_width + x] < 128 { found = (found.0.min(x), found.1.min(y), found.2.max(x + 1), found.3.max(y + 1)); }
                }
            }
            found
        };
        let level = bounds(&recognition, (560 + pad_x, 260 + pad_y, 900 + pad_x, 370 + pad_y));
        let (level_width, level_height) = (level.2 - level.0, level.3 - level.1);
        assert!((196..=204).contains(&level_width) && (28..=32).contains(&level_height), "level box {level:?}");
        let mapped = prepared.skew.box_to_page(level.0 as u32, level.1 as u32, level.2 as u32, level.3 as u32, width as u32, height as u32);
        let near = |a: u32, b: usize| (i64::from(a) - b as i64).abs() <= 3;
        assert!(near(mapped.0, drawn.0) && near(mapped.1, drawn.1) && near(mapped.2, drawn.2) && near(mapped.3, drawn.3),
            "mapped {mapped:?} vs drawn {drawn:?}");
    }

    #[test]
    fn mapped_words_stay_inside_the_raster_and_their_line_band() {
        let tsv_for = |(width, height): (u32, u32)| format!("level\tpage_num\tblock_num\tpar_num\tline_num\tword_num\tleft\ttop\twidth\theight\tconf\ttext\n\
1\t1\t0\t0\t0\t0\t0\t0\t{width}\t{height}\t-1\t\n\
2\t1\t1\t0\t0\t0\t0\t100\t1000\t40\t-1\t\n\
3\t1\t1\t1\t0\t0\t0\t100\t1000\t40\t-1\t\n\
4\t1\t1\t1\t1\t0\t0\t100\t1000\t40\t-1\t\n\
5\t1\t1\t1\t1\t1\t0\t110\t120\t25\t95\tLeft\n\
5\t1\t1\t1\t1\t2\t880\t105\t120\t30\t95\tRight\n");
        let skew = Skew::for_page(3.0, 1000, 1300).unwrap();
        let canvas = skew.recognition_size(1000, 1300);
        assert!(canvas.0 > 1000 && canvas.1 > 1300);
        let mut parsed = parse_tesseract_tsv(tsv_for(canvas).as_bytes(), canvas.0, canvas.1).unwrap();
        map_words_to_page(&mut parsed, skew, 1000, 1300);
        assert_eq!((parsed.raster_width, parsed.raster_height), (1000, 1300));
        let (left, right) = (&parsed.words[0], &parsed.words[1]);
        // Positive rotation moves the right end of a line down on the page.
        assert!(right.top > left.top + 30, "{left:?} {right:?}");
        for word in &parsed.words {
            assert!(word.left + word.width <= 1000 && word.top + word.height <= 1300);
            assert!(word.line_top <= word.top && word.top + word.height <= word.line_top + word.line_height);
            assert!(word.line_height <= 40 + 10, "band stays close to the line height: {word:?}");
        }
        assert!(map_words_to_display(&parsed, 480.0, 624.0).is_ok());
        let before = parse_tesseract_tsv(tsv_for((1000, 1300)).as_bytes(), 1000, 1300).unwrap();
        let mut unchanged = before.clone();
        map_words_to_page(&mut unchanged, Skew::NONE, 1000, 1300);
        assert_eq!(unchanged, before);
        let steep = Skew::for_page(-4.8, 1000, 1300).unwrap();
        let steep_canvas = steep.recognition_size(1000, 1300);
        let mut corner = parse_tesseract_tsv(tsv_for(steep_canvas).as_bytes(), steep_canvas.0, steep_canvas.1).unwrap();
        map_words_to_page(&mut corner, steep, 1000, 1300);
        assert!(corner.words.iter().all(|w| w.left + w.width <= 1000 && w.top + w.height <= 1300 && w.width > 0 && w.height > 0));
    }

    const SMOKE_TEXT: &str = "Receipt #A-17: Coffee & Tea, $12.50. Mixed case: 3rd Avenue; ready.";

    fn runner() -> crate::ocr_process::OcrProcessRunner {
        let setup = crate::ocr_test_support::local_setup().expect("the test must call require_local_setup first");
        let engine = setup.engine_directory();
        crate::ocr_process::OcrProcessRunner::new(engine.join("bin/tesseract.exe"), engine.join("tessdata"), setup.engine).unwrap()
    }

    fn edits(a: &str, b: &str) -> usize {
        let (a, b) = (a.chars().collect::<Vec<_>>(), b.chars().collect::<Vec<_>>());
        let mut row = (0..=b.len()).collect::<Vec<_>>();
        for i in 1..=a.len() {
            let mut previous = row[0];
            row[0] = i;
            for j in 1..=b.len() {
                let substitution = previous + usize::from(a[i - 1] != b[j - 1]);
                previous = row[j];
                row[j] = substitution.min(row[j] + 1).min(row[j - 1] + 1);
            }
        }
        row[b.len()]
    }

    fn collapse(text: &str) -> String { text.split_whitespace().collect::<Vec<_>>().join(" ") }

    /// The engine's known-text sample on a larger page, tilted by `degrees`,
    /// on a dim, unevenly lit background with faded ink. Returns the page and
    /// its ink mask (ink as drawn, before shading).
    fn degraded_smoke_page(degrees: f64) -> (usize, usize, Vec<u8>, Vec<bool>) {
        let smoke = std::fs::read(crate::ocr_test_support::local_setup().unwrap().smoke_pnm()).unwrap();
        let (sw, sh, rgb) = split_p6(&smoke).unwrap();
        let (width, height) = (sw + 200, sh + 460);
        let tilt = Skew::same_canvas(-degrees, width, height);
        let mut mask = vec![false; width * height];
        let page = p6(width, height, |x, y| {
            let (sx, sy) = tilt.to_page(x as f64 + 0.5, y as f64 + 0.5);
            let (ux, uy) = (sx.floor() as isize - 100, sy.floor() as isize - 230);
            let ink = ux >= 0 && uy >= 0 && (ux as usize) < sw && (uy as usize) < sh && rgb[(uy as usize * sw + ux as usize) * 3] < 128;
            mask[y * width + x] = ink;
            let paper = 70.0 + 110.0 * x as f64 / width as f64;
            (if ink { paper * 0.55 } else { paper }) as u8
        });
        (width, height, page, mask)
    }

    #[test]
    #[ignore = "needs PDF_WORKSTATION_OCR_SETUP_ROOT pointing at a scripts/setup-ocr.ps1 output root"]
    fn real_engine_reads_a_skewed_low_contrast_page_better_after_preprocessing() {
        if crate::ocr_test_support::require_local_setup("real_engine_reads_a_skewed_low_contrast_page_better_after_preprocessing").is_none() { return; }
        let _serial = crate::ocr_process::test_lock();
        let (width, height, page, mask) = degraded_smoke_page(3.0);
        let runner = runner();
        let raw = runner.recognize_p6_with_preprocess(&page, PreprocessOptions::NONE, false).unwrap();
        let cleaned = runner.recognize_p6_with_preprocess(&page, PreprocessOptions::PRODUCTION, false).unwrap();
        let (raw_edits, cleaned_edits) = (edits(&collapse(&raw.text), SMOKE_TEXT), edits(&collapse(&cleaned.text), SMOKE_TEXT));
        println!("SM4_PREPROCESS raw {raw_edits} edits {:?}; cleaned {cleaned_edits} edits {:?}; skew {:.2}", collapse(&raw.text), collapse(&cleaned.text), cleaned.skew.degrees());
        assert!((cleaned.skew.degrees() - 3.0).abs() <= 0.15, "skew {}", cleaned.skew.degrees());
        assert!(cleaned_edits < raw_edits, "preprocessing should read better: raw {raw_edits}, cleaned {cleaned_edits}");
        assert!(cleaned_edits <= 3, "cleaned text {:?}", cleaned.text);

        // The searchable path: words recognised in the level image map back
        // onto the tilted ink of the page as rendered.
        let tsv = runner.recognize_p6_with_preprocess(&page, PreprocessOptions::PRODUCTION, true).unwrap();
        let (canvas_width, canvas_height) = tsv.skew.recognition_size(width as u32, height as u32);
        let mut parsed = parse_tesseract_tsv(tsv.text.as_bytes(), canvas_width, canvas_height).unwrap();
        let unmapped = parsed.clone();
        map_words_to_page(&mut parsed, tsv.skew, width as u32, height as u32);
        let ink_total = mask.iter().filter(|ink| **ink).count() as f64;
        let covered = |page: &ParsedTsvPage| {
            let inside = mask.iter().enumerate().filter(|(index, ink)| {
                let (x, y) = ((index % width) as u32, (index / width) as u32);
                **ink && page.words.iter().any(|w| x >= w.left && x < w.left + w.width && y >= w.top && y < w.top + w.height)
            }).count();
            inside as f64 / ink_total
        };
        let (mapped_cover, unmapped_cover) = (covered(&parsed), covered(&unmapped));
        println!("SM4_PREPROCESS ink inside word boxes: mapped {mapped_cover:.3}, unmapped {unmapped_cover:.3}");
        assert!(mapped_cover >= 0.97, "mapped words cover {mapped_cover}");
        assert!(mapped_cover > unmapped_cover + 0.1, "mapping must matter: {mapped_cover} vs {unmapped_cover}");
        assert!(map_words_to_display(&parsed, width as f64 * 0.48, height as f64 * 0.48).is_ok());
    }

    /// Accuracy evidence, not a gate: writes the app's own recognised text for
    /// every `*.pnm` in `SM4_OCR_ACCURACY_INPUT` to `SM4_OCR_ACCURACY_OUTPUT`,
    /// and checks that each page's TSV parses and maps back onto the page.
    #[test]
    #[ignore = "measurement: needs the OCR setup root and SM4_OCR_ACCURACY_INPUT / SM4_OCR_ACCURACY_OUTPUT"]
    fn accuracy_corpus_through_the_app_path() {
        if crate::ocr_test_support::require_local_setup("accuracy_corpus_through_the_app_path").is_none() { return; }
        let (Some(input), Some(output)) = (std::env::var_os("SM4_OCR_ACCURACY_INPUT"), std::env::var_os("SM4_OCR_ACCURACY_OUTPUT")) else {
            eprintln!("SKIPPED accuracy_corpus_through_the_app_path: set SM4_OCR_ACCURACY_INPUT and SM4_OCR_ACCURACY_OUTPUT");
            return;
        };
        let _serial = crate::ocr_process::test_lock();
        let runner = runner();
        let options = match std::env::var("SM4_OCR_ACCURACY_PREPROCESS").as_deref() {
            Ok("none") => PreprocessOptions::NONE,
            Ok("flatten") => PreprocessOptions { flatten_background: true, deskew: false },
            _ => PreprocessOptions::PRODUCTION,
        };
        std::fs::create_dir_all(&output).unwrap();
        let mut entries = std::fs::read_dir(&input).unwrap().map(|entry| entry.unwrap().path())
            .filter(|path| path.extension().is_some_and(|extension| extension == "pnm")).collect::<Vec<_>>();
        entries.sort();
        for path in entries {
            let p6 = std::fs::read(&path).unwrap();
            let (width, height, _) = split_p6(&p6).unwrap();
            let started = std::time::Instant::now();
            let prepared = preprocess_p6(&p6, options, &never).unwrap();
            let prepare_ms = started.elapsed().as_millis();
            let started = std::time::Instant::now();
            let text = runner.recognize_p6_with_preprocess(&p6, options, false).unwrap();
            let text_ms = started.elapsed().as_millis();
            let tsv = runner.recognize_p6_with_preprocess(&p6, options, true).unwrap();
            let (canvas_width, canvas_height) = tsv.skew.recognition_size(width as u32, height as u32);
            let layer = parse_tesseract_tsv(tsv.text.as_bytes(), canvas_width, canvas_height).and_then(|mut parsed| {
                map_words_to_page(&mut parsed, tsv.skew, width as u32, height as u32);
                map_words_to_display(&parsed, width as f64 * 0.48, height as f64 * 0.48).map(|words| words.len())
            });
            let stem = path.file_stem().unwrap().to_string_lossy().into_owned();
            std::fs::write(std::path::Path::new(&output).join(format!("{stem}.ocr.txt")), &text.text).unwrap();
            println!("SM4_ACCURACY {stem} prepare_ms={prepare_ms} ocr_ms={text_ms} skew={:.2} text_layer={layer:?}", prepared.skew.degrees());
        }
    }
}
