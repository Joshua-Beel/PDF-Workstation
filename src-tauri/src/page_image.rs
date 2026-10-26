use std::{fs::File, io::{BufReader, Read, Write}, path::Path};
use image::{ColorType, ExtendedColorType, ImageDecoder};
use serde::{Deserialize, Serialize};

const MAX_EDGE: u32 = 16_384;
const MAX_PIXELS: u64 = 32_000_000;
const MAX_BGRA_BYTES: u64 = 128 * 1024 * 1024;
const MAX_IMAGE_BYTES: u64 = 256 * 1024 * 1024;
const JPEG_QUALITY: u8 = 90;

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum PageImageFormat {
    #[default]
    Png,
    Jpeg,
}

impl PageImageFormat {
    pub fn label(self) -> &'static str { match self { Self::Png => "PNG", Self::Jpeg => "JPEG" } }
    pub fn extension(self) -> &'static str { match self { Self::Png => "png", Self::Jpeg => "jpg" } }
    pub fn extensions(self) -> &'static [&'static str] { match self { Self::Png => &["png"], Self::Jpeg => &["jpg", "jpeg"] } }
    pub fn dialog_title(self) -> &'static str { match self { Self::Png => "Export page as PNG", Self::Jpeg => "Export page as JPEG" } }
    pub fn filter_name(self) -> &'static str { match self { Self::Png => "PNG images", Self::Jpeg => "JPEG images" } }

    fn accepts_extension(self, path: &Path) -> bool {
        path.extension().is_some_and(|extension| self.extensions().iter().any(|expected| extension.eq_ignore_ascii_case(expected)))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RasterDimensions {
    pub width: u32,
    pub height: u32,
    pixels: usize,
    bgra_bytes: usize,
    rgb_bytes: usize,
}

/// Points to whole pixels at `dpi`, rounded up, computed exactly as the export dialog does
/// (`Math.ceil(points * dpi / 72)` in `ExportPageImageDialog.tsx`). The f32 size is read through its
/// shortest decimal form, the number the dialog receives in JSON, and is multiplied before dividing:
/// dividing first (150 / 72 = 2.0833333333333335) turned Letter's 792 pt into 1650.0000000000002 and
/// so 1651 px. Multiplying first is exact for whole-point sizes (the product is an exact integer and a
/// non-integer quotient is at least 1/72 away from the next integer).
pub(crate) fn pixels_at(points: f32, dpi: u16) -> f64 {
    let points = points.to_string().parse::<f64>().unwrap_or(f64::from(points));
    (points * f64::from(dpi) / 72.0).ceil()
}

pub fn dimensions(width_points: f32, height_points: f32, dpi: u16, format: PageImageFormat) -> Result<RasterDimensions, String> {
    if !matches!(dpi, 72 | 150 | 300) { return Err(format!("Choose 72, 150, or 300 DPI for {} export.", format.label())); }
    if !width_points.is_finite() || !height_points.is_finite() || width_points <= 0.0 || height_points <= 0.0 {
        return Err("The page has invalid displayed dimensions.".into());
    }
    let width = pixels_at(width_points, dpi);
    let height = pixels_at(height_points, dpi);
    if !width.is_finite() || !height.is_finite() || width < 1.0 || height < 1.0 || width > f64::from(MAX_EDGE) || height > f64::from(MAX_EDGE) {
        return Err(format!("The {} dimensions exceed the 16,384 pixel edge limit.", format.label()));
    }
    let width = width as u32;
    let height = height as u32;
    let pixels = u64::from(width).checked_mul(u64::from(height)).ok_or_else(|| format!("The {} pixel count is out of range.", format.label()))?;
    if pixels > MAX_PIXELS { return Err(format!("The {} exceeds the 32 megapixel limit.", format.label())); }
    let bgra_bytes = pixels.checked_mul(4).ok_or_else(|| format!("The {} bitmap size is out of range.", format.label()))?;
    if bgra_bytes > MAX_BGRA_BYTES { return Err(format!("The {} bitmap exceeds the 128 MiB limit.", format.label())); }
    let rgb_bytes = pixels.checked_mul(3).ok_or_else(|| format!("The {} pixel data size is out of range.", format.label()))?;
    Ok(RasterDimensions {
        width,
        height,
        pixels: usize::try_from(pixels).map_err(|_| format!("The {} pixel count is out of range.", format.label()))?,
        bgra_bytes: usize::try_from(bgra_bytes).map_err(|_| format!("The {} bitmap size is out of range.", format.label()))?,
        rgb_bytes: usize::try_from(rgb_bytes).map_err(|_| format!("The {} pixel data size is out of range.", format.label()))?,
    })
}

pub fn pixels_per_meter(dpi: u16) -> Result<u32, String> {
    match dpi {
        72 => Ok(2_835),
        150 => Ok(5_906),
        300 => Ok(11_811),
        _ => Err("Choose 72, 150, or 300 DPI for PNG export.".into()),
    }
}

pub(crate) fn bgra_to_rgb(dimensions: RasterDimensions, bgra: &[u8], format: PageImageFormat) -> Result<Vec<u8>, String> {
    if bgra.len() != dimensions.bgra_bytes { return Err(format!("Unexpected {} export bitmap layout.", format.label())); }
    let mut rgb = Vec::with_capacity(dimensions.rgb_bytes);
    for pixel in bgra.chunks_exact(4) {
        let alpha = u16::from(pixel[3]);
        for channel in [pixel[2], pixel[1], pixel[0]] {
            rgb.push(((u16::from(channel) * alpha + 255 * (255 - alpha) + 127) / 255) as u8);
        }
    }
    if rgb.len() != dimensions.rgb_bytes { return Err(format!("Unexpected {} export pixel layout.", format.label())); }
    Ok(rgb)
}

struct BoundedWriter<W> {
    inner: W,
    written: u64,
    limit: u64,
    format: PageImageFormat,
}

impl<W> BoundedWriter<W> {
    fn new(inner: W, limit: u64, format: PageImageFormat) -> Self { Self { inner, written: 0, limit, format } }
}

impl<W: Write> Write for BoundedWriter<W> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        let next = self.written.checked_add(bytes.len() as u64).ok_or_else(|| std::io::Error::other(format!("{} output size overflow", self.format.label())))?;
        if next > self.limit { return Err(std::io::Error::other(format!("{} exceeds the 256 MiB encoded-size limit", self.format.label()))); }
        let count = self.inner.write(bytes)?;
        self.written += count as u64;
        Ok(count)
    }
    fn flush(&mut self) -> std::io::Result<()> { self.inner.flush() }
}

