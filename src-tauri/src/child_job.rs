//! A child process confined to its own Windows Job Object, with bounded pipes.
//!
//! Shared by the OCR engine child (`ocr_process.rs`) and the PDF-engine helper (`pdf_helper`). The parent holds the
//! only job handle, so closing it (or the parent exiting) kills the child (`KILL_ON_JOB_CLOSE`). Optional
//! hardening for children that run untrusted input: no error dialogs and no Windows Error Reporting for
//! unhandled exceptions or refused allocations (`DIE_ON_UNHANDLED_EXCEPTION` plus `harden_job_child_process`
//! inside the child; fail-fast exits are not covered, see there), UI restrictions, and an I/O completion port
//! that reports limit violations. A memory limit makes allocations fail; it does not end the process, and
//! Windows reports it once per process, so a caller that enables notifications must check
//! `JobChild::limit_violation` after every reply and discard a child that has hit a limit.

use std::alloc::{GlobalAlloc, Layout, System};
use std::ffi::OsStr;
use std::fmt;
use std::fs::File;
use std::io::{Read, Write};
use std::mem::size_of;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::sync::atomic::{AtomicBool, AtomicPtr, AtomicU8, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use windows::core::{BOOL, PCWSTR, PWSTR};
use windows::Win32::Foundation::{
    DuplicateHandle, SetHandleInformation, DUPLICATE_HANDLE_OPTIONS, HANDLE, HANDLE_FLAG_INHERIT, INVALID_HANDLE_VALUE,
    WAIT_OBJECT_0, WAIT_TIMEOUT,
};
use windows::Win32::Security::SECURITY_ATTRIBUTES;
use windows::Win32::System::Diagnostics::Debug::{
    SetErrorMode, SetUnhandledExceptionFilter, EXCEPTION_POINTERS, SEM_FAILCRITICALERRORS,
    SEM_NOGPFAULTERRORBOX, SEM_NOOPENFILEERRORBOX,
};
use windows::Win32::Storage::FileSystem::WriteFile;
use windows::Win32::System::ErrorReporting::{WerSetFlags, WER_FAULT_REPORTING, WER_FAULT_REPORTING_NO_UI};
use windows::Win32::System::IO::{CancelSynchronousIo, CreateIoCompletionPort, GetQueuedCompletionStatus, OVERLAPPED};
use windows::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JobObjectAssociateCompletionPortInformation,
    JobObjectBasicUIRestrictions, JobObjectExtendedLimitInformation, QueryInformationJobObject,
    SetInformationJobObject, TerminateJobObject, JOBOBJECT_ASSOCIATE_COMPLETION_PORT,
    JOBOBJECT_BASIC_UI_RESTRICTIONS, JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JOB_OBJECT_LIMIT_ACTIVE_PROCESS,
    JOB_OBJECT_LIMIT_DIE_ON_UNHANDLED_EXCEPTION, JOB_OBJECT_LIMIT_JOB_MEMORY,
    JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE, JOB_OBJECT_LIMIT_PROCESS_MEMORY, JOB_OBJECT_UILIMIT_DESKTOP,
    JOB_OBJECT_UILIMIT_DISPLAYSETTINGS, JOB_OBJECT_UILIMIT_EXITWINDOWS, JOB_OBJECT_UILIMIT_GLOBALATOMS,
    JOB_OBJECT_UILIMIT_HANDLES, JOB_OBJECT_UILIMIT_READCLIPBOARD, JOB_OBJECT_UILIMIT_SYSTEMPARAMETERS,
    JOB_OBJECT_UILIMIT_WRITECLIPBOARD,
};
use windows::Win32::System::Pipes::CreatePipe;
use windows::Win32::System::Threading::{
    CreateProcessW, DeleteProcThreadAttributeList, GetCurrentProcess, GetCurrentThreadId, GetExitCodeProcess,
    GetProcessId, InitializeProcThreadAttributeList, OpenThread, ResumeThread, TerminateProcess,
    UpdateProcThreadAttribute, WaitForSingleObject, CREATE_NO_WINDOW, CREATE_SUSPENDED,
    EXTENDED_STARTUPINFO_PRESENT, LPPROC_THREAD_ATTRIBUTE_LIST, PROCESS_INFORMATION,
    PROC_THREAD_ATTRIBUTE_HANDLE_LIST, STARTF_USESTDHANDLES, STARTUPINFOEXW, THREAD_TERMINATE,
};

/// Exit code the parent uses when it ends a child because the caller cancelled (`STATUS_CONTROL_C_EXIT`).
pub(crate) const EXIT_CANCELLED: u32 = 0xc000_013a;
/// Exit code the parent uses when it ends a child for a limit, a timeout or a failure (`STATUS_INSUFFICIENT_RESOURCES`).
pub(crate) const EXIT_RESOURCE_LIMIT: u32 = 0xc000_009a;
/// Exit code a hardened child uses when an allocation fails, instead of Rust's abort (which goes to
/// Windows Error Reporting and can write a dump to the system drive).
pub(crate) const EXIT_ALLOCATION_FAILURE: u32 = 0xe000_0a11;

/// How long `run_bounded` waits, once the child has ended, for its pipe threads to finish. A pipe end held by
/// some other process (for example one started by another thread while the child's pipe ends were briefly
/// inheritable) keeps the pipe open after the child is gone; the blocked read or write is then cancelled and
/// the run reports `io_cancelled`.
pub(crate) const PIPE_DRAIN_GRACE: Duration = Duration::from_secs(2);
/// Longest wait for a terminated process to finish exiting.
const REAP_TIMEOUT: Duration = Duration::from_secs(5);
/// Longest time spent cancelling blocked pipe I/O after the grace period.
const IO_CANCEL_LIMIT: Duration = Duration::from_secs(5);
const UNHANDLED_EXCEPTION_MARKER: &[u8] = b"\nchild_job: unhandled exception, exiting with its code\n";

const JOB_OBJECT_MSG_ACTIVE_PROCESS_LIMIT: u32 = 3;
const JOB_OBJECT_MSG_PROCESS_MEMORY_LIMIT: u32 = 9;
const JOB_OBJECT_MSG_JOB_MEMORY_LIMIT: u32 = 10;
const EXCEPTION_EXECUTE_HANDLER: i32 = 1;
const VIOLATION_NONE: u8 = 0;

/// A system call failed while creating, supervising or reading a child. The text names the child by its label.
#[derive(Debug, Eq, PartialEq)]
pub(crate) struct ChildSystemError(pub(crate) String);

impl fmt::Display for ChildSystemError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// A path or argument contained a NUL character and cannot be passed to `CreateProcessW`.
#[derive(Debug, Eq, PartialEq)]
pub(crate) struct ContainsNul;

/// Job Object limits for one child. The child is always killed when the job handle closes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct JobLimits {
    /// Committed private memory one process may hold (`ProcessMemoryLimit`).
    pub(crate) process_memory: usize,
    /// Committed private memory the whole job may hold (`JobMemoryLimit`).
    pub(crate) job_memory: usize,
    pub(crate) active_processes: u32,
    /// The system sets `SEM_NOGPFAULTERRORBOX` in the child: no error dialog can hold it open.
    pub(crate) die_on_unhandled_exception: bool,
    /// No clipboard, global atoms, desktop switch, display or system settings, exit-windows, or USER handles
    /// outside the job.
    pub(crate) ui_restrictions: bool,
    /// Associate an I/O completion port so memory and process-count violations can be read back.
    pub(crate) limit_notifications: bool,
}

impl JobLimits {
    /// One process, both memory limits equal to `memory`, no other hardening. This is the OCR child's job.
    pub(crate) const fn single_process(memory: usize) -> Self {
        Self {
            process_memory: memory,
            job_memory: memory,
            active_processes: 1,
            die_on_unhandled_exception: false,
            ui_restrictions: false,
            limit_notifications: false,
        }
    }

