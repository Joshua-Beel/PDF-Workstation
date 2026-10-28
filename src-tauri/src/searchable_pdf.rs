use std::collections::HashMap;

use lopdf::{content::{Content, Operation}, dictionary, Dictionary, Document, Object, Stream, StringFormat};

pub const MAX_SEARCHABLE_PAGES: usize = 4_096;
pub const MAX_SEARCHABLE_PIXELS: u64 = 32 * 1024 * 1024;
pub const MAX_SEARCHABLE_WORDS: usize = 100_000;
pub const MAX_SEARCHABLE_TEXT_BYTES: usize = 8 * 1024 * 1024;
pub const MAX_SEARCHABLE_OUTPUT_BYTES: usize = 256 * 1024 * 1024;
pub const MAX_SEARCHABLE_WORD_CHARS: usize = 512;
pub const MAX_SEARCHABLE_DISTINCT_CHARS: usize = 8_192;

pub(crate) const FONT_NAME: &str = "InvisibleOcrText";
pub(crate) const GLYPH_ADVANCE: i64 = 500;
pub(crate) const FONT_ASCENT: i64 = 800;
pub(crate) const FONT_DESCENT: i64 = -200;

/// Characters the invisible text layer accepts: any Unicode scalar value the
/// engine emits except controls, whitespace (words are already split),
/// invisible format/bidi controls, private-use code points, noncharacters and
/// U+FFFD (the engine's decoding-failure marker). Every accepted character is
/// carried through the font's /ToUnicode CMap, including those outside the BMP.
pub fn is_supported_word_char(character: char) -> bool {
    let code = u32::from(character);
    !(character.is_control()
        || character.is_whitespace()
        || matches!(code,
            0x00AD | 0x034F | 0x061C | 0x115F | 0x1160 | 0x17B4 | 0x17B5 | 0x180B..=0x180F
            | 0x200B..=0x200F | 0x202A..=0x202E | 0x2060..=0x206F | 0x3164 | 0xFE00..=0xFE0F
            | 0xFEFF | 0xFFA0 | 0xFFF0..=0xFFFD | 0x1BCA0..=0x1BCA3 | 0x1D173..=0x1D17A
            | 0x0600..=0x0605 | 0x06DD | 0x070F | 0x0890 | 0x0891 | 0x08E2 | 0x110BD | 0x110CD | 0x13430..=0x1343F
            | 0xE0000..=0xE0FFF | 0xE000..=0xF8FF | 0xF0000..=0x10FFFF | 0xFDD0..=0xFDEF)
        || code & 0xFFFE == 0xFFFE)
}

pub fn is_supported_word_text(text: &str) -> bool {
    let mut count = 0_usize;
    for character in text.chars() {
        count += 1;
        if count > MAX_SEARCHABLE_WORD_CHARS || !is_supported_word_char(character) {
            return false;
        }
    }
    count > 0
}

