use super::*;
use crate::service::{OpenResult, PdfService};
use lopdf::encryption::crypt_filters::{Aes128CryptFilter, Aes256CryptFilter, CryptFilter};
use lopdf::{EncryptionState, EncryptionVersion, Permissions};
use std::{collections::BTreeMap, path::{Path, PathBuf}, sync::{Arc, Mutex, MutexGuard}};

static SERVICE_TESTS: Mutex<()> = Mutex::new(());
fn serial() -> MutexGuard<'static, ()> { SERVICE_TESTS.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) }

#[derive(Clone, Copy, Debug, PartialEq)]
enum Flavor { R2, R3, R4, R6 }
const FLAVORS: [Flavor; 4] = [Flavor::R2, Flavor::R3, Flavor::R4, Flavor::R6];
const OWNER: &str = "owner pw";

impl Flavor {
    fn revision(self) -> i64 { match self { Flavor::R2 => 2, Flavor::R3 => 3, Flavor::R4 => 4, Flavor::R6 => 6 } }
}

fn root() -> PathBuf { PathBuf::from(env!("CARGO_MANIFEST_DIR")) }
fn plain() -> Vec<u8> { std::fs::read(root().join("resources/welcome.pdf")).unwrap() }

/// The welcome PDF encrypted by the given revision with distinct user and owner passwords and copying/editing denied.
fn encrypted(flavor: Flavor, user: &str, owner: &str) -> Vec<u8> {
    let mut pdf = lopdf::Document::load_mem(&plain()).unwrap();
    pdf.trailer.set("ID", vec![lopdf::Object::string_literal("owner-unlock-id"), lopdf::Object::string_literal("owner-unlock-id")]);
    let permissions = Permissions::PRINTABLE;
    let key = [0x42u8; 32];
    let encryption = match flavor {
        Flavor::R2 => EncryptionVersion::V1 { document: &pdf, owner_password: owner, user_password: user, permissions },
        Flavor::R3 => EncryptionVersion::V2 { document: &pdf, owner_password: owner, user_password: user, key_length: 128, permissions },
        Flavor::R4 => {
            let filter: Arc<dyn CryptFilter> = Arc::new(Aes128CryptFilter);
            EncryptionVersion::V4 { document: &pdf, encrypt_metadata: true, crypt_filters: BTreeMap::from([(b"StdCF".to_vec(), filter)]), stream_filter: b"StdCF".to_vec(), string_filter: b"StdCF".to_vec(), owner_password: owner, user_password: user, permissions }
        }
        Flavor::R6 => {
            let filter: Arc<dyn CryptFilter> = Arc::new(Aes256CryptFilter);
            EncryptionVersion::V5 { encrypt_metadata: true, crypt_filters: BTreeMap::from([(b"StdCF".to_vec(), filter)]), file_encryption_key: &key, stream_filter: b"StdCF".to_vec(), string_filter: b"StdCF".to_vec(), owner_password: owner, user_password: user, permissions }
        }
    };
    let state = EncryptionState::try_from(encryption).unwrap();
    pdf.encrypt(&state).unwrap();
    let mut bytes = Vec::new();
    pdf.save_to(&mut bytes).unwrap();
    // The fixture really is the revision it claims to be and really restricts copying.
    let check = lopdf::Document::load_mem(&bytes).unwrap();
    let dictionary_revision = check.encryption_state.as_ref().map(|state| state.revision()).or_else(|| check.get_encrypted().ok().and_then(|dictionary| dictionary.get(b"R").ok()?.as_i64().ok()));
    assert_eq!(dictionary_revision, Some(flavor.revision()), "{flavor:?} fixture has the wrong /R");
    bytes
}

fn contents(document: &lopdf::Document) -> Vec<Vec<u8>> {
    document.get_pages().values().map(|id| document.get_page_content(*id)).collect()
}

#[test]
fn user_password_recovery_returns_the_padded_user_password() {
    for flavor in [Flavor::R2, Flavor::R3] {
        let source = encrypted(flavor, "reader", OWNER);
        let document = lopdf::Document::load_mem(&source).unwrap();
        let dictionary = document.get_encrypted().unwrap();
        let owner_value = dictionary.get(b"O").unwrap().as_str().unwrap();
        let recovered = user_password_from_owner(OWNER.as_bytes(), owner_value, flavor.revision(), key_bytes(dictionary, flavor.revision()).unwrap());
        let mut expected = b"reader".to_vec();
        expected.extend_from_slice(&PAD[..26]);
        assert_eq!(recovered, expected, "{flavor:?}");
        assert_ne!(user_password_from_owner(b"wrong", owner_value, flavor.revision(), key_bytes(dictionary, flavor.revision()).unwrap()), expected, "{flavor:?}");
    }
}

