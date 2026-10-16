use std::fs::{File, OpenOptions};
use std::io::Read;
use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
use std::path::Path;

use windows::Win32::Storage::FileSystem::FILE_SHARE_READ;

const BASE_NOTICES_RELATIVE: &str = "resources/third-party-licenses/THIRD-PARTY-NOTICES.txt";
const BASE_NOTICES_LIMIT: u64 = 8 * 1024 * 1024;
const REPARSE_POINT: u32 = 0x400;

pub(crate) fn load(resource_dir: &Path) -> Result<String, String> {
    read_utf8_bounded(&resource_dir.join(BASE_NOTICES_RELATIVE), BASE_NOTICES_LIMIT)
        .map_err(|error| format!("Could not read bundled notices: {error}"))
}

fn read_utf8_bounded(path: &Path, limit: u64) -> Result<String, String> {
    String::from_utf8(read_bounded(path, limit)?).map_err(|_| "bundled notice text is not UTF-8".into())
}

fn read_bounded(path: &Path, limit: u64) -> Result<Vec<u8>, String> {
    reject_reparse_ancestors(path)?;
    let mut file = open_locked(path)?;
    let metadata = file.metadata().map_err(|error| format!("could not inspect bundled notice text: {error}"))?;
    if !metadata.is_file() || metadata.len() > limit { return Err("bundled notice text exceeds its size limit".into()); }
    let capacity = usize::try_from(metadata.len()).map_err(|_| "bundled notice text is too large")?;
    let mut bytes = Vec::with_capacity(capacity);
    file.by_ref().take(limit + 1).read_to_end(&mut bytes).map_err(|error| format!("could not read bundled notice text: {error}"))?;
    if bytes.len() != capacity { return Err("bundled notice text changed while it was read".into()); }
    Ok(bytes)
}

fn open_locked(path: &Path) -> Result<File, String> {
    OpenOptions::new().read(true).share_mode(FILE_SHARE_READ.0).open(path)
        .map_err(|error| format!("could not open bundled notice text: {error}"))
}

fn reject_reparse_ancestors(path: &Path) -> Result<(), String> {
    let mut current = Some(path);
    while let Some(candidate) = current {
        match std::fs::symlink_metadata(candidate) {
            Ok(metadata) if metadata.file_attributes() & REPARSE_POINT != 0 => return Err("bundled notice paths may not contain reparse points".into()),
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(format!("could not inspect bundled notice path: {error}")),
        }
        current = candidate.parent();
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_base(root: &Path, text: &[u8]) {
        let path = root.join(BASE_NOTICES_RELATIVE);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    #[test]
    fn bundled_text_is_returned_byte_for_byte_even_with_ocr_license_sidecars() {
        let folder = tempfile::tempdir().unwrap();
        let base = "base notices\r\nexact bytes";
        write_base(folder.path(), base.as_bytes());
        let sidecar = folder.path().join(format!("resources/ocr/{}/licenses/Tesseract-Apache-2.0.txt", "a".repeat(64)));
        std::fs::create_dir_all(sidecar.parent().unwrap()).unwrap();
        std::fs::write(sidecar, b"sidecar").unwrap();
        assert_eq!(load(folder.path()).unwrap(), base);
    }

    #[test]
    fn missing_or_non_utf8_notices_are_errors() {
        let folder = tempfile::tempdir().unwrap();
        assert!(load(folder.path()).unwrap_err().starts_with("Could not read bundled notices:"));
        write_base(folder.path(), &[0xff, 0xfe]);
        assert!(load(folder.path()).is_err());
    }

    #[test]
    fn bounded_reader_accepts_boundary_and_rejects_one_byte_over() {
        let folder = tempfile::tempdir().unwrap();
        let path = folder.path().join("notice.txt");
        std::fs::write(&path, b"1234").unwrap();
        assert_eq!(read_utf8_bounded(&path, 4).unwrap(), "1234");
        assert!(read_utf8_bounded(&path, 3).is_err());
    }

    #[test]
    fn reparse_notice_ancestor_is_rejected() {
        let folder = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        write_base(outside.path(), b"outside");
        let resources = folder.path().join("resources");
        let output = std::process::Command::new("cmd.exe").args(["/d", "/c", "mklink", "/J"]).arg(&resources).arg(outside.path().join("resources")).output().unwrap();
        assert!(output.status.success(), "mklink failed: stdout={} stderr={}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
        assert!(load(folder.path()).is_err());
        std::fs::remove_dir(&resources).unwrap();
    }
}
