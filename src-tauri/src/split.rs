use crate::editor::{write_new_file, EditSession};
use serde::Serialize;
use std::{ffi::OsStr, path::{Component, Path, PathBuf}};

const MAX_OUTPUT_FILES: usize = 64;
const MAX_CHUNK_BYTES: usize = 256 * 1024 * 1024;

#[derive(Debug, Serialize)]
pub struct SplitFile {
    pub path: PathBuf,
    pub first_page: usize,
    pub last_page: usize,
    pub page_count: usize,
}

#[derive(Debug, Serialize)]
pub struct SplitOutput { pub folder: PathBuf, pub files: Vec<SplitFile> }

fn group_count(page_count: usize, pages_per_file: usize) -> Result<usize, String> {
    if page_count == 0 { return Err("This document has no pages to split.".into()); }
    if pages_per_file == 0 { return Err("Pages per file must be greater than zero.".into()); }
    let count = page_count / pages_per_file + usize::from(page_count % pages_per_file != 0);
    if count > MAX_OUTPUT_FILES { return Err(format!("Split is limited to {MAX_OUTPUT_FILES} output files. Increase pages per file.")); }
    Ok(count)
}

fn validate_folder_name(name: &OsStr) -> Result<(), String> {
    let text = name.to_str().ok_or("Choose a folder name containing valid Unicode.")?;
    let mut components = Path::new(name).components();
    if text.is_empty() || text.encode_utf16().count() > 120 || !matches!(components.next(), Some(Component::Normal(_))) || components.next().is_some()
        || text.ends_with(['.', ' ']) || text.chars().any(|character| character.is_control() || "\\/:*?\"<>|".contains(character)) {
        return Err("Choose a single folder name with no path separators or reserved characters.".into());
    }
    let stem = text.split('.').next().unwrap().to_ascii_uppercase();
    if ["CON", "PRN", "AUX", "NUL"].contains(&stem.as_str()) || (stem.len() == 4 && (stem.starts_with("COM") || stem.starts_with("LPT")) && matches!(stem.as_bytes()[3], b'1'..=b'9')) {
        return Err("Choose a folder name that is not reserved by Windows.".into());
    }
    Ok(())
}

const SPLIT_FOLDER_NAME: &str = "Split PDFs";
const MAX_SPLIT_FOLDER_SUFFIX: u32 = 99;

/// The new split folder inside `parent` (a folder the user picked in the native dialog): the
/// first of "Split PDFs", "Split PDFs (2)", ... "Split PDFs (99)" that does not exist yet. Nothing
/// is created here; publishing stays atomic and still refuses a folder that appears meanwhile.
pub fn new_split_folder(parent: &Path) -> Result<PathBuf, String> {
    for suffix in 1..=MAX_SPLIT_FOLDER_SUFFIX {
        let name = if suffix == 1 { SPLIT_FOLDER_NAME.to_owned() } else { format!("{SPLIT_FOLDER_NAME} ({suffix})") };
        let folder = parent.join(&name);
        match folder.symlink_metadata() {
            Ok(_) => continue,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(folder),
            Err(error) => return Err(format!("The split folder could not be checked: {error}")),
        }
    }
    Err(format!("That folder already holds {MAX_SPLIT_FOLDER_SUFFIX} split folders. Choose another folder."))
}

const NOT_A_FOLDER_ON_DISK: &str = "Choose a folder on a drive or network share. Libraries, This PC and other virtual locations cannot hold split files.";