    /// One process with every hardening option on, for a child that handles untrusted documents.
    pub(crate) const fn hardened_single_process(memory: usize) -> Self {
        Self {
            die_on_unhandled_exception: true,
            ui_restrictions: true,
            limit_notifications: true,
            ..Self::single_process(memory)
        }
    }
}

/// A limit the job reported through its completion port.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum LimitViolation {
    ProcessMemory = 1,
    JobMemory = 2,
    ActiveProcess = 3,
}

impl LimitViolation {
    fn from_message(message: u32) -> Option<Self> {
        match message {
            JOB_OBJECT_MSG_PROCESS_MEMORY_LIMIT => Some(Self::ProcessMemory),
            JOB_OBJECT_MSG_JOB_MEMORY_LIMIT => Some(Self::JobMemory),
            JOB_OBJECT_MSG_ACTIVE_PROCESS_LIMIT => Some(Self::ActiveProcess),
            _ => None,
        }
    }

    fn from_code(code: u8) -> Option<Self> {
        match code {
            1 => Some(Self::ProcessMemory),
            2 => Some(Self::JobMemory),
            3 => Some(Self::ActiveProcess),
            _ => None,
        }
    }
}

/// What a child's exit code means under the conventions above. The code alone does not prove who ended the
/// child: a child could exit with any value itself.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ExitKind {
    Success,
    Code(u32),
    Cancelled,
    EndedForResourceLimit,
    AllocationFailure,
    Exception(u32),
}

pub(crate) fn classify_exit_code(code: u32) -> ExitKind {
    match code {
        0 => ExitKind::Success,
        EXIT_CANCELLED => ExitKind::Cancelled,
        EXIT_RESOURCE_LIMIT => ExitKind::EndedForResourceLimit,
        EXIT_ALLOCATION_FAILURE => ExitKind::AllocationFailure,
        code if code >= 0x8000_0000 => ExitKind::Exception(code),
        code => ExitKind::Code(code),
    }
}

/// The program to start: NUL-terminated wide strings, as `CreateProcessW` takes them.
pub(crate) struct ChildCommand {
    pub(crate) application: Vec<u16>,
    pub(crate) command_line: Vec<u16>,
    pub(crate) current_directory: Vec<u16>,
}

/// Parent ends of the child's stdin (write), stdout (read) and stderr (read).
pub(crate) struct ChildPipes {
    pub(crate) stdin: OwnedHandle,
    pub(crate) stdout: OwnedHandle,
    pub(crate) stderr: OwnedHandle,
}

/// The child process and its job. Dropping it ends the child and waits for it.
pub(crate) struct JobChild {
    label: &'static str,
    process: OwnedHandle,
    job: OwnedHandle,
    notifications: Option<OwnedHandle>,
    violation: AtomicU8,
    assigned: bool,
    reaped: bool,
}

impl JobChild {
    fn new(label: &'static str, process: OwnedHandle, job: OwnedHandle, notifications: Option<OwnedHandle>) -> Self {
        Self { label, process, job, notifications, violation: AtomicU8::new(VIOLATION_NONE), assigned: false, reaped: false }
    }

    pub(crate) fn process_id(&self) -> u32 {
        unsafe { GetProcessId(as_windows_handle(&self.process)) }
    }

    /// Duplicates `source` into the child with exactly `access` (never the source's own access) and
    /// returns the handle value in the child. The child owns and must close it; it dies with the child.
    pub(crate) fn share_handle(&self, source: HANDLE, access: u32) -> Result<u64, ChildSystemError> {
        let mut target = HANDLE::default();
        unsafe {
            DuplicateHandle(GetCurrentProcess(), source, as_windows_handle(&self.process), &mut target, access, false, DUPLICATE_HANDLE_OPTIONS(0))
        }
        .map_err(|error| ChildSystemError(format!("Could not share a handle with the {} process: {error}", self.label)))?;
        Ok(target.0 as usize as u64)
    }

    /// Drains the job's completion port without waiting and returns the first limit violation it has ever
    /// reported. Sticky: Windows sends each violation once per process. `None` when notifications are off.
    pub(crate) fn limit_violation(&self) -> Option<LimitViolation> {
        if let Some(port) = &self.notifications {
            loop {
                let mut message = 0_u32;
                let mut key = 0_usize;
                let mut overlapped: *mut OVERLAPPED = std::ptr::null_mut();
                let received = unsafe {
                    GetQueuedCompletionStatus(as_windows_handle(port), &mut message, &mut key, &mut overlapped, 0)
                };
                if received.is_err() {
                    break;
                }
                if let Some(violation) = LimitViolation::from_message(message) {
                    let _ = self.violation.compare_exchange(VIOLATION_NONE, violation as u8, Ordering::AcqRel, Ordering::Acquire);
                }
            }
        }
        LimitViolation::from_code(self.violation.load(Ordering::Acquire))
    }

    fn terminate_direct_and_reap(&mut self) {
        if !self.reaped {
            let _ = unsafe { TerminateProcess(as_windows_handle(&self.process), EXIT_RESOURCE_LIMIT) };
            self.reaped = self.wait_for_exit(REAP_TIMEOUT);
        }
    }

    pub(crate) fn wait_for_exit(&self, limit: Duration) -> bool {
        let millis = u32::try_from(limit.as_millis()).unwrap_or(u32::MAX - 1);
        (unsafe { WaitForSingleObject(as_windows_handle(&self.process), millis) }) == WAIT_OBJECT_0
    }

    /// Ends every process in the job with `exit_code` and waits for the child. No-op once it has been reaped.
    pub(crate) fn terminate_job_and_reap(&mut self, exit_code: u32) -> Result<(), ChildSystemError> {
        if self.reaped {
            return Ok(());
        }
        if let Err(job_error) = unsafe { TerminateJobObject(as_windows_handle(&self.job), exit_code) } {
            unsafe { TerminateProcess(as_windows_handle(&self.process), exit_code) }.map_err(|process_error| {
                ChildSystemError(format!("Could not terminate {} job ({job_error}) or process ({process_error})", self.label))
            })?;
        }
        if !self.wait_for_exit(REAP_TIMEOUT) {
            return Err(ChildSystemError(format!("Could not reap {} process", self.label)));
        }
        self.reaped = true;
        Ok(())
    }

    pub(crate) fn exit_code(&self) -> Result<u32, ChildSystemError> {
        let mut exit_code = 0_u32;
        unsafe { GetExitCodeProcess(as_windows_handle(&self.process), &mut exit_code) }
            .map_err(|error| ChildSystemError(format!("Could not read {} exit code: {error}", self.label)))?;
        Ok(exit_code)
    }

    /// Highest committed memory the job has held (`PeakJobMemoryUsed`).
    pub(crate) fn peak_job_commit(&self) -> Result<usize, ChildSystemError> {
        Ok(query_extended_limits(&self.job, self.label)?.PeakJobMemoryUsed)
    }
}

impl Drop for JobChild {
    fn drop(&mut self) {
        if !self.reaped {
            if self.assigned {
                let _ = unsafe { TerminateJobObject(as_windows_handle(&self.job), EXIT_RESOURCE_LIMIT) };
            } else {
                let _ = unsafe { TerminateProcess(as_windows_handle(&self.process), EXIT_RESOURCE_LIMIT) };
            }
            self.reaped = self.wait_for_exit(REAP_TIMEOUT);
        }
    }
}

/// A child created suspended and already inside its job: it has run no code yet.
pub(crate) struct SuspendedChild {
    child: JobChild,
    thread: OwnedHandle,
    pipes: ChildPipes,
}

impl SuspendedChild {
    /// Ends the child before it runs.
    pub(crate) fn terminate_and_reap(mut self, exit_code: u32) -> Result<(), ChildSystemError> {
        self.child.terminate_job_and_reap(exit_code)
    }

