use std::ffi::{OsStr, OsString};
use std::fmt;
use std::fs::{File, OpenOptions};
use std::io::Read;
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::Arc;
use std::time::Duration;

use windows::core::PCWSTR;
use windows::Win32::Security::Cryptography::{
    BCryptCloseAlgorithmProvider, BCryptHash, BCryptOpenAlgorithmProvider,
    BCRYPT_ALG_HANDLE, BCRYPT_OPEN_ALGORITHM_PROVIDER_FLAGS, BCRYPT_SHA256_ALGORITHM,
};
use windows::Win32::Storage::FileSystem::FILE_SHARE_READ;

use crate::child_job::{
    self, ChildCommand, ChildSystemError, JobLimits, OutputBounds, TerminalReason, EXIT_CANCELLED,
};

pub(crate) const OCR_INPUT_LIMIT: usize = 16 * 1024 * 1024;
pub(crate) const OCR_STDOUT_LIMIT: usize = 1024 * 1024;
pub(crate) const OCR_STDERR_LIMIT: usize = 64 * 1024;
pub(crate) const OCR_TIMEOUT: Duration = Duration::from_secs(30);
pub(crate) const OCR_MAX_EDGE: usize = 16_384;
pub(crate) const OCR_CHILD_COMMIT_LIMIT: usize = 256 * 1024 * 1024;

const OCR_ASSET_SIZE_LIMIT: u64 = 64 * 1024 * 1024;
const OCR_POLL_INTERVAL: Duration = Duration::from_millis(10);
const OCR_MODEL_BYTES: u64 = 4_113_088;
const OCR_MODEL_SHA256: [u8; 32] = [
    0x7d, 0x43, 0x22, 0xbd, 0x2a, 0x77, 0x49, 0x72, 0x48, 0x79, 0x68, 0x3f, 0xc3,
    0x91, 0x2c, 0xb5, 0x42, 0xf1, 0x99, 0x06, 0xc8, 0x3b, 0xcc, 0x1a, 0x52, 0x13,
    0x25, 0x56, 0x42, 0x71, 0x70, 0xb2,
];
const FILE_ATTRIBUTE_REPARSE_POINT_VALUE: u32 = 0x400;
const OCR_LABEL: &str = "OCR";

static OCR_SLOT: AtomicBool = AtomicBool::new(false);
#[cfg(test)]
static OCR_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
const RUN_STATE_RUNNING: u8 = 0;
const RUN_STATE_CANCELLED: u8 = 1;
const RUN_STATE_COMPLETED: u8 = 2;

#[cfg(test)]
pub(crate) fn test_lock() -> std::sync::MutexGuard<'static, ()> {
    OCR_TEST_LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[derive(Clone)]
pub(crate) struct OcrCancellation(Arc<AtomicU8>);

impl Default for OcrCancellation {
    fn default() -> Self {
        Self(Arc::new(AtomicU8::new(RUN_STATE_RUNNING)))
    }
}