#[test]
fn rc4_matches_the_published_test_vectors() {
    assert_eq!(rc4(b"Key", b"Plaintext"), [0xBB, 0xF3, 0x16, 0xE8, 0xD9, 0x40, 0xAF, 0x0A, 0xD3]);
    assert_eq!(rc4(b"Wiki", b"pedia"), [0x10, 0x21, 0xBF, 0x04, 0x20]);
    assert_eq!(rc4(b"Secret", b"Attack at dawn"), [0x45, 0xA0, 0x1F, 0x64, 0x5F, 0xC3, 0x5B, 0x38, 0x35, 0x52, 0x54, 0x4B, 0x9B, 0xF5]);
}

#[test]
fn the_owner_password_removes_security_for_every_revision_and_keeps_the_pages() {
    let original = contents(&lopdf::Document::load_mem(&plain()).unwrap());
    for flavor in FLAVORS {
        for user in ["", "reader"] {
            let source = encrypted(flavor, user, OWNER);
            let output = remove_security(&source, OWNER).unwrap_or_else(|error| panic!("{flavor:?} user {user:?}: {error}"));
            assert!(!output.windows(8).any(|window| window == b"/Encrypt"), "{flavor:?} user {user:?} still names /Encrypt");
            let document = lopdf::Document::load_mem(&output).unwrap();
            assert!(!document.is_encrypted() && document.encryption_state.is_none());
            assert_eq!(contents(&document), original, "{flavor:?} user {user:?} changed the page content");
        }
    }
}

#[test]
fn a_wrong_password_the_user_password_and_near_misses_are_refused() {
    for flavor in FLAVORS {
        for user in ["", "reader"] {
            let source = encrypted(flavor, user, OWNER);
            let mut refused = vec!["wrong", "owner", "OWNER PW", "owner pw ", " owner pw", "x"];
            if !user.is_empty() { refused.push(user); }
            for password in refused {
                assert_eq!(remove_security(&source, password), Err(INCORRECT.to_owned()), "{flavor:?} user {user:?} accepted {password:?}");
            }
        }
    }
}

#[test]
fn the_user_password_is_the_owner_password_only_when_they_are_the_same() {
    for flavor in FLAVORS {
        let source = encrypted(flavor, "same", "same");
        assert!(remove_security(&source, "same").is_ok(), "{flavor:?}");
        assert_eq!(remove_security(&source, "other"), Err(INCORRECT.to_owned()), "{flavor:?}");
    }
}

#[test]
fn long_and_non_ascii_owner_passwords_work() {
    let long = "an owner password that is longer than thirty-two bytes";
    let accented = "p\u{e4}ssw\u{f6}rd \u{e9}";
    for flavor in FLAVORS {
        for (user, owner) in [("reader", long), ("", long), ("reader", accented), ("", accented)] {
            let source = encrypted(flavor, user, owner);
            assert!(remove_security(&source, owner).is_ok(), "{flavor:?} {owner:?}");
            assert_eq!(remove_security(&source, "p\u{e4}ssw\u{f6}rd"), Err(INCORRECT.to_owned()), "{flavor:?} {owner:?}");
        }
    }
}

#[test]
fn a_non_ascii_user_password_is_refused_on_the_legacy_handlers_rather_than_decrypted_wrongly() {
    let accented = "p\u{e4}ssw\u{f6}rd";
    for flavor in [Flavor::R2, Flavor::R3, Flavor::R4] {
        assert_eq!(remove_security(&encrypted(flavor, accented, OWNER), OWNER), Err(NON_ASCII_USER_PASSWORD.to_owned()), "{flavor:?}");
        assert_eq!(remove_security(&encrypted(flavor, accented, OWNER), "wrong"), Err(INCORRECT.to_owned()), "{flavor:?}");
    }
    let output = remove_security(&encrypted(Flavor::R6, accented, OWNER), OWNER).unwrap();
    assert_eq!(contents(&lopdf::Document::load_mem(&output).unwrap()), contents(&lopdf::Document::load_mem(&plain()).unwrap()));
}

#[test]
fn the_ascii_user_password_is_read_out_of_the_padded_bytes() {
    let padded = |password: &[u8]| { let mut bytes = password.to_vec(); bytes.extend_from_slice(&PAD[..32 - password.len()]); bytes };
    assert_eq!(ascii_user_password(&padded(b"reader")).as_deref(), Some("reader"));
    assert_eq!(ascii_user_password(&padded(b"")).as_deref(), Some(""));
    assert_eq!(ascii_user_password(&padded(b"(")).as_deref(), Some("("));
    assert_eq!(ascii_user_password(&[b'k'; 32]).as_deref(), Some("kkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkk"));
    assert_eq!(ascii_user_password(&padded(b"p\xe4ss")), None);
    assert_eq!(ascii_user_password(&padded(b"a\x01b")), None);
    assert_eq!(ascii_user_password(&[b'k'; 31]), None);
    assert_eq!(ascii_user_password(&[0x80; 32]), None);
}

