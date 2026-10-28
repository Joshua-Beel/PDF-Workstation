use crate::searchable_pdf::is_supported_word_text;

const TSV_HEADER: &str =
    "level\tpage_num\tblock_num\tpar_num\tline_num\tword_num\tleft\ttop\twidth\theight\tconf\ttext";

pub const MAX_TSV_BYTES_PER_PAGE: usize = 1024 * 1024;
pub const MAX_WORDS_PER_PAGE: usize = 20_000;

#[derive(Clone, Debug, PartialEq)]
pub struct OcrWord {
    pub block: u32,
    pub paragraph: u32,
    pub line: u32,
    pub word: u32,
    pub left: u32,
    pub top: u32,
    pub width: u32,
    pub height: u32,
    pub line_top: u32,
    pub line_height: u32,
    pub confidence: f32,
    pub text: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ParsedTsvPage {
    pub raster_width: u32,
    pub raster_height: u32,
    pub words: Vec<OcrWord>,
    pub extracted_text: String,
}

/// A word placed on the displayed page: `x`/`width` are the word's own box,
/// `y`/`height` are its text line's band, so a thin glyph (a dash, a period)
/// shares the line's height and baseline the way tesseract's own PDF text
/// layer places words, and extraction keeps it on its line.
#[derive(Clone, Debug, PartialEq)]
pub struct DisplayWord {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
    pub confidence: f32,
    pub text: String,
}

pub fn parse_tesseract_tsv(
    input: &[u8],
    raster_width: u32,
    raster_height: u32,
) -> Result<ParsedTsvPage, String> {
    if input.len() > MAX_TSV_BYTES_PER_PAGE {
        return Err("OCR TSV exceeds the per-page byte limit".to_string());
    }
    if raster_width == 0 || raster_height == 0 {
        return Err("OCR raster dimensions must be positive".to_string());
    }
    if input.contains(&0) {
        return Err("OCR TSV contains a NUL byte".to_string());
    }
    let input = std::str::from_utf8(input)
        .map_err(|_| "OCR TSV is not valid UTF-8".to_string())?;

    let mut rows = input.lines();
    let header = rows
        .next()
        .map(strip_cr)
        .ok_or_else(|| "OCR TSV is empty".to_string())?;
    if header != TSV_HEADER {
        return Err("OCR TSV header does not match the supported profile".to_string());
    }

    let mut words = Vec::new();
    let mut extracted_text = String::new();
    let mut previous_position: Option<(u32, u32, u32, u32)> = None;
    let mut hierarchy = Hierarchy::default();

    for (index, raw_row) in rows.enumerate() {
        let row_number = index + 2;
        let row = strip_cr(raw_row);
        if row.is_empty() {
            return Err(format!("OCR TSV row {row_number} is empty"));
        }
        let fields: Vec<&str> = row.splitn(12, '\t').collect();
        if fields.len() != 12 {
            return Err(format!("OCR TSV row {row_number} does not have 12 fields"));
        }

        let level = parse_u32(fields[0], row_number, "level")?;
        let page = parse_u32(fields[1], row_number, "page_num")?;
        let block = parse_u32(fields[2], row_number, "block_num")?;
        let paragraph = parse_u32(fields[3], row_number, "par_num")?;
        let line = parse_u32(fields[4], row_number, "line_num")?;
        let word = parse_u32(fields[5], row_number, "word_num")?;
        let left = parse_u32(fields[6], row_number, "left")?;
        let top = parse_u32(fields[7], row_number, "top")?;
        let width = parse_u32(fields[8], row_number, "width")?;
        let height = parse_u32(fields[9], row_number, "height")?;
        let confidence = fields[10]
            .parse::<f32>()
            .map_err(|_| format!("OCR TSV row {row_number} has invalid confidence"))?;
        let text = fields[11];

        if !(1..=5).contains(&level) || page != 1 {
            return Err(format!(
                "OCR TSV row {row_number} is outside the supported profile"
            ));
        }
        if !confidence.is_finite() || !(-1.0..=100.0).contains(&confidence) {
            return Err(format!(
                "OCR TSV row {row_number} has out-of-range confidence"
            ));
        }
        let right = left
            .checked_add(width)
            .ok_or_else(|| format!("OCR TSV row {row_number} box overflows"))?;
        let bottom = top
            .checked_add(height)
            .ok_or_else(|| format!("OCR TSV row {row_number} box overflows"))?;
        if right > raster_width || bottom > raster_height {
            return Err(format!(
                "OCR TSV row {row_number} box is outside the raster"
            ));
        }

        let bounds = Bounds {
            left,
            top,
            width,
            height,
        };
        if level != 5 {
            if !text.is_empty() || confidence != -1.0 {
                return Err(format!(
                    "OCR TSV row {row_number} has unsupported structural content"
                ));
            }
            hierarchy.accept_structure(
                level,
                block,
                paragraph,
                line,
                word,
                bounds,
                raster_width,
                raster_height,
                row_number,
            )?;
            continue;
        }
        if block == 0 || paragraph == 0 || line == 0 || word == 0 || width == 0 || height == 0 {
            return Err(format!(
                "OCR TSV row {row_number} has an invalid word record"
            ));
        }
        // The engine sometimes leaves an ASCII space at the edge of a word's text
        // (seen on real scans); words are already split, so edges are trimmed
        // and a word that is only spaces is checked and then left out.
        let text = expand_latin_ligatures(text.trim_matches(' '));
        let text = text.as_str();
        if confidence < 0.0 || (!text.is_empty() && !is_supported_word_text(text)) {
            return Err(format!(
                "OCR TSV row {row_number} contains unsupported word text"
            ));
        }
        if words.len() >= MAX_WORDS_PER_PAGE {
            return Err("OCR TSV exceeds the per-page word limit".to_string());
        }

        let line_bounds = hierarchy.accept_word(block, paragraph, line, word, bounds, row_number)?;
        if text.is_empty() {
            continue;
        }

        let position = (block, paragraph, line, word);
        if let Some(previous) = previous_position {
            if position <= previous {
                return Err(format!("OCR TSV row {row_number} word order is ambiguous"));
            }
            if (block, paragraph, line) == (previous.0, previous.1, previous.2) {
                extracted_text.push(' ');
            } else {
                extracted_text.push('\n');
            }
        }
        extracted_text.push_str(text);
        previous_position = Some(position);
        words.push(OcrWord {
            block,
            paragraph,
            line,
            word,
            left,
            top,
            width,
            height,
            line_top: line_bounds.top,
            line_height: line_bounds.height,
            confidence,
            text: text.to_string(),
        });
    }

    if hierarchy.page.is_none() {
        return Err("OCR TSV does not contain its required page row".to_string());
    }
    if hierarchy.block.is_some() && hierarchy.last_word == 0 {
        return Err("OCR TSV ends with an incomplete page hierarchy".to_string());
    }

    Ok(ParsedTsvPage {
        raster_width,
        raster_height,
        words,
        extracted_text,
    })
}

pub fn map_words_to_display(
    page: &ParsedTsvPage,
    display_width: f64,
    display_height: f64,
) -> Result<Vec<DisplayWord>, String> {
    if !display_width.is_finite()
        || !display_height.is_finite()
        || display_width <= 0.0
        || display_height <= 0.0
    {
        return Err("displayed page dimensions must be finite and positive".to_string());
    }
    if page.raster_width == 0 || page.raster_height == 0 {
        return Err("OCR raster dimensions must be positive".to_string());
    }
    for word in &page.words {
        validate_word(word, page.raster_width, page.raster_height)?;
    }
    let x_scale = display_width / f64::from(page.raster_width);
    let y_scale = display_height / f64::from(page.raster_height);
    let mapped = page
        .words
        .iter()
        .map(|word| DisplayWord {
            x: f64::from(word.left) * x_scale,
            y: f64::from(word.line_top) * y_scale,
            width: f64::from(word.width) * x_scale,
            height: f64::from(word.line_height) * y_scale,
            confidence: word.confidence,
            text: word.text.clone(),
        })
        .collect::<Vec<_>>();
    if mapped.iter().any(|word| {
        !word.x.is_finite()
            || !word.y.is_finite()
            || !word.width.is_finite()
            || !word.height.is_finite()
    }) {
        return Err("mapped OCR geometry is not finite".to_string());
    }
    Ok(mapped)
}

#[derive(Clone, Copy, Debug, Default)]
struct Bounds {
    left: u32,
    top: u32,
    width: u32,
    height: u32,
}

impl Bounds {
    fn contains(self, child: Self) -> bool {
        let Some(right) = self.left.checked_add(self.width) else {
            return false;
        };
        let Some(bottom) = self.top.checked_add(self.height) else {
            return false;
        };
        let Some(child_right) = child.left.checked_add(child.width) else {
            return false;
        };
        let Some(child_bottom) = child.top.checked_add(child.height) else {
            return false;
        };
        child.left >= self.left
            && child.top >= self.top
            && child_right <= right
            && child_bottom <= bottom
    }

