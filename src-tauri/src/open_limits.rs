use std::{io::Read, path::Path};

const GIB: u64 = 1024 * 1024 * 1024;
/// Largest single PDF held open, whether opened from disk or created as a copy (combine, fill, redact, OCR, ...).
/// The worker keeps the source twice while a document is open (`EditSession.source` and PDFium's copy),
/// so a document at this limit costs about 2 GiB.
pub const MAX_OPEN_SOURCE_BYTES: u64 = GIB;
/// Most documents held by the worker at once. Opens and every copy that opens as a new tab count.
pub const MAX_OPEN_DOCUMENTS: usize = 32;
/// Total source bytes held across all open documents (about 8 GiB including PDFium's copies).
pub const MAX_OPEN_SOURCE_TOTAL: u64 = 4 * GIB;

fn mib(bytes: u64) -> u64 { bytes.div_ceil(1024 * 1024) }

fn size_text(bytes: u64) -> String { if bytes >= GIB && bytes % GIB == 0 { format!("{} GiB", bytes / GIB) } else { format!("{} MiB", mib(bytes)) } }

fn too_large(len: Option<u64>, limit: u64) -> String {
    let limit = size_text(limit);
    match len {
        Some(len) => format!("This PDF is {} MiB, which is over the {limit} size limit for opening a file. Nothing was opened.", mib(len)),
        None => format!("This PDF is over the {limit} size limit for opening a file. Nothing was opened."),
    }
}

fn capacity(open: impl IntoIterator<Item = u64>, incoming: u64, max_documents: usize, retry: &str) -> Result<(), String> {
    let (count, total) = open.into_iter().fold((0usize, 0u64), |(count, total), len| (count + 1, total.saturating_add(len)));
    if count >= max_documents { return Err(format!("Up to {max_documents} PDFs can be open at once. Close a tab, then {retry} Nothing was opened.")); }
    if total.saturating_add(incoming) > MAX_OPEN_SOURCE_TOTAL { return Err(format!("Open PDFs are limited to {} in total. Close a large tab, then {retry} Nothing was opened.", size_text(MAX_OPEN_SOURCE_TOTAL))); }
    Ok(())
}

/// Refuses a new document when the worker already holds the maximum count, or when adding `incoming`
/// bytes would take the open sources past the total budget. `open` yields the source size of each open document.
/// `max_documents` is `MAX_OPEN_DOCUMENTS` in the app; the worker passes it in so the shared in-process test
/// worker, which many parallel tests use without closing their documents, can run with a larger count.
pub fn ensure_open_capacity(open: impl IntoIterator<Item = u64>, incoming: u64, max_documents: usize) -> Result<(), String> { capacity(open, incoming, max_documents, "open this PDF again.") }

/// The same limits for a copy that opens as a new tab, checked with the produced bytes before the copy is written.
pub fn ensure_copy_capacity(open: impl IntoIterator<Item = u64>, len: u64, max_documents: usize) -> Result<(), String> {
    if len > MAX_OPEN_SOURCE_BYTES { return Err(format!("The new PDF would be {} MiB, over the {} size limit for an open file. No file was written. Nothing was opened.", mib(len), size_text(MAX_OPEN_SOURCE_BYTES))); }
    capacity(open, len, max_documents, "try again. No file was written.")
}

/// Reads a whole file of at most `limit` bytes. The size is checked from the open handle's metadata before any
/// byte is read, and the read itself stops one byte past the limit, so a file that grows after the check is refused too.
/// The not-a-regular-file check is a backstop for paths that skip the open-path gate (recovery re-reads); devices,
/// pipes and UNC device paths from the webview are stopped earlier by `open_path::gate_open_path_from`.
pub fn read_bounded(path: &Path, limit: u64) -> Result<Vec<u8>, String> {
    let file = std::fs::File::open(path).map_err(|error| error.to_string())?;
    let metadata = file.metadata().map_err(|error| error.to_string())?;
    if !metadata.is_file() { return Err("This PDF could not be opened because it is not a regular file. Nothing was opened.".into()); }
    if metadata.len() > limit { return Err(too_large(Some(metadata.len()), limit)); }
    read_limited(file, metadata.len(), limit)
}

fn read_limited(reader: impl Read, expected: u64, limit: u64) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::with_capacity(usize::try_from(expected.min(limit)).map_err(|_| too_large(Some(expected), limit))?);
    reader.take(limit.saturating_add(1)).read_to_end(&mut bytes).map_err(|error| error.to_string())?;
    if bytes.len() as u64 > limit { return Err(too_large(None, limit)); }
    Ok(bytes)
}

/// Reads a document source for opening: capacity first (before touching the file), then size from metadata, then the bytes.
pub fn read_open_source(path: &Path, open: impl IntoIterator<Item = u64> + Clone, max_documents: usize) -> Result<Vec<u8>, String> { read_open_source_within(path, open, max_documents, MAX_OPEN_SOURCE_BYTES) }