impl OcrCancellation {
    pub(crate) fn cancel(&self) -> bool {
        self.0
            .compare_exchange(
                RUN_STATE_RUNNING,
                RUN_STATE_CANCELLED,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
    }

    pub(crate) fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Acquire) == RUN_STATE_CANCELLED
    }

    pub(crate) fn is_completed(&self) -> bool {
        self.0.load(Ordering::Acquire) == RUN_STATE_COMPLETED
    }

    pub(crate) fn ensure_runnable(&self) -> Result<(), OcrProcessError> {
        match self.0.load(Ordering::Acquire) {
            RUN_STATE_RUNNING => Ok(()),
            RUN_STATE_CANCELLED => Err(OcrProcessError::Cancelled),
            RUN_STATE_COMPLETED => Err(OcrProcessError::System("OCR cancellation token was already used".into())),
            _ => Err(OcrProcessError::System("OCR cancellation state is invalid".into())),
        }
    }

    pub(crate) fn commit(&self) -> Result<(), OcrProcessError> {
        match self.0.compare_exchange(
            RUN_STATE_RUNNING,
            RUN_STATE_COMPLETED,
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            Ok(_) => Ok(()),
            Err(RUN_STATE_CANCELLED) => Err(OcrProcessError::Cancelled),
            Err(RUN_STATE_COMPLETED) => Err(OcrProcessError::System("OCR cancellation token was already completed".into())),
            Err(_) => Err(OcrProcessError::System("OCR cancellation state is invalid".into())),
        }
    }

    fn finish<T>(&self, result: Result<T, OcrProcessError>) -> Result<T, OcrProcessError> {
        match self.0.compare_exchange(
            RUN_STATE_RUNNING,
            RUN_STATE_COMPLETED,
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            Ok(_) => result,
            Err(RUN_STATE_CANCELLED) => Err(OcrProcessError::Cancelled),
            Err(RUN_STATE_COMPLETED) => Err(OcrProcessError::System("OCR cancellation token was already completed".into())),
            Err(_) => Err(OcrProcessError::System("OCR cancellation state is invalid".into())),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct OcrExecutableIdentity {
    pub(crate) bytes: u64,
    pub(crate) sha256: [u8; 32],
}

#[derive(Debug, Eq, PartialEq)]
pub(crate) struct OcrProcessOutput {
    pub(crate) text: String,
    pub(crate) peak_job_commit_bytes: usize,
}

#[derive(Debug, Eq, PartialEq)]
pub(crate) enum OcrProcessError {
    Busy,
    Cancelled,
    TimedOut,
    InvalidAsset(String),
    AssetIntegrity(String),
    InvalidInput(String),
    OutputLimit(&'static str),
    InvalidUtf8(&'static str),
    UnexpectedStderr(String),
    ChildExit { code: u32, stderr: String },
    Document(String),
    System(String),
}

impl fmt::Display for OcrProcessError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Busy => write!(formatter, "OCR is already running"),
            Self::Cancelled => write!(formatter, "OCR was cancelled"),
            Self::TimedOut => write!(formatter, "OCR exceeded its time limit"),
            Self::InvalidAsset(message)
            | Self::AssetIntegrity(message)
            | Self::InvalidInput(message)
            | Self::Document(message)
            | Self::System(message) => formatter.write_str(message),
            Self::OutputLimit(stream) => write!(formatter, "OCR {stream} exceeded its byte limit"),
            Self::InvalidUtf8(stream) => write!(formatter, "OCR {stream} was not valid UTF-8"),
            Self::UnexpectedStderr(_) => write!(formatter, "OCR reported an unexpected internal error"),
            Self::ChildExit { code, .. } => write!(formatter, "OCR process failed with code {code}"),
        }
    }
}

impl std::error::Error for OcrProcessError {}

impl From<ChildSystemError> for OcrProcessError {
    fn from(error: ChildSystemError) -> Self {
        Self::System(error.0)
    }
}

pub(crate) struct OcrProcessRunner {
    engine_path: PathBuf,
    tessdata_dir: PathBuf,
    _engine_lock: File,
    _model_lock: File,
}

impl OcrProcessRunner {
    pub(crate) fn new(
        engine_path: PathBuf,
        tessdata_dir: PathBuf,
        expected_engine: OcrExecutableIdentity,
    ) -> Result<Self, OcrProcessError> {
        Self::new_with_model_identity(
            engine_path,
            tessdata_dir,
            expected_engine,
            OcrExecutableIdentity {
                bytes: OCR_MODEL_BYTES,
                sha256: OCR_MODEL_SHA256,
            },
        )
    }

    fn new_with_model_identity(
        engine_path: PathBuf,
        tessdata_dir: PathBuf,
        expected_engine: OcrExecutableIdentity,
        expected_model: OcrExecutableIdentity,
    ) -> Result<Self, OcrProcessError> {
        let engine_path = validate_asset_path(&engine_path, "tesseract.exe", false)?;
        let tessdata_dir = validate_asset_path(&tessdata_dir, "tessdata", true)?;
        let model_path = validate_asset_path(&tessdata_dir.join("eng.traineddata"), "eng.traineddata", false)?;
        let engine_lock = open_and_verify(&engine_path, expected_engine, "OCR executable")?;
        let model_lock = open_and_verify(&model_path, expected_model, "English OCR model")?;
        Ok(Self { engine_path, tessdata_dir, _engine_lock: engine_lock, _model_lock: model_lock })
    }

    pub(crate) fn recognize_p6(
        &self,
        input: &[u8],
        cancellation: &OcrCancellation,
    ) -> Result<OcrProcessOutput, OcrProcessError> {
        self.recognize_with_options(input, cancellation, RunOptions::production())
    }

    fn recognize_with_options(
        &self,
        input: &[u8],
        cancellation: &OcrCancellation,
        options: RunOptions<'_>,
    ) -> Result<OcrProcessOutput, OcrProcessError> {
        let operation = match OcrOperation::try_begin(cancellation) {
            Ok(operation) => operation,
            Err(error) => return cancellation.finish(Err(error)),
        };
        let result = self.recognize_inner_admitted(input, options, OcrOutputProfile::Text, &operation);
        if let Some(hook) = options.before_finish {
            hook();
        }
        operation.finish(result)
    }

    pub(crate) fn recognize_p6_admitted(
        &self,
        input: &[u8],
        operation: &OcrOperation,
    ) -> Result<OcrProcessOutput, OcrProcessError> {
        self.recognize_inner_admitted(input, RunOptions::production(), OcrOutputProfile::Text, operation)
    }

    pub(crate) fn recognize_p6_tsv_admitted(
        &self,
        input: &[u8],
        operation: &OcrOperation,
    ) -> Result<OcrProcessOutput, OcrProcessError> {
        self.recognize_inner_admitted(input, RunOptions::production(), OcrOutputProfile::Tsv, operation)
    }

    pub(crate) fn recognize_p6_tsv_admitted_with_timeout(
        &self,
        input: &[u8],
        operation: &OcrOperation,
        timeout: Duration,
    ) -> Result<OcrProcessOutput, OcrProcessError> {
        let mut options = RunOptions::production();
        options.timeout = timeout.min(OCR_TIMEOUT);
        self.recognize_inner_admitted(input, options, OcrOutputProfile::Tsv, operation)
    }

    #[cfg(test)]
    pub(crate) fn recognize_p6_admitted_with_hooks(
        &self,
        input: &[u8],
        operation: &OcrOperation,
        before_resume: Option<&dyn Fn()>,
        after_resume: Option<&dyn Fn()>,
    ) -> Result<OcrProcessOutput, OcrProcessError> {
        self.recognize_inner_admitted(
            input,
            RunOptions { before_resume, after_resume, ..RunOptions::production() },
            OcrOutputProfile::Text,
            operation,
        )
    }

    fn recognize_inner_admitted(
        &self,
        input: &[u8],
        options: RunOptions<'_>,
        output_profile: OcrOutputProfile,
        operation: &OcrOperation,
    ) -> Result<OcrProcessOutput, OcrProcessError> {
        let cancellation = &operation.cancellation;
        cancellation.ensure_runnable()?;
        validate_p6(input)?;
        run_child(
            &self.engine_path,
            &self.tessdata_dir,
            input,
            cancellation,
            options,
            output_profile,
        )
    }
}

#[derive(Clone, Copy)]
enum OcrOutputProfile { Text, Tsv }

#[derive(Clone, Copy)]
struct RunOptions<'a> {
    timeout: Duration,
    before_resume: Option<&'a dyn Fn()>,
    after_resume: Option<&'a dyn Fn()>,
    before_finish: Option<&'a dyn Fn()>,
}

impl RunOptions<'_> {
    fn production() -> Self {
        Self { timeout: OCR_TIMEOUT, before_resume: None, after_resume: None, before_finish: None }
    }
}

struct OcrPermit;

impl OcrPermit {
    fn acquire() -> Result<Self, OcrProcessError> {
        OCR_SLOT
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map(|_| Self)
            .map_err(|_| OcrProcessError::Busy)
    }
}

impl Drop for OcrPermit {
    fn drop(&mut self) {
        OCR_SLOT.store(false, Ordering::Release);
    }
}

pub(crate) struct OcrOperation {
    permit: Arc<OcrPermit>,
    cancellation: OcrCancellation,
}

pub(crate) struct OcrWorkerHold {
    _permit: Arc<OcrPermit>,
}

impl OcrOperation {
    pub(crate) fn try_begin(cancellation: &OcrCancellation) -> Result<Self, OcrProcessError> {
        cancellation.ensure_runnable()?;
        let permit = Arc::new(OcrPermit::acquire()?);
        cancellation.ensure_runnable()?;
        Ok(Self { permit, cancellation: cancellation.clone() })
    }

    pub(crate) fn worker_hold(&self) -> OcrWorkerHold { OcrWorkerHold { _permit: self.permit.clone() } }

    pub(crate) fn finish<T>(
        self,
        result: Result<T, OcrProcessError>,
    ) -> Result<T, OcrProcessError> {
        self.cancellation.finish(result)
    }

    pub(crate) fn finish_committed<T>(self, result: Result<T, OcrProcessError>) -> Result<T, OcrProcessError> {
        if self.cancellation.is_completed() { result } else { self.cancellation.finish(result) }
    }
}