    /// Real engine output places word boxes outside their line box: on ten
    /// real scans by up to 17 px above, 12 px below, 138 px left and 38 px
    /// right, because the line box is the text row, not the union of its
    /// words. A word must still share at least one pixel row with its line
    /// (it already has to lie inside the page raster).
    fn overlaps_vertically(self, word: Self) -> bool {
        let (Some(bottom), Some(word_bottom)) = (
            self.top.checked_add(self.height),
            word.top.checked_add(word.height),
        ) else {
            return false;
        };
        word.top < bottom && self.top < word_bottom
    }

    /// With automatic page segmentation (`--psm 3`) paragraph and line boxes
    /// do not nest either: on real scans a line started 29 px left of its
    /// paragraph, a paragraph was 14 px wider than its block and a line box
    /// reached 43 px above its paragraph. A paragraph must still overlap its
    /// block and a line its paragraph (and every box lies inside the page).
    fn overlaps(self, child: Self) -> bool {
        let (Some(right), Some(bottom), Some(child_right), Some(child_bottom)) = (
            self.left.checked_add(self.width),
            self.top.checked_add(self.height),
            child.left.checked_add(child.width),
            child.top.checked_add(child.height),
        ) else {
            return false;
        };
        child.left < right && self.left < child_right && child.top < bottom && self.top < child_bottom
    }
}

/// Raster resolution the OCR page images are rendered at (matches `ocr.rs`).
#[allow(dead_code)]
pub(crate) const RASTER_DPI: u32 = 150;

#[derive(Default)]
struct Hierarchy {
    page: Option<Bounds>,
    block: Option<(u32, Bounds)>,
    paragraph: Option<(u32, Bounds)>,
    line: Option<(u32, Bounds)>,
    last_block: u32,
    last_paragraph: u32,
    last_line: u32,
    last_word: u32,
}

impl Hierarchy {
    #[allow(clippy::too_many_arguments)]
    fn accept_structure(
        &mut self,
        level: u32,
        block: u32,
        paragraph: u32,
        line: u32,
        word: u32,
        bounds: Bounds,
        raster_width: u32,
        raster_height: u32,
        row: usize,
    ) -> Result<(), String> {
        match level {
            1 if self.page.is_none()
                && block == 0
                && paragraph == 0
                && line == 0
                && word == 0
                && bounds.left == 0
                && bounds.top == 0
                && bounds.width == raster_width
                && bounds.height == raster_height =>
            {
                self.page = Some(bounds);
                Ok(())
            }
            2 if self.page.is_some()
                && self.last_block.checked_add(1) == Some(block)
                && paragraph == 0
                && line == 0
                && word == 0
                && self.page.unwrap().contains(bounds) =>
            {
                self.block = Some((block, bounds));
                self.paragraph = None;
                self.line = None;
                self.last_block = block;
                self.last_paragraph = 0;
                self.last_line = 0;
                self.last_word = 0;
                Ok(())
            }
            3 if self.block.is_some()
                && block == self.last_block
                && self.last_paragraph.checked_add(1) == Some(paragraph)
                && line == 0
                && word == 0
                && self.block.unwrap().1.overlaps(bounds) =>
            {
                self.paragraph = Some((paragraph, bounds));
                self.line = None;
                self.last_paragraph = paragraph;
                self.last_line = 0;
                self.last_word = 0;
                Ok(())
            }
            4 if self.paragraph.is_some()
                && block == self.last_block
                && paragraph == self.last_paragraph
                && self.last_line.checked_add(1) == Some(line)
                && word == 0
                && self.paragraph.unwrap().1.overlaps(bounds) =>
            {
                self.line = Some((line, bounds));
                self.last_line = line;
                self.last_word = 0;
                Ok(())
            }
            _ => Err(format!(
                "OCR TSV row {row} violates the required page hierarchy"
            )),
        }
    }

