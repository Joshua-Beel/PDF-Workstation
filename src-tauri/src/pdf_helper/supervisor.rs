//! Supervises the PDF-engine helper process from the app side.
//!
//! The helper runs in its own Job Object (`JobLimits::hardened_single_process`, memory cap
//! `HELPER_MEMORY_LIMIT`, completion port on). One request is in flight at a time. Every reply is accepted
//! only after `JobChild::limit_violation()` reports nothing: a memory-capped PDFium does not crash, it
//! renders blank pages, and Windows reports the limit once per process, so a helper that has hit its limit
//! is discarded together with the reply. A helper that exits, crashes, misses the request's deadline,
//! sends a malformed or oversized frame, or whose pipe I/O had to be cancelled is ended and the request
//! fails. The next request starts a new helper, after a backoff, until the crash-loop cap stops restarts.

use std::collections::VecDeque;
use std::fs::File;
use std::io::{ErrorKind, Read, Write};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, SyncSender, TryRecvError};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use super::protocol::*;
use crate::child_job::{
    classify_exit_code, command_line, finish_io, spawn_suspended, wide_null, ChildCommand, ChildSystemError, ExitKind, IoWorker,
    JobChild, JobLimits, LimitViolation, RunningChild, EXIT_CANCELLED, EXIT_RESOURCE_LIMIT,
};

pub(crate) const HELPER_LABEL: &str = "PDF engine helper";
/// The command-line flag that starts the exe in helper mode (it must be the only argument).
pub(crate) const HELPER_MODE_FLAG: &str = "--pdf-engine-helper";
const STDERR_TAIL_BYTES: usize = 64 * 1024;
/// How long a helper whose pipe closed gets to finish exiting before it is ended.
const EXIT_WAIT: Duration = Duration::from_secs(2);
/// How often a pending request checks whether the helper process has exited.
const EXIT_POLL: Duration = Duration::from_millis(20);
/// How long an exited helper's last frames get to arrive before the request fails.
const EXITED_DRAIN: Duration = Duration::from_millis(30);
/// Pipe threads of a discarded (already ended) helper get this long before their blocked I/O is cancelled;
/// nothing they could still read is used.
const DISCARD_DRAIN_GRACE: Duration = Duration::from_millis(20);

/// Per-request deadlines. A request that misses its deadline ends the helper.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Deadlines {
    /// Process start, hello and PDFium bind.
    pub(crate) startup: Duration,
    pub(crate) load: Duration,
    /// Viewer, BGRA and thumbnail renders.
    pub(crate) render: Duration,
    /// Everything else: sizes, text, geometry, bookmarks, properties, forms.
    pub(crate) read: Duration,
    pub(crate) close: Duration,
}

impl Deadlines {
    /// Step-0 measured the slowest real 3000-px page at 538 ms (release); these leave wide headroom for
    /// debug builds and loaded machines.
    pub(crate) const PRODUCTION: Self = Self {
        startup: Duration::from_secs(20),
        load: Duration::from_secs(60),
        render: Duration::from_secs(20),
        read: Duration::from_secs(10),
        close: Duration::from_secs(5),
    };
}

/// When a dead helper may be replaced.
#[derive(Clone, Copy, Debug)]
pub(crate) struct RestartPolicy {
    /// Restarts counted toward the cap.
    pub(crate) window: Duration,
    /// Restarts allowed within `window`; the next one stops the engine until `reset_after_stop`.
    pub(crate) max_restarts: usize,
    /// The first restart in a window is immediate; later ones wait `base`, `2 × base`, ... up to `max`.
    pub(crate) backoff_base: Duration,
    pub(crate) backoff_max: Duration,
}

impl RestartPolicy {
    pub(crate) const PRODUCTION: Self = Self {
        window: Duration::from_secs(60),
        max_restarts: 3,
        backoff_base: Duration::from_millis(100),
        backoff_max: Duration::from_secs(2),
    };

    pub(crate) fn backoff(&self, recent_restarts: usize) -> Duration {
        if recent_restarts == 0 {
            return Duration::ZERO;
        }
        let factor = 1_u32.checked_shl(u32::try_from(recent_restarts - 1).unwrap_or(31)).unwrap_or(u32::MAX);
        self.backoff_base.saturating_mul(factor).min(self.backoff_max)
    }
}

