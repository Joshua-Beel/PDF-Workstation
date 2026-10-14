use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

const DEVICE_NAMES: [&str; 6] = ["CON", "PRN", "AUX", "NUL", "CONIN$", "CONOUT$"];

fn reject(reason: &str) -> String { format!("This path cannot be opened: {reason}.") }

fn reserved_device_component(component: &str) -> bool {
    let stem = component.split('.').next().unwrap_or("").trim_end_matches(' ').to_ascii_uppercase();
    if DEVICE_NAMES.contains(&stem.as_str()) { return true; }
    let numbered = |prefix: &str| stem.strip_prefix(prefix).is_some_and(|rest| matches!(rest.chars().collect::<Vec<_>>().as_slice(), [digit] if matches!(digit, '1'..='9' | '\u{b9}' | '\u{b2}' | '\u{b3}')));
    numbered("COM") || numbered("LPT")
}

pub(crate) fn is_drive_letter_path(text: &str) -> bool {
    let bytes = text.as_bytes();
    bytes.len() >= 3 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' && bytes[2] == b'\\'
}

/// Where an open request came from. The backend decides this; the webview can never assert `UserChosen`.
/// `UserChosen` means the path came straight out of a native dialog in this process, or was dropped onto the window
/// and recorded by the backend's own drag-drop event (see `OpenPathGrants`). Only that origin may name a network share.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum OpenOrigin { Webview, UserChosen }

const UNC_ONLY_PICKED: &str = "network (UNC) paths are not supported unless you pick the file in the Open dialog or drop it onto the window";

fn bad_unc_part(part: &str) -> bool { part.is_empty() || part == "." || part == ".." || part.contains(|c| matches!(c, ':' | '*' | '?' | '"' | '<' | '>' | '|')) }

/// `rest` is the text after `\\` or `\\?\UNC\`: `server\share\file...`. Plain share paths only.
fn classify_unc(rest: &str) -> Result<(), String> {
    let parts: Vec<&str> = rest.split('\\').collect();
    if parts.len() < 3 || parts.iter().any(|part| bad_unc_part(part)) { return Err(reject("a network path must look like \\\\server\\share\\folder\\file.pdf")); }
    if ["pipe", "ipc$", "mailslot"].iter().any(|name| parts[1].eq_ignore_ascii_case(name)) { return Err(reject("pipe and IPC shares are not files and cannot be opened")); }
    if parts[1..].iter().any(|part| reserved_device_component(part)) { return Err(reject("device names such as CON, NUL and COM1 are not supported")); }
    Ok(())
}

/// String-level classification. Accepts only an absolute `X:\...` path or a verbatim `\\?\X:\...` path; with
/// `OpenOrigin::UserChosen` it also accepts a plain `\\server\share\...` or `\\?\UNC\server\share\...` file path.
/// Rejects relative paths, device and namespace forms (`\\.\`, other `\\?\`, pipes), UNC for the webview origin,
/// reserved device names in any component, and alternate data streams. Pure; touches no filesystem.
pub(crate) fn classify_open_path(path: &Path) -> Result<(), String> { classify_open_path_for(path, OpenOrigin::Webview) }

pub(crate) fn classify_open_path_for(path: &Path, origin: OpenOrigin) -> Result<(), String> {
    let text = path.to_string_lossy().replace('/', "\\");
    let allow_unc = origin == OpenOrigin::UserChosen;
    if text.is_empty() || text.contains('\0') { return Err(reject("the path is empty or contains a null character")); }
    let rest = if let Some(verbatim) = text.strip_prefix("\\\\?\\") {
        if verbatim.get(..4).is_some_and(|prefix| prefix.eq_ignore_ascii_case("UNC\\")) {
            return if allow_unc { classify_unc(verbatim.get(4..).unwrap_or("")) } else { Err(reject(UNC_ONLY_PICKED)) };
        }
        if !is_drive_letter_path(verbatim) { return Err(reject("device and namespace paths are not supported")); }
        verbatim
    } else if text.starts_with("\\\\") {
        if text.starts_with("\\\\.\\") { return Err(reject("device and pipe paths are not supported")); }
        return if allow_unc { classify_unc(text.get(2..).unwrap_or("")) } else { Err(reject(UNC_ONLY_PICKED)) };
    } else if is_drive_letter_path(&text) { text.as_str() } else {
        return Err(reject("the path must be absolute and start with a drive letter"));
    };
    let tail = rest.get(3..).unwrap_or("");
    if tail.contains(':') { return Err(reject("alternate data streams are not supported")); }
    if tail.split('\\').any(reserved_device_component) { return Err(reject("device names such as CON, NUL and COM1 are not supported")); }
    Ok(())
}

