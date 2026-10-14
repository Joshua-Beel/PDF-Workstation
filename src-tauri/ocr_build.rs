use std::collections::BTreeMap;
use std::env;
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use serde_json::Value;
use sha2::{Digest, Sha256};

const SETUP_ROOT_ENV: &str = "PDF_WORKSTATION_OCR_SETUP_ROOT";
const MODEL_BYTES: u64 = 4_113_088;
const MODEL_SHA256: &str = "7D4322BD2A7749724879683FC3912CB542F19906C83BCC1A52132556427170B2";
const MAX_ENGINE_BYTES: u64 = 64 * 1024 * 1024;

const RECIPE_INPUTS: &[&str] = &[
    "scripts/setup-ocr.ps1",
    "scripts/ocr/pins.json",
    "scripts/ocr/tesseract-options.cmake",
    "scripts/ocr/generate-smoke.ps1",
];

const LICENSES: &[&str] = &[
    "engine/licenses/Tesseract-Apache-2.0.txt",
    "engine/licenses/Leptonica-BSD-2-Clause.txt",
    "engine/licenses/eng-fast-Apache-2.0.txt",
];

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Receipt {
    pub(crate) path: String,
    pub(crate) bytes: u64,
    pub(crate) sha256: String,
}

pub(crate) fn configure() -> Result<(), String> {
    println!("cargo:rustc-check-cfg=cfg(ocr_opt_in)");
    println!("cargo:rerun-if-env-changed={SETUP_ROOT_ENV}");
    let Some(root) = env::var_os(SETUP_ROOT_ENV) else {
        println!("cargo:rustc-env=PDF_WORKSTATION_OCR_AVAILABLE=0");
        return Ok(());
    };
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return Err("the opt-in OCR source build supports Windows only".into());
    }
    let manifest_dir = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").ok_or("CARGO_MANIFEST_DIR is unavailable")?);
    let repository = manifest_dir.parent().ok_or("the repository root is unavailable")?;
    let root = absolute_from(repository, Path::new(&root))?;
    let manifest_path = root.join("engine/ocr-engine-manifest.json");
    println!("cargo:rerun-if-changed={}", manifest_path.display());
    let verified = verify_setup(&root, repository, &manifest_path)?;

    let out_dir = PathBuf::from(env::var_os("OUT_DIR").ok_or("OUT_DIR is unavailable")?);
    let target_profile = out_dir
        .parent().and_then(Path::parent).and_then(Path::parent)
        .ok_or("Cargo target profile directory is unavailable")?;
    let identity = verified.executable.sha256.to_ascii_lowercase();
    let relative = PathBuf::from("resources").join("ocr").join(&identity);
    let destination = target_profile.join(&relative);
    copy_verified(&root.join(&verified.executable.path), &destination.join("bin/tesseract.exe"), &verified.executable)?;
    copy_verified(&root.join(&verified.model.path), &destination.join("tessdata/eng.traineddata"), &verified.model)?;
    for license in &verified.licenses {
        let name = Path::new(&license.path).file_name().ok_or("OCR license name is invalid")?;
        copy_verified(&root.join(&license.path), &destination.join("licenses").join(name), license)?;
    }

    println!("cargo:rustc-cfg=ocr_opt_in");
    println!("cargo:rustc-env=PDF_WORKSTATION_OCR_AVAILABLE=1");
    println!("cargo:rustc-env=PDF_WORKSTATION_OCR_RESOURCE_RELATIVE={}", slash_path(&relative)?);
    println!("cargo:rustc-env=PDF_WORKSTATION_OCR_ENGINE_BYTES={}", verified.executable.bytes);
    println!("cargo:rustc-env=PDF_WORKSTATION_OCR_ENGINE_SHA256={}", verified.executable.sha256);
    for (prefix, license) in license_environment(&verified.licenses)? {
        println!("cargo:rustc-env=PDF_WORKSTATION_OCR_LICENSE_{prefix}_BYTES={}", license.bytes);
        println!("cargo:rustc-env=PDF_WORKSTATION_OCR_LICENSE_{prefix}_SHA256={}", license.sha256);
    }
    Ok(())
}

pub(crate) struct VerifiedSetup {
    pub(crate) executable: Receipt,
    pub(crate) model: Receipt,
    pub(crate) licenses: Vec<Receipt>,
}