fn validate_png(path: &Path, expected: RasterDimensions, dpi: u16, rgb: &[u8]) -> Result<(), String> {
    if rgb.len() != expected.rgb_bytes { return Err("PNG validation received an invalid pixel buffer.".into()); }
    let file = File::open(path).map_err(|error| format!("Could not reopen the PNG for validation: {error}"))?;
    let decoder = png::Decoder::new_with_limits(BufReader::new(file), png::Limits { bytes: MAX_BGRA_BYTES as usize });
    let mut reader = decoder.read_info().map_err(|error| format!("Could not validate the PNG header: {error}"))?;
    let info = reader.info();
    if info.width != expected.width || info.height != expected.height || info.color_type != png::ColorType::Rgb || info.bit_depth != png::BitDepth::Eight {
        return Err("The PNG header differs from the export plan.".into());
    }
    let meter = pixels_per_meter(dpi)?;
    let physical = info.pixel_dims.ok_or("The PNG is missing physical-resolution metadata.")?;
    if physical.xppu != meter || physical.yppu != meter || physical.unit != png::Unit::Meter {
        return Err("The PNG physical-resolution metadata differs from the export plan.".into());
    }
    if info.animation_control.is_some() { return Err("The PNG unexpectedly contains animation metadata.".into()); }
    if reader.output_buffer_size() != Some(expected.rgb_bytes) { return Err("The decoded PNG size differs from the export plan.".into()); }
    let mut decoded = vec![0; expected.rgb_bytes];
    let output = reader.next_frame(&mut decoded).map_err(|error| format!("Could not decode the PNG for validation: {error}"))?;
    if output.width != expected.width || output.height != expected.height || output.color_type != png::ColorType::Rgb || output.bit_depth != png::BitDepth::Eight || output.buffer_size() != expected.rgb_bytes || decoded != rgb {
        return Err("The decoded PNG pixels differ from the export plan.".into());
    }
    reader.finish().map_err(|error| format!("Could not finish validating the PNG: {error}"))?;
    Ok(())
}