/// Full gate: classify the string, canonicalise, classify again, then require a regular file.
/// A drive letter that canonicalises to a UNC path is a mapped network drive and is allowed; a UNC string is not.
pub(crate) fn validate_open_path(path: &Path) -> Result<(), String> { validate_open_path_from(path, OpenOrigin::Webview) }

pub(crate) fn validate_open_path_from(path: &Path, origin: OpenOrigin) -> Result<(), String> {
    validate_with(path, origin, &|path| std::fs::canonicalize(path), &|path| std::fs::metadata(path).map(|metadata| metadata.is_file()))
}

/// The filesystem is reached only through `canonicalize` and `is_file`, and only after the string classifier has
/// accepted the path for this origin. A UNC string from the webview origin therefore never reaches either.
fn validate_with(path: &Path, origin: OpenOrigin, canonicalize: &dyn Fn(&Path) -> std::io::Result<PathBuf>, is_file: &dyn Fn(&Path) -> std::io::Result<bool>) -> Result<(), String> {
    classify_open_path_for(path, origin)?;
    let canonical = canonicalize(path).map_err(|error| format!("This file could not be found or read: {error}."))?;
    let text = canonical.to_string_lossy().into_owned();
    let mapped = text.get(..8).is_some_and(|prefix| prefix.eq_ignore_ascii_case("\\\\?\\UNC\\"));
    if !mapped || origin == OpenOrigin::UserChosen { classify_open_path_for(&canonical, origin)?; }
    else if text.get(8..).is_some_and(|unc| unc.split('\\').skip(2).any(reserved_device_component) || unc.contains(':')) { return Err(reject("device names and streams are not supported")); }
    if !is_file(&canonical).map_err(|error| format!("This file could not be read: {error}."))? { return Err(reject("it is not a regular file (folders, devices and pipes cannot be opened)")); }
    Ok(())
}

pub(crate) async fn gate_open_path(path: PathBuf) -> Result<PathBuf, String> { gate_open_path_from(path, OpenOrigin::Webview).await }

pub(crate) async fn gate_open_path_from(path: PathBuf, origin: OpenOrigin) -> Result<PathBuf, String> {
    classify_open_path_for(&path, origin)?;
    tauri::async_runtime::spawn_blocking(move || validate_open_path_from(&path, origin).map(|()| path)).await.map_err(|error| error.to_string())?
}

const GRANT_TTL: Duration = Duration::from_secs(30);
const MAX_GRANTS: usize = 8;

/// Network paths the backend itself saw the user drop onto the window. Recorded by the Rust `DragDrop` window event,
/// never from a webview message; each entry is consumed by one open and expires after 30 seconds.
#[derive(Default)]
pub(crate) struct OpenPathGrants(Mutex<Vec<(String, Instant)>>);

fn grant_key(path: &Path) -> Option<String> {
    let text = path.to_string_lossy().replace('/', "\\");
    let key = match text.get(..8) { Some(prefix) if prefix.eq_ignore_ascii_case("\\\\?\\UNC\\") => format!("\\\\{}", text.get(8..).unwrap_or("")), _ => text };
    let rest = key.strip_prefix("\\\\")?;
    let mut parts = rest.splitn(3, '\\');
    let (host, share, tail) = (parts.next()?, parts.next()?, parts.next().unwrap_or(""));
    Some(format!("\\\\{}\\{}\\{}", host.to_lowercase(), share.to_lowercase(), tail))
}

