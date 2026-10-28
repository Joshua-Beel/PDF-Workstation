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
    BCryptCloseAlgorithmProvider, BCryptGenRandom, BCryptHash, BCryptOpenAlgorithmProvider,
    BCRYPT_ALG_HANDLE, BCRYPT_OPEN_ALGORITHM_PROVIDER_FLAGS, BCRYPT_SHA256_ALGORITHM,
    BCRYPT_USE_SYSTEM_PREFERRED_RNG,
};
use windows::Win32::Storage::FileSystem::FILE_SHARE_READ;

use crate::child_job::{
    self, ChildCommand, ChildSystemError, JobLimits, OutputBounds, TerminalReason, EXIT_CANCELLED,
};
use crate::ocr_preprocess::{preprocess_p6, PreprocessOptions, Skew};

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
    let guard = OCR_TEST_LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let deadline = std::time::Instant::now() + Duration::from_secs(120);
    while OCR_SLOT.load(Ordering::Acquire) && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(1));
    }
    assert!(!OCR_SLOT.load(Ordering::Acquire), "the OCR slot stayed held after the previous test released the test lock");
    guard
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

#[derive(Debug, PartialEq)]
pub(crate) struct OcrProcessOutput {
    pub(crate) text: String,
    pub(crate) peak_job_commit_bytes: usize,
    /// The rotation applied to the recognition image; TSV boxes are in that
    /// image and go back to page pixels with `ocr_preprocess::map_words_to_page`.
    pub(crate) skew: Skew,
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

