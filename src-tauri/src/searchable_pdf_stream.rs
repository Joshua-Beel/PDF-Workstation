//! Streaming searchable-PDF writer for large OCR jobs.
//!
//! Pages are written to `output.part` one at a time, in page order, and only a
//! small per-page record stays in memory. After each page the part file is
//! flushed to disk and then one journal record is appended and flushed, so a
//! journal record never points past bytes that are on disk. A crash, a kill or
//! a power loss leaves a state that `resume` can truncate back to the last
//! complete page. The output is deterministic: the same pages give the same
//! bytes whether or not the job was interrupted.
//!
//! Object layout (ids are a pure function of the page index and count):
//! 1 catalog, 2 page-tree root, 3-8 the shared invisible font, then three
//! objects per page (image, content, page) and finally the intermediate
//! page-tree nodes (fan-out 128). The font, tree, xref and trailer are written
//! once, at finalisation; until then the header is `%PDW-PARTIAL`, so a part
//! file can never be opened as a PDF.

use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use lopdf::{Dictionary, Stream};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::searchable_pdf::{
    glyphless_truetype, to_unicode_cmap, validate_word, SearchableWord, FONT_ASCENT, FONT_DESCENT, FONT_NAME,
    GLYPH_ADVANCE, MAX_SEARCHABLE_DISTINCT_CHARS,
};

pub(crate) const MAX_STREAM_PAGES: u32 = 10_000;
pub(crate) const MAX_STREAM_OUTPUT_BYTES: u64 = 1024 * 1024 * 1024;
pub(crate) const MAX_PAGE_PIXEL_BYTES: usize = 16 * 1024 * 1024;
const MAX_CONTENT_BYTES: usize = 8 * 1024 * 1024;
const TREE_FAN_OUT: u32 = 128;
const FIRST_PAGE_OBJECT: u32 = 9;
const PARTIAL_HEADER: &[u8; 16] = b"%PDW-PARTIAL-1\n\n";
const FINAL_HEADER: &[u8; 16] = b"%PDF-1.7\n%\xE2\xE3\xCF\xD3\n\n";
const RECORD_MAGIC: &[u8; 4] = b"PJR1";
const RECORD_FIXED_PAYLOAD: usize = 4 + 1 + 1 + 1 + 8 * 4 + 32 + 32 + 4 + 4 + 4;
const RECORD_MAX_PAYLOAD: usize = RECORD_FIXED_PAYLOAD + 4 * MAX_SEARCHABLE_DISTINCT_CHARS;
const MANIFEST_LIMIT: u64 = 64 * 1024;
/// Pages are flushed to the OS after every page (a killed app loses nothing
/// that was written), and made durable with at most two disk flushes per this
/// interval: the part file first, then the journal records that point into it.
const SYNC_INTERVAL: std::time::Duration = std::time::Duration::from_secs(1);
pub(crate) const MANIFEST_FORMAT: u32 = 1;

pub(crate) const PART_FILE: &str = "output.part";
pub(crate) const JOURNAL_FILE: &str = "journal";
pub(crate) const MANIFEST_FILE: &str = "manifest.json";

/// What a finished page holds.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum PageOutcome {
    /// Image and invisible text.
    Text,
    /// Image kept, text skipped (the reason says why).
    ImageOnly,
    /// The page could not be rendered: a white page of the same size.
    Placeholder,
}

/// Why a page has no text, or no image.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum PageIssue {
    None,
    TimedOut,
    EngineStopped,
    EngineOutputRefused,
    RenderFailed,
    TooManyCharacters,
}

impl PageOutcome {
    fn code(self) -> u8 { match self { Self::Text => 0, Self::ImageOnly => 1, Self::Placeholder => 2 } }
    fn from_code(code: u8) -> Option<Self> { [Self::Text, Self::ImageOnly, Self::Placeholder].get(usize::from(code)).copied() }
}

impl PageIssue {
    fn code(self) -> u8 {
        match self { Self::None => 0, Self::TimedOut => 1, Self::EngineStopped => 2, Self::EngineOutputRefused => 3, Self::RenderFailed => 4, Self::TooManyCharacters => 5 }
    }
    fn from_code(code: u8) -> Option<Self> {
        [Self::None, Self::TimedOut, Self::EngineStopped, Self::EngineOutputRefused, Self::RenderFailed, Self::TooManyCharacters].get(usize::from(code)).copied()
    }
}

/// A page image encoded in its smallest exact colour space (lossless).
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct EncodedImage {
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) bits: u8,
    pub(crate) colors: u8,
    pub(crate) predicted: bool,
    pub(crate) compressed: bool,
    pub(crate) data: Vec<u8>,
    /// SHA-256 of the white-composited RGB pixels the image reproduces exactly.
    pub(crate) pixel_sha256: [u8; 32],
}

/// One finished page, ready for the writer.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct StreamPage {
    pub(crate) page_width: f32,
    pub(crate) page_height: f32,
    pub(crate) image: EncodedImage,
    pub(crate) words: Vec<SearchableWord>,
    pub(crate) outcome: PageOutcome,
    pub(crate) issue: PageIssue,
    pub(crate) reduced_resolution: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct JobManifest {
    pub(crate) format: u32,
    pub(crate) page_count: u32,
    /// Opaque identity of what is being OCR'd (source bytes and page plan), hex.
    pub(crate) source_identity: String,
    pub(crate) source_name: String,
    pub(crate) destination: String,
    pub(crate) dpi: u16,
    pub(crate) language: String,
    pub(crate) engine_identity: String,
}

/// The small per-page record kept for the whole job (and in the journal).
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct PageRecord {
    pub(crate) page: u32,
    pub(crate) outcome: PageOutcome,
    pub(crate) issue: PageIssue,
    pub(crate) reduced_resolution: bool,
    image_offset: u64,
    content_offset: u64,
    page_offset: u64,
    end_offset: u64,
    pixel_sha256: [u8; 32],
    text_sha256: [u8; 32],
    pub(crate) words: u32,
    pub(crate) dropped_words: u32,
    new_characters: Vec<char>,
}

#[derive(Debug)]
pub(crate) enum StreamError {
    /// Writing failed (disk full, file locked, removed folder...). The checkpoint is consistent.
    Io(String),
    /// The partial copy would pass the 1 GiB limit.
    SizeLimit,
    /// The saved progress cannot be used (missing, foreign, inconsistent or tampered).
    Unusable(String),
    /// Invalid input from the caller.
    Invalid(String),
    /// The finished copy failed its check from disk; nothing was published.
    CheckFailed(String),
    /// The destination already exists; nothing was published.
    DestinationExists,
    /// The caller's commit barrier refused (cancelled or paused).
    NotCommitted(String),
}

impl std::fmt::Display for StreamError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(message) => write!(formatter, "Could not write the OCR copy: {message}"),
            Self::SizeLimit => formatter.write_str("The copy would be larger than 1 GiB, the largest file PDF Workstation can open."),
            Self::Unusable(message) => write!(formatter, "The saved OCR progress can't be used: {message}"),
            Self::Invalid(message) => formatter.write_str(message),
            Self::CheckFailed(message) => write!(formatter, "The copy did not pass its final check. Nothing was saved. ({message})"),
            Self::DestinationExists => formatter.write_str("That file already exists. Choose a new filename; Save a Copy never overwrites an existing file."),
            Self::NotCommitted(message) => formatter.write_str(message),
        }
    }
}

fn io(error: std::io::Error) -> StreamError { StreamError::Io(error.to_string()) }