impl OpenPathGrants {
    pub(crate) fn grant(&self, path: &Path) { self.grant_at(path, Instant::now()); }
    pub(crate) fn consume(&self, path: &Path) -> bool { self.consume_at(path, Instant::now()) }
    fn grant_at(&self, path: &Path, now: Instant) {
        let Some(key) = grant_key(path) else { return; };
        let mut entries = self.0.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        entries.retain(|(_, at)| now.saturating_duration_since(*at) <= GRANT_TTL);
        if entries.len() >= MAX_GRANTS { entries.remove(0); }
        entries.push((key, now));
    }
    fn consume_at(&self, path: &Path, now: Instant) -> bool {
        let Some(key) = grant_key(path) else { return false; };
        let mut entries = self.0.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        entries.retain(|(_, at)| now.saturating_duration_since(*at) <= GRANT_TTL);
        match entries.iter().position(|(entry, _)| *entry == key) { Some(index) => { entries.remove(index); true } None => false }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_unc_device_namespace_and_relative_forms() {
        for text in [
            r"\\server\share\a.pdf", "//server/share/a.pdf", r"\\?\UNC\server\share\a.pdf", r"\\?\unc\server\share\a.pdf",
            r"\\.\pipe\name", r"\\.\PhysicalDrive0", r"\\.\C:\a.pdf", r"\\?\GLOBALROOT\Device\X", r"\\?\pipe\x", r"\\?\Volume{1234}\a.pdf",
            "CON", "nul", "COM1", r"C:\dir\NUL", r"C:\dir\nul.pdf", r"C:\dir\COM9.txt", r"C:\dir\LPT3", r"C:\CON\a.pdf", r"C:\dir\CONOUT$", r"C:\dir\com1 .pdf",
            r"a.pdf", r"dir\a.pdf", r"\a.pdf", r"C:a.pdf", r"C:", "", "C:\\a\0.pdf", r"C:\a.pdf:stream", r"C:\a.pdf::$DATA",
        ] {
            assert!(classify_open_path(Path::new(text)).is_err(), "should reject {text:?}");
        }
    }

    #[test]
    fn accepts_local_drive_and_verbatim_disk_forms() {
        for text in [r"C:\a.pdf", r"c:\Users\me\My Docs\a.pdf", "C:/Users/me/a.pdf", r"G:\My Drive\a.pdf", r"\\?\C:\Users\me\a.pdf", r"C:\console.pdf", r"C:\com10.pdf", r"C:\comma\a.pdf", r"C:\nullable.pdf"] {
            assert!(classify_open_path(Path::new(text)).is_ok(), "should accept {text:?}");
        }
    }

    #[test]
    fn non_ascii_text_never_panics_at_prefix_boundaries() {
        for text in [
            "\\\\?\\abc\u{20ac}", "\\\\?\\\u{20ac}", "\\\\?\\a\u{20ac}", "\\\\?\\ab\u{20ac}", "\\\\?\\\u{e9}\u{e9}\u{e9}\u{e9}", "\\\\?\\UN\u{20ac}",
            "\\\\?\\UNC\u{20ac}", "\\\\?\\C:\u{20ac}", "\\\\\u{20ac}", "\\\\.\u{20ac}", "\u{20ac}", "\u{20ac}:\\a.pdf", "C\u{20ac}\\a.pdf", "C:\u{20ac}a.pdf",
            "\u{dc}:\\a.pdf", "\\\\?\\\u{dc}:\\a.pdf", "C:\\\u{20ac}\u{20ac}\u{20ac}", "\\\\\u{20ac}\u{20ac}\u{20ac}\u{20ac}\u{20ac}",
        ] {
            assert!(classify_open_path(Path::new(text)).is_err() || text.starts_with("C:\\"), "unexpected result for {text:?}");
            let _ = validate_open_path(Path::new(text));
        }
        for text in ["\\\\?\\abc\u{20ac}", "\\\\?\\UNC\u{20ac}", "\\\\?\\\u{20ac}", "\\\\\u{20ac}"] {
            assert!(classify_open_path(Path::new(text)).is_err(), "should reject {text:?}");
        }
    }

    #[test]
    fn accepts_a_real_file_under_a_non_ascii_top_level_folder() {
        let folder = tempfile::tempdir().unwrap();
        let unicode = folder.path().join("\u{dc}nterlagen \u{20ac}");
        std::fs::create_dir(&unicode).unwrap();
        let file = unicode.join("a.pdf"); std::fs::write(&file, b"%PDF-1.4").unwrap();
        validate_open_path(&file).unwrap();
        assert!(classify_open_path(Path::new("C:\\\u{dc}nterlagen\\a.pdf")).is_ok());
        assert!(tauri::async_runtime::block_on(gate_open_path(file.clone())).is_ok());
    }

    #[test]
    fn requires_an_existing_regular_file() {
        let folder = tempfile::tempdir().unwrap();
        let file = folder.path().join("a.pdf"); std::fs::write(&file, b"%PDF-1.4").unwrap();
        validate_open_path(&file).unwrap();
        let error = validate_open_path(folder.path()).unwrap_err();
        assert!(error.contains("not a regular file"), "{error}");
        assert!(validate_open_path(&folder.path().join("missing.pdf")).unwrap_err().contains("could not be found"));
        assert!(validate_open_path(Path::new(r"\\localhost\c$\Windows\win.ini")).unwrap_err().contains("UNC"));
    }

    #[test]
    fn service_open_entries_used_by_reopen_document_are_gated() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let service = crate::service::PdfService::start(root.join("resources/pdfium/bin/pdfium.dll"));
        let folder = tempfile::tempdir().unwrap();
        let file = folder.path().join("ok.pdf"); std::fs::copy(root.join("resources/welcome.pdf"), &file).unwrap();
        for bad in [PathBuf::from(r"\\127.0.0.1\share\a.pdf"), PathBuf::from(r"\\.\pipe\x"), PathBuf::from("NUL"), folder.path().to_path_buf(), folder.path().join("missing.pdf")] {
            assert!(tauri::async_runtime::block_on(service.begin_open(bad.clone())).is_err(), "begin_open must reject {bad:?}");
            assert!(tauri::async_runtime::block_on(service.open(bad.clone())).is_err(), "open must reject {bad:?}");
        }
        assert!(matches!(tauri::async_runtime::block_on(service.begin_open(file)).unwrap(), crate::service::OpenResult::Opened { .. }));
    }