fn validate_p6(input: &[u8]) -> Result<(usize, usize), OcrProcessError> {
    if input.len() > OCR_INPUT_LIMIT {
        return Err(OcrProcessError::InvalidInput(format!(
            "OCR image exceeds the {} byte limit",
            OCR_INPUT_LIMIT
        )));
    }
    let mut lines = input.splitn(4, |byte| *byte == b'\n');
    if lines.next() != Some(b"P6".as_slice()) {
        return Err(OcrProcessError::InvalidInput("OCR input must use the exact P6 format".into()));
    }
    let dimensions = lines.next().ok_or_else(|| OcrProcessError::InvalidInput("OCR P6 dimensions are missing".into()))?;
    let max_value = lines.next().ok_or_else(|| OcrProcessError::InvalidInput("OCR P6 maximum value is missing".into()))?;
    let pixels = lines.next().ok_or_else(|| OcrProcessError::InvalidInput("OCR P6 pixel bytes are missing".into()))?;
    if max_value != b"255" {
        return Err(OcrProcessError::InvalidInput("OCR P6 maximum value must be 255".into()));
    }
    let dimension_text = std::str::from_utf8(dimensions)
        .map_err(|_| OcrProcessError::InvalidInput("OCR P6 dimensions are not ASCII".into()))?;
    let mut parts = dimension_text.split(' ');
    let width = parse_dimension(parts.next(), "width")?;
    let height = parse_dimension(parts.next(), "height")?;
    if parts.next().is_some() {
        return Err(OcrProcessError::InvalidInput("OCR P6 dimensions must contain one space".into()));
    }
    if width > OCR_MAX_EDGE || height > OCR_MAX_EDGE {
        return Err(OcrProcessError::InvalidInput(format!(
            "OCR P6 edge exceeds {} pixels",
            OCR_MAX_EDGE
        )));
    }
    let expected = width
        .checked_mul(height)
        .and_then(|count| count.checked_mul(3))
        .ok_or_else(|| OcrProcessError::InvalidInput("OCR P6 dimensions overflow".into()))?;
    if pixels.len() != expected {
        return Err(OcrProcessError::InvalidInput(format!(
            "OCR P6 contains {} pixel bytes; expected {expected}",
            pixels.len()
        )));
    }
    Ok((width, height))
}

fn parse_dimension(value: Option<&str>, name: &str) -> Result<usize, OcrProcessError> {
    let value = value.ok_or_else(|| OcrProcessError::InvalidInput(format!("OCR P6 {name} is missing")))?;
    if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(OcrProcessError::InvalidInput(format!("OCR P6 {name} is invalid")));
    }
    let parsed = value.parse::<usize>().map_err(|_| OcrProcessError::InvalidInput(format!("OCR P6 {name} is invalid")))?;
    if parsed == 0 {
        return Err(OcrProcessError::InvalidInput(format!("OCR P6 {name} must be positive")));
    }
    Ok(parsed)
}

fn validate_asset_path(path: &Path, expected_name: &str, directory: bool) -> Result<PathBuf, OcrProcessError> {
    if !path.is_absolute() {
        return Err(OcrProcessError::InvalidAsset(format!("{expected_name} path must be absolute")));
    }
    if path.components().any(|component| matches!(component, Component::CurDir | Component::ParentDir)) {
        return Err(OcrProcessError::InvalidAsset(format!("{expected_name} path must be normalized")));
    }
    if !directory && !path.file_name().is_some_and(|name| name.to_string_lossy().eq_ignore_ascii_case(expected_name)) {
        return Err(OcrProcessError::InvalidAsset(format!("Expected {expected_name}")));
    }
    assert_no_reparse_ancestors(path, expected_name)?;
    let canonical = path.canonicalize().map_err(|error| OcrProcessError::InvalidAsset(format!("Could not resolve {expected_name}: {error}")))?;
    let canonical = external_windows_path(&canonical)?;
    let metadata = canonical.metadata().map_err(|error| OcrProcessError::InvalidAsset(format!("Could not inspect {expected_name}: {error}")))?;
    if (directory && !metadata.is_dir()) || (!directory && !metadata.is_file()) {
        return Err(OcrProcessError::InvalidAsset(format!("{expected_name} has the wrong file type")));
    }
    Ok(canonical)
}

fn external_windows_path(path: &Path) -> Result<PathBuf, OcrProcessError> {
    let units = path.as_os_str().encode_wide().collect::<Vec<_>>();
    const VERBATIM: &[u16] = &[b'\\' as u16, b'\\' as u16, b'?' as u16, b'\\' as u16];
    const VERBATIM_UNC: &[u16] = &[
        b'\\' as u16,
        b'\\' as u16,
        b'?' as u16,
        b'\\' as u16,
        b'U' as u16,
        b'N' as u16,
        b'C' as u16,
        b'\\' as u16,
    ];
    let normalized = if units.starts_with(VERBATIM_UNC) {
        let mut result = vec![b'\\' as u16, b'\\' as u16];
        result.extend_from_slice(&units[VERBATIM_UNC.len()..]);
        result
    } else if units.starts_with(VERBATIM) {
        let result = units[VERBATIM.len()..].to_vec();
        if result.get(1) != Some(&(b':' as u16)) {
            return Err(OcrProcessError::InvalidAsset("OCR assets use an unsupported Windows device path".into()));
        }
        result
    } else {
        units
    };
    Ok(PathBuf::from(OsString::from_wide(&normalized)))
}

fn assert_no_reparse_ancestors(path: &Path, label: &str) -> Result<(), OcrProcessError> {
    let mut current = PathBuf::new();
    for component in path.components() {
        current.push(component.as_os_str());
        if matches!(component, Component::Prefix(_)) {
            continue;
        }
        match std::fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT_VALUE != 0 => {
                return Err(OcrProcessError::InvalidAsset(format!("{label} path cannot contain a reparse point")))
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => break,
            Err(error) => return Err(OcrProcessError::InvalidAsset(format!("Could not inspect {label} path at {}: {error}", current.display()))),
        }
    }
    Ok(())
}

fn open_and_verify(path: &Path, expected: OcrExecutableIdentity, label: &str) -> Result<File, OcrProcessError> {
    if expected.bytes == 0 || expected.bytes > OCR_ASSET_SIZE_LIMIT {
        return Err(OcrProcessError::InvalidAsset(format!("{label} expected size is invalid")));
    }
    let mut file = OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ.0)
        .open(path)
        .map_err(|error| OcrProcessError::InvalidAsset(format!("Could not lock {label}: {error}")))?;
    let actual_bytes = file.metadata().map_err(|error| OcrProcessError::InvalidAsset(format!("Could not inspect {label}: {error}")))?.len();
    if actual_bytes != expected.bytes {
        return Err(OcrProcessError::AssetIntegrity(format!("{label} size does not match its trusted identity")));
    }
    let capacity = usize::try_from(actual_bytes).map_err(|_| OcrProcessError::InvalidAsset(format!("{label} is too large")))?;
    let mut bytes = Vec::with_capacity(capacity);
    file.read_to_end(&mut bytes).map_err(|error| OcrProcessError::InvalidAsset(format!("Could not read {label}: {error}")))?;
    if bytes.len() != capacity || sha256(&bytes)? != expected.sha256 {
        return Err(OcrProcessError::AssetIntegrity(format!("{label} hash does not match its trusted identity")));
    }
    Ok(file)
}