    pub(crate) fn resume(mut self) -> Result<RunningChild, ChildSystemError> {
        if unsafe { ResumeThread(as_windows_handle(&self.thread)) } == u32::MAX {
            self.child.terminate_job_and_reap(EXIT_RESOURCE_LIMIT)?;
            return Err(ChildSystemError(format!("Could not resume {} process", self.child.label)));
        }
        let Self { child, thread, pipes } = self;
        drop(thread);
        Ok(RunningChild { child, pipes })
    }
}

/// A running child. Long-lived callers use the pipes directly and check `child.limit_violation()` after
/// every reply; one-shot callers hand it to `run_bounded`.
pub(crate) struct RunningChild {
    pub(crate) child: JobChild,
    pub(crate) pipes: ChildPipes,
}

/// Creates the job, the three pipes and a suspended child that inherits only those pipes, then assigns the
/// child to the job. `prepare` builds the command after the pipes exist; `before_create` runs just before
/// `CreateProcessW` and can refuse (for example on cancellation). System failures convert through `E::from`.
pub(crate) fn spawn_suspended<E: From<ChildSystemError>>(
    label: &'static str,
    limits: &JobLimits,
    prepare: impl FnOnce() -> Result<ChildCommand, E>,
    before_create: impl FnOnce() -> Result<(), E>,
) -> Result<SuspendedChild, E> {
    let (job, notifications) = create_job(label, limits)?;
    let (child_stdin, parent_stdin) = create_pipe(label, true)?;
    let (parent_stdout, child_stdout) = create_pipe(label, false)?;
    let (parent_stderr, child_stderr) = create_pipe(label, false)?;
    let inherited = [as_windows_handle(&child_stdin), as_windows_handle(&child_stdout), as_windows_handle(&child_stderr)];
    let mut attributes = ProcThreadAttributes::with_handle_list(label, &inherited)?;
    let mut startup = STARTUPINFOEXW::default();
    startup.StartupInfo.cb = u32::try_from(size_of::<STARTUPINFOEXW>()).unwrap();
    startup.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
    startup.StartupInfo.hStdInput = inherited[0];
    startup.StartupInfo.hStdOutput = inherited[1];
    startup.StartupInfo.hStdError = inherited[2];
    startup.lpAttributeList = attributes.list;
    let mut process_info = PROCESS_INFORMATION::default();
    let ChildCommand { application, mut command_line, current_directory } = prepare()?;
    let creation_flags = CREATE_SUSPENDED | CREATE_NO_WINDOW | EXTENDED_STARTUPINFO_PRESENT;
    before_create()?;
    unsafe {
        CreateProcessW(
            PCWSTR(application.as_ptr()),
            Some(PWSTR(command_line.as_mut_ptr())),
            None,
            None,
            true,
            creation_flags,
            None,
            PCWSTR(current_directory.as_ptr()),
            &startup.StartupInfo,
            &mut process_info,
        )
    }
    .map_err(|error| ChildSystemError(format!("Could not create suspended {label} process: {error}")))?;
    attributes.release();
    let process = unsafe { owned_handle(process_info.hProcess) };
    let thread = unsafe { owned_handle(process_info.hThread) };
    drop(child_stdin);
    drop(child_stdout);
    drop(child_stderr);
    let mut child = JobChild::new(label, process, job, notifications);
    if let Err(error) = unsafe { AssignProcessToJobObject(as_windows_handle(&child.job), as_windows_handle(&child.process)) } {
        child.terminate_direct_and_reap();
        return Err(ChildSystemError(format!("Could not assign {label} process to its job: {error}")).into());
    }
    child.assigned = true;
    Ok(SuspendedChild { child, thread, pipes: ChildPipes { stdin: parent_stdin, stdout: parent_stdout, stderr: parent_stderr } })
}

/// Byte caps and deadline for a one-shot child.
#[derive(Clone, Copy, Debug)]
pub(crate) struct OutputBounds {
    pub(crate) stdout_limit: usize,
    pub(crate) stderr_limit: usize,
    pub(crate) timeout: Duration,
    pub(crate) poll_interval: Duration,
    /// Wait for the pipe threads after the child has ended before cancelling their blocked I/O
    /// (normally `PIPE_DRAIN_GRACE`).
    pub(crate) drain_grace: Duration,
}

/// Why `run_bounded` stopped waiting. Every reason except `Exited` means the parent ended the child.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum TerminalReason {
    Exited,
    Cancelled,
    TimedOut,
    StdoutLimit,
    StderrLimit,
    LimitViolation(LimitViolation),
    WaitFailed,
}

/// Result of a one-shot child. The child has been reaped; read its exit code and job peak from `child`.
pub(crate) struct ChildRun {
    pub(crate) reason: TerminalReason,
    pub(crate) stdout_exceeded: bool,
    pub(crate) stderr_exceeded: bool,
    pub(crate) writer_result: Result<(), ChildSystemError>,
    pub(crate) stdout: Vec<u8>,
    pub(crate) stderr: Vec<u8>,
    /// Any limit the job reported, also when the child exited on its own after an allocation failed.
    pub(crate) limit_violation: Option<LimitViolation>,
    /// A pipe stayed open after the child ended (another process holds an end) and its blocked read or write
    /// was cancelled after `drain_grace`: stdout or stderr may be incomplete, or the input was not all written.
    pub(crate) io_cancelled: bool,
    pub(crate) child: JobChild,
}

/// Writes `input` to the child's stdin and closes it, captures stdout and stderr up to their caps, and waits.
/// The child is ended (whole job) on a cap overflow, a reported limit violation, cancellation or the timeout.
/// Bounded even if a pipe never closes: returns within about `timeout + drain_grace` plus the reap and
/// cancel limits.
pub(crate) fn run_bounded(
    running: RunningChild,
    input: &[u8],
    bounds: &OutputBounds,
    is_cancelled: &dyn Fn() -> bool,
) -> Result<ChildRun, ChildSystemError> {
    let RunningChild { mut child, pipes: ChildPipes { stdin, stdout, stderr } } = running;
    let label = child.label;
    let started = Instant::now();
    let stdout_exceeded = AtomicBool::new(false);
    let stderr_exceeded = AtomicBool::new(false);
    let poll_millis = u32::try_from(bounds.poll_interval.as_millis()).unwrap_or(u32::MAX - 1);
    let workers = [IoWorker::default(), IoWorker::default(), IoWorker::default()];
    let [writer_io, stdout_io, stderr_io] = &workers;
    let terminal = std::thread::scope(|scope| {
        let writer = scope.spawn(move || writer_io.run(|io| write_input(label, stdin, input, io)));
        let stdout_reader = scope.spawn(|| stdout_io.run(|io| read_bounded(label, stdout, bounds.stdout_limit, &stdout_exceeded, io)));
        let stderr_reader = scope.spawn(|| stderr_io.run(|io| read_bounded(label, stderr, bounds.stderr_limit, &stderr_exceeded, io)));
        let reason = loop {
            let wait = unsafe { WaitForSingleObject(as_windows_handle(&child.process), poll_millis) };
            if wait == WAIT_OBJECT_0 {
                child.reaped = true;
                break TerminalReason::Exited;
            }
            if wait != WAIT_TIMEOUT {
                let _ = child.terminate_job_and_reap(EXIT_RESOURCE_LIMIT);
                break TerminalReason::WaitFailed;
            }
            if stdout_exceeded.load(Ordering::Acquire) {
                let _ = child.terminate_job_and_reap(EXIT_RESOURCE_LIMIT);
                break TerminalReason::StdoutLimit;
            }
            if stderr_exceeded.load(Ordering::Acquire) {
                let _ = child.terminate_job_and_reap(EXIT_RESOURCE_LIMIT);
                break TerminalReason::StderrLimit;
            }
            if child.notifications.is_some() {
                if let Some(violation) = child.limit_violation() {
                    let _ = child.terminate_job_and_reap(EXIT_RESOURCE_LIMIT);
                    break TerminalReason::LimitViolation(violation);
                }
            }
            if is_cancelled() {
                let _ = child.terminate_job_and_reap(EXIT_CANCELLED);
                break TerminalReason::Cancelled;
            }
            if started.elapsed() >= bounds.timeout {
                let _ = child.terminate_job_and_reap(EXIT_RESOURCE_LIMIT);
                break TerminalReason::TimedOut;
            }
        };
        finish_io(&workers.each_ref(), bounds.drain_grace);
        let writer_result = writer.join().map_err(|_| ChildSystemError(format!("{label} input worker panicked")))?;
        let stdout = stdout_reader.join().map_err(|_| ChildSystemError(format!("{label} stdout worker panicked")))??;
        let stderr = stderr_reader.join().map_err(|_| ChildSystemError(format!("{label} stderr worker panicked")))??;
        Ok::<_, ChildSystemError>((reason, writer_result, stdout, stderr))
    })?;
    let (reason, writer_result, stdout, stderr) = terminal;
    let limit_violation = child.limit_violation();
    Ok(ChildRun {
        reason,
        stdout_exceeded: stdout_exceeded.load(Ordering::Acquire),
        stderr_exceeded: stderr_exceeded.load(Ordering::Acquire),
        writer_result,
        stdout,
        stderr,
        limit_violation,
        io_cancelled: workers.iter().any(IoWorker::was_cancelled),
        child,
    })
}