pub(crate) fn sha256(bytes: &[u8]) -> [u8; 32] { Sha256::digest(bytes).into() }

pub(crate) fn hex(bytes: &[u8]) -> String { bytes.iter().map(|byte| format!("{byte:02x}")).collect() }

/// Encodes white-composited RGB pixels losslessly in the smallest exact colour
/// space: 1-bit gray when every pixel is pure black or white, 8-bit gray when
/// every pixel has R = G = B, otherwise RGB. Flate with the PNG Up predictor.
pub(crate) fn encode_image(width: u32, height: u32, rgb: &[u8]) -> Result<EncodedImage, StreamError> {
    let pixels = usize::try_from(u64::from(width) * u64::from(height)).map_err(|_| StreamError::Invalid("OCR page image is too large.".into()))?;
    if width == 0 || height == 0 || pixels.checked_mul(3) != Some(rgb.len()) || rgb.len() > MAX_PAGE_PIXEL_BYTES {
        return Err(StreamError::Invalid("OCR page image has an invalid size.".into()));
    }
    let pixel_sha256 = sha256(rgb);
    let gray = rgb.chunks_exact(3).all(|pixel| pixel[0] == pixel[1] && pixel[1] == pixel[2]);
    let bilevel = gray && rgb.chunks_exact(3).all(|pixel| pixel[0] == 0 || pixel[0] == 255);
    let width_usize = width as usize;
    let (bits, colors, row_bytes) = if bilevel { (1_u8, 1_u8, width_usize.div_ceil(8)) } else if gray { (8, 1, width_usize) } else { (8, 3, width_usize * 3) };
    let mut raw = Vec::new();
    raw.try_reserve_exact(row_bytes * height as usize).map_err(|_| StreamError::Invalid("Could not allocate the OCR page image.".into()))?;
    for row in rgb.chunks_exact(width_usize * 3) {
        if bilevel {
            let start = raw.len();
            raw.resize(start + row_bytes, 0);
            for (x, pixel) in row.chunks_exact(3).enumerate() {
                if pixel[0] == 255 { raw[start + x / 8] |= 0x80 >> (x % 8); }
            }
        } else if gray {
            raw.extend(row.chunks_exact(3).map(|pixel| pixel[0]));
        } else {
            raw.extend_from_slice(row);
        }
    }
    let mut predicted = Vec::new();
    predicted.try_reserve_exact((row_bytes + 1) * height as usize).map_err(|_| StreamError::Invalid("Could not allocate the OCR page image.".into()))?;
    let mut previous: &[u8] = &[];
    for row in raw.chunks_exact(row_bytes) {
        predicted.push(2);
        if previous.is_empty() { predicted.extend_from_slice(row); } else { predicted.extend(row.iter().zip(previous).map(|(current, above)| current.wrapping_sub(*above))); }
        previous = row;
    }
    let mut stream = Stream::new(Dictionary::new(), predicted);
    stream.compress().map_err(|error| StreamError::Invalid(format!("OCR image compression failed: {error}")))?;
    let compressed = stream.dict.get(b"Filter").is_ok();
    let (predicted, data) = if compressed { (true, stream.content) } else { (false, raw) };
    Ok(EncodedImage { width, height, bits, colors, predicted, compressed, data, pixel_sha256 })
}

/// A white page of the given size (a placeholder for a page that could not be rendered).
pub(crate) fn placeholder_image() -> EncodedImage {
    encode_image(1, 1, &[255, 255, 255]).expect("a 1x1 white image always encodes")
}

fn text_sha256(words: &[SearchableWord]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    for word in words { hasher.update(word.text.as_bytes()); hasher.update([0]); }
    hasher.finalize().into()
}

fn real(value: f64) -> Result<String, StreamError> {
    if !value.is_finite() || value.abs() > 1.0e6 {
        return Err(StreamError::Invalid("searchable text transform is outside the supported range".into()));
    }
    let converted = value as f32;
    if !converted.is_finite() || (value != 0.0 && converted == 0.0) {
        return Err(StreamError::Invalid("searchable text transform cannot be represented exactly enough".into()));
    }
    Ok(format!("{converted}"))
}

fn page_dimension(value: f32) -> Result<String, StreamError> {
    if !value.is_finite() || value <= 0.0 || value > 14_400.0 { return Err(StreamError::Invalid("OCR page size is invalid.".into())); }
    Ok(format!("{value}"))
}

fn page_object(page: u32) -> u32 { FIRST_PAGE_OBJECT + 3 * page }
fn node_count(page_count: u32) -> u32 { page_count.div_ceil(TREE_FAN_OUT) }
fn node_object(page_count: u32, node: u32) -> u32 { FIRST_PAGE_OBJECT + 3 * page_count + node }

pub(crate) struct PartialWriter {
    directory: PathBuf,
    manifest: JobManifest,
    part: BufWriter<File>,
    journal: File,
    position: u64,
    records: Vec<PageRecord>,
    characters: Vec<char>,
    ids: HashMap<char, u16>,
    pending: Vec<u8>,
    last_sync: std::time::Instant,
    sync_interval: std::time::Duration,
    poisoned: bool,
    #[cfg(test)]
    pub(crate) fail_write_at: Option<u64>,
}

impl PartialWriter {
    /// Starts a new job directory (which must not exist yet).
    pub(crate) fn create(directory: &Path, manifest: JobManifest) -> Result<Self, StreamError> {
        validate_manifest(&manifest)?;
        std::fs::create_dir(directory).map_err(io)?;
        let manifest_bytes = serde_json::to_vec_pretty(&manifest).map_err(|error| StreamError::Invalid(error.to_string()))?;
        let mut file = OpenOptions::new().write(true).create_new(true).open(directory.join(MANIFEST_FILE)).map_err(io)?;
        file.write_all(&manifest_bytes).map_err(io)?;
        file.sync_all().map_err(io)?;
        let mut part = OpenOptions::new().read(true).write(true).create_new(true).open(directory.join(PART_FILE)).map_err(io)?;
        part.write_all(PARTIAL_HEADER).map_err(io)?;
        part.sync_data().map_err(io)?;
        let journal = OpenOptions::new().read(true).write(true).create_new(true).open(directory.join(JOURNAL_FILE)).map_err(io)?;
        journal.sync_all().map_err(io)?;
        Ok(Self {
            directory: directory.to_path_buf(), manifest, part: BufWriter::with_capacity(1 << 20, part), journal,
            position: PARTIAL_HEADER.len() as u64, records: Vec::new(), characters: Vec::new(), ids: HashMap::new(),
            pending: Vec::new(), last_sync: std::time::Instant::now(), sync_interval: SYNC_INTERVAL, poisoned: false,
            #[cfg(test)] fail_write_at: None,
        })
    }

