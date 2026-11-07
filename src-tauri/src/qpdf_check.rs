//! Test-only harness for the annotation interchange E2 check (SM3 P2-5): runs a pinned qpdf over a committed
//! synthetic fixture and returns its JSON. qpdf only ever runs here, inside a Job Object with one process, a
//! memory cap, every hardening option, capped stdout and stderr and a deadline. The argument list is fixed and
//! read-only (qpdf writes no file in JSON mode), and the input must be a file under `src-tauri/tests/fixtures`,
//! so the harness cannot be pointed at real documents. Not wired into CI until SM7 has reviewed the pinned
//! binaries; without `QPDF_EXE` the run test reports that E2 did not run.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::time::Duration;

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

/// Runs qpdf over one fixture under the job limits above and returns its stdout (qpdf JSON v2).
pub(crate) fn qpdf_json(qpdf: &Path, pdf: &Path) -> Result<Vec<u8>, QpdfError> {
    let pdf = checked_fixture(pdf)?;
    let qpdf = checked_qpdf(qpdf)?;
    let directory = fixtures_root()?;
    let suspended = child_job::spawn_suspended::<QpdfError>(
        QPDF_LABEL,
        &JobLimits::hardened_single_process(QPDF_MEMORY),
        || {
            let null = |_: child_job::ContainsNul| QpdfError::Refused("a path contains a NUL character".into());
            let arguments = qpdf_arguments(&qpdf, &pdf);
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

    #[test]
    fn the_argument_list_is_fixed_and_read_only() {
        let arguments = qpdf_arguments(Path::new("Q:\\qpdf.exe"), Path::new("Q:\\in.pdf"));
        assert_eq!(arguments, ["Q:\\qpdf.exe", "--json=2", "--json-key=qpdf", "Q:\\in.pdf"].map(std::ffi::OsString::from));
    }

    #[test]
    fn qpdf_reads_the_interchange_fixtures_when_provided() {
        let Some(qpdf) = std::env::var_os("QPDF_EXE") else {
            eprintln!("E2 not run: qpdf missing (set QPDF_EXE from scripts/setup-qpdf.ps1)");
            return;
        };
        for name in ["app-annotations.pdf", "foreign-plus-app.pdf"] {
            let json = qpdf_json(Path::new(&qpdf), &fixture(name)).unwrap_or_else(|error| panic!("{name}: {error:?}"));
            let value: serde_json::Value = serde_json::from_slice(&json).unwrap();
            assert_eq!(value["version"], 2, "{name}");
            assert!(value["qpdf"].is_array(), "{name}");
        }
    }
}