#[derive(Clone, Debug, PartialEq)]
pub struct SearchableWord {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
    pub text: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SearchableRasterPage {
    pub page_width: f32,
    pub page_height: f32,
    pub image_width: u32,
    pub image_height: u32,
    pub rgb: Vec<u8>,
    pub words: Vec<SearchableWord>,
}

pub fn write_searchable_pdf(pages: &[SearchableRasterPage]) -> Result<Vec<u8>, String> {
    validate_pages(pages)?;
    let bytes = assemble(pages)?;
    if bytes.len() > MAX_SEARCHABLE_OUTPUT_BYTES {
        return Err("searchable PDF exceeds the output byte limit".to_string());
    }
    verify_searchable_pdf(&bytes, pages)?;
    Ok(bytes)
}

pub fn verify_searchable_pdf(
    bytes: &[u8],
    expected_pages: &[SearchableRasterPage],
) -> Result<(), String> {
    validate_pages(expected_pages)?;
    if bytes.len() > MAX_SEARCHABLE_OUTPUT_BYTES {
        return Err("searchable PDF exceeds the output byte limit".to_string());
    }
    Document::load_mem(bytes)
        .map_err(|error| format!("searchable PDF cannot be reopened: {error}"))?;
    let canonical = assemble(expected_pages)?;
    if canonical != bytes {
        return Err("candidate is not the exact trusted searchable-PDF output".to_string());
    }
    Ok(())
}

fn validate_pages(pages: &[SearchableRasterPage]) -> Result<(), String> {
    if pages.is_empty() || pages.len() > MAX_SEARCHABLE_PAGES {
        return Err("searchable PDF page count is outside the supported range".to_string());
    }
    let mut pixels = 0_u64;
    let mut words = 0_usize;
    let mut text_bytes = 0_usize;
    for (index, page) in pages.iter().enumerate() {
        let label = index + 1;
        if !page.page_width.is_finite()
            || !page.page_height.is_finite()
            || page.page_width <= 0.0
            || page.page_height <= 0.0
            || page.image_width == 0
            || page.image_height == 0
        {
            return Err(format!("searchable PDF page {label} has invalid dimensions"));
        }
        let page_pixels = u64::from(page.image_width)
            .checked_mul(u64::from(page.image_height))
            .ok_or_else(|| format!("searchable PDF page {label} pixel count overflows"))?;
        pixels = pixels
            .checked_add(page_pixels)
            .ok_or_else(|| "searchable PDF aggregate pixel count overflows".to_string())?;
        if pixels > MAX_SEARCHABLE_PIXELS {
            return Err("searchable PDF exceeds the aggregate pixel limit".to_string());
        }
        let rgb_len = page_pixels
            .checked_mul(3)
            .and_then(|value| usize::try_from(value).ok())
            .ok_or_else(|| format!("searchable PDF page {label} RGB length overflows"))?;
        if page.rgb.len() != rgb_len {
            return Err(format!("searchable PDF page {label} has an invalid RGB length"));
        }
        words = words
            .checked_add(page.words.len())
            .ok_or_else(|| "searchable PDF word count overflows".to_string())?;
        if words > MAX_SEARCHABLE_WORDS {
            return Err("searchable PDF exceeds the aggregate word limit".to_string());
        }
        for word in &page.words {
            validate_word(word, f64::from(page.page_width), f64::from(page.page_height), label)?;
            text_bytes = text_bytes
                .checked_add(word.text.len())
                .ok_or_else(|| "searchable PDF text length overflows".to_string())?;
            if text_bytes > MAX_SEARCHABLE_TEXT_BYTES {
                return Err("searchable PDF exceeds the aggregate text limit".to_string());
            }
        }
    }
    CharacterIds::build(pages)?;
    Ok(())
}

pub(crate) fn validate_word(word: &SearchableWord, page_width: f64, page_height: f64, page: usize) -> Result<(), String> {
    if !word.x.is_finite()
        || !word.y.is_finite()
        || !word.width.is_finite()
        || !word.height.is_finite()
        || word.x < 0.0
        || word.y < 0.0
        || word.width <= 0.0
        || word.height <= 0.0
        || word.x + word.width > page_width
        || word.y + word.height > page_height
    {
        return Err(format!("searchable PDF page {page} has invalid word geometry"));
    }
    if !is_supported_word_text(&word.text) {
        return Err(format!("searchable PDF page {page} has unsupported word text"));
    }
    Ok(())
}

fn assemble(pages: &[SearchableRasterPage]) -> Result<Vec<u8>, String> {
    let ids = CharacterIds::build(pages)?;
    let mut document = Document::with_version("1.7");
    let pages_id = document.new_object_id();
    let font_id = add_invisible_font(&mut document, &ids)?;
    let mut kids = Vec::with_capacity(pages.len());
    for page in pages {
        let mut image = Stream::new(
            dictionary! {
                "Type" => "XObject", "Subtype" => "Image",
                "Width" => i64::from(page.image_width), "Height" => i64::from(page.image_height),
                "ColorSpace" => "DeviceRGB", "BitsPerComponent" => 8,
            },
            page.rgb.clone(),
        );
        image.compress().map_err(|error| format!("searchable image compression failed: {error}"))?;
        let image_id = document.add_object(image);
        let mut operations = vec![
            Operation::new("q", vec![]),
            Operation::new("cm", vec![Object::Real(page.page_width), 0.into(), 0.into(), Object::Real(page.page_height), 0.into(), 0.into()]),
            Operation::new("Do", vec![Object::Name(b"Image".to_vec())]),
            Operation::new("Q", vec![]),
            Operation::new("BT", vec![]),
            Operation::new("Tf", vec![Object::Name(b"F1".to_vec()), 1.into()]),
            Operation::new("Tr", vec![3.into()]),
        ];
        for word in &page.words {
            let mut codes = Vec::with_capacity(word.text.len() * 2);
            let mut glyphs = 0_u32;
            for character in word.text.chars() {
                let id = ids.id(character).ok_or("searchable word character has no font code")?;
                codes.extend_from_slice(&id.to_be_bytes());
                glyphs += 1;
            }
            let nominal_width = f64::from(glyphs) * GLYPH_ADVANCE as f64 / 1000.0;
            let em_height = (FONT_ASCENT - FONT_DESCENT) as f64 / 1000.0;
            let vertical_scale = word.height / em_height;
            let horizontal_scale = word.width / nominal_width;
            let baseline = f64::from(page.page_height) - word.y - word.height
                - FONT_DESCENT as f64 / 1000.0 * vertical_scale;
            operations.push(Operation::new("Tm", vec![
                real(horizontal_scale)?, 0.into(), 0.into(), real(vertical_scale)?,
                real(word.x)?, real(baseline)?,
            ]));
            operations.push(Operation::new("Tj", vec![Object::String(codes, StringFormat::Hexadecimal)]));
        }
        operations.push(Operation::new("ET", vec![]));
        let content = Content { operations }.encode()
            .map_err(|error| format!("searchable page encoding failed: {error}"))?;
        let mut content = Stream::new(Dictionary::new(), content);
        content.compress().map_err(|error| format!("searchable text compression failed: {error}"))?;
        let content_id = document.add_object(content);
        let page_id = document.add_object(dictionary! {
            "Type" => "Page", "Parent" => pages_id,
            "MediaBox" => vec![0.into(), 0.into(), Object::Real(page.page_width), Object::Real(page.page_height)],
            "Resources" => dictionary! {
                "XObject" => dictionary! { "Image" => image_id },
                "Font" => dictionary! { "F1" => font_id },
            },
            "Contents" => content_id,
        });
        kids.push(page_id.into());
    }
    document.objects.insert(pages_id, dictionary! {
        "Type" => "Pages", "Count" => kids.len() as i64, "Kids" => kids,
    }.into());
    let catalog = document.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages_id });
    document.trailer.set("Root", catalog);
    let mut bytes = Vec::new();
    document.save_to(&mut bytes)
        .map_err(|error| format!("searchable PDF serialization failed: {error}"))?;
    Ok(bytes)
}

