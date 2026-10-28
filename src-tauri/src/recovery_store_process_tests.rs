use super::*;
use crate::recovery_journal::{RecoveryEditState, RecoveryPage};
use std::{
    fs::OpenOptions,
    io::Write,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};

const ROOT_ENV: &str = "SMACROBAT_STORE_PROCESS_ROOT";
const NONCE_ENV: &str = "SMACROBAT_STORE_PROCESS_NONCE";
const MODE_ENV: &str = "SMACROBAT_STORE_PROCESS_MODE";
const SEED_ENV: &str = "SMACROBAT_STORE_PROCESS_SEED";
const GENERATION_ENV: &str = "SMACROBAT_STORE_PROCESS_GENERATION";
const READY_ENV: &str = "SMACROBAT_STORE_PROCESS_READY";
const RELEASE_ENV: &str = "SMACROBAT_STORE_PROCESS_RELEASE";
const RESULT_ENV: &str = "SMACROBAT_STORE_PROCESS_RESULT";
const MAX_RECEIPT_BYTES: u64 = 1024;

fn plain_directory(path: &Path) -> PathBuf {
    let metadata = std::fs::symlink_metadata(path).expect("store-process directory metadata");
    assert!(
        metadata.is_dir() && !metadata.file_type().is_symlink(),
        "store-process directory must be plain"
    );
    assert_eq!(
        metadata.file_attributes() & REPARSE_POINT,
        0,
        "store-process directory must not be a reparse point"
    );
    std::fs::canonicalize(path).expect("canonical store-process directory")
}

fn plain_contained_file(path: &Path, root: &Path) -> PathBuf {
    let metadata = std::fs::symlink_metadata(path).expect("store-process file metadata");
    assert!(
        metadata.is_file() && !metadata.file_type().is_symlink(),
        "store-process file must be plain"
    );
    assert_eq!(
        metadata.file_attributes() & REPARSE_POINT,
        0,
        "store-process file must not be a reparse point"
    );
    assert!(
        metadata.len() <= MAX_RECEIPT_BYTES,
        "store-process receipt exceeded its bound"
    );
    let canonical = std::fs::canonicalize(path).expect("canonical store-process file");
    assert!(
        canonical.starts_with(root),
        "store-process file escaped its owned root"
    );
    canonical
}

fn create_receipt(path: &Path, value: &[u8]) {
    assert!(value.len() as u64 <= MAX_RECEIPT_BYTES);
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .expect("create store-process receipt");
    file.write_all(value).expect("write store-process receipt");
    file.sync_all().expect("synchronize store-process receipt");
}

fn bounded_receipt(path: &Path, root: &Path) -> Vec<u8> {
    let path = plain_contained_file(path, root);
    std::fs::read(path).expect("read store-process receipt")
}

fn fixture_root() -> PathBuf {
    let root = PathBuf::from(std::env::var_os(ROOT_ENV).expect("store-process root"));
    assert!(root.is_absolute());
    let root = plain_directory(&root);
    let nonce = std::env::var(NONCE_ENV).expect("store-process nonce");
    assert_eq!(
        bounded_receipt(&root.join(".owned"), &root),
        nonce.as_bytes()
    );
    root
}

fn record(seed: u8, generation: u64) -> RecoveryRecord {
    RecoveryRecord {
        active: true,
        generation,
        revision: generation,
        current_page: 0,
        source_pages: 1,
        source: SourceIdentity {
            bytes: 4096 + u64::from(seed),
            sha256: [seed; 32],
        },
        edit_state: RecoveryEditState {
            pages: vec![RecoveryPage {
                source: 0,
                turns: 0,
                crop: None,
                annotations: vec![],
            }],
        },
    }
}

fn wait_for(path: &Path, root: &Path, label: &str) {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if path.exists() {
            plain_contained_file(path, root);
            return;
        }
        assert!(Instant::now() < deadline, "timed out waiting for {label}");
        thread::sleep(Duration::from_millis(10));
    }
}