/// How the helper process is started.
#[derive(Clone, Debug)]
pub(crate) enum HelperLaunch {
    /// This exe with `--pdf-engine-helper`.
    #[cfg_attr(test, allow(dead_code))]
    ThisExe,
    /// The test binary re-run as a helper (its test harness prints a banner before the helper starts).
    #[cfg(test)]
    TestEntry { mode: String },
}

impl HelperLaunch {
    fn command(&self) -> Result<ChildCommand, ChildSystemError> {
        match self {
            Self::ThisExe => {
                let executable = std::env::current_exe().map_err(|error| ChildSystemError(format!("Could not locate the {HELPER_LABEL}: {error}")))?;
                let directory = executable.parent().ok_or_else(|| ChildSystemError(format!("Could not locate the {HELPER_LABEL} folder")))?;
                let invalid = |_| ChildSystemError(format!("The {HELPER_LABEL} path contains a NUL character"));
                Ok(ChildCommand {
                    application: wide_null(executable.as_os_str()).map_err(invalid)?,
                    command_line: command_line(&[executable.as_os_str(), HELPER_MODE_FLAG.as_ref()]).map_err(invalid)?,
                    current_directory: wide_null(directory.as_os_str()).map_err(invalid)?,
                })
            }
            #[cfg(test)]
            Self::TestEntry { mode } => Ok(super::tests::test_entry_command(mode)),
        }
    }