fn sha256(input: &[u8]) -> Result<[u8; 32], OcrProcessError> {
    let mut algorithm = BCRYPT_ALG_HANDLE::default();
    let open_status = unsafe {
        BCryptOpenAlgorithmProvider(
            &mut algorithm,
            BCRYPT_SHA256_ALGORITHM,
            PCWSTR::null(),
            BCRYPT_OPEN_ALGORITHM_PROVIDER_FLAGS(0),
        )
    };
    if open_status.0 < 0 {
        return Err(OcrProcessError::System(format!("Could not initialize SHA-256: {open_status:?}")));
    }
    let mut output = [0_u8; 32];
    let hash_status = unsafe { BCryptHash(algorithm, None, input, &mut output) };
    let close_status = unsafe { BCryptCloseAlgorithmProvider(algorithm, 0) };
    if hash_status.0 < 0 {
        return Err(OcrProcessError::System(format!("Could not calculate SHA-256: {hash_status:?}")));
    }
    if close_status.0 < 0 {
        return Err(OcrProcessError::System(format!("Could not close SHA-256 provider: {close_status:?}")));
    }
    Ok(output)
}

fn run_child(
    engine: &Path,
    tessdata: &Path,
    input: &[u8],
    cancellation: &OcrCancellation,
    options: RunOptions<'_>,
    output_profile: OcrOutputProfile,
) -> Result<OcrProcessOutput, OcrProcessError> {
    cancellation.ensure_runnable()?;
    let suspended = child_job::spawn_suspended(
        OCR_LABEL,
        &JobLimits::single_process(OCR_CHILD_COMMIT_LIMIT),
        || {
            let application = wide_null(engine.as_os_str())?;
            let current_directory = wide_null(engine.parent().ok_or_else(|| OcrProcessError::InvalidAsset("OCR executable has no parent directory".into()))?.as_os_str())?;
            let command_line = build_command_line(engine, tessdata, output_profile)?;
            Ok(ChildCommand { application, command_line, current_directory })
        },
        || cancellation.ensure_runnable(),
    )?;
    if let Some(hook) = options.before_resume {
        hook();
    }
    if cancellation.is_cancelled() {
        suspended.terminate_and_reap(EXIT_CANCELLED)?;
        return Err(OcrProcessError::Cancelled);
    }
    let running = suspended.resume()?;
    if let Some(hook) = options.after_resume {
        hook();
    }

    let bounds = OutputBounds {
        stdout_limit: OCR_STDOUT_LIMIT,
        stderr_limit: OCR_STDERR_LIMIT,
        timeout: options.timeout,
        poll_interval: OCR_POLL_INTERVAL,
    };
    let run = child_job::run_bounded(running, input, &bounds, &|| cancellation.is_cancelled())?;
    let reason = run.reason;
    if run.stdout_exceeded || reason == TerminalReason::StdoutLimit {
        return Err(OcrProcessError::OutputLimit("stdout"));
    }
    if run.stderr_exceeded || reason == TerminalReason::StderrLimit {
        return Err(OcrProcessError::OutputLimit("stderr"));
    }
    match reason {
        TerminalReason::Cancelled => return Err(OcrProcessError::Cancelled),
        TerminalReason::TimedOut => return Err(OcrProcessError::TimedOut),
        TerminalReason::WaitFailed => return Err(OcrProcessError::System("Could not wait for OCR process".into())),
        TerminalReason::LimitViolation(_) => return Err(OcrProcessError::System("OCR exceeded a job limit".into())),
        TerminalReason::Exited | TerminalReason::StdoutLimit | TerminalReason::StderrLimit => {}
    }
    if cancellation.is_cancelled() {
        return Err(OcrProcessError::Cancelled);
    }
    let stdout = String::from_utf8(run.stdout).map_err(|_| OcrProcessError::InvalidUtf8("stdout"))?;
    let stderr = String::from_utf8(run.stderr).map_err(|_| OcrProcessError::InvalidUtf8("stderr"))?;
    let exit_code = run.child.exit_code()?;
    let peak_job_commit_bytes = run.child.peak_job_commit()?;
    if exit_code != 0 {
        return Err(OcrProcessError::ChildExit { code: exit_code, stderr });
    }
    run.writer_result?;
    if !stderr.is_empty() {
        return Err(OcrProcessError::UnexpectedStderr(stderr));
    }
    if cancellation.is_cancelled() {
        return Err(OcrProcessError::Cancelled);
    }
    Ok(OcrProcessOutput { text: stdout, peak_job_commit_bytes })
}

fn build_command_line(engine: &Path, tessdata: &Path, output_profile: OcrOutputProfile) -> Result<Vec<u16>, OcrProcessError> {
    let mut arguments = vec![
        engine.as_os_str().to_os_string(),
        OsString::from("stdin"),
        OsString::from("stdout"),
        OsString::from("--tessdata-dir"),
        tessdata.as_os_str().to_os_string(),
        OsString::from("-l"),
        OsString::from("eng"),
        OsString::from("--oem"),
        OsString::from("1"),
        OsString::from("--psm"),
        OsString::from("6"),
        OsString::from("--dpi"),
        OsString::from("150"),
        OsString::from("--loglevel"),
        OsString::from("ERROR"),
    ];
    if matches!(output_profile, OcrOutputProfile::Tsv) { arguments.push(OsString::from("tsv")); }
    let arguments = arguments.iter().map(OsString::as_os_str).collect::<Vec<_>>();
    child_job::command_line(&arguments).map_err(|_| null_character())
}

fn wide_null(value: &OsStr) -> Result<Vec<u16>, OcrProcessError> {
    child_job::wide_null(value).map_err(|_| null_character())
}

fn null_character() -> OcrProcessError {
    OcrProcessError::InvalidAsset("OCR path contains a null character".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::child_job::{as_windows_handle, owned_handle};
    use std::process::Command;
    use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};
    use std::sync::OnceLock;
    use windows::Win32::Foundation::WAIT_TIMEOUT;
    use windows::Win32::System::Threading::{OpenProcess, WaitForSingleObject, PROCESS_ACCESS_RIGHTS, PROCESS_QUERY_LIMITED_INFORMATION};

    static CASE_NUMBER: AtomicUsize = AtomicUsize::new(0);
    static FAKE_EXECUTABLE: OnceLock<Result<PathBuf, String>> = OnceLock::new();

    const TEST_P6: &[u8] = b"P6\n1 1\n255\n\xFF\xFF\xFF";
    struct FakeCase {
        runner: OcrProcessRunner,
        directory: PathBuf,
    }

    fn repository_root() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).parent().unwrap().to_path_buf()
    }

    fn fake_source() -> &'static str {
        r#"
