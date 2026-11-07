//! Test-only harness for the annotation interchange E2 check (SM3 P2-5): runs a pinned qpdf over a committed
//! synthetic fixture and returns its JSON. qpdf only ever runs here, inside a Job Object with one process, a
//! memory cap, every hardening option, capped stdout and stderr and a deadline. The argument list is fixed and
//! read-only (qpdf writes no file in JSON mode), and the input must be a file under `src-tauri/tests/fixtures`,
//! so the harness cannot be pointed at real documents. Before every run, qpdf.exe and every file pinned beside
//! it are checked against `scripts/qpdf/pins.json` and held open against change (SM7 C-1). The child gets a
//! minimal environment and the PREFER_SYSTEM32 and NO_REMOTE image-load mitigations (SM7 R-1), so a library it
//! loads by name comes from System32 or its own pinned folder only. CI runs E2 explicitly (`ci.yml`, C-2).

use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::fs::File;
use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::time::Duration;

use sha2::{Digest, Sha256};

use crate::child_job::{self, ChildCommand, ChildSystemError, JobLimits, OutputBounds, TerminalReason};

const QPDF_LABEL: &str = "qpdf interchange check";
const QPDF_MEMORY: usize = 1 << 30;
const QPDF_STDOUT_LIMIT: usize = 64 << 20;
const QPDF_STDERR_LIMIT: usize = 64 << 10;
const QPDF_TIMEOUT: Duration = Duration::from_secs(60);
const QPDF_POLL: Duration = Duration::from_millis(20);

/// The text is read through `Debug` in test failure messages.
#[derive(Debug)]
#[allow(dead_code)]
pub(crate) enum QpdfError {
    Refused(String),
    System(String),
    Failed(String),
}

impl From<ChildSystemError> for QpdfError {
    fn from(error: ChildSystemError) -> Self {
        Self::System(error.0)
    }
}

fn fixtures_root() -> Result<PathBuf, QpdfError> {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .canonicalize()
        .map_err(|error| QpdfError::System(format!("fixtures folder: {error}")))
}

/// The input must be an existing `.pdf` file whose canonical path (links resolved) lies under the committed
/// fixtures folder.
pub(crate) fn checked_fixture(pdf: &Path) -> Result<PathBuf, QpdfError> {
    if !pdf.is_absolute() {
        return Err(QpdfError::Refused(format!("{} is not an absolute path", pdf.display())));
    }
    let canonical = pdf.canonicalize().map_err(|error| QpdfError::Refused(format!("{}: {error}", pdf.display())))?;
    if !canonical.starts_with(fixtures_root()?) {
        return Err(QpdfError::Refused(format!("{} is not a committed test fixture", pdf.display())));
    }
    if !canonical.is_file() || canonical.extension().and_then(OsStr::to_str).map(str::to_ascii_lowercase).as_deref() != Some("pdf") {
        return Err(QpdfError::Refused(format!("{} is not a PDF file", pdf.display())));
    }
    Ok(canonical)
}

/// The program must be an existing file named `qpdf.exe` given by absolute path (as setup-qpdf.ps1 prints it).
pub(crate) fn checked_qpdf(qpdf: &Path) -> Result<PathBuf, QpdfError> {
    if !qpdf.is_absolute() {
        return Err(QpdfError::Refused(format!("{} is not an absolute path", qpdf.display())));
    }
    let canonical = qpdf.canonicalize().map_err(|error| QpdfError::Refused(format!("{}: {error}", qpdf.display())))?;
    if !canonical.is_file() || !canonical.file_name().and_then(OsStr::to_str).is_some_and(|name| name.eq_ignore_ascii_case("qpdf.exe")) {
        return Err(QpdfError::Refused(format!("{} is not qpdf.exe", qpdf.display())));
    }
    Ok(canonical)
}