    /// Reads a job directory written by `create` and truncates it to the last
    /// complete page. Everything read here is untrusted input.
    pub(crate) fn resume(directory: &Path) -> Result<Self, StreamError> {
        let manifest = read_manifest(directory)?;
        let mut part = OpenOptions::new().read(true).write(true).open(directory.join(PART_FILE)).map_err(|error| StreamError::Unusable(error.to_string()))?;
        let mut journal = OpenOptions::new().read(true).write(true).open(directory.join(JOURNAL_FILE)).map_err(|error| StreamError::Unusable(error.to_string()))?;
        let part_len = part.metadata().map_err(io)?.len();
        let mut header = [0_u8; 16];
        if part_len < header.len() as u64 { return Err(StreamError::Unusable("the partial copy is too short".into())); }
        part.read_exact(&mut header).map_err(io)?;
        if &header != PARTIAL_HEADER { return Err(StreamError::Unusable("the partial copy has an unexpected header".into())); }
        let journal_len = journal.metadata().map_err(io)?.len();
        if journal_len > (manifest.page_count as u64 + 1) * (RECORD_MAX_PAYLOAD as u64 + 16) {
            return Err(StreamError::Unusable("the journal is too large".into()));
        }
        let mut bytes = Vec::with_capacity(journal_len as usize);
        journal.read_to_end(&mut bytes).map_err(io)?;
        let mut records = Vec::new();
        let mut characters = Vec::new();
        let mut ids = HashMap::new();
        let mut cursor = 0_usize;
        let mut position = PARTIAL_HEADER.len() as u64;
        while let Some((record, used)) = parse_record(&bytes[cursor..]) {
            let expected_page = records.len() as u32;
            let consistent = record.page == expected_page
                && record.page < manifest.page_count
                && record.image_offset == position
                && record.content_offset > record.image_offset
                && record.page_offset > record.content_offset
                && record.end_offset > record.page_offset
                && record.end_offset <= part_len
                && characters.len() + record.new_characters.len() <= MAX_SEARCHABLE_DISTINCT_CHARS
                && record.new_characters.iter().all(|character| !ids.contains_key(character) && crate::searchable_pdf::is_supported_word_char(*character))
                && record.new_characters.iter().collect::<std::collections::HashSet<_>>().len() == record.new_characters.len();
            if !consistent { break; }
            for character in &record.new_characters {
                characters.push(*character);
                ids.insert(*character, characters.len() as u16);
            }
            position = record.end_offset;
            records.push(record);
            cursor += used;
        }
        journal.set_len(cursor as u64).map_err(io)?;
        journal.sync_all().map_err(io)?;
        part.set_len(position).map_err(io)?;
        part.sync_all().map_err(io)?;
        part.seek(SeekFrom::Start(position)).map_err(io)?;
        journal.seek(SeekFrom::End(0)).map_err(io)?;
        Ok(Self {
            directory: directory.to_path_buf(), manifest, part: BufWriter::with_capacity(1 << 20, part), journal, position, records,
            characters, ids, pending: Vec::new(), last_sync: std::time::Instant::now(), sync_interval: SYNC_INTERVAL, poisoned: false,
            #[cfg(test)] fail_write_at: None,
        })
    }

    pub(crate) fn manifest(&self) -> &JobManifest { &self.manifest }
    pub(crate) fn next_page(&self) -> u32 { self.records.len() as u32 }
    pub(crate) fn records(&self) -> &[PageRecord] { &self.records }
    pub(crate) fn bytes_written(&self) -> u64 { self.position }

    /// Makes every appended page durable: flush the part file to disk, then
    /// append and flush the journal records that point into it. A journal
    /// record is therefore never on disk before the bytes it describes.
    pub(crate) fn checkpoint(&mut self) -> Result<(), StreamError> {
        if self.poisoned { return Err(StreamError::Io("the OCR copy stopped after an earlier write error".into())); }
        if self.pending.is_empty() { return Ok(()); }
        let result = (|| {
            self.part.flush().map_err(io)?;
            self.part.get_ref().sync_data().map_err(io)?;
            let journal_start = self.journal.stream_position().map_err(io)?;
            if let Err(error) = self.journal.write_all(&self.pending).and_then(|_| self.journal.sync_data()) {
                let _ = self.journal.set_len(journal_start);
                return Err(io(error));
            }
            Ok(())
        })();
        if result.is_err() { self.poisoned = true; }
        result?;
        self.pending.clear();
        self.last_sync = std::time::Instant::now();
        Ok(())
    }

    fn put(&mut self, bytes: &[u8]) -> Result<(), StreamError> {
        let next = self.position.checked_add(bytes.len() as u64).ok_or(StreamError::SizeLimit)?;
        if next > MAX_STREAM_OUTPUT_BYTES { return Err(StreamError::SizeLimit); }
        #[cfg(test)]
        if self.fail_write_at.is_some_and(|limit| next > limit) { return Err(StreamError::Io("There is not enough space on the disk. (test fault)".into())); }
        self.part.write_all(bytes).map_err(io)?;
        self.position = next;
        Ok(())
    }

    fn rollback(&mut self, offset: u64) -> Result<(), StreamError> {
        self.part.flush().map_err(io)?;
        let file = self.part.get_mut();
        file.set_len(offset).map_err(io)?;
        file.seek(SeekFrom::Start(offset)).map_err(io)?;
        self.position = offset;
        Ok(())
    }

    /// Appends the next page. On error nothing of this page stays on disk and
    /// the checkpoint still ends at the previous page.
    pub(crate) fn append(&mut self, page: &StreamPage) -> Result<&PageRecord, StreamError> {
        if self.poisoned { return Err(StreamError::Io("the OCR copy stopped after an earlier write error".into())); }
        let index = self.next_page();
        if index >= self.manifest.page_count { return Err(StreamError::Invalid("The OCR job has no more pages.".into())); }
        page_dimension(page.page_width)?;
        page_dimension(page.page_height)?;
        let start = self.position;
        let saved_characters = self.characters.len();
        match self.append_inner(index, page) {
            Ok(record) => {
                self.pending.extend_from_slice(&encode_record(&record));
                self.records.push(record);
                if self.last_sync.elapsed() >= self.sync_interval { self.checkpoint()?; }
            }
            Err(error) => {
                for character in self.characters.drain(saved_characters..) { self.ids.remove(&character); }
                let _ = self.rollback(start);
                return Err(error);
            }
        }
        Ok(self.records.last().expect("record was just pushed"))
    }