fn jfif_density(bytes: &[u8]) -> Option<(u8, u16, u16)> {
    if bytes.len() < 20 || bytes[..6] != [0xff, 0xd8, 0xff, 0xe0, 0x00, 0x10] || &bytes[6..11] != b"JFIF\0" { return None; }
    Some((bytes[13], u16::from_be_bytes([bytes[14], bytes[15]]), u16::from_be_bytes([bytes[16], bytes[17]])))
}

fn validate_jpeg(path: &Path, expected: RasterDimensions, dpi: u16) -> Result<(), String> {
    let mut file = File::open(path).map_err(|error| format!("Could not reopen the JPEG for validation: {error}"))?;
    let mut jfif = [0u8; 20];
    file.read_exact(&mut jfif).map_err(|error| format!("Could not read the JPEG density metadata: {error}"))?;
    if jfif_density(&jfif) != Some((1, dpi, dpi)) {
        return Err("The JPEG physical-resolution metadata differs from the export plan.".into());
    }
    let decoder = image::codecs::jpeg::JpegDecoder::new(BufReader::new(File::open(path).map_err(|error| format!("Could not reopen the JPEG for decoding: {error}"))?)).map_err(|error| format!("Could not validate the JPEG header: {error}"))?;
    if decoder.dimensions() != (expected.width, expected.height) || decoder.color_type() != ColorType::Rgb8 || decoder.total_bytes() != expected.rgb_bytes as u64 {
        return Err("The JPEG header differs from the export plan.".into());
    }
    let mut decoded = vec![0; expected.rgb_bytes];
    decoder.read_image(&mut decoded).map_err(|error| format!("Could not decode the JPEG for validation: {error}"))?;
    Ok(())
}

