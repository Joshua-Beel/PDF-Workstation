//! Saving a large OCR copy in parts when it would pass the size cap (1 GiB).
//!
//! A job writes into one part at a time. Part 1 is the job folder's own
//! partial copy; part n (n >= 2) lives in `part-n/` inside the job folder,
//! with its own manifest, partial copy and journal, so each part resumes with
//! the same journal rules as a single copy. When a page would push the open
//! part past the cap (keeping room for the font, page tree and xref), the open
//! part is closed at its last page: `parts.json` records its page count
//! durably, then the next part starts with the page that did not fit. Part
//! boundaries therefore depend only on the pages, so an interrupted job gives
//! the same parts as an uninterrupted one.
//!
//! Nothing is published until every page is written. Then every unpublished
//! part is finalised and checked from disk, the names `<name> (part i of N).pdf`
//! are checked to be free, and the parts are moved into place in order with
//! the no-clobber publish. `parts.json` records how many parts are published,
//! and the size of the part being moved, so a crash between a move and the
//! next save is recognised on resume and an existing file is never replaced.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::large_ocr_job::{report_of, PageReport};
use crate::searchable_pdf_stream::{
    publish_noclobber, read_manifest, reopen_for_resume, saved_records, JobManifest, PartialWriter, StreamError, StreamPage,
    PART_FILE,
};

pub(crate) const PARTS_FILE: &str = "parts.json";
const PARTS_FORMAT: u32 = 1;
const PARTS_LIMIT: u64 = 256 * 1024;

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct PartsState {
    pub(crate) format: u32,
    /// Page counts of the closed parts, in order; the open part follows them.
    pub(crate) closed: Vec<u32>,
    /// How many parts are already at their final names (always the first ones).
    pub(crate) published: u32,
    /// Size of the part being moved into place, set just before the move.
    pub(crate) publishing: Option<u64>,
}

/// One saved part, with the source pages it holds (0-based first page).
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PublishedPart {
    pub(crate) path: String,
    pub(crate) first_page: u32,
    pub(crate) pages: u32,
    pub(crate) bytes: u64,
}

fn io(error: std::io::Error) -> StreamError { StreamError::Io(error.to_string()) }

/// Folder of part `index` (0-based): the job folder itself for the first part.
pub(crate) fn part_directory(root: &Path, index: usize) -> PathBuf {
    if index == 0 { root.to_path_buf() } else { root.join(format!("part-{}", index + 1)) }
}

/// `<folder>\<name> (part i of N).pdf` for each part, from the chosen destination.
pub(crate) fn part_names(destination: &Path, count: usize) -> Vec<PathBuf> {
    let stem = destination.file_stem().map(|stem| stem.to_string_lossy().into_owned()).unwrap_or_else(|| "OCR copy".into());
    let folder = destination.parent().unwrap_or(Path::new(""));
    (1..=count).map(|part| folder.join(format!("{stem} (part {part} of {count}).pdf"))).collect()
}

fn part_manifest(root: &JobManifest, first_page: u32) -> JobManifest {
    JobManifest { page_count: root.page_count - first_page, ..root.clone() }
}

/// A part's manifest matches the job's (the destination may have moved since).
fn same_part(found: &JobManifest, expected: &JobManifest) -> bool {
    JobManifest { destination: expected.destination.clone(), ..found.clone() } == *expected
}

/// Reads `parts.json`; `None` when the job is a single copy. Untrusted input.
pub(crate) fn load_state(root: &Path, manifest: &JobManifest) -> Result<Option<PartsState>, StreamError> {
    let file = match std::fs::File::open(root.join(PARTS_FILE)) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(StreamError::Unusable(error.to_string())),
    };
    let mut bytes = Vec::new();
    file.take(PARTS_LIMIT + 1).read_to_end(&mut bytes).map_err(io)?;
    let state: PartsState = (bytes.len() as u64 <= PARTS_LIMIT).then(|| serde_json::from_slice(&bytes).ok()).flatten()
        .ok_or_else(|| StreamError::Unusable("the record of the copy's parts is unreadable".into()))?;
    let closed_pages = state.closed.iter().try_fold(0_u32, |sum, pages| sum.checked_add(*pages));
    let valid = state.format == PARTS_FORMAT
        && state.closed.iter().all(|pages| *pages > 0)
        && closed_pages.is_some_and(|pages| pages < manifest.page_count)
        && state.published as usize <= state.closed.len() + 1
        && (state.publishing.is_none() || (state.published as usize) <= state.closed.len());
    if !valid { return Err(StreamError::Unusable("the record of the copy's parts is invalid".into())); }
    Ok(Some(state))
}