    fn append_inner(&mut self, index: u32, page: &StreamPage) -> Result<PageRecord, StreamError> {
        let first_new = self.characters.len();
        let mut kept = Vec::with_capacity(page.words.len());
        let mut dropped = 0_u32;
        for word in &page.words {
            validate_word(word, f64::from(page.page_width), f64::from(page.page_height), index as usize + 1).map_err(StreamError::Invalid)?;
            let fresh = word.text.chars().filter(|character| !self.ids.contains_key(character)).collect::<std::collections::BTreeSet<_>>();
            if self.characters.len() + fresh.len() > MAX_SEARCHABLE_DISTINCT_CHARS { dropped += 1; continue; }
            for character in word.text.chars() {
                if !self.ids.contains_key(&character) {
                    self.characters.push(character);
                    self.ids.insert(character, self.characters.len() as u16);
                }
            }
            kept.push(word);
        }
        let image_id = page_object(index);
        let image_offset = self.position;
        let image = &page.image;
        let mut dictionary = format!(
            "<< /Type /XObject /Subtype /Image /Width {} /Height {} /ColorSpace /{} /BitsPerComponent {}",
            image.width, image.height, if image.colors == 1 { "DeviceGray" } else { "DeviceRGB" }, image.bits,
        );
        if image.compressed {
            dictionary.push_str(" /Filter /FlateDecode");
            if image.predicted {
                dictionary.push_str(&format!(" /DecodeParms << /Predictor 12 /Colors {} /BitsPerComponent {} /Columns {} >>", image.colors, image.bits, image.width));
            }
        }
        dictionary.push_str(&format!(" /Length {} >>", image.data.len()));
        self.put(format!("{image_id} 0 obj\n{dictionary}\nstream\n").as_bytes())?;
        self.put(&image.data)?;
        self.put(b"\nendstream\nendobj\n")?;

        let content_offset = self.position;
        let mut content = format!("q\n{} 0 0 {} 0 0 cm\n/Image Do\nQ\n", page_dimension(page.page_width)?, page_dimension(page.page_height)?);
        if !kept.is_empty() {
            content.push_str("BT\n/F1 1 Tf\n3 Tr\n");
            let em_height = (FONT_ASCENT - FONT_DESCENT) as f64 / 1000.0;
            for word in &kept {
                let glyphs = word.text.chars().count() as f64;
                let vertical = word.height / em_height;
                let horizontal = word.width / (glyphs * GLYPH_ADVANCE as f64 / 1000.0);
                let baseline = f64::from(page.page_height) - word.y - word.height - FONT_DESCENT as f64 / 1000.0 * vertical;
                content.push_str(&format!("{} 0 0 {} {} {} Tm\n<", real(horizontal)?, real(vertical)?, real(word.x)?, real(baseline)?));
                for character in word.text.chars() { content.push_str(&format!("{:04X}", self.ids[&character])); }
                content.push_str("> Tj\n");
            }
            content.push_str("ET\n");
        }
        if content.len() > MAX_CONTENT_BYTES { return Err(StreamError::Invalid("OCR page text is too large.".into())); }
        let mut stream = Stream::new(Dictionary::new(), content.into_bytes());
        stream.compress().map_err(|error| StreamError::Invalid(format!("OCR text compression failed: {error}")))?;
        let filter = if stream.dict.get(b"Filter").is_ok() { " /Filter /FlateDecode" } else { "" };
        self.put(format!("{} 0 obj\n<<{filter} /Length {} >>\nstream\n", image_id + 1, stream.content.len()).as_bytes())?;
        self.put(&stream.content)?;
        self.put(b"\nendstream\nendobj\n")?;

        let page_offset = self.position;
        let parent = node_object(self.manifest.page_count, index / TREE_FAN_OUT);
        let fonts = if kept.is_empty() { String::new() } else { " /Font << /F1 3 0 R >>".into() };
        self.put(format!(
            "{} 0 obj\n<< /Type /Page /Parent {parent} 0 R /MediaBox [0 0 {} {}] /Resources << /XObject << /Image {image_id} 0 R >>{fonts} >> /Contents {} 0 R >>\nendobj\n",
            image_id + 2, page_dimension(page.page_width)?, page_dimension(page.page_height)?, image_id + 1,
        ).as_bytes())?;
        let end_offset = self.position;
        self.part.flush().map_err(io)?;

        let kept_words = kept.into_iter().cloned().collect::<Vec<_>>();
        let issue = if dropped > 0 && page.issue == PageIssue::None { PageIssue::TooManyCharacters } else { page.issue };
        Ok(PageRecord {
            page: index, outcome: page.outcome, issue, reduced_resolution: page.reduced_resolution,
            image_offset, content_offset, page_offset, end_offset,
            pixel_sha256: image.pixel_sha256, text_sha256: text_sha256(&kept_words),
            words: kept_words.len() as u32, dropped_words: dropped,
            new_characters: self.characters[first_new..].to_vec(),
        })
    }