fn real(value: f64) -> Result<Object, String> {
    if !value.is_finite() || value < f64::from(f32::MIN) || value > f64::from(f32::MAX) {
        return Err("searchable text transform is outside the supported range".to_string());
    }
    let converted = value as f32;
    if !converted.is_finite() || (value != 0.0 && converted == 0.0) {
        return Err("searchable text transform cannot be represented exactly enough".to_string());
    }
    Ok(Object::Real(converted))
}

/// Per-document two-byte character codes, assigned from 1 in first-use order so
/// the output stays byte-for-byte reproducible for the canonical oracle.
struct CharacterIds {
    characters: Vec<char>,
    ids: HashMap<char, u16>,
}

impl CharacterIds {
    fn build(pages: &[SearchableRasterPage]) -> Result<Self, String> {
        let mut characters = Vec::new();
        let mut ids = HashMap::new();
        for character in pages.iter().flat_map(|page| &page.words).flat_map(|word| word.text.chars()) {
            if ids.contains_key(&character) {
                continue;
            }
            if characters.len() >= MAX_SEARCHABLE_DISTINCT_CHARS {
                return Err("searchable PDF exceeds the distinct character limit".to_string());
            }
            let id = u16::try_from(characters.len() + 1)
                .map_err(|_| "searchable PDF exceeds the distinct character limit".to_string())?;
            ids.insert(character, id);
            characters.push(character);
        }
        Ok(Self { characters, ids })
    }

    fn id(&self, character: char) -> Option<u16> {
        self.ids.get(&character).copied()
    }
}

