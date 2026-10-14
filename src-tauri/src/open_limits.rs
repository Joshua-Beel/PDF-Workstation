use std::{io::Read, path::Path};

/// Largest single PDF the app opens. The worker keeps the source twice while a document is open
/// (`EditSession.source` and PDFium's copy), so a document at this limit costs about 2 GiB.
pub const MAX_OPEN_SOURCE_BYTES: u64 = 1024 * 1024 * 1024;
/// Most documents held by the worker at once, counting derived copies such as redacted or OCR output tabs.
pub const MAX_OPEN_DOCUMENTS: usize = 32;
/// Total source bytes held across all open documents (about 8 GiB including PDFium's copies).
pub const MAX_OPEN_SOURCE_TOTAL: u64 = 4 * 1024 * 1024 * 1024;

fn mib(bytes: u64) -> u64 { bytes.div_ceil(1024 * 1024) }

fn too_large(len: Option<u64>, limit: u64) -> String {
    let limit = if limit >= 1024 * 1024 * 1024 && limit % (1024 * 1024 * 1024) == 0 { format!("{} GiB", limit / (1024 * 1024 * 1024)) } else { format!("{} MiB", mib(limit)) };
    match len {
        Some(len) => format!("This PDF is {} MiB, which is over the {limit} size limit for opening a file. Nothing was opened.", mib(len)),
        None => format!("This PDF is over the {limit} size limit for opening a file. Nothing was opened."),
    }
}

/// Refuses a new document when the worker already holds the maximum count, or when adding `incoming`
/// bytes would take the open sources past the total budget. `open` yields the source size of each open document.
pub fn ensure_open_capacity(open: impl IntoIterator<Item = u64>, incoming: u64) -> Result<(), String> {
    let (count, total) = open.into_iter().fold((0usize, 0u64), |(count, total), len| (count + 1, total.saturating_add(len)));
    if count >= MAX_OPEN_DOCUMENTS { return Err(format!("Up to {MAX_OPEN_DOCUMENTS} PDFs can be open at once. Close a tab, then open this PDF again. Nothing was opened.")); }
    if total.saturating_add(incoming) > MAX_OPEN_SOURCE_TOTAL { return Err(format!("Open PDFs are limited to {} GiB in total. Close a large tab, then open this PDF again. Nothing was opened.", MAX_OPEN_SOURCE_TOTAL / (1024 * 1024 * 1024))); }
    Ok(())
}

/// Reads a whole file of at most `limit` bytes. The size is checked from the open handle's metadata before any
/// byte is read, and the read itself stops one byte past the limit, so a file that grows after the check is refused too.
pub fn read_bounded(path: &Path, limit: u64) -> Result<Vec<u8>, String> {
    let file = std::fs::File::open(path).map_err(|error| error.to_string())?;
    let metadata = file.metadata().map_err(|error| error.to_string())?;
    if !metadata.is_file() { return Err("This PDF could not be opened: it is not a regular file (folders, devices and pipes cannot be opened).".into()); }
    if metadata.len() > limit { return Err(too_large(Some(metadata.len()), limit)); }
    let mut bytes = Vec::with_capacity(usize::try_from(metadata.len()).map_err(|_| too_large(Some(metadata.len()), limit))?);
    file.take(limit.saturating_add(1)).read_to_end(&mut bytes).map_err(|error| error.to_string())?;
    if bytes.len() as u64 > limit { return Err(too_large(None, limit)); }
    Ok(bytes)
}

/// Reads a document source for opening: capacity first (before touching the file), then size from metadata, then the bytes.
pub fn read_open_source(path: &Path, open: impl IntoIterator<Item = u64> + Clone) -> Result<Vec<u8>, String> { read_open_source_within(path, open, MAX_OPEN_SOURCE_BYTES) }

fn read_open_source_within(path: &Path, open: impl IntoIterator<Item = u64> + Clone, limit: u64) -> Result<Vec<u8>, String> {
    ensure_open_capacity(open.clone(), 0)?;
    let len = std::fs::metadata(path).map_err(|error| error.to_string())?.len();
    if len > limit { return Err(too_large(Some(len), limit)); }
    ensure_open_capacity(open.clone(), len)?;
    let bytes = read_bounded(path, limit)?;
    ensure_open_capacity(open, bytes.len() as u64)?;
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
        let error = read_open_source_within(&path, std::iter::empty(), 3 * 1024 * 1024).unwrap_err();
        assert!(error.contains("This PDF is 4 MiB") && error.contains("3 MiB size limit") && error.contains("Nothing was opened"), "{error}");
        assert_eq!(read_open_source_within(&path, std::iter::empty(), 4 * 1024 * 1024).unwrap().len(), 3 * 1024 * 1024 + 1);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn folders_are_refused() {
        let dir = temp("folder");
        assert!(read_bounded(&dir, 1024).is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn open_count_and_total_budget_refuse_as_a_whole() {
        assert!(ensure_open_capacity(vec![1; MAX_OPEN_DOCUMENTS - 1], 1).is_ok());
        let error = ensure_open_capacity(vec![1; MAX_OPEN_DOCUMENTS], 1).unwrap_err();
        assert!(error.contains("Up to 32 PDFs"), "{error}");
        assert!(ensure_open_capacity(vec![MAX_OPEN_SOURCE_BYTES; 3], MAX_OPEN_SOURCE_BYTES).is_ok());
        let error = ensure_open_capacity(vec![MAX_OPEN_SOURCE_BYTES; 3], MAX_OPEN_SOURCE_BYTES + 1).unwrap_err();
        assert!(error.contains("4 GiB in total"), "{error}");
        let dir = temp("capacity");
        let path = dir.join("doc.pdf");
        std::fs::write(&path, b"%PDF-1.4").unwrap();
        assert!(read_open_source(&path, vec![1u64; MAX_OPEN_DOCUMENTS]).unwrap_err().contains("Up to 32 PDFs"));
        assert!(read_open_source(&path, vec![MAX_OPEN_SOURCE_TOTAL]).unwrap_err().contains("4 GiB in total"));
        assert_eq!(read_open_source(&path, vec![1u64; MAX_OPEN_DOCUMENTS - 1]).unwrap(), b"%PDF-1.4");
        std::fs::remove_dir_all(dir).unwrap();
    }
}