pub fn write_page_image(path: &Path, dimensions: RasterDimensions, dpi: u16, bgra: &[u8], format: PageImageFormat, canceled: impl Fn() -> bool) -> Result<(), String> {
    if !format.accepts_extension(path) {
        let expected = if format == PageImageFormat::Png { ".png" } else { ".jpg or .jpeg" };
        return Err(format!("The output filename must end in {expected}."));
    }
    if path.symlink_metadata().is_ok() { return Err(format!("That file already exists. Choose a new filename; {} export never overwrites an existing file.", format.label())); }
    let parent = path.parent().filter(|parent| !parent.as_os_str().is_empty()).ok_or("Choose an output folder.")?;
    if canceled() { return Err(format!("{} export was canceled.", format.label())); }
    let rgb = bgra_to_rgb(dimensions, bgra, format)?;
    let mut temporary = tempfile::NamedTempFile::new_in(parent).map_err(|error| format!("Could not create the {} output: {error}", format.label()))?;
    {
        let bounded = BoundedWriter::new(temporary.as_file_mut(), MAX_IMAGE_BYTES, format);
        match format {
            PageImageFormat::Png => {
                let mut encoder = png::Encoder::new(bounded, dimensions.width, dimensions.height);
                encoder.set_color(png::ColorType::Rgb);
                encoder.set_depth(png::BitDepth::Eight);
                let meter = pixels_per_meter(dpi)?;
                encoder.set_pixel_dims(Some(png::PixelDimensions { xppu: meter, yppu: meter, unit: png::Unit::Meter }));
                encoder.validate_sequence(true);
                let mut writer = encoder.write_header().map_err(|error| format!("Could not encode the PNG header: {error}"))?;
                writer.write_image_data(&rgb).map_err(|error| format!("Could not encode the PNG pixels: {error}"))?;
                writer.finish().map_err(|error| format!("Could not finish the PNG: {error}"))?;
            }
            PageImageFormat::Jpeg => {
                let mut encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(bounded, JPEG_QUALITY);
                encoder.set_pixel_density(image::codecs::jpeg::PixelDensity::dpi(dpi));
                encoder.encode(&rgb, dimensions.width, dimensions.height, ExtendedColorType::Rgb8).map_err(|error| format!("Could not encode the JPEG pixels: {error}"))?;
            }
        }
    }
    let encoded = temporary.as_file().metadata().map_err(|error| format!("Could not inspect the {} output: {error}", format.label()))?.len();
    if encoded > MAX_IMAGE_BYTES { return Err(format!("The {} exceeds the 256 MiB encoded-size limit.", format.label())); }
    temporary.as_file().sync_all().map_err(|error| format!("Could not sync the {} output: {error}", format.label()))?;
    match format {
        PageImageFormat::Png => validate_png(temporary.path(), dimensions, dpi, &rgb)?,
        PageImageFormat::Jpeg => validate_jpeg(temporary.path(), dimensions, dpi)?,
    }
    if canceled() { return Err(format!("{} export was canceled.", format.label())); }
    temporary.persist_noclobber(path).map_err(|error| format!("Could not publish the {} without overwriting another file: {error}", format.label()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dpi_ceil_and_resource_bounds_are_exact() {
        assert_eq!(PageImageFormat::default(), PageImageFormat::Png);
        assert_eq!(serde_json::from_str::<PageImageFormat>("\"png\"").unwrap(), PageImageFormat::Png);
        assert_eq!(serde_json::from_str::<PageImageFormat>("\"jpeg\"").unwrap(), PageImageFormat::Jpeg);
        assert!(serde_json::from_str::<PageImageFormat>("\"jpg\"").is_err());
        assert_eq!(dimensions(612.0, 792.0, 72, PageImageFormat::Png).unwrap(), RasterDimensions { width: 612, height: 792, pixels: 484_704, bgra_bytes: 1_938_816, rgb_bytes: 1_454_112 });
        assert_eq!((dimensions(612.01, 792.01, 150, PageImageFormat::Png).unwrap().width, dimensions(612.01, 792.01, 150, PageImageFormat::Png).unwrap().height), (1_276, 1_651));
        assert_eq!((dimensions(1.0, 1.0, 300, PageImageFormat::Jpeg).unwrap().width, dimensions(1.0, 1.0, 300, PageImageFormat::Jpeg).unwrap().height), (5, 5));
        for format in [PageImageFormat::Png, PageImageFormat::Jpeg] {
            for dpi in [0, 71, 73, 149, 151, 299, 301, 600] { assert!(dimensions(1.0, 1.0, dpi, format).is_err(), "{format:?} dpi {dpi}"); }
            assert!(dimensions(f32::NAN, 1.0, 72, format).is_err());
            assert!(dimensions(16_385.0, 1.0, 72, format).is_err());
            assert!(dimensions(8_000.0, 4_001.0, 72, format).is_err());
            assert!(dimensions(8_000.0, 4_000.0, 72, format).is_ok());
        }
        assert_eq!([pixels_per_meter(72).unwrap(), pixels_per_meter(150).unwrap(), pixels_per_meter(300).unwrap()], [2_835, 5_906, 11_811]);
    }

    #[test]
    fn letter_and_a4_match_the_export_dialog_at_every_dpi() {
        // The 0.3.1 rehearsal regression: Letter at 150 DPI was 1275 x 1651, the dialog promised 1650.
        for format in [PageImageFormat::Png, PageImageFormat::Jpeg] {
            for (width, height, dpi, expected) in [
                (612.0, 792.0, 72, (612, 792)), (612.0, 792.0, 150, (1_275, 1_650)), (612.0, 792.0, 300, (2_550, 3_300)),
                (595.276, 841.89, 72, (596, 842)), (595.276, 841.89, 150, (1_241, 1_754)), (595.276, 841.89, 300, (2_481, 3_508)),
                (100.5, 200.25, 150, (210, 418)),
            ] {
                let actual = dimensions(width, height, dpi, format).unwrap();
                assert_eq!((actual.width, actual.height), expected, "{format:?} {width}x{height} at {dpi}");
            }
        }
    }

    #[test]
    fn dimensions_agree_with_the_dialog_formula_on_the_shared_table() {
        // `src/pageImageDimensions.fixture.json` is also checked against `pageImageDimensions` in
        // `src/ExportPageImageDialog.test.tsx`, so Rust and the dialog agree on every row.
        let table: Vec<serde_json::Value> = serde_json::from_str(&std::fs::read_to_string(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../src/pageImageDimensions.fixture.json")).unwrap()).unwrap();
        assert!(table.len() >= 30);
        for row in table {
            let (width, height) = (row["width"].as_f64().unwrap() as f32, row["height"].as_f64().unwrap() as f32);
            let dpi = row["dpi"].as_u64().unwrap() as u16;
            let expected = (row["pixelWidth"].as_u64().unwrap() as u32, row["pixelHeight"].as_u64().unwrap() as u32);
            for format in [PageImageFormat::Png, PageImageFormat::Jpeg] {
                let actual = dimensions(width, height, dpi, format).unwrap();
                assert_eq!((actual.width, actual.height), expected, "{row}");
            }
        }
    }

    #[test]
    fn disk_png_is_rgb8_white_composited_and_never_overwritten() {
        let folder = tempfile::tempdir().unwrap();
        let path = folder.path().join("page.png");
        let dimensions = dimensions(2.0, 1.0, 72, PageImageFormat::Png).unwrap();
        let bgra = [0, 0, 255, 128, 255, 0, 0, 255];
        write_page_image(&path, dimensions, 72, &bgra, PageImageFormat::Png, || false).unwrap();
        let decoder = png::Decoder::new(BufReader::new(File::open(&path).unwrap()));
        let mut reader = decoder.read_info().unwrap();
        assert_eq!(reader.info().pixel_dims.unwrap().xppu, 2_835);
        let mut pixels = vec![0; reader.output_buffer_size().unwrap()];
        let output = reader.next_frame(&mut pixels).unwrap();
        assert_eq!((output.width, output.height, output.color_type, output.bit_depth), (2, 1, png::ColorType::Rgb, png::BitDepth::Eight));
        assert_eq!(pixels, [255, 127, 127, 0, 0, 255]);
        let bytes = std::fs::read(&path).unwrap();
        assert!(write_page_image(&path, dimensions, 72, &bgra, PageImageFormat::Png, || false).unwrap_err().contains("never overwrites"));
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        assert!(write_page_image(&folder.path().join("wrong.jpg"), dimensions, 72, &bgra, PageImageFormat::Png, || false).is_err());
        assert!(write_page_image(&folder.path().join("canceled.png"), dimensions, 72, &bgra, PageImageFormat::Png, || true).is_err());
        assert!(!folder.path().join("canceled.png").exists());
        let probe = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../target/page-image-probe"); std::fs::create_dir_all(&probe).unwrap();
        let artifact = probe.join("rgb-alpha-2x1-72dpi.png"); if !artifact.exists() { std::fs::copy(&path, &artifact).unwrap(); }
        println!("PAGE_IMAGE_PROBE path={} width=2 height=1 dpi=72 pixels=rgb[(255,127,127),(0,0,255)]", artifact.display());
    }

    #[test]
    fn bitmap_layout_and_prepublication_cancellation_fail_without_output() {
        let folder = tempfile::tempdir().unwrap(); let path = folder.path().join("page.png"); let dimensions = dimensions(1.0, 1.0, 72, PageImageFormat::Png).unwrap();
        assert!(write_page_image(&path, dimensions, 72, &[0, 0, 0], PageImageFormat::Png, || false).is_err()); assert!(!path.exists());
        let checks = std::cell::Cell::new(0); let canceled = || { let count = checks.get(); checks.set(count + 1); count >= 1 };
        assert!(write_page_image(&path, dimensions, 72, &[0, 0, 0, 255], PageImageFormat::Png, canceled).unwrap_err().contains("canceled")); assert!(!path.exists());
        let mut bounded = BoundedWriter::new(Vec::new(), 4, PageImageFormat::Png); assert_eq!(bounded.write(&[1, 2, 3, 4]).unwrap(), 4); assert!(bounded.write(&[5]).is_err());
        let corrupt = folder.path().join("corrupt.tmp"); std::fs::write(&corrupt, b"not a PNG").unwrap();
        assert!(validate_png(&corrupt, dimensions, 72, &[0, 0, 0]).is_err());
        assert!(validate_jpeg(&corrupt, dimensions, 72).is_err());

        let raced = folder.path().join("raced.png"); let checks = std::cell::Cell::new(0);
        let race = || { let count = checks.get(); checks.set(count + 1); if count == 1 { std::fs::write(&raced, b"racer owns this path").unwrap(); } false };
        assert!(write_page_image(&raced, dimensions, 72, &[0, 0, 0, 255], PageImageFormat::Png, race).is_err());
        assert_eq!(std::fs::read(&raced).unwrap(), b"racer owns this path");
    }

    #[test]
    fn disk_jpeg_is_lossy_rgb8_with_exact_density_and_never_overwritten() {
        let folder = tempfile::tempdir().unwrap();
        let dimensions = dimensions(32.0, 16.0, 72, PageImageFormat::Jpeg).unwrap();
        let mut bgra = Vec::with_capacity(dimensions.bgra_bytes);
        for y in 0..dimensions.height {
            for x in 0..dimensions.width {
                let pixel = if x < dimensions.width / 2 { [0, 0, 255, if y < 8 { 128 } else { 255 }] } else { [255, 0, 0, 255] };
                bgra.extend_from_slice(&pixel);
            }
        }
        for (dpi, extension) in [(72, "jpg"), (150, "jpeg"), (300, "jpg")] {
            let path = folder.path().join(format!("page-{dpi}.{extension}"));
            write_page_image(&path, dimensions, dpi, &bgra, PageImageFormat::Jpeg, || false).unwrap();
            let bytes = std::fs::read(&path).unwrap();
            assert!(bytes.starts_with(&[0xff, 0xd8, 0xff, 0xe0]));
            assert_eq!(jfif_density(&bytes), Some((1, dpi, dpi)));
            let decoder = image::codecs::jpeg::JpegDecoder::new(BufReader::new(File::open(&path).unwrap())).unwrap();
            assert_eq!(decoder.dimensions(), (32, 16));
            assert_eq!(decoder.color_type(), ColorType::Rgb8);
            let mut decoded = vec![0; decoder.total_bytes() as usize];
            decoder.read_image(&mut decoded).unwrap();
            let expected = bgra_to_rgb(dimensions, &bgra, PageImageFormat::Jpeg).unwrap();
            let differences = decoded.iter().zip(&expected).map(|(actual, expected)| actual.abs_diff(*expected)).collect::<Vec<_>>();
            let total_error = differences.iter().map(|difference| u64::from(*difference)).sum::<u64>();
            let max_error = differences.into_iter().max().unwrap();
            assert!(total_error <= expected.len() as u64 * 8, "quality-90 mean channel error exceeded 8");
            assert!(max_error <= 80, "quality-90 maximum channel error was {max_error}");
            assert!(decoded != expected, "JPEG output must exercise lossy encoding");
            let original = bytes.clone();
            assert!(write_page_image(&path, dimensions, dpi, &bgra, PageImageFormat::Jpeg, || false).unwrap_err().contains("never overwrites"));
            assert_eq!(std::fs::read(&path).unwrap(), original);
        }
        let wrong = folder.path().join("wrong.png");
        assert!(write_page_image(&wrong, dimensions, 72, &bgra, PageImageFormat::Jpeg, || false).is_err());
        assert!(!wrong.exists());
        let canceled = folder.path().join("canceled.jpg");
        assert!(write_page_image(&canceled, dimensions, 72, &bgra, PageImageFormat::Jpeg, || true).unwrap_err().contains("canceled"));
        assert!(!canceled.exists());
        let invalid = folder.path().join("invalid.jpg");
        assert!(write_page_image(&invalid, dimensions, 72, &[0, 0, 0], PageImageFormat::Jpeg, || false).is_err());
        assert!(!invalid.exists());
        let raced = folder.path().join("raced.jpg");
        let checks = std::cell::Cell::new(0);
        let race = || { let count = checks.get(); checks.set(count + 1); if count == 1 { std::fs::write(&raced, b"racer owns this path").unwrap(); } false };
        assert!(write_page_image(&raced, dimensions, 72, &bgra, PageImageFormat::Jpeg, race).is_err());
        assert_eq!(std::fs::read(&raced).unwrap(), b"racer owns this path");
    }
}