    /// Writes the font, page tree, catalog, xref and trailer, switches the
    /// header to `%PDF`, then checks every page from disk. Returns the path of
    /// the finished (still private) file.
    pub(crate) fn finalize(mut self) -> Result<FinishedCopy, StreamError> {
        let page_count = self.manifest.page_count;
        if self.next_page() != page_count { return Err(StreamError::Invalid("The OCR job is not complete.".into())); }
        self.checkpoint()?;
        let mut offsets = vec![0_u64; (FIRST_PAGE_OBJECT + 3 * page_count + node_count(page_count)) as usize];
        for record in &self.records {
            let id = page_object(record.page) as usize;
            offsets[id] = record.image_offset;
            offsets[id + 1] = record.content_offset;
            offsets[id + 2] = record.page_offset;
        }
        let font_objects = font_objects(&self.characters)?;
        for (id, body) in font_objects {
            offsets[id as usize] = self.position;
            self.put(&body)?;
        }
        for node in 0..node_count(page_count) {
            let first = node * TREE_FAN_OUT;
            let last = (first + TREE_FAN_OUT).min(page_count);
            let kids = (first..last).map(|page| format!("{} 0 R", page_object(page) + 2)).collect::<Vec<_>>().join(" ");
            let id = node_object(page_count, node);
            offsets[id as usize] = self.position;
            self.put(format!("{id} 0 obj\n<< /Type /Pages /Parent 2 0 R /Count {} /Kids [{kids}] >>\nendobj\n", last - first).as_bytes())?;
        }
        let kids = (0..node_count(page_count)).map(|node| format!("{} 0 R", node_object(page_count, node))).collect::<Vec<_>>().join(" ");
        offsets[2] = self.position;
        self.put(format!("2 0 obj\n<< /Type /Pages /Count {page_count} /Kids [{kids}] >>\nendobj\n").as_bytes())?;
        offsets[1] = self.position;
        self.put(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n")?;
        let xref_offset = self.position;
        let mut xref = format!("xref\n0 {}\n0000000000 65535 f\r\n", offsets.len());
        for offset in &offsets[1..] { xref.push_str(&format!("{offset:010} 00000 n\r\n")); }
        xref.push_str(&format!("trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref_offset}\n%%EOF\n", offsets.len()));
        self.put(xref.as_bytes())?;
        self.part.flush().map_err(io)?;
        let file = self.part.get_mut();
        file.seek(SeekFrom::Start(0)).map_err(io)?;
        file.write_all(FINAL_HEADER).map_err(io)?;
        file.sync_all().map_err(io)?;
        let path = self.directory.join(PART_FILE);
        let total = self.position;
        drop(self.part);
        if let Err(error) = check_finished(&path, &self.records, &self.characters, &offsets, page_count) {
            let _ = reopen_for_resume(&path);
            return Err(error);
        }
        Ok(FinishedCopy { path, bytes: total, records: self.records })
    }
}

/// Turns a finished but unpublished file back into a partial file, so the job
/// can be resumed (resume truncates the font, tree and xref and writes them
/// again, byte for byte).
pub(crate) fn reopen_for_resume(path: &Path) -> Result<(), StreamError> {
    let mut file = OpenOptions::new().write(true).open(path).map_err(io)?;
    file.write_all(PARTIAL_HEADER).map_err(io)?;
    file.sync_all().map_err(io)
}

pub(crate) struct FinishedCopy {
    pub(crate) path: PathBuf,
    pub(crate) bytes: u64,
    pub(crate) records: Vec<PageRecord>,
}

fn validate_manifest(manifest: &JobManifest) -> Result<(), StreamError> {
    if manifest.format != MANIFEST_FORMAT || manifest.page_count == 0 || manifest.page_count > MAX_STREAM_PAGES
        || manifest.source_identity.len() > 256 || manifest.source_name.len() > 1024 || manifest.destination.len() > 4096
        || manifest.language.len() > 32 || manifest.engine_identity.len() > 256
    {
        return Err(StreamError::Unusable("the job description is invalid".into()));
    }
    Ok(())
}

pub(crate) fn read_manifest(directory: &Path) -> Result<JobManifest, StreamError> {
    let file = File::open(directory.join(MANIFEST_FILE)).map_err(|error| StreamError::Unusable(error.to_string()))?;
    let mut bytes = Vec::new();
    file.take(MANIFEST_LIMIT + 1).read_to_end(&mut bytes).map_err(io)?;
    if bytes.len() as u64 > MANIFEST_LIMIT { return Err(StreamError::Unusable("the job description is too large".into())); }
    let manifest: JobManifest = serde_json::from_slice(&bytes).map_err(|_| StreamError::Unusable("the job description is unreadable".into()))?;
    validate_manifest(&manifest)?;
    Ok(manifest)
}

/// How many pages a job folder has saved, read only (nothing is truncated).
pub(crate) fn saved_pages(directory: &Path) -> u32 {
    let Ok(file) = File::open(directory.join(JOURNAL_FILE)) else { return 0 };
    let mut bytes = Vec::new();
    if file.take(u64::from(MAX_STREAM_PAGES) * (RECORD_MAX_PAYLOAD as u64 + 16)).read_to_end(&mut bytes).is_err() { return 0; }
    let (mut cursor, mut pages) = (0_usize, 0_u32);
    while let Some((record, used)) = parse_record(&bytes[cursor..]) {
        if record.page != pages { break; }
        pages += 1;
        cursor += used;
    }
    pages
}

fn encode_record(record: &PageRecord) -> Vec<u8> {
    let mut payload = Vec::with_capacity(RECORD_FIXED_PAYLOAD + 4 * record.new_characters.len());
    payload.extend_from_slice(&record.page.to_le_bytes());
    payload.push(record.outcome.code());
    payload.push(record.issue.code());
    payload.push(u8::from(record.reduced_resolution));
    for offset in [record.image_offset, record.content_offset, record.page_offset, record.end_offset] { payload.extend_from_slice(&offset.to_le_bytes()); }
    payload.extend_from_slice(&record.pixel_sha256);
    payload.extend_from_slice(&record.text_sha256);
    payload.extend_from_slice(&record.words.to_le_bytes());
    payload.extend_from_slice(&record.dropped_words.to_le_bytes());
    payload.extend_from_slice(&(record.new_characters.len() as u32).to_le_bytes());
    for character in &record.new_characters { payload.extend_from_slice(&u32::from(*character).to_le_bytes()); }
    let mut encoded = Vec::with_capacity(payload.len() + 16);
    encoded.extend_from_slice(RECORD_MAGIC);
    encoded.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    encoded.extend_from_slice(&payload);
    encoded.extend_from_slice(&sha256(&payload)[..8]);
    encoded
}

fn parse_record(bytes: &[u8]) -> Option<(PageRecord, usize)> {
    if bytes.len() < 8 || &bytes[..4] != RECORD_MAGIC { return None; }
    let length = u32::from_le_bytes(bytes[4..8].try_into().ok()?) as usize;
    if !(RECORD_FIXED_PAYLOAD..=RECORD_MAX_PAYLOAD).contains(&length) || bytes.len() < 8 + length + 8 { return None; }
    let payload = &bytes[8..8 + length];
    if sha256(payload)[..8] != bytes[8 + length..16 + length] { return None; }
    let u32_at = |at: usize| u32::from_le_bytes(payload[at..at + 4].try_into().unwrap());
    let u64_at = |at: usize| u64::from_le_bytes(payload[at..at + 8].try_into().unwrap());
    let page = u32_at(0);
    let outcome = PageOutcome::from_code(payload[4])?;
    let issue = PageIssue::from_code(payload[5])?;
    if payload[6] > 1 { return None; }
    let offsets = [u64_at(7), u64_at(15), u64_at(23), u64_at(31)];
    let pixel_sha256: [u8; 32] = payload[39..71].try_into().ok()?;
    let text_sha256: [u8; 32] = payload[71..103].try_into().ok()?;
    let words = u32_at(103);
    let dropped_words = u32_at(107);
    let count = u32_at(111) as usize;
    if length != RECORD_FIXED_PAYLOAD + 4 * count { return None; }
    let mut new_characters = Vec::with_capacity(count);
    for index in 0..count { new_characters.push(char::from_u32(u32_at(RECORD_FIXED_PAYLOAD + 4 * index))?); }
    Some((PageRecord {
        page, outcome, issue, reduced_resolution: payload[6] == 1,
        image_offset: offsets[0], content_offset: offsets[1], page_offset: offsets[2], end_offset: offsets[3],
        pixel_sha256, text_sha256, words, dropped_words, new_characters,
    }, 16 + length))
}

fn font_objects(characters: &[char]) -> Result<Vec<(u32, Vec<u8>)>, StreamError> {
    fn stream_object(id: u32, extra: &str, data: Vec<u8>) -> Result<(u32, Vec<u8>), StreamError> {
        let mut stream = Stream::new(Dictionary::new(), data);
        stream.compress().map_err(|error| StreamError::Invalid(format!("OCR font compression failed: {error}")))?;
        let filter = if stream.dict.get(b"Filter").is_ok() { " /Filter /FlateDecode" } else { "" };
        let mut body = format!("{id} 0 obj\n<<{extra}{filter} /Length {} >>\nstream\n", stream.content.len()).into_bytes();
        body.extend_from_slice(&stream.content);
        body.extend_from_slice(b"\nendstream\nendobj\n");
        Ok((id, body))
    }
    let program = glyphless_truetype();
    let mut gid_map = vec![0_u8; (characters.len() + 1) * 2];
    for pair in gid_map.chunks_exact_mut(2).skip(1) { pair[1] = 1; }
    Ok(vec![
        (3, format!("3 0 obj\n<< /Type /Font /Subtype /Type0 /BaseFont /{FONT_NAME} /Encoding /Identity-H /DescendantFonts [4 0 R] /ToUnicode 8 0 R >>\nendobj\n").into_bytes()),
        (4, format!("4 0 obj\n<< /Type /Font /Subtype /CIDFontType2 /BaseFont /{FONT_NAME} /CIDSystemInfo << /Registry (Adobe) /Ordering (Identity) /Supplement 0 >> /FontDescriptor 5 0 R /DW {GLYPH_ADVANCE} /CIDToGIDMap 7 0 R >>\nendobj\n").into_bytes()),
        (5, format!("5 0 obj\n<< /Type /FontDescriptor /FontName /{FONT_NAME} /Flags 5 /FontBBox [0 {FONT_DESCENT} {GLYPH_ADVANCE} {FONT_ASCENT}] /ItalicAngle 0 /Ascent {FONT_ASCENT} /Descent {FONT_DESCENT} /CapHeight {FONT_ASCENT} /StemV 80 /FontFile2 6 0 R >>\nendobj\n").into_bytes()),
        stream_object(6, &format!(" /Length1 {}", program.len()), program)?,
        stream_object(7, "", gid_map)?,
        stream_object(8, "", to_unicode_cmap(characters).into_bytes())?,
    ])
}

/// Re-reads the finished file from disk, one page at a time: the xref points
/// at every object, each page image decodes to exactly the recorded pixels and
/// each content stream carries exactly the recorded words.
fn check_finished(path: &Path, records: &[PageRecord], characters: &[char], offsets: &[u64], page_count: u32) -> Result<(), StreamError> {
    let fail = |message: String| StreamError::CheckFailed(message);
    let mut file = File::open(path).map_err(io)?;
    let length = file.metadata().map_err(io)?.len();
    let mut header = [0_u8; 16];
    file.read_exact(&mut header).map_err(io)?;
    if &header != FINAL_HEADER { return Err(fail("header".into())); }
    let read_object = |file: &mut File, id: usize, end: u64| -> Result<Vec<u8>, StreamError> {
        let start = offsets[id];
        if start == 0 || end <= start || end > length || end - start > (MAX_PAGE_PIXEL_BYTES as u64) * 2 { return Err(fail(format!("object {id} offset"))); }
        file.seek(SeekFrom::Start(start)).map_err(io)?;
        let mut bytes = vec![0_u8; (end - start) as usize];
        file.read_exact(&mut bytes).map_err(io)?;
        if !bytes.starts_with(format!("{id} 0 obj\n").as_bytes()) || !bytes.ends_with(b"endobj\n") { return Err(fail(format!("object {id} framing"))); }
        Ok(bytes)
    };
    let stream_of = |bytes: &[u8], id: usize| -> Result<Stream, StreamError> {
        let dictionary_end = find(bytes, b">>\nstream\n").ok_or_else(|| fail(format!("object {id} stream")))?;
        let body_start = dictionary_end + b">>\nstream\n".len();
        let body_end = bytes.len().checked_sub(b"\nendstream\nendobj\n".len()).filter(|end| *end >= body_start).ok_or_else(|| fail(format!("object {id} stream end")))?;
        let dictionary_text = std::str::from_utf8(&bytes[..dictionary_end + 2]).map_err(|_| fail(format!("object {id} dictionary")))?;
        let mut dictionary = Dictionary::new();
        if dictionary_text.contains("/Filter /FlateDecode") { dictionary.set("Filter", "FlateDecode"); }
        if let Some(at) = dictionary_text.find("/DecodeParms << /Predictor 12 /Colors ") {
            let fields = dictionary_text[at..].split_whitespace().collect::<Vec<_>>();
            let number = |index: usize| fields.get(index).and_then(|value| value.parse::<i64>().ok()).ok_or_else(|| fail(format!("object {id} parameters")));
            let mut parameters = Dictionary::new();
            parameters.set("Predictor", 12);
            parameters.set("Colors", number(5)?);
            parameters.set("BitsPerComponent", number(7)?);
            parameters.set("Columns", number(9)?);
            dictionary.set("DecodeParms", parameters);
        }
        Ok(Stream::new(dictionary, bytes[body_start..body_end].to_vec()))
    };
    for (index, record) in records.iter().enumerate() {
        if record.page as usize != index { return Err(fail("page order".into())); }
        let id = page_object(record.page) as usize;
        let image_bytes = read_object(&mut file, id, record.content_offset)?;
        let header_text = String::from_utf8_lossy(&image_bytes[..image_bytes.len().min(400)]).into_owned();
        let field = |name: &str| header_text.split(name).nth(1).and_then(|rest| rest.split_whitespace().next()).and_then(|value| value.parse::<usize>().ok());
        let (width, height, bits) = (field("/Width ").ok_or_else(|| fail(format!("page {} width", index + 1)))?, field("/Height ").ok_or_else(|| fail(format!("page {} height", index + 1)))?, field("/BitsPerComponent ").ok_or_else(|| fail(format!("page {} bits", index + 1)))?);
        let colors = if header_text.contains("/DeviceRGB") { 3 } else { 1 };
        let row_bytes = if bits == 1 { width.div_ceil(8) } else { width * colors };
        let image = stream_of(&image_bytes, id)?;
        let raw = image.decompressed_content_with_limit(MAX_PAGE_PIXEL_BYTES + height).map_err(|error| fail(format!("page {} image: {error}", index + 1)))?;
        if raw.len() != row_bytes * height { return Err(fail(format!("page {} image length", index + 1))); }
        let mut hasher = Sha256::new();
        let mut row_rgb = Vec::with_capacity(width * 3);
        for row in raw.chunks_exact(row_bytes) {
            row_rgb.clear();
            match (bits, colors) {
                (1, 1) => for x in 0..width { let value = if row[x / 8] & (0x80 >> (x % 8)) != 0 { 255 } else { 0 }; row_rgb.extend_from_slice(&[value; 3]); },
                (8, 1) => for value in row { row_rgb.extend_from_slice(&[*value; 3]); },
                (8, 3) => row_rgb.extend_from_slice(row),
                _ => return Err(fail(format!("page {} colour space", index + 1))),
            }
            hasher.update(&row_rgb);
        }
        if <[u8; 32]>::from(hasher.finalize()) != record.pixel_sha256 { return Err(fail(format!("page {} pixels", index + 1))); }
        let content_bytes = read_object(&mut file, id + 1, record.page_offset)?;
        let content = stream_of(&content_bytes, id + 1)?.decompressed_content_with_limit(MAX_CONTENT_BYTES).map_err(|error| fail(format!("page {} text: {error}", index + 1)))?;
        let content = String::from_utf8(content).map_err(|_| fail(format!("page {} text encoding", index + 1)))?;
        let mut hasher = Sha256::new();
        let mut words = 0_u32;
        for line in content.lines().filter(|line| line.ends_with("> Tj")) {
            let codes = line.strip_prefix('<').and_then(|rest| rest.strip_suffix("> Tj")).ok_or_else(|| fail(format!("page {} text operator", index + 1)))?;
            if codes.is_empty() || codes.len() % 4 != 0 { return Err(fail(format!("page {} text codes", index + 1))); }
            for chunk in codes.as_bytes().chunks_exact(4) {
                let code = u16::from_str_radix(std::str::from_utf8(chunk).unwrap_or("x"), 16).map_err(|_| fail(format!("page {} text code", index + 1)))?;
                let character = characters.get(usize::from(code).wrapping_sub(1)).ok_or_else(|| fail(format!("page {} text character", index + 1)))?;
                let mut buffer = [0_u8; 4];
                hasher.update(character.encode_utf8(&mut buffer).as_bytes());
            }
            hasher.update([0]);
            words += 1;
        }
        if words != record.words || <[u8; 32]>::from(hasher.finalize()) != record.text_sha256 { return Err(fail(format!("page {} text", index + 1))); }
        let page_bytes = read_object(&mut file, id + 2, record.end_offset)?;
        let expected_parent = format!("/Parent {} 0 R", node_object(page_count, record.page / TREE_FAN_OUT));
        if find(&page_bytes, expected_parent.as_bytes()).is_none() { return Err(fail(format!("page {} parent", index + 1))); }
    }
    Ok(())
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|window| window == needle)
}