/// The file-system path of a folder chosen in the folder picker, or a clear refusal for a virtual
/// location (a Library, This PC) that has none.
#[cfg(windows)]
fn folder_path_of(item: &windows::Win32::UI::Shell::IShellItem) -> Result<PathBuf, String> {
    use windows::Win32::{System::Com::CoTaskMemFree, UI::Shell::SIGDN_FILESYSPATH};
    let name = unsafe { item.GetDisplayName(SIGDN_FILESYSPATH) }.map_err(|_| NOT_A_FOLDER_ON_DISK.to_owned())?;
    let text = unsafe { name.to_string() };
    unsafe { CoTaskMemFree(Some(name.0 as *const _)) };
    let path = PathBuf::from(text.map_err(|_| NOT_A_FOLDER_ON_DISK.to_owned())?);
    if path.as_os_str().is_empty() || !path.is_dir() { return Err(NOT_A_FOLDER_ON_DISK.into()); }
    Ok(path)
}

/// Shows the native folder picker (file-system folders only), owned by `owner`, and returns the new
/// split folder inside the chosen folder; `Ok(None)` when the user cancels. rfd's `pick_folder`
/// drops `FOS_FORCEFILESYSTEM` and reports a virtual location (a Library) exactly like Cancel, so
/// the dialog closed silently. Call it on a blocking thread: it initialises COM there.
#[cfg(windows)]
pub fn pick_new_split_folder(owner: *mut std::ffi::c_void, title: &str) -> Result<Option<PathBuf>, String> {
    use windows::{core::HSTRING, Win32::{Foundation::{ERROR_CANCELLED, HWND}, System::Com::{CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE}, UI::Shell::{FileOpenDialog, IFileOpenDialog, FOS_FORCEFILESYSTEM, FOS_PATHMUSTEXIST, FOS_PICKFOLDERS}}};
    let opened = |error: windows::core::Error| format!("The folder picker could not open: {error}");
    let initialised = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE) }.is_ok();
    let result = (|| unsafe {
        let dialog: IFileOpenDialog = CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER).map_err(opened)?;
        dialog.SetOptions(dialog.GetOptions().map_err(opened)? | FOS_PICKFOLDERS | FOS_FORCEFILESYSTEM | FOS_PATHMUSTEXIST).map_err(opened)?;
        dialog.SetTitle(&HSTRING::from(title)).map_err(opened)?;
        match dialog.Show(Some(HWND(owner))) {
            Ok(()) => {}
            Err(error) if error.code() == ERROR_CANCELLED.to_hresult() => return Ok(None),
            Err(error) => return Err(format!("The folder picker failed: {error}")),
        }
        let item = dialog.GetResult().map_err(|_| NOT_A_FOLDER_ON_DISK.to_owned())?;
        new_split_folder(&folder_path_of(&item)?).map(Some)
    })();
    if initialised { unsafe { CoUninitialize() }; }
    result
}

#[cfg(not(windows))]
pub fn pick_new_split_folder(_: *mut std::ffi::c_void, _: &str) -> Result<Option<PathBuf>, String> {
    Err("Splitting into a folder is supported on Windows in this build.".into())
}

#[cfg(windows)]
fn publish_directory(source: &Path, destination: &Path) -> Result<(), String> {
    use std::os::windows::ffi::OsStrExt;
    use windows::{core::PCWSTR, Win32::Storage::FileSystem::{MoveFileExW, MOVE_FILE_FLAGS}};
    let source: Vec<_> = source.as_os_str().encode_wide().chain(Some(0)).collect();
    let destination: Vec<_> = destination.as_os_str().encode_wide().chain(Some(0)).collect();
    unsafe { MoveFileExW(PCWSTR(source.as_ptr()), PCWSTR(destination.as_ptr()), MOVE_FILE_FLAGS(0)) }
        .map_err(|error| format!("The split folder could not be published. An existing folder is never replaced: {error}"))
}

#[cfg(not(windows))]
fn publish_directory(_: &Path, _: &Path) -> Result<(), String> {
    Err("Atomic split folder publishing is supported on Windows in this build.".into())
}