#[test]
#[ignore = "spawned by cross_process_lock_serializes_admission_and_generations"]
fn store_process_child() {
    let root = fixture_root();
    let mode = std::env::var(MODE_ENV).expect("store-process mode");
    let seed = std::env::var(SEED_ENV)
        .expect("store-process seed")
        .parse::<u8>()
        .expect("numeric seed");
    let generation = std::env::var(GENERATION_ENV)
        .expect("store-process generation")
        .parse::<u64>()
        .expect("numeric generation");
    let result = PathBuf::from(std::env::var_os(RESULT_ENV).expect("store-process result"));
    assert_eq!(result.parent(), Some(root.as_path()));
    if mode == "hold" {
        let ready = PathBuf::from(std::env::var_os(READY_ENV).expect("store-process ready"));
        let release = PathBuf::from(std::env::var_os(RELEASE_ENV).expect("store-process release"));
        assert_eq!(ready.parent(), Some(root.as_path()));
        assert_eq!(release.parent(), Some(root.as_path()));
        let guard = acquire_writer_lock(&root).expect("acquire held writer lock");
        create_receipt(&ready, b"ready");
        wait_for(&release, &root, "owned release marker");
        drop(guard);
    } else {
        assert_eq!(mode, "write");
    }
    let outcome = match write_recovery(&root, &record(seed, generation)) {
        Ok(_) => "ok".to_owned(),
        Err(error) => format!("err:{error}"),
    };
    create_receipt(&result, outcome.as_bytes());
}

fn child(
    root: &Path,
    nonce: &str,
    mode: &str,
    seed: u8,
    generation: u64,
    result: &Path,
    ready: Option<&Path>,
    release: Option<&Path>,
) -> Child {
    let mut command = Command::new(std::env::current_exe().expect("current test executable"));
    command
        .args([
            "--exact",
            "recovery_store::process_tests::store_process_child",
            "--ignored",
            "--test-threads=1",
        ])
        .env_clear()
        .env(ROOT_ENV, root)
        .env(NONCE_ENV, nonce)
        .env(MODE_ENV, mode)
        .env(SEED_ENV, seed.to_string())
        .env(GENERATION_ENV, generation.to_string())
        .env(RESULT_ENV, result)
        .env("RUST_BACKTRACE", "0")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    if let Some(path) = ready {
        command.env(READY_ENV, path);
    }
    if let Some(path) = release {
        command.env(RELEASE_ENV, path);
    }
    for name in ["SystemRoot", "WINDIR", "TEMP", "TMP"] {
        if let Some(value) = std::env::var_os(name) {
            command.env(name, value);
        }
    }
    crate::child_job::test_spawn(&mut command).expect("spawn owned store-process child")
}

fn wait_child(mut child: Child, label: &str) {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        if let Some(status) = child.try_wait().expect("poll store-process child") {
            assert!(status.success(), "store-process child {label} failed");
            return;
        }
        if Instant::now() >= deadline {
            child.kill().expect("terminate timed-out owned child");
            let _ = child.wait();
            panic!("store-process child {label} exceeded its deadline");
        }
        thread::sleep(Duration::from_millis(10));
    }
}

