use super::*;
use std::{
    fs::OpenOptions,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

const ROOT_ENV: &str = "SMACROBAT_RECOVERY_RESTART_ROOT";
const NONCE_ENV: &str = "SMACROBAT_RECOVERY_RESTART_NONCE";
const MAX_FIXTURE_BYTES: u64 = 32 * 1024 * 1024;

fn direct<T>(service: &PdfService, request: impl FnOnce(Reply<T>) -> Request) -> Result<T, String> {
    let (reply, receiver) = oneshot::channel();
    service.sender.send(request(reply)).map_err(|error| error.to_string())?;
    receiver.blocking_recv().map_err(|error| error.to_string())?
}

fn begin_open(service: &PdfService, path: PathBuf) -> Result<OpenResult, String> {
    direct(service, |reply| Request::BeginOpen(path, reply)).map(ReplyLease::accept)
}

fn bounded_read(path: &Path) -> Vec<u8> {
    let metadata = std::fs::metadata(path).expect("restart fixture metadata");
    assert!(metadata.is_file() && metadata.len() <= MAX_FIXTURE_BYTES, "restart fixture exceeded its read bound");
    std::fs::read(path).expect("restart fixture read")
}

fn plain_directory(path: &Path) -> PathBuf {
    let metadata = std::fs::symlink_metadata(path).expect("restart fixture directory metadata");
    assert!(metadata.is_dir() && !metadata.file_type().is_symlink(), "restart fixture directory must be plain");
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        assert_eq!(metadata.file_attributes() & 0x400, 0, "restart fixture directory must not be a reparse point");
    }
    std::fs::canonicalize(path).expect("canonical restart fixture directory")
}

fn contained(path: &Path, root: &Path) -> PathBuf {
    let canonical = std::fs::canonicalize(path).expect("canonical restart fixture path");
    assert!(canonical.starts_with(root), "restart fixture escaped its owned root");
    canonical
}

fn plain_contained_file(path: &Path, root: &Path) -> PathBuf {
    let metadata = std::fs::symlink_metadata(path).expect("restart fixture file metadata");
    assert!(metadata.is_file() && !metadata.file_type().is_symlink(), "restart fixture file must be plain");
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        assert_eq!(metadata.file_attributes() & 0x400, 0, "restart fixture file must not be a reparse point");
    }
    contained(path, root)
}

fn fixture_root() -> PathBuf {
    let root = PathBuf::from(std::env::var_os(ROOT_ENV).expect("restart fixture root must be supplied by the parent test"));
    assert!(root.is_absolute(), "restart fixture root must be absolute");
    let root = plain_directory(&root);
    let temp = plain_directory(&std::env::temp_dir());
    assert_eq!(root.parent(), Some(temp.as_path()), "restart fixture root must be a direct temporary-directory child");
    let nonce = std::env::var(NONCE_ENV).expect("restart fixture nonce must be supplied by the parent test");
    let marker = plain_contained_file(&root.join(".owned"), &root);
    assert_eq!(bounded_read(&marker), nonce.as_bytes(), "restart fixture ownership marker mismatch");
    root
}

fn fixture_paths() -> (PathBuf, PathBuf, PathBuf) {
    let root = fixture_root();
    let source = plain_contained_file(&root.join("source.pdf"), &root);
    let recovery = plain_directory(&root.join("recovery"));
    assert!(recovery.starts_with(&root), "recovery fixture escaped its owned root");
    (source, recovery, root.join("render.sha256"))
}

fn worker(recovery: PathBuf) -> PdfService {
    PdfService::start_worker(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("resources/pdfium/bin/pdfium.dll"),
        4096,
        Some(recovery),
    )
}

fn source_identity(source: &[u8]) -> crate::recovery_journal::SourceIdentity {
    recovery_source_identity(source).expect("source identity")
}