    /// For large-document jobs: TSV output, the engine process at below-normal
    /// priority so the app and the rest of the PC stay responsive.
    pub(crate) fn recognize_p6_tsv_background(
        &self,
        input: &[u8],
        operation: &OcrOperation,
    ) -> Result<OcrProcessOutput, OcrProcessError> {
        let mut options = RunOptions::production();
        options.background = true;
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

    /// Text or TSV output with a chosen clean-up, for accuracy comparisons.
    #[cfg(test)]
    pub(crate) fn recognize_p6_with_preprocess(
        &self,
        input: &[u8],
        preprocess: PreprocessOptions,
        tsv: bool,
    ) -> Result<OcrProcessOutput, OcrProcessError> {
        let cancellation = OcrCancellation::default();
        let operation = OcrOperation::try_begin(&cancellation)?;
        let profile = if tsv { OcrOutputProfile::Tsv } else { OcrOutputProfile::Text };
        let result = self.recognize_inner_admitted(input, RunOptions { preprocess, ..RunOptions::production() }, profile, &operation);
        operation.finish(result)
    }

    fn recognize_inner_admitted(
        &self,
        input: &[u8],
        mut options: RunOptions<'_>,
        output_profile: OcrOutputProfile,
        operation: &OcrOperation,
    ) -> Result<OcrProcessOutput, OcrProcessError> {
        let cancellation = &operation.cancellation;
        cancellation.ensure_runnable()?;
        validate_p6(input)?;
        let started = std::time::Instant::now();
        // Clean-up is an improvement, not a requirement: if it cannot run (for
        // example memory is short), the page is recognised as rendered.
        let (recognition, skew) = match preprocess_p6(input, options.preprocess, &|| cancellation.is_cancelled()) {
            Ok(prepared) => (std::borrow::Cow::Owned(prepared.p6), prepared.skew),
            Err(_) if cancellation.is_cancelled() => return Err(OcrProcessError::Cancelled),
            Err(_) => (std::borrow::Cow::Borrowed(input), Skew::NONE),
        };
        cancellation.ensure_runnable()?;
        options.timeout = options.timeout.checked_sub(started.elapsed()).filter(|left| !left.is_zero())
            .ok_or(OcrProcessError::TimedOut)?;
        let mut output = run_child(
            &self.engine_path,
            &self.tessdata_dir,
            &recognition,
            cancellation,
            options,
            output_profile,
        )?;
        output.skew = skew;
        Ok(output)
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
    background: bool,
    preprocess: PreprocessOptions,
}

impl RunOptions<'_> {
    fn production() -> Self {
        Self { timeout: OCR_TIMEOUT, before_resume: None, after_resume: None, before_finish: None, background: false, preprocess: PreprocessOptions::PRODUCTION }
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

const JOB_DIRECTORY_PREFIX: &str = "pdf-workstation-ocr-";
const JOB_DIRECTORY_STALE_AFTER: Duration = Duration::from_secs(24 * 60 * 60);
const JOB_DIRECTORY_ATTEMPTS: usize = 5;

struct JobDirectory(PathBuf);

fn random_directory_name() -> Result<String, OcrProcessError> {
    let mut random = [0_u8; 16];
    let status = unsafe { BCryptGenRandom(None, &mut random, BCRYPT_USE_SYSTEM_PREFERRED_RNG) };
    if status.0 < 0 {
        return Err(OcrProcessError::System(format!("Could not generate an OCR work directory name: {status:?}")));
    }
    Ok(format!("{JOB_DIRECTORY_PREFIX}{}", random.iter().map(|byte| format!("{byte:02x}")).collect::<String>()))
}

fn is_job_directory_name(name: &str) -> bool {
    name.strip_prefix(JOB_DIRECTORY_PREFIX)
        .is_some_and(|rest| rest.len() == 32 && rest.bytes().all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)))
}

fn sweep_stale_job_directories(root: &Path) {
    let Ok(entries) = std::fs::read_dir(root) else { return };
    for entry in entries.flatten() {
        let name = entry.file_name();
        if !name.to_str().is_some_and(is_job_directory_name) {
            continue;
        }
        let Ok(metadata) = std::fs::symlink_metadata(entry.path()) else { continue };
        if !metadata.is_dir() || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT_VALUE != 0 {
            continue;
        }
        let stale = metadata.modified().ok().and_then(|modified| modified.elapsed().ok()).is_some_and(|age| age > JOB_DIRECTORY_STALE_AFTER);
        if stale {
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
}

impl JobDirectory {
    fn create() -> Result<Self, OcrProcessError> {
        let root = std::env::temp_dir();
        static SWEEP: std::sync::Once = std::sync::Once::new();
        SWEEP.call_once(|| sweep_stale_job_directories(&root));
        let mut last_error = None;
        for _ in 0..JOB_DIRECTORY_ATTEMPTS {
            let path = root.join(random_directory_name()?);
            match std::fs::create_dir(&path) {
                Ok(()) => return Ok(Self(path)),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => last_error = Some(error),
                Err(error) => return Err(OcrProcessError::System(format!("Could not create the OCR work directory: {error}"))),
            }
        }
        Err(OcrProcessError::System(format!("Could not create the OCR work directory: {}", last_error.map(|error| error.to_string()).unwrap_or_default())))
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for JobDirectory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
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
    let job_directory = JobDirectory::create()?;
    let suspended = child_job::spawn_suspended(
        OCR_LABEL,
        &JobLimits::single_process(OCR_CHILD_COMMIT_LIMIT),
        || {
            let application = wide_null(engine.as_os_str())?;
            let current_directory = wide_null(job_directory.path().as_os_str())?;
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
    if options.background {
        lower_priority(running.child.process_id());
    }
    if let Some(hook) = options.after_resume {
        hook();
    }

    let bounds = OutputBounds {
        stdout_limit: OCR_STDOUT_LIMIT,
        stderr_limit: OCR_STDERR_LIMIT,
        timeout: options.timeout,
        poll_interval: OCR_POLL_INTERVAL,
        drain_grace: child_job::PIPE_DRAIN_GRACE,
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
    if run.io_cancelled {
        return Err(OcrProcessError::System("OCR output pipes did not close".into()));
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
    let mut text = stdout;
    match classify_stderr(&stderr) {
        Some(StderrVerdict::Clean) => {}
        Some(StderrVerdict::PageSkipped) => {
            // `input` is the image the engine saw: with a deskew it is the
            // padded canvas, the size the TSV is parsed at (`Skew::recognition_size`).
            let (width, height) = validate_p6(input)?;
            text = match output_profile {
                OcrOutputProfile::Text => String::new(),
                OcrOutputProfile::Tsv => blank_page_tsv(width, height),
            };
        }
        None => return Err(OcrProcessError::UnexpectedStderr(stderr)),
    }
    if cancellation.is_cancelled() {
        return Err(OcrProcessError::Cancelled);
    }
    Ok(OcrProcessOutput { text, peak_job_commit_bytes, skew: Skew::NONE })
}

/// Lines the engine writes to stderr with `--psm 3` on ordinary scans even at
/// `--loglevel ERROR` (seen on real pages, 2026-10-28). Each must match a
/// whole line exactly; `#` stands for 1 to 9 ASCII digits and nothing else.
/// Any other line, a line over 200 bytes or more than `OCR_STDERR_MAX_LINES`
/// lines still fails the page, as does a non-zero exit.
const BENIGN_STDERR_LINES: [&str; 4] = [
    "Detected # diacritics",
    "Warning:guessing pitch as xheight on row #, block #",
    "Error in boxClipToRectangle: box outside rectangle",
    "Error in pixScanForForeground: invalid box",
];
/// The engine found too little text to lay out: the page has no text.
const SKIPPED_PAGE_LINE: &str = "Too few characters. Skipping this page";
const OCR_STDERR_MAX_LINES: usize = 64;

#[derive(Debug, PartialEq)]
enum StderrVerdict { Clean, PageSkipped }

fn classify_stderr(stderr: &str) -> Option<StderrVerdict> {
    if stderr.is_empty() {
        return Some(StderrVerdict::Clean);
    }
    let body = stderr.strip_suffix('\n')?;
    let mut verdict = StderrVerdict::Clean;
    for (index, line) in body.split('\n').enumerate() {
        let line = line.strip_suffix('\r').unwrap_or(line);
        if index >= OCR_STDERR_MAX_LINES || line.len() > 200 {
            return None;
        }
        if line == SKIPPED_PAGE_LINE {
            verdict = StderrVerdict::PageSkipped;
        } else if !BENIGN_STDERR_LINES.iter().any(|template| matches_template(line, template)) {
            return None;
        }
    }
    Some(verdict)
}

fn matches_template(line: &str, template: &str) -> bool {
    let (mut line, mut template) = (line.as_bytes(), template.as_bytes());
    while let Some((&expected, rest)) = template.split_first() {
        if expected == b'#' {
            let digits = line.iter().take_while(|byte| byte.is_ascii_digit()).count();
            if digits == 0 || digits > 9 {
                return false;
            }
            line = &line[digits..];
        } else {
            match line.split_first() {
                Some((&actual, remaining)) if actual == expected => line = remaining,
                _ => return false,
            }
        }
        template = rest;
    }
    line.is_empty()
}

fn blank_page_tsv(width: usize, height: usize) -> String {
    format!("level\tpage_num\tblock_num\tpar_num\tline_num\tword_num\tleft\ttop\twidth\theight\tconf\ttext\n1\t1\t0\t0\t0\t0\t0\t0\t{width}\t{height}\t-1\t\n")
}

/// Best effort: a child that keeps normal priority still works, only less politely.
fn lower_priority(process_id: u32) {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Threading::{OpenProcess, SetPriorityClass, BELOW_NORMAL_PRIORITY_CLASS, PROCESS_SET_INFORMATION};
    if let Ok(handle) = unsafe { OpenProcess(PROCESS_SET_INFORMATION, false, process_id) } {
        let _ = unsafe { SetPriorityClass(handle, BELOW_NORMAL_PRIORITY_CLASS) };
        let _ = unsafe { CloseHandle(handle) };
    }
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
        OsString::from("3"),
        OsString::from("--dpi"),
        OsString::from("150"),
        OsString::from("--loglevel"),
        OsString::from("ERROR"),
    ];
    if matches!(output_profile, OcrOutputProfile::Tsv) {
        for argument in ["-c", "tessedit_create_tsv=1", "-c", "tessedit_create_txt=0"] {
            arguments.push(OsString::from(argument));
        }
    }
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
    let directory = std::env::current_exe().unwrap().parent().unwrap().to_path_buf();
    let name = std::env::current_exe().unwrap().file_name().unwrap().to_string_lossy().to_ascii_lowercase();
    if name == "grandchild.exe" {
        fs::write(directory.join("grandchild.pid"), std::process::id().to_string()).unwrap();
        fs::write(directory.join("grandchild.marker"), b"ran").unwrap();
        thread::sleep(Duration::from_secs(30));
        return;
    }
    fs::write(directory.join("child.pid.tmp"), std::process::id().to_string()).unwrap();
    fs::rename(directory.join("child.pid.tmp"), directory.join("child.pid")).unwrap();
    fs::write(directory.join("cwd.txt"), std::env::current_dir().unwrap().to_string_lossy().as_bytes()).unwrap();
    let behavior = fs::read_to_string(directory.join("behavior.txt")).unwrap();
    if matches!(behavior.trim(), "stdout-exact" | "stdout-over" | "stderr-exact" | "stderr-over" | "invalid-stdout" | "invalid-stderr" | "nonzero" | "unexpected-stderr") {
        let mut input = Vec::new();
        std::io::stdin().read_to_end(&mut input).unwrap();
    }
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
        "nonzero-unread" => {
            eprintln!("bounded fake failure");
            exit(7);
        }
        "unexpected-stderr" => {
            print!("recognized text\n");
            eprintln!("unexpected warning");
        }
        "stderr-file" => {
            print!("recognized text\n");
            eprint!("{}", fs::read_to_string(directory.join("stderr.txt")).unwrap());
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
                let directory = repository_root().join("target/ocr-process-tests").join(format!("fake-build-{}", std::process::id()));
                std::fs::create_dir_all(&directory).map_err(|error| error.to_string())?;
                let source = directory.join("fake-child.rs");
                let executable = directory.join("fake-child.exe");
                std::fs::write(&source, fake_source()).map_err(|error| error.to_string())?;
                let output = crate::child_job::test_output(Command::new("rustc")
                    .arg("--edition=2021")
                    .arg("-O")
                    .arg(&source)
                    .arg("-o")
                    .arg(&executable))
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
        // A leftover folder from an earlier run whose process had the same id
        // would carry its child.pid; start every case from an empty folder.
        let _ = std::fs::remove_dir_all(&directory);
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
        let deadline = std::time::Instant::now() + Duration::from_secs(120);
        while std::time::Instant::now() < deadline {
            if let Ok(text) = std::fs::read_to_string(&path) {
                if let Ok(pid) = text.trim().parse() {
                    return pid;
                }
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
        let deadline = std::time::Instant::now() + Duration::from_secs(120);
        while std::time::Instant::now() < deadline {
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
        assert_eq!(text, r#""C:\OCR engine\tesseract.exe" "stdin" "stdout" "--tessdata-dir" "C:\model space\tessdata" "-l" "eng" "--oem" "1" "--psm" "3" "--dpi" "150" "--loglevel" "ERROR""#);
        let tsv = build_command_line(Path::new(r"C:\OCR engine\tesseract.exe"), Path::new(r"C:\model space\tessdata"), OcrOutputProfile::Tsv).unwrap();
        let tsv = String::from_utf16(&tsv[..tsv.len() - 1]).unwrap();
        assert_eq!(tsv, format!("{text} \"-c\" \"tessedit_create_tsv=1\" \"-c\" \"tessedit_create_txt=0\""));
    }

    #[test]
    fn canonical_windows_paths_are_presented_without_unsupported_device_syntax() {
        assert_eq!(external_windows_path(Path::new(r"\\?\C:\OCR\tesseract.exe")).unwrap(), PathBuf::from(r"C:\OCR\tesseract.exe"));
        assert_eq!(external_windows_path(Path::new(r"\\?\UNC\server\share\tessdata")).unwrap(), PathBuf::from(r"\\server\share\tessdata"));
        assert!(matches!(external_windows_path(Path::new(r"\\?\Volume{1234}\tesseract.exe")), Err(OcrProcessError::InvalidAsset(_))));
    }

    #[test]
    fn child_working_directory_is_a_private_temp_directory_outside_the_engine_root() {
        let _serial = test_lock();
        let case = fake_case("exact");
        assert_eq!(case.runner.recognize_p6(TEST_P6, &OcrCancellation::default()).unwrap().text, "recognized text\n");
        let reported = PathBuf::from(std::fs::read_to_string(case.directory.join("cwd.txt")).unwrap());
        let engine_root = external_windows_path(&case.directory.canonicalize().unwrap()).unwrap();
        let temp_root = external_windows_path(&std::env::temp_dir().canonicalize().unwrap()).unwrap();
        assert!(!reported.starts_with(&engine_root), "{reported:?} is inside the engine root {engine_root:?}");
        assert!(!reported.starts_with(&case.directory), "{reported:?} is inside the install folder");
        let reported_parent = external_windows_path(&reported.parent().unwrap().canonicalize().unwrap()).unwrap();
        assert_eq!(reported_parent, temp_root, "{reported:?} is not directly under {temp_root:?}");
        assert!(is_job_directory_name(&reported.file_name().unwrap().to_string_lossy()));
        assert!(!reported.exists(), "the per-job directory must be removed after the job");
    }

    #[test]
    fn job_directories_are_random_exclusive_and_the_sweep_spares_fresh_and_foreign_entries() {
        let first = random_directory_name().unwrap();
        assert!(is_job_directory_name(&first));
        assert_ne!(first, random_directory_name().unwrap());
        for bad in ["pdf-workstation-ocr-", "pdf-workstation-ocr-1234-0", "pdf-workstation-ocr-0123456789ABCDEF0123456789abcdef", "other-0123456789abcdef0123456789abcdef"] {
            assert!(!is_job_directory_name(bad), "{bad}");
        }
        let root = tempfile::tempdir().unwrap();
        let fresh = root.path().join(random_directory_name().unwrap());
        let foreign = root.path().join("pdf-workstation-ocr-keep-me");
        std::fs::create_dir(&fresh).unwrap();
        std::fs::create_dir(&foreign).unwrap();
        let stale = root.path().join(random_directory_name().unwrap());
        let stale_foreign = root.path().join("pdf-workstation-ocr-old-but-not-ours");
        for path in [&stale, &stale_foreign] {
            std::fs::create_dir(path).unwrap();
            std::fs::write(path.join("leftover.txt"), b"x").unwrap();
            let handle = OpenOptions::new().write(true).custom_flags(0x0200_0000).open(path).unwrap();
            handle.set_modified(std::time::SystemTime::now() - Duration::from_secs(25 * 60 * 60)).unwrap();
        }
        sweep_stale_job_directories(root.path());
        assert!(fresh.exists() && foreign.exists() && stale_foreign.exists());
        assert!(!stale.exists(), "a matching directory older than 24 h must be swept");
        let one = JobDirectory::create().unwrap();
        let two = JobDirectory::create().unwrap();
        assert_ne!(one.path(), two.path());
        assert!(one.path().is_dir() && two.path().is_dir());
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
            RunOptions { timeout: Duration::from_secs(120), before_resume: Some(&hook), after_resume: None, before_finish: None, background: false, preprocess: PreprocessOptions::PRODUCTION },
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
                    timeout: Duration::from_secs(120),
                    before_resume: None,
                    after_resume: None,
                    before_finish: Some(&before_finish),
                    background: false,
                    preprocess: PreprocessOptions::PRODUCTION,
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
        let completed_result = completed.runner.recognize_p6(TEST_P6, &cancellation);
        assert!(completed_result.is_ok(), "{completed_result:?}");
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
    fn processes_started_by_other_threads_cannot_keep_a_childs_pipe_ends_open() {
        let _serial = test_lock();
        let case = fake_case("exact");
        let stop = std::sync::atomic::AtomicBool::new(false);
        let mut leaked = 0;
        std::thread::scope(|scope| {
            let spawner = scope.spawn(|| {
                let mut holders = std::collections::VecDeque::new();
                while !stop.load(AtomicOrdering::Acquire) {
                    let mut command = Command::new("ping");
                    command.args(["-n", "6", "127.0.0.1"]).stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
                    if let Ok(child) = crate::child_job::test_spawn(&mut command) {
                        holders.push_back((std::time::Instant::now(), child));
                    }
                    while holders.front().is_some_and(|(started, _)| started.elapsed() > Duration::from_millis(2_600)) {
                        let (_, mut child) = holders.pop_front().unwrap();
                        let _ = child.kill();
                        let _ = child.wait();
                    }
                    std::thread::sleep(Duration::from_millis(5));
                }
                for (_, mut child) in holders {
                    let _ = child.kill();
                    let _ = child.wait();
                }
            });
            for _ in 0..40 {
                match case.runner.recognize_p6(TEST_P6, &OcrCancellation::default()) {
                    Ok(output) => assert_eq!(output.text, "recognized text\n"),
                    Err(OcrProcessError::System(message)) if message == "OCR output pipes did not close" => leaked += 1,
                    Err(other) => panic!("{other:?}"),
                }
            }
            stop.store(true, AtomicOrdering::Release);
            spawner.join().unwrap();
        });
        assert_eq!(leaked, 0, "another thread's process kept a pipe end of the OCR child open");
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
    fn nonzero_exit_is_reported_ahead_of_a_writer_error_when_input_is_never_read() {
        let _serial = test_lock();
        let (width, height) = (1000_usize, 1000_usize);
        let mut input = format!("P6
{width} {height}
255
").into_bytes();
        input.resize(input.len() + width * height * 3, 255);
        for _ in 0..3 {
            let case = fake_case("nonzero-unread");
            let result = case.runner.recognize_p6(&input, &OcrCancellation::default());
            assert!(
                matches!(result, Err(OcrProcessError::ChildExit { code: 7, ref stderr }) if stderr == "bounded fake failure
"),
                "{result:?}"
            );
        }
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
    fn only_the_exact_engine_diagnostics_are_allowed_on_stderr() {
        let _serial = test_lock();
        let run = |stderr: &str| {
            let case = fake_case("stderr-file");
            std::fs::write(case.directory.join("stderr.txt"), stderr).unwrap();
            case.runner.recognize_p6(TEST_P6, &OcrCancellation::default()).map(|output| output.text)
        };
        assert_eq!(run("Detected 178 diacritics\r\nWarning:guessing pitch as xheight on row 4, block 21\n").unwrap(), "recognized text\n");
        assert_eq!(run("Error in boxClipToRectangle: box outside rectangle\nError in pixScanForForeground: invalid box\n").unwrap(), "recognized text\n");
        assert_eq!(run("Too few characters. Skipping this page\n").unwrap(), "", "a skipped page has no text");
        for refused in [
            "Detected 17a diacritics\n", "Detected  17 diacritics\n", "Detected 1234567890 diacritics\n",
            "detected 17 diacritics\n", "Detected 17 diacritics", "Detected 17 diacritics \n",
            "Detected -1 diacritics\n", "Warning:guessing pitch as xheight on row 4, block\n",
            "Detected 1 diacritics\nunexpected warning\n", "Error in boxClipToRectangle: box outside rectangle!\n",
        ] {
            assert!(matches!(run(refused), Err(OcrProcessError::UnexpectedStderr(_))), "{refused:?}");
        }
        let many = "Detected 1 diacritics\n".repeat(OCR_STDERR_MAX_LINES + 1);
        assert!(matches!(run(&many), Err(OcrProcessError::UnexpectedStderr(_))));
        assert!(matches!(run(&"Detected 1 diacritics\n".repeat(OCR_STDERR_MAX_LINES)), Ok(_)));
    }

    #[test]
    fn a_skipped_page_is_an_empty_strict_tsv_page() {
        assert_eq!(classify_stderr(""), Some(StderrVerdict::Clean));
        assert_eq!(classify_stderr("Too few characters. Skipping this page\r\n"), Some(StderrVerdict::PageSkipped));
        let parsed = crate::searchable_ocr::parse_tesseract_tsv(blank_page_tsv(1275, 1650).as_bytes(), 1275, 1650).unwrap();
        assert!(parsed.words.is_empty() && parsed.extracted_text.is_empty());
    }

    #[test]
    fn timeout_and_unwind_drop_kill_and_reap_the_owned_child() {
        let _serial = test_lock();
        let timed = fake_case("sleep");
        let timed_directory = timed.directory.clone();
        let published = || { wait_for_pid(&timed_directory); };
        let result = timed.runner.recognize_with_options(
            TEST_P6,
            &OcrCancellation::default(),
            RunOptions { timeout: Duration::from_millis(200), before_resume: None, after_resume: Some(&published), before_finish: None, background: false, preprocess: PreprocessOptions::PRODUCTION },
        );
        assert_eq!(result, Err(OcrProcessError::TimedOut));
        assert_process_reaped(wait_for_pid(&timed.directory));

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
                RunOptions { timeout: Duration::from_secs(120), before_resume: None, after_resume: Some(&after_resume), before_finish: None, background: false, preprocess: PreprocessOptions::PRODUCTION },
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
        let status = crate::child_job::test_status(Command::new(std::env::current_exe().unwrap())
            .arg("--exact")
            .arg("ocr_process::tests::abrupt_host_exit_helper_closes_the_job")
            .arg("--nocapture")
            .env("OCR_PROCESS_HOST_EXIT_HELPER", "1")
            .env("OCR_PROCESS_HOST_PID_FILE", &pid_file))
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
        let output = crate::child_job::test_output(Command::new("cmd.exe").args(["/d", "/c", "mklink", "/J"]).arg(&link).arg(&actual)).unwrap();
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