/// Moves the finished file to the destination without ever replacing an
/// existing file. Same volume: one atomic rename. Another volume: copy into a
/// temporary file next to the destination, flush, then a no-clobber rename.
/// `commit` runs immediately before the rename and can still refuse.
pub(crate) fn publish_noclobber(
    finished: &Path,
    destination: &Path,
    commit: impl FnOnce() -> Result<(), String>,
) -> Result<(), StreamError> {
    if destination.exists() { return Err(StreamError::DestinationExists); }
    let parent = destination.parent().filter(|parent| !parent.as_os_str().is_empty()).ok_or_else(|| StreamError::Invalid("Choose an output folder.".into()))?;
    if !destination.extension().is_some_and(|extension| extension.eq_ignore_ascii_case("pdf")) { return Err(StreamError::Invalid("The output filename must end in .pdf.".into())); }
    let same_volume = volume_of(finished) == volume_of(destination);
    if same_volume {
        commit().map_err(StreamError::NotCommitted)?;
        return move_noclobber(finished, destination);
    }
    let mut temporary = tempfile::NamedTempFile::new_in(parent).map_err(io)?;
    let mut source = File::open(finished).map_err(io)?;
    std::io::copy(&mut source, temporary.as_file_mut()).map_err(io)?;
    temporary.as_file().sync_all().map_err(io)?;
    commit().map_err(StreamError::NotCommitted)?;
    temporary.persist_noclobber(destination).map_err(|error| if error.error.kind() == std::io::ErrorKind::AlreadyExists { StreamError::DestinationExists } else { io(error.error) })?;
    let _ = std::fs::remove_file(finished);
    Ok(())
}