fn save_state(root: &Path, state: &PartsState) -> Result<(), StreamError> {
    let bytes = serde_json::to_vec_pretty(state).map_err(|error| StreamError::Invalid(error.to_string()))?;
    let temporary = root.join("parts.json.new");
    let mut file = std::fs::File::create(&temporary).map_err(io)?;
    file.write_all(&bytes).map_err(io)?;
    file.sync_all().map_err(io)?;
    drop(file);
    std::fs::rename(&temporary, root.join(PARTS_FILE)).map_err(io)
}

/// True when the saved job is being saved in parts.
pub(crate) fn has_parts(root: &Path) -> bool { root.join(PARTS_FILE).exists() }

/// True when some part already has its final name (the set's folder and names are fixed).
pub(crate) fn any_published(root: &Path) -> bool {
    read_manifest(root).ok().and_then(|manifest| load_state(root, &manifest).ok().flatten())
        .is_some_and(|state| state.published > 0 || state.publishing.is_some())
}

/// Pages saved so far, read only, for a job from an earlier session.
pub(crate) fn saved_progress(root: &Path, manifest: &JobManifest) -> u32 {
    match load_state(root, manifest) {
        Ok(Some(state)) if state.published as usize == state.closed.len() + 1 => manifest.page_count,
        Ok(Some(state)) => state.closed.iter().sum::<u32>() + saved_records(&part_directory(root, state.closed.len())).len() as u32,
        _ => crate::searchable_pdf_stream::saved_pages(root),
    }
}

/// Every folder of a job that may hold a cross-volume publish marker.
pub(crate) fn part_directories(root: &Path) -> Vec<PathBuf> {
    let mut folders = vec![root.to_path_buf()];
    if let Ok(entries) = std::fs::read_dir(root) {
        let mut parts = entries.flatten()
            .filter(|entry| entry.file_name().to_str().is_some_and(|name| name.strip_prefix("part-").is_some_and(|number| !number.is_empty() && number.bytes().all(|byte| byte.is_ascii_digit()))))
            .filter(|entry| std::fs::symlink_metadata(entry.path()).is_ok_and(|metadata| metadata.is_dir()))
            .map(|entry| entry.path())
            .collect::<Vec<_>>();
        parts.sort();
        folders.extend(parts);
    }
    folders
}

/// After a crash between moving a part and saving that, counts the move.
fn recover_publishing(root: &Path, manifest: &JobManifest, state: &mut PartsState) -> Result<(), StreamError> {
    let Some(bytes) = state.publishing else { return Ok(()) };
    let total = state.closed.len() + 1;
    let index = state.published as usize;
    let name = &part_names(Path::new(&manifest.destination), total)[index];
    let moved = !part_directory(root, index).join(PART_FILE).exists()
        && std::fs::symlink_metadata(name).is_ok_and(|metadata| metadata.is_file() && metadata.len() == bytes);
    if moved { state.published += 1; }
    state.publishing = None;
    save_state(root, state)
}

pub(crate) enum Opened {
    Output(Output),
    /// Every part was already published (the app stopped before cleaning up).
    AlreadyPublished { parts: Vec<PublishedPart>, report: Vec<PageReport> },
}

/// The job's output: one open writer (the open part) plus what earlier parts hold.
pub(crate) struct Output {
    root: PathBuf,
    manifest: JobManifest,
    cap: u64,
    parts: Option<PartsState>,
    writer: PartialWriter,
    first_page: u32,
    earlier_report: Vec<PageReport>,
    earlier_bytes: u64,
}

impl Output {
    pub(crate) fn create(root: &Path, manifest: JobManifest, cap: u64) -> Result<Self, StreamError> {
        let mut writer = PartialWriter::create(root, manifest.clone())?;
        writer.set_size_cap(cap);
        Ok(Self { root: root.to_path_buf(), manifest, cap, parts: None, writer, first_page: 0, earlier_report: Vec::new(), earlier_bytes: 0 })
    }