fn license_environment(licenses: &[Receipt]) -> Result<Vec<(&'static str, &Receipt)>, String> {
    let mut by_path: BTreeMap<&str, &Receipt> = licenses.iter().map(|license| (license.path.as_str(), license)).collect();
    let mut ordered = Vec::with_capacity(LICENSES.len());
    for (path, prefix) in [
        ("engine/licenses/Tesseract-Apache-2.0.txt", "TESSERACT"),
        ("engine/licenses/Leptonica-BSD-2-Clause.txt", "LEPTONICA"),
        ("engine/licenses/eng-fast-Apache-2.0.txt", "ENG_FAST"),
    ] {
        ordered.push((prefix, by_path.remove(path).ok_or("an OCR license receipt is missing")?));
    }
    if !by_path.is_empty() { return Err("an unexpected OCR license receipt is present".into()); }
    Ok(ordered)
}

pub(crate) fn verify_setup(root: &Path, repository: &Path, manifest_path: &Path) -> Result<VerifiedSetup, String> {
    reject_reparse_ancestors(root)?;
    reject_reparse_ancestors(manifest_path)?;
    let bytes = fs::read(manifest_path).map_err(|error| format!("could not read the OCR setup manifest: {error}"))?;
    if bytes.len() > 1024 * 1024 { return Err("the OCR setup manifest is too large".into()); }
    let manifest: Value = serde_json::from_slice(&bytes).map_err(|_| "the OCR setup manifest is invalid JSON")?;
    require_u64(&manifest, &["schemaVersion"], 1)?;
    require_string(&manifest, &["versions", "recipe"], "1")?;
    require_string(&manifest, &["versions", "tesseract", "version"], "5.5.3")?;
    require_string(&manifest, &["versions", "tesseract", "commit"], "db0ec62f81b0737fbbe184d8fea40af5738f8eef")?;
    require_string(&manifest, &["versions", "leptonica", "version"], "1.87.0")?;
    require_string(&manifest, &["versions", "leptonica", "commit"], "13275a278eb55b5746e33f95fbf5a2c8f604b3ab")?;
    require_string(&manifest, &["versions", "model", "language"], "eng")?;
    require_string(&manifest, &["versions", "model", "variant"], "fast")?;
    require_string(&manifest, &["versions", "model", "version"], "4.1.0")?;
    require_string(&manifest, &["versions", "model", "commit"], "65727574dfcd264acbb0c3e07860e4e9e9b22185")?;
    require_string(&manifest, &["build", "architecture"], "x64")?;
    require_string(&manifest, &["build", "configuration"], "Release")?;
    require_string(&manifest, &["build", "runtimeLibrary"], "MultiThreaded")?;
    require_string(&manifest, &["build", "tesseractCompileDefinition"], "TESSERACT_DISABLE_DEBUG_FONTS")?;

    let executable = receipt(&manifest, &["artifacts", "executable"])?;
    require_receipt_path(&executable, "engine/bin/tesseract.exe")?;
    if executable.bytes == 0 || executable.bytes > MAX_ENGINE_BYTES { return Err("the OCR executable size is invalid".into()); }
    let model = receipt(&manifest, &["artifacts", "model"])?;
    require_receipt_path(&model, "engine/tessdata/eng.traineddata")?;
    if model.bytes != MODEL_BYTES || model.sha256 != MODEL_SHA256 { return Err("the English OCR model identity is not the pinned fast model".into()); }

    let recipe = receipt_array(&manifest, &["artifacts", "recipeInputs"])?;
    require_exact_paths(&recipe, RECIPE_INPUTS, "recipe input")?;
    for item in &recipe {
        println!("cargo:rerun-if-changed={}", repository.join(&item.path).display());
        verify_file(&repository.join(&item.path), item)?;
    }
    let licenses = receipt_array(&manifest, &["artifacts", "licenses"])?;
    require_exact_paths(&licenses, LICENSES, "license")?;

    println!("cargo:rerun-if-changed={}", root.join(&executable.path).display());
    println!("cargo:rerun-if-changed={}", root.join(&model.path).display());
    verify_file(&root.join(&executable.path), &executable)?;
    verify_file(&root.join(&model.path), &model)?;
    for license in &licenses {
        println!("cargo:rerun-if-changed={}", root.join(&license.path).display());
        verify_file(&root.join(&license.path), license)?;
    }
    Ok(VerifiedSetup { executable, model, licenses })
}