    fn accept_word(
        &mut self,
        block: u32,
        paragraph: u32,
        line: u32,
        word: u32,
        bounds: Bounds,
        row: usize,
    ) -> Result<Bounds, String> {
        let valid = self.line.is_some()
            && block == self.last_block
            && paragraph == self.last_paragraph
            && line == self.last_line
            && self.last_word.checked_add(1) == Some(word)
            && self.line.unwrap().1.overlaps_vertically(bounds);
        if !valid {
            return Err(format!(
                "OCR TSV row {row} violates the required word hierarchy"
            ));
        }
        self.last_word = word;
        Ok(self.line.unwrap().1)
    }
}

fn validate_word(word: &OcrWord, raster_width: u32, raster_height: u32) -> Result<(), String> {
    if word.block == 0
        || word.paragraph == 0
        || word.line == 0
        || word.word == 0
        || word.width == 0
        || word.height == 0
        || !word.confidence.is_finite()
        || !(0.0..=100.0).contains(&word.confidence)
        || !is_supported_word_text(&word.text)
    {
        return Err("OCR word is outside the supported profile".to_string());
    }
    let right = word
        .left
        .checked_add(word.width)
        .ok_or_else(|| "OCR word box overflows".to_string())?;
    let bottom = word
        .top
        .checked_add(word.height)
        .ok_or_else(|| "OCR word box overflows".to_string())?;
    if right > raster_width || bottom > raster_height {
        return Err("OCR word box is outside the raster".to_string());
    }
    let line_bottom = word
        .line_top
        .checked_add(word.line_height)
        .ok_or_else(|| "OCR line box overflows".to_string())?;
    if word.line_height == 0
        || line_bottom > raster_height
        || word.top >= line_bottom
        || bottom <= word.line_top
    {
        return Err("OCR word is outside its line band".to_string());
    }
    Ok(())
}

/// PDF text extraction (PDFium included) reports the Latin presentation-form
/// ligatures U+FB00..U+FB06 as their letter sequences, so the text layer and
/// its extraction oracle carry the letters instead of the ligature code point.
pub fn expand_latin_ligatures(text: &str) -> String {
    let mut expanded = String::with_capacity(text.len());
    for character in text.chars() {
        match character {
            '\u{FB00}' => expanded.push_str("ff"),
            '\u{FB01}' => expanded.push_str("fi"),
            '\u{FB02}' => expanded.push_str("fl"),
            '\u{FB03}' => expanded.push_str("ffi"),
            '\u{FB04}' => expanded.push_str("ffl"),
            '\u{FB05}' => expanded.push_str("\u{17F}t"),
            '\u{FB06}' => expanded.push_str("st"),
            other => expanded.push(other),
        }
    }
    expanded
}

fn parse_u32(value: &str, row: usize, field: &str) -> Result<u32, String> {
    value
        .parse::<u32>()
        .map_err(|_| format!("OCR TSV row {row} has invalid {field}"))
}

fn strip_cr(value: &str) -> &str {
    value.strip_suffix('\r').unwrap_or(value)
}