    pub(crate) fn resume(root: &Path, cap: u64) -> Result<Opened, StreamError> {
        let manifest = read_manifest(root)?;
        let Some(mut state) = load_state(root, &manifest)? else {
            let mut writer = PartialWriter::resume(root)?;
            writer.set_size_cap(cap);
            return Ok(Opened::Output(Self { root: root.to_path_buf(), manifest, cap, parts: None, writer, first_page: 0, earlier_report: Vec::new(), earlier_bytes: 0 }));
        };
        recover_publishing(root, &manifest, &mut state)?;
        let mut earlier_report = Vec::new();
        let (mut first_page, mut earlier_bytes) = (0_u32, 0_u64);
        for (index, pages) in state.closed.iter().enumerate() {
            let records = saved_records(&part_directory(root, index));
            let Some(last) = records.get(*pages as usize - 1) else { return Err(StreamError::Unusable(format!("part {} of the copy is incomplete", index + 1))) };
            earlier_report.extend(report_of(&records[..*pages as usize], first_page));
            earlier_bytes += last.end_offset();
            first_page += pages;
        }
        let total = state.closed.len() + 1;
        if state.published as usize == total {
            let names = part_names(Path::new(&manifest.destination), total);
            let mut counts = state.closed.clone();
            counts.push(manifest.page_count - first_page);
            return Ok(Opened::AlreadyPublished { parts: describe(&names, &counts, |name| std::fs::metadata(name).map(|metadata| metadata.len()).unwrap_or(0)), report: earlier_report });
        }
        let expected = part_manifest(&manifest, first_page);
        let directory = part_directory(root, state.closed.len());
        let writer = if state.closed.is_empty() {
            PartialWriter::resume(root)?
        } else {
            match PartialWriter::resume(&directory) {
                Ok(writer) if same_part(writer.manifest(), &expected) => writer,
                Ok(_) | Err(StreamError::Unusable(_)) => {
                    std::fs::remove_dir_all(&directory).or_else(|error| if error.kind() == std::io::ErrorKind::NotFound { Ok(()) } else { Err(io(error)) })?;
                    PartialWriter::create(&directory, expected)?
                }
                Err(error) => return Err(error),
            }
        };
        let mut writer = writer;
        writer.set_size_cap(cap);
        Ok(Opened::Output(Self { root: root.to_path_buf(), manifest, cap, parts: Some(state), writer, first_page, earlier_report, earlier_bytes }))
    }

    pub(crate) fn manifest(&self) -> &JobManifest { &self.manifest }
    pub(crate) fn next_page(&self) -> u32 { self.first_page + self.writer.next_page() }
    pub(crate) fn bytes_written(&self) -> u64 { self.earlier_bytes + self.writer.bytes_written() }
    pub(crate) fn in_parts(&self) -> bool { self.parts.is_some() }
    /// The open part's number (1 for a single copy).
    pub(crate) fn part(&self) -> u32 { self.parts.as_ref().map_or(1, |state| state.closed.len() as u32 + 1) }
    pub(crate) fn report(&self) -> Vec<PageReport> {
        let mut report = self.earlier_report.clone();
        report.extend(report_of(self.writer.records(), self.first_page));
        report
    }
    pub(crate) fn checkpoint(&mut self) -> Result<(), StreamError> { self.writer.checkpoint() }

    /// Appends the next page. When it does not fit under the cap and saving in
    /// parts is allowed (or already under way), closes the open part and
    /// starts the next one with this page.
    pub(crate) fn append(&mut self, page: &StreamPage, may_split: bool) -> Result<(), StreamError> {
        let appended = self.writer.append(page).map(|_| ());
        match appended {
            Ok(()) => Ok(()),
            Err(StreamError::SizeLimit) if (may_split || self.parts.is_some()) && self.writer.next_page() > 0 => {
                self.roll_over()?;
                self.writer.append(page).map(|_| ())
            }
            Err(error) => Err(error),
        }
    }

    fn roll_over(&mut self) -> Result<(), StreamError> {
        self.writer.checkpoint()?;
        let closed = self.writer.next_page();
        let next_first = self.first_page + closed;
        if next_first >= self.manifest.page_count { return Err(StreamError::Invalid("The OCR job has no more pages.".into())); }
        let mut state = self.parts.clone().unwrap_or(PartsState { format: PARTS_FORMAT, ..PartsState::default() });
        state.closed.push(closed);
        save_state(&self.root, &state)?;
        let directory = part_directory(&self.root, state.closed.len());
        if std::fs::symlink_metadata(&directory).is_ok() { std::fs::remove_dir_all(&directory).map_err(io)?; }
        let mut writer = PartialWriter::create(&directory, part_manifest(&self.manifest, next_first))?;
        writer.set_size_cap(self.cap);
        let previous = std::mem::replace(&mut self.writer, writer);
        self.earlier_report.extend(report_of(previous.records(), self.first_page));
        self.earlier_bytes += previous.bytes_written();
        self.first_page = next_first;
        self.parts = Some(state);
        Ok(())
    }

    /// For a single copy: finalises and checks the whole copy from disk.
    pub(crate) fn finalize_single(self) -> Result<crate::searchable_pdf_stream::FinishedCopy, StreamError> { self.writer.finalize() }