fn read_open_source_within(path: &Path, open: impl IntoIterator<Item = u64> + Clone, max_documents: usize, limit: u64) -> Result<Vec<u8>, String> {
    ensure_open_capacity(open.clone(), 0, max_documents)?;
    let len = std::fs::metadata(path).map_err(|error| error.to_string())?.len();
    if len > limit { return Err(too_large(Some(len), limit)); }
    ensure_open_capacity(open.clone(), len, max_documents)?;
    let bytes = read_bounded(path, limit)?;
    ensure_open_capacity(open, bytes.len() as u64, max_documents)?;
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("smacrobat-open-limits-{}-{name}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn size_limit_is_one_gib_and_admits_large_real_files() {
        assert_eq!(MAX_OPEN_SOURCE_BYTES, 1 << 30);
        assert!(MAX_OPEN_SOURCE_BYTES >= 900 * 1024 * 1024);
        assert!(too_large(Some(MAX_OPEN_SOURCE_BYTES + 1), MAX_OPEN_SOURCE_BYTES).contains("1 GiB size limit"));
    }

    #[test]
    fn bounded_read_accepts_the_limit_and_refuses_one_byte_more_before_reading() {
        let dir = temp("bounded");
        let path = dir.join("doc.pdf");
        std::fs::write(&path, vec![b'%'; 64]).unwrap();
        assert_eq!(read_bounded(&path, 64).unwrap().len(), 64);
        let error = read_bounded(&path, 63).unwrap_err();
        assert!(error.contains("size limit") && error.contains("Nothing was opened"), "{error}");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn oversized_source_is_refused_from_metadata_before_capacity_is_spent() {
        let dir = temp("oversized");
        let path = dir.join("big.pdf");
        std::fs::write(&path, vec![b'%'; 3 * 1024 * 1024 + 1]).unwrap();
        let error = read_open_source_within(&path, std::iter::empty(), MAX_OPEN_DOCUMENTS, 3 * 1024 * 1024).unwrap_err();
        assert!(error.contains("This PDF is 4 MiB") && error.contains("3 MiB size limit") && error.contains("Nothing was opened"), "{error}");
        assert_eq!(read_open_source_within(&path, std::iter::empty(), MAX_OPEN_DOCUMENTS, 4 * 1024 * 1024).unwrap().len(), 3 * 1024 * 1024 + 1);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn growth_after_the_metadata_check_is_refused() {
        let grown = std::io::Cursor::new(vec![b'%'; 69]);
        let error = read_limited(grown, 64, 64).unwrap_err();
        assert!(error.contains("over the 1 MiB size limit") && error.ends_with("Nothing was opened."), "{error}");
        assert_eq!(read_limited(std::io::Cursor::new(vec![b'%'; 64]), 64, 64).unwrap().len(), 64);
    }

    #[test]
    fn copies_obey_the_size_count_and_total_limits_before_writing() {
        let error = ensure_copy_capacity(std::iter::empty(), MAX_OPEN_SOURCE_BYTES + 1, MAX_OPEN_DOCUMENTS).unwrap_err();
        assert!(error.contains("over the 1 GiB size limit") && error.contains("No file was written. Nothing was opened."), "{error}");
        assert!(ensure_copy_capacity(std::iter::empty(), MAX_OPEN_SOURCE_BYTES, MAX_OPEN_DOCUMENTS).is_ok());
        let error = ensure_copy_capacity(vec![1; MAX_OPEN_DOCUMENTS], 1, MAX_OPEN_DOCUMENTS).unwrap_err();
        assert!(error.contains("Up to 32 PDFs") && error.contains("No file was written. Nothing was opened."), "{error}");
        let error = ensure_copy_capacity(vec![MAX_OPEN_SOURCE_BYTES; 4], 1, MAX_OPEN_DOCUMENTS).unwrap_err();
        assert!(error.contains("4 GiB in total") && error.ends_with("Nothing was opened."), "{error}");
    }

    #[test]
    fn the_document_count_is_the_one_the_worker_passes() {
        assert_eq!(MAX_OPEN_DOCUMENTS, 32);
        assert!(ensure_open_capacity(vec![1; MAX_OPEN_DOCUMENTS], 1, 4096).is_ok());
        assert!(ensure_open_capacity(vec![1; 4096], 1, 4096).unwrap_err().contains("Up to 4096 PDFs"));
    }

    #[test]
    fn folders_are_refused() {
        let dir = temp("folder");
        assert!(read_bounded(&dir, 1024).is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn open_count_and_total_budget_refuse_as_a_whole() {
        assert!(ensure_open_capacity(vec![1; MAX_OPEN_DOCUMENTS - 1], 1, MAX_OPEN_DOCUMENTS).is_ok());
        let error = ensure_open_capacity(vec![1; MAX_OPEN_DOCUMENTS], 1, MAX_OPEN_DOCUMENTS).unwrap_err();
        assert!(error.contains("Up to 32 PDFs"), "{error}");
        assert!(ensure_open_capacity(vec![MAX_OPEN_SOURCE_BYTES; 3], MAX_OPEN_SOURCE_BYTES, MAX_OPEN_DOCUMENTS).is_ok());
        let error = ensure_open_capacity(vec![MAX_OPEN_SOURCE_BYTES; 3], MAX_OPEN_SOURCE_BYTES + 1, MAX_OPEN_DOCUMENTS).unwrap_err();
        assert!(error.contains("4 GiB in total"), "{error}");
        let dir = temp("capacity");
        let path = dir.join("doc.pdf");
        std::fs::write(&path, b"%PDF-1.4").unwrap();
        assert!(read_open_source(&path, vec![1u64; MAX_OPEN_DOCUMENTS], MAX_OPEN_DOCUMENTS).unwrap_err().contains("Up to 32 PDFs"));
        assert!(read_open_source(&path, vec![MAX_OPEN_SOURCE_TOTAL], MAX_OPEN_DOCUMENTS).unwrap_err().contains("4 GiB in total"));
        assert_eq!(read_open_source(&path, vec![1u64; MAX_OPEN_DOCUMENTS - 1], MAX_OPEN_DOCUMENTS).unwrap(), b"%PDF-1.4");
        std::fs::remove_dir_all(dir).unwrap();
    }
}