/// One pipe thread of `run_bounded` or of the PDF-engine helper's supervisor: its own thread handle (so
/// blocked synchronous I/O can be cancelled from the supervising thread), whether it has finished, and
/// whether it was cancelled.
#[derive(Default)]
pub(crate) struct IoWorker {
    thread: Mutex<Option<OwnedHandle>>,
    done: AtomicBool,
    stop: AtomicBool,
    cancelled_io: AtomicBool,
}

impl IoWorker {
    pub(crate) fn run<T>(&self, work: impl FnOnce(&IoWorker) -> T) -> T {
        struct MarkDone<'a>(&'a AtomicBool);
        impl Drop for MarkDone<'_> {
            fn drop(&mut self) {
                self.0.store(true, Ordering::Release);
            }
        }
        let thread = unsafe { OpenThread(THREAD_TERMINATE, false, GetCurrentThreadId()) }.ok().map(|thread| unsafe { owned_handle(thread) });
        self.arm(thread);
        let _done = MarkDone(&self.done);
        work(self)
    }

    /// Records this thread's handle so `cancel_blocked_io` can reach a blocked read or write. Without
    /// one, blocked I/O could not be cancelled and the scoped join in `run_bounded` could wait on the
    /// pipe forever, so the worker does no I/O at all: `stop` is set before `work` starts, the pipe
    /// end is closed unread or unwritten, and the run reports `io_cancelled`.
    fn arm(&self, thread: Option<OwnedHandle>) {
        match thread {
            Some(thread) => *self.thread.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(thread),
            None => {
                self.stop.store(true, Ordering::Release);
                self.mark_cancelled();
            }
        }
    }

    pub(crate) fn is_done(&self) -> bool {
        self.done.load(Ordering::Acquire)
    }

    pub(crate) fn should_stop(&self) -> bool {
        self.stop.load(Ordering::Acquire)
    }

    /// Records that the I/O was cut short; the caller then returns what it has.
    pub(crate) fn mark_cancelled(&self) {
        self.cancelled_io.store(true, Ordering::Release);
    }

    pub(crate) fn was_cancelled(&self) -> bool {
        self.cancelled_io.load(Ordering::Acquire)
    }

    fn cancel_blocked_io(&self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = &*self.thread.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) {
            let _ = unsafe { CancelSynchronousIo(as_windows_handle(thread)) };
        }
    }
}