fn copy_verified(source: &Path, destination: &Path, receipt: &Receipt) -> Result<(), String> {
    reject_reparse_ancestors(destination)?;
    if let Some(parent) = destination.parent() { fs::create_dir_all(parent).map_err(|error| format!("could not create OCR resource directory: {error}"))?; }
    reject_reparse_ancestors(destination)?;
    if destination.exists() {
        return verify_file(destination, receipt);
    }
    let data = fs::read(source).map_err(|error| format!("could not read verified OCR resource: {error}"))?;
    let mut output = OpenOptions::new().write(true).create_new(true).open(destination)
        .map_err(|error| format!("could not create OCR build resource: {error}"))?;
    output.write_all(&data).and_then(|_| output.sync_all())
        .map_err(|error| format!("could not persist OCR build resource: {error}"))?;
    verify_file(destination, receipt)
}

fn verify_file(path: &Path, receipt: &Receipt) -> Result<(), String> {
    reject_reparse_ancestors(path)?;
    let metadata = fs::metadata(path).map_err(|error| format!("could not inspect verified OCR input: {error}"))?;
    if !metadata.is_file() || metadata.len() != receipt.bytes { return Err(format!("OCR input receipt mismatch for {}", receipt.path)); }
    let actual = sha256(path)?;
    if actual != receipt.sha256 { return Err(format!("OCR input hash mismatch for {}", receipt.path)); }
    Ok(())
}

fn sha256(path: &Path) -> Result<String, String> {
    let mut file = fs::File::open(path).map_err(|error| format!("could not open OCR input: {error}"))?;
    let mut hash = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let count = file.read(&mut buffer).map_err(|error| format!("could not hash OCR input: {error}"))?;
        if count == 0 { break; }
        hash.update(&buffer[..count]);
    }
    Ok(format!("{:X}", hash.finalize()))
}

fn receipt(manifest: &Value, path: &[&str]) -> Result<Receipt, String> {
    let value = value_at(manifest, path)?;
    let receipt = Receipt {
        path: value.get("path").and_then(Value::as_str).ok_or("OCR receipt path is missing")?.replace('\\', "/"),
        bytes: value.get("bytes").and_then(Value::as_u64).ok_or("OCR receipt size is missing")?,
        sha256: value.get("sha256").and_then(Value::as_str).ok_or("OCR receipt hash is missing")?.to_ascii_uppercase(),
    };
    if receipt.sha256.len() != 64 || !receipt.sha256.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("OCR receipt hash is invalid".into());
    }
    Ok(receipt)
}

fn receipt_array(manifest: &Value, path: &[&str]) -> Result<Vec<Receipt>, String> {
    value_at(manifest, path)?.as_array().ok_or("OCR receipt list is missing")?
        .iter().map(|value| receipt(value, &[])).collect()
}

fn require_exact_paths(receipts: &[Receipt], expected: &[&str], kind: &str) -> Result<(), String> {
    let actual: BTreeMap<&str, &Receipt> = receipts.iter().map(|receipt| (receipt.path.as_str(), receipt)).collect();
    let expected: std::collections::BTreeSet<&str> = expected.iter().copied().collect();
    if actual.len() != receipts.len() || actual.keys().copied().collect::<std::collections::BTreeSet<_>>() != expected {
        return Err(format!("OCR {kind} receipts do not match the required set"));
    }
    Ok(())
}

fn require_receipt_path(receipt: &Receipt, expected: &str) -> Result<(), String> {
    if receipt.path != expected { return Err(format!("OCR artifact path must be {expected}")); }
    Ok(())
}

fn value_at<'a>(value: &'a Value, path: &[&str]) -> Result<&'a Value, String> {
    path.iter().try_fold(value, |current, name| current.get(*name).ok_or_else(|| format!("OCR manifest field {} is missing", path.join("."))))
}

fn require_string(value: &Value, path: &[&str], expected: &str) -> Result<(), String> {
    if value_at(value, path)?.as_str() != Some(expected) { return Err(format!("OCR manifest field {} is not supported", path.join("."))); }
    Ok(())
}

fn require_u64(value: &Value, path: &[&str], expected: u64) -> Result<(), String> {
    if value_at(value, path)?.as_u64() != Some(expected) { return Err(format!("OCR manifest field {} is not supported", path.join("."))); }
    Ok(())
}

pub(crate) fn absolute_from(base: &Path, path: &Path) -> Result<PathBuf, String> {
    let path = if path.is_absolute() { path.to_path_buf() } else { base.join(path) };
    reject_reparse_ancestors(&path)?;
    let canonical = path.canonicalize().map_err(|error| format!("could not resolve OCR setup root: {error}"))?;
    reject_reparse_ancestors(&canonical)?;
    Ok(canonical)
}