pub fn prepare_and_publish(
    session: &EditSession,
    parent: &Path,
    folder_name: &OsStr,
    pages_per_file: usize,
    mut validate: impl FnMut(&[u8], usize) -> Result<(), String>,
) -> Result<SplitOutput, String> {
    let count = group_count(session.plan.len(), pages_per_file)?;
    validate_folder_name(folder_name)?;
    let parent = parent.canonicalize().map_err(|error| format!("The output folder is unavailable: {error}"))?;
    if !parent.is_dir() { return Err("Choose an existing output folder.".into()); }
    let folder = parent.join(folder_name);
    match folder.symlink_metadata() {
        Ok(_) => return Err("A split folder with that name already exists now (it appeared in the chosen folder during the split). Split again to use the next free Split PDFs name.".into()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {},
        Err(error) => return Err(format!("The split folder could not be checked: {error}")),
    }
    let staged = tempfile::Builder::new().prefix(".pdf-split-").tempdir_in(&parent).map_err(|error| format!("The split folder could not be prepared: {error}"))?;
    let mut files = Vec::with_capacity(count);
    let mut start = 0;
    while start < session.plan.len() {
        let end = start.saturating_add(pages_per_file).min(session.plan.len());
        let pages: Vec<_> = (start..end).collect();
        let bytes = session.export(Some(&pages))?;
        if bytes.len() > MAX_CHUNK_BYTES { return Err("A split PDF exceeds the output limit of 256 MiB. Reduce pages per file.".into()); }
        validate(&bytes, pages.len()).map_err(|error| format!("Split pages {}–{end} failed validation: {error}", start + 1))?;
        let name = format!("pages-{:04}-{:04}.pdf", start + 1, end);
        write_new_file(&staged.path().join(&name), &bytes)?;
        files.push(SplitFile { path: folder.join(name), first_page: start + 1, last_page: end, page_count: pages.len() });
        start = end;
    }
    publish_directory(staged.path(), &folder)?;
    let _ = staged.keep();
    Ok(SplitOutput { folder, files })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor::PageEdit;
    use lopdf::{dictionary, Document, Object};

    fn sample() -> Vec<u8> { std::fs::read(Path::new(env!("CARGO_MANIFEST_DIR")).join("resources/welcome.pdf")).unwrap() }
    fn validate(bytes: &[u8], expected: usize) -> Result<(), String> {
        let document = Document::load_mem(bytes).map_err(|error| error.to_string())?;
        if document.get_pages().len() != expected { return Err("Page count differs.".into()); }
        Ok(())
    }

    #[test]
    fn groups_are_bounded_and_folder_names_cannot_escape_parent() {
        assert_eq!(group_count(6, 2).unwrap(), 3); assert_eq!(group_count(7, 3).unwrap(), 3);
        assert_eq!(group_count(6, usize::MAX).unwrap(), 1); assert_eq!(group_count(64, 1).unwrap(), 64);
        assert!(group_count(0, 1).is_err()); assert!(group_count(6, 0).is_err()); assert!(group_count(65, 1).unwrap_err().contains("64"));
        for name in ["", ".", "..", "../escape", "a/b", "a\\b", "C:\\folder", "NUL", "COM1.pdf", "LPT9", "trailing.", "trailing ", "a\0b"] { assert!(validate_folder_name(OsStr::new(name)).is_err(), "{name:?}"); }
        assert!(validate_folder_name(OsStr::new("split café")).is_ok());
    }

    #[test]
    #[cfg(windows)]
    fn folder_picker_result_refuses_virtual_locations_with_a_clear_message() {
        use windows::{core::HSTRING, Win32::{System::Com::{CoInitializeEx, CoUninitialize, COINIT_APARTMENTTHREADED}, UI::Shell::{IShellItem, SHCreateItemFromParsingName}}};
        let initialised = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) }.is_ok();
        {
            let item = |path: &str| -> IShellItem { unsafe { SHCreateItemFromParsingName(&HSTRING::from(path), None) }.unwrap() };
            let folder = tempfile::tempdir().unwrap();
            // The shell may spell the same folder differently from TEMP (a runner's TEMP uses the 8.3
            // name RUNNER~1, the shell returns runneradmin), so compare the canonical folders.
            let chosen = folder_path_of(&item(folder.path().to_str().unwrap())).unwrap();
            assert!(chosen.is_dir(), "{chosen:?}");
            assert_eq!(std::fs::canonicalize(&chosen).unwrap(), std::fs::canonicalize(folder.path()).unwrap());
            // The Libraries root and This PC: shell items with no file-system path.
            for virtual_location in ["::{031E4825-7B94-4dc3-B131-E946B44C8DD5}", "::{20D04FE0-3AEA-1069-A2D8-08002B30309D}"] {
                assert_eq!(folder_path_of(&item(virtual_location)).unwrap_err(), NOT_A_FOLDER_ON_DISK, "{virtual_location}");
            }
        }
        if initialised { unsafe { CoUninitialize() }; }
    }

    #[test]
    fn new_split_folder_picks_the_first_unused_name_inside_the_chosen_folder() {
        let parent = tempfile::tempdir().unwrap();
        assert_eq!(new_split_folder(parent.path()).unwrap(), parent.path().join("Split PDFs"));
        std::fs::create_dir(parent.path().join("Split PDFs")).unwrap();
        std::fs::write(parent.path().join("Split PDFs (2)"), b"a file, not a folder").unwrap();
        let next = new_split_folder(parent.path()).unwrap();
        assert_eq!(next, parent.path().join("Split PDFs (3)"));
        assert!(!next.exists(), "nothing is created before publishing");
        validate_folder_name(next.file_name().unwrap()).unwrap();
        for suffix in 3..=MAX_SPLIT_FOLDER_SUFFIX { std::fs::create_dir(parent.path().join(format!("Split PDFs ({suffix})"))).unwrap(); }
        assert!(new_split_folder(parent.path()).unwrap_err().contains("Choose another folder"));
    }

    #[test]
    #[cfg(windows)]
    fn split_uses_current_order_rotation_and_preserves_session_state() {
        let source = sample();
        let original = Document::load_mem(&source).unwrap();
        let original_pages: Vec<_> = original.get_pages().into_values().collect();
        let mut session = EditSession::new(source.clone(), 6);
        session.apply(PageEdit::Move { from: 0, to: 2 }).unwrap();
        session.mark_saved();
        let saved_plan = session.plan.clone();
        session.apply(PageEdit::Rotate { pages: vec![0], clockwise: true }).unwrap();
        let plan = session.plan.clone(); let revision = session.revision;
        let folder = tempfile::tempdir().unwrap();
        let output = prepare_and_publish(&session, folder.path(), OsStr::new("split café"), 4, validate).unwrap();
        assert_eq!(output.files.len(), 2);
        for (chunk, file) in output.files.iter().enumerate() {
            assert_eq!((file.first_page, file.last_page, file.page_count), if chunk == 0 { (1, 4, 4) } else { (5, 6, 2) });
            let document = Document::load(&file.path).unwrap();
            for (index, page) in document.get_pages().into_values().enumerate() {
                let spec = &plan[file.first_page - 1 + index];
                assert_eq!(document.get_page_content(page), original.get_page_content(original_pages[spec.source]));
                let rotation = document.get_dictionary(page).unwrap().get(b"Rotate").unwrap().as_i64().unwrap();
                assert_eq!(rotation, i64::from(spec.turns) * 90);
            }
        }
        assert_eq!(session.source, source); assert_eq!(session.plan, plan); assert_eq!(session.revision, revision);
        assert!(session.dirty()); assert!(session.can_undo()); assert!(!session.can_redo());
        session.apply(PageEdit::Undo).unwrap(); assert_eq!(session.plan, saved_plan); assert!(!session.dirty());
    }

    #[test]
    fn failed_validation_cleans_staging_without_publishing_partial_output() {
        let session = EditSession::new(sample(), 6);
        let folder = tempfile::tempdir().unwrap(); let mut calls = 0;
        let error = prepare_and_publish(&session, folder.path(), OsStr::new("new split"), 2, |bytes, expected| {
            calls += 1; validate(bytes, expected)?;
            if calls == 2 { return Err("Injected validation failure.".into()); } Ok(())
        }).unwrap_err();
        assert_eq!(calls, 2); assert!(error.contains("Injected validation failure"));
        assert_eq!(std::fs::read_dir(folder.path()).unwrap().count(), 0);
    }

    #[test]
    #[cfg(windows)]
    fn existing_and_racing_targets_are_never_replaced() {
        let session = EditSession::new(sample(), 6);
        let folder = tempfile::tempdir().unwrap();
        let existing = folder.path().join("existing"); std::fs::create_dir(&existing).unwrap();
        let sentinel = existing.join("sentinel.pdf"); std::fs::write(&sentinel, b"source bytes").unwrap();
        assert!(prepare_and_publish(&session, folder.path(), OsStr::new("existing"), 2, |_, _| panic!("Existing target must reject before validation")).is_err());
        assert_eq!(std::fs::read(&sentinel).unwrap(), b"source bytes");
        let raced = folder.path().join("raced"); let mut calls = 0;
        assert!(prepare_and_publish(&session, folder.path(), OsStr::new("raced"), 2, |bytes, expected| {
            validate(bytes, expected)?; calls += 1;
            if calls == 3 { std::fs::create_dir(&raced).unwrap(); } Ok(())
        }).is_err());
        assert!(raced.is_dir()); assert_eq!(std::fs::read_dir(&raced).unwrap().count(), 0);
        assert_eq!(std::fs::read_dir(folder.path()).unwrap().count(), 2);
    }

    #[test]
    fn preservation_guards_reject_malformed_signed_encrypted_and_complex_documents() {
        let folder = tempfile::tempdir().unwrap();
        let check = |source: Vec<u8>| {
            let session = EditSession::new(source.clone(), 6);
            assert!(prepare_and_publish(&session, folder.path(), OsStr::new("blocked"), 2, validate).is_err());
            assert_eq!(session.source, source); assert_eq!(session.revision, 0); assert!(!session.can_undo());
            assert_eq!(std::fs::read_dir(folder.path()).unwrap().count(), 0);
        };
        check(b"not a PDF".to_vec());
        for key in [b"Perms".as_slice(), b"AcroForm", b"StructTreeRoot", b"PageLabels", b"Threads", b"Outlines", b"Dests", b"Names", b"OpenAction"] {
            let mut document = Document::load_mem(&sample()).unwrap(); document.catalog_mut().unwrap().set(key, dictionary! {});
            let mut bytes = Vec::new(); document.save_to(&mut bytes).unwrap(); check(bytes);
        }
        let mut document = Document::load_mem(&sample()).unwrap();
        document.add_object(dictionary! { "Type" => "Sig", "ByteRange" => vec![0.into(), 1.into(), 2.into(), 3.into()] });
        let mut bytes = Vec::new(); document.save_to(&mut bytes).unwrap(); check(bytes);
        let mut document = Document::load_mem(&sample()).unwrap();
        let first = document.get_pages()[&1]; document.get_object_mut(first).unwrap().as_dict_mut().unwrap().set("Annots", Vec::<Object>::new());
        let mut bytes = Vec::new(); document.save_to(&mut bytes).unwrap(); check(bytes);
        let mut document = Document::load_mem(&sample()).unwrap();
        document.trailer.set("ID", vec![Object::string_literal("split-fixture"), Object::string_literal("split-fixture")]);
        let encryption = lopdf::EncryptionVersion::V2 { document: &document, owner_password: "fixture owner", user_password: "fixture reader", key_length: 128, permissions: lopdf::Permissions::all() };
        document.encrypt(&lopdf::EncryptionState::try_from(encryption).unwrap()).unwrap();
        let mut bytes = Vec::new(); document.save_to(&mut bytes).unwrap(); check(bytes);
    }
}
