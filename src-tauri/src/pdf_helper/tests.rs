//! Helper step 3 tests: design T3 (crash), T4 (hang), T5 (suspect document, crash loop), T12 (orphans),
//! T13 (handle hygiene), T14 (hostile replies), memory-limit handling under a low test cap, and
//! equivalence of `RemotePdfEngine` with `InProcessEngine` on the fixtures.
//!
//! The helper here is this test binary re-run with a mode argument (`helper_entry`), which runs the same
//! request loop as the app's helper mode plus the test-only fault requests.

use std::alloc::{GlobalAlloc, Layout};
use std::ffi::{OsStr, OsString};
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use windows::core::BOOL;
use windows::Win32::Foundation::{CloseHandle, WAIT_OBJECT_0, WAIT_TIMEOUT};
use windows::Win32::Security::SECURITY_ATTRIBUTES;
use windows::Win32::System::Threading::{
    CreateEventW, OpenProcess, SetEvent, WaitForSingleObject, PROCESS_ACCESS_RIGHTS, PROCESS_QUERY_LIMITED_INFORMATION,
};

use super::faults::{Fault, Misreply, CRASH_ON_LOAD_EXIT, CRASH_ON_LOAD_MAGIC};
use super::protocol::*;
use super::remote::{RemoteDocument, RemotePdfEngine, CLOSED_MESSAGE, SUSPECT_MESSAGE};
use super::supervisor::{CallError, Deadlines, HelperConfig, HelperFailure, HelperLaunch, RestartPolicy};
use super::{helper_enabled, helper_mode_requested, is_helper_invocation, HELPER_MODE_AVAILABLE, HELPER_SETTING_VARIABLE};
use crate::child_job::{as_windows_handle, command_line, owned_handle, wide_null, ChildCommand, ExitKind, JobLimits, LimitViolation, ModeSwitchAllocator};
use crate::editor::{CropBox, PageSpec};
use crate::pdf_engine::{EngineDocument, InProcessEngine, PageTarget, PdfEngine, RenderError, RenderMode};

const MODE_PREFIX: &str = "pdf-helper-test-mode=";
const ENTRY: &str = "pdf_helper::tests::helper_entry";
const TEST_CAP: usize = 256 * 1024 * 1024;
/// Below one 6000 x 6000 BGRA bitmap (144 MB), above the helper's own baseline.
const LOW_CAP: usize = 128 * 1024 * 1024;
const GIB: u64 = 1024 * 1024 * 1024;
const WELCOME_PAGES: usize = 6;
const LONG: Duration = Duration::from_secs(60);

/// Helper side. Runs only when a test starts this binary with a mode argument; in a normal run it returns.
#[test]
fn helper_entry() {
    let Some(mode) = std::env::args().find_map(|argument| argument.strip_prefix(MODE_PREFIX).map(str::to_owned)) else {
        return;
    };
    match mode.as_str() {
        "serve" => super::run_helper_mode(),
        "host" => {
            let engine = RemotePdfEngine::start(test_config(TEST_CAP)).unwrap();
            let mut stdout = std::io::stdout();
            let _ = writeln!(stdout, "helper-pid={}", engine.stats().process_id.unwrap());
            let _ = stdout.flush();
            std::thread::sleep(Duration::from_secs(60));
            std::process::exit(0)
        }
        _ => std::process::exit(94),
    }
}