    /// For a copy in parts (every page written): checks the part names are
    /// free, finalises and checks every unpublished part from disk, then
    /// moves them into place in order. `commit` runs before the first move.
    pub(crate) fn publish_parts(self, commit: impl FnOnce() -> Result<(), String>) -> Result<(Vec<PublishedPart>, Vec<PageReport>), (StreamError, Vec<PageReport>)> {
        let report = self.report();
        let Self { root, manifest, cap, parts, writer, first_page, .. } = self;
        let Some(mut state) = parts else { return Err((StreamError::Invalid("The copy is not being saved in parts.".into()), report)) };
        let total = state.closed.len() + 1;
        let names = part_names(Path::new(&manifest.destination), total);
        let mut counts = state.closed.clone();
        counts.push(manifest.page_count - first_page);
        let start = state.published as usize;
        if let Some(taken) = names[start..].iter().find(|name| std::fs::symlink_metadata(name).is_ok()) {
            let name = taken.file_name().unwrap_or_default().to_string_lossy();
            return Err((StreamError::Invalid(format!("\"{name}\" already exists, so the parts were not saved. Move or rename that file, then resume; OCR never overwrites a file.")), report));
        }
        let mut finished = Vec::with_capacity(total - start);
        let mut open = Some(writer);
        for index in start..total {
            let part = if index + 1 == total {
                open.take().expect("the open part is used once")
            } else {
                match PartialWriter::resume(&part_directory(&root, index)) {
                    Ok(mut writer) => { writer.set_size_cap(cap); writer }
                    Err(error) => { revert(&finished); return Err((error, report)); }
                }
            };
            match part.finalize_pages(counts[index]) {
                Ok(copy) => finished.push(copy),
                Err(error) => { revert(&finished); return Err((error, report)); }
            }
        }
        let mut commit = Some(commit);
        let mut published = describe(&names[..start], &counts[..start], |name| std::fs::metadata(name).map(|metadata| metadata.len()).unwrap_or(0));
        for (offset, copy) in finished.iter().enumerate() {
            let index = start + offset;
            state.publishing = Some(copy.bytes);
            if let Err(error) = save_state(&root, &state) { revert(&finished[offset..]); return Err((error, report)); }
            let moved = match commit.take() {
                Some(commit) => publish_noclobber(&copy.path, &names[index], commit),
                None => publish_noclobber(&copy.path, &names[index], || Ok(())),
            };
            match moved {
                Ok(()) => {
                    state.published = index as u32 + 1;
                    state.publishing = None;
                    let _ = save_state(&root, &state);
                    let first = counts[..index].iter().sum();
                    published.push(PublishedPart { path: names[index].to_string_lossy().into_owned(), first_page: first, pages: counts[index], bytes: copy.bytes });
                }
                Err(error) => {
                    state.publishing = None;
                    let _ = save_state(&root, &state);
                    revert(&finished[offset..]);
                    return Err((error, report));
                }
            }
        }
        Ok((published, report))
    }
}

fn revert(copies: &[crate::searchable_pdf_stream::FinishedCopy]) {
    for copy in copies { let _ = reopen_for_resume(&copy.path); }
}

fn describe(names: &[PathBuf], counts: &[u32], bytes: impl Fn(&Path) -> u64) -> Vec<PublishedPart> {
    let mut first_page = 0;
    names.iter().zip(counts).map(|(name, pages)| {
        let part = PublishedPart { path: name.to_string_lossy().into_owned(), first_page, pages: *pages, bytes: bytes(name) };
        first_page += pages;
        part
    }).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn part_names_follow_the_chosen_name() {
        let names = part_names(Path::new("E:/scans/Deed book 12.pdf"), 3);
        assert_eq!(names, ["Deed book 12 (part 1 of 3).pdf", "Deed book 12 (part 2 of 3).pdf", "Deed book 12 (part 3 of 3).pdf"].map(|name| Path::new("E:/scans").join(name)));
    }

    #[test]
    fn an_invalid_parts_record_is_refused() {
        let folder = tempfile::tempdir().unwrap();
        let manifest = crate::searchable_pdf_stream::tests::manifest(10, Path::new("E:/unused.pdf"));
        assert_eq!(load_state(folder.path(), &manifest).unwrap(), None);
        for bad in [
            &br#"{"format":2,"closed":[3],"published":0,"publishing":null}"#[..],
            br#"{"format":1,"closed":[0],"published":0,"publishing":null}"#,
            br#"{"format":1,"closed":[4,6],"published":0,"publishing":null}"#,
            br#"{"format":1,"closed":[4],"published":3,"publishing":null}"#,
            br#"{"format":1,"closed":[4],"published":2,"publishing":5}"#,
            br#"{"format":1,"closed":[4],"published":0,"publishing":null,"extra":1}"#,
            b"not json",
        ] {
            std::fs::write(folder.path().join(PARTS_FILE), bad).unwrap();
            assert!(matches!(load_state(folder.path(), &manifest), Err(StreamError::Unusable(_))), "{}", String::from_utf8_lossy(bad));
        }
        let good = PartsState { format: 1, closed: vec![4, 5], published: 1, publishing: Some(99) };
        save_state(folder.path(), &good).unwrap();
        assert_eq!(load_state(folder.path(), &manifest).unwrap(), Some(good));
    }
}
