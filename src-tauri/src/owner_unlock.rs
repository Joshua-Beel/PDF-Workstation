//! Unlock with the owner (permissions) password: Acrobat's File > Properties > Security > "No Security".
//!
//! The owner password is checked against the file's own `/Encrypt` dictionary by lopdf (Algorithm 7 for
//! revisions 2 to 4, RC4 and AES-128; Algorithm 2.A owner validation for revisions 5 and 6, AES-256). Only a password
//! that passes that check yields a decrypted copy of the source, with no `/Encrypt`. There is no other route: a user
//! password, an empty password or a guess never reaches the decryption, and nothing here tries more than the one
//! password it was given.
//!
//! lopdf can derive the file key from an owner password for revisions 5 and 6 only. For revisions 2 to 4 the key comes
//! from the user password, so the user password is recovered from `/O` with the owner password (the first half of
//! Algorithm 7) and lopdf then authenticates and uses that recovered value. A file whose user password is empty is
//! already decrypted by lopdf at load; its `/Encrypt` values are rebuilt from the decryption state to run the same owner
//! check.
//!
//! The password is never stored, logged or journaled: it lives in this call and in the request that carries it.
use lopdf::{Document, Object};
use md5::{Digest as _, Md5};

pub(crate) const MAX_PASSWORD_BYTES: usize = 1024;
pub(crate) const INCORRECT: &str = "Incorrect owner password. Nothing was changed.";
pub(crate) const NOT_PROTECTED: &str = "This PDF has no security to remove.";
pub(crate) const EMPTY: &str = "Enter the owner password.";
pub(crate) const UNREADABLE: &str = "This PDF's security settings could not be read, so its owner password cannot be checked.";

/// ISO 32000-1 section 7.6.3.3, table 21: the password padding string.
const PAD: [u8; 32] = [
    0x28, 0xBF, 0x4E, 0x5E, 0x4E, 0x75, 0x8A, 0x41, 0x64, 0x00, 0x4E, 0x56, 0xFF, 0xFA, 0x01, 0x08,
    0x2E, 0x2E, 0x00, 0xB6, 0xD0, 0x68, 0x3E, 0x80, 0x2F, 0x0C, 0xA9, 0xFE, 0x64, 0x53, 0x69, 0x7A,
];

fn rc4(key: &[u8], data: &[u8]) -> Vec<u8> {
    let mut state: [u8; 256] = std::array::from_fn(|index| index as u8);
    let mut j = 0u8;
    for i in 0..256 {
        j = j.wrapping_add(state[i]).wrapping_add(key[i % key.len()]);
        state.swap(i, j as usize);
    }
    let (mut i, mut j) = (0u8, 0u8);
    data.iter().map(|byte| {
        i = i.wrapping_add(1);
        j = j.wrapping_add(state[i as usize]);
        state.swap(i as usize, j as usize);
        byte ^ state[state[i as usize].wrapping_add(state[j as usize]) as usize]
    }).collect()
}

/// Algorithm 7, step "retrieve the user password": decrypts `/O` with the key derived from `owner_password` (already in
/// PDFDocEncoding). The result is the padded user password when the owner password is the one that built `/O`.
fn user_password_from_owner(owner_password: &[u8], owner_value: &[u8], revision: i64, key_bytes: usize) -> Vec<u8> {
    let used = owner_password.len().min(32);
    let mut hasher = Md5::new();
    hasher.update(&owner_password[..used]);
    hasher.update(&PAD[..32 - used]);
    let mut hash = hasher.finalize();
    if revision >= 3 { for _ in 0..50 { hash = Md5::digest(hash); } }
    let key = &hash[..key_bytes];
    let mut result = owner_value.to_vec();
    if revision >= 3 {
        for round in (1..=19u8).rev() {
            let round_key: Vec<u8> = key.iter().map(|byte| byte ^ round).collect();
            result = rc4(&round_key, &result);
        }
    }
    rc4(key, &result)
}

/// The key length in bytes that lopdf and the specification use for revisions 2 to 4.
fn key_bytes(dictionary: &lopdf::Dictionary, revision: i64) -> Result<usize, String> {
    if revision < 3 { return Ok(5); }
    let version = dictionary.get(b"V").and_then(Object::as_i64).map_err(|_| UNREADABLE)?;
    let length = match dictionary.get(b"Length") {
        Ok(value) => value.as_i64().map_err(|_| UNREADABLE)?,
        Err(_) => if version == 4 { 128 } else { 40 },
    };
    let bytes = usize::try_from(length / 8).map_err(|_| UNREADABLE)?;
    if bytes == 0 || bytes > 16 { return Err(UNREADABLE.into()); }
    Ok(bytes)
}

const NON_ASCII_USER_PASSWORD: &str = "This PDF also has a user password with non-ASCII characters, which this build cannot use to remove its security.";