/// The command that starts this test binary as a helper in `mode`.
pub(crate) fn test_entry_command(mode: &str) -> ChildCommand {
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

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn library() -> PathBuf {
    root().join("resources/pdfium/bin/pdfium.dll")
}

fn welcome() -> Vec<u8> {
    std::fs::read(root().join("resources/welcome.pdf")).unwrap()
}

fn test_config(memory: usize) -> HelperConfig {
    HelperConfig {
        launch: HelperLaunch::TestEntry { mode: "serve".into() },
        limits: JobLimits::hardened_single_process(memory),
        deadlines: Deadlines { startup: LONG, load: LONG, render: LONG, read: LONG, close: Duration::from_secs(10) },
        restart: RestartPolicy { window: Duration::from_secs(60), max_restarts: 100, backoff_base: Duration::from_millis(10), backoff_max: Duration::from_millis(100) },
    }
}

fn spec(source: usize) -> PageSpec {
    PageSpec { source, turns: 0, crop: None, notes: Vec::new() }
}

fn render(document: &RemoteDocument<'_>) -> Vec<u8> {
    let mut pixels = vec![0; 200 * 260 * 4];
    document.render_bgra_into(PageTarget::Planned(&spec(0)), RenderMode::Plain, 200, 260, &mut pixels).unwrap();
    pixels
}

fn process_is_alive(pid: u32) -> bool {
    let access = PROCESS_ACCESS_RIGHTS(PROCESS_QUERY_LIMITED_INFORMATION.0 | 0x0010_0000);
    let Ok(process) = (unsafe { OpenProcess(access, false, pid) }) else { return false };
    let handle = unsafe { owned_handle(process) };
    (unsafe { WaitForSingleObject(as_windows_handle(&handle), 0) }) == WAIT_TIMEOUT
}

fn assert_gone_within(pid: u32, limit: Duration) -> Duration {
    let started = Instant::now();
    while started.elapsed() < limit {
        if !process_is_alive(pid) {
            return started.elapsed();
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    panic!("helper {pid} is still alive after {limit:?}");
}

fn failure(error: CallError) -> HelperFailure {
    match error {
        CallError::Failure(failure) => failure,
        other => panic!("expected a helper failure, got {other:?}"),
    }
}

#[test]
fn t3_an_exit_mid_request_fails_that_request_and_other_documents_reload_after_restart() {
    let engine = RemotePdfEngine::start(test_config(TEST_CAP)).unwrap();
    let survivor = engine.load_from_vec(welcome(), None).unwrap();
    let before = render(&survivor);
    let victim = engine.load_from_vec(welcome(), None).unwrap();
    let pid = engine.stats().process_id.unwrap();
    let started = Instant::now();
    let error = victim.inject(Fault::Exit(3), LONG).unwrap_err();
    let elapsed = started.elapsed();
    assert_eq!(failure(error), HelperFailure::Exited(Some(ExitKind::Code(3))));
    println!("t3 exit: request failed {elapsed:?} after it was sent");
    assert!(elapsed < LONG / 10, "the exit is noticed long before the request deadline: {elapsed:?}");
    assert!(!process_is_alive(pid));
    assert!(victim.is_suspect());
    assert_eq!(render(&survivor), before, "the other document reloads into the new helper and renders the same");
    assert_eq!(engine.stats().spawns, 2);
    assert_ne!(engine.stats().process_id, Some(pid));
    assert_eq!(victim.page_sizes().err().unwrap(), SUSPECT_MESSAGE);
}

/// A helper that dies while no request is in flight is found before the next request and replaced (the
/// restart counts toward the crash-loop cap); no document is blamed, so documents reload and render as before.
#[test]
fn an_idle_helper_crash_is_replaced_before_the_next_request() {
    let engine = RemotePdfEngine::start(test_config(TEST_CAP)).unwrap();
    let document = engine.load_from_vec(welcome(), None).unwrap();
    let before = render(&document);
    assert!(engine.kill_helper());
    assert_eq!(render(&document), before, "the next render reloads into a new helper");
    let stats = engine.stats();
    assert_eq!(stats.spawns, 2);
    assert_eq!(stats.last_failure, Some(HelperFailure::Exited(Some(ExitKind::Code(0xdead)))));
    assert!(!document.is_suspect());
    let other = engine.load_from_vec(welcome(), None).unwrap();
    assert!(engine.kill_helper());
    assert_eq!(other.page_sizes().unwrap().len(), WELCOME_PAGES, "a load-path request (section sharing) also recovers");
}

/// Code review P3: a helper that dies after a document looked up its handle but before the request is sent
/// must not receive that stale handle (a spurious "no such document"); the document reloads into the new
/// helper and the request succeeds. Deterministic: the test hook kills the helper exactly in that window.
#[test]
fn a_helper_death_between_lookup_and_send_reloads_instead_of_sending_a_stale_handle() {
    let engine = RemotePdfEngine::start(test_config(TEST_CAP)).unwrap();
    let document = engine.load_from_vec(welcome(), None).unwrap();
    let before = render(&document);
    engine.kill_after_next_lookup();
    assert_eq!(document.page_sizes().unwrap().len(), WELCOME_PAGES, "plain request");
    engine.kill_after_next_lookup();
    assert_eq!(render(&document), before, "request that shares a render section");
    let stats = engine.stats();
    assert_eq!(stats.spawns, 3, "each death replaced once");
    assert_eq!(stats.last_failure, Some(HelperFailure::Exited(Some(ExitKind::Code(0xdead)))));
    assert!(!document.is_suspect(), "no request was in flight, so the document is not set aside");
}

#[test]
fn t3_an_access_violation_ends_the_helper_with_its_exception_code() {
    let engine = RemotePdfEngine::start(test_config(TEST_CAP)).unwrap();
    let document = engine.load_from_vec(welcome(), None).unwrap();
    let error = document.inject(Fault::RaiseAccessViolation, LONG).unwrap_err();
    assert_eq!(failure(error), HelperFailure::Exited(Some(ExitKind::Exception(0xc000_0005))));
    let stats = engine.stats();
    assert!(String::from_utf8_lossy(&stats.last_stderr).contains("unhandled exception"), "{:?}", String::from_utf8_lossy(&stats.last_stderr));
    let other = engine.load_from_vec(welcome(), None).unwrap();
    assert_eq!(other.page_sizes().unwrap().len(), WELCOME_PAGES);
}

#[test]
fn t4_a_hang_fails_at_its_deadline_and_the_helper_is_replaced() {
    let engine = RemotePdfEngine::start(test_config(TEST_CAP)).unwrap();
    let survivor = engine.load_from_vec(welcome(), None).unwrap();
    let before = render(&survivor);
    let hung = engine.load_from_vec(welcome(), None).unwrap();
    let pid = engine.stats().process_id.unwrap();
    let deadline = Duration::from_millis(800);
    let started = Instant::now();
    let error = hung.inject(Fault::Hang, deadline).unwrap_err();
    let elapsed = started.elapsed();
    assert_eq!(failure(error), HelperFailure::TimedOut);
    println!("t4 hang: deadline {deadline:?}, request failed after {elapsed:?}");
    assert!(elapsed >= deadline && elapsed < deadline * 5, "{elapsed:?}");
    assert!(!process_is_alive(pid));
    let stats = engine.stats();
    assert!(stats.last_ended_by_app && stats.last_exit.is_none(), "the app ended the hung helper: {stats:?}");
    assert_eq!(render(&survivor), before);
}

#[test]
fn a_refused_allocation_ends_the_helper_and_is_reported_as_its_memory_limit() {
    let engine = RemotePdfEngine::start(test_config(TEST_CAP)).unwrap();
    let error = engine.fault(Fault::AllocateVec(GIB), LONG).unwrap_err();
    assert!(matches!(failure(error), HelperFailure::LimitViolation(LimitViolation::ProcessMemory | LimitViolation::JobMemory)));
    assert_eq!(engine.stats().last_exit, Some(ExitKind::AllocationFailure));
    assert_eq!(engine.fault(Fault::AllocateVec(16 * 1024 * 1024), LONG), Ok(1), "a new helper serves the next request");
}

#[test]
fn a_reply_sent_after_a_refused_allocation_is_discarded_with_its_helper() {
    let engine = RemotePdfEngine::start(test_config(TEST_CAP)).unwrap();
    assert_eq!(engine.fault(Fault::RefusedAllocationThenReply(16 * 1024 * 1024), LONG), Ok(1));
    let pid = engine.stats().process_id.unwrap();
    let error = engine.fault(Fault::RefusedAllocationThenReply(GIB), LONG).unwrap_err();
    assert!(matches!(failure(error), HelperFailure::LimitViolation(_)), "the helper replied, but the reply must not be used");
    assert!(!process_is_alive(pid));
    let stats = engine.stats();
    assert!(stats.last_ended_by_app && stats.last_exit.is_none(), "the app ended the capped helper: {stats:?}");
    assert_eq!(engine.fault(Fault::RefusedAllocationThenReply(16 * 1024 * 1024), LONG), Ok(1));
    assert_ne!(engine.stats().process_id, Some(pid));
}

/// Step 4: render targets are app-owned sections, which are not charged to the helper's job, so a bitmap
/// larger than the whole cap renders (step 3 refused it, the pixels then lived in the helper).
#[test]
fn a_bitmap_larger_than_the_cap_renders_into_an_app_section() {
    let engine = RemotePdfEngine::start(test_config(LOW_CAP)).unwrap();
    let document = engine.load_from_vec(welcome(), None).unwrap();
    let (width, height) = (6_000_u32, 6_000_u32);
    let mut pixels = vec![0x5a_u8; (width * height * 4) as usize];
    document.render_bgra_into(PageTarget::Source(0), RenderMode::Print, width, height, &mut pixels).unwrap();
    assert!(pixels.chunks_exact(4).any(|pixel| pixel != [0x5a; 4]), "the render reached the caller's buffer");
    assert!(engine.stats().last_failure.is_none(), "{:?}", engine.stats().last_failure);
}

/// Step 4: sources are app-owned read-only sections, also not charged to the helper's job, so a source
/// larger than the whole cap loads and renders (step 3 copied it into the helper's heap).
#[test]
fn a_source_larger_than_the_cap_loads_from_an_app_section() {
    use lopdf::{dictionary, Object, Stream};
    let mut padded = lopdf::Document::load_mem(&welcome()).unwrap();
    let ballast = padded.add_object(Stream::new(dictionary! {}, vec![b'0'; LOW_CAP + 64 * 1024 * 1024]));
    padded.trailer.set("SM5Ballast", Object::Reference(ballast));
    let mut bytes = Vec::new();
    padded.save_to(&mut bytes).unwrap();
    assert!(bytes.len() > LOW_CAP);
    let engine = RemotePdfEngine::start(test_config(LOW_CAP)).unwrap();
    let document = engine.load_from_vec(bytes, None).unwrap();
    assert_eq!(document.page_sizes().unwrap().len(), WELCOME_PAGES);
    assert!(render(&document).chunks_exact(4).any(|pixel| pixel != [255; 4]));
    assert!(engine.stats().last_failure.is_none(), "{:?}", engine.stats().last_failure);
}

#[test]
fn t5_a_suspect_document_waits_for_retry_and_a_second_death_closes_it() {
    let engine = RemotePdfEngine::start(test_config(TEST_CAP)).unwrap();
    let document = engine.load_from_vec(welcome(), None).unwrap();
    let other = engine.load_from_vec(welcome(), None).unwrap();
    document.inject(Fault::Exit(3), LONG).unwrap_err();
    assert_eq!(document.page_sizes().err().unwrap(), SUSPECT_MESSAGE);
    assert_eq!(engine.stats().spawns, 1, "a suspect document does not start a helper or reload itself");
    document.retry();
    assert_eq!(document.page_sizes().unwrap().len(), WELCOME_PAGES);
    assert_eq!(engine.stats().spawns, 2);
    document.inject(Fault::Exit(3), LONG).unwrap_err();
    document.retry();
    assert_eq!(document.page_sizes().err().unwrap(), CLOSED_MESSAGE);
    assert_eq!(other.page_sizes().unwrap().len(), WELCOME_PAGES, "other documents keep working");
}

#[test]
fn t5_a_file_that_kills_every_helper_stops_restarts_after_three_in_the_window() {
    let mut config = test_config(TEST_CAP);
    config.restart = RestartPolicy { window: Duration::from_secs(60), max_restarts: 3, backoff_base: Duration::from_millis(100), backoff_max: Duration::from_secs(1) };
    let engine = RemotePdfEngine::start(config).unwrap();
    let mut crash = CRASH_ON_LOAD_MAGIC.to_vec();
    crash.extend_from_slice(&welcome());
    let mut attempts = Vec::new();
    for _ in 0..4 {
        let started = Instant::now();
        let error = engine.load_from_vec(crash.clone(), None).map(|_| ()).unwrap_err();
        attempts.push((started.elapsed(), engine.stats().last_failure.clone().unwrap()));
        assert!(!error.is_password());
    }
    println!("t5 crash loop attempts: {attempts:?}");
    for (_, failure) in &attempts {
        assert_eq!(*failure, HelperFailure::Exited(Some(ExitKind::Code(CRASH_ON_LOAD_EXIT))));
    }
    assert!(attempts[2].0 >= Duration::from_millis(100) && attempts[3].0 >= Duration::from_millis(200), "restarts back off: {attempts:?}");
    assert_eq!(engine.stats().spawns, 4, "the first helper and three restarts");
    let started = Instant::now();
    let stopped = engine.load_from_vec(welcome(), None).map(|_| ()).unwrap_err();
    assert_eq!(stopped.to_string(), HelperFailure::Stopped.message());
    assert!(started.elapsed() < Duration::from_secs(2), "a stopped engine refuses without waiting for a restart");
    assert_eq!(engine.stats().spawns, 4, "no helper is started once stopped");
    assert!(engine.stats().stopped);
    engine.reset_after_stop();
    assert_eq!(engine.load_from_vec(welcome(), None).unwrap().page_count(), WELCOME_PAGES);
    assert_eq!(engine.stats().spawns, 5);
}

#[test]
fn t12_the_helper_dies_when_the_app_is_killed() {
    let mut host = crate::child_job::test_spawn(std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", ENTRY, "--nocapture", "--quiet", "--test-threads=1", &format!("{MODE_PREFIX}host")])
        .stdout(std::process::Stdio::piped()))
        .unwrap();
    let mut lines = BufReader::new(host.stdout.take().unwrap()).lines();
    let pid: u32 = loop {
        let line = lines.next().expect("host printed no helper pid").unwrap();
        if let Some(pid) = line.strip_prefix("helper-pid=") {
            break pid.trim().parse().unwrap();
        }
    };
    assert!(process_is_alive(pid));
    host.kill().unwrap();
    let _ = host.wait();
    let gone = assert_gone_within(pid, Duration::from_secs(1));
    println!("t12: helper gone {gone:?} after the host was terminated");
}

#[test]
fn t13_the_helper_inherits_only_its_pipes() {
    let attributes = SECURITY_ATTRIBUTES { nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32, lpSecurityDescriptor: std::ptr::null_mut(), bInheritHandle: BOOL(1) };
    let event = unsafe { CreateEventW(Some(&attributes), true, false, None) }.unwrap();
    let engine = RemotePdfEngine::start(test_config(TEST_CAP)).unwrap();
    engine.fault(Fault::SetEvent(event.0 as usize as u64), LONG).unwrap();
    assert_eq!(unsafe { WaitForSingleObject(event, 0) }, WAIT_TIMEOUT, "the helper reached an inheritable handle it was not given");
    unsafe { SetEvent(event) }.unwrap();
    assert_eq!(unsafe { WaitForSingleObject(event, 0) }, WAIT_OBJECT_0);
    unsafe { CloseHandle(event) }.unwrap();
}

/// T13, read-only half: a source section reaches the helper through a handle that allows reading only, so
/// a write mapping through it fails while a read mapping sees the app's bytes. A handle shared writable (as
/// render targets are) does allow writing, which shows the probe can tell the difference.
#[test]
fn t13_source_sections_reach_the_helper_read_only() {
    let engine = RemotePdfEngine::start(test_config(TEST_CAP)).unwrap();
    let section = super::section::Section::from_bytes(&[0xa5; 8192]).unwrap();
    assert_eq!(engine.probe_section(&section, super::section::FILE_MAP_READ), Ok(0b110), "read-only: no write mapping, reads the app's bytes");
    assert_eq!(engine.probe_section(&section, super::section::FILE_MAP_READ | super::section::FILE_MAP_WRITE), Ok(0b111));
}

#[test]
fn t14_malformed_replies_end_the_helper_and_fail_only_that_request() {
    type Probe = fn(&RemoteDocument<'_>) -> bool;
    let sizes: Probe = |document| document.page_sizes().is_err();
    let bitmap: Probe = |document| {
        let mut pixels = vec![0; 100 * 100 * 4];
        document.render_bgra_into(PageTarget::Source(0), RenderMode::Plain, 100, 100, &mut pixels).is_err()
    };
    let text: Probe = |document| document.visible_text(PageTarget::Source(0)).is_err();
    let png: Probe = |document| document.render_viewer_png(&spec(0), 300).is_err();
    let cases: [(Misreply, Probe); 10] = [
        (Misreply::OversizedLength, sizes),
        (Misreply::WrongVersion, sizes),
        (Misreply::WrongSequence, sizes),
        (Misreply::WrongKind, sizes),
        (Misreply::TrailingByte, sizes),
        (Misreply::TruncatedThenExit, sizes),
        (Misreply::DropLastPage, sizes),
        (Misreply::DropLastFourBytes, bitmap),
        (Misreply::CorruptLastByte, text),
        (Misreply::BumpFirstPayloadU32, sizes),
    ];
    let engine = RemotePdfEngine::start(test_config(TEST_CAP)).unwrap();
    let bystander = engine.load_from_vec(welcome(), None).unwrap();
    let before = render(&bystander);
    assert!(!text(&bystander) && !png(&bystander), "the probes succeed on a well-behaved helper");
    for (misreply, probe) in cases {
        let document = engine.load_from_vec(welcome(), None).unwrap();
        let pid = engine.stats().process_id.unwrap();
        assert_eq!(document.inject(Fault::Misreply(misreply), LONG), Ok(1));
        assert!(probe(&document), "{misreply:?} was accepted");
        let failure = engine.stats().last_failure.unwrap();
        match misreply {
            Misreply::TruncatedThenExit => assert!(matches!(failure, HelperFailure::Exited(_) | HelperFailure::Malformed(_)), "{failure:?}"),
            _ => assert!(matches!(failure, HelperFailure::Malformed(_)), "{misreply:?}: {failure:?}"),
        }
        assert!(!process_is_alive(pid), "{misreply:?}: the helper was kept");
        assert!(document.is_suspect());
        assert_eq!(render(&bystander), before, "{misreply:?}: the bystander recovers");
    }
}

/// SM7 M2: frames the helper writes while no request is pending are not read into memory (the reader's
/// limit is 0 between requests and the queue holds one event), and the next request ends the helper.
#[test]
fn frames_written_while_no_request_is_pending_end_the_helper() {
    let engine = RemotePdfEngine::start(test_config(TEST_CAP)).unwrap();
    let document = engine.load_from_vec(welcome(), None).unwrap();
    let pid = engine.stats().process_id.unwrap();
    assert_eq!(document.inject(Fault::Misreply(Misreply::FloodAfter), LONG), Ok(1));
    assert_eq!(document.page_sizes().unwrap().len(), WELCOME_PAGES, "the reply itself is correct");
    assert!(document.page_sizes().is_err(), "the next request ends the helper whichever way the stray frame arrives");
    assert!(matches!(engine.stats().last_failure, Some(HelperFailure::Malformed(_))), "{:?}", engine.stats().last_failure);
    assert!(!process_is_alive(pid));
    let other = engine.load_from_vec(welcome(), None).unwrap();
    assert_eq!(other.page_sizes().unwrap().len(), WELCOME_PAGES);
}

/// SM7 L1: a helper that hands out a handle already in use is ended, and the load fails.
#[test]
fn a_reused_document_handle_ends_the_helper() {
    let engine = RemotePdfEngine::start(test_config(TEST_CAP)).unwrap();
    let first = engine.load_from_vec(welcome(), None).unwrap();
    let before = render(&first);
    let pid = engine.stats().process_id.unwrap();
    assert_eq!(engine.fault(Fault::Misreply(Misreply::HandleOne), LONG), Ok(1));
    let error = engine.load_from_vec(welcome(), None).map(|_| ()).unwrap_err();
    assert_eq!(error.to_string(), HelperFailure::Malformed(String::new()).message());
    assert!(matches!(engine.stats().last_failure, Some(HelperFailure::Malformed(reason)) if reason.contains("same handle")));
    assert!(!process_is_alive(pid));
    assert_eq!(render(&first), before, "the first document reloads into a new helper");
}

#[test]
fn the_helper_is_off_unless_the_debug_setting_is_on() {
    if std::env::var_os(HELPER_SETTING_VARIABLE).is_none() {
        assert!(!helper_enabled());
    }
    let arguments = |list: &[&str]| list.iter().map(OsString::from).collect::<Vec<_>>();
    assert!(is_helper_invocation(arguments(&["app.exe", "--pdf-engine-helper"])));
    assert!(!is_helper_invocation(arguments(&["app.exe"])));
    assert!(!is_helper_invocation(arguments(&["app.exe", "--pdf-engine-helper", "extra"])));
    assert!(!is_helper_invocation(arguments(&["app.exe", "file.pdf", "--pdf-engine-helper"])));
    assert!(!is_helper_invocation(arguments(&["app.exe", "--PDF-ENGINE-HELPER"])));
}

/// SM7 M1: a release build ignores `--pdf-engine-helper` (main.rs checks it only under
/// `debug_assertions`, and `helper_mode_requested` is `false` there too) and starts the app normally.
/// Run in both profiles: `cargo test --bins` and `cargo test --release --bins`.
#[test]
fn release_builds_ignore_the_helper_flag() {
    let invocation = ["app.exe", "--pdf-engine-helper"].iter().map(OsString::from).collect::<Vec<_>>();
    assert_eq!(HELPER_MODE_AVAILABLE, cfg!(debug_assertions));
    assert_eq!(helper_mode_requested(invocation), cfg!(debug_assertions));
}

/// SM7 M1: the helper never loads a library named by a request. An init frame with any payload (here a
/// DLL path) ends the helper before anything is loaded.
#[test]
fn an_init_request_carrying_a_library_path_is_refused() {
    let mut init = FrameWriter::new(kind::INIT, 1);
    init.str(r"C:\elsewhere\evil.dll");
    let mut input = std::io::Cursor::new(init.finish());
    let mut output = Vec::new();
    assert_eq!(super::serve::serve(&mut input, &mut output), super::serve::EXIT_PROTOCOL);
    let mut hello = Vec::from(SYNC);
    let mut frame = FrameWriter::new(kind::HELLO, 0);
    frame.u32(std::process::id());
    hello.extend_from_slice(&frame.finish());
    assert_eq!(output, hello, "nothing but the hello was written");
    assert!(super::helper_library().unwrap().ends_with(super::PDFIUM_RELATIVE_PATH));
}

#[test]
fn the_global_allocator_is_the_system_allocator_until_armed() {
    let layout = Layout::from_size_align(usize::MAX / 4, 4096).unwrap();
    assert!(unsafe { ModeSwitchAllocator.alloc(layout) }.is_null(), "an unarmed refusal returns null and the process lives");
    assert!(Vec::<u8>::new().try_reserve_exact(usize::MAX / 4).is_err());
}

#[test]
fn restart_backoff_grows_and_is_capped() {
    let policy = RestartPolicy { window: Duration::from_secs(60), max_restarts: 3, backoff_base: Duration::from_millis(100), backoff_max: Duration::from_millis(350) };
    let delays = (0..5).map(|recent| policy.backoff(recent).as_millis()).collect::<Vec<_>>();
    assert_eq!(delays, [0, 100, 200, 350, 350]);
    assert_eq!(RestartPolicy::PRODUCTION.backoff(64), RestartPolicy::PRODUCTION.backoff_max);
}

// T14 at the codec level: nothing past a cap is allocated, and inconsistent content is refused.

fn frame(kind: u16, sequence: u32, payload: impl FnOnce(&mut FrameWriter)) -> Vec<u8> {
    let mut writer = FrameWriter::new(kind, sequence);
    payload(&mut writer);
    writer.finish()
}

fn payload_of(frame: &[u8]) -> Vec<u8> {
    frame[4 + HEADER_LEN..].to_vec()
}

fn decode<T>(payload: &[u8], get: impl FnOnce(&mut WireReader<'_>) -> Result<T, Malformed>) -> Result<T, Malformed> {
    let mut reader = WireReader::new(payload);
    let value = get(&mut reader)?;
    reader.finish().map(|()| value)
}

/// SM0 P2: the reader takes the limit with `swap(0)`, so one stored limit admits exactly one frame and the
/// next frame is refused unallocated, with no window for a second frame at the old limit.
#[test]
fn one_request_limit_admits_exactly_one_frame() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let mut stream = frame(kind::PAGE_SIZES, 1, |writer| writer.u8(1));
    stream.extend_from_slice(&frame(kind::PAGE_SIZES, 2, |writer| writer.u8(2)));
    let mut input = std::io::Cursor::new(stream);
    let limit = AtomicUsize::new(1024);
    let take = || limit.swap(0, Ordering::AcqRel);
    assert!(read_frame(&mut input, &take).is_ok());
    assert!(matches!(read_frame(&mut input, &take), Err(FrameError::BadLength(_))));
    assert_eq!(limit.load(Ordering::Acquire), 0);
}

/// Render modes keep fixed wire values (0 Print, 1 Plain, 2 Viewer, the redaction path); others are refused.
#[test]
fn render_modes_have_fixed_wire_values() {
    for (mode, value) in [(RenderMode::Print, 0_u8), (RenderMode::Plain, 1), (RenderMode::Viewer, 2)] {
        let encoded = payload_of(&frame(kind::RENDER_BGRA, 1, |writer| put_mode(writer, mode)));
        assert_eq!(encoded, [value]);
        assert_eq!(decode(&encoded, get_mode), Ok(mode));
    }
    for value in [3_u8, 255] {
        assert!(decode(&[value], get_mode).is_err());
    }
}

#[test]
fn frames_are_length_checked_before_anything_is_allocated() {
    let read = |bytes: &[u8], limit: usize| read_frame(&mut std::io::Cursor::new(bytes.to_vec()), &move || limit);
    assert!(matches!(read(&u32::MAX.to_le_bytes(), 1024), Err(FrameError::BadLength(length)) if length == u64::from(u32::MAX)));
    assert!(matches!(read(&4_u32.to_le_bytes(), 1024), Err(FrameError::BadLength(4))));
    assert!(matches!(read(&2048_u32.to_le_bytes(), 1024), Err(FrameError::BadLength(2048))));
    assert!(matches!(read(&[], 1024), Err(FrameError::Closed)));
    assert!(matches!(read(&[8, 0], 1024), Err(FrameError::Truncated)));
    assert!(matches!(read(&[16, 0, 0, 0, 1, 2, 3], 1024), Err(FrameError::Truncated)));
    let good = frame(kind::PAGE_SIZES, 7, |writer| writer.u8(1));
    let body = read(&good, 1024).unwrap();
    let (header, mut reader) = parse_body(&body).unwrap();
    assert_eq!(header, Header { kind: kind::PAGE_SIZES, sequence: 7 });
    assert_eq!(reader.u8(), Ok(1));
    let mut wrong_version = body.clone();
    wrong_version[..2].copy_from_slice(&(PROTOCOL_VERSION + 1).to_le_bytes());
    assert!(parse_body(&wrong_version).is_err());
    assert!(parse_body(&body[..HEADER_LEN - 1]).is_err());
}

#[test]
fn primitive_decoding_rejects_bad_tags_counts_and_strings() {
    assert!(decode(&[2], |reader| reader.bool()).is_err());
    assert!(decode(&[2], |reader| reader.opt(WireReader::u8)).is_err());
    assert!(decode(&[1, 2], |reader| reader.u8()).is_err(), "trailing bytes");
    assert!(decode(&[0, 0, 0], |reader| reader.u32()).is_err(), "short");
    assert!(decode(&10_u32.to_le_bytes(), |reader| reader.count(100, 1)).is_err(), "count beyond the payload");
    assert!(decode(&[200, 0, 0, 0], |reader| reader.count(100, 0)).is_err(), "count beyond its maximum");
    let mut long = 5_u32.to_le_bytes().to_vec();
    long.extend_from_slice(b"hello");
    assert!(decode(&long, |reader| reader.str(4)).is_err());
    assert_eq!(decode(&long, |reader| reader.str(5)), Ok("hello".to_owned()));
    let mut invalid = 2_u32.to_le_bytes().to_vec();
    invalid.extend_from_slice(&[0xc3, 0x28]);
    assert!(decode(&invalid, |reader| reader.str(16)).is_err());
    assert!(decode(&f32::NAN.to_bits().to_le_bytes(), |reader| reader.finite_f32()).is_err());
    let error = payload_of(&frame(kind::ERROR, 1, |writer| put_error(writer, class::RENDER_PAGE, "no")));
    assert!(decode(&error, |reader| get_error(reader, class::TEXT)).is_err(), "an error class that does not fit the request");
    assert_eq!(decode(&error, |reader| get_error(reader, class::RENDER)), Ok((class::RENDER_PAGE, "no".to_owned())));
    let mut huge = vec![class::GENERAL];
    huge.extend_from_slice(&(MAX_MESSAGE_BYTES as u32 + 1).to_le_bytes());
    huge.resize(huge.len() + MAX_MESSAGE_BYTES + 1, b'a');
    assert!(decode(&huge, |reader| get_error(reader, class::ONLY_GENERAL)).is_err());
}

/// SM7 L-b: page sizes, crop boxes and destination views from the helper must be finite (page sizes also
/// positive), otherwise the reply is malformed.
#[test]
fn helper_floats_must_be_finite() {
    let pair = |width: f32, height: f32| payload_of(&frame(kind::PLANNED_PAGE_SIZE, 1, |writer| put_pair(writer, (width, height))));
    assert_eq!(decode(&pair(612.0, 792.0), get_page_size), Ok((612.0, 792.0)));
    for (width, height) in [(f32::NAN, 792.0), (612.0, f32::INFINITY), (0.0, 792.0), (612.0, -1.0)] {
        assert!(decode(&pair(width, height), get_page_size).is_err(), "{width} x {height}");
    }
    let crop = |left: f32| payload_of(&frame(kind::DISPLAYED_CROP, 1, |writer| put_crop(writer, CropBox { left, bottom: 0.0, right: 10.0, top: 10.0 })));
    assert!(decode(&crop(1.0), get_finite_crop).is_ok());
    assert!(decode(&crop(f32::NEG_INFINITY), get_finite_crop).is_err());
    let view = |left: f32| payload_of(&frame(kind::BOOKMARK_PAGE_VIEW, 1, |writer| put_view(writer, crate::bookmark_destinations::DestinationView { left, bottom: 0.0, width: 612.0, height: 792.0, rotation: 0 })));
    assert!(decode(&view(0.0), get_view).is_ok());
    assert!(decode(&view(f32::NAN), get_view).is_err());
}

/// A render request whose size is refused still closes the section handle the app passed with it.
#[test]
fn a_refused_render_size_closes_the_passed_section_handle() {
    use windows::Win32::Foundation::{DuplicateHandle, DUPLICATE_HANDLE_OPTIONS, HANDLE};
    use windows::Win32::System::Threading::GetCurrentProcess;
    let section = super::section::Section::create(4096).unwrap();
    let mut passed = HANDLE::default();
    unsafe { DuplicateHandle(GetCurrentProcess(), section.handle(), GetCurrentProcess(), &mut passed, super::section::FILE_MAP_READ | super::section::FILE_MAP_WRITE, false, DUPLICATE_HANDLE_OPTIONS(0)) }.unwrap();
    // Count the section's open handles (not the handle value, which another test thread may reuse at once).
    #[repr(C)]
    struct BasicInformation { attributes: u32, granted_access: u32, handle_count: u32, pointer_count: u32, reserved: [u32; 10] }
    #[link(name = "ntdll")]
    extern "system" {
        fn NtQueryObject(handle: HANDLE, class: u32, information: *mut BasicInformation, length: u32, returned: *mut u32) -> i32;
    }
    let handles = || {
        let mut information = BasicInformation { attributes: 0, granted_access: 0, handle_count: 0, pointer_count: 0, reserved: [0; 10] };
        assert_eq!(unsafe { NtQueryObject(section.handle(), 0, &mut information, std::mem::size_of::<BasicInformation>() as u32, std::ptr::null_mut()) }, 0);
        information.handle_count
    };
    assert_eq!(handles(), 2);
    assert!(super::serve::map_render_target(passed.0 as usize as u64, 0, 0).is_err());
    assert_eq!(handles(), 1, "the passed handle was closed");
}

#[test]
fn reply_decoders_reject_inconsistent_content() {
    use crate::pdf_engine::PageSize;
    let sizes = payload_of(&frame(kind::PAGE_SIZES, 1, |writer| put_page_sizes(writer, &[PageSize { width: 612.0, height: 792.0 }, PageSize { width: 100.0, height: 50.0 }])));
    assert_eq!(decode(&sizes, |reader| get_page_sizes(reader, 2)).unwrap().len(), 2);
    assert!(decode(&sizes, |reader| get_page_sizes(reader, 3)).is_err(), "page count differs from the load");
    let negative = payload_of(&frame(kind::PAGE_SIZES, 1, |writer| put_page_sizes(writer, &[PageSize { width: -1.0, height: 792.0 }])));
    assert!(decode(&negative, |reader| get_page_sizes(reader, 1)).is_err());

    assert_eq!(viewer_size(612.0, 792.0, 800), Some((800, 1035)));
    assert_eq!(viewer_size(612.0, 7920.0, 800), Some((386, 5000)), "clamped to 5,000 high, aspect kept");
    assert_eq!(viewer_size(0.0, 792.0, 800), None);
    assert_eq!(viewer_size(f32::NAN, 792.0, 800), None);

    let expected = GeometryRequest { id: 4, index: 2, revision: 9 };
    let geometry = |id: u64, text: &str, angle: u16| {
        let geometry = crate::text_geometry::PageTextGeometry {
            id,
            page: 2,
            revision: 9,
            status: "ok",
            truncated: false,
            characters: vec![crate::text_geometry::TextCharacter { text: text.into(), bounds: None, angle }],
            reason: None,
        };
        payload_of(&frame(kind::TEXT_GEOMETRY, 1, |writer| put_geometry(writer, &geometry)))
    };
    assert!(decode(&geometry(4, "A", 90), |reader| get_geometry(reader, expected)).is_ok());
    assert!(decode(&geometry(5, "A", 90), |reader| get_geometry(reader, expected)).is_err(), "another document's geometry");
    assert!(decode(&geometry(4, "AB", 90), |reader| get_geometry(reader, expected)).is_err(), "two characters in one");
    assert!(decode(&geometry(4, "A", 360), |reader| get_geometry(reader, expected)).is_err());
    let mut inspected = geometry(4, "A", 0);
    inspected.extend_from_slice(&2_u32.to_le_bytes());
    inspected.extend_from_slice(&[0, 0]);
    assert!(decode(&inspected, |reader| get_inspected(reader, expected)).is_err(), "bounds count differs from characters");

    let bookmark = |depth: usize| crate::pdf_engine::EngineBookmark { title: Some("t".into()), depth, page_index: Some(0), destination: None };
    let outline = payload_of(&frame(kind::BOOKMARKS, 1, |writer| put_bookmarks(writer, &[bookmark(0), bookmark(1)])));
    assert_eq!(decode(&outline, |reader| get_bookmarks(reader, 2)).unwrap().len(), 2);
    assert!(decode(&outline, |reader| get_bookmarks(reader, 1)).is_err(), "more entries than asked for");
    let deep = payload_of(&frame(kind::BOOKMARKS, 1, |writer| put_bookmarks(writer, &[bookmark(0), bookmark(5)])));
    assert!(decode(&deep, |reader| get_bookmarks(reader, 2)).is_err());

    let properties = crate::document_properties::DocumentProperties {
        version: Some("1.7".into()),
        source_size_bytes: 1000,
        page_count: 2,
        metadata: vec![crate::document_properties::MetadataEntry { name: "title", value: "x".into(), truncated: false }],
        page_dimensions: vec![crate::document_properties::PageDimensions { width_points: 612.0, height_points: 792.0, count: 2 }],
        page_dimensions_truncated: false,
        security: crate::document_properties::SecurityProperties { encrypted: Some(false), handler_revision: None, print_high_quality: None, print_low_quality_only: None, modify_contents: None, assemble_document: None, fill_existing_forms: None },
        reported_signature_count: Some(0),
        signature_validation: "not_performed",
    };
    let encoded = payload_of(&frame(kind::PROPERTIES, 1, |writer| put_properties(writer, &properties)));
    let decoded = decode(&encoded, |reader| get_properties(reader, 1000, 2)).unwrap();
    assert_eq!(serde_json::to_string(&decoded).unwrap(), serde_json::to_string(&properties).unwrap());
    assert!(decode(&encoded, |reader| get_properties(reader, 999, 2)).is_err(), "another source size");
    assert!(decode(&encoded, |reader| get_properties(reader, 1000, 3)).is_err(), "groups do not add up");

    let rects = |rect: crate::comments::DisplayRect| payload_of(&frame(kind::DISPLAYED_NOTE_RECTS, 1, |writer| put_display_rects(writer, &[Some(rect), None])));
    let inside = crate::comments::DisplayRect { x: 0.1, y: 0.1, width: 0.5, height: 0.5 };
    assert!(decode(&rects(inside.clone()), |reader| get_display_rects(reader, 2)).is_ok());
    assert!(decode(&rects(inside), |reader| get_display_rects(reader, 3)).is_err());
    assert!(decode(&rects(crate::comments::DisplayRect { x: 0.8, y: 0.1, width: 0.5, height: 0.5 }), |reader| get_display_rects(reader, 2)).is_err());

    let field = |name: &str| crate::forms::FormField { field_id: String::new(), name: name.into(), page: 0, value: String::new(), max_length: None, checked: None, on_state: None, radio: None, choice: None };
    let fields = [field("a"), field("b")];
    let values = |entries: &[(&str, Option<&str>)]| {
        let mut writer = FrameWriter::new(kind::FORM_VALUES, 1);
        writer.u8(1);
        writer.count(entries.len());
        for (name, value) in entries {
            writer.str(name);
            writer.opt(*value, FrameWriter::str);
        }
        payload_of(&writer.finish())
    };
    assert_eq!(decode(&values(&[("a", Some("1"))]), |reader| get_form_values(reader, &fields)).unwrap().unwrap().len(), 1);
    assert!(decode(&values(&[("c", None)]), |reader| get_form_values(reader, &fields)).is_err(), "a field nobody asked about");
    assert!(decode(&values(&[("a", None), ("a", None)]), |reader| get_form_values(reader, &fields)).is_err(), "the same field twice");
}

const EQUIVALENCE_CHILD: &str = "SM5_HELPER_EQUIVALENCE_CHILD";
const EQUIVALENCE_NAME: &str = "pdf_helper::tests::remote_engine_matches_in_process_engine_on_the_fixtures";

/// Every `EngineDocument` method through the helper must give exactly what PDFium in process gives:
/// byte-identical BGRA and viewer PNG renders (both render modes, every quarter turn, with and without a
/// crop), and equal sizes, text, geometry, bookmarks, properties, page labels and form values. Runs in a
/// child process so this test binary binds PDFium in process only once per process.
#[test]
fn remote_engine_matches_in_process_engine_on_the_fixtures() {
    if std::env::var_os(EQUIVALENCE_CHILD).is_none() {
        let output = crate::child_job::test_output(std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", EQUIVALENCE_NAME, "--nocapture", "--test-threads=1"])
            .env(EQUIVALENCE_CHILD, "1"))
            .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(output.status.success() && stdout.contains("helper-equivalence-ok"), "child failed:\n{stdout}\n{}", String::from_utf8_lossy(&output.stderr));
        if let Some(line) = stdout.lines().find(|line| line.contains("helper-equivalence-ok")) {
            println!("{line}");
        }
        return;
    }
    let local = InProcessEngine::bind(library()).unwrap();
    let remote = RemotePdfEngine::start(test_config(crate::open_limits::HELPER_MEMORY_LIMIT)).unwrap();
    let files = [
        root().join("resources/welcome.pdf"),
        root().join("../test-corpus/synthetic-text-1500.pdf"),
        root().join("../test-corpus/synthetic-scan-98.pdf"),
        root().join("tests/fixtures/reportlab-mixed-fields.pdf"),
        root().join("tests/fixtures/reportlab-plain-fields.pdf"),
        root().join("tests/fixtures/reportlab-radio-fields.pdf"),
        root().join("tests/fixtures/reportlab-choice-fields.pdf"),
        root().join("tests/fixtures/reportlab-page-labels.pdf"),
    ];
    let (mut renders, mut checks) = (0_usize, 0_usize);
    let mut same = |what: String, left: String, right: String| {
        assert!(left == right, "{what} differs:\nin process: {left}\nhelper:     {right}");
        checks += 1;
    };
    let invalid = std::fs::read(root().join("../test-corpus/invalid.pdf")).unwrap();
    let (left, right) = (local.load_from_vec(invalid.clone(), None).map(|_| ()).unwrap_err(), remote.load_from_vec(invalid, None).map(|_| ()).unwrap_err());
    same("invalid.pdf load error".into(), format!("{left} {}", left.is_password()), format!("{right} {}", right.is_password()));
    for (allowed, permissions) in [(false, lopdf::Permissions::all() - lopdf::Permissions::COPYABLE), (true, lopdf::Permissions::all())] {
        let bytes = crate::xref_resolve::tests::encrypted_page(permissions);
        let (left, right) = (local.load_from_vec(bytes.clone(), None).unwrap(), remote.load_from_slice(&bytes, None).unwrap());
        assert_eq!((left.is_unprotected(), left.text_extraction_allowed()), (false, allowed), "generated encrypted fixture, copy allowed: {allowed}");
        same(format!("encrypted, copy allowed {allowed}, permission"), format!("{} {}", left.is_unprotected(), left.text_extraction_allowed()), format!("{} {}", right.is_unprotected(), right.text_extraction_allowed()));
    }
    for path in files {
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        let bytes = std::fs::read(&path).unwrap();
        let (left, right) = (local.load_from_vec(bytes.clone(), None).unwrap(), remote.load_from_slice(&bytes, None).unwrap());
        same(format!("{name} pages"), format!("{} {} {}", left.page_count(), left.is_unprotected(), left.text_extraction_allowed()), format!("{} {} {}", right.page_count(), right.is_unprotected(), right.text_extraction_allowed()));
        let sizes = |document: &dyn Fn() -> Result<Vec<crate::pdf_engine::PageSize>, String>| format!("{:?}", document().map(|sizes| sizes.iter().map(|size| (size.width.to_bits(), size.height.to_bits())).collect::<Vec<_>>()));
        same(format!("{name} page sizes"), sizes(&|| left.page_sizes()), sizes(&|| right.page_sizes()));
        same(format!("{name} labels"), format!("{:?}", left.engine_page_labels()), format!("{:?}", right.engine_page_labels()));
        let outline = |document: &dyn EngineDocument| {
            let mut entries = Vec::new();
            let result = document.bookmarks(1001, &mut |bookmark| {
                entries.push(format!("{:?}|{}|{:?}|{:?}", bookmark.title, bookmark.depth, bookmark.page_index, bookmark.destination));
                Ok(std::ops::ControlFlow::Continue(()))
            });
            format!("{result:?} {entries:?}")
        };
        same(format!("{name} bookmarks"), outline(&left), outline(&right));
        let dimensions = left.page_sizes().unwrap().iter().map(|size| (size.width, size.height)).collect::<Vec<_>>();
        let properties = |document: &dyn EngineDocument| format!("{:?}", document.properties(bytes.len(), &dimensions).map(|properties| serde_json::to_string(&properties).unwrap()));
        same(format!("{name} properties"), properties(&left), properties(&right));
        let session = crate::editor::EditSession::new(bytes.clone(), 1);
        let fields = crate::forms::FormDocument::query(&session, 1).fields;
        let form = |document: &dyn EngineDocument| {
            let mut values = document.form_values(&fields).map(|values| values.into_iter().collect::<Vec<_>>());
            if let Some(values) = &mut values {
                values.sort();
            }
            format!("{values:?}")
        };
        same(format!("{name} form values ({} fields)", fields.len()), form(&left), form(&right));
        for source in 0..left.page_count().min(2) {
            let page = format!("{name} page {source}");
            same(format!("{page} source size"), format!("{:?}", left.source_page_size(source)), format!("{:?}", right.source_page_size(source)));
            same(format!("{page} thumbnail"), format!("{:?}", left.thumbnail_check(source)), format!("{:?}", right.thumbnail_check(source)));
            same(format!("{page} source text"), format!("{:?}", left.visible_text(PageTarget::Source(source))), format!("{:?}", right.visible_text(PageTarget::Source(source))));
            let inspected = |document: &dyn EngineDocument| format!("{:?}", document.text_geometry_with_source(PageTarget::Source(source), 7, source as u16, 3).map(|inspected| (serde_json::to_string(&inspected.geometry).unwrap(), format!("{:?}", inspected.source_bounds))));
            same(format!("{page} inspected geometry"), inspected(&left), inspected(&right));
            let (width, height) = left.source_page_size(source).unwrap();
            let crop = CropBox { left: width * 0.1, bottom: height * 0.2, right: width * 0.8, top: height * 0.9 };
            for turns in 0..4 {
                for crop in [None, Some(crop)] {
                    let spec = PageSpec { source, turns, crop, notes: Vec::new() };
                    let case = format!("{page} turns {turns} crop {}", crop.is_some());
                    same(format!("{case} planned size"), format!("{:?}", left.planned_page_size(&spec)), format!("{:?}", right.planned_page_size(&spec)));
                    let viewer = |document: &dyn EngineDocument| document.render_viewer_png(&spec, 700).map(|png| (png.width, png.height, png.png));
                    let (left_png, right_png) = (viewer(&left).unwrap(), viewer(&right).unwrap());
                    assert!(left_png == right_png, "{case} viewer PNG differs");
                    renders += 1;
                    same(format!("{case} text"), format!("{:?}", left.visible_text(PageTarget::Planned(&spec))), format!("{:?}", right.visible_text(PageTarget::Planned(&spec))));
                    let geometry = |document: &dyn EngineDocument| format!("{:?}", document.text_geometry(PageTarget::Planned(&spec), 7, source as u16, 3).map(|geometry| serde_json::to_string(&geometry).unwrap()));
                    same(format!("{case} geometry"), geometry(&left), geometry(&right));
                    same(format!("{case} bookmark view"), format!("{:?}", left.bookmark_page_view(&spec)), format!("{:?}", right.bookmark_page_view(&spec)));
                    let rect = crate::service::CropRect { x: 0.1, y: 0.2, width: 0.5, height: 0.4 };
                    same(format!("{case} displayed crop"), format!("{:?}", left.displayed_crop(&spec, rect)), format!("{:?}", right.displayed_crop(&spec, rect)));
                    let notes = [crop.unwrap_or(CropBox { left: 10.0, bottom: 10.0, right: 200.0, top: 300.0 }), CropBox { left: -50.0, bottom: -50.0, right: -10.0, top: -10.0 }];
                    same(format!("{case} note rects"), format!("{:?}", left.displayed_note_rects(&spec, &notes)), format!("{:?}", right.displayed_note_rects(&spec, &notes)));
                    for mode in [RenderMode::Print, RenderMode::Viewer, RenderMode::Plain] {
                        let (pixel_width, pixel_height) = (613_u32, 457_u32);
                        let mut expected = vec![0; (pixel_width * pixel_height * 4) as usize];
                        let mut actual = vec![1; expected.len()];
                        left.render_bgra_into(PageTarget::Planned(&spec), mode, pixel_width, pixel_height, &mut expected).unwrap();
                        right.render_bgra_into(PageTarget::Planned(&spec), mode, pixel_width, pixel_height, &mut actual).unwrap();
                        assert!(expected == actual, "{case} {mode:?} BGRA differs");
                        renders += 1;
                    }
                }
            }
        }
        let mut wrong = vec![0; 16];
        assert!(matches!(right.render_bgra_into(PageTarget::Source(0), RenderMode::Plain, 10, 10, &mut wrong), Err(RenderError::Layout)));
    }
    assert_eq!(remote.stats().spawns, 1, "no helper restarted during the comparison");
    println!("helper-equivalence-ok renders={renders} checks={checks}");
}

// Helper step 4: T1, T2 (memory past the cap, from legitimate large content under a cap set between a
// measured baseline and the measured need) and the T15/T16 measurement harnesses (ignored; run on demand).

const MIB: usize = 1024 * 1024;

/// A one-page PDF showing a `side` x `side` RGB photo-like gradient, JPEG-compressed (what a scan page is).
fn photo_page(side: u32) -> Vec<u8> {
    use lopdf::{dictionary, Document, Object, Stream};
    let mut rgb = Vec::with_capacity((side * side * 3) as usize);
    for y in 0..side {
        for x in 0..side {
            rgb.extend_from_slice(&[(x * 255 / side) as u8, (y * 255 / side) as u8, ((x + y) * 127 / side) as u8]);
        }
    }
    let mut jpeg = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg, 85).encode(&rgb, side, side, image::ExtendedColorType::Rgb8).unwrap();
    let mut document = Document::with_version("1.7");
    let pages = document.new_object_id();
    let image = document.add_object(Stream::new(
        dictionary! { "Type" => "XObject", "Subtype" => "Image", "Width" => side as i64, "Height" => side as i64, "ColorSpace" => "DeviceRGB", "BitsPerComponent" => 8, "Filter" => "DCTDecode" },
        jpeg,
    ));
    let content = document.add_object(Stream::new(dictionary! {}, b"q 600 0 0 600 0 0 cm /Im0 Do Q".to_vec()));
    let page = document.add_object(dictionary! {
        "Type" => "Page", "Parent" => pages, "Contents" => content, "MediaBox" => vec![0.into(), 0.into(), 600.into(), 600.into()],
        "Resources" => dictionary! { "XObject" => dictionary! { "Im0" => image } },
    });
    document.objects.insert(pages, Object::Dictionary(dictionary! { "Type" => "Pages", "Kids" => vec![Object::Reference(page)], "Count" => 1 }));
    let catalog = document.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages });
    document.trailer.set("Root", catalog);
    let mut bytes = Vec::new();
    document.save_to(&mut bytes).unwrap();
    bytes
}

/// A PDF with `count` letter pages in one flat page tree sharing one small content stream.
fn many_pages(count: usize) -> Vec<u8> {
    use lopdf::{dictionary, Document, Object, Stream};
    let mut document = Document::with_version("1.7");
    let pages = document.new_object_id();
    let content = document.add_object(Stream::new(dictionary! {}, b"0 0 m 100 100 l S".to_vec()));
    let kids = (0..count).map(|_| Object::Reference(document.add_object(dictionary! { "Type" => "Page", "Parent" => pages, "Contents" => content }))).collect::<Vec<_>>();
    document.objects.insert(pages, Object::Dictionary(dictionary! { "Type" => "Pages", "Kids" => kids, "Count" => count as i64, "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()] }));
    let catalog = document.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages });
    document.trailer.set("Root", catalog);
    let mut bytes = Vec::new();
    document.save_to(&mut bytes).unwrap();
    bytes
}

fn peak(engine: &RemotePdfEngine) -> usize {
    engine.stats().peak_commit.unwrap()
}

/// A cap halfway between what the helper needs for the welcome PDF and what `work` needs on top, measured
/// in a helper with room to spare. `work` gets a fresh engine and must leave the large allocation done.
fn cap_between(work: impl Fn(&RemotePdfEngine)) -> (usize, usize, usize) {
    let engine = RemotePdfEngine::start(test_config(TEST_CAP)).unwrap();
    let welcome_document = engine.load_from_vec(welcome(), None).unwrap();
    render(&welcome_document);
    let baseline = peak(&engine);
    work(&engine);
    let needed = peak(&engine);
    assert!(needed > baseline + 16 * MIB, "the large work needs only {} MiB more than the baseline {} MiB", (needed - baseline) / MIB, baseline / MIB);
    (baseline, needed, baseline + (needed - baseline) / 2)
}

/// T1: a render whose decoding needs more memory than the helper may use fails with the memory message;
/// nothing from the capped helper (no blank page) reaches the caller; the document is set aside; another
/// document in the same engine renders exactly as before once the helper is replaced.
#[test]
fn t1_a_render_past_the_cap_fails_and_other_documents_recover() {
    let photo = photo_page(3_000);
    let side = 3_000_u32;
    let render_photo = |document: &RemoteDocument<'_>, out: &mut [u8]| document.render_bgra_into(PageTarget::Source(0), RenderMode::Viewer, side, side, out);
    let (baseline, needed, cap) = cap_between(|engine| {
        let document = engine.load_from_vec(photo.clone(), None).unwrap();
        let mut pixels = vec![0; (side * side * 4) as usize];
        render_photo(&document, &mut pixels).unwrap();
        assert!(pixels.chunks_exact(4).any(|pixel| pixel != [255; 4]));
    });
    println!("step4: t1 baseline {} MiB, photo render needs {} MiB, cap {} MiB", baseline / MIB, needed / MIB, cap / MIB);
    let engine = RemotePdfEngine::start(test_config(cap)).unwrap();
    let other = engine.load_from_vec(welcome(), None).unwrap();
    let before = render(&other);
    let document = engine.load_from_vec(photo, None).unwrap();
    let mut pixels = vec![0x5a_u8; (side * side * 4) as usize];
    let error = render_photo(&document, &mut pixels).unwrap_err();
    assert!(matches!(&error, RenderError::Render(message) if *message == HelperFailure::LimitViolation(LimitViolation::ProcessMemory).message()), "{error:?}");
    assert!(pixels.iter().all(|byte| *byte == 0x5a), "no blank or partial page reaches the caller");
    assert!(matches!(engine.stats().last_failure, Some(HelperFailure::LimitViolation(_))));
    assert!(document.is_suspect());
    let started = Instant::now();
    assert_eq!(render(&other), before, "the other document re-renders identically in the new helper");
    println!("step4: t1 other document re-rendered {:?} after the limit", started.elapsed());
}

/// T2: opening a document whose page tree needs more memory than the helper may use fails with the
/// memory message, leaves nothing open in the helper, and other documents keep working.
#[test]
fn t2_an_open_past_the_cap_fails_and_other_documents_recover() {
    let big = many_pages(65_536);
    let (baseline, needed, cap) = cap_between(|engine| {
        let document = engine.load_from_vec(big.clone(), None).unwrap();
        assert_eq!(document.page_sizes().unwrap().len(), 65_536);
    });
    println!("step4: t2 baseline {} MiB, 65,536-page open needs {} MiB, cap {} MiB", baseline / MIB, needed / MIB, cap / MIB);
    let engine = RemotePdfEngine::start(test_config(cap)).unwrap();
    let other = engine.load_from_vec(welcome(), None).unwrap();
    let before = render(&other);
    let opened = engine.load_from_vec(big, None).map_err(|error| error.to_string()).and_then(|document| document.page_sizes().map(|_| ()));
    assert_eq!(opened, Err(HelperFailure::LimitViolation(LimitViolation::ProcessMemory).message()));
    assert!(matches!(engine.stats().last_failure, Some(HelperFailure::LimitViolation(_))));
    assert_eq!(render(&other), before);
}

fn listed_files(variable: &str) -> Vec<PathBuf> {
    let Some(list) = std::env::var_os(variable) else { return Vec::new() };
    std::fs::read_to_string(list).unwrap().lines().map(str::trim).filter(|line| !line.is_empty()).map(PathBuf::from).collect()
}

/// T15 (run on demand: `SM5_HELPER_REAL=<list of PDF paths>`, read only): every listed file opens through
/// the helper with the same page count and sizes as in process, and page 1 at 1,600 px is the same PNG.
#[test]
#[ignore]
fn t15_real_files_open_and_render_identically() {
    let files = listed_files("SM5_HELPER_REAL");
    let local = InProcessEngine::bind(library()).unwrap();
    let remote = RemotePdfEngine::start(test_config(crate::open_limits::HELPER_MEMORY_LIMIT)).unwrap();
    let (mut identical, mut refused_alike, mut pages) = (0, 0, 0);
    for (index, path) in files.iter().enumerate() {
        let bytes = std::fs::read(path).unwrap();
        let (left, right) = (local.load_from_vec(bytes.clone(), None), remote.load_from_vec(bytes, None));
        let (left, right) = match (left, right) {
            (Ok(left), Ok(right)) => (left, right),
            (Err(left), Err(right)) => {
                assert_eq!(left.to_string(), right.to_string(), "file {}: different load errors", index + 1);
                refused_alike += 1;
                continue;
            }
            (left, right) => panic!("file {}: in process {:?}, helper {:?}", index + 1, left.err().map(|error| error.to_string()), right.err().map(|error| error.to_string())),
        };
        let sizes = |document: &dyn EngineDocument| format!("{:?}", document.page_sizes().map(|sizes| sizes.iter().map(|size| (size.width.to_bits(), size.height.to_bits())).collect::<Vec<_>>()));
        assert_eq!(sizes(&left), sizes(&right), "file {}: page sizes differ", index + 1);
        let first = spec(0);
        let png = |document: &dyn EngineDocument| document.render_viewer_png(&first, 1_600).map(|png| png.png);
        assert!(png(&left).unwrap() == png(&right).unwrap(), "file {}: page 1 at 1,600 px differs", index + 1);
        pages += left.page_count();
        identical += 1;
    }
    assert_eq!(remote.stats().spawns, 1, "no helper restarted");
    println!("step4: t15 files={} identical={identical} refused_alike={refused_alike} pages={pages} helper_peak_mib={}", files.len(), peak(&remote) / MIB);
}

fn percentile(values: &mut [f64], fraction: f64) -> f64 {
    values.sort_by(f64::total_cmp);
    values[((values.len() - 1) as f64 * fraction).round() as usize]
}

/// T16 (run on demand, release build: `SM5_HELPER_PERF=<list of PDF paths>`, read only): viewer render
/// time in process vs through the helper at 800, 1,600 and 3,000 px on the fixtures and the listed files
/// (15 timed renders each after one warm-up), helper start time and idle commit, and restart plus re-render
/// of the active document.
#[test]
#[ignore]
fn t16_helper_performance_budget() {
    let mut files = vec![root().join("resources/welcome.pdf"), root().join("../test-corpus/synthetic-text-1500.pdf"), root().join("../test-corpus/synthetic-scan-98.pdf")];
    let fixtures = files.len();
    files.extend(listed_files("SM5_HELPER_PERF"));
    let local = InProcessEngine::bind(library()).unwrap();
    let mut starts = Vec::new();
    for _ in 0..5 {
        let started = Instant::now();
        let engine = RemotePdfEngine::start(HelperConfig::production()).unwrap();
        starts.push(started.elapsed().as_secs_f64() * 1000.0);
        drop(engine);
    }
    let remote = RemotePdfEngine::start(HelperConfig::production()).unwrap();
    println!("step4: t16 helper start ms p50 {:.1} max {:.1}; idle commit {:.1} MiB", percentile(&mut starts.clone(), 0.5), starts.iter().cloned().fold(0.0, f64::max), peak(&remote) as f64 / MIB as f64);
    let first = spec(0);
    let (mut within, mut rows) = (0, 0);
    for (index, path) in files.iter().enumerate() {
        let label = if index < fixtures { path.file_name().unwrap().to_string_lossy().into_owned() } else { format!("real-{:02}", index - fixtures + 1) };
        let bytes = std::fs::read(path).unwrap();
        let (left, right) = (local.load_from_vec(bytes.clone(), None).unwrap(), remote.load_from_vec(bytes, None).unwrap());
        for width in [800, 1_600, 3_000] {
            let time = |document: &dyn EngineDocument| {
                document.render_viewer_png(&first, width).unwrap();
                let mut samples = (0..15).map(|_| { let started = Instant::now(); document.render_viewer_png(&first, width).unwrap(); started.elapsed().as_secs_f64() * 1000.0 }).collect::<Vec<_>>();
                (percentile(&mut samples, 0.5), percentile(&mut samples, 0.95))
            };
            let ((local_p50, local_p95), (remote_p50, remote_p95)) = (time(&left), time(&right));
            let ok = remote_p50 - local_p50 <= f64::max(5.0, local_p50 * 0.05) && remote_p95 - local_p95 <= f64::max(15.0, local_p95 * 0.10);
            rows += 1;
            within += usize::from(ok);
            println!("step4: t16 {label} {width}px in-process p50 {local_p50:.1} p95 {local_p95:.1} ms; helper p50 {remote_p50:.1} p95 {remote_p95:.1} ms; overhead p50 {:+.1} p95 {:+.1} ms; budget {}", remote_p50 - local_p50, remote_p95 - local_p95, if ok { "met" } else { "MISSED" });
        }
    }
    let active = remote.load_from_vec(welcome(), None).unwrap();
    let before = active.render_viewer_png(&first, 1_600).unwrap().png;
    remote.fault(Fault::Exit(3), LONG).unwrap_err();
    let started = Instant::now();
    let after = active.render_viewer_png(&first, 1_600).unwrap().png;
    let restart = started.elapsed();
    assert!(before == after);
    println!("step4: t16 budget met on {within}/{rows} rows; restart + re-render of the active document {restart:?}");
}