    #[test]
    fn unlock_rejects_passwords_over_one_kib_before_any_read() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let service = crate::service::PdfService::start(root.join("resources/pdfium/bin/pdfium.dll"));
        let error = tauri::async_runtime::block_on(service.unlock(1, "a".repeat(1025))).err().unwrap();
        assert!(error.contains("1 KiB"), "{error}");
        let multibyte = "\u{e9}".repeat(513);
        assert!(tauri::async_runtime::block_on(service.unlock(1, multibyte)).err().unwrap().contains("1 KiB"));
        let at_limit = tauri::async_runtime::block_on(service.unlock(1, "a".repeat(1024))).err().unwrap();
        assert!(at_limit.contains("expired"), "{at_limit}");
    }

    use std::cell::Cell;
    fn fake_validate(path: &str, origin: OpenOrigin, canonical: &str) -> (Result<(), String>, usize) {
        let calls = Cell::new(0usize);
        let result = validate_with(Path::new(path), origin, &|_| { calls.set(calls.get() + 1); Ok(PathBuf::from(canonical)) }, &|_| { calls.set(calls.get() + 1); Ok(true) });
        (result, calls.get())
    }

    #[test]
    fn user_chosen_origin_accepts_plain_unc_without_a_real_share() {
        for text in [r"\\server\share\a.pdf", "//server/share/dir/a.pdf", r"\\?\UNC\server\share\a.pdf", r"\\?\unc\10.0.0.5\Docs\a.pdf", r"\\localhost\c$\a.pdf"] {
            assert!(classify_open_path_for(Path::new(text), OpenOrigin::UserChosen).is_ok(), "should accept {text:?}");
            assert!(classify_open_path_for(Path::new(text), OpenOrigin::Webview).is_err(), "webview must reject {text:?}");
        }
        let (result, calls) = fake_validate(r"\\server\share\a.pdf", OpenOrigin::UserChosen, r"\\?\UNC\server\share\a.pdf");
        assert!(result.is_ok(), "{result:?}"); assert_eq!(calls, 2);
    }

    #[test]
    fn webview_unc_is_rejected_before_any_filesystem_call() {
        for text in [r"\\server\share\a.pdf", r"\\?\UNC\server\share\a.pdf"] {
            let (result, calls) = fake_validate(text, OpenOrigin::Webview, r"\\?\UNC\server\share\a.pdf");
            assert!(result.unwrap_err().contains("UNC"));
            assert_eq!(calls, 0, "{text:?} reached the filesystem");
        }
    }

    #[test]
    fn dialog_provenance_still_refuses_devices_pipes_and_malformed_shares() {
        for text in [r"\\.\pipe\x", r"\\.\PhysicalDrive0", r"\\?\GLOBALROOT\Device\X", r"\\?\pipe\x", r"\\?\Volume{1234}\a.pdf", "NUL", r"C:\dir\NUL", r"a.pdf",
            r"\\server", r"\\server\share", r"\\server\share\", r"\\\share\a.pdf", r"\\server\\a.pdf", r"\\server\share\..\a.pdf", r"\\server\share\a.pdf:s", r"\\server\share\NUL", r"\\server\share\dir\COM1.pdf", r"\\?\UNC\server", r"\\?\UNC\server\share\a?.pdf", r"\\.\UNC\server\share\a.pdf"] {
            let (result, calls) = fake_validate(text, OpenOrigin::UserChosen, text);
            assert!(result.is_err(), "should refuse {text:?}");
            assert_eq!(calls, 0, "{text:?} reached the filesystem");
        }
        let (result, _) = fake_validate(r"\\server\share\a.pdf", OpenOrigin::UserChosen, r"\\.\pipe\x");
        assert!(result.is_err(), "a canonical form that is a device must be refused");
        let calls = Cell::new(0);
        let folder = validate_with(Path::new(r"\\server\share\dir"), OpenOrigin::UserChosen, &|path| Ok(path.to_path_buf()), &|_| { calls.set(1); Ok(false) });
        assert!(folder.unwrap_err().contains("not a regular file"));
    }

    #[test]
    fn grants_are_exact_single_use_and_expire() {
        let grants = OpenPathGrants::default();
        let start = Instant::now();
        grants.grant_at(Path::new(r"\\Server\Share\a.pdf"), start);
        assert!(!grants.consume_at(Path::new(r"\\server\share\b.pdf"), start));
        assert!(!grants.consume_at(Path::new(r"\\?\UNC\server\share\A.pdf"), start), "file part is case-exact");
        assert!(grants.consume_at(Path::new(r"\\?\UNC\server\share\a.pdf"), start + Duration::from_secs(5)));
        assert!(!grants.consume_at(Path::new(r"\\server\share\a.pdf"), start + Duration::from_secs(6)), "a grant is consumed by one open");
        grants.grant_at(Path::new("//server/share/a.pdf"), start);
        assert!(!grants.consume_at(Path::new(r"\\server\share\a.pdf"), start + GRANT_TTL + Duration::from_secs(1)), "expired");
        grants.grant_at(Path::new(r"C:\local\a.pdf"), start);
        assert!(!grants.consume_at(Path::new(r"C:\local\a.pdf"), start), "only network paths are recorded");
        for index in 0..20 { grants.grant_at(Path::new(&format!(r"\\s\h\{index}.pdf")), start); }
        assert!(grants.0.lock().unwrap().len() <= MAX_GRANTS);
        assert!(!grants.consume_at(Path::new(r"\\s\h\0.pdf"), start), "oldest grants are evicted first");
        assert!(grants.consume_at(Path::new(r"\\s\h\19.pdf"), start));
    }

    #[test]
    fn grant_folds_only_host_and_share() {
        let grants = OpenPathGrants::default();
        let now = Instant::now();
        grants.grant_at(Path::new(r"\\Host\Share\Dir\Report.pdf"), now);
        assert!(!grants.consume_at(Path::new(r"\\host\share\dir\report.pdf"), now), "different case below the share is a different file");
        assert!(!grants.consume_at(Path::new(r"\\host\share\Dir\REPORT.pdf"), now));
        assert!(grants.consume_at(Path::new(r"\\HOST\SHARE\Dir\Report.pdf"), now));
        assert_eq!(grant_key(Path::new(r"\\?\UNC\H\S\X\y.pdf")).as_deref(), Some(r"\\h\s\X\y.pdf"));
        assert_eq!(grant_key(Path::new(r"C:\x\y.pdf")), None);
        assert_eq!(grant_key(Path::new(r"\\only")), None);
    }

    #[test]
    fn dialog_provenance_refuses_pipe_and_ipc_shares() {
        for text in [r"\\host\pipe\name", r"\\host\PIPE\a.pdf", r"\\host\IPC$\a.pdf", r"\\host\ipc$\x\a.pdf", r"\\?\UNC\host\Pipe\a.pdf", r"\\host\MailSlot\a.pdf"] {
            assert!(classify_open_path_for(Path::new(text), OpenOrigin::UserChosen).is_err(), "should refuse {text:?}");
        }
        for text in [r"\\host\pipes\a.pdf", r"\\host\share\pipe\a.pdf", r"\\host\ipc\a.pdf"] {
            assert!(classify_open_path_for(Path::new(text), OpenOrigin::UserChosen).is_ok(), "should accept {text:?}");
        }
    }

    #[test]
    fn gate_rejects_before_touching_the_filesystem() {
        let error = tauri::async_runtime::block_on(gate_open_path(PathBuf::from(r"\\127.0.0.1\share\a.pdf"))).unwrap_err();
        assert!(error.contains("UNC"), "{error}");
        let folder = tempfile::tempdir().unwrap();
        assert!(tauri::async_runtime::block_on(gate_open_path(folder.path().to_path_buf())).is_err());
    }
}