/// Waits up to `grace` for every pipe thread, then keeps cancelling the blocked ones (repeatedly, so a
/// thread that was between its stop check and the system call is caught) for up to `IO_CANCEL_LIMIT`.
pub(crate) fn finish_io(workers: &[&IoWorker], grace: Duration) {
    let all_done = || workers.iter().all(|worker| worker.is_done());
    let started = Instant::now();
    while !all_done() && started.elapsed() < grace {
        std::thread::sleep(Duration::from_millis(1));
    }
    let cancelling = Instant::now();
    while !all_done() && cancelling.elapsed() < IO_CANCEL_LIMIT {
        for worker in workers.iter().filter(|worker| !worker.is_done()) {
            worker.cancel_blocked_io();
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn create_job(label: &'static str, limits: &JobLimits) -> Result<(OwnedHandle, Option<OwnedHandle>), ChildSystemError> {
    let handle = unsafe { CreateJobObjectW(None, PCWSTR::null()) }
        .map_err(|error| ChildSystemError(format!("Could not create {label} job: {error}")))?;
    let job = unsafe { owned_handle(handle) };
    let mut information = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
    information.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE
        | JOB_OBJECT_LIMIT_ACTIVE_PROCESS
        | JOB_OBJECT_LIMIT_PROCESS_MEMORY
        | JOB_OBJECT_LIMIT_JOB_MEMORY;
    if limits.die_on_unhandled_exception {
        information.BasicLimitInformation.LimitFlags |= JOB_OBJECT_LIMIT_DIE_ON_UNHANDLED_EXCEPTION;
    }
    information.BasicLimitInformation.ActiveProcessLimit = limits.active_processes;
    information.ProcessMemoryLimit = limits.process_memory;
    information.JobMemoryLimit = limits.job_memory;
    unsafe {
        SetInformationJobObject(
            as_windows_handle(&job),
            JobObjectExtendedLimitInformation,
            &information as *const _ as *const _,
            u32::try_from(size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>()).unwrap(),
        )
    }
    .map_err(|error| ChildSystemError(format!("Could not configure {label} job: {error}")))?;
    if limits.ui_restrictions {
        let restrictions = JOBOBJECT_BASIC_UI_RESTRICTIONS {
            UIRestrictionsClass: JOB_OBJECT_UILIMIT_DESKTOP
                | JOB_OBJECT_UILIMIT_DISPLAYSETTINGS
                | JOB_OBJECT_UILIMIT_EXITWINDOWS
                | JOB_OBJECT_UILIMIT_GLOBALATOMS
                | JOB_OBJECT_UILIMIT_HANDLES
                | JOB_OBJECT_UILIMIT_READCLIPBOARD
                | JOB_OBJECT_UILIMIT_SYSTEMPARAMETERS
                | JOB_OBJECT_UILIMIT_WRITECLIPBOARD,
        };
        unsafe {
            SetInformationJobObject(
                as_windows_handle(&job),
                JobObjectBasicUIRestrictions,
                &restrictions as *const _ as *const _,
                u32::try_from(size_of::<JOBOBJECT_BASIC_UI_RESTRICTIONS>()).unwrap(),
            )
        }
        .map_err(|error| ChildSystemError(format!("Could not restrict {label} job UI: {error}")))?;
    }
    let notifications = if limits.limit_notifications {
        let port = unsafe { CreateIoCompletionPort(INVALID_HANDLE_VALUE, None, 0, 1) }
            .map_err(|error| ChildSystemError(format!("Could not create {label} job notification port: {error}")))?;
        let port = unsafe { owned_handle(port) };
        let association = JOBOBJECT_ASSOCIATE_COMPLETION_PORT {
            CompletionKey: std::ptr::null_mut(),
            CompletionPort: as_windows_handle(&port),
        };
        unsafe {
            SetInformationJobObject(
                as_windows_handle(&job),
                JobObjectAssociateCompletionPortInformation,
                &association as *const _ as *const _,
                u32::try_from(size_of::<JOBOBJECT_ASSOCIATE_COMPLETION_PORT>()).unwrap(),
            )
        }
        .map_err(|error| ChildSystemError(format!("Could not attach {label} job notification port: {error}")))?;
        Some(port)
    } else {
        None
    };
    Ok((job, notifications))
}

fn query_extended_limits(job: &OwnedHandle, label: &str) -> Result<JOBOBJECT_EXTENDED_LIMIT_INFORMATION, ChildSystemError> {
    let mut information = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
    unsafe {
        QueryInformationJobObject(
            Some(as_windows_handle(job)),
            JobObjectExtendedLimitInformation,
            &mut information as *mut _ as *mut _,
            u32::try_from(size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>()).unwrap(),
            None,
        )
    }
    .map_err(|error| ChildSystemError(format!("Could not read {label} job memory accounting: {error}")))?;
    Ok(information)
}

fn create_pipe(label: &str, stdin: bool) -> Result<(OwnedHandle, OwnedHandle), ChildSystemError> {
    let attributes = SECURITY_ATTRIBUTES {
        nLength: u32::try_from(size_of::<SECURITY_ATTRIBUTES>()).unwrap(),
        lpSecurityDescriptor: std::ptr::null_mut(),
        bInheritHandle: BOOL(1),
    };
    let mut read = HANDLE::default();
    let mut write = HANDLE::default();
    unsafe { CreatePipe(&mut read, &mut write, Some(&attributes), 0) }
        .map_err(|error| ChildSystemError(format!("Could not create {label} pipe: {error}")))?;
    let read = unsafe { owned_handle(read) };
    let write = unsafe { owned_handle(write) };
    let parent = if stdin { &write } else { &read };
    unsafe { SetHandleInformation(as_windows_handle(parent), HANDLE_FLAG_INHERIT.0, windows::Win32::Foundation::HANDLE_FLAGS(0)) }
        .map_err(|error| ChildSystemError(format!("Could not restrict {label} pipe inheritance: {error}")))?;
    Ok((read, write))
}

fn write_input(label: &str, handle: OwnedHandle, input: &[u8], io: &IoWorker) -> Result<(), ChildSystemError> {
    let mut file = File::from(handle);
    if io.should_stop() {
        io.mark_cancelled();
        return Ok(());
    }
    match file.write_all(input) {
        Ok(()) => Ok(()),
        Err(_) if io.should_stop() => {
            io.mark_cancelled();
            Ok(())
        }
        Err(error) => Err(ChildSystemError(format!("Could not write {label} input: {error}"))),
    }
}

fn read_bounded(label: &str, handle: OwnedHandle, limit: usize, exceeded: &AtomicBool, io: &IoWorker) -> Result<Vec<u8>, ChildSystemError> {
    let mut file = File::from(handle);
    let mut output = Vec::with_capacity(limit.min(8192));
    let mut buffer = [0_u8; 8192];
    loop {
        if io.should_stop() {
            io.mark_cancelled();
            return Ok(output);
        }
        let count = match file.read(&mut buffer) {
            Ok(count) => count,
            Err(_) if io.should_stop() => {
                io.mark_cancelled();
                return Ok(output);
            }
            Err(error) => return Err(ChildSystemError(format!("Could not read {label} output: {error}"))),
        };
        if count == 0 {
            return Ok(output);
        }
        let remaining = limit.saturating_sub(output.len());
        if count > remaining {
            output.extend_from_slice(&buffer[..remaining]);
            exceeded.store(true, Ordering::Release);
            return Ok(output);
        }
        output.extend_from_slice(&buffer[..count]);
    }
}

struct ProcThreadAttributes {
    storage: Vec<usize>,
    list: LPPROC_THREAD_ATTRIBUTE_LIST,
    released: bool,
}

impl ProcThreadAttributes {
    fn with_handle_list(label: &str, handles: &[HANDLE]) -> Result<Self, ChildSystemError> {
        let mut bytes = 0_usize;
        let _ = unsafe { InitializeProcThreadAttributeList(None, 1, None, &mut bytes) };
        if bytes == 0 {
            return Err(ChildSystemError(format!("Could not size {label} process attributes")));
        }
        let words = bytes
            .checked_add(size_of::<usize>() - 1)
            .ok_or_else(|| ChildSystemError(format!("{label} process attributes overflow")))?
            / size_of::<usize>();
        let mut storage = vec![0_usize; words];
        let list = LPPROC_THREAD_ATTRIBUTE_LIST(storage.as_mut_ptr().cast());
        unsafe { InitializeProcThreadAttributeList(Some(list), 1, None, &mut bytes) }
            .map_err(|error| ChildSystemError(format!("Could not initialize {label} process attributes: {error}")))?;
        let update = unsafe {
            UpdateProcThreadAttribute(
                list,
                0,
                PROC_THREAD_ATTRIBUTE_HANDLE_LIST as usize,
                Some(handles.as_ptr().cast()),
                std::mem::size_of_val(handles),
                None,
                None,
            )
        };
        if let Err(error) = update {
            unsafe { DeleteProcThreadAttributeList(list) };
            return Err(ChildSystemError(format!("Could not restrict {label} process handles: {error}")));
        }
        Ok(Self { storage, list, released: false })
    }

    fn release(&mut self) {
        if !self.released {
            unsafe { DeleteProcThreadAttributeList(self.list) };
            self.released = true;
        }
    }
}

impl Drop for ProcThreadAttributes {
    fn drop(&mut self) {
        self.release();
        let _ = self.storage.len();
    }
}

/// Quotes each argument for `CommandLineToArgvW` / the MSVC runtime, joins them with spaces and appends a NUL.
/// No shell is involved.
pub(crate) fn command_line(arguments: &[&OsStr]) -> Result<Vec<u16>, ContainsNul> {
    let mut command = Vec::new();
    for (index, argument) in arguments.iter().enumerate() {
        if index != 0 {
            command.push(b' ' as u16);
        }
        append_quoted_argument(&mut command, argument)?;
    }
    command.push(0);
    Ok(command)
}

fn append_quoted_argument(output: &mut Vec<u16>, argument: &OsStr) -> Result<(), ContainsNul> {
    let units = argument.encode_wide().collect::<Vec<_>>();
    if units.contains(&0) {
        return Err(ContainsNul);
    }
    output.push(b'"' as u16);
    let mut backslashes = 0_usize;
    for unit in units {
        if unit == b'\\' as u16 {
            backslashes += 1;
        } else {
            if unit == b'"' as u16 {
                output.extend(std::iter::repeat(b'\\' as u16).take(backslashes * 2 + 1));
            } else {
                output.extend(std::iter::repeat(b'\\' as u16).take(backslashes));
            }
            backslashes = 0;
            output.push(unit);
        }
    }
    output.extend(std::iter::repeat(b'\\' as u16).take(backslashes * 2));
    output.push(b'"' as u16);
    Ok(())
}

pub(crate) fn wide_null(value: &OsStr) -> Result<Vec<u16>, ContainsNul> {
    let mut units = value.encode_wide().collect::<Vec<_>>();
    if units.contains(&0) {
        return Err(ContainsNul);
    }
    units.push(0);
    Ok(units)
}

pub(crate) unsafe fn owned_handle(handle: HANDLE) -> OwnedHandle {
    unsafe { OwnedHandle::from_raw_handle(handle.0) }
}

pub(crate) fn as_windows_handle(handle: &OwnedHandle) -> HANDLE {
    HANDLE(handle.as_raw_handle())
}

// Inside the child.

static STDERR_HANDLE: AtomicPtr<core::ffi::c_void> = AtomicPtr::new(std::ptr::null_mut());

/// Called first thing in a hardened child. Covered: no error dialogs (`SetErrorMode`, and WER's fault UI off);
/// an unhandled SEH exception ends the process with the exception code after writing one marker line to
/// stderr, before Windows Error Reporting sees it; refused allocations exit through `TerminatingAllocator`.
/// Not covered: fail-fast exits (`__fastfail`, which Rust's `abort` uses, `/GS` stack-cookie failures, heap
/// corruption detection) bypass exception filters and still reach Windows Error Reporting, which may queue a
/// report on the system drive.
///
/// Call it only from a child's own entry point: the helper's mode-flag branch in `main` before Tauri
/// starts (`pdf_helper::run_helper_mode`), or a test child. Never in the app process: it replaces the process-wide
/// error mode, WER flags and unhandled-exception filter. Debug builds assert it runs once per process.
pub(crate) fn harden_job_child_process() {
    static HARDENED: AtomicBool = AtomicBool::new(false);
    debug_assert!(!HARDENED.swap(true, Ordering::AcqRel), "harden_job_child_process must run once, from a child's own entry point");
    STDERR_HANDLE.store(std::io::stderr().as_raw_handle(), Ordering::Release);
    unsafe { SetErrorMode(SEM_FAILCRITICALERRORS | SEM_NOGPFAULTERRORBOX | SEM_NOOPENFILEERRORBOX) };
    let _ = unsafe { WerSetFlags(WER_FAULT_REPORTING(WER_FAULT_REPORTING_NO_UI)) };
    unsafe { SetUnhandledExceptionFilter(Some(exit_with_exception_code)) };
}

unsafe extern "system" fn exit_with_exception_code(information: *const EXCEPTION_POINTERS) -> i32 {
    let code = unsafe { information.as_ref().and_then(|pointers| pointers.ExceptionRecord.as_ref()) }
        .map_or(EXIT_RESOURCE_LIMIT, |record| record.ExceptionCode.0 as u32);
    let stderr = STDERR_HANDLE.load(Ordering::Acquire);
    if !stderr.is_null() {
        let _ = unsafe { WriteFile(HANDLE(stderr), Some(UNHANDLED_EXCEPTION_MARKER), None, None) };
    }
    let _ = unsafe { TerminateProcess(GetCurrentProcess(), code) };
    EXCEPTION_EXECUTE_HANDLER
}

/// Ends the current process with `EXIT_ALLOCATION_FAILURE` without unwinding, aborting or reporting.
pub(crate) fn exit_after_allocation_failure() -> ! {
    let _ = unsafe { TerminateProcess(GetCurrentProcess(), EXIT_ALLOCATION_FAILURE) };
    loop {
        std::thread::park();
    }
}

/// The system allocator, except that a refused allocation ends the process with `EXIT_ALLOCATION_FAILURE`.
/// For a hardened child's `#[global_allocator]`.
pub(crate) struct TerminatingAllocator;

unsafe impl GlobalAlloc for TerminatingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let pointer = unsafe { System.alloc(layout) };
        if pointer.is_null() {
            exit_after_allocation_failure();
        }
        pointer
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        let pointer = unsafe { System.alloc_zeroed(layout) };
        if pointer.is_null() {
            exit_after_allocation_failure();
        }
        pointer
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        unsafe { System.dealloc(pointer, layout) }
    }

    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let resized = unsafe { System.realloc(pointer, layout, new_size) };
        if resized.is_null() {
            exit_after_allocation_failure();
        }
        resized
    }
}