/// The full argument list. Nothing else is ever passed to qpdf.
pub(crate) fn qpdf_arguments(qpdf: &Path, pdf: &Path) -> Vec<std::ffi::OsString> {
    vec![qpdf.as_os_str().to_os_string(), "--json=2".into(), "--json-key=qpdf".into(), pdf.as_os_str().to_os_string()]
}

/// Pinned binaries by file name, from `scripts/qpdf/pins.json`. Every pinned path must sit in one folder (the
/// zip's `bin`), and `files` must not be null.
pub(crate) fn pinned_files() -> Result<BTreeMap<String, String>, QpdfError> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("scripts").join("qpdf").join("pins.json");
    let text = std::fs::read(&path).map_err(|error| QpdfError::System(format!("{}: {error}", path.display())))?;
    let pins: serde_json::Value = serde_json::from_slice(&text).map_err(|error| QpdfError::Refused(format!("pins.json: {error}")))?;
    let Some(files) = pins["files"].as_object() else {
        return Err(QpdfError::Refused("pins.json has no per-file hashes; qpdf is not trusted".into()));
    };
    let mut folder = None;
    let mut pinned = BTreeMap::new();
    for (name, hash) in files {
        let (parent, file) = name.rsplit_once('/').ok_or_else(|| QpdfError::Refused(format!("pinned path {name} has no folder")))?;
        if *folder.get_or_insert(parent) != parent {
            return Err(QpdfError::Refused("pinned files are not all in one folder".into()));
        }
        let hash = hash.as_str().filter(|hash| hash.len() == 64 && hash.bytes().all(|byte| byte.is_ascii_hexdigit()));
        let hash = hash.ok_or_else(|| QpdfError::Refused(format!("pinned hash for {name} is not a SHA-256")))?;
        pinned.insert(file.to_ascii_lowercase(), hash.to_ascii_lowercase());
    }
    Ok(pinned)
}

fn is_binary(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    name.ends_with(".exe") || name.ends_with(".dll")
}

/// Checks `qpdf.exe` and every pinned file in its folder against `pinned` (file name to SHA-256), and refuses any
/// other `.exe` or `.dll` there, so no unpinned library can be loaded beside it. Each pinned file is opened
/// without following a reparse point, hashed through that handle and kept open with read-only sharing: the
/// returned handles stop anyone writing, renaming or deleting the files until they are dropped, which closes
/// the gap between this check and the run.
pub(crate) fn verify_and_hold(qpdf: &Path, pinned: &BTreeMap<String, String>) -> Result<Vec<File>, QpdfError> {
    const FILE_SHARE_READ: u32 = 0x1;
    const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
    if !pinned.contains_key("qpdf.exe") {
        return Err(QpdfError::Refused("qpdf.exe is not pinned".into()));
    }
    let folder = qpdf.parent().ok_or_else(|| QpdfError::Refused(format!("{} has no folder", qpdf.display())))?;
    refuse_unpinned(folder, pinned)?;
    let mut held = Vec::with_capacity(pinned.len());
    for (name, expected) in pinned {
        let path = folder.join(name);
        let mut file = std::fs::OpenOptions::new()
            .read(true)
            .share_mode(FILE_SHARE_READ)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
            .open(&path)
            .map_err(|error| QpdfError::Refused(format!("pinned file {name}: {error}")))?;
        let metadata = file.metadata().map_err(|error| QpdfError::System(format!("{name}: {error}")))?;
        if !metadata.is_file() || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return Err(QpdfError::Refused(format!("pinned file {name} is not a plain file")));
        }
        let mut hasher = Sha256::new();
        std::io::copy(&mut file, &mut hasher).map_err(|error| QpdfError::System(format!("{name}: {error}")))?;
        let actual = hasher.finalize().iter().map(|byte| format!("{byte:02x}")).collect::<String>();
        if &actual != expected {
            return Err(QpdfError::Refused(format!("{name} does not match its pinned SHA-256")));
        }
        held.push(file);
    }
    // Again with the pinned files held, so nothing added during the check is missed.
    refuse_unpinned(folder, pinned)?;
    Ok(held)
}