#[test]
#[ignore = "spawned by restart_recovery_survives_fresh_workers"]
fn restart_child_a_persists_without_close() {
    let (source_path, recovery, render_receipt) = fixture_paths();
    let source = bounded_read(&source_path);
    let expected_source = bounded_read(&PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("resources/welcome.pdf"));
    assert_eq!(Sha256::digest(&source), Sha256::digest(&expected_source));

    let service = worker(recovery.clone());
    let opened = match begin_open(&service, source_path.clone()).expect("open source") {
        OpenResult::Opened { document, recovery: None } => document,
        _ => panic!("fresh source unexpectedly offered recovery"),
    };
    let edited = direct(&service, |reply| Request::DurableEdit(
        opened.id,
        PageEdit::Rotate { pages: vec![0], clockwise: true },
        1,
        reply,
    )).expect("durable edit");
    assert!(edited.dirty && edited.revision == 1);
    let pixels = direct(&service, |reply| Request::Render(edited.id, 0, 240, reply)).expect("render edited page");
    let mut receipt = OpenOptions::new().write(true).create_new(true).open(&render_receipt).expect("create owned render receipt");
    receipt.write_all(&Sha256::digest(&pixels)).expect("write bounded render receipt");
    receipt.sync_all().expect("synchronize bounded render receipt");
    plain_contained_file(&render_receipt, &fixture_root());

    let record = crate::recovery_store::read_recovery(&recovery, source_identity(&source), None, 0)
        .expect("read active recovery").expect("active recovery record");
    assert!(record.active);
    assert_eq!((record.generation, record.revision, record.current_page), (1, edited.revision, 1));
    assert_eq!(Sha256::digest(bounded_read(&source_path)), Sha256::digest(source));
    // Intentionally exit without Close: the next phase must rely only on the published record.
}

#[test]
#[ignore = "spawned by restart_recovery_survives_fresh_workers"]
fn restart_child_b_keeps_and_undoes() {
    let (source_path, recovery, render_receipt) = fixture_paths();
    let source = bounded_read(&source_path);
    let expected_source = bounded_read(&PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("resources/welcome.pdf"));
    assert_eq!(Sha256::digest(&source), Sha256::digest(&expected_source));
    let expected_render = bounded_read(&plain_contained_file(&render_receipt, &fixture_root()));
    assert_eq!(expected_render.len(), 32);

    let service = worker(recovery.clone());
    let (clean, offer) = match begin_open(&service, source_path.clone()).expect("reopen source") {
        OpenResult::Opened { document, recovery: Some(offer) } => (document, offer),
        _ => panic!("fresh worker did not offer active recovery"),
    };
    assert_eq!((clean.revision, clean.dirty, offer.revision, offer.current_page), (0, false, 1, 1));
    let kept = direct(&service, |reply| Request::KeepRecoveredEdits(clean.id, clean.revision, reply)).expect("keep recovery");
    assert_eq!((kept.document.revision, kept.current_page), (offer.revision, offer.current_page));
    assert!(kept.document.dirty && kept.document.can_undo);
    let pixels = direct(&service, |reply| Request::Render(kept.document.id, 0, 240, reply)).expect("render kept page");
    assert_eq!(Sha256::digest(&pixels).as_slice(), expected_render.as_slice());

    let undone = direct(&service, |reply| Request::DurableEdit(
        kept.document.id,
        PageEdit::Undo,
        kept.current_page,
        reply,
    )).expect("durable undo");
    assert!(!undone.dirty && undone.can_redo);
    let record = crate::recovery_store::read_recovery(&recovery, source_identity(&source), None, 0)
        .expect("read tombstone").expect("inactive recovery record");
    assert!(!record.active);
    assert_eq!((record.generation, record.revision, record.current_page), (2, undone.revision, 0));
    assert_eq!(Sha256::digest(bounded_read(&source_path)), Sha256::digest(source));
}