static TERMINATE_ON_REFUSED_ALLOCATION: AtomicBool = AtomicBool::new(false);

/// Makes `ModeSwitchAllocator` behave like `TerminatingAllocator` for the rest of the process. Call it only
/// from a hardened child's own entry point (the PDF-engine helper's mode branch, or a test child), never in
/// the app process.
pub(crate) fn arm_terminating_allocator() {
    TERMINATE_ON_REFUSED_ALLOCATION.store(true, Ordering::Release);
}

/// The exe's `#[global_allocator]`. A `#[global_allocator]` is fixed for the whole binary and cannot depend
/// on the launch mode, so this one is the system allocator, unchanged, until `arm_terminating_allocator`
/// runs; after that a refused allocation ends the process with `EXIT_ALLOCATION_FAILURE`. The flag is read
/// only once the system allocator has already returned null, so the app process (never armed) gets exactly
/// the system allocator's results and Rust's usual allocation-failure handling.
pub(crate) struct ModeSwitchAllocator;

impl ModeSwitchAllocator {
    #[inline]
    fn refused(pointer: *mut u8) -> *mut u8 {
        if pointer.is_null() && TERMINATE_ON_REFUSED_ALLOCATION.load(Ordering::Acquire) {
            exit_after_allocation_failure();
        }
        pointer
    }
}