    /// Bytes the helper may write before [`SYNC`]: none in the app.
    fn banner_allowance(&self) -> usize {
        match self {
            Self::ThisExe => 0,
            #[cfg(test)]
            Self::TestEntry { .. } => 4096,
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct HelperConfig {
    pub(crate) launch: HelperLaunch,
    pub(crate) limits: JobLimits,
    pub(crate) deadlines: Deadlines,
    pub(crate) restart: RestartPolicy,
}

impl HelperConfig {
    /// The app's helper: this exe, hardened job, 1 GiB cap, production deadlines and restart policy. In
    /// the test binary the helper is the test entry instead (the harness cannot take the mode flag).
    pub(crate) fn production() -> Self {
        Self {
            #[cfg(not(test))]
            launch: HelperLaunch::ThisExe,
            #[cfg(test)]
            launch: HelperLaunch::TestEntry { mode: "serve".into() },
            limits: JobLimits::hardened_single_process(crate::open_limits::HELPER_MEMORY_LIMIT),
            deadlines: Deadlines::PRODUCTION,
            restart: RestartPolicy::PRODUCTION,
        }
    }
}

/// Why the helper could not answer.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum HelperFailure {
    /// The helper could not be started or did not complete its start-up.
    Start(String),
    /// The helper exited (or was found exited) while a request was in flight.
    Exited(Option<ExitKind>),
    /// The request missed its deadline; the helper was ended.
    TimedOut,
    /// The job reported a memory or process limit; the helper and its reply were discarded.
    LimitViolation(LimitViolation),
    /// The helper broke the protocol; it was ended.
    Malformed(String),
    /// Too many restarts in the window; no helper is started until `reset_after_stop`.
    Stopped,
}

impl HelperFailure {
    /// Whether this failure ended a helper while it worked on a request (the request's document is then suspect).
    pub(crate) fn ended_helper(&self) -> bool {
        !matches!(self, Self::Start(_) | Self::Stopped)
    }

    pub(crate) fn message(&self) -> String {
        match self {
            Self::Start(reason) => format!("The PDF engine could not start: {reason}"),
            Self::Exited(_) => "The PDF engine stopped unexpectedly while working on this file.".into(),
            Self::TimedOut => "The PDF engine did not finish in time and was restarted.".into(),
            Self::LimitViolation(_) => "This file needs more memory than the PDF engine is allowed to use.".into(),
            Self::Malformed(_) => "The PDF engine sent an invalid reply and was restarted.".into(),
            Self::Stopped => "The PDF engine stopped repeatedly. Try again to restart it.".into(),
        }
    }
}

/// A failed call: the helper failed, the engine reported an error (class and message), or the document
/// cannot be used until the user retries.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum CallError {
    Failure(HelperFailure),
    Engine(u8, String),
    Unavailable(&'static str),
}

impl CallError {
    pub(crate) fn message(self) -> String {
        match self {
            Self::Failure(failure) => failure.message(),
            Self::Engine(_, message) => message,
            Self::Unavailable(message) => message.into(),
        }
    }
}

enum Event {
    Frame(Vec<u8>),
    Closed,
    Truncated,
    BadLength(u64),
    NoSync,
    ReadFailed,
    WriteFailed,
}

/// One running helper process and its pipe threads.
struct Helper {
    child: JobChild,
    requests: Option<Sender<Vec<u8>>>,
    events: Receiver<Event>,
    reply_limit: Arc<AtomicUsize>,
    workers: [Arc<IoWorker>; 3],
    threads: Vec<JoinHandle<()>>,
    stderr: Arc<Mutex<Vec<u8>>>,
}

/// What was left of a discarded helper.
struct Discarded {
    /// How the helper exited, if it exited on its own; `None` when the app ended it.
    exit: Option<ExitKind>,
    io_cancelled: bool,
    stderr: Vec<u8>,
}

impl Helper {
    fn spawn(config: &HelperConfig) -> Result<Self, HelperFailure> {
        let start = |error: ChildSystemError| HelperFailure::Start(error.0);
        let suspended = spawn_suspended::<ChildSystemError>(HELPER_LABEL, &config.limits, || config.launch.command(), || Ok(())).map_err(start)?;
        let RunningChild { child, pipes } = suspended.resume().map_err(start)?;
        let reply_limit = Arc::new(AtomicUsize::new(MAX_CONTROL_REPLY));
        let (event_sender, events) = mpsc::sync_channel(1);
        let (requests, request_receiver) = mpsc::channel::<Vec<u8>>();
        let workers: [Arc<IoWorker>; 3] = Default::default();
        let stderr = Arc::new(Mutex::new(Vec::new()));
        let mut threads = Vec::with_capacity(3);
        let spawn_thread = |name: &str, work: Box<dyn FnOnce() + Send>| {
            std::thread::Builder::new().name(name.into()).spawn(work).map_err(|error| HelperFailure::Start(format!("Could not start a {HELPER_LABEL} pipe thread: {error}")))
        };
        let (worker, sender, stdin) = (workers[0].clone(), event_sender.clone(), File::from(pipes.stdin));
        threads.push(spawn_thread("pdf-helper-in", Box::new(move || worker.run(|io| write_requests(io, stdin, request_receiver, sender))))?);
        let (worker, sender, stdout, limit, allowance) = (workers[1].clone(), event_sender, File::from(pipes.stdout), reply_limit.clone(), config.launch.banner_allowance());
        threads.push(spawn_thread("pdf-helper-out", Box::new(move || worker.run(|io| read_replies(io, stdout, allowance, limit, sender))))?);
        let (worker, tail, stderr_pipe) = (workers[2].clone(), stderr.clone(), File::from(pipes.stderr));
        threads.push(spawn_thread("pdf-helper-err", Box::new(move || worker.run(|io| read_stderr(io, stderr_pipe, tail))))?);
        Ok(Self { child, requests: Some(requests), events, reply_limit, workers, threads, stderr })
    }

    /// Waits for the hello frame, which must carry the helper's own process id.
    fn hello(&mut self, deadline: Instant) -> Result<(), HelperFailure> {
        let body = self.next_frame(deadline)?;
        let (header, mut reader) = parse_body(&body).map_err(|error| HelperFailure::Malformed(error.0))?;
        let pid = reader.u32().map_err(|error| HelperFailure::Malformed(error.0))?;
        reader.finish().map_err(|error| HelperFailure::Malformed(error.0))?;
        if header != (Header { kind: kind::HELLO, sequence: 0 }) || pid != self.child.process_id() {
            return Err(HelperFailure::Malformed("the helper's hello does not match the process".into()));
        }
        self.reply_limit.store(0, Ordering::Release);
        Ok(())
    }

    /// Sends one request frame and returns the matching reply body (its kind or `ERROR`, same sequence).
    ///
    /// The reader takes the reply limit with `swap(0)` when a length prefix arrives, so each request admits
    /// exactly one frame: any further frame (written while nothing is pending, or a second one for this
    /// request) meets a limit of 0 and is refused before anything is allocated. At most one event waits in
    /// the bounded queue; the next exchange finds it and ends the helper. A stray frame whose prefix arrives
    /// only after the next request set its limit is taken as that reply and fails the sequence check.
    fn exchange(&mut self, frame: Vec<u8>, request: Header, reply_limit: usize, deadline: Instant) -> Result<(Header, Vec<u8>), HelperFailure> {
        match self.events.try_recv() {
            Ok(Event::Frame(_) | Event::BadLength(_) | Event::NoSync) => {
                return Err(HelperFailure::Malformed("the helper wrote while no request was pending".into()));
            }
            Ok(event) => return Err(self.ended(event)),
            Err(TryRecvError::Empty | TryRecvError::Disconnected) => {}
        }
        self.reply_limit.store(reply_limit, Ordering::Release);
        if let Some(requests) = &self.requests {
            let _ = requests.send(frame);
        }
        let reply = self.next_frame(deadline);
        self.reply_limit.store(0, Ordering::Release);
        let body = reply?;
        let (header, _) = parse_body(&body).map_err(|error| HelperFailure::Malformed(error.0))?;
        if header.sequence != request.sequence {
            return Err(HelperFailure::Malformed(format!("reply to request {} while {} was in flight", header.sequence, request.sequence)));
        }
        if header.kind != request.kind && header.kind != kind::ERROR {
            return Err(HelperFailure::Malformed(format!("reply of kind {} to a request of kind {}", header.kind, request.kind)));
        }
        Ok((header, body))
    }

    /// The next frame, accepted only if the job has reported no limit violation.
    /// The process is watched as well as the pipe: another process that inherited a pipe end (any child the
    /// app starts with handle inheritance while the helper's ends were briefly inheritable) keeps the pipe
    /// open after the helper is gone, so an exit is detected from the process handle within `EXIT_POLL`.
    fn next_frame(&mut self, deadline: Instant) -> Result<Vec<u8>, HelperFailure> {
        let mut exited_at: Option<Instant> = None;
        let event = loop {
            let now = Instant::now();
            if now >= deadline {
                return Err(self.child.limit_violation().map_or(HelperFailure::TimedOut, HelperFailure::LimitViolation));
            }
            match self.events.recv_timeout(EXIT_POLL.min(deadline - now)) {
                Ok(event) => break event,
                Err(RecvTimeoutError::Disconnected) => break Event::Closed,
                Err(RecvTimeoutError::Timeout) => {}
            }
            match exited_at {
                Some(at) if at.elapsed() >= EXITED_DRAIN => break Event::Closed,
                Some(_) => {}
                None if self.child.wait_for_exit(Duration::ZERO) => exited_at = Some(Instant::now()),
                None => {}
            }
        };
        match event {
            Event::Frame(body) => match self.child.limit_violation() {
                Some(violation) => Err(HelperFailure::LimitViolation(violation)),
                None => Ok(body),
            },
            Event::BadLength(length) => Err(self.child.limit_violation().map_or_else(
                || HelperFailure::Malformed(format!("a frame of {length} bytes is outside the size this request allows")),
                HelperFailure::LimitViolation,
            )),
            Event::NoSync => Err(HelperFailure::Malformed("the helper did not start with the protocol marker".into())),
            Event::Closed | Event::Truncated | Event::ReadFailed | Event::WriteFailed => Err(self.ended(event)),
        }
    }

    /// The failure for a helper whose pipe closed or failed: a limit it hit, else how it exited.
    fn ended(&mut self, event: Event) -> HelperFailure {
        let exited = self.child.wait_for_exit(EXIT_WAIT);
        if let Some(violation) = self.child.limit_violation() {
            return HelperFailure::LimitViolation(violation);
        }
        if exited {
            return HelperFailure::Exited(self.child.exit_code().ok().map(classify_exit_code));
        }
        HelperFailure::Malformed(match event {
            Event::Truncated => "the helper's reply ended inside a frame".into(),
            _ => "the helper closed its pipes but kept running".into(),
        })
    }

    /// Ends the helper (whole job), then its pipe threads: they get `DISCARD_DRAIN_GRACE` to finish before
    /// their blocked I/O is cancelled (recorded as `io_cancelled`). Threads that still have not finished are
    /// left detached.
    fn discard(mut self, exit_code: u32) -> Discarded {
        let exited_itself = self.child.wait_for_exit(Duration::ZERO);
        let _ = self.child.terminate_job_and_reap(exit_code);
        let exit = if exited_itself { self.child.exit_code().ok().map(classify_exit_code) } else { None };
        self.requests = None;
        drop(std::mem::replace(&mut self.events, mpsc::sync_channel(0).1));
        let workers = [&*self.workers[0], &*self.workers[1], &*self.workers[2]];
        finish_io(&workers, DISCARD_DRAIN_GRACE);
        let io_cancelled = workers.iter().any(|worker| worker.was_cancelled());
        for thread in self.threads.drain(..) {
            if thread.is_finished() {
                let _ = thread.join();
            }
        }
        let stderr = std::mem::take(&mut *self.stderr.lock().unwrap_or_else(|poisoned| poisoned.into_inner()));
        Discarded { exit, io_cancelled, stderr }
    }
}

fn write_requests(io: &IoWorker, mut stdin: File, requests: Receiver<Vec<u8>>, events: SyncSender<Event>) {
    for frame in requests {
        if io.should_stop() {
            io.mark_cancelled();
            return;
        }
        if stdin.write_all(&frame).and_then(|()| stdin.flush()).is_err() {
            if io.should_stop() {
                io.mark_cancelled();
            }
            let _ = events.send(Event::WriteFailed);
            return;
        }
    }
}

fn read_replies(io: &IoWorker, mut stdout: File, banner_allowance: usize, limit: Arc<AtomicUsize>, events: SyncSender<Event>) {
    let finish = |event: Event| {
        if io.should_stop() {
            io.mark_cancelled();
        }
        let _ = events.send(event);
    };
    if let Err(event) = read_sync(&mut stdout, banner_allowance) {
        return finish(event);
    }
    loop {
        if io.should_stop() {
            io.mark_cancelled();
            return;
        }
        match read_frame(&mut stdout, &|| limit.swap(0, Ordering::AcqRel)) {
            Ok(body) => {
                if events.send(Event::Frame(body)).is_err() {
                    return;
                }
            }
            Err(error) => {
                return finish(match error {
                    FrameError::Closed => Event::Closed,
                    FrameError::Truncated => Event::Truncated,
                    FrameError::BadLength(length) => Event::BadLength(length),
                    FrameError::Io => Event::ReadFailed,
                })
            }
        }
    }
}

/// Reads up to and including [`SYNC`], allowing at most `allowance` bytes before it.
fn read_sync(input: &mut impl Read, allowance: usize) -> Result<(), Event> {
    let mut window = [0_u8; SYNC.len()];
    let mut seen = 0_usize;
    loop {
        let mut byte = [0_u8; 1];
        match input.read(&mut byte) {
            Ok(0) => return Err(Event::Closed),
            Ok(_) => {}
            Err(error) if error.kind() == ErrorKind::Interrupted => continue,
            Err(error) if error.kind() == ErrorKind::BrokenPipe => return Err(Event::Closed),
            Err(_) => return Err(Event::ReadFailed),
        }
        window.rotate_left(1);
        window[SYNC.len() - 1] = byte[0];
        seen += 1;
        if seen >= SYNC.len() && window == SYNC {
            return Ok(());
        }
        if seen >= SYNC.len() + allowance {
            return Err(Event::NoSync);
        }
    }
}

/// Keeps the last `STDERR_TAIL_BYTES` the helper writes to stderr (diagnostics only, never shown).
fn read_stderr(io: &IoWorker, mut stderr: File, tail: Arc<Mutex<Vec<u8>>>) {
    let mut buffer = [0_u8; 4096];
    loop {
        if io.should_stop() {
            io.mark_cancelled();
            return;
        }
        match stderr.read(&mut buffer) {
            Ok(0) => return,
            Ok(count) => {
                let mut kept = tail.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
                kept.extend_from_slice(&buffer[..count]);
                if kept.len() > STDERR_TAIL_BYTES {
                    let excess = kept.len() - STDERR_TAIL_BYTES;
                    kept.drain(..excess);
                }
            }
            Err(error) if error.kind() == ErrorKind::Interrupted => {}
            Err(_) => {
                if io.should_stop() {
                    io.mark_cancelled();
                }
                return;
            }
        }
    }
}

/// Counters and the last failure, for tests and diagnostics.
#[derive(Clone, Debug, Default)]
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) struct SupervisorStats {
    pub(crate) spawns: u64,
    pub(crate) stopped: bool,
    pub(crate) last_failure: Option<HelperFailure>,
    /// The last discarded helper's own exit; `None` when the app ended it (see `last_ended_by_app`).
    pub(crate) last_exit: Option<ExitKind>,
    /// The app ended the last discarded helper (deadline, limit, malformed reply, shutdown); the reason
    /// is `last_failure`.
    pub(crate) last_ended_by_app: bool,
    pub(crate) last_io_cancelled: bool,
    pub(crate) last_stderr: Vec<u8>,
    pub(crate) process_id: Option<u32>,
}

/// Owns at most one helper. Not thread-safe: the pdf-worker thread is its only user.
pub(crate) struct Supervisor {
    config: HelperConfig,
    helper: Option<Helper>,
    generation: u64,
    restarts: VecDeque<Instant>,
    last_failure_at: Option<Instant>,
    next_sequence: u32,
    stats: SupervisorStats,
}

impl Supervisor {
    /// Starts the first helper now, so a broken setup fails at start-up rather than at the first file.
    pub(crate) fn start(config: HelperConfig) -> Result<Self, HelperFailure> {
        let mut supervisor = Self { config, helper: None, generation: 0, restarts: VecDeque::new(), last_failure_at: None, next_sequence: 1, stats: SupervisorStats::default() };
        supervisor.spawn_now()?;
        Ok(supervisor)
    }