/// The user password inside the padded 32 bytes `user_password_from_owner` returns, as the printable ASCII string
/// lopdf's loader needs, or `None` when it holds anything else. The loader hashes the string's UTF-8 bytes where the
/// specification hashes PDFDocEncoding bytes; the two are the same only for printable ASCII. Padding and truncation
/// make the key depend on the 32 padded bytes alone, so the shortest prefix that pads back to them stands for them all.
fn ascii_user_password(recovered: &[u8]) -> Option<String> {
    let prefix = (0..=recovered.len().min(32)).find(|&length| recovered.len() == 32 && recovered[..length].iter().chain(&PAD[..32 - length]).eq(recovered.iter()))?;
    let password = &recovered[..prefix];
    password.iter().all(|byte| (0x20..=0x7E).contains(byte)).then(|| String::from_utf8_lossy(password).into_owned())
}

enum Source { Locked, Opened }

/// Checks `password` as the owner password of `source` and, only when it is correct, returns the same document
/// re-saved without encryption (no `/Encrypt`, no permission restrictions). `Err(INCORRECT)` leaves nothing changed.
pub(crate) fn remove_security(source: &[u8], password: &str) -> Result<Vec<u8>, String> {
    if password.is_empty() { return Err(EMPTY.into()); }
    if password.len() > MAX_PASSWORD_BYTES { return Err("Passwords cannot be longer than 1 KiB.".into()); }
    if password.contains('\0') { return Err("Passwords cannot contain a null character.".into()); }
    let probe = crate::pdf_load::load(source).map_err(|error| error.to_string())?;
    let kind = if probe.is_encrypted() { Source::Locked } else if probe.encryption_state.is_some() { Source::Opened } else { return Err(NOT_PROTECTED.into()); };

    // The /Encrypt values to check against. A file lopdf already decrypted (empty user password) no longer holds its
    // dictionary, so it is rebuilt from the decryption state, which keeps every value the check reads.
    let rebuilt;
    let verifier: &Document = match kind {
        Source::Locked => &probe,
        Source::Opened => {
            let dictionary = probe.encryption_state.as_ref().ok_or(UNREADABLE)?.encode().map_err(|_| UNREADABLE)?;
            let mut shell = Document::with_version("1.7");
            let id = shell.add_object(Object::Dictionary(dictionary));
            shell.trailer.set("Encrypt", Object::Reference(id));
            if let Ok(file_id) = probe.trailer.get(b"ID") { shell.trailer.set("ID", file_id.clone()); }
            rebuilt = shell;
            &rebuilt
        }
    };
    let algorithm = lopdf::encryption::PasswordAlgorithm::try_from(verifier).map_err(|_| UNREADABLE)?;
    let encoded = algorithm.sanitize_password(password).map_err(|_| INCORRECT)?;
    match algorithm.authenticate_owner_password(verifier, &encoded) {
        Ok(()) => {}
        Err(lopdf::encryption::DecryptionError::IncorrectPassword) => return Err(INCORRECT.into()),
        Err(_) => return Err(UNREADABLE.into()),
    }

    // A file with a user password is decrypted by loading it with a password lopdf derives its key from. For revisions
    // 5 and 6 that is the (SASLprep-ed) owner password. For revisions 2 to 4 lopdf derives the key from the user
    // password, so the loader is given the user password recovered from /O.
    let mut document = match kind {
        Source::Opened => probe,
        Source::Locked => {
            let dictionary = probe.get_encrypted().map_err(|_| UNREADABLE)?;
            let revision = dictionary.get(b"R").and_then(Object::as_i64).map_err(|_| UNREADABLE)?;
            let load_password = if revision >= 5 {
                String::from_utf8(encoded.clone()).map_err(|_| UNREADABLE)?
            } else {
                let owner_value = dictionary.get(b"O").and_then(Object::as_str).map_err(|_| UNREADABLE)?;
                let recovered = user_password_from_owner(&encoded, owner_value, revision, key_bytes(dictionary, revision)?);
                ascii_user_password(&recovered).ok_or(NON_ASCII_USER_PASSWORD)?
            };
            let document = crate::pdf_load::load_with_password(source, &load_password).map_err(|error| error.to_string())?;
            if document.is_encrypted() || document.encryption_state.is_none() { return Err(UNREADABLE.into()); }
            document
        }
    };
    document.encryption_state = None;
    document.trailer.remove(b"Encrypt");
    crate::pdf_load::prune_unreachable(&mut document);
    let page_count = document.get_pages().len();
    if page_count == 0 { return Err(UNREADABLE.into()); }

    let mut bytes = Vec::new();
    document.save_to(&mut bytes).map_err(|error| format!("The unlocked copy could not be written: {error}"))?;
    let check = crate::pdf_load::load(&bytes).map_err(|error| error.to_string())?;
    if check.is_encrypted() || check.encryption_state.is_some() || check.get_pages().len() != page_count {
        return Err("The unlocked copy did not pass its own check, so nothing was changed.".into());
    }
    Ok(bytes)
}

#[cfg(test)]
#[path = "owner_unlock_tests.rs"]
mod tests;