fn volume_of(path: &Path) -> Option<std::ffi::OsString> {
    let absolute = std::path::absolute(path).ok()?;
    match absolute.components().next()? {
        std::path::Component::Prefix(prefix) => Some(prefix.as_os_str().to_ascii_uppercase()),
        _ => None,
    }
}

#[cfg(windows)]
fn move_noclobber(from: &Path, to: &Path) -> Result<(), StreamError> {
    use std::os::windows::ffi::OsStrExt;
    use windows::Win32::Storage::FileSystem::{MoveFileExW, MOVEFILE_WRITE_THROUGH};
    let wide = |path: &Path| path.as_os_str().encode_wide().chain(std::iter::once(0)).collect::<Vec<u16>>();
    let (from_wide, to_wide) = (wide(from), wide(to));
    unsafe { MoveFileExW(windows::core::PCWSTR(from_wide.as_ptr()), windows::core::PCWSTR(to_wide.as_ptr()), MOVEFILE_WRITE_THROUGH) }
        .map_err(|error| if to.exists() { StreamError::DestinationExists } else { StreamError::Io(error.message()) })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) fn manifest(page_count: u32, destination: &Path) -> JobManifest {
        JobManifest {
            format: MANIFEST_FORMAT, page_count, source_identity: "ab".repeat(32), source_name: "synthetic.pdf".into(),
            destination: destination.to_string_lossy().into_owned(), dpi: 150, language: "eng".into(), engine_identity: "test".into(),
        }
    }

    /// A deterministic synthetic page: a small gray, bilevel or colour image
    /// that differs per page, plus a few words including non-ASCII text.
    pub(crate) fn synthetic_page(index: u32) -> StreamPage {
        let (width, height) = (64 + index % 7, 48 + index % 5);
        let mut rgb = Vec::with_capacity((width * height * 3) as usize);
        for y in 0..height {
            for x in 0..width {
                let value = ((x * 7 + y * 3 + index * 11) % 256) as u8;
                match index % 3 {
                    0 => rgb.extend_from_slice(&[if value > 127 { 255 } else { 0 }; 3]),
                    1 => rgb.extend_from_slice(&[value; 3]),
                    _ => rgb.extend_from_slice(&[value, value.wrapping_mul(3), 255 - value]),
                }
            }
        }
        let words = vec![
            SearchableWord { x: 10.0, y: 10.0, width: 80.0, height: 12.0, text: format!("Page{}", index + 1) },
            SearchableWord { x: 100.0, y: 10.0, width: 60.0, height: 12.0, text: ["café", "naïve", "“ok”", "Straße", "𝔄"][index as usize % 5].into() },
        ];
        StreamPage {
            page_width: 612.0, page_height: 792.0, image: encode_image(width, height, &rgb).unwrap(), words,
            outcome: PageOutcome::Text, issue: PageIssue::None, reduced_resolution: false,
        }
    }

    fn write_all(directory: &Path, pages: u32) -> FinishedCopy {
        let mut writer = PartialWriter::create(directory, manifest(pages, Path::new("E:/unused.pdf"))).unwrap();
        for index in 0..pages { writer.append(&synthetic_page(index)).unwrap(); }
        writer.finalize().unwrap()
    }

    #[test]
    fn encodes_each_colour_class_losslessly() {
        let gray = encode_image(3, 1, &[1, 1, 1, 2, 2, 2, 3, 3, 3]).unwrap();
        assert_eq!((gray.bits, gray.colors), (8, 1));
        let bilevel = encode_image(9, 1, &[0; 27]).unwrap();
        assert_eq!((bilevel.bits, bilevel.colors), (1, 1));
        let colour = encode_image(1, 1, &[1, 2, 3]).unwrap();
        assert_eq!((colour.bits, colour.colors), (8, 3));
        assert!(encode_image(2, 2, &[0; 11]).is_err());
    }

    #[test]
    fn writes_a_multi_hundred_page_copy_that_lopdf_reopens() {
        let folder = tempfile::tempdir().unwrap();
        let finished = write_all(&folder.path().join("job"), 300);
        let bytes = std::fs::read(&finished.path).unwrap();
        assert!(bytes.starts_with(b"%PDF-1.7"));
        let document = lopdf::Document::load_mem(&bytes).unwrap();
        assert_eq!(document.get_pages().len(), 300);
        assert_eq!(finished.records.len(), 300);
        assert!(finished.records.iter().all(|record| record.outcome == PageOutcome::Text && record.words == 2));
    }

    #[test]
    fn output_is_deterministic() {
        let folder = tempfile::tempdir().unwrap();
        let first = std::fs::read(write_all(&folder.path().join("a"), 40).path).unwrap();
        let second = std::fs::read(write_all(&folder.path().join("b"), 40).path).unwrap();
        assert_eq!(first, second);
    }

    #[test]
    fn resume_after_torn_part_and_journal_gives_identical_bytes() {
        let folder = tempfile::tempdir().unwrap();
        let clean = std::fs::read(write_all(&folder.path().join("clean"), 60).path).unwrap();
        for (tear_part, tear_journal) in [(true, false), (false, true), (true, true), (false, false)] {
            let directory = folder.path().join(format!("torn-{tear_part}-{tear_journal}"));
            let mut writer = PartialWriter::create(&directory, manifest(60, Path::new("E:/unused.pdf"))).unwrap();
            for index in 0..25 { writer.append(&synthetic_page(index)).unwrap(); }
            writer.checkpoint().unwrap();
            drop(writer);
            if tear_part {
                let part = OpenOptions::new().write(true).open(directory.join(PART_FILE)).unwrap();
                let length = part.metadata().unwrap().len();
                part.set_len(length - 37).unwrap();
            }
            if tear_journal {
                let journal = OpenOptions::new().write(true).open(directory.join(JOURNAL_FILE)).unwrap();
                let length = journal.metadata().unwrap().len();
                journal.set_len(length - 5).unwrap();
            }
            let mut resumed = PartialWriter::resume(&directory).unwrap();
            let expected_next = if tear_part || tear_journal { 24 } else { 25 };
            assert_eq!(resumed.next_page(), expected_next, "{tear_part} {tear_journal}");
            for index in resumed.next_page()..60 { resumed.append(&synthetic_page(index)).unwrap(); }
            let bytes = std::fs::read(resumed.finalize().unwrap().path).unwrap();
            assert!(bytes == clean, "resumed output differs ({tear_part}, {tear_journal})");
        }
    }

    #[test]
    fn resume_refuses_a_foreign_or_damaged_directory() {
        let folder = tempfile::tempdir().unwrap();
        let directory = folder.path().join("job");
        let mut writer = PartialWriter::create(&directory, manifest(10, Path::new("E:/unused.pdf"))).unwrap();
        writer.append(&synthetic_page(0)).unwrap();
        writer.checkpoint().unwrap();
        drop(writer);
        std::fs::write(directory.join(MANIFEST_FILE), b"{\"format\":1}").unwrap();
        assert!(matches!(PartialWriter::resume(&directory), Err(StreamError::Unusable(_))));
        let other = folder.path().join("other");
        let writer = PartialWriter::create(&other, manifest(10, Path::new("E:/unused.pdf"))).unwrap();
        drop(writer);
        let mut part = OpenOptions::new().write(true).open(other.join(PART_FILE)).unwrap();
        part.write_all(b"%PDF-1.7\n").unwrap();
        drop(part);
        assert!(matches!(PartialWriter::resume(&other), Err(StreamError::Unusable(_))));
    }

    #[test]
    fn a_tampered_journal_record_stops_resume_at_that_page() {
        let folder = tempfile::tempdir().unwrap();
        let directory = folder.path().join("job");
        let mut writer = PartialWriter::create(&directory, manifest(10, Path::new("E:/unused.pdf"))).unwrap();
        for index in 0..4 { writer.append(&synthetic_page(index)).unwrap(); }
        writer.checkpoint().unwrap();
        drop(writer);
        let mut journal = std::fs::read(directory.join(JOURNAL_FILE)).unwrap();
        let (_, first) = parse_record(&journal).unwrap();
        journal[first + 20] ^= 1;
        std::fs::write(directory.join(JOURNAL_FILE), journal).unwrap();
        assert_eq!(PartialWriter::resume(&directory).unwrap().next_page(), 1);
    }

    #[test]
    fn final_check_catches_a_changed_pixel_on_disk() {
        let folder = tempfile::tempdir().unwrap();
        let directory = folder.path().join("job");
        let mut writer = PartialWriter::create(&directory, manifest(3, Path::new("E:/unused.pdf"))).unwrap();
        for index in 0..3 { writer.append(&synthetic_page(index)).unwrap(); }
        writer.checkpoint().unwrap();
        drop(writer);
        let mut part = std::fs::read(directory.join(PART_FILE)).unwrap();
        let journal = std::fs::read(directory.join(JOURNAL_FILE)).unwrap();
        let (record, _) = parse_record(&journal).unwrap();
        let at = record.content_offset as usize - 30;
        part[at] ^= 0x40;
        std::fs::write(directory.join(PART_FILE), part).unwrap();
        let resumed = PartialWriter::resume(&directory).unwrap();
        assert!(matches!(resumed.finalize(), Err(StreamError::CheckFailed(_))));
    }

    #[test]
    fn a_failed_write_rolls_back_to_the_previous_page() {
        let folder = tempfile::tempdir().unwrap();
        let directory = folder.path().join("job");
        let mut writer = PartialWriter::create(&directory, manifest(5, Path::new("E:/unused.pdf"))).unwrap();
        writer.append(&synthetic_page(0)).unwrap();
        let after_first = writer.bytes_written();
        writer.fail_write_at = Some(after_first + 100);
        assert!(matches!(writer.append(&synthetic_page(1)), Err(StreamError::Io(_))));
        assert_eq!((writer.next_page(), writer.bytes_written()), (1, after_first));
        writer.fail_write_at = None;
        for index in 1..5 { writer.append(&synthetic_page(index)).unwrap(); }
        let bytes = std::fs::read(writer.finalize().unwrap().path).unwrap();
        let clean = std::fs::read(write_all(&folder.path().join("clean"), 5).path).unwrap();
        assert!(bytes == clean);
    }

    #[test]
    fn placeholder_and_reduced_pages_are_recorded() {
        let folder = tempfile::tempdir().unwrap();
        let directory = folder.path().join("job");
        let mut writer = PartialWriter::create(&directory, manifest(2, Path::new("E:/unused.pdf"))).unwrap();
        writer.append(&StreamPage { page_width: 300.0, page_height: 400.0, image: placeholder_image(), words: vec![], outcome: PageOutcome::Placeholder, issue: PageIssue::RenderFailed, reduced_resolution: false }).unwrap();
        let mut reduced = synthetic_page(1);
        reduced.reduced_resolution = true;
        writer.append(&reduced).unwrap();
        let finished = writer.finalize().unwrap();
        assert_eq!(finished.records[0].outcome, PageOutcome::Placeholder);
        assert!(finished.records[1].reduced_resolution);
        let document = lopdf::Document::load(&finished.path).unwrap();
        assert_eq!(document.get_pages().len(), 2);
    }

    #[test]
    fn pdfium_renders_exact_pixels_and_extracts_the_words() {
        let _serial = crate::ocr_process::test_lock();
        let folder = tempfile::tempdir().unwrap();
        let directory = folder.path().join("job");
        let (width, height) = (1275_u32, 1650_u32);
        let mut writer = PartialWriter::create(&directory, manifest(3, Path::new("E:/unused.pdf"))).unwrap();
        let mut images = Vec::new();
        for index in 0..3_u32 {
            let mut rgb = Vec::with_capacity((width * height * 3) as usize);
            for y in 0..height {
                for x in 0..width {
                    let value = ((x / 3 + y / 5 + index * 40) % 256) as u8;
                    match index {
                        0 => rgb.extend_from_slice(&[if (x / 17 + y / 23) % 2 == 0 { 0 } else { 255 }; 3]),
                        1 => rgb.extend_from_slice(&[value; 3]),
                        _ => rgb.extend_from_slice(&[value, 255 - value, value / 2]),
                    }
                }
            }
            let mut page = synthetic_page(index);
            page.image = encode_image(width, height, &rgb).unwrap();
            writer.append(&page).unwrap();
            images.push(rgb);
        }
        let finished = writer.finalize().unwrap();
        let output = folder.path().join("copy.pdf");
        publish_noclobber(&finished.path, &output, || Ok(())).unwrap();
        let service = crate::service::PdfService::start(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("resources/pdfium/bin/pdfium.dll"));
        let opened = tauri::async_runtime::block_on(service.open(output)).unwrap();
        for index in 0..3_u16 {
            let request = opened.ocr_request(index);
            let cancellation = crate::ocr_process::OcrCancellation::default();
            let operation = crate::ocr_process::OcrOperation::try_begin(&cancellation).unwrap();
            let raster = tauri::async_runtime::block_on(service.ocr_raster(request, cancellation.clone(), operation.worker_hold())).unwrap();
            drop(operation);
            assert_eq!((raster.width, raster.height), (width, height));
            let header = format!("P6\n{width} {height}\n255\n");
            assert!(raster.p6[header.len()..] == images[usize::from(index)][..], "page {index} pixels differ after PDFium render");
            let text = tauri::async_runtime::block_on(service.text(request.document_id, index, request.revision)).unwrap();
            let expected = synthetic_page(u32::from(index)).words;
            for word in expected { assert!(text.contains(&word.text), "page {index}: {text:?} lacks {:?}", word.text); }
        }
        tauri::async_runtime::block_on(service.close(opened.ocr_request(0).document_id)).unwrap();
    }

    #[test]
    fn publish_never_replaces_an_existing_file() {
        let folder = tempfile::tempdir().unwrap();
        let finished = write_all(&folder.path().join("job"), 2);
        let destination = folder.path().join("out.pdf");
        std::fs::write(&destination, b"keep").unwrap();
        assert!(matches!(publish_noclobber(&finished.path, &destination, || Ok(())), Err(StreamError::DestinationExists)));
        assert_eq!(std::fs::read(&destination).unwrap(), b"keep");
        let fresh = folder.path().join("fresh.pdf");
        assert!(matches!(publish_noclobber(&finished.path, &fresh, || Err("OCR was cancelled".into())), Err(StreamError::NotCommitted(_))));
        assert!(!fresh.exists());
        publish_noclobber(&finished.path, &fresh, || Ok(())).unwrap();
        assert!(std::fs::read(&fresh).unwrap().starts_with(b"%PDF-1.7"));
        assert!(!finished.path.exists());
    }
}