fn add_invisible_font(document: &mut Document, ids: &CharacterIds) -> Result<lopdf::ObjectId, String> {
    let program = glyphless_truetype();
    let mut font_file = Stream::new(dictionary! { "Length1" => program.len() as i64 }, program);
    font_file.compress().map_err(|error| format!("searchable font compression failed: {error}"))?;
    let font_file_id = document.add_object(font_file);
    let descriptor_id = document.add_object(dictionary! {
        "Type" => "FontDescriptor", "FontName" => FONT_NAME, "Flags" => 5,
        "FontBBox" => vec![0.into(), FONT_DESCENT.into(), GLYPH_ADVANCE.into(), FONT_ASCENT.into()],
        "ItalicAngle" => 0, "Ascent" => FONT_ASCENT, "Descent" => FONT_DESCENT,
        "CapHeight" => FONT_ASCENT, "StemV" => 80, "FontFile2" => font_file_id,
    });
    let mut gid_map = vec![0_u8; (ids.characters.len() + 1) * 2];
    for pair in gid_map.chunks_exact_mut(2).skip(1) {
        pair[1] = 1;
    }
    let mut gid_map = Stream::new(Dictionary::new(), gid_map);
    gid_map.compress().map_err(|error| format!("searchable font compression failed: {error}"))?;
    let gid_map_id = document.add_object(gid_map);
    let cid_font_id = document.add_object(dictionary! {
        "Type" => "Font", "Subtype" => "CIDFontType2", "BaseFont" => FONT_NAME,
        "CIDSystemInfo" => dictionary! {
            "Registry" => Object::string_literal("Adobe"),
            "Ordering" => Object::string_literal("Identity"),
            "Supplement" => 0,
        },
        "FontDescriptor" => descriptor_id, "DW" => GLYPH_ADVANCE, "CIDToGIDMap" => gid_map_id,
    });
    let mut to_unicode = Stream::new(Dictionary::new(), to_unicode_cmap(&ids.characters).into_bytes());
    to_unicode.compress().map_err(|error| format!("searchable ToUnicode compression failed: {error}"))?;
    let to_unicode_id = document.add_object(to_unicode);
    Ok(document.add_object(dictionary! {
        "Type" => "Font", "Subtype" => "Type0", "BaseFont" => FONT_NAME,
        "Encoding" => "Identity-H", "DescendantFonts" => vec![cid_font_id.into()],
        "ToUnicode" => to_unicode_id,
    }))
}

pub(crate) fn to_unicode_cmap(characters: &[char]) -> String {
    use std::fmt::Write;
    let mut cmap = String::from(
        "/CIDInit /ProcSet findresource begin\n12 dict begin\nbegincmap\n\
         /CIDSystemInfo << /Registry (Adobe) /Ordering (UCS) /Supplement 0 >> def\n\
         /CMapName /Adobe-Identity-UCS def\n/CMapType 2 def\n\
         1 begincodespacerange\n<0000> <FFFF>\nendcodespacerange\n",
    );
    for (block, chunk) in characters.chunks(100).enumerate() {
        let _ = writeln!(cmap, "{} beginbfchar", chunk.len());
        for (offset, character) in chunk.iter().enumerate() {
            let id = block * 100 + offset + 1;
            let mut units = [0_u16; 2];
            let destination = character.encode_utf16(&mut units).iter()
                .map(|unit| format!("{unit:04X}"))
                .collect::<String>();
            let _ = writeln!(cmap, "<{id:04X}> <{destination}>");
        }
        cmap.push_str("endbfchar\n");
    }
    cmap.push_str("endcmap\nCMapName currentdict /CMap defineresource pop\nend\nend\n");
    cmap
}