fn refuse_unpinned(folder: &Path, pinned: &BTreeMap<String, String>) -> Result<(), QpdfError> {
    let entries = std::fs::read_dir(folder).map_err(|error| QpdfError::System(format!("{}: {error}", folder.display())))?;
    for entry in entries {
        let entry = entry.map_err(|error| QpdfError::System(format!("{}: {error}", folder.display())))?;
        let name = entry.file_name().to_string_lossy().to_ascii_lowercase();
        if is_binary(&name) && !pinned.contains_key(&name) {
            return Err(QpdfError::Refused(format!("unpinned executable or library {name} beside qpdf.exe")));
        }
    }
    Ok(())
}

/// Runs qpdf over one fixture under the job limits above and returns its stdout (qpdf JSON v2). The binaries are
/// checked against `scripts/qpdf/pins.json` first, on every call, whatever `QPDF_EXE` says.
pub(crate) fn qpdf_json(qpdf: &Path, pdf: &Path) -> Result<Vec<u8>, QpdfError> {
    let pdf = checked_fixture(pdf)?;
    let qpdf = checked_qpdf(qpdf)?;
    run_pinned(&qpdf, &pdf, &pinned_files()?)
}

/// The qpdf child's whole environment (SM7 R-1): `SystemRoot`, a `PATH` of only System32 and the Windows folder,
/// and `TEMP`/`TMP` set to `temp`. Nothing else from this process reaches the child, so a library qpdf loads
/// by name can only come from its own pinned folder or the system folders, never from a user-writable `PATH`
/// entry. Entries are sorted by name, as `CreateProcessW` expects.
pub(crate) fn qpdf_environment(temp: &Path) -> Result<Vec<u16>, QpdfError> {
    let system_root = std::env::var_os("SystemRoot").map(PathBuf::from).ok_or_else(|| QpdfError::System("SystemRoot is not set".into()))?;
    let plain = |path: &Path| path.is_absolute() && !path.as_os_str().to_string_lossy().contains([';', '\0', '=']);
    if !plain(&system_root) || !plain(temp) {
        return Err(QpdfError::Refused("SystemRoot or the temporary folder is not a plain absolute path".into()));
    }
    let entries = [
        ("PATH", format!("{};{}", system_root.join("System32").display(), system_root.display())),
        ("SystemRoot", system_root.display().to_string()),
        ("TEMP", temp.display().to_string()),
        ("TMP", temp.display().to_string()),
    ];
    let mut block = Vec::new();
    for (name, value) in entries {
        block.extend(format!("{name}={value}").encode_utf16());
        block.push(0);
    }
    block.push(0);
    Ok(block)
}