use std::fs;
use std::io::{Read, Write};
use std::process::{Command, exit};
use std::thread;
use std::time::Duration;

fn main() {
    let directory = std::env::current_dir().unwrap();
    let name = std::env::current_exe().unwrap().file_name().unwrap().to_string_lossy().to_ascii_lowercase();
    if name == "grandchild.exe" {
        fs::write(directory.join("grandchild.pid"), std::process::id().to_string()).unwrap();
        fs::write(directory.join("grandchild.marker"), b"ran").unwrap();
        thread::sleep(Duration::from_secs(30));
        return;
    }
    fs::write(directory.join("child.pid"), std::process::id().to_string()).unwrap();
    let behavior = fs::read_to_string(directory.join("behavior.txt")).unwrap();
    match behavior.trim() {
        "exact" => {
            let mut input = Vec::new();
            std::io::stdin().read_to_end(&mut input).unwrap();
            assert!(input.starts_with(b"P6\n"));
            print!("recognized text\n");
        }
        "stdout-exact" => std::io::stdout().write_all(&vec![b'a'; 1024 * 1024]).unwrap(),
        "stdout-over" => std::io::stdout().write_all(&vec![b'a'; 1024 * 1024 + 1]).unwrap(),
        "stderr-exact" => {
            std::io::stderr().write_all(&vec![b'e'; 64 * 1024]).unwrap();
            exit(7);
        }
        "stderr-over" => {
            std::io::stderr().write_all(&vec![b'e'; 64 * 1024 + 1]).unwrap();
            exit(7);
        }
        "invalid-stdout" => std::io::stdout().write_all(&[0xff]).unwrap(),
        "invalid-stderr" => {
            std::io::stderr().write_all(&[0xff]).unwrap();
            exit(7);
        }
        "nonzero" => {
            eprintln!("bounded fake failure");
            exit(7);
        }
        "unexpected-stderr" => {
            print!("recognized text\n");
            eprintln!("unexpected warning");
        }
        "sleep" => thread::sleep(Duration::from_secs(30)),
        "spawn-grandchild" => {
            match Command::new(directory.join("grandchild.exe")).spawn() {
                Ok(mut child) => {
                    let status = child.wait().unwrap();
                    fs::write(directory.join("spawn-result.txt"), format!("started:{status}")).unwrap();
                }
                Err(error) => fs::write(directory.join("spawn-result.txt"), format!("blocked:{error}")).unwrap(),
            }
            print!("recognized text\n");
        }
        "memory-hog" => {
            let mut memory = Vec::<u8>::new();
            if memory.try_reserve_exact(600 * 1024 * 1024).is_err() { exit(77); }
            unsafe { memory.set_len(600 * 1024 * 1024); }
            for index in (0..memory.len()).step_by(4096) { memory[index] = 1; }
            print!("unexpected allocation success {}\n", memory[0]);
        }
        other => panic!("unknown behavior {other}"),
    }
}
"#
    }

    fn fake_executable() -> Result<PathBuf, String> {
        FAKE_EXECUTABLE
            .get_or_init(|| {
                let directory = repository_root().join("target/ocr-process-tests/fake-build");
                std::fs::create_dir_all(&directory).map_err(|error| error.to_string())?;
                let source = directory.join("fake-child.rs");
                let executable = directory.join("fake-child.exe");
                std::fs::write(&source, fake_source()).map_err(|error| error.to_string())?;
                let output = Command::new("rustc")
                    .arg("--edition=2021")
                    .arg("-O")
                    .arg(&source)
                    .arg("-o")
                    .arg(&executable)
                    .output()
                    .map_err(|error| format!("could not start rustc: {error}"))?;
                if !output.status.success() {
                    return Err(format!("fake OCR child did not compile: {}", String::from_utf8_lossy(&output.stderr)));
                }
                Ok(executable)
            })
            .clone()
    }

    fn fake_case(behavior: &str) -> FakeCase {
        let number = CASE_NUMBER.fetch_add(1, AtomicOrdering::Relaxed);
        let directory = repository_root().join("target/ocr-process-tests/cases").join(format!("{}-{}-{number}", behavior, std::process::id()));
        let tessdata = directory.join("tessdata");
        std::fs::create_dir_all(&tessdata).unwrap();
        let engine = directory.join("tesseract.exe");
        std::fs::copy(fake_executable().unwrap(), &engine).unwrap();
        std::fs::copy(&engine, directory.join("grandchild.exe")).unwrap();
        std::fs::write(directory.join("behavior.txt"), behavior).unwrap();
        let model = tessdata.join("eng.traineddata");
        std::fs::write(&model, b"test-only model").unwrap();
        let engine_bytes = std::fs::read(&engine).unwrap();
        let model_bytes = std::fs::read(&model).unwrap();
        let runner = OcrProcessRunner::new_with_model_identity(
            engine,
            tessdata,
            OcrExecutableIdentity { bytes: engine_bytes.len() as u64, sha256: sha256(&engine_bytes).unwrap() },
            OcrExecutableIdentity { bytes: model_bytes.len() as u64, sha256: sha256(&model_bytes).unwrap() },
        )
        .unwrap();
        FakeCase { runner, directory }
    }

    fn wait_for_pid(directory: &Path) -> u32 {
        let path = directory.join("child.pid");
        for _ in 0..300 {
            if let Ok(text) = std::fs::read_to_string(&path) {
                return text.parse().unwrap();
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("fake OCR child did not publish its PID");
    }

    fn process_is_alive(pid: u32) -> bool {
        let access = PROCESS_ACCESS_RIGHTS(PROCESS_QUERY_LIMITED_INFORMATION.0 | 0x0010_0000);
        let process = unsafe { OpenProcess(access, false, pid) };
        let Ok(process) = process else { return false; };
        let handle = unsafe { owned_handle(process) };
        (unsafe { WaitForSingleObject(as_windows_handle(&handle), 0) }) == WAIT_TIMEOUT
    }

    fn assert_process_reaped(pid: u32) {
        for _ in 0..100 {
            if !process_is_alive(pid) {
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("fake OCR child {pid} remains alive");
    }

    #[test]
    fn strict_p6_validation_enforces_dimensions_edges_and_byte_count() {
        assert_eq!(validate_p6(b"P6\n2 1\n255\n\0\0\0\xFF\xFF\xFF"), Ok((2, 1)));
        for invalid in [
            b"P3\n2 1\n255\n\0\0\0\xFF\xFF\xFF".as_slice(),
            b"P6\n0 1\n255\n".as_slice(),
            b"P6\n2 1\n254\n\0\0\0\xFF\xFF\xFF".as_slice(),
            b"P6\n2 1\n255\n\0\0\0".as_slice(),
            b"P6\n16385 1\n255\n".as_slice(),
        ] {
            assert!(matches!(validate_p6(invalid), Err(OcrProcessError::InvalidInput(_))));
        }
    }

    #[test]
    fn command_line_quotes_paths_without_using_a_shell() {
        let command = build_command_line(Path::new(r"C:\OCR engine\tesseract.exe"), Path::new(r"C:\model space\tessdata"), OcrOutputProfile::Text).unwrap();
        let text = String::from_utf16(&command[..command.len() - 1]).unwrap();
        assert_eq!(text, r#""C:\OCR engine\tesseract.exe" "stdin" "stdout" "--tessdata-dir" "C:\model space\tessdata" "-l" "eng" "--oem" "1" "--psm" "6" "--dpi" "150" "--loglevel" "ERROR""#);
        let tsv = build_command_line(Path::new(r"C:\OCR engine\tesseract.exe"), Path::new(r"C:\model space\tessdata"), OcrOutputProfile::Tsv).unwrap();
        let tsv = String::from_utf16(&tsv[..tsv.len() - 1]).unwrap();
        assert_eq!(tsv, format!("{text} \"tsv\""));
    }

    #[test]
    fn canonical_windows_paths_are_presented_without_unsupported_device_syntax() {
        assert_eq!(external_windows_path(Path::new(r"\\?\C:\OCR\tesseract.exe")).unwrap(), PathBuf::from(r"C:\OCR\tesseract.exe"));
        assert_eq!(external_windows_path(Path::new(r"\\?\UNC\server\share\tessdata")).unwrap(), PathBuf::from(r"\\server\share\tessdata"));
        assert!(matches!(external_windows_path(Path::new(r"\\?\Volume{1234}\tesseract.exe")), Err(OcrProcessError::InvalidAsset(_))));
    }

    #[test]
    fn cancellation_before_spawn_and_while_suspended_runs_no_child_code() {
        let _serial = test_lock();
        let pre_cancelled = fake_case("exact");
        let cancellation = OcrCancellation::default();
        assert!(cancellation.cancel());
        assert_eq!(pre_cancelled.runner.recognize_p6(TEST_P6, &cancellation), Err(OcrProcessError::Cancelled));
        assert!(!pre_cancelled.directory.join("child.pid").exists());

        let suspended = fake_case("exact");
        let cancellation = OcrCancellation::default();
        let hook_token = cancellation.clone();
        let hook = || assert!(hook_token.cancel());
        let result = suspended.runner.recognize_with_options(
            TEST_P6,
            &cancellation,
            RunOptions { timeout: Duration::from_secs(2), before_resume: Some(&hook), after_resume: None, before_finish: None },
        );
        assert_eq!(result, Err(OcrProcessError::Cancelled));
        assert!(!suspended.directory.join("child.pid").exists());
        assert!(!cancellation.cancel());
    }

    #[test]
    fn in_flight_cancel_reaps_child_and_completed_cancel_is_neutral() {
        let _serial = test_lock();
        let sleeping = fake_case("sleep");
        let directory = sleeping.directory.clone();
        let cancellation = OcrCancellation::default();
        let worker_token = cancellation.clone();
        let worker = std::thread::spawn(move || sleeping.runner.recognize_p6(TEST_P6, &worker_token));
        let pid = wait_for_pid(&directory);
        assert!(cancellation.cancel());
        assert_eq!(worker.join().unwrap(), Err(OcrProcessError::Cancelled));
        assert_process_reaped(pid);

        let completed = fake_case("exact");
        let cancellation = OcrCancellation::default();
        let result = completed.runner.recognize_p6(TEST_P6, &cancellation).unwrap();
        assert_eq!(result.text, "recognized text\n");
        assert!(!cancellation.cancel());
    }

    #[test]
    fn cancellation_wins_at_the_finish_barrier_then_loses_after_completion() {
        let _serial = test_lock();
        let cancelled = fake_case("exact");
        let cancellation = OcrCancellation::default();
        let worker_token = cancellation.clone();
        let (ready_sender, ready_receiver) = std::sync::mpsc::sync_channel(0);
        let (release_sender, release_receiver) = std::sync::mpsc::sync_channel(0);
        let worker = std::thread::spawn(move || {
            let before_finish = || {
                ready_sender.send(()).unwrap();
                release_receiver.recv().unwrap();
            };
            cancelled.runner.recognize_with_options(
                TEST_P6,
                &worker_token,
                RunOptions {
                    timeout: Duration::from_secs(2),
                    before_resume: None,
                    after_resume: None,
                    before_finish: Some(&before_finish),
                },
            )
        });
        ready_receiver.recv().unwrap();
        assert!(cancellation.cancel());
        release_sender.send(()).unwrap();
        let result = worker.join().unwrap();
        assert_eq!(result, Err(OcrProcessError::Cancelled));
        assert!(!cancellation.cancel());

        let completed = fake_case("exact");
        let cancellation = OcrCancellation::default();
        assert!(completed.runner.recognize_p6(TEST_P6, &cancellation).is_ok());
        assert!(!cancellation.cancel());
    }

    #[test]
    fn global_busy_gate_never_spawns_second_child_and_releases_after_cancel() {
        let _serial = test_lock();
        let sleeping = fake_case("sleep");
        let sleeping_directory = sleeping.directory.clone();
        let cancellation = OcrCancellation::default();
        let worker_token = cancellation.clone();
        let worker = std::thread::spawn(move || sleeping.runner.recognize_p6(TEST_P6, &worker_token));
        let pid = wait_for_pid(&sleeping_directory);

        let blocked = fake_case("exact");
        let blocked_token = OcrCancellation::default();
        assert_eq!(blocked.runner.recognize_p6(TEST_P6, &blocked_token), Err(OcrProcessError::Busy));
        assert!(!blocked.directory.join("child.pid").exists());
        assert!(!blocked_token.cancel());
        assert!(cancellation.cancel());
        assert_eq!(worker.join().unwrap(), Err(OcrProcessError::Cancelled));
        assert_process_reaped(pid);

        let released = fake_case("exact");
        assert_eq!(released.runner.recognize_p6(TEST_P6, &OcrCancellation::default()).unwrap().text, "recognized text\n");
    }

    #[test]
    fn stdout_and_stderr_caps_are_exact_and_fast_exit_cannot_bypass_them() {
        let _serial = test_lock();
        let exact_stdout = fake_case("stdout-exact");
        let output = exact_stdout.runner.recognize_p6(TEST_P6, &OcrCancellation::default()).unwrap();
        assert_eq!(output.text.len(), OCR_STDOUT_LIMIT);

        let over_stdout = fake_case("stdout-over");
        let directory = over_stdout.directory.clone();
        assert_eq!(over_stdout.runner.recognize_p6(TEST_P6, &OcrCancellation::default()), Err(OcrProcessError::OutputLimit("stdout")));
        assert_process_reaped(wait_for_pid(&directory));

        let exact_stderr = fake_case("stderr-exact");
        let error = exact_stderr.runner.recognize_p6(TEST_P6, &OcrCancellation::default()).unwrap_err();
        assert!(matches!(error, OcrProcessError::ChildExit { code: 7, ref stderr } if stderr.len() == OCR_STDERR_LIMIT));

        let over_stderr = fake_case("stderr-over");
        let directory = over_stderr.directory.clone();
        assert_eq!(over_stderr.runner.recognize_p6(TEST_P6, &OcrCancellation::default()), Err(OcrProcessError::OutputLimit("stderr")));
        assert_process_reaped(wait_for_pid(&directory));
    }

    #[test]
    fn strict_utf8_exit_and_successful_stderr_fail_closed() {
        let _serial = test_lock();
        for (behavior, expected) in [
            ("invalid-stdout", OcrProcessError::InvalidUtf8("stdout")),
            ("invalid-stderr", OcrProcessError::InvalidUtf8("stderr")),
        ] {
            assert_eq!(fake_case(behavior).runner.recognize_p6(TEST_P6, &OcrCancellation::default()), Err(expected));
        }
        let nonzero = fake_case("nonzero");
        assert!(matches!(nonzero.runner.recognize_p6(TEST_P6, &OcrCancellation::default()), Err(OcrProcessError::ChildExit { code: 7, ref stderr }) if stderr == "bounded fake failure\n"));
        let width = 1024_usize;
        let height = 4096_usize;
        let header = format!("P6\n{width} {height}\n255\n");
        let mut large_input = header.as_bytes().to_vec();
        large_input.resize(header.len() + width * height * 3, 255);
        let early_nonzero = fake_case("nonzero");
        assert!(matches!(early_nonzero.runner.recognize_p6(&large_input, &OcrCancellation::default()), Err(OcrProcessError::ChildExit { code: 7, ref stderr }) if stderr == "bounded fake failure\n"));
        let warning = fake_case("unexpected-stderr");
        assert!(matches!(warning.runner.recognize_p6(TEST_P6, &OcrCancellation::default()), Err(OcrProcessError::UnexpectedStderr(ref stderr)) if stderr == "unexpected warning\n"));
    }

    #[test]
    fn timeout_and_unwind_drop_kill_and_reap_the_owned_child() {
        let _serial = test_lock();
        let timed = fake_case("sleep");
        let timed_directory = timed.directory.clone();
        let result = timed.runner.recognize_with_options(
            TEST_P6,
            &OcrCancellation::default(),
            RunOptions { timeout: Duration::from_millis(200), before_resume: None, after_resume: None, before_finish: None },
        );
        assert_eq!(result, Err(OcrProcessError::TimedOut));
        assert_process_reaped(wait_for_pid(&timed_directory));

        let unwound = fake_case("sleep");
        let directory = unwound.directory.clone();
        let after_resume = || {
            let pid = wait_for_pid(&directory);
            assert!(process_is_alive(pid));
            panic!("test-only unwind");
        };
        let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = unwound.runner.recognize_with_options(
                TEST_P6,
                &OcrCancellation::default(),
                RunOptions { timeout: Duration::from_secs(2), before_resume: None, after_resume: Some(&after_resume), before_finish: None },
            );
        }));
        assert!(caught.is_err());
        assert_process_reaped(wait_for_pid(&directory));

        let recovered = fake_case("exact");
        assert_eq!(recovered.runner.recognize_p6(TEST_P6, &OcrCancellation::default()).unwrap().text, "recognized text\n");
    }

    #[test]
    fn abrupt_host_exit_helper_closes_the_job() {
        if std::env::var_os("OCR_PROCESS_HOST_EXIT_HELPER").is_none() {
            return;
        }
        let sleeping = fake_case("sleep");
        let directory = sleeping.directory.clone();
        let cancellation = OcrCancellation::default();
        std::thread::spawn(move || {
            let _ = sleeping.runner.recognize_p6(TEST_P6, &cancellation);
        });
        let pid = wait_for_pid(&directory);
        let pid_file = PathBuf::from(std::env::var_os("OCR_PROCESS_HOST_PID_FILE").unwrap());
        std::fs::write(pid_file, pid.to_string()).unwrap();
        std::process::exit(0);
    }

    #[test]
    fn abrupt_host_exit_kills_the_job_child() {
        let _serial = test_lock();
        let number = CASE_NUMBER.fetch_add(1, AtomicOrdering::Relaxed);
        let directory = repository_root().join("target/ocr-process-tests/host-exit").join(format!("{}-{number}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        let pid_file = directory.join("child.pid");
        let status = Command::new(std::env::current_exe().unwrap())
            .arg("--exact")
            .arg("ocr_process::tests::abrupt_host_exit_helper_closes_the_job")
            .arg("--nocapture")
            .env("OCR_PROCESS_HOST_EXIT_HELPER", "1")
            .env("OCR_PROCESS_HOST_PID_FILE", &pid_file)
            .status()
            .unwrap();
        assert!(status.success());
        let pid = std::fs::read_to_string(&pid_file).unwrap().parse().unwrap();
        assert_process_reaped(pid);
    }

    #[test]
    fn active_process_and_commit_limits_fail_closed() {
        let _serial = test_lock();
        let spawning = fake_case("spawn-grandchild");
        let output = spawning.runner.recognize_p6(TEST_P6, &OcrCancellation::default()).unwrap();
        assert_eq!(output.text, "recognized text\n");
        assert!(spawning.directory.join("spawn-result.txt").exists());
        assert!(!spawning.directory.join("grandchild.marker").exists());

        let memory = fake_case("memory-hog");
        let error = memory.runner.recognize_p6(TEST_P6, &OcrCancellation::default()).unwrap_err();
        assert_eq!(error, OcrProcessError::ChildExit { code: 77, stderr: String::new() });
    }

    #[test]
    fn input_and_asset_integrity_fail_before_process_creation() {
        let _serial = test_lock();
        let invalid = fake_case("exact");
        let mut oversize = vec![0_u8; OCR_INPUT_LIMIT + 1];
        oversize[..2].copy_from_slice(b"P6");
        assert!(matches!(invalid.runner.recognize_p6(&oversize, &OcrCancellation::default()), Err(OcrProcessError::InvalidInput(_))));
        assert!(!invalid.directory.join("child.pid").exists());

        let case = fake_case("exact");
        let engine_path = case.directory.join("tesseract.exe");
        let model_path = case.directory.join("tessdata/eng.traineddata");
        let engine_bytes = std::fs::read(&engine_path).unwrap();
        let model_bytes = std::fs::read(&model_path).unwrap();
        let error = match OcrProcessRunner::new_with_model_identity(
            engine_path.clone(),
            case.directory.join("tessdata"),
            OcrExecutableIdentity { bytes: engine_bytes.len() as u64, sha256: [0; 32] },
            OcrExecutableIdentity { bytes: model_bytes.len() as u64, sha256: sha256(&model_bytes).unwrap() },
        ) {
            Ok(_) => panic!("invalid identity was accepted"),
            Err(error) => error,
        };
        assert!(matches!(error, OcrProcessError::AssetIntegrity(_)));
        let error = match OcrProcessRunner::new_with_model_identity(
            engine_path.clone(),
            case.directory.join("tessdata"),
            OcrExecutableIdentity { bytes: engine_bytes.len() as u64, sha256: sha256(&engine_bytes).unwrap() },
            OcrExecutableIdentity { bytes: model_bytes.len() as u64, sha256: [0; 32] },
        ) {
            Ok(_) => panic!("invalid model identity was accepted"),
            Err(error) => error,
        };
        assert!(matches!(error, OcrProcessError::AssetIntegrity(_)));
        assert!(OpenOptions::new().write(true).open(&engine_path).is_err());
        assert!(std::fs::rename(&model_path, case.directory.join("tessdata/replaced.traineddata")).is_err());
        assert!(!case.directory.join("child.pid").exists());
    }

    #[test]
    fn wrong_declared_size_is_refused_for_engine_and_model_before_process_creation() {
        let _serial = test_lock();
        let case = fake_case("exact");
        let engine_path = case.directory.join("tesseract.exe");
        let engine_bytes = std::fs::read(&engine_path).unwrap();
        let model_bytes = std::fs::read(case.directory.join("tessdata/eng.traineddata")).unwrap();
        let engine = OcrExecutableIdentity { bytes: engine_bytes.len() as u64, sha256: sha256(&engine_bytes).unwrap() };
        let model = OcrExecutableIdentity { bytes: model_bytes.len() as u64, sha256: sha256(&model_bytes).unwrap() };
        let wrong_engine = OcrExecutableIdentity { bytes: engine.bytes + 1, ..engine };
        let wrong_model = OcrExecutableIdentity { bytes: model.bytes + 1, ..model };
        for (engine, model) in [(wrong_engine, model), (engine, wrong_model)] {
            let error = match OcrProcessRunner::new_with_model_identity(engine_path.clone(), case.directory.join("tessdata"), engine, model) {
                Ok(_) => panic!("a wrong declared size was accepted"),
                Err(error) => error,
            };
            assert!(matches!(&error, OcrProcessError::AssetIntegrity(message) if message.contains("size does not match")), "{error:?}");
        }
        assert!(!case.directory.join("child.pid").exists());
    }

    fn verified_recipe_root() -> PathBuf {
        crate::ocr_test_support::local_setup().expect("the test must call require_local_setup first").root.clone()
    }

    fn verified_runner() -> OcrProcessRunner {
        let setup = crate::ocr_test_support::local_setup().expect("the test must call require_local_setup first");
        OcrProcessRunner::new(
            setup.root.join("engine/bin/tesseract.exe"),
            setup.root.join("engine/tessdata"),
            setup.engine,
        )
        .unwrap()
    }

    #[test]
    fn a_junction_ancestor_is_rejected_as_an_asset_ancestor() {
        let folder = tempfile::tempdir().unwrap();
        let actual = folder.path().join("actual");
        std::fs::create_dir_all(actual.join("engine/bin")).unwrap();
        std::fs::write(actual.join("engine/bin/tesseract.exe"), b"stand-in").unwrap();
        assert!(assert_no_reparse_ancestors(&actual.join("engine/bin/tesseract.exe"), "OCR executable").is_ok());
        let link = folder.path().join("link");
        let output = Command::new("cmd.exe").args(["/d", "/c", "mklink", "/J"]).arg(&link).arg(&actual).output().unwrap();
        assert!(output.status.success(), "mklink failed: {}", String::from_utf8_lossy(&output.stderr));
        let result = assert_no_reparse_ancestors(&link.join("engine/bin/tesseract.exe"), "OCR executable");
        std::fs::remove_dir(&link).unwrap();
        assert!(matches!(result, Err(OcrProcessError::InvalidAsset(_))));
    }

    #[test]
    #[ignore = "needs PDF_WORKSTATION_OCR_SETUP_ROOT pointing at a scripts/setup-ocr.ps1 output root"]
    fn verified_engine_recognizes_the_owned_known_text_fixture() {
        if crate::ocr_test_support::require_local_setup("verified_engine_recognizes_the_owned_known_text_fixture").is_none() { return; }
        let _serial = test_lock();
        let input = std::fs::read(verified_recipe_root().join("smoke/known-text.pnm")).unwrap();
        let output = verified_runner().recognize_p6(&input, &OcrCancellation::default()).unwrap();
        assert_eq!(output.text.trim_end(), "Receipt #A-17: Coffee & Tea, $12.50.\r\nMixed case: 3rd Avenue; ready.");
        assert!(output.peak_job_commit_bytes < OCR_CHILD_COMMIT_LIMIT);
    }

    #[test]
    #[ignore = "needs PDF_WORKSTATION_OCR_SETUP_ROOT pointing at a scripts/setup-ocr.ps1 output root"]
    fn verified_engine_accepts_a_deterministic_near_input_cap_page() {
        if crate::ocr_test_support::require_local_setup("verified_engine_accepts_a_deterministic_near_input_cap_page").is_none() { return; }
        let _serial = test_lock();
        let source = std::fs::read(verified_recipe_root().join("smoke/known-text.pnm")).unwrap();
        let (_, source_height) = validate_p6(&source).unwrap();
        let source_pixels = source.splitn(4, |byte| *byte == b'\n').nth(3).unwrap();
        let source_width = source_pixels.len() / source_height / 3;
        let width = 2048_usize;
        let height = 2729_usize;
        let header = format!("P6\n{width} {height}\n255\n");
        let total_bytes = header.len() + width * height * 3;
        let mut input = Vec::with_capacity(total_bytes);
        input.extend_from_slice(header.as_bytes());
        input.resize(total_bytes, 255);
        let pixel_offset = header.len();
        for tile_y in (0..height.saturating_sub(source_height)).step_by(source_height + 32) {
            for row in 0..source_height {
                let source_start = row * source_width * 3;
                let target_start = pixel_offset + ((tile_y + row) * width * 3);
                input[target_start..target_start + source_width * 3]
                    .copy_from_slice(&source_pixels[source_start..source_start + source_width * 3]);
            }
        }
        assert!(input.len() <= OCR_INPUT_LIMIT);
        let output = verified_runner().recognize_p6(&input, &OcrCancellation::default()).unwrap();
        assert!(output.text.contains("Receipt #A-17"));
        assert!(output.peak_job_commit_bytes < OCR_CHILD_COMMIT_LIMIT);
        eprintln!("near-cap OCR child peak committed bytes: {}", output.peak_job_commit_bytes);
    }
}