fn slash_path(path: &Path) -> Result<String, String> {
    path.to_str().map(|value| value.replace('\\', "/")).ok_or("OCR resource path is not UTF-8".into())
}

#[cfg(windows)]
fn reject_reparse_ancestors(path: &Path) -> Result<(), String> {
    use std::os::windows::fs::MetadataExt;
    const REPARSE: u32 = 0x400;
    let mut current = Some(path);
    while let Some(candidate) = current {
        match fs::symlink_metadata(candidate) {
            Ok(metadata) if metadata.file_attributes() & REPARSE != 0 => return Err("OCR paths may not contain reparse points".into()),
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(format!("could not inspect OCR path: {error}")),
        }
        current = candidate.parent();
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(path: &Path, bytes: &[u8]) -> Receipt {
        if let Some(parent) = path.parent() { fs::create_dir_all(parent).unwrap(); }
        fs::write(path, bytes).unwrap();
        Receipt {
            path: path.file_name().unwrap().to_string_lossy().into_owned(),
            bytes: bytes.len() as u64,
            sha256: sha256(path).unwrap(),
        }
    }

    #[test]
    fn receipts_reject_missing_tampered_and_wrong_sized_files() {
        let folder = tempfile::tempdir().unwrap();
        let path = folder.path().join("engine.bin");
        let receipt = write(&path, b"verified engine bytes");
        verify_file(&path, &receipt).unwrap();
        fs::write(&path, b"tampered engine bytes").unwrap();
        assert!(verify_file(&path, &receipt).unwrap_err().contains("hash mismatch"));
        fs::write(&path, b"short").unwrap();
        assert!(verify_file(&path, &receipt).unwrap_err().contains("receipt mismatch"));
        fs::remove_file(&path).unwrap();
        assert!(verify_file(&path, &receipt).is_err());
    }

    #[test]
    fn exact_receipt_sets_ignore_manifest_order_but_reject_duplicates_and_substitutions() {
        let receipts = vec![
            Receipt { path: "b".into(), bytes: 1, sha256: "0".repeat(64) },
            Receipt { path: "a".into(), bytes: 1, sha256: "0".repeat(64) },
        ];
        require_exact_paths(&receipts, &["a", "b"], "test").unwrap();
        let duplicate = vec![receipts[0].clone(), receipts[0].clone()];
        assert!(require_exact_paths(&duplicate, &["a", "b"], "test").is_err());
        assert!(require_exact_paths(&receipts, &["a", "c"], "test").is_err());
    }

    #[test]
    fn license_environment_has_fixed_names_and_order() {
        let licenses = vec![
            Receipt { path: LICENSES[2].into(), bytes: 3, sha256: "3".repeat(64) },
            Receipt { path: LICENSES[0].into(), bytes: 1, sha256: "1".repeat(64) },
            Receipt { path: LICENSES[1].into(), bytes: 2, sha256: "2".repeat(64) },
        ];
        let values = license_environment(&licenses).unwrap();
        assert_eq!(values.iter().map(|(prefix, receipt)| (*prefix, receipt.bytes)).collect::<Vec<_>>(), vec![("TESSERACT", 1), ("LEPTONICA", 2), ("ENG_FAST", 3)]);
        assert!(license_environment(&licenses[..2]).is_err());
    }

    #[test]
    fn verified_copy_never_overwrites_a_different_existing_file() {
        let folder = tempfile::tempdir().unwrap();
        let source = folder.path().join("source");
        let destination = folder.path().join("destination");
        let receipt = write(&source, b"trusted");
        fs::write(&destination, b"existing").unwrap();
        assert!(copy_verified(&source, &destination, &receipt).is_err());
        assert_eq!(fs::read(&destination).unwrap(), b"existing");
        fs::remove_file(&destination).unwrap();
        copy_verified(&source, &destination, &receipt).unwrap();
        assert_eq!(fs::read(&destination).unwrap(), b"trusted");
        copy_verified(&source, &destination, &receipt).unwrap();
    }

    #[test]
    fn manifest_tampered_recipe_input_fails_before_artifact_use() {
        let folder = tempfile::tempdir().unwrap();
        let repository = PathBuf::from(env!("CARGO_MANIFEST_DIR")).parent().unwrap().to_path_buf();
        let mut recipe = Vec::new();
        for path in RECIPE_INPUTS {
            let full = repository.join(path);
            recipe.push(serde_json::json!({
                "path": path,
                "bytes": fs::metadata(&full).unwrap().len(),
                "sha256": sha256(&full).unwrap(),
            }));
        }
        recipe[0]["sha256"] = Value::String("0".repeat(64));
        let licenses: Vec<Value> = LICENSES.iter().map(|path| serde_json::json!({
            "path": path, "bytes": 1, "sha256": "0".repeat(64),
        })).collect();
        let manifest = serde_json::json!({
            "schemaVersion": 1,
            "versions": {
                "recipe": "1",
                "tesseract": {"version":"5.5.3", "commit":"db0ec62f81b0737fbbe184d8fea40af5738f8eef"},
                "leptonica": {"version":"1.87.0", "commit":"13275a278eb55b5746e33f95fbf5a2c8f604b3ab"},
                "model": {"language":"eng", "variant":"fast", "version":"4.1.0", "commit":"65727574dfcd264acbb0c3e07860e4e9e9b22185"}
            },
            "build": {"architecture":"x64", "configuration":"Release", "runtimeLibrary":"MultiThreaded", "tesseractCompileDefinition":"TESSERACT_DISABLE_DEBUG_FONTS"},
            "artifacts": {
                "executable": {"path":"engine/bin/tesseract.exe", "bytes":1, "sha256":"0".repeat(64)},
                "model": {"path":"engine/tessdata/eng.traineddata", "bytes":MODEL_BYTES, "sha256":MODEL_SHA256},
                "recipeInputs": recipe,
                "licenses": licenses
            }
        });
        let manifest_path = folder.path().join("engine/ocr-engine-manifest.json");
        fs::create_dir_all(manifest_path.parent().unwrap()).unwrap();
        fs::write(&manifest_path, serde_json::to_vec(&manifest).unwrap()).unwrap();
        let error = verify_setup(folder.path(), &repository, &manifest_path).err().unwrap();
        assert!(error.contains(RECIPE_INPUTS[0]));
        assert!(!folder.path().join("engine/bin").exists());
    }

    #[cfg(windows)]
    fn junction(link: &Path, target: &Path) {
        let output = std::process::Command::new("cmd.exe")
            .args(["/d", "/c", "mklink", "/J"])
            .arg(link)
            .arg(target)
            .output()
            .unwrap();
        assert!(output.status.success(), "mklink failed: {}", String::from_utf8_lossy(&output.stderr));
    }

    #[cfg(windows)]
    #[test]
    fn setup_manifest_child_junction_is_rejected_before_read() {
        let folder = tempfile::tempdir().unwrap();
        let root = folder.path().join("setup");
        let outside = folder.path().join("outside-engine");
        fs::create_dir_all(&root).unwrap();
        fs::create_dir_all(&outside).unwrap();
        fs::write(outside.join("ocr-engine-manifest.json"), b"{}").unwrap();
        let linked_engine = root.join("engine");
        junction(&linked_engine, &outside);
        let error = verify_setup(&root, folder.path(), &linked_engine.join("ocr-engine-manifest.json")).err().unwrap();
        assert!(error.contains("reparse"));
        fs::remove_dir(&linked_engine).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn destination_junction_is_rejected_before_outside_mutation() {
        let folder = tempfile::tempdir().unwrap();
        let source = folder.path().join("source");
        let receipt = write(&source, b"trusted");
        let output = folder.path().join("output");
        let outside = folder.path().join("outside");
        fs::create_dir_all(&output).unwrap();
        fs::create_dir_all(&outside).unwrap();
        let linked_resources = output.join("resources");
        junction(&linked_resources, &outside);
        let destination = linked_resources.join("new/engine.bin");
        let error = copy_verified(&source, &destination, &receipt).unwrap_err();
        assert!(error.contains("reparse"));
        assert!(!outside.join("new").exists());
        fs::remove_dir(&linked_resources).unwrap();
    }
}

#[cfg(not(windows))]
fn reject_reparse_ancestors(path: &Path) -> Result<(), String> {
    let mut current = Some(path);
    while let Some(candidate) = current {
        match fs::symlink_metadata(candidate) {
            Ok(metadata) if metadata.file_type().is_symlink() => return Err("OCR paths may not contain symbolic links".into()),
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(format!("could not inspect OCR path: {error}")),
        }
        current = candidate.parent();
    }
    Ok(())
}