fn run_pinned(qpdf: &Path, pdf: &Path, pinned: &BTreeMap<String, String>) -> Result<Vec<u8>, QpdfError> {
    let _held = verify_and_hold(qpdf, pinned)?;
    let directory = fixtures_root()?;
    let temp = tempfile::Builder::new().prefix("sm3-qpdf-child-").tempdir().map_err(|error| QpdfError::System(format!("qpdf temporary folder: {error}")))?;
    let environment = qpdf_environment(temp.path())?;
    let suspended = child_job::spawn_suspended_isolated::<QpdfError>(
        QPDF_LABEL,
        &JobLimits::hardened_single_process(QPDF_MEMORY),
        &environment,
        || {
            let null = |_: child_job::ContainsNul| QpdfError::Refused("a path contains a NUL character".into());
            let arguments = qpdf_arguments(qpdf, pdf);
            let arguments = arguments.iter().map(|argument| argument.as_os_str()).collect::<Vec<_>>();
            Ok(ChildCommand {
                application: child_job::wide_null(qpdf.as_os_str()).map_err(null)?,
                command_line: child_job::command_line(&arguments).map_err(null)?,
                current_directory: child_job::wide_null(directory.as_os_str()).map_err(null)?,
            })
        },
        || Ok(()),
    )?;
    let running = suspended.resume()?;
    let bounds = OutputBounds {
        stdout_limit: QPDF_STDOUT_LIMIT,
        stderr_limit: QPDF_STDERR_LIMIT,
        timeout: QPDF_TIMEOUT,
        poll_interval: QPDF_POLL,
        drain_grace: child_job::PIPE_DRAIN_GRACE,
    };
    let run = child_job::run_bounded(running, &[], &bounds, &|| false)?;
    if run.stdout_exceeded || run.stderr_exceeded || run.io_cancelled || run.reason != TerminalReason::Exited {
        return Err(QpdfError::Failed(format!(
            "qpdf was stopped: {:?} (stdout over cap {}, stderr over cap {}, pipes cancelled {})",
            run.reason, run.stdout_exceeded, run.stderr_exceeded, run.io_cancelled
        )));
    }
    let code = run.child.exit_code()?;
    // qpdf exits 0 when clean and 3 when it only warned; anything else is a failure.
    if code != 0 && code != 3 {
        let stderr = String::from_utf8_lossy(&run.stderr[..run.stderr.len().min(4096)]).into_owned();
        return Err(QpdfError::Failed(format!("qpdf exited with {code}: {stderr}")));
    }
    Ok(run.stdout)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests").join("fixtures").join("annotations").join(name)
    }

    #[test]
    fn only_committed_fixtures_are_accepted() {
        assert!(checked_fixture(&fixture("app-annotations.pdf")).is_ok());
        assert!(matches!(checked_fixture(Path::new("tests/fixtures/annotations/app-annotations.pdf")), Err(QpdfError::Refused(_))));
        let escaped = fixture("..").join("..").join("..").join("Cargo.toml");
        assert!(matches!(checked_fixture(&escaped), Err(QpdfError::Refused(_))));
        let outside = tempfile::tempdir().unwrap();
        let elsewhere = outside.path().join("elsewhere.pdf");
        std::fs::write(&elsewhere, b"%PDF-1.4\n").unwrap();
        assert!(matches!(checked_fixture(&elsewhere), Err(QpdfError::Refused(_))));
        assert!(matches!(checked_fixture(&fixture("missing.pdf")), Err(QpdfError::Refused(_))));
    }

    #[test]
    fn a_fixture_reached_through_a_junction_is_refused() {
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("real.pdf"), b"%PDF-1.4\n").unwrap();
        let link = fixture("..").join(format!("sm3-qpdf-junction-{}", std::process::id()));
        let made = child_job::test_output(std::process::Command::new("cmd.exe").args(["/d", "/c", "mklink", "/J"]).arg(&link).arg(outside.path())).unwrap();
        assert!(made.status.success(), "{}", String::from_utf8_lossy(&made.stderr));
        let refused = checked_fixture(&link.join("real.pdf"));
        std::fs::remove_dir(&link).unwrap();
        assert!(matches!(refused, Err(QpdfError::Refused(_))), "{refused:?}");
    }

    #[test]
    fn the_program_must_be_qpdf_exe_and_nothing_is_spawned_otherwise() {
        let outside = tempfile::tempdir().unwrap();
        let impostor = outside.path().join("cmd.exe");
        std::fs::write(&impostor, b"").unwrap();
        assert!(matches!(qpdf_json(&impostor, &fixture("app-annotations.pdf")), Err(QpdfError::Refused(_))));
        assert!(matches!(qpdf_json(Path::new("qpdf.exe"), &fixture("app-annotations.pdf")), Err(QpdfError::Refused(_))));
        let named = outside.path().join("qpdf.exe");
        std::fs::write(&named, b"").unwrap();
        assert!(matches!(qpdf_json(&named, Path::new("C:\\Windows\\win.ini")), Err(QpdfError::Refused(_))));
    }

    fn sha256_hex(bytes: &[u8]) -> String {
        Sha256::digest(bytes).iter().map(|byte| format!("{byte:02x}")).collect()
    }

    fn stand_in(files: &[(&str, &[u8])]) -> (tempfile::TempDir, BTreeMap<String, String>) {
        let folder = tempfile::tempdir().unwrap();
        let mut pinned = BTreeMap::new();
        for (name, bytes) in files {
            std::fs::write(folder.path().join(name), bytes).unwrap();
            pinned.insert(name.to_ascii_lowercase(), sha256_hex(bytes));
        }
        (folder, pinned)
    }

    #[test]
    fn the_committed_pins_cover_qpdf_and_its_libraries_in_one_folder() {
        let pinned = pinned_files().unwrap();
        assert_eq!(pinned.len(), 12);
        assert_eq!(pinned["qpdf.exe"], "1bdaf878f37eaffea1ea3c30591214c77636a0f2fc7432427516b6e22e36c0ff");
        assert!(pinned.contains_key("qpdf30.dll"));
    }

    #[test]
    fn pinned_binaries_are_checked_and_held_against_change() {
        let (folder, pinned) = stand_in(&[("qpdf.exe", b"stand-in qpdf"), ("qpdf30.dll", b"stand-in library")]);
        let qpdf = folder.path().join("qpdf.exe");
        std::fs::write(folder.path().join("readme.txt"), b"not a binary").unwrap();
        let held = verify_and_hold(&qpdf, &pinned).unwrap();
        assert_eq!(held.len(), 2);
        assert!(std::fs::OpenOptions::new().write(true).open(folder.path().join("qpdf30.dll")).is_err(), "held files cannot be opened for writing");
        assert!(std::fs::rename(&qpdf, folder.path().join("moved.exe")).is_err(), "held files cannot be renamed");
        assert!(std::fs::remove_file(&qpdf).is_err(), "held files cannot be deleted");
        drop(held);
        std::fs::write(folder.path().join("qpdf30.dll"), b"altered library").unwrap();
        assert!(matches!(verify_and_hold(&qpdf, &pinned), Err(QpdfError::Refused(message)) if message.contains("qpdf30.dll does not match")));
    }

    #[test]
    fn unpinned_missing_or_absent_pins_are_refused() {
        let (folder, pinned) = stand_in(&[("qpdf.exe", b"stand-in qpdf")]);
        let qpdf = folder.path().join("qpdf.exe");
        std::fs::write(folder.path().join("Planted.DLL"), b"planted").unwrap();
        assert!(matches!(verify_and_hold(&qpdf, &pinned), Err(QpdfError::Refused(message)) if message.contains("planted.dll")));
        std::fs::remove_file(folder.path().join("Planted.DLL")).unwrap();
        let mut more = pinned.clone();
        more.insert("qpdf30.dll".into(), "0".repeat(64));
        assert!(matches!(verify_and_hold(&qpdf, &more), Err(QpdfError::Refused(message)) if message.contains("qpdf30.dll")));
        assert!(matches!(verify_and_hold(&qpdf, &BTreeMap::new()), Err(QpdfError::Refused(message)) if message.contains("not pinned")));
        assert!(verify_and_hold(&qpdf, &pinned).is_ok());
    }

    #[test]
    fn a_held_pinned_program_still_starts_under_the_job() {
        // A copy of where.exe stands in for qpdf.exe (qpdf itself is never run here). It rejects qpdf's
        // arguments and exits 2, which shows the program starts while the harness holds its handles.
        let system = std::env::var_os("SystemRoot").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(r"C:\Windows"));
        let bytes = std::fs::read(system.join("System32").join("where.exe")).unwrap();
        let (folder, pinned) = stand_in(&[("qpdf.exe", &bytes)]);
        let result = run_pinned(&folder.path().join("qpdf.exe"), &checked_fixture(&fixture("app-annotations.pdf")).unwrap(), &pinned);
        assert!(matches!(&result, Err(QpdfError::Failed(message)) if message.contains("exited with")), "{result:?}");
    }

    #[test]
    fn the_child_sees_only_the_minimal_environment() {
        assert!(std::env::var_os("USERNAME").is_some(), "the parent has a variable the child must not see");
        let temp = tempfile::tempdir().unwrap();
        let environment = qpdf_environment(temp.path()).unwrap();
        let system = PathBuf::from(std::env::var_os("SystemRoot").unwrap());
        let cmd = system.join("System32").join("cmd.exe");
        // The `set` command goes through stdin: cmd.exe's `/c` quote handling mangles the quoted arguments
        // `command_line` produces.
        let arguments = [cmd.as_os_str(), OsStr::new("/d"), OsStr::new("/q")];
        let suspended = child_job::spawn_suspended_isolated::<QpdfError>(
            "qpdf environment test",
            &JobLimits::hardened_single_process(QPDF_MEMORY),
            &environment,
            || {
                Ok(ChildCommand {
                    application: child_job::wide_null(cmd.as_os_str()).unwrap(),
                    command_line: child_job::command_line(&arguments).unwrap(),
                    current_directory: child_job::wide_null(temp.path().as_os_str()).unwrap(),
                })
            },
            || Ok(()),
        )
        .unwrap();
        let bounds = OutputBounds { stdout_limit: 1 << 16, stderr_limit: 1 << 16, timeout: QPDF_TIMEOUT, poll_interval: QPDF_POLL, drain_grace: child_job::PIPE_DRAIN_GRACE };
        let run = child_job::run_bounded(suspended.resume().unwrap(), b"set\r\nexit 0\r\n", &bounds, &|| false).unwrap();
        assert_eq!(run.reason, TerminalReason::Exited);
        let code = run.child.exit_code().unwrap();
        let listing = String::from_utf8_lossy(&run.stdout).into_owned();
        assert_eq!(code, 0, "cmd exited with {code:#x}; stdout {listing:?}; stderr {:?}", String::from_utf8_lossy(&run.stderr));
        // cmd.exe prints its prompt (`<folder>>`) before the first listed variable.
        let prompt = format!("{}>", temp.path().display()).to_ascii_lowercase();
        let unprompted = |line: &str| if line.to_ascii_lowercase().starts_with(&prompt) { line[prompt.len()..].to_owned() } else { line.to_owned() };
        let names = listing.lines().map(unprompted).filter_map(|line| line.split_once('=').map(|(name, _)| name.to_ascii_uppercase())).collect::<Vec<_>>();
        assert!(names.len() >= 4, "{listing}");
        // cmd.exe itself adds COMSPEC, PATHEXT and PROMPT to what it lists.
        for name in &names {
            assert!(["PATH", "SYSTEMROOT", "TEMP", "TMP", "COMSPEC", "PATHEXT", "PROMPT"].contains(&name.as_str()), "unexpected {name} in:\n{listing}");
        }
        let path = format!("Path={};{}", system.join("System32").display(), system.display());
        assert!(listing.lines().any(|line| line.eq_ignore_ascii_case(&path)), "{listing}");
        assert!(listing.lines().any(|line| line.eq_ignore_ascii_case(&format!("TEMP={}", temp.path().display()))), "{listing}");
        assert!(child_job::spawn_suspended_isolated::<QpdfError>("unterminated", &JobLimits::hardened_single_process(QPDF_MEMORY), &[65, 0], || unreachable!(), || Ok(())).is_err());
    }

    /// Reads the running child's image-load policy, then ends it. The child is `cmd /d /q`, which waits on its
    /// stdin pipe, so it is still alive when queried.
    fn image_load_policy(suspended: child_job::SuspendedChild) -> u32 {
        use windows::Win32::Foundation::CloseHandle;
        use windows::Win32::System::Threading::{GetProcessMitigationPolicy, OpenProcess, ProcessImageLoadPolicy, PROCESS_QUERY_INFORMATION};
        let mut running = suspended.resume().unwrap();
        let process = unsafe { OpenProcess(PROCESS_QUERY_INFORMATION, false, running.child.process_id()) }.unwrap();
        let mut flags = 0_u32;
        let read = unsafe { GetProcessMitigationPolicy(process, ProcessImageLoadPolicy, (&mut flags as *mut u32).cast(), std::mem::size_of::<u32>()) };
        unsafe { CloseHandle(process) }.unwrap();
        running.child.terminate_job_and_reap(0).unwrap();
        read.unwrap();
        flags
    }

    #[test]
    fn only_the_isolated_spawn_sets_prefer_system32_and_no_remote() {
        const NO_REMOTE_IMAGES: u32 = 1 << 0;
        const PREFER_SYSTEM32_IMAGES: u32 = 1 << 2;
        let temp = tempfile::tempdir().unwrap();
        let cmd = PathBuf::from(std::env::var_os("SystemRoot").unwrap()).join("System32").join("cmd.exe");
        let arguments = [cmd.as_os_str(), OsStr::new("/d"), OsStr::new("/q")];
        let command = || -> Result<ChildCommand, QpdfError> {
            Ok(ChildCommand {
                application: child_job::wide_null(cmd.as_os_str()).unwrap(),
                command_line: child_job::command_line(&arguments).unwrap(),
                current_directory: child_job::wide_null(temp.path().as_os_str()).unwrap(),
            })
        };
        let limits = JobLimits::hardened_single_process(QPDF_MEMORY);
        let environment = qpdf_environment(temp.path()).unwrap();
        let isolated = child_job::spawn_suspended_isolated::<QpdfError>("qpdf mitigation test", &limits, &environment, command, || Ok(())).unwrap();
        let flags = image_load_policy(isolated);
        assert_eq!(flags & (NO_REMOTE_IMAGES | PREFER_SYSTEM32_IMAGES), NO_REMOTE_IMAGES | PREFER_SYSTEM32_IMAGES, "{flags:#x}");
        let plain = child_job::spawn_suspended::<QpdfError>("qpdf mitigation control", &limits, command, || Ok(())).unwrap();
        let flags = image_load_policy(plain);
        assert_eq!(flags & PREFER_SYSTEM32_IMAGES, 0, "spawn_suspended is unchanged: {flags:#x}");
    }

    #[test]
    fn the_argument_list_is_fixed_and_read_only() {
        let arguments = qpdf_arguments(Path::new("Q:\\qpdf.exe"), Path::new("Q:\\in.pdf"));
        assert_eq!(arguments, ["Q:\\qpdf.exe", "--json=2", "--json-key=qpdf", "Q:\\in.pdf"].map(std::ffi::OsString::from));
    }

    /// E2. Ignored so that a run without qpdf never counts as a pass: run it with `--ignored` and `QPDF_EXE` set
    /// from the line scripts/setup-qpdf.ps1 prints. Without `QPDF_EXE` it fails.
    #[test]
    #[ignore = "E2: needs QPDF_EXE from scripts/setup-qpdf.ps1; ci.yml runs it with --ignored --exact"]
    fn qpdf_reads_the_interchange_fixtures_when_provided() {
        let qpdf = std::env::var_os("QPDF_EXE").expect("E2 not run: qpdf missing (set QPDF_EXE from scripts/setup-qpdf.ps1)");
        for name in ["app-annotations.pdf", "foreign-plus-app.pdf"] {
            let json = qpdf_json(Path::new(&qpdf), &fixture(name)).unwrap_or_else(|error| panic!("{name}: {error:?}"));
            let value: serde_json::Value = serde_json::from_slice(&json).unwrap();
            assert_eq!(value["version"], 2, "{name}");
            assert!(value["qpdf"].is_array(), "{name}");
        }
    }
}