fn random_nonce() -> String {
    use windows::Win32::Security::Cryptography::{
        BCryptGenRandom, BCRYPT_USE_SYSTEM_PREFERRED_RNG,
    };
    let mut random = [0u8; 16];
    let status = unsafe { BCryptGenRandom(None, &mut random, BCRYPT_USE_SYSTEM_PREFERRED_RNG) };
    assert!(status.0 >= 0, "system random UUID generation failed");
    random[6] = (random[6] & 0x0f) | 0x40;
    random[8] = (random[8] & 0x3f) | 0x80;
    random.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn owned_root(parent: &Path, nonce: &str, label: &str) -> PathBuf {
    let root = parent.join(format!("smacrobat-store-process-{label}-{nonce}"));
    std::fs::create_dir(&root).expect("create store-process root");
    let root = plain_directory(&root);
    assert!(root.starts_with(parent));
    create_receipt(&root.join(".owned"), nonce.as_bytes());
    root
}

fn seed_cap(root: &Path) {
    for index in 0..(MAX_PERSISTENT_RECORDS - 1) {
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(root.join(format!("{index:064x}.smacrec")))
            .expect("create bounded cap seed");
    }
}

fn record_count(root: &Path) -> usize {
    std::fs::read_dir(root)
        .expect("read store-process root")
        .filter(|entry| {
            entry
                .as_ref()
                .ok()
                .and_then(|entry| entry.file_name().to_str().map(is_record_name))
                .unwrap_or(false)
        })
        .count()
}

#[test]
#[ignore = "uses fresh processes and retained owned fixtures"]
fn cross_process_lock_serializes_admission_and_generations() {
    let parent = plain_directory(&std::env::temp_dir());
    let nonce = random_nonce();

    let capped = owned_root(&parent, &nonce, "cap");
    seed_cap(&capped);
    let ready = capped.join("a.ready");
    let release = capped.join("a.release");
    let a_result = capped.join("a.result");
    let busy_result = capped.join("b-busy.result");
    let retry_result = capped.join("b-retry.result");
    let a = child(
        &capped,
        &nonce,
        "hold",
        0xe1,
        1,
        &a_result,
        Some(&ready),
        Some(&release),
    );
    wait_for(&ready, &capped, "writer A readiness");
    let busy = child(&capped, &nonce, "write", 0xe2, 1, &busy_result, None, None);
    wait_child(busy, "contending writer B");
    assert_eq!(
        bounded_receipt(&busy_result, &capped),
        b"err:Recovery storage is busy in another process."
    );
    create_receipt(&release, b"release");
    wait_child(a, "writer A");
    assert_eq!(bounded_receipt(&a_result, &capped), b"ok");
    let retry = child(&capped, &nonce, "write", 0xe2, 1, &retry_result, None, None);
    wait_child(retry, "writer B retry");
    assert_eq!(
        bounded_receipt(&retry_result, &capped),
        b"err:Recovery storage limit was reached."
    );
    assert_eq!(record_count(&capped), MAX_PERSISTENT_RECORDS);
    assert_eq!(
        read_recovery(&capped, record(0xe1, 1).source, None, 0).expect("read writer A"),
        Some(record(0xe1, 1))
    );
    assert!(read_recovery(&capped, record(0xe2, 1).source, None, 0)
        .expect("read rejected writer B")
        .is_none());

    let generations = owned_root(&parent, &nonce, "generation");
    let ready = generations.join("new.ready");
    let release = generations.join("new.release");
    let new_result = generations.join("new.result");
    let busy_result = generations.join("old-busy.result");
    let stale_result = generations.join("old-stale.result");
    let newer = child(
        &generations,
        &nonce,
        "hold",
        0xf1,
        2,
        &new_result,
        Some(&ready),
        Some(&release),
    );
    wait_for(&ready, &generations, "new generation readiness");
    let busy = child(
        &generations,
        &nonce,
        "write",
        0xf1,
        1,
        &busy_result,
        None,
        None,
    );
    wait_child(busy, "contending old generation");
    assert_eq!(
        bounded_receipt(&busy_result, &generations),
        b"err:Recovery storage is busy in another process."
    );
    create_receipt(&release, b"release");
    wait_child(newer, "new generation");
    assert_eq!(bounded_receipt(&new_result, &generations), b"ok");
    let stale = child(
        &generations,
        &nonce,
        "write",
        0xf1,
        1,
        &stale_result,
        None,
        None,
    );
    wait_child(stale, "stale generation retry");
    assert_eq!(
        bounded_receipt(&stale_result, &generations),
        b"err:Recovery record is stale."
    );
    assert_eq!(record_count(&generations), 1);
    assert_eq!(
        read_recovery(&generations, record(0xf1, 2).source, None, 0)
            .expect("read final generation"),
        Some(record(0xf1, 2))
    );
}