#[test]
fn unprotected_empty_nul_and_oversized_passwords_are_refused_before_any_work() {
    assert_eq!(remove_security(&plain(), "anything"), Err(NOT_PROTECTED.to_owned()));
    let source = encrypted(Flavor::R3, "reader", OWNER);
    assert_eq!(remove_security(&source, ""), Err(EMPTY.to_owned()));
    assert!(remove_security(&source, "owner\0pw").unwrap_err().contains("null"));
    assert!(remove_security(&source, &"x".repeat(MAX_PASSWORD_BYTES + 1)).unwrap_err().contains("1 KiB"));
    assert!(remove_security(&source, &"x".repeat(MAX_PASSWORD_BYTES)).is_err());
}

fn json(value: &impl serde::Serialize) -> serde_json::Value { serde_json::to_value(value).unwrap() }
fn id_and_revision(info: &serde_json::Value) -> (u64, u64) { (info["id"].as_u64().unwrap(), info["revision"].as_u64().unwrap()) }

fn open(service: &PdfService, path: &Path, user: &str) -> serde_json::Value {
    match tauri::async_runtime::block_on(service.begin_open(path.to_path_buf())).unwrap() {
        OpenResult::Opened { document, .. } => json(&document),
        OpenResult::PasswordRequired { request_id, .. } => {
            assert!(!user.is_empty(), "an empty user password must open without a prompt");
            match tauri::async_runtime::block_on(service.unlock(request_id, user.into())).unwrap() {
                OpenResult::Opened { document, .. } => json(&document),
                OpenResult::PasswordRequired { .. } => panic!("the user password was refused"),
            }
        }
    }
}

fn start() -> PdfService { PdfService::start(root().join("resources/pdfium/bin/pdfium.dll")) }
fn block<T>(future: impl std::future::Future<Output = T>) -> T { tauri::async_runtime::block_on(future) }

#[test]
fn service_lifts_restrictions_then_saves_an_unencrypted_copy_with_the_text_intact() {
    let _serial = serial();
    let service = start();
    let folder = tempfile::tempdir().unwrap();
    let baseline = folder.path().join("plain.pdf");
    std::fs::write(&baseline, plain()).unwrap();
    let plain_info = open(&service, &baseline, "");
    let (plain_id, plain_revision) = id_and_revision(&plain_info);
    let expected_text = block(service.text(plain_id, 0, plain_revision)).unwrap();
    assert!(!expected_text.trim().is_empty());
    for flavor in FLAVORS {
        for user in ["", "reader"] {
            let label = format!("{flavor:?} user {user:?}");
            let path = folder.path().join(format!("{flavor:?}-{}.pdf", if user.is_empty() { "open" } else { "locked" }));
            let source = encrypted(flavor, user, OWNER);
            std::fs::write(&path, &source).unwrap();
            let info = open(&service, &path, user);
            let (id, revision) = id_and_revision(&info);

            assert!(block(service.text(id, 0, revision)).is_err(), "{label}: copying must be denied before the unlock");
            assert_ne!(block(service.properties(id, revision)).unwrap().security.encrypted, Some(false), "{label}");
            let denied = folder.path().join("denied.pdf");
            assert!(block(service.save_at(id, revision, None, denied.clone())).is_err(), "{label}: a restricted document must not save");
            assert!(!denied.exists());

            // Wrong passwords, the user password and a stale revision change nothing.
            let mut wrong = vec!["wrong".to_owned(), String::new()];
            if !user.is_empty() { wrong.push(user.to_owned()); }
            for password in wrong {
                let error = block(service.owner_unlock(id, revision, password.clone())).unwrap_err();
                assert!(error == INCORRECT || error == EMPTY, "{label} {password:?}: {error}");
                assert!(block(service.text(id, 0, revision)).is_err(), "{label}: {password:?} lifted a restriction");
            }
            assert!(block(service.owner_unlock(id, revision + 1, OWNER.into())).unwrap_err().contains("changed"), "{label}");
            assert!(block(service.owner_unlock(id, revision, "x".repeat(MAX_PASSWORD_BYTES + 1))).unwrap_err().contains("1 KiB"), "{label}");
            assert!(block(service.text(id, 0, revision)).is_err(), "{label}");

            block(service.owner_unlock(id, revision, OWNER.into())).unwrap_or_else(|error| panic!("{label}: {error}"));
            assert_eq!(block(service.text(id, 0, revision)).unwrap(), expected_text, "{label}: text after the unlock");
            let properties = block(service.properties(id, revision)).unwrap();
            assert_eq!(properties.security.encrypted, Some(false), "{label}");
            assert_eq!(properties.security.modify_contents, Some(true), "{label}");
            assert_eq!(properties.source_size_bytes, source.len(), "{label}: properties describe the file opened from disk");
            assert_eq!(block(service.owner_unlock(id, revision, OWNER.into())).unwrap_err(), NOT_PROTECTED, "{label}: nothing is left to unlock");

            let output = folder.path().join(format!("{flavor:?}-{}-copy.pdf", if user.is_empty() { "open" } else { "locked" }));
            block(service.save_at(id, revision, None, output.clone())).unwrap_or_else(|error| panic!("{label}: {error}"));
            let saved = std::fs::read(&output).unwrap();
            assert!(!saved.windows(8).any(|window| window == b"/Encrypt"), "{label}: the copy still names /Encrypt");
            let reopened = open(&service, &output, "");
            let (copy_id, copy_revision) = id_and_revision(&reopened);
            assert_eq!(block(service.properties(copy_id, copy_revision)).unwrap().security.encrypted, Some(false), "{label}");
            assert_eq!(block(service.text(copy_id, 0, copy_revision)).unwrap(), expected_text, "{label}: text in the saved copy");
            assert_eq!(std::fs::read(&path).unwrap(), source, "{label}: the original file must not change");
            block(service.close(copy_id)).unwrap();
            block(service.close(id)).unwrap();
        }
    }
    block(service.close(plain_id)).unwrap();
}

