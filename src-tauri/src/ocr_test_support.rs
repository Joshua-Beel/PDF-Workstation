use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use crate::ocr_build::{absolute_from, verify_setup};
use crate::ocr_process::OcrExecutableIdentity;

pub(crate) const SETUP_ROOT_ENV: &str = "PDF_WORKSTATION_OCR_SETUP_ROOT";

pub(crate) struct LocalOcrSetup {
    pub(crate) root: PathBuf,
    pub(crate) engine: OcrExecutableIdentity,
}

impl LocalOcrSetup {
    pub(crate) fn engine_directory(&self) -> PathBuf { self.root.join("engine") }
    pub(crate) fn smoke_pnm(&self) -> PathBuf { self.root.join("smoke/known-text.pnm") }
}

static SETUP: OnceLock<Option<LocalOcrSetup>> = OnceLock::new();

pub(crate) fn local_setup() -> Option<&'static LocalOcrSetup> {
    SETUP.get_or_init(load).as_ref()
}

pub(crate) fn require_local_setup(test: &str) -> Option<&'static LocalOcrSetup> {
    let setup = local_setup();
    if setup.is_none() {
        eprintln!("SKIPPED {test}: set {SETUP_ROOT_ENV} to a scripts/setup-ocr.ps1 output root to run it");
    }
    setup
}

fn load() -> Option<LocalOcrSetup> {
    let value = std::env::var_os(SETUP_ROOT_ENV).filter(|value| !value.is_empty())?;
    let repository = Path::new(env!("CARGO_MANIFEST_DIR")).parent().expect("repository root");
    if !Path::new(&value).exists() && !repository.join(&value).exists() {
        eprintln!("{SETUP_ROOT_ENV} points at a path that does not exist");
        return None;
    }
    let root = absolute_from(repository, Path::new(&value)).unwrap_or_else(|error| panic!("{SETUP_ROOT_ENV}: {error}"));
    let manifest = root.join("engine/ocr-engine-manifest.json");
    let verified = verify_setup(&root, repository, &manifest).unwrap_or_else(|error| panic!("{SETUP_ROOT_ENV} failed validation: {error}"));
    let digest = &verified.executable.sha256;
    let mut sha256 = [0_u8; 32];
    for (index, byte) in sha256.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&digest[index * 2..index * 2 + 2], 16).expect("validated hexadecimal digest");
    }
    Some(LocalOcrSetup { root, engine: OcrExecutableIdentity { bytes: verified.executable.bytes, sha256 } })
}