unsafe impl GlobalAlloc for ModeSwitchAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        Self::refused(unsafe { System.alloc(layout) })
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        Self::refused(unsafe { System.alloc_zeroed(layout) })
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        unsafe { System.dealloc(pointer, layout) }
    }

    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        Self::refused(unsafe { System.realloc(pointer, layout, new_size) })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::open_limits::HELPER_MEMORY_LIMIT;
    use std::ffi::OsString;
    use windows::Win32::System::Diagnostics::Debug::RaiseException;
    use windows::Win32::System::Threading::{OpenProcess, PROCESS_ACCESS_RIGHTS, PROCESS_QUERY_LIMITED_INFORMATION};

    const LABEL: &str = "test child";
    const MODE_PREFIX: &str = "job-child-mode=";
    const ENTRY: &str = "child_job::tests::job_child_entry";
    const TEST_CAP: usize = 64 * 1024 * 1024;
    const TEST_EXCEPTION_CODE: u32 = 0xe000_5a5a;
    const EXCEPTION_NONCONTINUABLE: u32 = 1;

    /// Child side. Runs only when this test binary is started by a test below with a mode argument; in a
    /// normal run it returns at once.
    #[test]
    fn job_child_entry() {
        let Some(mode) = std::env::args().find_map(|argument| argument.strip_prefix(MODE_PREFIX).map(str::to_owned)) else {
            return;
        };
        let exit = |code: i32| -> ! {
            let _ = std::io::stdout().flush();
            std::process::exit(code)
        };
        if mode == "host" {
            let running = spawn_test_child("sleep", JobLimits::hardened_single_process(TEST_CAP));
            print!("child-pid={}\n", running.child.process_id());
            exit(0);
        }
        harden_job_child_process();
        match mode.as_str() {
            "exit-3" => exit(3),
            "echo" => {
                let mut input = Vec::new();
                std::io::stdin().read_to_end(&mut input).unwrap();
                print!("echo:{}\n", String::from_utf8_lossy(&input));
                exit(0);
            }
            "flood-stdout" => {
                let chunk = vec![b'a'; 64 * 1024];
                for _ in 0..32 {
                    if std::io::stdout().write_all(&chunk).is_err() {
                        exit(0);
                    }
                }
                exit(0);
            }
            "sleep" => {
                std::thread::sleep(Duration::from_secs(30));
                exit(0);
            }
            "refused-then-wait" => {
                let mut memory = Vec::<u8>::new();
                if memory.try_reserve_exact(512 * 1024 * 1024).is_ok() {
                    exit(91);
                }
                print!("refused\n");
                let _ = std::io::stdout().flush();
                std::thread::sleep(Duration::from_secs(30));
                exit(0);
            }
            "allocate-until-refused" => {
                let layout = Layout::from_size_align(16 * 1024 * 1024, 4096).unwrap();
                for _ in 0..64 {
                    let pointer = unsafe { TerminatingAllocator.alloc(layout) };
                    std::hint::black_box(pointer);
                }
                exit(90);
            }
            "raise" => {
                unsafe { RaiseException(TEST_EXCEPTION_CODE, EXCEPTION_NONCONTINUABLE, None) };
                exit(92);
            }
            "spawn" => {
                if std::process::Command::new(std::env::current_exe().unwrap()).arg("--list").spawn().is_ok() {
                    exit(93);
                }
                std::thread::sleep(Duration::from_secs(30));
                exit(0);
            }
            other => {
                eprint!("unknown mode {other}\n");
                exit(94);
            }
        }
    }

    fn test_command(mode: &str) -> ChildCommand {
        let executable = std::env::current_exe().unwrap();
        let mode = OsString::from(format!("{MODE_PREFIX}{mode}"));
        let arguments: [&OsStr; 7] = [
            executable.as_os_str(),
            OsStr::new("--exact"),
            OsStr::new(ENTRY),
            OsStr::new("--nocapture"),
            OsStr::new("--quiet"),
            OsStr::new("--test-threads=1"),
            &mode,
        ];
        ChildCommand {
            application: wide_null(executable.as_os_str()).unwrap(),
            command_line: command_line(&arguments).unwrap(),
            current_directory: wide_null(executable.parent().unwrap().as_os_str()).unwrap(),
        }
    }

    fn spawn_test_child(mode: &str, limits: JobLimits) -> RunningChild {
        spawn_suspended::<ChildSystemError>(LABEL, &limits, || Ok(test_command(mode)), || Ok(()))
            .unwrap()
            .resume()
            .unwrap()
    }

    fn bounds(timeout: Duration) -> OutputBounds {
        OutputBounds {
            stdout_limit: 1024 * 1024,
            stderr_limit: 64 * 1024,
            timeout,
            poll_interval: Duration::from_millis(10),
            drain_grace: PIPE_DRAIN_GRACE,
        }
    }

    /// Replaces one of the child's parent-side pipe ends with a fresh pipe whose other end the test keeps,
    /// as if another process had inherited it: that pipe never closes when the child exits. Also returns
    /// the replaced end, which the test keeps open so the child's own writes still succeed.
    fn leak_pipe(running: &mut RunningChild, which: &str) -> (OwnedHandle, OwnedHandle) {
        let (read, write) = create_pipe(LABEL, false).unwrap();
        match which {
            "stdout" => (write, std::mem::replace(&mut running.pipes.stdout, read)),
            "stdin" => (read, std::mem::replace(&mut running.pipes.stdin, write)),
            other => panic!("unknown pipe {other}"),
        }
    }

    fn run(mode: &str, limits: JobLimits, input: &[u8], bounds: OutputBounds) -> ChildRun {
        run_bounded(spawn_test_child(mode, limits), input, &bounds, &|| false).unwrap()
    }

    fn process_is_alive(pid: u32) -> bool {
        let access = PROCESS_ACCESS_RIGHTS(PROCESS_QUERY_LIMITED_INFORMATION.0 | 0x0010_0000);
        let Ok(process) = (unsafe { OpenProcess(access, false, pid) }) else { return false };
        let handle = unsafe { owned_handle(process) };
        (unsafe { WaitForSingleObject(as_windows_handle(&handle), 0) }) == WAIT_TIMEOUT
    }

    fn assert_gone_within(pid: u32, limit: Duration) {
        let started = Instant::now();
        while started.elapsed() < limit {
            if !process_is_alive(pid) {
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("child {pid} is still alive after {limit:?}");
    }

    #[test]
    fn exit_codes_and_stdin_reach_the_parent() {
        let echoed = run("echo", JobLimits::single_process(TEST_CAP), b"hello child", bounds(Duration::from_secs(30)));
        assert_eq!(echoed.reason, TerminalReason::Exited);
        assert_eq!(echoed.child.exit_code().unwrap(), 0);
        assert!(String::from_utf8_lossy(&echoed.stdout).contains("echo:hello child\n"));
        assert_eq!(echoed.writer_result, Ok(()));
        assert_eq!(echoed.limit_violation, None);
        let peak = echoed.child.peak_job_commit().unwrap();
        assert!(peak > 0 && peak < TEST_CAP, "test child peak {peak}");

        let three = run("exit-3", JobLimits::hardened_single_process(TEST_CAP), b"", bounds(Duration::from_secs(30)));
        assert_eq!(three.reason, TerminalReason::Exited);
        assert_eq!(three.child.exit_code().unwrap(), 3);
        assert_eq!(classify_exit_code(3), ExitKind::Code(3));
        assert_eq!(three.limit_violation, None);
    }

    #[test]
    fn a_timeout_ends_and_reaps_the_child() {
        let started = Instant::now();
        let timed = run("sleep", JobLimits::single_process(TEST_CAP), b"", bounds(Duration::from_millis(300)));
        assert_eq!(timed.reason, TerminalReason::TimedOut);
        assert!(started.elapsed() < Duration::from_secs(10), "{:?}", started.elapsed());
        assert_eq!(timed.child.exit_code().unwrap(), EXIT_RESOURCE_LIMIT);
        assert!(!process_is_alive(timed.child.process_id()));
    }

    #[test]
    fn cancellation_ends_the_child_with_the_cancel_code() {
        let started = Instant::now();
        let cancelled = run_bounded(
            spawn_test_child("sleep", JobLimits::single_process(TEST_CAP)),
            b"",
            &bounds(Duration::from_secs(30)),
            &|| started.elapsed() > Duration::from_millis(200),
        )
        .unwrap();
        assert_eq!(cancelled.reason, TerminalReason::Cancelled);
        assert_eq!(cancelled.child.exit_code().unwrap(), EXIT_CANCELLED);
    }

    #[test]
    fn stdout_is_capped_exactly_and_the_child_is_ended() {
        let cap = 64 * 1024;
        let flooded = run(
            "flood-stdout",
            JobLimits::single_process(TEST_CAP),
            b"",
            OutputBounds { stdout_limit: cap, ..bounds(Duration::from_secs(30)) },
        );
        assert!(flooded.stdout_exceeded);
        assert_eq!(flooded.stdout.len(), cap);
        assert!(matches!(flooded.reason, TerminalReason::StdoutLimit | TerminalReason::Exited), "{:?}", flooded.reason);
        assert!(!process_is_alive(flooded.child.process_id()));
    }

    #[test]
    fn a_memory_limit_violation_is_reported_and_ends_the_child() {
        let started = Instant::now();
        let refused = run("refused-then-wait", JobLimits::hardened_single_process(TEST_CAP), b"", bounds(Duration::from_secs(30)));
        assert!(
            matches!(refused.reason, TerminalReason::LimitViolation(LimitViolation::ProcessMemory | LimitViolation::JobMemory)),
            "{:?} stdout {:?}",
            refused.reason,
            String::from_utf8_lossy(&refused.stdout)
        );
        assert!(started.elapsed() < Duration::from_secs(10), "{:?}", started.elapsed());
        assert!(matches!(refused.limit_violation, Some(LimitViolation::ProcessMemory | LimitViolation::JobMemory)));
        assert_eq!(refused.child.exit_code().unwrap(), EXIT_RESOURCE_LIMIT);
        assert!(refused.child.peak_job_commit().unwrap() <= TEST_CAP);
    }

    #[test]
    fn a_refused_allocation_exits_with_the_allocation_failure_code() {
        let limits = JobLimits { limit_notifications: false, ..JobLimits::hardened_single_process(TEST_CAP) };
        let refused = run("allocate-until-refused", limits, b"", bounds(Duration::from_secs(30)));
        assert_eq!(refused.reason, TerminalReason::Exited);
        let code = refused.child.exit_code().unwrap();
        assert_eq!(code, EXIT_ALLOCATION_FAILURE, "exit code {code:#x}");
        assert_eq!(classify_exit_code(code), ExitKind::AllocationFailure);
        assert_eq!(refused.limit_violation, None);

        let reported = run("allocate-until-refused", JobLimits::hardened_single_process(TEST_CAP), b"", bounds(Duration::from_secs(30)));
        assert!(matches!(reported.limit_violation, Some(LimitViolation::ProcessMemory | LimitViolation::JobMemory)));
        assert!(matches!(reported.child.exit_code().unwrap(), EXIT_ALLOCATION_FAILURE | EXIT_RESOURCE_LIMIT));
    }

    #[test]
    fn the_exception_filter_runs_and_exits_with_the_exception_code() {
        let raised = run("raise", JobLimits::hardened_single_process(TEST_CAP), b"", bounds(Duration::from_secs(30)));
        assert_eq!(raised.reason, TerminalReason::Exited);
        let code = raised.child.exit_code().unwrap();
        assert_eq!(code, TEST_EXCEPTION_CODE, "exit code {code:#x}");
        assert_eq!(classify_exit_code(code), ExitKind::Exception(TEST_EXCEPTION_CODE));
        assert!(
            raised.stderr.ends_with(UNHANDLED_EXCEPTION_MARKER),
            "the filter's marker is missing, so it did not run: {:?}",
            String::from_utf8_lossy(&raised.stderr)
        );
    }

    #[test]
    fn a_leaked_stdout_write_end_cannot_hold_run_bounded_after_exit() {
        let mut running = spawn_test_child("exit-3", JobLimits::single_process(TEST_CAP));
        let _leaked = leak_pipe(&mut running, "stdout");
        let started = Instant::now();
        let finished = run_bounded(
            running,
            b"",
            &OutputBounds { drain_grace: Duration::from_millis(200), ..bounds(Duration::from_secs(30)) },
            &|| false,
        )
        .unwrap();
        assert!(started.elapsed() < Duration::from_secs(10), "{:?}", started.elapsed());
        assert_eq!(finished.reason, TerminalReason::Exited);
        assert!(finished.io_cancelled);
        assert!(!process_is_alive(finished.child.process_id()));
    }

    #[test]
    fn a_leaked_stdout_write_end_cannot_hold_run_bounded_past_the_timeout() {
        let mut running = spawn_test_child("sleep", JobLimits::single_process(TEST_CAP));
        let _leaked = leak_pipe(&mut running, "stdout");
        let started = Instant::now();
        let timed = run_bounded(
            running,
            b"",
            &OutputBounds { drain_grace: Duration::from_millis(200), ..bounds(Duration::from_millis(300)) },
            &|| false,
        )
        .unwrap();
        assert!(started.elapsed() < Duration::from_secs(5), "{:?}", started.elapsed());
        assert_eq!(timed.reason, TerminalReason::TimedOut);
        assert!(timed.io_cancelled);
        assert_eq!(timed.child.exit_code().unwrap(), EXIT_RESOURCE_LIMIT);
    }

    #[test]
    fn a_leaked_stdin_read_end_cannot_block_the_input_writer() {
        let mut running = spawn_test_child("exit-3", JobLimits::single_process(TEST_CAP));
        let _leaked = leak_pipe(&mut running, "stdin");
        let input = vec![b'x'; 4 * 1024 * 1024];
        let started = Instant::now();
        let finished = run_bounded(
            running,
            &input,
            &OutputBounds { drain_grace: Duration::from_millis(200), ..bounds(Duration::from_secs(30)) },
            &|| false,
        )
        .unwrap();
        assert!(started.elapsed() < Duration::from_secs(10), "{:?}", started.elapsed());
        assert!(finished.io_cancelled);
        assert_eq!(finished.writer_result, Ok(()));
    }

    #[test]
    fn a_worker_without_a_thread_handle_does_no_blocking_io() {
        // Both pipes keep their other end open here, so a read or a large write would block forever;
        // without a handle to cancel it with, the worker must not start the I/O at all.
        let (read, held_write) = create_pipe(LABEL, false).unwrap();
        let reader = IoWorker::default();
        reader.arm(None);
        let exceeded = AtomicBool::new(false);
        let started = Instant::now();
        assert_eq!(read_bounded(LABEL, read, 1024, &exceeded, &reader).unwrap(), Vec::<u8>::new());
        assert!(reader.cancelled_io.load(Ordering::Acquire));
        let (held_read, write) = create_pipe(LABEL, true).unwrap();
        let writer = IoWorker::default();
        writer.arm(None);
        assert_eq!(write_input(LABEL, write, &vec![b'x'; 4 * 1024 * 1024], &writer), Ok(()));
        assert!(writer.cancelled_io.load(Ordering::Acquire));
        assert!(started.elapsed() < Duration::from_secs(5), "{:?}", started.elapsed());
        drop((held_write, held_read));
    }

    #[test]
    fn a_second_process_is_refused_and_reported() {
        let spawned = run("spawn", JobLimits::hardened_single_process(TEST_CAP), b"", bounds(Duration::from_secs(30)));
        assert_eq!(spawned.reason, TerminalReason::LimitViolation(LimitViolation::ActiveProcess));
    }

    #[test]
    fn dropping_a_running_child_ends_it() {
        let running = spawn_test_child("sleep", JobLimits::single_process(TEST_CAP));
        let pid = running.child.process_id();
        assert!(process_is_alive(pid));
        drop(running);
        assert!(!process_is_alive(pid));
    }

    #[test]
    fn the_child_dies_when_its_owner_exits_without_cleanup() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", ENTRY, "--nocapture", "--quiet", "--test-threads=1", &format!("{MODE_PREFIX}host")])
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        let stdout = String::from_utf8_lossy(&output.stdout);
        let pid: u32 = stdout
            .lines()
            .find_map(|line| line.strip_prefix("child-pid="))
            .unwrap_or_else(|| panic!("host printed no child pid: {stdout:?}"))
            .trim()
            .parse()
            .unwrap();
        assert_gone_within(pid, Duration::from_secs(2));
    }

    #[test]
    fn the_helper_limits_are_applied_to_the_job() {
        let (job, port) = create_job(LABEL, &JobLimits::hardened_single_process(HELPER_MEMORY_LIMIT)).unwrap();
        assert!(port.is_some());
        let applied = query_extended_limits(&job, LABEL).unwrap();
        assert_eq!(HELPER_MEMORY_LIMIT, 1024 * 1024 * 1024);
        assert_eq!(applied.ProcessMemoryLimit, HELPER_MEMORY_LIMIT);
        assert_eq!(applied.JobMemoryLimit, HELPER_MEMORY_LIMIT);
        assert_eq!(applied.BasicLimitInformation.ActiveProcessLimit, 1);
        let flags = applied.BasicLimitInformation.LimitFlags;
        for flag in [
            JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
            JOB_OBJECT_LIMIT_ACTIVE_PROCESS,
            JOB_OBJECT_LIMIT_PROCESS_MEMORY,
            JOB_OBJECT_LIMIT_JOB_MEMORY,
            JOB_OBJECT_LIMIT_DIE_ON_UNHANDLED_EXCEPTION,
        ] {
            assert_eq!(flags & flag, flag);
        }

        let (plain, none) = create_job(LABEL, &JobLimits::single_process(TEST_CAP)).unwrap();
        assert!(none.is_none());
        let plain = query_extended_limits(&plain, LABEL).unwrap().BasicLimitInformation.LimitFlags;
        assert_eq!(
            plain,
            JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE | JOB_OBJECT_LIMIT_ACTIVE_PROCESS | JOB_OBJECT_LIMIT_PROCESS_MEMORY | JOB_OBJECT_LIMIT_JOB_MEMORY
        );
    }

    #[test]
    fn exit_codes_are_classified_and_arguments_quoted() {
        assert_eq!(classify_exit_code(0), ExitKind::Success);
        assert_eq!(classify_exit_code(EXIT_CANCELLED), ExitKind::Cancelled);
        assert_eq!(classify_exit_code(EXIT_RESOURCE_LIMIT), ExitKind::EndedForResourceLimit);
        assert_eq!(classify_exit_code(0xc000_0005), ExitKind::Exception(0xc000_0005));
        assert_eq!(classify_exit_code(77), ExitKind::Code(77));
        assert_eq!(classify_exit_code(0x8000_0003), ExitKind::Exception(0x8000_0003));
        let line = command_line(&[OsStr::new(r"C:\a b\x.exe"), OsStr::new(r#"say "hi"\"#)]).unwrap();
        assert_eq!(String::from_utf16(&line[..line.len() - 1]).unwrap(), r#""C:\a b\x.exe" "say \"hi\"\\""#);
        assert_eq!(command_line(&[OsStr::new("a\0b")]), Err(ContainsNul));
        assert_eq!(wide_null(OsStr::new("a\0b")), Err(ContainsNul));
    }
}