#[test]
fn service_unlocked_documents_edit_and_save_but_never_enter_recovery() {
    let _serial = serial();
    let service = start();
    let folder = tempfile::tempdir().unwrap();
    let path = folder.path().join("locked.pdf");
    std::fs::write(&path, encrypted(Flavor::R3, "reader", OWNER)).unwrap();
    let info = open(&service, &path, "reader");
    let (id, revision) = id_and_revision(&info);
    block(service.owner_unlock(id, revision, OWNER.into())).unwrap();
    let edited = block(service.edit(id, crate::editor::PageEdit::Rotate { pages: vec![0], clockwise: true }, 0)).unwrap();
    let (_, edited_revision) = id_and_revision(&json(&edited));
    assert_eq!(edited_revision, revision + 1);
    let error = block(service.checkpoint_recovery(folder.path().join("recovery"), id, edited_revision, 0)).unwrap_err();
    assert!(error.contains("not available after unlocking"), "{error}");
    assert!(!folder.path().join("recovery").exists());
    let output = folder.path().join("rotated.pdf");
    block(service.save_at(id, edited_revision, None, output.clone())).unwrap();
    assert!(!std::fs::read(&output).unwrap().windows(8).any(|window| window == b"/Encrypt"));
    block(service.close(id)).unwrap();
}

#[test]
fn service_agrees_with_pdfium_about_which_password_is_the_owner_password() {
    let _serial = serial();
    let service = start();
    let folder = tempfile::tempdir().unwrap();
    for flavor in FLAVORS {
        let source = encrypted(flavor, "reader", OWNER);
        let path = folder.path().join(format!("oracle-{flavor:?}.pdf"));
        std::fs::write(&path, &source).unwrap();
        for password in [OWNER, "reader", "wrong", "owner pw "] {
            let pdfium_full_access = match block(service.begin_open(path.clone())).unwrap() {
                OpenResult::PasswordRequired { request_id, .. } => match block(service.unlock(request_id, password.into())).unwrap() {
                    OpenResult::Opened { document, .. } => {
                        let (id, revision) = id_and_revision(&json(&document));
                        // PDFium reports every permission bit only to the owner password; the text gate needs PDFium's copy bit.
                        let copy_allowed = block(service.text(id, 0, revision)).is_ok();
                        let modify = block(service.properties(id, revision)).unwrap().security.modify_contents;
                        if flavor != Flavor::R6 { assert_eq!(modify == Some(true), copy_allowed, "{flavor:?} {password:?}: PDFium's permission bits"); }
                        block(service.close(id)).unwrap();
                        copy_allowed
                    }
                    OpenResult::PasswordRequired { request_id, .. } => { block(service.cancel_password(request_id)).unwrap(); false }
                },
                OpenResult::Opened { .. } => panic!("{flavor:?}: a non-empty user password must prompt"),
            };
            assert_eq!(remove_security(&source, password).is_ok(), pdfium_full_access, "{flavor:?} password {password:?}: this check and PDFium disagree");
            assert_eq!(pdfium_full_access, password == OWNER, "{flavor:?} password {password:?}");
        }
    }
}