#[test]
#[ignore = "spawned by restart_recovery_survives_fresh_workers"]
fn restart_child_c_observes_tombstone() {
    let (source_path, recovery, _) = fixture_paths();
    let source = bounded_read(&source_path);
    let expected_source = bounded_read(&PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("resources/welcome.pdf"));
    assert_eq!(Sha256::digest(&source), Sha256::digest(&expected_source));
    let record = crate::recovery_store::read_recovery(&recovery, source_identity(&source), None, 0)
        .expect("read final tombstone").expect("final recovery record");
    assert!(!record.active);
    assert_eq!((record.generation, record.revision, record.current_page), (2, 2, 0));

    let service = worker(recovery);
    match begin_open(&service, source_path.clone()).expect("open after tombstone") {
        OpenResult::Opened { document, recovery: None } => assert_eq!((document.revision, document.dirty), (0, false)),
        _ => panic!("inactive higher-generation recovery was offered"),
    }
    assert_eq!(Sha256::digest(bounded_read(&source_path)), Sha256::digest(source));
}

fn run_child(name: &str, root: &Path, nonce: &str) {
    let mut command = Command::new(std::env::current_exe().expect("current test executable"));
    command.args(["--exact", name, "--ignored", "--test-threads=1"])
        .env_clear()
        .env(ROOT_ENV, root)
        .env(NONCE_ENV, nonce)
        .env("RUST_BACKTRACE", "0")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    for name in ["SystemRoot", "WINDIR", "TEMP", "TMP"] {
        if let Some(value) = std::env::var_os(name) { command.env(name, value); }
    }
    let mut child = crate::child_job::test_spawn(&mut command).expect("spawn owned restart child");
    let deadline = Instant::now() + Duration::from_secs(120);
    loop {
        if let Some(status) = child.try_wait().expect("poll owned restart child") {
            assert!(status.success(), "restart child {name} failed");
            return;
        }
        if Instant::now() >= deadline {
            child.kill().expect("terminate timed-out owned restart child");
            let _ = child.wait();
            panic!("restart child {name} exceeded its deadline");
        }
        thread::sleep(Duration::from_millis(25));
    }
}

#[test]
#[ignore = "uses three fresh PDFium worker processes and retained owned fixtures"]
fn restart_recovery_survives_fresh_workers() {
    let parent = plain_directory(&std::env::temp_dir());
    let mut random = [0u8; 16];
    #[cfg(windows)]
    {
        use windows::Win32::Security::Cryptography::{BCryptGenRandom, BCRYPT_USE_SYSTEM_PREFERRED_RNG};
        let status = unsafe { BCryptGenRandom(None, &mut random, BCRYPT_USE_SYSTEM_PREFERRED_RNG) };
        assert!(status.0 >= 0, "system random UUID generation failed");
    }
    #[cfg(not(windows))]
    panic!("restart recovery proof is Windows-only");
    random[6] = (random[6] & 0x0f) | 0x40;
    random[8] = (random[8] & 0x3f) | 0x80;
    let nonce = format!("{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}", random[0], random[1], random[2], random[3], random[4], random[5], random[6], random[7], random[8], random[9], random[10], random[11], random[12], random[13], random[14], random[15]);
    let root = parent.join(format!("smacrobat-recovery-restart-{nonce}"));
    std::fs::create_dir(&root).expect("create unique owned restart fixture");
    let root = plain_directory(&root);
    assert!(root.starts_with(&parent), "owned restart fixture escaped the temporary parent");
    let mut marker = OpenOptions::new().write(true).create_new(true).open(root.join(".owned")).expect("create ownership marker");
    marker.write_all(nonce.as_bytes()).expect("write ownership marker");
    marker.sync_all().expect("synchronize ownership marker");
    contained(&root.join(".owned"), &root);
    let recovery = root.join("recovery");
    std::fs::create_dir(&recovery).expect("create owned recovery fixture");
    let recovery = plain_directory(&recovery);
    assert!(recovery.starts_with(&root), "owned recovery fixture escaped its root");
    std::fs::copy(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("resources/welcome.pdf"),
        root.join("source.pdf"),
    ).expect("copy deterministic source fixture");
    plain_contained_file(&root.join("source.pdf"), &root);

    let prefix = "service::recovery_restart_tests";
    run_child(&format!("{prefix}::restart_child_a_persists_without_close"), &root, &nonce);
    run_child(&format!("{prefix}::restart_child_b_keeps_and_undoes"), &root, &nonce);
    run_child(&format!("{prefix}::restart_child_c_observes_tombstone"), &root, &nonce);
}