    pub(crate) fn deadlines(&self) -> Deadlines {
        self.config.deadlines
    }

    /// The generation of the running helper; documents loaded in an older generation are gone.
    pub(crate) fn running_generation(&self) -> Option<u64> {
        self.helper.as_ref().map(|_| self.generation)
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn stats(&self) -> SupervisorStats {
        let mut stats = self.stats.clone();
        stats.process_id = self.helper.as_ref().map(|helper| helper.child.process_id());
        stats
    }

    /// Clears the crash-loop stop and its restart history (the user's Retry).
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn reset_after_stop(&mut self) {
        self.stats.stopped = false;
        self.restarts.clear();
    }

    fn sequence(&mut self) -> u32 {
        let sequence = self.next_sequence;
        self.next_sequence = self.next_sequence.checked_add(1).unwrap_or(1);
        sequence
    }

    fn spawn_now(&mut self) -> Result<u64, HelperFailure> {
        let started = Instant::now();
        self.stats.spawns += 1;
        let mut helper = match Helper::spawn(&self.config) {
            Ok(helper) => helper,
            Err(failure) => return Err(self.record(failure, None)),
        };
        let deadline = started + self.config.deadlines.startup;
        let init = helper.hello(deadline).and_then(|()| {
            let sequence = self.sequence();
            let frame = FrameWriter::new(kind::INIT, sequence);
            let (header, body) = helper.exchange(frame.finish(), Header { kind: kind::INIT, sequence }, MAX_CONTROL_REPLY, deadline)?;
            let (_, mut reader) = parse_body(&body).map_err(|error| HelperFailure::Malformed(error.0))?;
            if header.kind == kind::ERROR {
                let (_, message) = get_error(&mut reader, class::ONLY_GENERAL).map_err(|error| HelperFailure::Malformed(error.0))?;
                return Err(HelperFailure::Start(message));
            }
            reader.finish().map_err(|error| HelperFailure::Malformed(error.0))
        });
        match init {
            Ok(()) => {
                self.helper = Some(helper);
                self.generation += 1;
                Ok(self.generation)
            }
            Err(failure) => {
                let failure = match failure {
                    HelperFailure::Start(reason) => HelperFailure::Start(reason),
                    other => HelperFailure::Start(format!("{other:?}")),
                };
                Err(self.record(failure, Some(helper)))
            }
        }
    }

    /// Ends `helper` (if any), records the failure and returns it.
    fn record(&mut self, failure: HelperFailure, helper: Option<Helper>) -> HelperFailure {
        if let Some(helper) = helper {
            let discarded = helper.discard(EXIT_RESOURCE_LIMIT);
            self.stats.last_ended_by_app = discarded.exit.is_none();
            self.stats.last_exit = discarded.exit;
            self.stats.last_io_cancelled = discarded.io_cancelled;
            self.stats.last_stderr = discarded.stderr;
        }
        self.last_failure_at = Some(Instant::now());
        self.stats.last_failure = Some(failure.clone());
        failure
    }

    fn fail(&mut self, failure: HelperFailure) -> CallError {
        let helper = self.helper.take();
        CallError::Failure(self.record(failure, helper))
    }

    /// Ends the running helper for a reply that decoded but contradicts the app's own state.
    pub(crate) fn fail_malformed(&mut self, reason: &str) -> CallError {
        self.fail(HelperFailure::Malformed(reason.into()))
    }

    /// A running helper, starting one if needed: immediately for the first restart in the window, after a
    /// growing backoff for later ones, and not at all once `max_restarts` restarts fall within the window.
    fn ensure_running(&mut self) -> Result<u64, HelperFailure> {
        if self.helper.is_some() {
            return Ok(self.generation);
        }
        if self.stats.stopped {
            return Err(HelperFailure::Stopped);
        }
        let policy = self.config.restart;
        let now = Instant::now();
        while self.restarts.front().is_some_and(|restart| now.duration_since(*restart) >= policy.window) {
            self.restarts.pop_front();
        }
        if self.restarts.len() >= policy.max_restarts {
            self.stats.stopped = true;
            self.stats.last_failure = Some(HelperFailure::Stopped);
            return Err(HelperFailure::Stopped);
        }
        let delay = policy.backoff(self.restarts.len());
        if let Some(failed) = self.last_failure_at {
            let waited = failed.elapsed();
            if waited < delay {
                std::thread::sleep(delay - waited);
            }
        }
        self.restarts.push_back(Instant::now());
        self.spawn_now()
    }

    /// Sends one request and decodes its reply. `payload` writes the request after the header;
    /// `reply_limit` bounds the reply frame; `allowed` lists the error classes this request may return;
    /// `decode` must consume the reply payload exactly. Any helper failure, or a reply that fails to decode,
    /// ends the helper.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn call<T>(
        &mut self,
        request_kind: u16,
        capacity: usize,
        payload: impl FnOnce(&mut FrameWriter),
        reply_limit: usize,
        deadline: Duration,
        allowed: &[u8],
        decode: impl FnOnce(&mut WireReader<'_>) -> Result<T, Malformed>,
    ) -> Result<T, CallError> {
        self.ensure_running().map_err(CallError::Failure)?;
        self.call_running(request_kind, capacity, payload, reply_limit, deadline, allowed, decode)
    }

    /// As `call`, but only if a helper is already running (used for close, which never starts one).
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn call_running<T>(
        &mut self,
        request_kind: u16,
        capacity: usize,
        payload: impl FnOnce(&mut FrameWriter),
        reply_limit: usize,
        deadline: Duration,
        allowed: &[u8],
        decode: impl FnOnce(&mut WireReader<'_>) -> Result<T, Malformed>,
    ) -> Result<T, CallError> {
        let sequence = self.sequence();
        let mut frame = FrameWriter::with_capacity(request_kind, sequence, capacity);
        payload(&mut frame);
        let frame = frame.finish();
        if frame.len() - 4 > MAX_REQUEST_FRAME {
            return Err(CallError::Engine(class::GENERAL, "This request is too large for the PDF engine.".into()));
        }
        let Some(helper) = self.helper.as_mut() else {
            return Err(CallError::Failure(HelperFailure::Stopped));
        };
        let deadline = Instant::now() + deadline;
        let (header, body) = match helper.exchange(frame, Header { kind: request_kind, sequence }, reply_limit.max(MAX_CONTROL_REPLY), deadline) {
            Ok(reply) => reply,
            Err(failure) => return Err(self.fail(failure)),
        };
        let outcome = {
            let (_, mut reader) = match parse_body(&body) {
                Ok(parsed) => parsed,
                Err(error) => return Err(self.fail(HelperFailure::Malformed(error.0))),
            };
            if header.kind == kind::ERROR {
                get_error(&mut reader, allowed).and_then(|error| reader.finish().map(|()| Err(error)))
            } else {
                decode(&mut reader).and_then(|value| reader.finish().map(|()| Ok(value)))
            }
        };
        match outcome {
            Ok(Ok(value)) => Ok(value),
            Ok(Err((error_class, message))) => Err(CallError::Engine(error_class, message)),
            Err(error) => Err(self.fail(HelperFailure::Malformed(error.0))),
        }
    }
}

impl Drop for Supervisor {
    fn drop(&mut self) {
        if let Some(helper) = self.helper.take() {
            helper.discard(EXIT_CANCELLED);
        }
    }
}