/// A minimal TrueType program: an empty .notdef and one shared glyph whose
/// outline is the full 500 x 1000 unit cell. Every character code maps to that
/// glyph through /CIDToGIDMap. The text is drawn in render mode 3, so the box
/// never paints; it exists so viewers give each character, including a
/// one-character word, a non-empty box (PDFium drops a text object whose only
/// glyph has no outline). The characters come only from /ToUnicode.
pub(crate) fn glyphless_truetype() -> Vec<u8> {
    fn be16(out: &mut Vec<u8>, values: &[i32]) {
        for value in values {
            out.extend_from_slice(&(*value as u16).to_be_bytes());
        }
    }
    fn be32(out: &mut Vec<u8>, value: u32) {
        out.extend_from_slice(&value.to_be_bytes());
    }
    let advance = GLYPH_ADVANCE as i32;
    let (ascent, descent) = (FONT_ASCENT as i32, FONT_DESCENT as i32);

    let mut cmap = Vec::new();
    be16(&mut cmap, &[0, 1, 3, 1]);
    be32(&mut cmap, 12);
    be16(&mut cmap, &[4, 24, 0, 2, 2, 0, 0, 0xFFFF, 0, 0xFFFF, 1, 0]);

    let mut glyf = Vec::new();
    be16(&mut glyf, &[0; 6]);
    be16(&mut glyf, &[1, 0, descent, advance, ascent, 3, 0]);
    glyf.extend_from_slice(&[1; 4]);
    be16(&mut glyf, &[0, 0, advance, 0, descent, ascent - descent, 0, descent - ascent, 0]);

    let mut head = Vec::new();
    be32(&mut head, 0x0001_0000);
    be32(&mut head, 0x0001_0000);
    be32(&mut head, 0);
    be32(&mut head, 0x5F0F_3CF5);
    be16(&mut head, &[0x000B, 1000]);
    head.extend_from_slice(&[0; 16]);
    be16(&mut head, &[0, descent, advance, ascent, 0, 3, 2, 0, 0]);

    let mut hhea = Vec::new();
    be32(&mut hhea, 0x0001_0000);
    be16(&mut hhea, &[ascent, descent, 0, advance, 0, 0, advance, 1, 0, 0, 0, 0, 0, 0, 0, 2]);

    let mut hmtx = Vec::new();
    be16(&mut hmtx, &[advance, 0, advance, 0]);

    let mut loca = Vec::new();
    be16(&mut loca, &[0, 6, 24]);

    let mut maxp = Vec::new();
    be32(&mut maxp, 0x0001_0000);
    be16(&mut maxp, &[2, 4, 1, 0, 0, 2, 0, 0, 0, 0, 0, 0, 0, 0]);

    let name_text = FONT_NAME.encode_utf16().flat_map(u16::to_be_bytes).collect::<Vec<_>>();
    let mut name = Vec::new();
    be16(&mut name, &[0, 2, 30]);
    be16(&mut name, &[3, 1, 0x0409, 1, name_text.len() as i32, 0]);
    be16(&mut name, &[3, 1, 0x0409, 6, name_text.len() as i32, 0]);
    name.extend_from_slice(&name_text);

    let mut post = Vec::new();
    be32(&mut post, 0x0003_0000);
    be32(&mut post, 0);
    be16(&mut post, &[-100, 50]);
    be32(&mut post, 1);
    post.extend_from_slice(&[0; 16]);

    let tables: [(&[u8; 4], Vec<u8>); 9] = [
        (b"cmap", cmap), (b"glyf", glyf), (b"head", head), (b"hhea", hhea), (b"hmtx", hmtx),
        (b"loca", loca), (b"maxp", maxp), (b"name", name), (b"post", post),
    ];
    let checksum = |data: &[u8]| {
        data.chunks(4).fold(0_u32, |sum, chunk| {
            let mut word = [0_u8; 4];
            word[..chunk.len()].copy_from_slice(chunk);
            sum.wrapping_add(u32::from_be_bytes(word))
        })
    };
    let mut font = Vec::new();
    be32(&mut font, 0x0001_0000);
    be16(&mut font, &[tables.len() as i32, 128, 3, 16]);
    let mut offset = 12 + 16 * tables.len();
    let mut head_offset = 0;
    for (tag, data) in &tables {
        font.extend_from_slice(*tag);
        be32(&mut font, checksum(data));
        be32(&mut font, offset as u32);
        be32(&mut font, data.len() as u32);
        if *tag == b"head" {
            head_offset = offset;
        }
        offset += data.len().div_ceil(4) * 4;
    }
    for (_, data) in &tables {
        font.extend_from_slice(data);
        font.resize(font.len().div_ceil(4) * 4, 0);
    }
    let adjustment = 0xB1B0_AFBA_u32.wrapping_sub(checksum(&font));
    font[head_offset + 8..head_offset + 12].copy_from_slice(&adjustment.to_be_bytes());
    font
}
